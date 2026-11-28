//! Nonblocking duplicate hints for the first parsed link and the host's default directory.

use std::time::Duration;

use fluxdown_protocol::{DaemonEvent, TaskDto, WsServerMsg};
use fluxdown_ui_components::{field_hint, tooltip_view};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _,
    StatefulInteractiveElement as _, Styled as _, Task, Window,
};

use super::{DuplicateScope, NewDownloadView};
use crate::controller::{DownloadsCommand, DownloadsResult};

const FILE_CHECK_DEBOUNCE: Duration = Duration::from_millis(250);

pub(super) struct DuplicateHint {
    scope: DuplicateScope,
    task_url: String,
    task: Option<TaskMatch>,
    file: Option<FileCheck>,
    request: Option<Task<()>>,
    generation: u64,
}

struct TaskMatch {
    id: String,
    name: String,
    by_origin: bool,
}

struct FileCheck {
    url: String,
    name: String,
    default_dir: String,
    exists: bool,
}

impl DuplicateHint {
    pub(super) fn new(scope: DuplicateScope) -> Self {
        Self {
            scope,
            task_url: String::new(),
            task: None,
            file: None,
            request: None,
            generation: 0,
        }
    }

    fn clear_file(&mut self) -> bool {
        let Some(check) = self.file.take() else {
            return false;
        };
        self.request = None;
        self.generation = self.generation.wrapping_add(1);
        check.exists
    }

    fn set_task(&mut self, task: Option<&TaskDto>, link: &str) -> bool {
        match task {
            Some(task) => {
                if let Some(current) = &mut self.task
                    && current.id == task.task_id
                {
                    current.by_origin = task.origin_url == link;
                    if current.name == task.file_name {
                        return false;
                    }
                    current.name.clone_from(&task.file_name);
                } else {
                    self.task = Some(TaskMatch {
                        id: task.task_id.clone(),
                        name: task.file_name.clone(),
                        by_origin: task.origin_url == link,
                    });
                }
                true
            }
            None => self.task.take().is_some(),
        }
    }
}

fn matches_link(url: &str, stored: &str, origin: &str) -> bool {
    url == stored || url == origin
}

fn reliable_file_name<'a>(
    rename: &'a str,
    out: &'a str,
    capture: &'a str,
    probe: &'a str,
) -> &'a str {
    [rename, out, capture, probe]
        .into_iter()
        .map(str::trim)
        .find(|name| !name.is_empty())
        .unwrap_or_default()
}

impl NewDownloadView {
    /// Ignore unrelated task deltas so progress elsewhere never scans the download table.
    #[must_use]
    pub fn duplicate_hint_needs_refresh(&self, event: &DaemonEvent) -> bool {
        let Some(entry) = self.entries.first().filter(|_| !self.is_remote()) else {
            return false;
        };
        let current = self.duplicate_hint.task.as_ref();
        let is_current = |id: &str| current.is_some_and(|task| task.id == id);
        let affects_match = |task: &TaskDto| {
            current.map_or_else(
                || matches_link(&entry.url, &task.url, &task.origin_url),
                |current| {
                    current.id == task.task_id
                        && (current.name != task.file_name
                            || current.by_origin != (task.origin_url == entry.url)
                            || !matches_link(&entry.url, &task.url, &task.origin_url))
                },
            )
        };
        match event {
            DaemonEvent::TaskChanged(task) => affects_match(task),
            DaemonEvent::TasksChanged(tasks) => tasks.iter().any(affects_match),
            DaemonEvent::TaskDeleted { task_id } => is_current(task_id),
            DaemonEvent::TasksDeleted { task_ids } => task_ids.iter().any(|id| is_current(id)),
            DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                file_name,
                url,
                status,
                error_message,
                ..
            }) => current.map_or(url == &entry.url, |task| {
                task.id == *task_id
                    && ((!file_name.is_empty() && task.name != *file_name)
                        || (!url.is_empty() && !task.by_origin && url != &entry.url)
                        || (*status == 4 && error_message == "deleted"))
            }),
            DaemonEvent::Engine(WsServerMsg::TaskMetaProbed {
                task_id, file_name, ..
            }) => current.is_some_and(|task| {
                task.id == *task_id && !file_name.is_empty() && task.name != *file_name
            }),
            DaemonEvent::SnapshotReplaced(_)
            | DaemonEvent::ConfigChanged(_)
            | DaemonEvent::Engine(WsServerMsg::TasksSnapshot { .. }) => true,
            DaemonEvent::TablesReloading { tasks, .. } => tasks.is_some(),
            _ => false,
        }
    }

    /// Discard an earlier file result when the host or its task table is replaced.
    pub fn reset_duplicate_hint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.duplicate_hint.clear_file() {
            cx.notify();
        }
        self.refresh_duplicate_hint(window, cx);
    }

    /// Reconcile the visible hint against the live host table and the current form inputs.
    pub fn refresh_duplicate_hint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_duplicate_hint(true, window, cx);
    }

    pub(super) fn sync_duplicate_hint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_duplicate_hint(false, window, cx);
    }

    fn update_duplicate_hint(
        &mut self,
        refresh_tasks: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((entry, (tasks, default_dir))) = self
            .entries
            .first()
            .filter(|_| !self.is_remote())
            .and_then(|entry| Some((entry, (self.duplicate_hint.scope)(cx)?)))
        else {
            self.duplicate_hint.task_url.clear();
            let task_changed = self.duplicate_hint.set_task(None, "");
            let file_changed = self.duplicate_hint.clear_file();
            if task_changed || file_changed {
                cx.notify();
            }
            return;
        };
        let task_changed = if refresh_tasks || self.duplicate_hint.task_url != entry.url {
            self.duplicate_hint.task_url.clone_from(&entry.url);
            let matched = tasks
                .iter()
                .find(|task| matches_link(&entry.url, &task.url, &task.origin_url));
            self.duplicate_hint.set_task(matched, &entry.url)
        } else {
            false
        };
        if self.duplicate_hint.task.is_some() {
            let file_changed = self.duplicate_hint.clear_file();
            if task_changed || file_changed {
                cx.notify();
            }
            return;
        }

        let rename = if self.entries.len() == 1 {
            self.rename_value(cx)
        } else {
            String::new()
        };
        let name = reliable_file_name(
            &rename,
            &entry.file_name,
            self.capture_for(&entry.url)
                .map_or("", |capture| capture.file_name.as_str()),
            self.probe.file_name(&self.entries),
        );
        if self.duplicate_hint.file.as_ref().is_some_and(|check| {
            check.name == name && check.url == entry.url && check.default_dir == default_dir
        }) {
            if task_changed {
                cx.notify();
            }
            return;
        }
        let check = (!name.is_empty()).then(|| FileCheck {
            url: entry.url.clone(),
            name: name.to_owned(),
            default_dir: default_dir.to_owned(),
            exists: false,
        });
        let file_changed = self.duplicate_hint.clear_file();
        if task_changed || file_changed {
            cx.notify();
        }
        if let Some(check) = check {
            self.start_file_check(check, window, cx);
        }
    }

    fn start_file_check(&mut self, check: FileCheck, window: &mut Window, cx: &mut Context<Self>) {
        let port = self.port.clone();
        let generation = self.duplicate_hint.generation;
        let file_name = check.name.clone();
        self.duplicate_hint.file = Some(check);
        self.duplicate_hint.request = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(FILE_CHECK_DEBOUNCE).await;
            let exists = matches!(
                port.execute(DownloadsCommand::DefaultFileExists { file_name })
                    .await,
                Ok(DownloadsResult::Value(value)) if value.as_bool() == Some(true)
            );
            if !exists {
                return;
            }
            let Ok(()) = this.update_in(cx, |this, _, cx| {
                let hint = &mut this.duplicate_hint;
                if hint.generation == generation
                    && let Some(check) = &mut hint.file
                    && (hint.scope)(cx).is_some_and(|(_, dir)| check.default_dir == dir)
                {
                    check.exists = true;
                    cx.notify();
                }
            }) else {
                // The dialog has already closed.
                return;
            };
        }));
    }

    pub(super) fn render_duplicate_hint(&self, cx: &App) -> Option<AnyElement> {
        let (name, task_exists) = if let Some(task) = &self.duplicate_hint.task {
            let name = if task.name.trim().is_empty() {
                self.entries.first()?.url.as_str()
            } else {
                task.name.as_str()
            };
            (name, true)
        } else {
            let check = self
                .duplicate_hint
                .file
                .as_ref()
                .filter(|check| check.exists)?;
            (check.name.as_str(), false)
        };
        let text = self.strings.format_duplicate_hint(name, task_exists);
        Some(
            field_hint(text.clone(), cx)
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_middle()
                .text_color(active_theme(cx).extended().colors.warning)
                .id("new-download-duplicate-hint")
                .tooltip(move |window, cx| tooltip_view(text.clone(), window, cx))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{matches_link, reliable_file_name};

    #[test]
    fn duplicate_link_matches_either_stored_url_exactly() {
        assert!(matches_link(
            "https://x/1",
            "https://x/1",
            "https://x/origin"
        ));
        assert!(matches_link(
            "https://x/origin",
            "https://x/1",
            "https://x/origin"
        ));
        assert!(!matches_link("https://x/1?a=2", "https://x/1?a=1", ""));
        assert!(!matches_link("https://x/A", "https://x/a", ""));
    }

    #[test]
    fn file_name_uses_only_reliable_hints_in_precedence_order() {
        assert_eq!(
            reliable_file_name("捕获  📦.tar.gz", "out", "capture", "probe"),
            "捕获  📦.tar.gz"
        );
        assert_eq!(
            reliable_file_name("", "out.7z.001", "capture", "probe"),
            "out.7z.001"
        );
        assert_eq!(
            reliable_file_name("", "", "مرحبا.zip", "probe"),
            "مرحبا.zip"
        );
        assert_eq!(reliable_file_name("", "", "   ", "probe"), "probe");
        assert_eq!(reliable_file_name("", "", "", ""), "");
    }
}
