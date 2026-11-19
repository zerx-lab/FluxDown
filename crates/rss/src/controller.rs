//! RSS 订阅和条目的投影、异步加载防串源与动作端口。

use crate::{PortFuture, RssPort};
use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, DaemonEvent, DaemonSnapshot, PluginDto, QueueDto, RssItemDto,
    RssSourceDto, ServiceEvent, WsServerMsg, method,
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

// A load carries both the selected source and a monotonically changing revision. An event
// snapshot can supersede an in-flight RPC even without switching sources.
pub(crate) struct ItemLoad {
    pub(crate) source_id: String,
    pub(crate) revision: u64,
    pub(crate) future: PortFuture<serde_json::Value>,
}

pub struct RssController {
    pub(crate) port: Arc<dyn RssPort>,
    pub(crate) sources: Vec<RssSourceDto>,
    pub(crate) queues: Vec<QueueDto>,
    /// 已安装插件快照：新建/编辑订阅时据此生成来源（provider）选项。
    pub(crate) plugins: Vec<PluginDto>,
    pub(crate) tasks: HashMap<String, (i32, bool)>,
    /// 任务表仍在分页载入：`tasks` 只是占位，条目关联任务「不在表里」不代表已被删除。
    pub(crate) tasks_loading: bool,
    pub(crate) selected_source: Option<String>,
    pub(crate) items: Vec<RssItemDto>,
    pub(crate) selected: BTreeSet<String>,
    pub(crate) busy: HashSet<String>,
    pub(crate) action_epoch: u64,
    pub(crate) revision: u64,
    pub(crate) loading: bool,
    pub(crate) refresh_busy: bool,
    pub(crate) read_busy: bool,
    pub(crate) delete_busy: bool,
    pub(crate) stale: bool,
}

impl RssController {
    pub fn new(port: Arc<dyn RssPort>) -> Self {
        Self {
            port,
            sources: Vec::new(),
            queues: Vec::new(),
            plugins: Vec::new(),
            tasks: HashMap::new(),
            tasks_loading: false,
            selected_source: None,
            items: Vec::new(),
            selected: BTreeSet::new(),
            busy: HashSet::new(),
            action_epoch: 0,
            revision: 0,
            loading: false,
            refresh_busy: false,
            read_busy: false,
            delete_busy: false,
            stale: true,
        }
    }

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot) {
        self.absorb_snapshot(&snapshot.daemon);
        self.stale = !snapshot.daemon_connected;
    }

    fn absorb_snapshot(&mut self, snapshot: &DaemonSnapshot) {
        self.tasks_loading = snapshot.task_total.is_some();
        self.tasks = snapshot
            .tasks
            .iter()
            .map(|task| (task.task_id.clone(), (task.status, task.file_missing)))
            .collect();
        self.queues.clone_from(&snapshot.queues);
        self.plugins.clone_from(&snapshot.plugins);
        self.absorb_sources(&snapshot.rss_sources);
        self.invalidate_items();
    }

    fn absorb_sources(&mut self, sources: &[RssSourceDto]) {
        self.sources.clear();
        self.sources.extend_from_slice(sources);
        self.sources.sort_by_key(|source| source.position);
        let selected_exists = self
            .selected_source
            .as_ref()
            .is_some_and(|id| self.sources.iter().any(|source| &source.source_id == id));
        if !selected_exists {
            self.selected_source = self.sources.first().map(|source| source.source_id.clone());
            self.items.clear();
            self.selected.clear();
            self.reset_actions();
            self.invalidate_items();
        }
    }

    fn reset_actions(&mut self) {
        self.action_epoch = self.action_epoch.wrapping_add(1);
        self.busy.clear();
        self.refresh_busy = false;
        self.read_busy = false;
    }

    fn invalidate_items(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.loading = false;
    }

    /// 应用一条服务事件；返回 RSS 页可见状态是否可能变化（无关事件与原值写入返回 `false`，
    /// 供视图跳过重绘——下载进度帧会高频到达）。
    pub fn apply_event(&mut self, event: &ServiceEvent) -> bool {
        let ServiceEvent::Agent(event) = event else {
            return false;
        };
        match event {
            AgentEvent::DaemonSnapshotReplaced(snapshot)
            | AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot)) => {
                self.absorb_snapshot(snapshot);
                self.stale = false;
                true
            }
            AgentEvent::DaemonConnectionChanged(connected) => {
                self.stale = !connected;
                if !connected {
                    self.reset_actions();
                    self.invalidate_items();
                }
                true
            }
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::RssSourcesChanged { sources })) => {
                self.absorb_sources(sources);
                true
            }
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::RssItemsChanged {
                source_id,
                items,
                ..
            })) => {
                let selected = self.selected_source.as_deref() == Some(source_id);
                if selected {
                    self.invalidate_items();
                    self.items.clone_from(items);
                    self.retain_existing_selection();
                }
                selected
            }
            // 插件列表只在打开编辑器时读取，变化本身不影响 RSS 页可见内容。
            AgentEvent::Daemon(DaemonEvent::PluginsChanged(plugins)) => {
                self.plugins.clone_from(plugins);
                false
            }
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::PluginAutoDisabled {
                identity,
                reason,
            })) => {
                if let Some(plugin) = self
                    .plugins
                    .iter_mut()
                    .find(|plugin| plugin.identity == *identity)
                {
                    plugin.enabled = false;
                    plugin.disabled_reason.clone_from(reason);
                }
                false
            }
            AgentEvent::Daemon(DaemonEvent::RssChanged { source_id, .. }) => {
                let selected = self.selected_source.as_deref() == Some(source_id);
                if selected {
                    self.invalidate_items();
                }
                selected
            }
            AgentEvent::Daemon(DaemonEvent::TaskChanged(task)) => {
                let value = (task.status, task.file_missing);
                self.tasks.insert(task.task_id.clone(), value) != Some(value)
            }
            AgentEvent::Daemon(DaemonEvent::TasksChanged(tasks)) => {
                let mut changed = false;
                for task in tasks {
                    let value = (task.status, task.file_missing);
                    changed |= self.tasks.insert(task.task_id.clone(), value) != Some(value);
                }
                changed
            }
            AgentEvent::Daemon(DaemonEvent::TaskDeleted { task_id }) => {
                self.tasks.remove(task_id).is_some()
            }
            AgentEvent::Daemon(DaemonEvent::TasksDeleted { task_ids }) => {
                let mut changed = false;
                for task_id in task_ids {
                    changed |= self.tasks.remove(task_id).is_some();
                }
                changed
            }
            // 任务表整表重载宣告：清空并进入加载态，完整表由会话分页载入后以快照信号送达。
            AgentEvent::Daemon(DaemonEvent::TablesReloading { tasks: Some(_), .. }) => {
                self.tasks.clear();
                self.tasks_loading = true;
                true
            }
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks })) => {
                self.tasks = tasks
                    .iter()
                    .map(|task| (task.task_id.clone(), (task.status, task.file_missing)))
                    .collect();
                true
            }
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                status,
                error_message,
                ..
            })) => {
                if *status == 4 && error_message == "deleted" {
                    self.tasks.remove(task_id).is_some()
                } else if let Some(task) = self.tasks.get_mut(task_id) {
                    let changed = task.0 != *status;
                    task.0 = *status;
                    changed
                } else {
                    false
                }
            }
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::FileMissingChanged {
                updates,
            })) => {
                let mut changed = false;
                for update in updates {
                    if let Some(task) = self.tasks.get_mut(&update.task_id)
                        && task.1 != update.missing
                    {
                        task.1 = update.missing;
                        changed = true;
                    }
                }
                changed
            }
            _ => false,
        }
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
        self.reset_actions();
        self.invalidate_items();
    }

    pub fn sources(&self) -> &[RssSourceDto] {
        &self.sources
    }
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn delete(&self, source_id: String) -> PortFuture<serde_json::Value> {
        if self.stale {
            return unavailable();
        }
        self.port.call(
            method::DAEMON_RSS_DELETE_SOURCE,
            serde_json::json!({"sourceId": source_id}),
        )
    }
    pub fn refresh(&self, source_id: String) -> PortFuture<serde_json::Value> {
        if self.stale {
            return unavailable();
        }
        self.port.call(
            method::DAEMON_RSS_REFRESH_SOURCE,
            serde_json::json!({"sourceId": source_id}),
        )
    }

    pub(crate) fn item_action(
        &self,
        source_id: &str,
        guid: &str,
        action: &'static str,
    ) -> PortFuture<serde_json::Value> {
        if self.stale {
            return unavailable();
        }
        self.port.call(
            method::DAEMON_RSS_ITEM_ACTION,
            serde_json::json!({"sourceId": source_id, "guid": guid, "action": action}),
        )
    }

    pub(crate) fn select_source(&mut self, id: &str) -> bool {
        if self.selected_source.as_deref() == Some(id)
            || !self.sources.iter().any(|source| source.source_id == id)
        {
            return false;
        }
        self.selected_source = Some(id.to_owned());
        self.items.clear();
        self.selected.clear();
        self.reset_actions();
        self.invalidate_items();
        true
    }

    pub(crate) fn load_items(&mut self) -> Option<ItemLoad> {
        if self.stale {
            return None;
        }
        let source_id = self.selected_source.clone()?;
        self.invalidate_items();
        self.loading = true;
        Some(ItemLoad {
            future: self.port.call(
                method::DAEMON_RSS_GET_ITEMS,
                serde_json::json!({"sourceId": source_id}),
            ),
            source_id,
            revision: self.revision,
        })
    }

    pub(crate) fn finish_load(
        &mut self,
        source_id: &str,
        revision: u64,
        result: Result<serde_json::Value, fluxdown_protocol::RpcErrorData>,
    ) -> bool {
        if self.stale
            || self.selected_source.as_deref() != Some(source_id)
            || self.revision != revision
        {
            return false;
        }
        self.loading = false;
        match result.and_then(|value| {
            serde_json::from_value::<Vec<RssItemDto>>(value).map_err(|_| {
                fluxdown_protocol::RpcErrorData::new(
                    fluxdown_protocol::ApplicationErrorCode::Unavailable,
                    true,
                )
            })
        }) {
            Ok(items) => {
                self.items = items;
                self.retain_existing_selection();
                true
            }
            Err(_) => false,
        }
    }

    fn retain_existing_selection(&mut self) {
        self.selected
            .retain(|guid| self.items.iter().any(|item| &item.guid == guid));
    }

    pub(crate) fn visible_indices(&self, query: &str, oldest_first: bool) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let mut indices: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.title.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect();
        indices.sort_by(|&ai, &bi| {
            let a = &self.items[ai];
            let b = &self.items[bi];
            let missing = (a.pub_date <= 0).cmp(&(b.pub_date <= 0));
            if missing != std::cmp::Ordering::Equal {
                return missing;
            }
            let date = if oldest_first {
                a.pub_date.cmp(&b.pub_date)
            } else {
                b.pub_date.cmp(&a.pub_date)
            };
            date.then(ai.cmp(&bi))
        });
        indices
    }

    pub(crate) fn select_visible(&mut self, query: &str, oldest_first: bool, checked: bool) {
        for index in self.visible_indices(query, oldest_first) {
            let guid = &self.items[index].guid;
            if checked {
                self.selected.insert(guid.clone());
            } else {
                self.selected.remove(guid);
            }
        }
    }

    /// Eligible bulk-download items are new or initial history rows with a usable link.
    pub(crate) fn bulk_download_candidates<'a>(
        &'a self,
        query: &str,
        oldest_first: bool,
    ) -> impl Iterator<Item = &'a str> {
        self.visible_indices(query, oldest_first)
            .into_iter()
            .filter_map(|index| self.items.get(index))
            .filter(|item| self.is_bulk_download_candidate(item))
            .map(|item| item.guid.as_str())
    }

    pub(crate) fn confirmed_bulk_download(
        &self,
        source_id: &str,
        query: &str,
        oldest_first: bool,
        confirmed: &HashSet<String>,
    ) -> Option<Vec<String>> {
        if self.stale || self.loading || self.selected_source.as_deref() != Some(source_id) {
            return None;
        }
        let current: Vec<_> = self.bulk_download_candidates(query, oldest_first).collect();
        (current.len() == confirmed.len() && current.iter().all(|guid| confirmed.contains(*guid)))
            .then(|| current.into_iter().map(str::to_owned).collect())
    }

    pub(crate) fn is_bulk_download_candidate(&self, item: &RssItemDto) -> bool {
        self.selected_source.as_deref() == Some(item.source_id.as_str())
            && matches!(item.status, 0 | 5)
            && (!item.link.trim().is_empty() || !item.enclosure_url.trim().is_empty())
            && (self.busy.is_empty()
                || !self
                    .busy
                    .contains(&format!("{}\0{}", item.source_id, item.guid)))
    }

    pub(crate) fn task_status(&self, item: &RssItemDto) -> Option<(i32, bool)> {
        (item.status == 1 && !item.task_id.is_empty())
            .then(|| self.tasks.get(&item.task_id).copied())
            .flatten()
    }
    pub(crate) fn begin_action(&mut self, source_id: &str, guid: &str) -> bool {
        !self.stale
            && self.selected_source.as_deref() == Some(source_id)
            && self.items.iter().any(|item| item.guid == guid)
            && self.busy.insert(format!("{source_id}\0{guid}"))
    }

    pub(crate) fn finish_action(
        &mut self,
        source_id: &str,
        guid: &str,
        epoch: u64,
        succeeded: bool,
    ) {
        if epoch != self.action_epoch || self.selected_source.as_deref() != Some(source_id) {
            return;
        }
        self.busy.remove(&format!("{source_id}\0{guid}"));
        if succeeded {
            self.selected.remove(guid);
        }
    }

    pub(crate) fn status_key(&self, item: &RssItemDto) -> &'static str {
        match item.status {
            1 => match self.task_status(item) {
                Some((0, _)) => "statusPending",
                Some((1, _)) => "statusDownloading",
                Some((2, _)) => "statusPaused",
                Some((3, true)) => "statusIncomplete",
                Some((3, false)) => "statusCompleted",
                Some((4, _)) => "statusError",
                Some((5, _)) => "statusPreparing",
                Some(_) => "rssTaskCreated",
                None if self.tasks_loading => "rssTaskLoading",
                None => "rssTaskMissing",
            },
            2 => "rssStatusIgnored",
            3 => "rssStatusFiltered",
            4 => "rssStatusDuplicate",
            5 => "rssStatusHistory",
            _ => "rssStatusNew",
        }
    }
}

fn unavailable() -> PortFuture<serde_json::Value> {
    Box::pin(async {
        Err(fluxdown_protocol::RpcErrorData::new(
            fluxdown_protocol::ApplicationErrorCode::Unavailable,
            true,
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc};

    use fluxdown_protocol::{
        AgentEvent, AgentSnapshot, DaemonEvent, RssItemDto, RssSourceDto, ServiceEvent, TaskDto,
        WsServerMsg,
    };
    use serde_json::json;

    use super::{RssController, RssPort};
    use crate::PortFuture;

    struct Port;
    impl RssPort for Port {
        fn call(&self, _: &'static str, _: serde_json::Value) -> PortFuture<serde_json::Value> {
            Box::pin(async { Ok(json!([])) })
        }
    }

    fn source(id: &str) -> Result<RssSourceDto, serde_json::Error> {
        serde_json::from_value(json!({"sourceId": id, "url": format!("https://example.org/{id}")}))
    }

    fn item(
        source_id: &str,
        guid: &str,
        title: &str,
        date: i64,
        status: i32,
        task_id: &str,
    ) -> RssItemDto {
        RssItemDto {
            source_id: source_id.into(),
            guid: guid.into(),
            title: title.into(),
            link: String::new(),
            enclosure_url: String::new(),
            enclosure_length: 0,
            pub_date: date,
            fetched_at: 0,
            status,
            task_id: task_id.into(),
            episode_key: String::new(),
            reason: String::new(),
        }
    }

    #[test]
    fn bulk_download_includes_initial_history_but_not_duplicates_or_other_sources() {
        let mut controller = RssController::new(Arc::new(Port));
        controller.stale = false;
        controller.selected_source = Some("a".into());
        controller.items = [
            ("a", "new", 0),
            ("a", "history", 5),
            ("a", "downloaded", 1),
            ("a", "ignored", 2),
            ("a", "filtered", 3),
            ("a", "duplicate", 4),
            ("a", "no-link", 5),
            ("b", "other", 5),
        ]
        .into_iter()
        .map(|(source, guid, status)| {
            let mut row = item(source, guid, guid, 0, status, "");
            if guid != "no-link" {
                row.enclosure_url = format!("https://example.org/{guid}");
            }
            row
        })
        .collect();
        assert_eq!(
            controller
                .bulk_download_candidates("", false)
                .collect::<Vec<_>>(),
            ["new", "history"]
        );
        assert!(controller.begin_action("a", "new"));
        assert_eq!(
            controller
                .bulk_download_candidates("", false)
                .collect::<Vec<_>>(),
            ["history"]
        );
    }

    #[test]
    fn bulk_download_candidates_are_limited_to_search_and_current_eligibility() {
        let mut controller = RssController::new(Arc::new(Port));
        controller.stale = false;
        controller.selected_source = Some("a".into());
        controller.items = [
            ("a", "match-new", "Match new", 0, true),
            ("a", "match-history", "Match history", 5, true),
            ("a", "hidden", "Hidden result", 0, true),
            ("a", "no-link", "Match no link", 5, false),
            ("a", "handled", "Match handled", 4, true),
        ]
        .into_iter()
        .map(|(source, guid, title, status, has_link)| {
            let mut row = item(source, guid, title, 0, status, "");
            if has_link {
                row.link = format!("https://example.org/{guid}");
            }
            row
        })
        .collect();
        let confirmed: HashSet<_> = ["match-new".to_owned(), "match-history".to_owned()].into();
        assert_eq!(
            controller.confirmed_bulk_download("a", "match", true, &confirmed),
            Some(vec!["match-new".to_owned(), "match-history".to_owned()])
        );
        assert!(controller.begin_action("a", "match-new"));
        assert_eq!(
            controller.confirmed_bulk_download("a", "match", false, &confirmed),
            None
        );
        // A refresh might replace a candidate without changing the count.
        controller.items[2].title = "Match replacement".to_owned();
        assert_eq!(
            controller.confirmed_bulk_download("a", "match", false, &confirmed),
            None
        );
        assert!(controller.is_bulk_download_candidate(&controller.items[1]));
        controller.items[1].status = 4;
        assert!(!controller.is_bulk_download_candidate(&controller.items[1]));
        assert_eq!(
            controller
                .bulk_download_candidates("", false)
                .collect::<Vec<_>>(),
            ["hidden"]
        );
    }

    fn snapshot() -> Result<AgentSnapshot, serde_json::Error> {
        Ok(AgentSnapshot {
            daemon_connected: true,
            daemon: fluxdown_protocol::DaemonSnapshot {
                rss_sources: vec![source("a")?, source("b")?],
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn progress(status: i32, error_message: &str) -> ServiceEvent {
        ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::Engine(
            WsServerMsg::TaskProgress {
                task_id: "task".into(),
                status,
                downloaded_bytes: 0,
                total_bytes: 0,
                speed: 0,
                upload_speed: 0,
                file_name: String::new(),
                save_dir: String::new(),
                url: String::new(),
                error_message: error_message.into(),
                uploaded_bytes: 0,
                seeding_status: 0,
                seeding_message: String::new(),
                seeding_time_secs: 0,
            },
        )))
    }

    fn task(status: i32) -> Result<TaskDto, serde_json::Error> {
        serde_json::from_value(json!({
            "taskId": "task", "url": "https://example.org/file", "fileName": "file",
            "saveDir": "/tmp", "status": status, "downloadedBytes": 0,
            "totalBytes": 0, "errorMessage": "", "createdAt": "1",
            "proxyUrl": "", "queueId": "", "checksum": ""
        }))
    }

    #[test]
    fn visible_selection_preserves_hidden_items_and_failed_actions()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut controller = RssController::new(Arc::new(Port));
        controller.replace_snapshot(&snapshot()?);
        let entries = vec![
            item("a", "new", "Alpha new", 100, 0, ""),
            item("a", "same", "Alpha same", 100, 0, ""),
            item("a", "old", "Other old", 30, 0, ""),
            item("a", "undated", "Alpha unknown", 0, 0, ""),
        ];
        let load = controller
            .load_items()
            .ok_or("expected selected source load")?;
        assert!(controller.finish_load(&load.source_id, load.revision, Ok(json!(entries))));
        let guids = |indices: Vec<usize>| {
            indices
                .into_iter()
                .map(|i| controller.items[i].guid.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            guids(controller.visible_indices("", false)),
            ["new", "same", "old", "undated"]
        );
        assert_eq!(
            guids(controller.visible_indices("", true)),
            ["old", "new", "same", "undated"]
        );
        controller.selected.insert("old".into());
        controller.select_visible("ALPHA", false, true);
        assert_eq!(controller.selected.len(), 4);
        controller.select_visible("Alpha", false, false);
        assert_eq!(
            controller
                .selected
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["old"]
        );
        let epoch = controller.action_epoch;
        assert!(controller.begin_action("a", "old"));
        assert!(!controller.begin_action("a", "old"));
        controller.finish_action("a", "old", epoch, false);
        assert!(controller.selected.contains("old"));
        assert!(controller.begin_action("a", "old"));
        controller.finish_action("a", "old", epoch, true);
        assert!(controller.selected.is_empty());
        Ok(())
    }

    #[test]
    fn late_rpc_cannot_replace_another_source_or_newer_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut controller = RssController::new(Arc::new(Port));
        controller.replace_snapshot(&snapshot()?);
        let from_a = controller.load_items().ok_or("expected source a load")?;
        assert!(controller.select_source("b"));
        let from_b = controller.load_items().ok_or("expected source b load")?;
        assert!(!controller.finish_load(
            &from_a.source_id,
            from_a.revision,
            Ok(json!([item("a", "old", "Old", 1, 0, "")]))
        ));
        let newer = item("b", "new", "New", 2, 0, "");
        controller.apply_event(&ServiceEvent::Agent(AgentEvent::Daemon(
            DaemonEvent::Engine(WsServerMsg::RssItemsChanged {
                source_id: "b".into(),
                items: vec![newer],
                notify_titles: vec![],
            }),
        )));
        assert!(!controller.finish_load(&from_b.source_id, from_b.revision, Ok(json!([]))));
        assert_eq!(controller.items[0].guid, "new");
        assert!(controller.select_source("a"));
        controller.items = vec![item("a", "new", "Another", 3, 0, "")];
        controller.selected.insert("new".into());
        let old_epoch = controller.action_epoch;
        assert!(controller.begin_action("a", "new"));
        assert!(controller.select_source("b"));
        controller.items = vec![item("b", "new", "Unrelated", 3, 0, "")];
        controller.selected.insert("new".into());
        controller.finish_action("a", "new", old_epoch, true);
        assert!(controller.selected.contains("new"));
        assert!(controller.select_source("a"));
        controller.items = vec![item("a", "new", "Another", 3, 0, "")];
        controller.selected.insert("new".into());
        let new_epoch = controller.action_epoch;
        assert!(controller.begin_action("a", "new"));
        controller.finish_action("a", "new", old_epoch, true);
        assert!(controller.selected.contains("new"));
        assert!(!controller.begin_action("a", "new"));
        controller.finish_action("a", "new", new_epoch, false);
        assert!(controller.selected.contains("new"));
        controller.mark_stale();
        assert!(controller.load_items().is_none());
        controller.apply_event(&ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(
            true,
        )));
        let recovered = controller.load_items().ok_or("expected reconnect load")?;
        assert!(controller.finish_load(&recovered.source_id, recovered.revision, Ok(json!([]))));
        Ok(())
    }

    #[test]
    fn downloaded_rss_flag_never_masks_actual_task_lifecycle()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut snapshot = snapshot()?;
        snapshot.daemon.tasks.push(task(1)?);
        let mut controller = RssController::new(Arc::new(Port));
        controller.replace_snapshot(&snapshot);
        let linked = item("a", "guid", "Linked", 1, 1, "task");
        assert_eq!(controller.status_key(&linked), "statusDownloading");
        controller.apply_event(&progress(3, ""));
        assert_eq!(controller.status_key(&linked), "statusCompleted");
        controller.apply_event(&ServiceEvent::Agent(AgentEvent::Daemon(
            DaemonEvent::Engine(WsServerMsg::FileMissingChanged {
                updates: vec![fluxdown_protocol::FileMissingUpdateDto {
                    task_id: "task".into(),
                    missing: true,
                }],
            }),
        )));
        assert_eq!(controller.status_key(&linked), "statusIncomplete");
        controller.apply_event(&progress(4, "broken"));
        assert_eq!(controller.status_key(&linked), "statusError");
        controller.apply_event(&progress(4, "deleted"));
        assert_eq!(controller.status_key(&linked), "rssTaskMissing");
        controller.apply_event(&ServiceEvent::Agent(AgentEvent::Daemon(
            DaemonEvent::TaskChanged(task(2)?),
        )));
        assert_eq!(controller.status_key(&linked), "statusPaused");
        controller.apply_event(&ServiceEvent::Agent(AgentEvent::Daemon(
            DaemonEvent::TaskDeleted {
                task_id: "task".into(),
            },
        )));
        assert_eq!(controller.status_key(&linked), "rssTaskMissing");
        Ok(())
    }

    #[test]
    fn batched_task_facts_update_linked_item_status_like_single_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut snapshot = snapshot()?;
        snapshot.daemon.tasks.push(task(1)?);
        let mut controller = RssController::new(Arc::new(Port));
        controller.replace_snapshot(&snapshot);
        let linked = item("a", "guid", "Linked", 1, 1, "task");
        assert_eq!(controller.status_key(&linked), "statusDownloading");
        let batch = ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TasksChanged(vec![task(
            2,
        )?])));
        assert!(controller.apply_event(&batch));
        assert_eq!(controller.status_key(&linked), "statusPaused");
        // 内容未变的批次不重绘。
        assert!(!controller.apply_event(&batch));
        Ok(())
    }

    /// 任务表分页载入期间，关联任务「不在表里」不是已删除：显示载入中而不是「任务已删除」；
    /// 完整快照到达后恢复按表判定。
    #[test]
    fn missing_linked_task_is_loading_not_deleted_while_the_task_table_pages_in()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut snapshot = snapshot()?;
        snapshot.daemon.tasks.push(task(1)?);
        let mut controller = RssController::new(Arc::new(Port));
        controller.replace_snapshot(&snapshot);
        let linked = item("a", "guid", "Linked", 1, 1, "task");
        assert_eq!(controller.status_key(&linked), "statusDownloading");

        assert!(
            controller.apply_event(&ServiceEvent::Agent(AgentEvent::Daemon(
                DaemonEvent::TablesReloading {
                    tasks: Some(5_000),
                    queue: None,
                }
            )))
        );
        assert_eq!(controller.status_key(&linked), "rssTaskLoading");

        controller.replace_snapshot(&snapshot);
        assert_eq!(controller.status_key(&linked), "statusDownloading");

        let mut header = snapshot.clone();
        header.daemon.tasks.clear();
        header.daemon.task_total = Some(5_000);
        controller.replace_snapshot(&header);
        assert_eq!(controller.status_key(&linked), "rssTaskLoading");
        header.daemon.task_total = None;
        controller.replace_snapshot(&header);
        assert_eq!(controller.status_key(&linked), "rssTaskMissing");
        Ok(())
    }
}
