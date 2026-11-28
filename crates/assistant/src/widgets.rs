//! 助手页共用的小部件：文字档位、徽标、转圈、圆形按钮、缩略图等。

use std::{sync::Arc, time::Duration};

use fluxdown_ui_components::{
    Button as ThemedButton, ButtonVariant, FluxIcon, StateLayer, button, icon_button, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Div, ElementId, FontWeight, Hsla, Image,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled, StyledImage as _, div, img,
    prelude::FluentBuilder as _, px, transparent_black,
};
use gpui_component::{Icon, Sizable as _, h_flex, spinner::Spinner};

/// 元信息文字：xs + 三级文字色。
pub(crate) fn meta_text(cx: &App) -> Div {
    let theme = active_theme(cx);
    let xs = theme.tokens().typography.xs;
    div()
        .text_size(xs.size)
        .line_height(xs.line_height)
        .text_color(theme.extended().colors.text_tertiary)
}

/// 正文：sm + 前景色。
pub(crate) fn body_text(cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let sm = tokens.typography.sm;
    div()
        .text_size(sm.size)
        .line_height(sm.line_height)
        .text_color(tokens.colors.foreground)
}

/// 中性小徽标。
pub(crate) fn badge(cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let caption = theme.extended().caption;
    div()
        .px(tokens.spacing.xs + tokens.spacing.xxs)
        .py(px(1.))
        .rounded(tokens.radius.sm)
        .bg(tokens.colors.muted)
        .text_size(caption.size)
        .line_height(caption.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(tokens.colors.muted_foreground)
}

/// 图标 + 文字的结果标签（已允许 / 已过期 / 已提交等）。
pub(crate) fn result_label(icon: FluxIcon, text: SharedString, tone: Hsla, cx: &App) -> AnyElement {
    let theme = active_theme(cx);
    h_flex()
        .items_center()
        .gap(theme.tokens().spacing.xs)
        .child(
            Icon::new(icon)
                .size(theme.extended().icon.sm)
                .text_color(tone),
        )
        .child(meta_text(cx).text_color(tone).child(text))
        .into_any_element()
}

pub(crate) fn spinner(size: Pixels) -> Spinner {
    Spinner::new().with_size(size)
}

/// 回复 / 执行中的停止按钮外圈：两道错相向外扩散淡出的光环 + 一圈旋转弧线。
pub(crate) fn busy_halo(side: Pixels, button: AnyElement, cx: &App) -> AnyElement {
    let primary = active_theme(cx).tokens().colors.primary;
    let ripple = move |id: &'static str, phase: f32| {
        div()
            .absolute()
            .rounded_full()
            .border_2()
            .border_color(primary)
            .with_animation(
                id,
                Animation::new(Duration::from_millis(1800)).repeat(),
                move |this, delta| {
                    let t = (delta + phase) % 1.;
                    let grow = side * (1. + 0.7 * t);
                    let offset = (side - grow) / 2.;
                    this.size(grow)
                        .top(offset)
                        .left(offset)
                        .opacity(0.55 * (1. - t) * (1. - t))
                },
            )
    };
    let ring = side + px(8.);
    div()
        .relative()
        .flex_none()
        .size(side)
        .child(ripple("assistant-busy-ripple-a", 0.))
        .child(ripple("assistant-busy-ripple-b", 0.5))
        .child(
            div()
                .absolute()
                .top(px(-4.))
                .left(px(-4.))
                .size(ring)
                .child(spinner(ring).color(primary)),
        )
        .child(div().absolute().inset_0().child(button))
        .into_any_element()
}

/// 方形缩略图：圆角 + 细边框，图片按 cover 裁切。
pub(crate) fn thumbnail(image: &Arc<Image>, side: Pixels, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let ext = theme.extended();
    div()
        .relative()
        .flex_none()
        .size(side)
        .rounded(tokens.radius.md)
        .border(ext.stroke.thin)
        .border_color(ext.colors.hairline)
        .child(
            img(Arc::clone(image))
                .size_full()
                .rounded(tokens.radius.md)
                .object_fit(ObjectFit::Cover),
        )
}

/// 缩略图尺寸的占位框：转圈（加载中）或图标（失败）放在里面。
pub(crate) fn thumbnail_slot(side: Pixels, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let ext = theme.extended();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(side)
        .rounded(tokens.radius.md)
        .border(ext.stroke.thin)
        .border_color(ext.colors.hairline)
        .bg(tokens.colors.muted)
}

/// 圆形按钮的配色。
#[derive(Clone, Copy)]
pub(crate) enum CircleTone {
    /// 主色填充（发送 / 停止）；不可用时灰底。
    Filled,
    /// 无底色的图标按钮（添加图片）。
    Ghost,
    /// 缩略图角上的移除按钮：前景色填充、反色图标。
    Overlay,
}

/// 圆形图标按钮：与 `icon_button` 同一套结构（无障碍 / 焦点环 / 状态层），悬停 / 按下由状态层在底色
/// 上叠一层极淡的对比色；不可用时无指针手型与悬停反馈。
pub(crate) fn circle_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    content: impl IntoElement,
    size: Pixels,
    tone: CircleTone,
    enabled: bool,
    cx: &App,
) -> ThemedButton {
    let label = label.into();
    let tip = label.clone();
    let theme = active_theme(cx);
    let colors = theme.tokens().colors;
    let radius = theme.tokens().radius.full;
    let stroke = theme.extended().stroke.thin;
    let (fill, foreground, overlay) = match (tone, enabled) {
        (CircleTone::Filled, true) => (
            colors.primary,
            colors.primary_foreground,
            colors.primary_foreground,
        ),
        (CircleTone::Filled, false) => (colors.muted, colors.muted_foreground, colors.foreground),
        (CircleTone::Ghost, true) => (
            transparent_black(),
            colors.muted_foreground,
            colors.foreground,
        ),
        (CircleTone::Ghost, false) => (
            transparent_black(),
            colors.muted_foreground.opacity(0.5),
            colors.foreground,
        ),
        (CircleTone::Overlay, _) => (colors.foreground, colors.background, colors.background),
    };
    let button = ThemedButton::new(id)
        .size(size)
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius)
        .border(stroke)
        .border_color(transparent_black())
        .bg(fill)
        .text_color(foreground)
        .focus_visible(move |style| style.border_color(colors.ring))
        .accessibility_label(label)
        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        .disabled(!enabled);
    let button = if enabled {
        button.cursor_pointer().child(
            StateLayer::new(radius)
                .hover(overlay.opacity(0.12))
                .press(overlay.opacity(0.22)),
        )
    } else {
        button
    };
    button.child(content)
}

/// 行内小号文字按钮（确认 / 取消这类次要动作）：在幽灵按钮上收成 `density.statusControl` 高、
/// caption 字号；`danger` = 危险色文字 + 极淡危险底色，不做整块红色填充。
pub(crate) fn compact_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    danger: bool,
    cx: &App,
) -> ThemedButton {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let caption = theme.extended().caption;
    let colors = tokens.colors;
    button(id, label, ButtonVariant::Ghost, cx)
        .h(theme.density().status_control)
        .px(tokens.spacing.sm)
        .text_size(caption.size)
        .line_height(caption.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(if danger {
            colors.destructive
        } else {
            colors.muted_foreground
        })
        .when(danger, |this| this.bg(colors.destructive.opacity(0.12)))
}

/// 回答下方的图标按钮边长（随文字缩放）。
const ACTION_BUTTON_SIDE: f32 = 24.;

/// 回答 / 提问下方的图标按钮（复制、评价、重新生成、编辑重发）：只有图标，没有边框与底色框。
pub(crate) fn action_button(
    id: impl Into<ElementId>,
    label: SharedString,
    icon: Icon,
    tone: Hsla,
    cx: &App,
) -> ThemedButton {
    let theme = active_theme(cx);
    let side = theme.text_extent(ACTION_BUTTON_SIDE);
    let tip = label.clone();
    icon_button(id, label, icon, ButtonVariant::Ghost, cx)
        .size(side)
        .text_color(tone)
        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
}
