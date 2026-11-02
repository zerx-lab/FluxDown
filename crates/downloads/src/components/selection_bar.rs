//! 表头选择条：选中 ≥1 项时覆盖全选框右侧的列头，承载批量操作；取消选择后恢复列名。
//! 不遮挡任何任务行，紧挨刚点过的复选框，鼠标移动距离最短。

use fluxdown_ui_components::{
    FluxIcon, IconControlExt as _, tabular_numbers, toolbar_action_button, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};
use gpui_component::{
    Icon, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
};

use crate::{
    components::task_table::{ToolbarCommand, table_header_height},
    model::{RowKey, TaskState, TaskStore},
    pages::downloads::DownloadView,
};

fn retain_stale_tasks(store: &TaskStore, keys: &mut Vec<RowKey>) {
    keys.retain(|key| {
        store.get(key).is_some_and(|row| {
            if key.is_local() {
                row.state == TaskState::Failed || row.is_file_missing()
            } else {
                row.remote_status == Some(fluxdown_protocol::RemoteTaskStatus::Failed)
            }
        })
    });
}

impl DownloadView {
    fn selected_stale_keys(&self, cx: &gpui::App) -> Vec<RowKey> {
        let mut keys = self.table_state.read(cx).delegate().selected_keys();
        retain_stale_tasks(self.controller.store(), &mut keys);
        keys
    }

    fn confirm_clean_stale_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.selected_stale_keys(cx);
        if keys.is_empty() || self.controller.is_stale() {
            return;
        }
        let title = self.strings.clean_stale_tasks.clone();
        let description = self
            .strings
            .clean_stale_tasks_description
            .replace("{count}", &keys.len().to_string());
        let ok_label = self.strings.confirm.clone();
        let cancel_label = self.strings.cancel.clone();
        let view = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let view = view.clone();
            let keys = keys.clone();
            dialog
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(description.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel_label.clone()),
                    ok_label.clone(),
                    fluxdown_ui_components::DialogIntent::Destructive,
                    cx,
                ))
                .on_ok(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        if this.controller.is_stale() {
                            return false;
                        }
                        // 固定确认时的候选集合，跳过期间已恢复下载或文件已找回的任务。
                        let mut keys = keys.clone();
                        retain_stale_tasks(this.controller.store(), &mut keys);
                        let commands = this.delete_commands(&keys, false);
                        this.execute_commands(commands, cx);
                        true
                    })
                    .unwrap_or(false)
                })
        });
    }

    /// chrome 风格图标按钮 + 悬浮提示；`on_click` 在下载页上下文执行。
    pub(crate) fn icon_action(
        &self,
        id: &'static str,
        label: SharedString,
        icon: Icon,
        destructive: bool,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.icon_action_with_state(id, label, icon, destructive, false, on_click, cx)
    }

    /// [`Self::icon_action`] 的可禁用版本：`disabled` 时保留悬浮提示但不响应点击。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn icon_action_with_state(
        &self,
        id: &'static str,
        label: SharedString,
        icon: Icon,
        destructive: bool,
        disabled: bool,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tooltip_label = label.clone();
        div()
            .id(SharedString::from(format!("{id}-tooltip")))
            .flex_none()
            .tooltip(move |window, cx| tooltip_view(tooltip_label.clone(), window, cx))
            .child(
                toolbar_action_button(id, label, icon, destructive, disabled, cx)
                    .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx))),
            )
            .into_any_element()
    }

    /// 删除入口：普通删除、删除文件，以及只清理选区内的无效任务。
    fn selection_delete_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let radius = theme.components().button_radius;
        let destructive = tokens.colors.destructive;
        let icon_size = theme.extended().icon.md;
        let delete_task = self.strings.delete_task.clone();
        let delete_with_files = self.strings.delete_task_and_file.clone();
        let clean_stale_tasks = self.strings.clean_stale_tasks.clone();
        let view = cx.weak_entity();
        div()
            .flex_none()
            .child(
                Button::new("download-selection-delete")
                    .ghost()
                    .control_icon(cx)
                    .rounded(radius)
                    .tooltip(self.strings.delete.clone())
                    .child(
                        Icon::new(FluxIcon::Trash2)
                            .size(icon_size)
                            .text_color(destructive),
                    )
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, cx| {
                        let cleanup_disabled = view.upgrade().is_none_or(|view| {
                            let this = view.read(cx);
                            this.controller.is_stale() || this.selected_stale_keys(cx).is_empty()
                        });
                        menu.item(
                            PopupMenuItem::element({
                                let label = delete_task.clone();
                                move |_, _| div().text_color(destructive).child(label.clone())
                            })
                            .icon(Icon::new(FluxIcon::Trash2).text_color(destructive))
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    let Ok(()) = view.update(cx, |this, cx| {
                                        this.execute_toolbar(ToolbarCommand::Delete, cx);
                                    }) else {
                                        // 视图已释放，结束这次回调而不再更新状态。
                                        return;
                                    };
                                }
                            }),
                        )
                        .item(
                            PopupMenuItem::element({
                                let label = delete_with_files.clone();
                                move |_, _| div().text_color(destructive).child(label.clone())
                            })
                            .icon(Icon::new(FluxIcon::Trash2).text_color(destructive))
                            .on_click({
                                let view = view.clone();
                                move |_, window, cx| {
                                    let Ok(()) = view.update(cx, |this, cx| {
                                        this.delete_selected_with_files(window, cx);
                                    }) else {
                                        // 视图已释放，结束这次回调而不再更新状态。
                                        return;
                                    };
                                }
                            }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(clean_stale_tasks.clone())
                                .icon(FluxIcon::Trash2)
                                .disabled(cleanup_disabled)
                                .on_click({
                                    let view = view.clone();
                                    move |_, window, cx| {
                                        let Ok(()) = view.update(cx, |this, cx| {
                                            this.confirm_clean_stale_tasks(window, cx);
                                        }) else {
                                            // 视图已释放，不再发起清理。
                                            return;
                                        };
                                    }
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    /// 表头选择条（覆盖在表格容器顶部、选择列右侧）；仅显式多选模式（Cmd/Ctrl+点击、全选）
    /// 出现，普通单击选中不出现（单项操作走右键菜单）。覆盖期间列头的排序 / 拖宽被 `occlude` 拦下，全选框仍可用。
    pub(crate) fn render_selection_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let delegate = self.table_state.read(cx).delegate();
        let selection = delegate.selection_summary();
        let multi_select = delegate.in_multi_select();
        let selection_column_width = delegate.selection_column_width();
        self.selection_summary.set(selection);
        if selection.count == 0 || !multi_select {
            return None;
        }

        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let spacing = tokens.spacing;
        let surface = tokens.colors.surface;
        let foreground = tokens.colors.foreground;
        let text_size = tokens.typography.xs.size;
        let line_height = tokens.typography.xs.line_height;
        let hairline = theme.extended().colors.hairline;
        let stroke = theme.extended().stroke.thin;
        let icon_size = theme.extended().icon.md;
        let count_label = SharedString::from(
            self.translator
                .read(cx)
                .text_with("selectedCount", &[("n", &selection.count.to_string())]),
        );
        let clear_label =
            SharedString::from(self.translator.read(cx).text("deselectAll").to_owned());
        // 竖分隔与按钮同一垂直中线：高取 icon.lg（16），左右留白与按钮间距一致。
        let separator_height = theme.extended().icon.lg;
        let separator = move || {
            div()
                .flex_none()
                .w(stroke)
                .h(separator_height)
                .mx(spacing.xs)
                .bg(hairline)
        };

        let mut actions: Vec<AnyElement> = Vec::new();
        if selection.any_resumable {
            actions.push(self.icon_action(
                "download-selection-resume",
                self.strings.resume.clone(),
                Icon::new(FluxIcon::Play).size(icon_size),
                false,
                |this, _, cx| this.execute_toolbar(ToolbarCommand::Resume, cx),
                cx,
            ));
        }
        if selection.any_active {
            actions.push(self.icon_action(
                "download-selection-pause",
                self.strings.pause.clone(),
                Icon::new(FluxIcon::Pause).size(icon_size),
                false,
                |this, _, cx| this.execute_toolbar(ToolbarCommand::Pause, cx),
                cx,
            ));
        }
        if selection.any_local {
            actions.push(self.icon_action_with_state(
                "download-selection-open",
                self.strings.open_file.clone(),
                Icon::new(FluxIcon::ExternalLink).size(icon_size),
                false,
                !selection.all_openable,
                |this, _, cx| this.execute_toolbar(ToolbarCommand::Open, cx),
                cx,
            ));
            actions.push(self.icon_action(
                "download-selection-reveal",
                self.strings.open_folder.clone(),
                Icon::new(FluxIcon::FolderOpen).size(icon_size),
                false,
                |this, _, cx| this.execute_toolbar(ToolbarCommand::Reveal, cx),
                cx,
            ));
        }
        actions.push(self.selection_delete_menu(cx));
        let clear = self.icon_action(
            "download-selection-clear",
            clear_label,
            Icon::new(FluxIcon::X).size(icon_size),
            false,
            |this, _, cx| this.clear_table_selection(cx),
            cx,
        );

        Some(
            h_flex()
                .id("download-selection-bar")
                .occlude()
                .absolute()
                .top_0()
                .left(px(selection_column_width))
                .right_0()
                // 留出表头底部分隔线。
                .h(table_header_height(cx) - stroke)
                .pl(spacing.sm)
                .gap(spacing.xxs)
                .items_center()
                .bg(surface)
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(text_size)
                        .line_height(line_height)
                        .font_features(tabular_numbers())
                        .text_color(foreground)
                        .child(count_label),
                )
                .child(separator())
                .children(actions)
                .child(separator())
                .child(clear)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::retain_stale_tasks;
    use crate::model::{DownloadTaskView, RowKey, TaskStore};

    fn local(id: &str, status: i32, missing: bool) -> DownloadTaskView {
        let dto = serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
            "taskId": id, "url": "https://example.com/file", "fileName": "file.bin",
            "saveDir": "/tmp", "status": status, "downloadedBytes": 0, "totalBytes": 1,
            "errorMessage": "", "createdAt": "1", "proxyUrl": "", "queueId": "main",
            "checksum": "", "fileMissing": missing
        }))
        .expect("task");
        DownloadTaskView::local(&dto, None, false)
    }

    fn keys(ids: &[&str]) -> Vec<RowKey> {
        ids.iter().map(|id| RowKey::Local((*id).into())).collect()
    }

    #[test]
    fn cleanup_filters_selection_without_touching_healthy_or_active_tasks() {
        let store = TaskStore::default();
        store.replace_local(vec![
            local("failed", 4, false),
            local("missing", 3, true),
            local("complete", 3, false),
            local("paused", 2, true),
            local("downloading", 1, true),
            local("pending", 0, true),
            local("preparing", 5, true),
            local("unselected", 4, false),
        ]);
        let mut selected = keys(&[
            "failed",
            "missing",
            "complete",
            "paused",
            "downloading",
            "pending",
            "preparing",
            "removed",
        ]);
        retain_stale_tasks(&store, &mut selected);
        assert_eq!(selected, keys(&["failed", "missing"]));
        let mut empty = Vec::new();
        retain_stale_tasks(&store, &mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn cleanup_rechecks_candidates_without_adding_new_failures() {
        let store = TaskStore::default();
        store.replace_local(vec![
            local("resumed", 4, false),
            local("restored", 3, true),
            local("still-failed", 4, false),
            local("later-failed", 1, false),
            local("removed", 4, false),
        ]);
        let mut candidates = keys(&[
            "resumed",
            "restored",
            "still-failed",
            "later-failed",
            "removed",
        ]);
        retain_stale_tasks(&store, &mut candidates);
        store.replace_local(vec![
            local("resumed", 1, false),
            local("restored", 3, false),
            local("still-failed", 4, false),
            local("later-failed", 4, false),
        ]);
        retain_stale_tasks(&store, &mut candidates);
        assert_eq!(candidates, keys(&["still-failed"]));
    }

    #[test]
    fn cleanup_remote_tasks_requires_explicit_failed_status() {
        let store = TaskStore::default();
        let rows: Vec<_> = [
            "failed",
            "canceled",
            "completed",
            "paused",
            "downloading",
            "pending",
            "new-status",
        ]
        .into_iter()
        .map(|status| {
            let dto =
                serde_json::from_value::<fluxdown_protocol::RemoteTaskDto>(serde_json::json!({
                    "id": status, "url": "https://example.com/file", "status": status,
                }))
                .expect("remote task");
            DownloadTaskView::remote(&dto)
        })
        .collect();
        let mut selected = rows.iter().map(|row| row.key.clone()).collect();
        store.replace_remote(rows);
        retain_stale_tasks(&store, &mut selected);
        assert_eq!(selected, [RowKey::Remote("failed".into())]);
    }
}
