//! 表头选择条：选中 ≥1 项时覆盖全选框右侧的列头，承载批量操作；取消选择后恢复列名。
//! 不遮挡任何任务行，紧挨刚点过的复选框，鼠标移动距离最短。

use fluxdown_ui_components::{
    FluxIcon, IconControlExt as _, tabular_numbers, toast, toolbar_action_button, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};
use gpui_component::{
    Icon, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
};

use crate::{
    components::task_table::{ToolbarCommand, table_header_height},
    deletion::{DeletionMode, DeletionRequest},
    model::{DownloadTaskView, RowKey, TaskState, TaskStore},
    pages::downloads::DownloadView,
};

fn push_task_info_cell(text: &mut String, value: &str, clipboard: bool) {
    // Keep spreadsheet pastes from interpreting untrusted text as formulas or quoted cells.
    if clipboard && value.trim_start().starts_with(['=', '+', '-', '@', '"']) {
        text.push('\'');
    }
    for ch in value.chars() {
        match ch {
            '\\' => text.push_str("\\\\"),
            '\t' => text.push_str("\\t"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            _ => text.push(ch),
        }
    }
}

const TASK_INFO_HEADER: &str = "Name\tURL\tSize (bytes)";

fn push_task_info_row(
    text: &mut String,
    name: &str,
    url: &str,
    size: Option<u64>,
    clipboard: bool,
) {
    text.push('\n');
    push_task_info_cell(text, name, clipboard);
    text.push('\t');
    push_task_info_cell(text, url, clipboard);
    text.push('\t');
    if let Some(size) = size {
        use std::fmt::Write as _;
        if let Err(error) = write!(text, "{size}") {
            eprintln!("could not format task size: {error}");
        }
    }
}

pub(crate) fn batch_link_info_text<'a>(
    entries: impl IntoIterator<Item = &'a crate::model::new_download::UrlEntry>,
) -> String {
    let mut text = TASK_INFO_HEADER.to_owned();
    for entry in entries {
        let name = batch_entry_name(entry);
        let size = batch_entry_size(&entry.url);
        push_task_info_row(&mut text, &name, &entry.url, size, false);
    }
    text
}

fn batch_entry_size(url: &str) -> Option<u64> {
    // Match the engine's eD2k wire fields, size ceiling, and MD4("") root for empty files.
    let mut fields = url.split('|');
    let (Some(scheme), Some(kind), Some(name), Some(size), Some(hash)) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return None;
    };
    if !scheme.eq_ignore_ascii_case("ed2k://")
        || !kind.eq_ignore_ascii_case("file")
        || name.is_empty()
        || hash.len() != 32
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    size.parse::<u64>().ok().filter(|&bytes| {
        bytes <= 0x40_0000_0000
            && (bytes != 0 || hash.eq_ignore_ascii_case("31d6cfe0d16ae931b73c59d7e0c089c0"))
    })
}

fn batch_entry_name(entry: &crate::model::new_download::UrlEntry) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;

    use crate::model::new_download::{percent_decode_utf8, url_file_segment};

    if !entry.file_name.is_empty() {
        return Cow::Borrowed(&entry.file_name);
    }
    let (scheme, rest) = entry.url.split_once(':').unwrap_or_default();
    let (raw_name, p2p) = if scheme.eq_ignore_ascii_case("magnet") {
        let query = rest.strip_prefix('?').unwrap_or_default();
        let query = query.split('#').next().unwrap_or_default();
        (
            query.split('&').find_map(|part| part.strip_prefix("dn=")),
            true,
        )
    } else if scheme.eq_ignore_ascii_case("ed2k") {
        let mut fields = rest.split('|');
        let name = match (fields.next(), fields.next()) {
            (Some("//"), Some(kind)) if kind.eq_ignore_ascii_case("file") => fields.next(),
            _ => None,
        };
        (name, true)
    } else if rest.starts_with("//")
        && ["http", "https", "ftp"]
            .into_iter()
            .any(|protocol| scheme.eq_ignore_ascii_case(protocol))
    {
        (url_file_segment(&entry.url), false)
    } else {
        (None, false)
    };
    let raw_name = raw_name.unwrap_or_default();
    let name = if raw_name.contains('%') {
        percent_decode_utf8(raw_name).map_or(Cow::Borrowed(raw_name), Cow::Owned)
    } else {
        Cow::Borrowed(raw_name)
    };
    if p2p && name.trim().is_empty() {
        Cow::Borrowed("")
    } else {
        name
    }
}

pub(crate) fn export_task_info_text(
    rows: impl IntoIterator<Item = (impl AsRef<str>, u64)>,
) -> String {
    let mut text = TASK_INFO_HEADER.to_owned();
    for (name, size) in rows {
        let size = (size > 0).then_some(size);
        push_task_info_row(&mut text, name.as_ref(), "", size, false);
    }
    text
}

fn task_info_text<'a>(
    rows: impl IntoIterator<Item = std::cell::Ref<'a, DownloadTaskView>>,
    clipboard: bool,
) -> Option<String> {
    let mut rows = rows.into_iter().peekable();
    rows.peek()?;
    let mut text = TASK_INFO_HEADER.to_owned();
    for row in rows {
        push_task_info_row(
            &mut text,
            &row.name,
            row.copyable_share_url().unwrap_or_default(),
            (row.size_bytes > 0).then_some(row.size_bytes),
            clipboard,
        );
    }
    Some(text)
}

pub(crate) fn save_task_info<T: 'static>(
    text: String,
    failed: String,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let directory = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
    let receiver = cx.prompt_for_new_path(&directory, Some("fluxdown-tasks.txt"));
    cx.spawn_in(window, async move |this, cx| {
        let error = match receiver.await {
            Ok(Ok(Some(path))) => cx
                .background_executor()
                .spawn(async move { std::fs::write(path, text) })
                .await
                .err()
                .map(|error| error.to_string()),
            Ok(Ok(None)) => return,
            Ok(Err(error)) => Some(format!("{error:#}")),
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = error
            && let Err(error) = this.update_in(cx, |_, window, cx| {
                window.push_notification(toast::error(format!("{failed}: {error}")), cx);
            })
        {
            eprintln!("window closed before reporting task export failure: {error:#}");
        }
    })
    .detach();
}

#[derive(Clone, Copy)]
pub(crate) enum TaskCleanup {
    Finished,
    InvalidSelection,
    UnfinishedSelection,
}

impl TaskCleanup {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Finished => "menuClearFinished",
            Self::InvalidSelection => "cleanStaleTasks",
            Self::UnfinishedSelection => "cleanUnfinishedSelection",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Finished => "cleanFinishedLocalDesc",
            Self::InvalidSelection => "cleanStaleTasksDesc",
            Self::UnfinishedSelection => "cleanUnfinishedSelectionDesc",
        }
    }

    fn matches(self, row: &DownloadTaskView) -> bool {
        match self {
            Self::Finished => {
                row.key.is_local() && row.state == TaskState::Completed && !row.is_seeding()
            }
            Self::InvalidSelection if row.key.is_local() => {
                row.state == TaskState::Failed || row.is_file_missing()
            }
            Self::InvalidSelection => {
                row.remote_status == Some(fluxdown_protocol::RemoteTaskStatus::Failed)
            }
            Self::UnfinishedSelection => {
                row.key.is_local() && matches!(row.state, TaskState::Paused | TaskState::Failed)
            }
        }
    }

    fn retain(self, store: &TaskStore, keys: &mut Vec<RowKey>) {
        keys.retain(|key| store.get(key).is_some_and(|row| self.matches(&row)));
    }
}

impl DownloadView {
    fn copy_selected_task_info(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self
            .table_state
            .read(cx)
            .delegate()
            .selected_rows_in_order();
        if let Some(text) = task_info_text(rows, true) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            let label = self.translator.read(cx).text("taskInfoCopied").to_owned();
            window.push_notification(toast::success(label), cx);
        }
    }

    fn export_selected_task_info(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self
            .table_state
            .read(cx)
            .delegate()
            .selected_rows_in_order();
        if let Some(text) = task_info_text(rows, false) {
            let failed = self
                .translator
                .read(cx)
                .text("taskInfoExportFailed")
                .to_owned();
            save_task_info(text, failed, window, cx);
        }
    }

    fn can_clean_tasks(&self) -> bool {
        !self.controller.is_stale() && !self.controller.tables_loading()
    }

    pub(crate) fn has_cleanup_targets(&self, scope: TaskCleanup, cx: &gpui::App) -> bool {
        if !self.can_clean_tasks() {
            return false;
        }
        let delegate = self.table_state.read(cx).delegate();
        let store = self.controller.store();
        match scope {
            TaskCleanup::Finished => store
                .local()
                .iter()
                .any(|row| scope.matches(row) && !delegate.is_pending_delete(&row.key)),
            _ => delegate
                .selected_key_refs()
                .any(|key| store.get(key).is_some_and(|row| scope.matches(&row))),
        }
    }

    pub(crate) fn confirm_task_cleanup(
        &mut self,
        scope: TaskCleanup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_clean_tasks() {
            return;
        }
        let store = self.controller.store();
        let delegate = self.table_state.read(cx).delegate();
        let keys = match scope {
            TaskCleanup::Finished => store
                .local()
                .iter()
                .filter(|row| scope.matches(row) && !delegate.is_pending_delete(&row.key))
                .map(|row| row.key.clone())
                .collect(),
            _ => {
                let mut keys = delegate.selected_keys();
                scope.retain(store, &mut keys);
                keys
            }
        };
        if keys.is_empty() {
            return;
        }
        let translator = self.translator.read(cx);
        let title = SharedString::from(translator.text(scope.label()).to_owned());
        let description =
            translator.text_with(scope.description(), &[("count", &keys.len().to_string())]);
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
                        if !this.can_clean_tasks() {
                            return false;
                        }
                        // Recheck the previewed candidates; never add tasks to an open dialog.
                        let mut keys = keys.clone();
                        scope.retain(this.controller.store(), &mut keys);
                        let delegate = this.table_state.read(cx).delegate();
                        keys.retain(|key| !delegate.is_pending_delete(key));
                        if keys.is_empty() {
                            return true;
                        }
                        if matches!(scope, TaskCleanup::UnfinishedSelection) {
                            this.start_deletion(
                                DeletionRequest {
                                    task_ids: keys
                                        .into_iter()
                                        .map(|key| key.task_id().to_owned())
                                        .collect(),
                                    mode: DeletionMode::Unfinished,
                                },
                                cx,
                            );
                        } else {
                            let commands = this.delete_commands(&keys, false);
                            this.execute_commands(commands, cx);
                        }
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
        let clean_stale_tasks = SharedString::from(
            self.translator
                .read(cx)
                .text(TaskCleanup::InvalidSelection.label())
                .to_owned(),
        );
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
                            !this.has_cleanup_targets(TaskCleanup::InvalidSelection, cx)
                        });
                        menu.item(
                            PopupMenuItem::element({
                                let label = delete_task.clone();
                                move |_, _| div().text_color(destructive).child(label.clone())
                            })
                            .icon(Icon::new(FluxIcon::Trash2).text_color(destructive))
                            .on_click({
                                let view = view.clone();
                                move |_, window, cx| {
                                    let Ok(()) = view.update(cx, |this, cx| {
                                        this.confirm_delete_selected(false, window, cx);
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
                                        this.confirm_delete_selected(true, window, cx);
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
                                            this.confirm_task_cleanup(
                                                TaskCleanup::InvalidSelection,
                                                window,
                                                cx,
                                            );
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
        let copy_label =
            SharedString::from(self.translator.read(cx).text("copyTaskInfo").to_owned());
        actions.push(self.icon_action(
            "download-selection-copy-info",
            copy_label,
            Icon::new(FluxIcon::Copy).size(icon_size),
            false,
            Self::copy_selected_task_info,
            cx,
        ));
        let export_label = SharedString::from(
            self.translator
                .read(cx)
                .text("exportTaskInfoTxt")
                .to_owned(),
        );
        actions.push(self.icon_action(
            "download-selection-export-info",
            export_label,
            Icon::new(FluxIcon::Download).size(icon_size),
            false,
            Self::export_selected_task_info,
            cx,
        ));
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
    use super::{TaskCleanup, batch_link_info_text, task_info_text};
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

    #[test]
    fn pr1241_remaining_cleanup_dialog_rechecks_task_status_at_confirmation() {
        use std::sync::{Arc, Mutex};

        use fluxdown_protocol::{AgentSnapshot, DaemonSnapshot, TaskDto};
        use fluxdown_ui_i18n::I18nCatalog;
        use gpui::{AppContext as _, TestApp};
        use gpui_component::{Root, WindowExt as _};

        use crate::{
            controller::{DownloadsCommand, DownloadsPort, DownloadsResult, PortFuture},
            pages::downloads::DownloadView,
        };

        #[derive(Default)]
        struct CleanupPort(Mutex<Vec<Vec<String>>>);

        impl DownloadsPort for CleanupPort {
            fn execute(&self, command: DownloadsCommand) -> PortFuture<DownloadsResult> {
                if let DownloadsCommand::CleanUnfinished { task_ids } = command {
                    let deleted = task_ids.len();
                    self.0.lock().unwrap().push(task_ids);
                    return Box::pin(
                        async move { Ok(DownloadsResult::CleanedUnfinished(deleted)) },
                    );
                }
                Box::pin(async { Ok(DownloadsResult::Unit) })
            }
        }

        let port = Arc::new(CleanupPort::default());
        let mut app = TestApp::new();
        let translator = app.update(|cx| {
            gpui_component::init(cx);
            fluxdown_ui_theme::init(cx);
            let catalog = I18nCatalog::load_embedded().unwrap();
            cx.new(|_| catalog.translator("en"))
        });
        let mut downloads = None;
        let mut win = app.open_window(|window, cx| {
            let view = cx.new(|cx| DownloadView::new(translator, port.clone(), window, cx));
            downloads = Some(view.clone());
            Root::new(view, window, cx)
        });
        let downloads = downloads.unwrap();
        let task = |id, status| {
            serde_json::from_value::<TaskDto>(serde_json::json!({
                "taskId": id, "url": "https://example.com/file", "fileName": "file.bin",
                "saveDir": "/tmp", "status": status, "downloadedBytes": 0, "totalBytes": 1,
                "errorMessage": "", "createdAt": "1", "proxyUrl": "", "queueId": "main",
                "checksum": ""
            }))
            .unwrap()
        };
        let mut snapshot = AgentSnapshot {
            daemon_connected: true,
            daemon: DaemonSnapshot {
                tasks: vec![task("still-paused", 2), task("now-complete", 4)],
                ..Default::default()
            },
            ..Default::default()
        };
        app.update(|cx| {
            cx.update_window(win.handle().into(), |_, window, cx| {
                downloads.update(cx, |view, cx| {
                    view.controller.replace_snapshot(&snapshot);
                    view.refresh_tasks(cx);
                    view.table_state.update(cx, |table, _| {
                        table.delegate_mut().select_all_tasks();
                    });
                    view.confirm_task_cleanup(TaskCleanup::UnfinishedSelection, window, cx);
                });
                assert!(window.has_active_dialog(cx));
            })
            .unwrap();
        });
        assert!(port.0.lock().unwrap().is_empty());
        snapshot.daemon.tasks[1].status = 3;
        app.update(|cx| {
            downloads.update(cx, |view, _| view.controller.replace_snapshot(&snapshot));
        });
        win.draw();
        win.simulate_keystroke("enter");
        assert_eq!(*port.0.lock().unwrap(), [vec!["still-paused".to_owned()]]);
    }

    fn keys(ids: &[&str]) -> Vec<RowKey> {
        ids.iter().map(|id| RowKey::Local((*id).into())).collect()
    }

    #[test]
    fn task_info_preserves_selected_rows_and_escapes_cell_boundaries() {
        let store = TaskStore::default();
        let mut known = local("known", 3, false);
        known.name = "=报告\tpart\n2".to_owned();
        known.origin_url = "https://example.com/original".to_owned();
        let mut unknown = local("unknown", 0, false);
        unknown.url = "torrent-file://local".to_owned();
        unknown.size_bytes = 0;
        store.replace_local(vec![known, unknown, local("unselected", 3, false)]);
        let selected = keys(&["known", "removed", "unknown"]);
        assert_eq!(
            task_info_text(selected.iter().filter_map(|key| store.get(key)), true).as_deref(),
            Some(
                "Name\tURL\tSize (bytes)\n'=报告\\tpart\\n2\thttps://example.com/original\t1\nfile.bin\t\t"
            )
        );
        assert_eq!(
            task_info_text(selected.iter().filter_map(|key| store.get(key)), false).as_deref(),
            Some(
                "Name\tURL\tSize (bytes)\n=报告\\tpart\\n2\thttps://example.com/original\t1\nfile.bin\t\t"
            )
        );
    }

    #[test]
    fn parsed_batch_exports_known_metadata_without_guessing_sizes() {
        let magnet = "MAGNET:?xt=urn:btih:a&dn=My+Show%21";
        let ed2k = "ED2K://|FILE|f%20x.iso|5044211712|1555B7DCA052B5958EE68DB58A42408D|/";
        let unnamed_magnet = "magnet:?xt=urn:btih:b";
        let unnamed_ed2k = "ed2k://|file||100|h|/";
        let renamed = "ed2k://|file|ignored|100|1555B7DCA052B5958EE68DB58A42408D|/";
        let empty = "ed2k://|file|empty.txt|0|31D6CFE0D16AE931B73C59D7E0C089C0|/";
        let wrong_hash = "ed2k://|file|invalid.txt|0|1555B7DCA052B5958EE68DB58A42408D|/";
        let p2p = crate::model::new_download::parse_entries(
            &format!(
                "{magnet}\n{ed2k}\n{unnamed_magnet}\n{unnamed_ed2k}\n{renamed}\n  out=keep%20name\n{empty}\n{wrong_hash}"
            ),
            false,
        );
        assert_eq!(
            batch_link_info_text(&p2p),
            format!(
                "Name\tURL\tSize (bytes)\nMy+Show!\t{magnet}\t\nf x.iso\t{ed2k}\t5044211712\n\t{unnamed_magnet}\t\n\t{unnamed_ed2k}\t\nkeep%20name\t{renamed}\t100\nempty.txt\t{empty}\t0\ninvalid.txt\t{wrong_hash}\t"
            )
        );

        let entries = crate::model::new_download::parse_entries(
            "https://example.com/a%20b.zip?key=v\nftp://example.com/f\n  out=报告.zip\nhttps://example.com/",
            false,
        );
        assert_eq!(
            batch_link_info_text(&entries),
            "Name\tURL\tSize (bytes)\na b.zip\thttps://example.com/a%20b.zip?key=v\t\n报告.zip\tftp://example.com/f\t\n\thttps://example.com/\t"
        );
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
        TaskCleanup::InvalidSelection.retain(&store, &mut selected);
        assert_eq!(selected, keys(&["failed", "missing"]));
        let mut empty = Vec::new();
        TaskCleanup::InvalidSelection.retain(&store, &mut empty);
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
        TaskCleanup::InvalidSelection.retain(&store, &mut candidates);
        store.replace_local(vec![
            local("resumed", 1, false),
            local("restored", 3, false),
            local("still-failed", 4, false),
            local("later-failed", 4, false),
        ]);
        TaskCleanup::InvalidSelection.retain(&store, &mut candidates);
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
        TaskCleanup::InvalidSelection.retain(&store, &mut selected);
        assert_eq!(selected, [RowKey::Remote("failed".into())]);
    }

    #[test]
    fn cleanup_keeps_seeding_and_active_tasks() {
        let store = TaskStore::default();
        let mut seeding = local("seeding", 3, false);
        seeding.seeding_status = 1;
        store.replace_local(vec![
            local("finished", 3, false),
            seeding,
            local("paused", 2, false),
            local("failed", 4, false),
            local("waiting", 0, false),
            local("active", 1, false),
        ]);
        let all = keys(&[
            "finished", "seeding", "paused", "failed", "waiting", "active",
        ]);
        let mut finished = all.clone();
        TaskCleanup::Finished.retain(&store, &mut finished);
        assert_eq!(finished, keys(&["finished"]));
        let mut unfinished = all;
        TaskCleanup::UnfinishedSelection.retain(&store, &mut unfinished);
        assert_eq!(unfinished, keys(&["paused", "failed"]));
    }
}
