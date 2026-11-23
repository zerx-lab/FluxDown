//! 文本悬浮提示：gpui-component 的 `Tooltip::new` 不限宽，长文本（插件描述、错误原因、完整路径）
//! 会撑成一整行横跨窗口。全应用的文本提示统一经这里构造，超过上限时按宽度折行。

use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyView, App, ElementId, InteractiveText, ParentElement as _, SharedString, Styled as _,
    StyledText, Window, div,
};
use gpui_component::tooltip::Tooltip;

/// 提示正文的最大宽度（默认字号下的 px，随界面缩放与字号同比放大）。
const TOOLTIP_MAX_WIDTH: f32 = 320.;

/// 构造文本悬浮提示视图，用于 `.tooltip(move |window, cx| tooltip_view(..))`。
pub fn tooltip_view(text: impl Into<SharedString>, window: &mut Window, cx: &mut App) -> AnyView {
    let text = text.into();
    Tooltip::element(move |_, cx| {
        div()
            .max_w(active_theme(cx).text_extent(TOOLTIP_MAX_WIDTH))
            .child(text.clone())
    })
    .build(window, cx)
}

/// 单行文本：被父级 `.truncate()` 截断（显示省略号）时悬停显示完整文本，放得下时不弹提示。
///
/// 截断与否在悬停时读排版结果判定，列宽变化后无需重算。提示走 [`tooltip_view`]，长文本按上限折行。
pub fn truncated_text(id: impl Into<ElementId>, text: impl Into<SharedString>) -> InteractiveText {
    let full = text.into();
    let styled = StyledText::new(full.clone());
    let layout = styled.layout().clone();
    InteractiveText::new(id, styled).tooltip(move |_, window, cx| {
        // 悬停发生在绘制之后，排版已完成；截断时排版文本是「前缀 + 省略号」，与原文不同。
        (layout.text() != full.as_ref()).then(|| tooltip_view(full.clone(), window, cx))
    })
}
