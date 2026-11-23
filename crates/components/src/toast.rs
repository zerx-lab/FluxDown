//! 窗口右上角的应用内提示（toast）。
//!
//! gpui-component 的 `Notification::info/success/...` 把语气图标绝对定位在卡片左上角固定偏移处，
//! 与单行正文不对齐，多行 / 带标题时也不跟随首行；正文与悬停出现的关闭按钮还会重叠。
//! 这里只替换卡片内容：卡片外壳、堆叠、自动隐藏与进出场动效仍由 gpui-component 负责。
//!
//! 全应用的提示一律经本模块创建，不直接调用 `Notification::info/success/warning/error`。
//! 返回值仍是 `Notification`，可继续链式设置 `id` / `autohide` / `action` / `on_click`；
//! 标题只能经 [`titled`] 设置，不要再调用 `Notification::title`（会画在内容之外、与图标错位）。

use fluxdown_ui_theme::active_theme;
use gpui::{
    App, FontWeight, Hsla, IntoElement as _, ParentElement as _, SharedString, Styled as _, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{Icon, notification::Notification};

use crate::FluxIcon;

/// 提示语气：决定前导图标与其颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    /// 中性结果反馈，不带图标。
    Neutral,
    Info,
    Success,
    Warning,
    Error,
}

impl ToastTone {
    fn icon(self, cx: &App) -> Option<(FluxIcon, Hsla)> {
        let theme = active_theme(cx);
        let colors = &theme.extended().colors;
        match self {
            Self::Neutral => None,
            Self::Info => Some((FluxIcon::Info, colors.info)),
            Self::Success => Some((FluxIcon::CircleCheck, colors.success)),
            Self::Warning => Some((FluxIcon::TriangleAlert, colors.warning)),
            Self::Error => Some((FluxIcon::CircleAlert, theme.tokens().colors.destructive)),
        }
    }
}

/// 中性提示（无图标）。
pub fn plain(message: impl Into<SharedString>) -> Notification {
    build(ToastTone::Neutral, None, message.into())
}

/// 信息提示。
pub fn info(message: impl Into<SharedString>) -> Notification {
    build(ToastTone::Info, None, message.into())
}

/// 成功提示。
pub fn success(message: impl Into<SharedString>) -> Notification {
    build(ToastTone::Success, None, message.into())
}

/// 警示提示。
pub fn warning(message: impl Into<SharedString>) -> Notification {
    build(ToastTone::Warning, None, message.into())
}

/// 错误提示。
pub fn error(message: impl Into<SharedString>) -> Notification {
    build(ToastTone::Error, None, message.into())
}

/// 指定语气的提示。
pub fn toast(tone: ToastTone, message: impl Into<SharedString>) -> Notification {
    build(tone, None, message.into())
}

/// 带标题的提示：标题加粗，正文降一级颜色作为补充说明。
pub fn titled(
    tone: ToastTone,
    title: impl Into<SharedString>,
    message: impl Into<SharedString>,
) -> Notification {
    build(tone, Some(title.into()), message.into())
}

fn build(tone: ToastTone, title: Option<SharedString>, message: SharedString) -> Notification {
    Notification::new().content(move |_, _, cx| {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let body = tokens.typography.sm;
        let has_title = title.is_some();
        div()
            .flex()
            .items_center()
            .gap(tokens.spacing.sm)
            // 关闭按钮悬停时浮在卡片右上角，正文给它留出位置。
            .pr(tokens.spacing.lg)
            .text_size(body.size)
            .line_height(body.line_height)
            .when_some(tone.icon(cx), |row, (icon, color)| {
                // 图标相对整段文字垂直居中（单行即与该行对齐，多行 / 带标题居于整段中间）。
                row.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(body.line_height)
                        .child(
                            Icon::new(icon)
                                .size(theme.extended().icon.lg)
                                .text_color(color),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(tokens.spacing.xxs)
                    .when_some(title.clone(), |column, title| {
                        column.child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(tokens.colors.foreground)
                                .child(title),
                        )
                    })
                    .child(
                        div()
                            .text_color(if has_title {
                                tokens.colors.muted_foreground
                            } else {
                                tokens.colors.foreground
                            })
                            .child(message.clone()),
                    ),
            )
            .into_any_element()
    })
}
