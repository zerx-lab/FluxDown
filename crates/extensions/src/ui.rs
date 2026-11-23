//! 扩展页共用的版式小积木：分隔线、标题 / 元信息文字、状态行、空状态（卡片 / 对话框标题直接用 `fluxdown_ui_components::{card, dialog_title}`）。
//!
//! 规则与全应用一致：分隔线用 hairline；
//! 标题 sm MEDIUM；说明 / 元信息 xs + 二级文字色；状态色只取扩展 token
//! （成功 success、警示 warning、失败 destructive、进行中 primary）。

use std::rc::Rc;

use fluxdown_ui_components::ControlExt as _;
use fluxdown_ui_theme::{FluxThemeState, active_theme};
use gpui::{
    Anchor, App, Div, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled, Window,
    div,
};
use gpui_component::{
    Icon, IconNamed,
    button::Button,
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
};

use crate::pages::Frame;

/// 扩展页下拉选择的按钮宽度（随字号缩放的基准值）：同屏多个选择框等宽对齐。
const SELECT_WIDTH: f32 = 168.;
/// 下拉菜单最多同时显示的选项数（超出滚动）。
const SELECT_VISIBLE_OPTIONS: f32 = 8.;

/// 下拉选择（同原生 select）：定宽按钮，展开的菜单与按钮等宽、右缘对齐，选项与按钮同高。
/// `button` 由调用方设好 id / label / busy / disabled；`options` = （值，显示名），
/// `current` 的选项带勾选。
pub(crate) fn select_button(
    button: Button,
    options: Vec<(String, SharedString)>,
    current: Option<String>,
    on_select: impl Fn(String, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = active_theme(cx);
    let width = theme.text_extent(SELECT_WIDTH);
    let height = theme.density().control;
    let on_select = Rc::new(on_select);
    button
        .outline()
        .dropdown_caret(true)
        .control(cx)
        .flex_none()
        .w(width)
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            options.iter().fold(
                menu.min_w(width)
                    .max_w(width)
                    .scrollable(true)
                    .max_h(height * SELECT_VISIBLE_OPTIONS),
                |menu, (value, name)| {
                    let name = name.clone();
                    let value = value.clone();
                    let on_select = on_select.clone();
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(height)
                                .flex()
                                .items_center()
                                .child(div().min_w_0().truncate().child(name.clone()))
                        })
                        .checked(current.as_deref() == Some(value.as_str()))
                        .on_click(move |_, window, cx| on_select(value.clone(), window, cx)),
                    )
                },
            )
        })
}

/// 卡片内的结构分隔线（hairline，`stroke.thin` 粗）。
pub(crate) fn divider(frame: Frame<'_>) -> Div {
    div()
        .w_full()
        .h(frame.extended.stroke.thin)
        .flex_none()
        .bg(frame.extended.colors.hairline)
}

/// 标题文字：sm MEDIUM 正文色。
pub(crate) fn title_text(text: impl Into<SharedString>, frame: Frame<'_>) -> Div {
    let sm = frame.tokens.typography.sm;
    div()
        .text_size(sm.size)
        .line_height(sm.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(frame.tokens.colors.foreground)
        .child(text.into())
}

/// 正文文字：sm 正文色。
pub(crate) fn body_text(text: impl Into<SharedString>, frame: Frame<'_>) -> Div {
    let sm = frame.tokens.typography.sm;
    div()
        .text_size(sm.size)
        .line_height(sm.line_height)
        .text_color(frame.tokens.colors.foreground)
        .child(text.into())
}

/// 说明 / 元信息文字：xs 二级文字色。
pub(crate) fn meta_text(text: impl Into<SharedString>, frame: Frame<'_>) -> Div {
    let xs = frame.tokens.typography.xs;
    div()
        .text_size(xs.size)
        .line_height(xs.line_height)
        .text_color(frame.tokens.colors.muted_foreground)
        .child(text.into())
}

/// 中性小胶囊（来源 / 版本 / 计数等标签）：caption 字号、muted 底、二级文字色、
/// `components.badge.radius` 圆角。
pub(crate) fn neutral_pill(text: impl Into<SharedString>, frame: Frame<'_>) -> Div {
    let caption = frame.extended.caption;
    div()
        .flex_none()
        .px(frame.tokens.spacing.sm)
        .py(frame.tokens.spacing.xxs)
        .rounded(frame.components.badge_radius)
        .bg(frame.tokens.colors.muted)
        .text_size(caption.size)
        .line_height(caption.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(frame.tokens.colors.muted_foreground)
        .child(text.into())
}

/// 状态行：`icon.md` 着色图标 + xs 文字（文字保持二级色，只有图标承载状态色）。
pub(crate) fn status_line(
    icon: impl IconNamed,
    color: Hsla,
    text: impl IntoElement,
    frame: Frame<'_>,
) -> Div {
    let xs = frame.tokens.typography.xs;
    h_flex()
        .gap(frame.tokens.spacing.sm)
        .items_start()
        .child(
            div()
                .flex_none()
                .h(xs.line_height)
                .flex()
                .items_center()
                .child(
                    Icon::new(icon)
                        .size(frame.extended.icon.md)
                        .text_color(color),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(xs.size)
                .line_height(xs.line_height)
                .text_color(frame.tokens.colors.muted_foreground)
                .child(text),
        )
}

/// 只读路径框：与输入框同高（统一控件档）、同描边；未选择时显示占位色文字。
/// 配合 `input_with_action` + 「浏览」按钮使用。
pub(crate) fn path_box(
    text: impl Into<SharedString>,
    placeholder: bool,
    theme: &FluxThemeState,
) -> Div {
    let tokens = theme.tokens();
    let sm = tokens.typography.sm;
    h_flex()
        .w_full()
        .h(theme.density().control)
        .px(tokens.spacing.md)
        .items_center()
        .rounded(tokens.radius.md)
        .border(theme.extended().stroke.thin)
        .border_color(tokens.colors.border)
        .bg(tokens.colors.background)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(sm.size)
                .line_height(sm.line_height)
                .text_color(if placeholder {
                    tokens.colors.muted_foreground
                } else {
                    tokens.colors.foreground
                })
                .child(text.into()),
        )
}

/// 状态色小胶囊：caption 字号，状态色文字 + 10% 同色底（失败 / 撤回等需要着色的状态）。
pub(crate) fn tone_pill(text: impl Into<SharedString>, color: Hsla, frame: Frame<'_>) -> Div {
    neutral_pill(text, frame)
        .bg(color.opacity(0.1))
        .text_color(color)
}

/// 风险等级的色调：`elevated` = warning，`full` = destructive，`standard` 无强调。
pub(crate) fn risk_color(risk: &str, frame: Frame<'_>) -> Option<Hsla> {
    match risk {
        "elevated" => Some(frame.extended.colors.warning),
        "full" => Some(frame.tokens.colors.destructive),
        _ => None,
    }
}

/// 风险等级的（名称，说明）文案键；未知等级按 standard。
pub(crate) fn risk_keys(risk: &str) -> (&'static str, &'static str) {
    match risk {
        "elevated" => ("pluginRiskElevated", "pluginRiskTooltipElevated"),
        "full" => ("pluginRiskFull", "pluginRiskTooltipFull"),
        _ => ("pluginRiskStandard", "pluginRiskTooltipStandard"),
    }
}

/// 风险徽标：只对 `elevated` / `full` 显示（standard 不打扰），悬停给出说明。
pub(crate) fn risk_pill(
    id: impl Into<gpui::ElementId>,
    risk: &str,
    frame: Frame<'_>,
) -> Option<gpui::Stateful<Div>> {
    use gpui::InteractiveElement as _;
    use gpui::StatefulInteractiveElement as _;
    let color = risk_color(risk, frame)?;
    let (name_key, tip_key) = risk_keys(risk);
    let tip = SharedString::from(frame.translator.text(tip_key).to_owned());
    Some(
        tone_pill(frame.translator.text(name_key).to_owned(), color, frame)
            .id(id)
            .tooltip(move |window, cx| {
                fluxdown_ui_components::tooltip_view(tip.clone(), window, cx)
            }),
    )
}

/// 空状态：32px 图标（text_tertiary）+ sm MEDIUM 标题 + 可选 xs 说明，居中。
pub(crate) fn empty_state(
    icon: impl IconNamed,
    title: impl Into<SharedString>,
    desc: Option<SharedString>,
    frame: Frame<'_>,
) -> Div {
    let tokens = frame.tokens;
    gpui_component::v_flex()
        .w_full()
        .items_center()
        .justify_center()
        .gap(tokens.spacing.xs)
        .py(tokens.spacing.xl)
        .child(
            Icon::new(icon)
                .size(frame.extended.icon.lg * 2.)
                .text_color(frame.extended.colors.text_tertiary),
        )
        .child(title_text(title, frame).mt(tokens.spacing.xs))
        .children(desc.map(|desc| meta_text(desc, frame).text_center()))
}

/// 主页链接只对 http(s) 渲染为可点击链接：插件 manifest / 市场索引的 homepage 不可信，
/// `file://`、自定义协议等交给系统打开会触发本地文件或协议处理器。
pub(crate) fn is_web_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.trim().split_once("://") else {
        return false;
    };
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && !rest.is_empty()
}

/// 主页地址的展示文本：去掉协议、`www.` 与末尾斜杠（`https://github.com/a/b/` → `github.com/a/b`）；
/// 链接目标仍用原地址。
pub(crate) fn display_url(url: &str) -> &str {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let trimmed = rest.trim_end_matches('/');
    if trimmed.is_empty() { url } else { trimmed }
}

#[cfg(test)]
mod tests {
    use super::{display_url, is_web_url};

    #[test]
    fn display_url_drops_scheme_www_and_trailing_slash() {
        assert_eq!(
            display_url("https://github.com/zerx-lab/x/"),
            "github.com/zerx-lab/x"
        );
        assert_eq!(display_url("HTTP://www.example.com"), "example.com");
        assert_eq!(display_url("example.com/a"), "example.com/a");
        assert_eq!(display_url("https://"), "https://");
    }

    #[test]
    fn only_http_and_https_are_clickable() {
        assert!(is_web_url("https://example.com/plugin"));
        assert!(is_web_url("HTTP://example.com"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("myapp://open"));
        assert!(!is_web_url("https://"));
        assert!(!is_web_url("example.com"));
    }
}
