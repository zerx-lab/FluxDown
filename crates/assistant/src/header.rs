//! 右侧面板顶栏插槽：当前标题、今日用量、新对话与历史会话切换。shell 在其右侧追加「收起」
//! 按钮（Windows / Linux 另有窗口按钮）；本视图随助手视图重绘。

use fluxdown_ui_components::{FluxIcon, nav_icon_color, toolbar_action_button, tooltip_view};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, ClickEvent, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{Icon, h_flex};

use crate::{model::Gate, view::AssistantView};

pub struct AssistantPanelHeader {
    view: Entity<AssistantView>,
}

impl AssistantPanelHeader {
    pub(crate) fn new(view: Entity<AssistantView>, cx: &mut Context<Self>) -> Self {
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        Self { view }
    }
}

/// 顶栏图标按钮：带悬浮提示；`selected` 时强调色图标 + 中性选中底（切换按钮的打开态）。
fn header_button(
    id: &'static str,
    label: SharedString,
    icon: FluxIcon,
    selected: bool,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = active_theme(cx);
    let extended = theme.extended();
    let tooltip = label.clone();
    div()
        .id(SharedString::from(format!("{id}-tooltip")))
        .flex_none()
        .rounded(theme.components().button_radius)
        .when(selected, |this| this.bg(extended.colors.nav_selected))
        .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
        .child(
            toolbar_action_button(
                id,
                label,
                Icon::new(icon)
                    .size(extended.icon.md)
                    .when(selected, |icon| icon.text_color(nav_icon_color(true, cx))),
                false,
                disabled,
                cx,
            )
            .on_click(on_click),
        )
}

impl Render for AssistantPanelHeader {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.view.read(cx);
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let open = matches!(view.gate(), Gate::Open { .. });
        let history = view.history_open;
        let stale = view.stale;
        let heading = view.heading(cx);
        let usage = open.then(|| view.usage(cx)).flatten();
        let new_chat_label = view.t("assistantNewChat", cx);
        let history_label = view.t("assistantHistory", cx);

        let new_chat_view = self.view.clone();
        let history_view = self.view.clone();
        h_flex()
            .size_full()
            .min_w_0()
            .items_center()
            .gap(tokens.spacing.xs)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.colors.foreground)
                    .child(heading),
            )
            .children(usage.map(|usage| {
                div()
                    .flex_none()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(extended.colors.text_tertiary)
                    .child(usage)
            }))
            .child(
                // 按钮拦截左键按下：否则冒泡到标题栏开始拖动窗口。
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(tokens.spacing.xxs)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(header_button(
                        "assistant-panel-new-chat",
                        new_chat_label,
                        FluxIcon::Plus,
                        false,
                        !open || stale,
                        move |_, window, cx| {
                            new_chat_view.update(cx, |view, cx| view.new_chat(window, cx));
                        },
                        cx,
                    ))
                    .child(header_button(
                        "assistant-panel-history",
                        history_label,
                        FluxIcon::Clock,
                        history,
                        !open,
                        move |_, _, cx| {
                            history_view.update(cx, |view, cx| view.toggle_history(cx));
                        },
                        cx,
                    )),
            )
    }
}
