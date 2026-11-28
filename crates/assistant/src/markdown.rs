//! 回答 Markdown 的呈现策略：链接只放行 https，图片一律不加载，代码块带复制按钮。
//!
//! 回答里的链接与图片来自模型（可能被工具输出里的提示注入左右），所以：
//! - 点击只打开 `https://` 地址（`file://`、自定义协议、`javascript:` 等全部忽略），悬停显示目标地址；
//! - 图片地址永远不发起网络请求（否则图片 URL 可以把数据带出去）。

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash as _, Hasher as _},
    sync::Arc,
};

use fluxdown_ui_components::{ButtonVariant, FluxIcon, icon_button, toast, tooltip_view};
use fluxdown_ui_theme::active_theme;
use gpui::{App, ClipboardItem, ImageSource, SharedString, Styled as _, Window};
use gpui_component::{Icon, WindowExt as _, text::TextView};

/// 链接长度上限：超长的多半是在夹带数据。
const MAX_LINK_CHARS: usize = 2048;
const HTTPS_PREFIX: &str = "https://";

/// 允许打开的链接：`https://` + 非空主机，无空白 / 控制字符，长度有限。返回去掉首尾空白后的地址。
pub(crate) fn safe_link(url: &str) -> Option<&str> {
    let url = url.trim();
    if url.chars().count() > MAX_LINK_CHARS
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let scheme = url.get(..HTTPS_PREFIX.len())?;
    if !scheme.eq_ignore_ascii_case(HTTPS_PREFIX) {
        return None;
    }
    let rest = url.get(HTTPS_PREFIX.len()..)?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // 主机里带 `@` 是 userinfo 伪装（`https://docs.example.com@evil.test/`）：不放行。
    (!host.is_empty() && !host.starts_with(['.', ':']) && !host.contains('@')).then_some(url)
}

/// 回答 Markdown 视图用到的文案。
#[derive(Clone)]
pub(crate) struct AnswerLabels {
    /// 代码块复制按钮的提示。
    pub(crate) copy_code: SharedString,
    /// 复制成功的 toast。
    pub(crate) copied: SharedString,
}

/// 助手回答的 Markdown 视图。`key` 稳定（流式增量只更新文本）；`text` 是 `Arc` 共享的整段文本，
/// 每次重绘不复制。
pub(crate) fn answer_view(
    key: &str,
    text: &Arc<str>,
    labels: &AnswerLabels,
    open_link: impl Fn(&str, &mut App) + Send + Sync + 'static,
) -> TextView {
    let turn_key = SharedString::from(key.to_owned());
    let labels = labels.clone();
    TextView::markdown(
        SharedString::from(format!("assistant-md-{key}")),
        SharedString::from(Arc::clone(text)),
    )
    .selectable(true)
    .on_link_click(move |url, _, _, cx| match safe_link(url) {
        Some(link) => open_link(link, cx),
        None => log::debug!("assistant markdown link ignored (not https)"),
    })
    .link_tooltip(|url, window, cx| tooltip_view(url.clone(), window, cx))
    // 不发任何图片请求：来源恒为「永远加载中」的自定义源。
    .image_source(|_| ImageSource::Custom(Arc::new(|_, _| None)))
    .code_block_actions(move |block, _: &mut Window, cx: &mut App| {
        let code = block.code();
        let mut hasher = DefaultHasher::new();
        code.hash(&mut hasher);
        let id = SharedString::from(format!(
            "assistant-code-copy-{turn_key}-{:x}",
            hasher.finish()
        ));
        let theme = active_theme(cx);
        let icon_size = theme.extended().icon.sm;
        let side = theme.density().status_control;
        let copied = labels.copied.clone();
        icon_button(
            id,
            labels.copy_code.clone(),
            Icon::new(FluxIcon::Copy).size(icon_size),
            ButtonVariant::Ghost,
            cx,
        )
        .size(side)
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            window.push_notification(toast::success(copied.clone()), cx);
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_links_with_a_host_open() {
        assert_eq!(
            safe_link("https://fluxdown.com/docs?a=1#x"),
            Some("https://fluxdown.com/docs?a=1#x")
        );
        assert_eq!(
            safe_link("  HTTPS://fluxdown.com  "),
            Some("HTTPS://fluxdown.com")
        );
        for bad in [
            "http://fluxdown.com",
            "file:///Applications/Calculator.app",
            "javascript:alert(1)",
            "fluxdown://open",
            "mailto:a@b.c",
            "//fluxdown.com",
            "https://",
            "https:///etc/passwd",
            "https://.evil.test",
            "https://docs.fluxdown.com@evil.test/x",
            "https://fluxdown.com/a b",
            "https://fluxdown.com/\u{7}",
            "",
        ] {
            assert_eq!(safe_link(bad), None, "{bad:?}");
        }
        let long = format!("https://fluxdown.com/{}", "a".repeat(MAX_LINK_CHARS));
        assert_eq!(safe_link(&long), None);
    }

    #[test]
    fn other_schemes_and_relative_forms_are_rejected() {
        for bad in [
            "  javascript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "docs/faq",
            "/docs",
            "https:/fluxdown.com",
            "httpss://fluxdown.com",
            "ftp://fluxdown.com",
            "https://:8443/x",
            "https://fluxdown.com/a\tb",
            "https://fluxdown.com/a\u{a0}b",
            "é́é́é́é́é́é́é́é́é́",
        ] {
            assert_eq!(safe_link(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn ports_case_and_surrounding_whitespace_are_accepted() {
        assert_eq!(
            safe_link("https://fluxdown.com:8443/x"),
            Some("https://fluxdown.com:8443/x")
        );
        assert_eq!(safe_link("HtTpS://a.b"), Some("HtTpS://a.b"));
        assert_eq!(safe_link("\thttps://a.b/x\n"), Some("https://a.b/x"));
        assert_eq!(safe_link("https://a.b?x=1"), Some("https://a.b?x=1"));
    }

    #[test]
    fn link_length_cap_is_inclusive() {
        let fill = |len: usize| format!("{HTTPS_PREFIX}{}", "a".repeat(len - HTTPS_PREFIX.len()));
        let at_cap = fill(MAX_LINK_CHARS);
        assert_eq!(safe_link(&at_cap), Some(at_cap.as_str()));
        assert_eq!(safe_link(&fill(MAX_LINK_CHARS + 1)), None);
    }
}
