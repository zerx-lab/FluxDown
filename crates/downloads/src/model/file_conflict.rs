//! 「文件已存在」待确认请求的纯模型：从协议 DTO 摘出展示字段并一次性预格式化，聚合窗口与
//! 任务表角标共用同一份逻辑（窗口按条渲染，任务表只问「这个任务有没有待确认」）。

use std::collections::HashSet;

use chrono::{Local, TimeZone as _};
use fluxdown_protocol::{FileExistsAction, SelectionKind, SelectionRequestDto};
use fluxdown_ui_icon_pack::FileKind;
use gpui::SharedString;

use super::{file_extension, format_bytes};

/// 一条待确认请求。保留字节数，以便对话框随设备的显示偏好刷新。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileConflictItem {
    pub(crate) request_id: String,
    pub(crate) task_id: String,
    pub(crate) file_name: SharedString,
    pub(crate) save_dir: SharedString,
    /// 文件图标解析键：小写名 / 种类 / 扩展名（系统图标按扩展名取）。
    pub(crate) name_fold: String,
    pub(crate) kind: FileKind,
    pub(crate) extension: SharedString,
    /// 已有文件大小；主机读取失败时为 `None`。
    pub(crate) existing_size: Option<u64>,
    /// 已有文件修改时间（本地时间，到分钟）；未知时为 `None`。
    pub(crate) existing_modified: Option<SharedString>,
    /// 新下载大小；未知时为 `None`（由视图显示「大小未知」）。
    pub(crate) incoming_size: Option<u64>,
    pub(crate) rename_preview: SharedString,
    actions: Vec<FileExistsAction>,
    pub(crate) deadline_unix_ms: i64,
}

impl FileConflictItem {
    /// 只接受 `FileExists` 请求；其余种类（HLS / BT / 变体）返回 `None`。
    pub(crate) fn from_request(request: &SelectionRequestDto) -> Option<Self> {
        let SelectionKind::FileExists {
            file_name,
            save_dir,
            existing_size,
            existing_modified_unix_ms,
            incoming_size,
            rename_preview,
            actions,
        } = &request.kind
        else {
            return None;
        };
        Some(Self {
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            file_name: SharedString::from(file_name.clone()),
            save_dir: SharedString::from(save_dir.clone()),
            name_fold: file_name.to_lowercase(),
            kind: FileKind::of_name(file_name),
            extension: file_extension(file_name),
            existing_size: *existing_size,
            existing_modified: existing_modified_unix_ms.and_then(format_modified),
            incoming_size: incoming_size
                .filter(|size| *size > 0)
                .map(i64::unsigned_abs),
            rename_preview: SharedString::from(rename_preview.clone()),
            actions: actions.clone(),
            deadline_unix_ms: request.deadline_unix_ms,
        })
    }

    pub(crate) fn existing_size_text(&self) -> Option<SharedString> {
        self.existing_size.map(format_bytes).map(SharedString::from)
    }

    pub(crate) fn incoming_size_text(&self) -> Option<SharedString> {
        self.incoming_size.map(format_bytes).map(SharedString::from)
    }

    /// 重命名恒可用（协议保证）；覆盖 / 跳过取决于任务协议。
    pub(crate) fn allows(&self, action: FileExistsAction) -> bool {
        action == FileExistsAction::Rename || self.actions.contains(&action)
    }
}

/// 本地时间 `YYYY-MM-DD HH:MM`；时间戳越界返回 `None`。
fn format_modified(unix_ms: i64) -> Option<SharedString> {
    Local
        .timestamp_millis_opt(unix_ms)
        .single()
        .map(|at| SharedString::from(at.format("%Y-%m-%d %H:%M").to_string()))
}

/// 距截止的整秒数（向上取整，未到点不显示 0）；已过期为 0。
pub(crate) fn remaining_seconds(deadline_unix_ms: i64, now_unix_ms: i64) -> i64 {
    (deadline_unix_ms.saturating_sub(now_unix_ms).max(0) + 999) / 1000
}

/// 全部待确认请求，保持到达顺序（快照顺序）。
#[derive(Default)]
pub(crate) struct FileConflictSet {
    items: Vec<FileConflictItem>,
    tasks: HashSet<String>,
}

impl FileConflictSet {
    /// 用快照整体替换；非 `FileExists` 请求忽略。
    pub(crate) fn replace(&mut self, requests: &[SelectionRequestDto]) {
        self.items = requests
            .iter()
            .filter_map(FileConflictItem::from_request)
            .collect();
        self.reindex();
    }

    /// 新增或原位更新；非 `FileExists` 或内容无变化返回 `false`。
    pub(crate) fn upsert(&mut self, request: &SelectionRequestDto) -> bool {
        let Some(item) = FileConflictItem::from_request(request) else {
            return false;
        };
        match self
            .items
            .iter_mut()
            .find(|existing| existing.request_id == item.request_id)
        {
            Some(existing) if *existing == item => return false,
            Some(existing) => *existing = item,
            None => self.items.push(item),
        }
        self.reindex();
        true
    }

    /// 移除并返回该请求（未知 id 为 `None`）。
    pub(crate) fn remove(&mut self, request_id: &str) -> Option<FileConflictItem> {
        let ix = self
            .items
            .iter()
            .position(|item| item.request_id == request_id)?;
        let removed = self.items.remove(ix);
        self.reindex();
        Some(removed)
    }

    pub(crate) fn items(&self) -> &[FileConflictItem] {
        &self.items
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 该任务是否有待确认请求（任务表角标）。
    pub(crate) fn contains_task(&self, task_id: &str) -> bool {
        !self.tasks.is_empty() && self.tasks.contains(task_id)
    }

    /// 最早的截止时间：倒计时以它为准（到点后该请求按默认重命名）。
    pub(crate) fn earliest_deadline(&self) -> Option<i64> {
        self.items.iter().map(|item| item.deadline_unix_ms).min()
    }

    /// 每一条都允许该动作（批量按钮可用）。空集合为 `false`。
    pub(crate) fn all_allow(&self, action: FileExistsAction) -> bool {
        !self.items.is_empty() && self.items.iter().all(|item| item.allows(action))
    }

    fn reindex(&mut self) {
        self.tasks = self.items.iter().map(|item| item.task_id.clone()).collect();
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{FileExistsAction, SelectionOutcome, SelectionRequestDto};
    use serde_json::json;

    use super::{FileConflictSet, remaining_seconds};

    fn request(id: &str, task: &str, actions: &[&str], deadline: i64) -> SelectionRequestDto {
        serde_json::from_value(json!({
            "requestId": id,
            "taskId": task,
            "kind": {
                "type": "fileExists",
                "fileName": "a.zip",
                "saveDir": "/dl",
                "existingSize": 2048,
                "incomingSize": 4096,
                "renamePreview": "a (1).zip",
                "actions": actions,
            },
            "defaultChoice": {"kind": "fileExists", "action": "rename"},
            "deadlineUnixMs": deadline,
        }))
        .expect("request fixture is valid wire JSON")
    }

    fn other_kind(id: &str) -> SelectionRequestDto {
        SelectionRequestDto {
            request_id: id.to_owned(),
            task_id: "t".to_owned(),
            kind: fluxdown_protocol::SelectionKind::Hls {
                options: Vec::new(),
            },
            default_choice: SelectionOutcome::Cancelled,
            deadline_unix_ms: 0,
        }
    }

    #[test]
    fn only_file_exists_requests_are_tracked_in_arrival_order() {
        let mut set = FileConflictSet::default();
        set.replace(&[
            request("r2", "t2", &["rename"], 10),
            other_kind("hls"),
            request("r1", "t1", &["rename"], 5),
        ]);
        let ids: Vec<_> = set.items().iter().map(|i| i.request_id.as_str()).collect();
        assert_eq!(ids, ["r2", "r1"]);
        assert!(set.contains_task("t1") && !set.contains_task("hls"));
        assert!(!set.upsert(&other_kind("hls2")));
    }

    #[test]
    fn upsert_is_idempotent_and_remove_drops_the_task_index() {
        let mut set = FileConflictSet::default();
        let r = request("r1", "t1", &["rename", "skip"], 5);
        assert!(set.upsert(&r));
        assert!(!set.upsert(&r), "same payload is not a change");
        assert!(set.upsert(&request("r1", "t1", &["rename"], 9)));
        assert_eq!(set.len(), 1);
        assert_eq!(set.remove("r1").map(|i| i.task_id), Some("t1".to_owned()));
        assert!(set.is_empty() && !set.contains_task("t1"));
        assert!(set.remove("r1").is_none());
    }

    #[test]
    fn bulk_actions_need_every_row_to_allow_them() {
        let mut set = FileConflictSet::default();
        assert!(!set.all_allow(FileExistsAction::Rename));
        set.upsert(&request("r1", "t1", &["rename", "overwrite", "skip"], 5));
        set.upsert(&request("r2", "t2", &["rename", "overwrite"], 3));
        assert!(set.all_allow(FileExistsAction::Rename));
        assert!(set.all_allow(FileExistsAction::Overwrite));
        assert!(!set.all_allow(FileExistsAction::Skip));
        assert_eq!(set.earliest_deadline(), Some(3));
    }

    #[test]
    fn countdown_rounds_up_and_never_goes_negative() {
        assert_eq!(remaining_seconds(60_000, 0), 60);
        assert_eq!(remaining_seconds(1_001, 0), 2);
        assert_eq!(remaining_seconds(1_000, 0), 1);
        assert_eq!(remaining_seconds(0, 5_000), 0);
    }
}
