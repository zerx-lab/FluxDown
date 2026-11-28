//! 右侧面板顶栏插槽：当前标题（点一下重命名）、今日用量、导出、新对话与历史会话切换。shell 在其
//! 右侧追加「收起」按钮（Windows / Linux 另有窗口按钮）；本视图随助手视图重绘。

use fluxdown_protocol::AssistantExportFormat;
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, FluxIcon, icon_button, nav_icon_color, toolbar_action_button,
    tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, App, ClickEvent, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Icon,
    button::{Button as ComponentButton, ButtonVariants as _},
    h_flex,
    input::Input,
    menu::{DropdownMenu as _, PopupMenuItem},
};

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
        let rename_label = view.t("assistantRename", cx);
        let export_label = view.t("assistantExport", cx);
        let markdown_label = view.t("assistantExportMarkdown", cx);
        let json_label = view.t("assistantExportJson", cx);
        let confirm_label = view.t("confirm", cx);
        let cancel_label = view.t("assistantCancel", cx);
        let renameable = view.can_rename_current();
        let current = view.current.clone();
        // 顶栏标题处正在重命名当前会话：换成输入框 + 确认 / 取消。
        let renaming = view
            .renaming
            .as_ref()
            .filter(|rename| !history && current.as_deref() == Some(rename.session_id.as_str()));
        let rename_input = renaming.map(|rename| rename.input.clone());
        let rename_busy = renaming.is_some_and(|rename| rename.busy);
        let rename_over = renaming.is_some() && view.rename_over_limit(cx);
        let exporting = current
            .as_deref()
            .is_some_and(|id| view.exporting.as_deref() == Some(id));
        let can_export =
            open && !history && !stale && current.is_some() && view.exporting.is_none();

        let new_chat_view = self.view.clone();
        let history_view = self.view.clone();
        let title_view = self.view.clone();
        let save_view = self.view.clone();
        let cancel_view = self.view.clone();
        let escape_view = self.view.clone();
        let export_view = self.view.downgrade();
        let title_id = current.clone();
        let export_id = current;

        let title: gpui::AnyElement = match rename_input {
            Some(input) => h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(tokens.spacing.xxs)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                // 顶栏插槽在 shell 标题栏下，不在面板的键位上下文里：Esc 在这里自己取消重命名。
                .on_key_down(move |event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        escape_view.update(cx, |view, cx| view.cancel_rename(cx));
                        cx.stop_propagation();
                    }
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&input).control(cx).w_full()),
                )
                .child(crate::render_history::rename_counter(view, cx))
                .child(
                    icon_button(
                        "assistant-header-rename-save",
                        confirm_label,
                        Icon::new(FluxIcon::Check)
                            .size(extended.icon.sm)
                            .text_color(extended.colors.success),
                        ButtonVariant::Ghost,
                        cx,
                    )
                    .disabled(rename_busy || stale || rename_over)
                    .on_click(move |_, _, cx| {
                        save_view.update(cx, |view, cx| view.commit_rename(cx));
                    }),
                )
                .child(
                    icon_button(
                        "assistant-header-rename-cancel",
                        cancel_label,
                        Icon::new(FluxIcon::X).size(extended.icon.sm),
                        ButtonVariant::Ghost,
                        cx,
                    )
                    .disabled(rename_busy)
                    .on_click(move |_, _, cx| {
                        cancel_view.update(cx, |view, cx| view.cancel_rename(cx));
                    }),
                )
                .into_any_element(),
            None => div()
                .id("assistant-panel-title")
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(tokens.typography.sm.size)
                .line_height(tokens.typography.sm.line_height)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tokens.colors.foreground)
                .when(renameable, |this| {
                    let tip = rename_label.clone();
                    this.cursor_pointer()
                        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            let Some(id) = title_id.clone() else {
                                return;
                            };
                            title_view.update(cx, |view, cx| view.start_rename(id, window, cx));
                        })
                })
                .child(heading)
                .into_any_element(),
        };

        h_flex()
            .size_full()
            .min_w_0()
            .items_center()
            .gap(tokens.spacing.xs)
            .child(title)
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
                    .when(open && !history && export_id.is_some(), |this| {
                        let theme_radius = theme.components().button_radius;
                        let button = gpui::Styled::size(
                            ComponentButton::new("assistant-panel-export")
                                .ghost()
                                .rounded(theme_radius)
                                .text_color(tokens.colors.muted_foreground)
                                .tooltip(export_label.clone())
                                .disabled(!can_export || exporting)
                                .child(Icon::new(FluxIcon::Download).size(extended.icon.md)),
                            theme.density().toolbar_button,
                        );
                        this.child(button.dropdown_menu_with_anchor(
                            Anchor::TopRight,
                            move |menu, _, _| {
                                let item = |label: SharedString,
                                                        icon: FluxIcon,
                                                        format: AssistantExportFormat| {
                                                let view = export_view.clone();
                                                let id = export_id.clone();
                                                PopupMenuItem::new(label).icon(icon).on_click(
                                                    move |_, window, cx| {
                                                        let Some(id) = id.clone() else {
                                                            return;
                                                        };
                                                        let Ok(()) = view.update(cx, |this, cx| {
                                                            this.export_session(
                                                                id, format, window, cx,
                                                            );
                                                        }) else {
                                                            // 视图已释放，不再发起导出。
                                                            return;
                                                        };
                                                    },
                                                )
                                            };
                                menu.item(item(
                                    markdown_label.clone(),
                                    FluxIcon::FileText,
                                    AssistantExportFormat::Markdown,
                                ))
                                .item(item(
                                    json_label.clone(),
                                    FluxIcon::Code,
                                    AssistantExportFormat::Json,
                                ))
                            },
                        ))
                    })
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
                        move |_, window, cx| {
                            history_view.update(cx, |view, cx| view.toggle_history(window, cx));
                        },
                        cx,
                    )),
            )
    }
}
