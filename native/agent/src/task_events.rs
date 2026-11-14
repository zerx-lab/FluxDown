//! 兼容 API 的任务生命周期事件（aria2 WS 通知源）与实时速率。
//!
//! daemon 只发布 `WsServerMsg::TaskProgress` / `TasksSnapshot`（以及防御性处理的
//! `TaskChanged` / `TasksChanged` / `TaskDeleted`）。这里按旧 headless 宿主 `ws_hub` 的迁移规则把它们
//! 翻译为 [`TaskEvent`]，判定统一委托 [`task_event_for_transition`]：
//!
//! - `TaskProgress` 是状态迁移的权威来源；`delete` 合成信号（`status=4` 且
//!   `error_message="deleted"`）既不发事件也不覆盖前态。
//! - `TasksSnapshot` 只做对账：消失且前态非终态的任务发 `Stop`，终态静默移除，
//!   首次观测只登记不发事件，已登记的前态不被快照往回冲。
//! - `BtComplete` 无法产生：daemon 事件中心不转发引擎的 `BtDataFinished`。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use fluxdown_api::service::{LiveSpeed, TaskEvent, TaskEventKind, task_event_for_transition};
use fluxdown_protocol::{
    AgentEvent, DaemonEvent, EventFrame, ServiceEvent, SnapshotBody, TaskDto, WsServerMsg,
};
use tokio::sync::broadcast;

use crate::event_hub::AgentEventHub;

const TASK_EVENT_CAPACITY: usize = 256;

/// 任务是否处于终态（completed / error）。终态任务消失不发 `Stop`（镜像 aria2
/// `removeDownloadResult` 不通知的语义）。
const fn is_terminal_status(status: i32) -> bool {
    matches!(status, 3 | 4)
}

/// 引擎用 `status=4` + `error_message="deleted"` 的合成进度清理各 sink 的状态表，
/// 不是真实错误。
fn is_delete_sentinel(status: i32, error_message: &str) -> bool {
    status == 4 && error_message == "deleted"
}

/// 下载中 / 准备中，或已完成但正在做种（`seeding_status == 1`）时保留实时速率。
const fn keeps_speed(status: i32, seeding_status: i32) -> bool {
    matches!(status, 1 | 5) || seeding_status == 1
}

/// 前态表 + 实时速率缓存；纯状态机，不接触锁与广播。
#[derive(Default)]
struct Tracker {
    states: HashMap<String, i32>,
    speeds: HashMap<String, LiveSpeed>,
}

impl Tracker {
    fn event(task_id: &str, kind: TaskEventKind) -> TaskEvent {
        TaskEvent {
            task_id: task_id.to_owned(),
            kind,
        }
    }

    /// 快照对账：返回需要广播 `Stop` 的任务。
    fn reconcile(&mut self, tasks: &[TaskDto]) -> Vec<TaskEvent> {
        let live: HashSet<&str> = tasks.iter().map(|task| task.task_id.as_str()).collect();
        self.speeds.retain(|id, _| live.contains(id.as_str()));
        let mut stopped = Vec::new();
        self.states.retain(|id, status| {
            if live.contains(id.as_str()) {
                return true;
            }
            if !is_terminal_status(*status) {
                stopped.push(Self::event(id, TaskEventKind::Stop));
            }
            false
        });
        for task in tasks {
            self.states
                .entry(task.task_id.clone())
                .or_insert(task.status);
        }
        stopped
    }

    /// daemon 快照整体替换（重连）：速率全部作废，前态按快照对账。
    fn replace(&mut self, tasks: &[TaskDto]) -> Vec<TaskEvent> {
        self.speeds.clear();
        self.reconcile(tasks)
    }

    fn transition(&mut self, task_id: &str, status: i32) -> Option<TaskEvent> {
        let prev = self.states.insert(task_id.to_owned(), status);
        task_event_for_transition(prev, status).map(|kind| Self::event(task_id, kind))
    }

    #[allow(clippy::too_many_arguments)]
    fn progress(
        &mut self,
        task_id: &str,
        status: i32,
        speed: i64,
        upload_speed: i64,
        seeding_status: i32,
        error_message: &str,
    ) -> Option<TaskEvent> {
        if keeps_speed(status, seeding_status) {
            self.speeds.insert(
                task_id.to_owned(),
                LiveSpeed {
                    download_bps: speed.max(0),
                    upload_bps: upload_speed.max(0),
                },
            );
        } else {
            self.speeds.remove(task_id);
        }
        if is_delete_sentinel(status, error_message) {
            return None;
        }
        self.transition(task_id, status)
    }

    fn task_changed(&mut self, task: &TaskDto) -> Option<TaskEvent> {
        if !keeps_speed(task.status, task.seeding_status) {
            self.speeds.remove(&task.task_id);
        }
        if is_delete_sentinel(task.status, &task.error_message) {
            return None;
        }
        self.transition(&task.task_id, task.status)
    }

    fn task_deleted(&mut self, task_id: &str) -> Option<TaskEvent> {
        self.speeds.remove(task_id);
        let prev = self.states.remove(task_id)?;
        (!is_terminal_status(prev)).then(|| Self::event(task_id, TaskEventKind::Stop))
    }

    fn apply(&mut self, event: &DaemonEvent) -> Vec<TaskEvent> {
        match event {
            DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                status,
                speed,
                upload_speed,
                seeding_status,
                error_message,
                ..
            }) => self
                .progress(
                    task_id,
                    *status,
                    *speed,
                    *upload_speed,
                    *seeding_status,
                    error_message,
                )
                .into_iter()
                .collect(),
            DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks }) => self.reconcile(tasks),
            DaemonEvent::TaskChanged(task) => self.task_changed(task).into_iter().collect(),
            // 批量事实事件 = 按序逐条 `TaskChanged`。
            DaemonEvent::TasksChanged(tasks) => tasks
                .iter()
                .filter_map(|task| self.task_changed(task))
                .collect(),
            DaemonEvent::TaskDeleted { task_id } => {
                self.task_deleted(task_id).into_iter().collect()
            }
            DaemonEvent::TasksDeleted { task_ids } => task_ids
                .iter()
                .filter_map(|task_id| self.task_deleted(task_id))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// 订阅 [`AgentEventHub`]，向兼容 API 提供任务生命周期事件流与实时速率快照。
#[derive(Clone)]
pub struct TaskEventHub {
    tracker: Arc<Mutex<Tracker>>,
    events: broadcast::Sender<TaskEvent>,
}

impl TaskEventHub {
    /// 以当前投影为前态基线并启动后台泵；必须在 Tokio 运行时内调用。
    #[must_use]
    pub fn spawn(hub: &AgentEventHub) -> Self {
        let (frames, snapshot) = hub.subscribe_and_snapshot();
        let (events, _) = broadcast::channel(TASK_EVENT_CAPACITY);
        let mut tracker = Tracker::default();
        if let SnapshotBody::Agent(agent) = &snapshot.body {
            // 基线只登记：启动时的历史状态不构成通知风暴。
            let _ = tracker.reconcile(&agent.daemon.tasks);
        }
        let this = Self {
            tracker: Arc::new(Mutex::new(tracker)),
            events,
        };
        tokio::spawn(this.clone().pump(hub.clone(), frames, snapshot.sequence));
        this
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    #[must_use]
    pub fn live_speeds(&self) -> HashMap<String, LiveSpeed> {
        lock_or_recover(&self.tracker).speeds.clone()
    }

    fn publish(&self, events: Vec<TaskEvent>) {
        if self.events.receiver_count() == 0 {
            return;
        }
        for event in events {
            if self.events.send(event).is_err() {
                tracing::trace!("last task event subscriber disconnected");
                break;
            }
        }
    }

    async fn pump(
        self,
        hub: AgentEventHub,
        mut frames: broadcast::Receiver<Arc<EventFrame>>,
        baseline: u64,
    ) {
        loop {
            match frames.recv().await {
                Ok(frame) => {
                    if frame.sequence <= baseline {
                        continue;
                    }
                    let events = match &frame.event {
                        // 整表替换在广播里只带头 / 重载宣告，任务表不在帧内：按 hub 当前
                        // 完整投影对账（只在对账那一刻持 hub 锁，回放无关）。
                        ServiceEvent::Agent(
                            AgentEvent::DaemonSnapshotReplaced(_)
                            | AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(_)),
                        ) => hub.inspect(|snapshot| {
                            lock_or_recover(&self.tracker).replace(&snapshot.daemon.tasks)
                        }),
                        ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TablesReloading {
                            tasks: Some(_),
                            ..
                        })) => hub.inspect(|snapshot| {
                            lock_or_recover(&self.tracker).reconcile(&snapshot.daemon.tasks)
                        }),
                        ServiceEvent::Agent(AgentEvent::Daemon(event)) => {
                            lock_or_recover(&self.tracker).apply(event)
                        }
                        _ => continue,
                    };
                    self.publish(events);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // 丢帧后以当前投影对账，速率缓存保持到下一帧进度覆盖。
                    let tasks = hub.inspect(|snapshot| snapshot.daemon.tasks.clone());
                    let stopped = lock_or_recover(&self.tracker).reconcile(&tasks);
                    self.publish(stopped);
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fluxdown_api::service::TaskEventKind;
    use fluxdown_protocol::{AgentSnapshot, DaemonEvent, DaemonSnapshot, TaskDto, WsServerMsg};

    use super::{TaskEventHub, Tracker};
    use crate::event_hub::AgentEventHub;

    fn task(id: &str, status: i32) -> TaskDto {
        let value = serde_json::json!({
            "taskId": id,
            "url": "https://example.com/f",
            "fileName": "f",
            "saveDir": "/tmp",
            "status": status,
            "downloadedBytes": 0,
            "totalBytes": 0,
            "errorMessage": "",
            "createdAt": "1",
            "proxyUrl": "",
            "queueId": "main",
            "checksum": ""
        });
        match serde_json::from_value(value) {
            Ok(task) => task,
            Err(error) => panic!("task dto fixture: {error}"),
        }
    }

    fn progress(id: &str, status: i32, speed: i64) -> DaemonEvent {
        progress_full(id, status, speed, 0, 0, "")
    }

    fn progress_full(
        id: &str,
        status: i32,
        speed: i64,
        upload_speed: i64,
        seeding_status: i32,
        error_message: &str,
    ) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: id.to_owned(),
            status,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed,
            upload_speed,
            file_name: String::new(),
            save_dir: String::new(),
            url: String::new(),
            error_message: error_message.to_owned(),
            uploaded_bytes: 0,
            seeding_status,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        })
    }

    fn snapshot_event(tasks: Vec<TaskDto>) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks })
    }

    fn kinds(events: &[fluxdown_api::service::TaskEvent]) -> Vec<(String, TaskEventKind)> {
        events
            .iter()
            .map(|event| (event.task_id.clone(), event.kind))
            .collect()
    }

    #[test]
    fn progress_lifecycle_follows_shared_transition_rules_and_dedupes() {
        let mut tracker = Tracker::default();
        let mut run = |event: DaemonEvent| kinds(&tracker.apply(&event));
        assert_eq!(
            run(progress("t", 1, 10)),
            [("t".to_owned(), TaskEventKind::Start)]
        );
        assert!(
            run(progress("t", 1, 20)).is_empty(),
            "same status must not repeat"
        );
        assert_eq!(
            run(progress("t", 2, 0)),
            [("t".to_owned(), TaskEventKind::Pause)]
        );
        assert_eq!(
            run(progress("t", 1, 5)),
            [("t".to_owned(), TaskEventKind::Start)]
        );
        assert_eq!(
            run(progress("t", 3, 0)),
            [("t".to_owned(), TaskEventKind::Complete)]
        );
        assert!(
            run(progress("t", 1, 5)).is_empty(),
            "completed is a terminal GID state: no second Start"
        );
        assert_eq!(
            run(progress("e", 1, 5)),
            [("e".to_owned(), TaskEventKind::Start)]
        );
        assert_eq!(
            run(progress("e", 4, 0)),
            [("e".to_owned(), TaskEventKind::Error)]
        );
    }

    #[test]
    fn first_observation_of_terminal_or_paused_task_only_registers() {
        let mut tracker = Tracker::default();
        assert!(tracker.apply(&progress("p", 2, 0)).is_empty());
        assert!(tracker.apply(&progress("c", 3, 0)).is_empty());
        assert!(tracker.apply(&progress("e", 4, 0)).is_empty());
        assert_eq!(
            kinds(&tracker.apply(&progress("p", 1, 0))),
            [("p".to_owned(), TaskEventKind::Start)]
        );
    }

    #[test]
    fn delete_sentinel_neither_errors_nor_overwrites_previous_state() {
        let mut tracker = Tracker::default();
        tracker.apply(&progress("t", 1, 10));
        assert!(
            tracker
                .apply(&progress_full("t", 4, 0, 0, 0, "deleted"))
                .is_empty()
        );
        // 前态仍是 downloading：随后快照里任务消失 → Stop（而不是静默或 Error）。
        assert_eq!(
            kinds(&tracker.apply(&snapshot_event(Vec::new()))),
            [("t".to_owned(), TaskEventKind::Stop)]
        );
        assert!(tracker.speeds.is_empty());
    }

    #[test]
    fn snapshot_reconcile_stops_vanished_active_tasks_and_registers_new_ones_silently() {
        let mut tracker = Tracker::default();
        tracker.apply(&progress("active", 1, 1));
        tracker.apply(&progress("paused", 1, 1));
        tracker.apply(&progress("paused", 2, 0));
        tracker.apply(&progress("done", 1, 1));
        tracker.apply(&progress("done", 3, 0));

        let events = tracker.apply(&snapshot_event(vec![task("fresh", 1)]));
        let mut stopped = kinds(&events);
        stopped.sort_by(|a, b| a.0.cmp(&b.0));
        // 终态 `done` 静默移除；`active` / `paused` 消失 → Stop；`fresh` 首次观测不发事件。
        assert_eq!(
            stopped,
            [
                ("active".to_owned(), TaskEventKind::Stop),
                ("paused".to_owned(), TaskEventKind::Stop),
            ]
        );
        assert_eq!(tracker.states.get("fresh"), Some(&1));
        assert!(!tracker.states.contains_key("done"));
    }

    #[test]
    fn snapshot_does_not_overwrite_already_tracked_state() {
        let mut tracker = Tracker::default();
        tracker.apply(&progress("t", 1, 1));
        // 过期快照仍把任务标成 pending：不得把已知前态往回冲。
        assert!(
            tracker
                .apply(&snapshot_event(vec![task("t", 0)]))
                .is_empty()
        );
        assert_eq!(
            kinds(&tracker.apply(&progress("t", 2, 0))),
            [("t".to_owned(), TaskEventKind::Pause)]
        );
    }

    #[test]
    fn task_deleted_stops_only_non_terminal_tasks() {
        let mut tracker = Tracker::default();
        tracker.apply(&progress("run", 1, 1));
        tracker.apply(&progress("done", 1, 1));
        tracker.apply(&progress("done", 3, 0));
        assert_eq!(
            kinds(&tracker.apply(&DaemonEvent::TaskDeleted {
                task_id: "run".into()
            })),
            [("run".to_owned(), TaskEventKind::Stop)]
        );
        assert!(
            tracker
                .apply(&DaemonEvent::TaskDeleted {
                    task_id: "done".into()
                })
                .is_empty()
        );
        assert!(
            tracker
                .apply(&DaemonEvent::TaskDeleted {
                    task_id: "unknown".into()
                })
                .is_empty()
        );
    }

    #[test]
    fn tasks_deleted_applies_task_deleted_per_id() {
        let mut tracker = Tracker::default();
        tracker.apply(&progress("run", 1, 1));
        tracker.apply(&progress("done", 1, 1));
        tracker.apply(&progress("done", 3, 0));
        assert_eq!(
            kinds(&tracker.apply(&DaemonEvent::TasksDeleted {
                task_ids: vec!["run".into(), "done".into(), "unknown".into()]
            })),
            [("run".to_owned(), TaskEventKind::Stop)]
        );
    }

    #[test]
    fn task_changed_uses_the_same_transition_rules() {
        let mut tracker = Tracker::default();
        assert_eq!(
            kinds(&tracker.apply(&DaemonEvent::TaskChanged(task("t", 1)))),
            [("t".to_owned(), TaskEventKind::Start)]
        );
        assert_eq!(
            kinds(&tracker.apply(&DaemonEvent::TaskChanged(task("t", 3)))),
            [("t".to_owned(), TaskEventKind::Complete)]
        );
    }

    #[test]
    fn tasks_changed_is_sequential_task_changed() {
        let batch = vec![task("a", 1), task("b", 1), task("a", 3)];
        let mut sequential = Tracker::default();
        let expected: Vec<_> = batch
            .iter()
            .flat_map(|task| sequential.apply(&DaemonEvent::TaskChanged(task.clone())))
            .collect();
        assert_eq!(expected.len(), 3);

        let mut batched = Tracker::default();
        let actual = batched.apply(&DaemonEvent::TasksChanged(batch));
        assert_eq!(kinds(&actual), kinds(&expected));
        assert_eq!(
            kinds(&actual),
            [
                ("a".to_owned(), TaskEventKind::Start),
                ("b".to_owned(), TaskEventKind::Start),
                ("a".to_owned(), TaskEventKind::Complete),
            ]
        );
        assert!(
            batched
                .apply(&DaemonEvent::TasksChanged(Vec::new()))
                .is_empty()
        );
    }

    #[test]
    fn live_speed_tracks_active_and_seeding_tasks_and_clears_otherwise() {
        let mut tracker = Tracker::default();
        tracker.apply(&progress_full("t", 1, 100, 7, 0, ""));
        assert_eq!(
            tracker
                .speeds
                .get("t")
                .map(|speed| (speed.download_bps, speed.upload_bps)),
            Some((100, 7))
        );

        // 已完成但做种中：保留上传速率。
        tracker.apply(&progress_full("t", 3, 0, 50, 1, ""));
        assert_eq!(tracker.speeds.get("t").map(|s| s.upload_bps), Some(50));

        // 停止做种：清除。
        tracker.apply(&progress_full("t", 3, 0, 0, 2, ""));
        assert!(!tracker.speeds.contains_key("t"));

        // 负值按 0 处理。
        tracker.apply(&progress_full("n", 1, -5, -6, 0, ""));
        assert_eq!(
            tracker.speeds.get("n").copied(),
            Some(fluxdown_api::service::LiveSpeed::default())
        );
    }

    #[test]
    fn snapshot_prunes_speeds_of_missing_tasks_and_replacement_clears_all() {
        let mut tracker = Tracker::default();
        tracker.apply(&progress("a", 1, 1));
        tracker.apply(&progress("b", 1, 2));
        tracker.apply(&snapshot_event(vec![task("a", 1)]));
        assert_eq!(tracker.speeds.len(), 1);
        assert!(tracker.speeds.contains_key("a"));

        assert!(tracker.replace(&[task("a", 1)]).is_empty());
        assert!(tracker.speeds.is_empty());
    }

    #[tokio::test]
    async fn pump_translates_hub_events_and_seeds_baseline_without_replaying_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let hub = AgentEventHub::new(AgentSnapshot::default());
        hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![task("old", 2), task("live", 1)],
            ..DaemonSnapshot::default()
        });
        let task_hub = TaskEventHub::spawn(&hub);
        let mut rx = task_hub.subscribe();

        // 基线里的历史状态只登记：`old` 从暂停恢复才算 Start，`live` 同状态不重复。
        hub.apply_daemon_event(progress("live", 1, 42));
        hub.apply_daemon_event(progress("old", 1, 9));
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await??;
        assert_eq!(
            (event.task_id.as_str(), event.kind),
            ("old", TaskEventKind::Start)
        );

        hub.apply_daemon_event(progress("old", 3, 0));
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await??;
        assert_eq!(
            (event.task_id.as_str(), event.kind),
            ("old", TaskEventKind::Complete)
        );

        let speeds = task_hub.live_speeds();
        assert_eq!(speeds.get("live").map(|s| s.download_bps), Some(42));
        assert!(!speeds.contains_key("old"));
        Ok(())
    }

    /// 超过内联上限的整表替换在广播里只是重载宣告：对账改读 hub 的完整投影，消失的任务照常
    /// 得到 `Stop`，而不是被空表（宣告里没有任务）清空整个前态。
    #[tokio::test]
    async fn pump_reconciles_from_the_projection_when_a_large_table_is_only_announced()
    -> Result<(), Box<dyn std::error::Error>> {
        let hub = AgentEventHub::new(AgentSnapshot::default());
        hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![task("gone", 1), task("kept", 1)],
            ..DaemonSnapshot::default()
        });
        let task_hub = TaskEventHub::spawn(&hub);
        let mut rx = task_hub.subscribe();

        let mut tasks: Vec<TaskDto> = (0..=fluxdown_protocol::TABLE_INLINE_LIMIT)
            .map(|index| task(&format!("n{index}"), 2))
            .collect();
        tasks.push(task("kept", 1));
        hub.apply_daemon_event(DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks }));

        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await??;
        assert_eq!(
            (event.task_id.as_str(), event.kind),
            ("gone", TaskEventKind::Stop)
        );
        // `kept` 的前态没有被冲掉：同状态进度不再触发事件。
        hub.apply_daemon_event(progress("kept", 1, 5));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err()
        );
        Ok(())
    }
}
