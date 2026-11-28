//! 助手页历史视图：搜索、会话列表（重命名、导出、删除、键盘光标）与待确认提示。

use fluxdown_protocol::{AssistantExportFormat, AssistantSessionDto};
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, FluxIcon, button, icon_button, loading_button, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, transparent_black,
};
use gpui_component::{
    Disableable as _, Icon,
    button::{Button as ComponentButton, ButtonVariants as _},
    h_flex,
    input::Input,
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    v_flex,
};

use crate::{
    actions::HISTORY_KEY_CONTEXT,
    dates::{SessionDate, session_date},
    history::{Load, pending_per_session},
    model::pending_elsewhere,
    view::AssistantView,
    widgets::{badge, body_text, compact_button, meta_text, spinner},
};

impl AssistantView {
    /// 历史视图：云端会话列表铺满面板；选中会话即回到对话视图。
    pub(crate) fn render_history(&self, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let now_ms = self.agent_now();
        let pending = pending_per_session(&self.state, now_ms);
        let visible = self.visible_sessions();
        let loading_first = matches!(self.sessions.load, Load::Idle | Load::Loading)
            && self.sessions.items.is_empty();
        let failed = self.sessions.load == Load::Failed;

        // 列表区：行必须是滚动容器的直接子元素（键盘光标按序号滚动到可见）。
        let mut list = v_flex()
            .id("assistant-history-list")
            .key_context(HISTORY_KEY_CONTEXT)
            .track_focus(&self.history_focus)
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.history_scroll)
            .px(tokens.spacing.sm)
            .pb(tokens.spacing.sm)
            .gap(tokens.spacing.xxs);
        if loading_first {
            list = list.child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .p(tokens.spacing.md)
                    .child(spinner(ext.icon.sm))
                    .child(meta_text(cx).child(self.t("assistantSessionsLoading", cx))),
            );
        } else if failed && self.sessions.items.is_empty() {
            list = list.child(
                v_flex()
                    .items_start()
                    .gap(tokens.spacing.sm)
                    .p(tokens.spacing.md)
                    .child(meta_text(cx).child(self.t("assistantSessionsLoadFailed", cx)))
                    .child(self.retry_button(cx)),
            );
        } else {
            if visible.is_empty() {
                let key = if self.search_text.trim().is_empty() {
                    "assistantSessionsEmpty"
                } else {
                    "assistantSearchEmpty"
                };
                list = list.child(
                    div()
                        .p(tokens.spacing.md)
                        .child(meta_text(cx).child(self.t(key, cx))),
                );
            }
            for index in &visible {
                if let Some(session) = self.sessions.items.get(*index) {
                    let count = pending
                        .iter()
                        .find(|(id, _)| *id == session.id)
                        .map_or(0, |(_, count)| *count);
                    list = list.child(self.render_session_row(session, count, cx));
                }
            }
            if self.sessions.has_more() {
                list = list.child(
                    loading_button(
                        "assistant-sessions-more",
                        self.t("assistantLoadMore", cx),
                        ButtonVariant::Ghost,
                        self.sessions.more_loading,
                        cx,
                    )
                    .disabled(self.stale || self.sessions.more_loading)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.load_more_sessions(cx);
                    })),
                );
            }
        }

        let mut column = v_flex().size_full().min_h_0().child(
            v_flex()
                .flex_none()
                .gap(tokens.spacing.sm)
                .p(tokens.spacing.md)
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap(tokens.spacing.xs)
                        .child(
                            meta_text(cx)
                                .font_weight(FontWeight::MEDIUM)
                                .child(self.t("assistantSessionsTitle", cx)),
                        )
                        .when(!self.sessions.items.is_empty(), |this| {
                            this.child(self.render_clear_button(cx))
                        }),
                )
                .child(
                    Input::new(&self.search)
                        .control(cx)
                        .w_full()
                        .cleanable(true)
                        .prefix(
                            Icon::new(FluxIcon::Search)
                                .size(ext.icon.md)
                                .text_color(ext.colors.text_tertiary),
                        ),
                ),
        );
        if self.stale {
            column = column.child(self.banner(self.t("localServiceDisconnected", cx), None, cx));
        }
        if let Some(first) = pending_elsewhere(&self.state, None, now_ms) {
            let total: usize = pending.iter().map(|(_, count)| *count).sum();
            let open = button(
                "assistant-history-pending-open",
                self.t("assistantPendingElsewhereOpen", cx),
                ButtonVariant::Secondary,
                cx,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_session(first.clone(), window, cx);
            }))
            .into_any_element();
            column = column.child(self.banner(
                SharedString::from(self.with(
                    "assistantPendingBadge",
                    &[("n", &total.to_string())],
                    cx,
                )),
                Some(open),
                cx,
            ));
        }
        if failed && !self.sessions.items.is_empty() {
            column = column.child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .px(tokens.spacing.md)
                    .pb(tokens.spacing.xs)
                    .child(
                        meta_text(cx)
                            .flex_1()
                            .min_w_0()
                            .text_color(tokens.colors.destructive)
                            .child(self.t("assistantSessionsLoadFailed", cx)),
                    )
                    .child(self.retry_button(cx)),
            );
        }
        if let Some(key) = self.history_error {
            column = column.child(
                div()
                    .flex_none()
                    .px(tokens.spacing.md)
                    .pb(tokens.spacing.xs)
                    .child(fluxdown_ui_components::field_error(self.t(key, cx), cx)),
            );
        }
        column
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(list)
                    .vertical_scrollbar(&self.history_scroll),
            )
            .into_any_element()
    }

    fn retry_button(&self, cx: &Context<Self>) -> AnyElement {
        button(
            "assistant-sessions-retry",
            self.t("assistantRetry", cx),
            ButtonVariant::Secondary,
            cx,
        )
        .disabled(self.stale)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.reload_sessions(cx);
        }))
        .into_any_element()
    }

    /// 「清空」：点一次后同一位置变为危险态的「确认清空」，再点才执行；鼠标移出、Esc 或超时即取消。
    fn render_clear_button(&self, cx: &Context<Self>) -> AnyElement {
        let confirming = self.clear_pending;
        let label = if self.clearing {
            self.t("assistantClearingSessions", cx)
        } else if confirming {
            self.t("assistantClearSessionsAgain", cx)
        } else {
            self.t("assistantClearSessions", cx)
        };
        let button = compact_button("assistant-sessions-clear", label, confirming, cx)
            .disabled(self.stale || self.clearing)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if this.clear_pending {
                    this.clear_sessions(cx);
                } else {
                    this.request_clear(window, cx);
                }
            }));
        div()
            .id("assistant-sessions-clear-zone")
            .flex_none()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.cancel_clear(cx);
                }
            }))
            .child(button)
            .into_any_element()
    }

    /// 会话日期：今天 / 昨天 / 本机时区的日期。
    fn session_date_label(&self, raw: &str, cx: &Context<Self>) -> SharedString {
        match session_date(raw, &self.now_local) {
            SessionDate::Today => self.t("assistantSessionDateToday", cx),
            SessionDate::Yesterday => self.t("assistantSessionDateYesterday", cx),
            SessionDate::Day(day) => SharedString::from(day),
        }
    }

    fn render_session_row(
        &self,
        session: &AssistantSessionDto,
        pending: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = theme.extended();
        let id = session.id.clone();
        let selected = self.current.as_deref() == Some(id.as_str());
        let cursor = self.history_cursor.as_deref() == Some(id.as_str());
        let confirming = self.delete_pending.as_deref() == Some(id.as_str());
        let deleting = self.deleting.contains(&id);
        let exporting = self.exporting.as_deref() == Some(id.as_str());
        let renaming = self
            .renaming
            .as_ref()
            .filter(|rename| rename.session_id == id);
        let title = {
            let title = session.title.trim();
            if title.is_empty() {
                self.t("assistantUntitled", cx)
            } else {
                SharedString::from(title.to_owned())
            }
        };
        let date = self.session_date_label(&session.updated_at, cx);
        let running = self
            .state
            .runs
            .iter()
            .any(|run| run.session_id == id && run.status.is_active());
        let feedback = session.kind == "feedback";
        let hover_id = id.clone();
        let button_side = theme.density().status_control;

        // 行 = 标题 / 日期 + 操作按钮。点删除后同一位置的按钮变为危险态，再点一次才删除（无需移动鼠标）；
        // 鼠标移出该行、Esc 或超时即取消。确认中日期行改为提示文字。
        let row = v_flex()
            .id(SharedString::from(format!("assistant-session-row-{id}")))
            .w_full()
            .rounded(tokens.radius.md)
            .border(ext.stroke.thin)
            .border_color(if cursor {
                tokens.colors.ring
            } else {
                transparent_black()
            })
            .when(selected, |this| this.bg(ext.colors.nav_selected))
            .when(!selected, |this| {
                this.hover(|style| style.bg(ext.colors.nav_hover))
            })
            .when(confirming, |this| {
                this.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if !*hovered {
                        this.cancel_delete(&hover_id, cx);
                    }
                }))
            });

        if let Some(rename) = renaming {
            let busy = rename.busy;
            let save = icon_button(
                SharedString::from(format!("assistant-session-rename-save-{id}")),
                self.t("confirm", cx),
                Icon::new(FluxIcon::Check).size(ext.icon.sm),
                ButtonVariant::Ghost,
                cx,
            )
            .size(button_side)
            .text_color(ext.colors.success)
            .disabled(self.stale || busy || self.rename_over_limit(cx))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_rename(cx)));
            let cancel = icon_button(
                SharedString::from(format!("assistant-session-rename-cancel-{id}")),
                self.t("assistantCancel", cx),
                Icon::new(FluxIcon::X).size(ext.icon.sm),
                ButtonVariant::Ghost,
                cx,
            )
            .size(button_side)
            .disabled(busy)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_rename(cx)));
            return row
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(tokens.spacing.xxs)
                        .px(tokens.spacing.xs)
                        .py(tokens.spacing.xs)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&rename.input).control(cx).w_full()),
                        )
                        .child(rename_counter(self, cx))
                        .child(if busy {
                            spinner(ext.icon.sm).into_any_element()
                        } else {
                            save.into_any_element()
                        })
                        .child(cancel),
                )
                .into_any_element();
        }

        let label = v_flex()
            .flex_1()
            .min_w_0()
            .gap(tokens.spacing.xxs)
            .child(
                body_text(cx)
                    .truncate()
                    .when(selected, |this| {
                        this.text_color(ext.colors.nav_selected_foreground)
                    })
                    .child(title),
            )
            .child(if confirming {
                meta_text(cx)
                    .truncate()
                    .text_color(tokens.colors.destructive)
                    .child(self.t("assistantDeleteAgain", cx))
            } else {
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .child(meta_text(cx).child(date))
                    .when(feedback, |this| {
                        this.child(meta_text(cx).child(self.t("assistantFeedbackBadge", cx)))
                    })
                    .when(pending > 0, |this| {
                        this.child(
                            badge(cx)
                                .bg(ext.colors.warning.opacity(0.15))
                                .text_color(ext.colors.warning)
                                .child(self.with(
                                    "assistantPendingBadge",
                                    &[("n", &pending.to_string())],
                                    cx,
                                )),
                        )
                    })
                    .when(running, |this| this.child(spinner(ext.icon.sm)))
            });

        let rename_button = {
            let rename_id = id.clone();
            icon_button(
                SharedString::from(format!("assistant-session-rename-{id}")),
                self.t("assistantRename", cx),
                Icon::new(FluxIcon::Pen).size(ext.icon.sm),
                ButtonVariant::Ghost,
                cx,
            )
            .size(button_side)
            .tooltip({
                let label = self.t("assistantRename", cx);
                move |window, cx| tooltip_view(label.clone(), window, cx)
            })
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.start_rename(rename_id.clone(), window, cx);
            }))
        };

        let export_button = self.export_menu_button(&id, exporting, button_side, cx);

        // 删除按钮收成状态栏控件见方；确认态只换危险色图标 + 极淡危险底，不整块红色填充；
        // 请求在途时换成转圈并禁用，避免重复点击。
        let delete_id = id.clone();
        let delete: AnyElement = if deleting {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(button_side)
                .child(spinner(ext.icon.sm))
                .into_any_element()
        } else if confirming {
            icon_button(
                SharedString::from(format!("assistant-session-delete-yes-{id}")),
                self.t("assistantDeleteAgain", cx),
                Icon::new(FluxIcon::Trash2).size(ext.icon.sm),
                ButtonVariant::Ghost,
                cx,
            )
            .size(button_side)
            .text_color(tokens.colors.destructive)
            .bg(tokens.colors.destructive.opacity(0.12))
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.delete_session(delete_id.clone(), cx);
            }))
            .into_any_element()
        } else {
            icon_button(
                SharedString::from(format!("assistant-session-delete-{id}")),
                self.t("assistantDelete", cx),
                Icon::new(FluxIcon::Trash2).size(ext.icon.sm),
                ButtonVariant::Ghost,
                cx,
            )
            .size(button_side)
            .tooltip({
                let label = self.t("assistantDelete", cx);
                move |window, cx| tooltip_view(label.clone(), window, cx)
            })
            .disabled(self.stale)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.request_delete(delete_id.clone(), window, cx);
            }))
            .into_any_element()
        };

        let select_id = id.clone();
        row.child(
            h_flex()
                .w_full()
                .items_center()
                .gap(tokens.spacing.xxs)
                .child(
                    div()
                        .id(SharedString::from(format!("assistant-session-{id}")))
                        .flex_1()
                        .min_w_0()
                        .px(tokens.spacing.sm)
                        .py(tokens.spacing.xs)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.select_session(select_id.clone(), window, cx);
                        }))
                        .child(label),
                )
                .child(rename_button)
                .child(export_button)
                .child(delete),
        )
        .into_any_element()
    }

    /// 导出按钮：下拉选 Markdown / JSON；导出进行中换成转圈。
    pub(crate) fn export_menu_button(
        &self,
        session_id: &str,
        exporting: bool,
        side: gpui::Pixels,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let ext = theme.extended();
        if exporting {
            return div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(side)
                .child(spinner(ext.icon.sm))
                .into_any_element();
        }
        let label = self.t("assistantExport", cx);
        let markdown_label = self.t("assistantExportMarkdown", cx);
        let json_label = self.t("assistantExportJson", cx);
        let view = cx.weak_entity();
        let id = session_id.to_owned();
        let button = Styled::size(
            ComponentButton::new(SharedString::from(format!(
                "assistant-session-export-{session_id}"
            )))
            .ghost()
            .rounded(theme.components().button_radius)
            .text_color(theme.tokens().colors.muted_foreground)
            .tooltip(label)
            .disabled(self.stale || self.exporting.is_some())
            .child(Icon::new(FluxIcon::Download).size(ext.icon.sm)),
            side,
        );
        button
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let item = |label: SharedString, icon: FluxIcon, format: AssistantExportFormat| {
                    let view = view.clone();
                    let id = id.clone();
                    PopupMenuItem::new(label)
                        .icon(icon)
                        .on_click(move |_, window, cx| {
                            let Ok(()) = view.update(cx, |this, cx| {
                                this.export_session(id.clone(), format, window, cx);
                            }) else {
                                // 视图已释放，不再发起导出。
                                return;
                            };
                        })
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
            })
            .into_any_element()
    }
}

/// 重命名输入框旁的字数提示（`assistantRenameCount`）；超过上限用危险色。
pub(crate) fn rename_counter(view: &AssistantView, cx: &gpui::App) -> AnyElement {
    let chars = view.rename_chars(cx);
    let max = fluxdown_protocol::ASSISTANT_MAX_TITLE_CHARS;
    meta_text(cx)
        .flex_none()
        .when(chars > max, |this| {
            this.text_color(active_theme(cx).tokens().colors.destructive)
        })
        .child(view.with(
            "assistantRenameCount",
            &[("count", &chars.to_string()), ("max", &max.to_string())],
            cx,
        ))
        .into_any_element()
}
