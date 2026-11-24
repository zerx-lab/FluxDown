//! agent 自有状态与 daemon 缓存的原子投影和事件序列。

use std::sync::{Arc, Mutex, MutexGuard};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, DaemonEvent, DaemonSnapshot, EventFrame, ServiceEvent, Snapshot,
    SnapshotBody, apply_agent_event,
};
use tokio::sync::broadcast;
use uuid::Uuid;

/// 广播队列容量（每个订阅者独立滞后判定）。环形缓冲在建通道时按容量整块分配：槽里放
/// `Arc<EventFrame>`（约 40B / 槽）而不是内联帧（约 600B / 槽），8192 槽常驻约 0.3MB；
/// 每个订阅者 `recv` 也只是引用计数加一，不再深拷贝整帧。
const EVENT_CAPACITY: usize = 8192;

struct AgentEventState {
    epoch: String,
    sequence: u64,
    snapshot: AgentSnapshot,
    #[cfg(all(feature = "desktop", any(windows, test)))]
    tray: TrayRound,
}

#[cfg(all(feature = "desktop", any(windows, test)))]
#[derive(Default)]
struct TrayRound {
    tasks: std::collections::HashMap<String, TrayTask>,
    pending_notices: std::collections::HashMap<String, PendingTrayNotice>,
    terminal_rows: std::collections::HashMap<String, TerminalTrayRow>,
    // Count unread rows for the tray's polling path.
    unread_failures: usize,
    change_sequence: u64,
    acknowledged_sequence: u64,
    sequence: u64,
    reconcile_failures: bool,
}

#[cfg(all(feature = "desktop", any(windows, test)))]
struct TerminalTrayRow {
    sequence: u64,
    status: i32,
    failure: FailureReceipt,
}

#[cfg(all(feature = "desktop", any(windows, test)))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum FailureReceipt {
    AwaitingNotice,
    Unread,
    Seen,
}

#[cfg(all(feature = "desktop", any(windows, test)))]
impl TerminalTrayRow {
    fn new(status: i32, sequence: u64) -> Self {
        Self {
            sequence,
            status,
            failure: FailureReceipt::AwaitingNotice,
        }
    }

    fn confirm_failure(&mut self) -> bool {
        if self.failure != FailureReceipt::AwaitingNotice {
            return false;
        }
        self.failure = FailureReceipt::Unread;
        true
    }
}

#[cfg(all(feature = "desktop", any(windows, test)))]
struct PendingTrayNotice {
    next: TrayTask,
    previous_status: Option<i32>,
    expires: std::time::Instant,
}

#[cfg(all(feature = "desktop", any(windows, test)))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrayTask {
    Unfinished,
    Running,
    Canceled,
    Completed,
    Failed,
}

#[cfg(all(feature = "desktop", any(windows, test)))]
impl TrayTask {
    fn is_unfinished(&self) -> bool {
        matches!(self, Self::Unfinished | Self::Running)
    }

    fn matches_row(self, status: i32) -> bool {
        match self {
            Self::Unfinished => matches!(status, 0 | 2),
            Self::Running => matches!(status, 1 | 5),
            Self::Completed => status == 3,
            Self::Failed => status == 4,
            Self::Canceled => false,
        }
    }
}

#[cfg(all(feature = "desktop", any(windows, test)))]
impl TrayRound {
    fn set(&mut self, id: &str, next: TrayTask) {
        if self.tasks.get(id) == Some(&next) {
            return;
        }
        if next.is_unfinished() && !self.tasks.values().any(TrayTask::is_unfinished) {
            self.tasks.clear();
            // The failed row can stay hidden even after a new round starts.
            self.terminal_rows
                .retain(|_, row| row.failure == FailureReceipt::Unread);
            self.reconcile_failures = false;
        }
        if let Some(task) = self.tasks.get_mut(id) {
            *task = next;
        } else {
            self.tasks.insert(id.to_owned(), next);
        }
        self.change_sequence = self.change_sequence.max(self.sequence);
    }

    fn forget_terminal_row(&mut self, id: &str) {
        if self
            .terminal_rows
            .remove(id)
            .is_some_and(|row| row.failure == FailureReceipt::Unread)
        {
            self.unread_failures -= 1;
        }
    }

    fn acknowledge(&mut self, sequence: u64, seen_failures: &[String]) {
        self.acknowledged_sequence = self.acknowledged_sequence.max(sequence);
        for id in seen_failures {
            let Some(row) = self.terminal_rows.get_mut(id) else {
                continue;
            };
            if row.status != 4 || row.sequence > sequence {
                continue;
            }
            if row.failure == FailureReceipt::Unread {
                self.unread_failures -= 1;
            }
            // A rendered failed row can be acknowledged before its notice.
            row.failure = FailureReceipt::Seen;
            if !self.tasks.contains_key(id) {
                self.terminal_rows.remove(id);
            }
        }
    }

    fn observe_status(&mut self, id: &str, status: i32, error: &str) {
        if matches!(status, 3 | 4) && error != "deleted" {
            if let Some(row) = self.terminal_rows.get_mut(id) {
                if row.status != status {
                    if row.failure == FailureReceipt::Unread {
                        self.unread_failures -= 1;
                    }
                    if self.tasks.contains_key(id) {
                        *row = TerminalTrayRow::new(status, self.sequence);
                    } else {
                        self.terminal_rows.remove(id);
                    }
                }
            } else if self
                .tasks
                .get(id)
                .is_some_and(|task| *task != TrayTask::Canceled)
            {
                self.terminal_rows
                    .insert(id.to_owned(), TerminalTrayRow::new(status, self.sequence));
            }
        } else if !self.terminal_rows.is_empty() {
            self.forget_terminal_row(id);
        }
        match status {
            0 => self.set(id, TrayTask::Unfinished),
            1 | 5 => self.set(id, TrayTask::Running),
            4 if error == "deleted" => self.cancel(id),
            // A live status=4 may still be retried; defer red until the terminal notice.
            2 | 4
                if self.tasks.get(id).is_some_and(|task| {
                    *task == TrayTask::Running || (status == 2 && *task == TrayTask::Failed)
                }) =>
            {
                self.set(id, TrayTask::Unfinished);
            }
            _ => {}
        }
    }

    fn cancel(&mut self, id: &str) {
        self.pending_notices.remove(id);
        self.forget_terminal_row(id);
        if let Some(task) = self.tasks.get_mut(id)
            && (task.is_unfinished() || *task == TrayTask::Failed)
        {
            *task = TrayTask::Canceled;
            self.change_sequence = self.change_sequence.max(self.sequence);
        }
    }

    fn expire_notices(&mut self, now: std::time::Instant) {
        if !self.pending_notices.is_empty() {
            self.pending_notices
                .retain(|_, notice| notice.expires > now);
        }
    }

    fn resolve_notice(&mut self, id: &str, status: i32) {
        let Some(notice) = self.pending_notices.get(id) else {
            return;
        };
        let next = notice.next;
        if notice.expires <= std::time::Instant::now() {
            self.pending_notices.remove(id);
            return;
        }
        if next.matches_row(status) {
            self.pending_notices.remove(id);
            self.set(id, next);
        } else if notice.previous_status != Some(status) {
            // The next distinct row superseded this notice rather than confirming it.
            self.pending_notices.remove(id);
        }
    }

    fn reconcile(&mut self, snapshot: &mut DaemonSnapshot, replaced: bool) {
        if snapshot.tables_loading() {
            return;
        }
        snapshot.ensure_task_index();
        if replaced {
            self.terminal_rows.retain(|id, terminal| {
                let retained = snapshot.task(id).is_some_and(|row| {
                    row.status == terminal.status && row.error_message != "deleted"
                });
                if !retained && terminal.failure == FailureReceipt::Unread {
                    self.unread_failures -= 1;
                }
                retained
            });
        }
        // Only live/queued work enters from the snapshot. A paused task counts only
        // if it was already in this round or later resumes.
        for row in &snapshot.tasks {
            if matches!(row.status, 0 | 1 | 5) {
                self.observe_status(&row.task_id, row.status, &row.error_message);
            }
        }
        self.reconcile_failures = false;
        for (id, task) in &mut self.tasks {
            if *task == TrayTask::Canceled {
                continue;
            }
            let status = snapshot
                .task(id)
                .filter(|row| row.status != 4 || row.error_message != "deleted")
                .map(|row| row.status);
            if let Some(status @ (3 | 4)) = status
                && !self.terminal_rows.contains_key(id)
            {
                self.terminal_rows
                    .insert(id.clone(), TerminalTrayRow::new(status, self.sequence));
            }
            let next = match status {
                Some(2) if task.is_unfinished() || *task == TrayTask::Failed => {
                    TrayTask::Unfinished
                }
                Some(3) => TrayTask::Completed,
                Some(4) if *task == TrayTask::Failed => continue,
                Some(4) if snapshot.runtime_stats.retry_pending_tasks == 0 => TrayTask::Failed,
                // The snapshot reports the retry count, not which tasks are waiting.
                Some(4) => {
                    self.reconcile_failures = true;
                    continue;
                }
                None if task.is_unfinished() || *task == TrayTask::Failed => TrayTask::Canceled,
                _ => continue,
            };
            if *task != next {
                *task = next;
                self.change_sequence = self.change_sequence.max(self.sequence);
            }
            if next == TrayTask::Failed
                && self
                    .terminal_rows
                    .get_mut(id)
                    .is_some_and(TerminalTrayRow::confirm_failure)
            {
                self.unread_failures += 1;
            }
        }
        if !replaced {
            return;
        }
        let now = std::time::Instant::now();
        // A complete replacement is authoritative, including for missing tasks.
        let mut pending = std::mem::take(&mut self.pending_notices);
        for (id, notice) in pending.drain() {
            if notice.expires > now
                && snapshot
                    .task(&id)
                    .is_some_and(|row| notice.next.matches_row(row.status))
            {
                self.set(&id, notice.next);
            }
        }
        self.pending_notices = pending;
    }

    fn apply(&mut self, event: &AgentEvent, snapshot: &mut DaemonSnapshot) {
        use fluxdown_protocol::WsServerMsg;

        match event {
            AgentEvent::DaemonSnapshotReplaced(_)
            | AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(_))
            | AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TasksSnapshot { .. })) => {
                self.reconcile(snapshot, true)
            }
            AgentEvent::Daemon(DaemonEvent::RuntimeStatsChanged(stats))
                if self.reconcile_failures && stats.retry_pending_tasks == 0 =>
            {
                self.reconcile(snapshot, false);
            }
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                status,
                error_message,
                ..
            })) => {
                self.observe_status(task_id, *status, error_message);
                if !self.pending_notices.is_empty()
                    && let Some(row) = snapshot.task(task_id)
                {
                    self.resolve_notice(task_id, row.status);
                }
            }
            AgentEvent::Daemon(DaemonEvent::TaskChanged(task)) => {
                self.observe_status(&task.task_id, task.status, &task.error_message);
                self.resolve_notice(&task.task_id, task.status);
            }
            AgentEvent::Daemon(DaemonEvent::TasksChanged(tasks)) => {
                for task in tasks {
                    self.observe_status(&task.task_id, task.status, &task.error_message);
                    self.resolve_notice(&task.task_id, task.status);
                }
            }
            AgentEvent::Daemon(DaemonEvent::TaskDeleted { task_id }) => self.cancel(task_id),
            AgentEvent::Daemon(DaemonEvent::TasksDeleted { task_ids }) => {
                for id in task_ids {
                    self.cancel(id);
                }
            }
            _ => {}
        }
    }
}

/// agent 物化投影的唯一同步边界。
#[derive(Clone)]
pub struct AgentEventHub {
    state: Arc<Mutex<AgentEventState>>,
    events: broadcast::Sender<Arc<EventFrame>>,
}

impl AgentEventHub {
    #[must_use]
    pub fn new(snapshot: AgentSnapshot) -> Self {
        // 慢连接（网络 RPC 在途 / 网络抖动）期间事件会在广播队列里排队；容量溢出会让该连接
        // 以 4009 event-gap 断线重连并重拉快照。任务进度事件较密，留足余量。
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            state: Arc::new(Mutex::new(AgentEventState {
                epoch: Uuid::new_v4().to_string(),
                sequence: 0,
                snapshot,
                #[cfg(all(feature = "desktop", any(windows, test)))]
                tray: TrayRound::default(),
            })),
            events,
        }
    }

    /// 进程内的**完整**快照（含任务表，O(任务数) 克隆）。线上一律走 [`Self::wire_snapshot`]；
    /// 只需个别字段时用 [`Self::inspect`]。
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let state = lock_or_recover(&self.state);
        let mut snapshot = state.snapshot.clone();
        // 入站配对请求的相对剩余时间按取快照时刻重算（晚连接的客户端不能拿到建请求时的旧值）。
        crate::link::refresh_request_ttls(&mut snapshot.link_pairing_requests);
        Snapshot {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            body: SnapshotBody::Agent(Box::new(snapshot)),
        }
    }

    /// `system.snapshot` 的线上快照：嵌套 daemon 快照只带头（`taskTotal` / `queueTotal`），
    /// 不克隆任务表；表经 [`Self::task_page`] 分页取。
    #[must_use]
    pub fn wire_snapshot(&self) -> Snapshot {
        let state = lock_or_recover(&self.state);
        let mut header = state.snapshot.paged_header();
        crate::link::refresh_request_ttls(&mut header.link_pairing_requests);
        Snapshot {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            body: SnapshotBody::Agent(Box::new(header)),
        }
    }

    /// `system.taskPage`：在同一临界区内读出事件位置与一页行。
    ///
    /// # Errors
    ///
    /// 游标无法解析。
    pub fn task_page(
        &self,
        params: &fluxdown_protocol::TablePageParams,
    ) -> Result<fluxdown_protocol::TablePage, fluxdown_protocol::InvalidPageCursor> {
        let state = lock_or_recover(&self.state);
        fluxdown_protocol::TablePage::build(
            &state.snapshot.daemon,
            &state.epoch,
            state.sequence,
            params,
        )
    }

    #[must_use]
    pub fn subscribe_and_snapshot(&self) -> (broadcast::Receiver<Arc<EventFrame>>, Snapshot) {
        let receiver = self.events.subscribe();
        (receiver, self.snapshot())
    }

    /// 只订阅增量广播（不克隆快照）；订阅早于调用方随后拉取的快照即可。
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<EventFrame>> {
        self.events.subscribe()
    }

    /// 在临界区内只读访问当前投影。高频事件路径用它按需提取少量字段，
    /// 避免每帧克隆整份 `AgentSnapshot`。闭包内不得再调用本 hub（非重入锁）。
    pub fn inspect<R>(&self, read: impl FnOnce(&AgentSnapshot) -> R) -> R {
        let state = lock_or_recover(&self.state);
        read(&state.snapshot)
    }

    #[cfg(all(feature = "desktop", any(windows, test)))]
    pub(crate) fn record_tray_result(&self, notice: &fluxdown_protocol::TaskNoticeDto) {
        let Some(task) = &notice.task else {
            return;
        };
        let next = match notice.event.as_str() {
            "task.created" => TrayTask::Unfinished,
            "task.started" => TrayTask::Running,
            "task.completed" => TrayTask::Completed,
            "task.failed" => TrayTask::Failed,
            _ => return,
        };
        let mut state = lock_or_recover(&self.state);
        // Terminal notices can finish a member of this round, not enroll a
        // historical task whose terminal row is still in the daemon snapshot.
        if !next.is_unfinished()
            && !state
                .tray
                .tasks
                .get(&task.id)
                .is_some_and(|task| *task != TrayTask::Canceled)
        {
            return;
        }
        let status = state
            .snapshot
            .daemon
            .task(&task.id)
            .filter(|row| row.status != 4 || row.error_message != "deleted")
            .map(|row| row.status);
        if status.is_some_and(|status| next.matches_row(status)) {
            state.tray.pending_notices.remove(&task.id);
            // A late terminal notice belongs to the frame that displayed
            // that result, not to intervening unrelated progress frames.
            state.tray.sequence = state
                .tray
                .terminal_rows
                .get(&task.id)
                .filter(|row| Some(row.status) == status)
                .map_or(state.sequence, |row| row.sequence);
            state.tray.set(&task.id, next);
            if next == TrayTask::Failed {
                let tray = &mut state.tray;
                let confirmed = if let Some(row) = tray.terminal_rows.get_mut(&task.id) {
                    if row.status != 4 {
                        *row = TerminalTrayRow::new(4, tray.sequence);
                    }
                    row.confirm_failure()
                } else {
                    let mut row = TerminalTrayRow::new(4, tray.sequence);
                    let confirmed = row.confirm_failure();
                    tray.terminal_rows.insert(task.id.clone(), row);
                    confirmed
                };
                if confirmed {
                    tray.unread_failures += 1;
                }
            }
        } else if next.is_unfinished() && status.is_none() {
            state.tray.pending_notices.insert(
                task.id.clone(),
                PendingTrayNotice {
                    next,
                    previous_status: status,
                    // Bound unmatched notices even when no more task frames arrive.
                    expires: std::time::Instant::now() + std::time::Duration::from_secs(30),
                },
            );
        }
    }

    #[cfg(all(feature = "desktop", any(windows, test)))]
    pub(crate) fn acknowledge_tray_downloads(
        &self,
        epoch: &str,
        sequence: u64,
        seen_failures: &[String],
    ) {
        let mut state = lock_or_recover(&self.state);
        if state.epoch == epoch && sequence <= state.sequence {
            state.tray.acknowledge(sequence, seen_failures);
        }
    }

    #[cfg(all(feature = "desktop", any(windows, test)))]
    pub(crate) fn tray_download_dot(&self) -> Option<crate::shell::DownloadDot> {
        use crate::shell::DownloadDot;
        let mut state = lock_or_recover(&self.state);
        if !state.tray.pending_notices.is_empty() {
            state.tray.expire_notices(std::time::Instant::now());
        }
        if !state.snapshot.daemon_connected
            || state
                .snapshot
                .preferences
                .values
                .get("desktop.tray_download_status")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
        {
            return None;
        }
        let tray = &state.tray;
        let stats = &state.snapshot.daemon.runtime_stats;
        if tray.unread_failures > 0 {
            Some(DownloadDot::Failed)
        } else if tray.change_sequence <= tray.acknowledged_sequence {
            None
        } else if stats.active_tasks > 0
            || stats.pending_tasks > 0
            || stats.retry_pending_tasks > 0
            || tray.tasks.values().any(TrayTask::is_unfinished)
        {
            Some(DownloadDot::Downloading)
        } else {
            let mut eligible = tray
                .tasks
                .values()
                .filter(|&&task| task != TrayTask::Canceled);
            (eligible.next() == Some(&TrayTask::Completed)
                && eligible.all(|task| *task == TrayTask::Completed))
            .then_some(DownloadDot::Completed)
        }
    }

    /// daemon 增量先更新缓存，再发布 agent sequence。
    pub fn apply_daemon_event(&self, event: DaemonEvent) -> Arc<EventFrame> {
        self.publish(AgentEvent::Daemon(event))
    }

    /// daemon 重连快照原子替换并发布替换事件。
    pub fn replace_daemon_snapshot(&self, snapshot: DaemonSnapshot) -> Arc<EventFrame> {
        self.publish(AgentEvent::DaemonSnapshotReplaced(snapshot))
    }

    /// 先更新完整投影，再递增 sequence 并广播。广播 / 线上形态里的整表替换换成头或重载宣告
    /// （[`AgentEvent::into_wire`]），任何一帧都不随任务数增长；订阅者经 `system.taskPage`
    /// 分页取表，进程内消费者读 [`Self::inspect`] 的完整投影。
    pub fn publish(&self, event: AgentEvent) -> Arc<EventFrame> {
        let mut state = lock_or_recover(&self.state);
        apply_agent_event(&mut state.snapshot, &event);
        state.sequence = state.sequence.saturating_add(1);
        #[cfg(all(feature = "desktop", any(windows, test)))]
        {
            let AgentEventState {
                snapshot,
                tray,
                sequence,
                ..
            } = &mut *state;
            tray.sequence = *sequence;
            tray.apply(&event, &mut snapshot.daemon);
        }
        let frame = Arc::new(EventFrame {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            previous: None,
            event: ServiceEvent::Agent(event.into_wire()),
        });
        // 无订阅者时快照仍需更新，但无需入队，也不逐事件刷生命周期日志。
        if self.events.receiver_count() > 0 && self.events.send(Arc::clone(&frame)).is_err() {
            tracing::trace!(
                sequence = frame.sequence,
                "last agent event subscriber disconnected"
            );
        }
        frame
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "desktop")]
    fn acknowledge_current(hub: &AgentEventHub, seen: &[&str]) {
        let snapshot = hub.snapshot();
        let failures: Vec<_> = seen.iter().map(|id| (*id).to_owned()).collect();
        hub.acknowledge_tray_downloads(&snapshot.epoch, snapshot.sequence, &failures);
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_does_not_resurface_an_already_visible_failure() -> Result<(), serde_json::Error> {
        let hub = AgentEventHub::new(AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        });
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 1)?));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 4)?));
        acknowledge_current(&hub, &["a"]);
        hub.publish(fluxdown_protocol::AgentEvent::DaemonConnectionChanged(true));
        hub.record_tray_result(&failed_notice("a"));
        assert_eq!(hub.tray_download_dot(), None);
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_only_clears_failures_named_in_the_receipt() -> Result<(), serde_json::Error> {
        use crate::shell::DownloadDot;

        let hub = AgentEventHub::new(AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        });
        for id in ["visible", "hidden"] {
            hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task(id, 1)?));
        }
        let failed = hub.apply_daemon_event(DaemonEvent::TasksChanged(vec![
            tray_task("visible", 4)?,
            tray_task("hidden", 4)?,
        ]));
        for id in ["visible", "hidden"] {
            hub.record_tray_result(&failed_notice(id));
        }
        acknowledge_current(&hub, &["visible", "visible"]);
        hub.record_tray_result(&failed_notice("visible"));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));

        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("new", 1)?));
        acknowledge_current(&hub, &[]);
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        hub.acknowledge_tray_downloads("another epoch", failed.sequence, &["hidden".into()]);
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        hub.acknowledge_tray_downloads(&failed.epoch, failed.sequence, &["hidden".into()]);
        assert_eq!(hub.tray_download_dot(), None);
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_uses_the_failed_row_sequence_for_a_late_notice() -> Result<(), serde_json::Error> {
        use crate::shell::DownloadDot;
        use fluxdown_protocol::AgentEvent;

        let hub = AgentEventHub::new(AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        });
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 1)?));
        let failed = hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 4)?));
        hub.publish(AgentEvent::DaemonConnectionChanged(true));
        acknowledge_current(&hub, &[]);
        assert_eq!(hub.tray_download_dot(), None);
        hub.record_tray_result(&failed_notice("a"));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        hub.acknowledge_tray_downloads(&failed.epoch, failed.sequence, &["a".into()]);
        assert_eq!(hub.tray_download_dot(), None);
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_discards_stale_receipts_after_retry_and_removes_missing_failures()
    -> Result<(), serde_json::Error> {
        use crate::shell::DownloadDot;

        let hub = AgentEventHub::new(AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        });
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 1)?));
        let first_failure = hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 4)?));
        hub.record_tray_result(&failed_notice("a"));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 0)?));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 4)?));
        hub.acknowledge_tray_downloads(&first_failure.epoch, first_failure.sequence, &["a".into()]);
        hub.record_tray_result(&failed_notice("a"));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));

        hub.apply_daemon_event(DaemonEvent::TaskDeleted {
            task_id: "a".into(),
        });
        hub.record_tray_result(&failed_notice("a"));
        assert_eq!(hub.tray_download_dot(), None);

        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("b", 1)?));
        let failed = hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("b", 4)?));
        hub.record_tray_result(&failed_notice("b"));
        hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![tray_task("b", 4)?],
            ..DaemonSnapshot::default()
        });
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        hub.acknowledge_tray_downloads(&failed.epoch, failed.sequence, &["b".into()]);
        assert_eq!(hub.tray_download_dot(), None);

        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("c", 1)?));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("c", 4)?));
        hub.record_tray_result(&failed_notice("c"));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        hub.apply_daemon_event(DaemonEvent::Engine(WsServerMsg::TasksSnapshot {
            tasks: Vec::new(),
        }));
        assert_eq!(hub.tray_download_dot(), None);
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_ignores_a_failure_notice_from_the_previous_round() -> Result<(), serde_json::Error>
    {
        use crate::shell::DownloadDot;

        let hub = AgentEventHub::new(AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        });
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 1)?));
        // Snapshot reconciliation can finish the round before the notice arrives.
        hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![tray_task("old", 4)?],
            ..DaemonSnapshot::default()
        });
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        acknowledge_current(&hub, &["old"]);

        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("new", 1)?));
        hub.record_tray_result(&failed_notice("old"));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("new", 3)?));
        hub.record_tray_result(&fluxdown_protocol::TaskNoticeDto {
            event: "task.completed".into(),
            ..failed_notice("new")
        });
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Completed));

        // The same task ID can join a new round by actually resuming.
        acknowledge_current(&hub, &[]);
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 1)?));
        hub.record_tray_result(&failed_notice("old"));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 4)?));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        hub.record_tray_result(&failed_notice("old"));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_does_not_apply_a_superseded_failure_to_a_later_attempt()
    -> Result<(), serde_json::Error> {
        let hub = AgentEventHub::new(AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        });
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 1)?));
        hub.record_tray_result(&failed_notice("a"));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 0)?));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 1)?));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("a", 4)?));
        assert_eq!(
            hub.tray_download_dot(),
            Some(crate::shell::DownloadDot::Downloading)
        );
        Ok(())
    }

    #[cfg(feature = "desktop")]
    fn failed_notice(id: &str) -> fluxdown_protocol::TaskNoticeDto {
        fluxdown_protocol::TaskNoticeDto {
            delivery_id: String::new(),
            event: "task.failed".into(),
            timestamp_ms: 0,
            queue_id: String::new(),
            queue_name: String::new(),
            task: Some(fluxdown_protocol::TaskNoticeTaskDto {
                id: id.into(),
                ..Default::default()
            }),
        }
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_discards_create_notices_superseded_by_running_rows() -> Result<(), serde_json::Error>
    {
        let hub = AgentEventHub::new(AgentSnapshot::default());
        for (id, row_first) in [("early", false), ("late", true)] {
            let row = DaemonEvent::TaskChanged(tray_task(id, 1)?);
            if row_first {
                hub.apply_daemon_event(row.clone());
            }
            hub.record_tray_result(&fluxdown_protocol::TaskNoticeDto {
                delivery_id: String::new(),
                event: "task.created".into(),
                timestamp_ms: 0,
                queue_id: String::new(),
                queue_name: String::new(),
                task: Some(fluxdown_protocol::TaskNoticeTaskDto {
                    id: id.into(),
                    ..Default::default()
                }),
            });
            if !row_first {
                hub.apply_daemon_event(row);
            }
            assert!(
                !super::lock_or_recover(&hub.state)
                    .tray
                    .pending_notices
                    .contains_key(id)
            );
        }
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_requires_a_successful_round_and_acknowledges_failure()
    -> Result<(), serde_json::Error> {
        use crate::shell::DownloadDot;

        let hub = AgentEventHub::new(AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        });
        let notice = |id: &str, event: &str| -> Result<(), serde_json::Error> {
            if matches!(event, "task.completed" | "task.failed") {
                let status = if event == "task.completed" { 3 } else { 4 };
                hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task(id, status)?));
            }
            hub.record_tray_result(&fluxdown_protocol::TaskNoticeDto {
                event: event.to_owned(),
                ..failed_notice(id)
            });
            if matches!(event, "task.started" | "task.created") {
                let status = i32::from(event == "task.started");
                hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task(id, status)?));
            }
            Ok(())
        };
        notice("a", "task.started")?;
        notice("b", "task.created")?;
        let before_failure = hub.snapshot();
        notice("a", "task.failed")?;
        hub.acknowledge_tray_downloads(
            &before_failure.epoch,
            before_failure.sequence,
            &["a".into()],
        );
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Failed));
        acknowledge_current(&hub, &["a"]);
        assert_eq!(hub.tray_download_dot(), None);
        notice("b", "task.completed")?;
        assert_eq!(hub.tray_download_dot(), None);
        notice("c", "task.started")?;
        notice("d", "task.created")?;
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        let seen = hub.snapshot();
        acknowledge_current(&hub, &[]);
        assert_eq!(hub.tray_download_dot(), None);
        notice("c", "task.completed")?;
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        notice("d", "task.completed")?;
        hub.acknowledge_tray_downloads(&seen.epoch, seen.sequence, &[]);
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Completed));
        acknowledge_current(&hub, &[]);
        assert_eq!(hub.tray_download_dot(), None);

        notice("e", "task.started")?;
        notice("f", "task.created")?;
        notice("e", "task.completed")?;
        acknowledge_current(&hub, &[]);
        hub.apply_daemon_event(DaemonEvent::TaskDeleted {
            task_id: "f".to_owned(),
        });
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Completed));

        acknowledge_current(&hub, &[]);
        notice("g", "task.started")?;
        hub.apply_daemon_event(DaemonEvent::TaskDeleted {
            task_id: "g".to_owned(),
        });
        assert_eq!(hub.tray_download_dot(), None);
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_reconciles_terminal_tasks_after_disconnect() -> Result<(), serde_json::Error> {
        use crate::shell::DownloadDot;
        use fluxdown_protocol::AgentEvent;

        for (status, expected) in [(3, DownloadDot::Completed), (4, DownloadDot::Failed)] {
            let hub = AgentEventHub::new(AgentSnapshot::default());
            hub.replace_daemon_snapshot(DaemonSnapshot {
                tasks: vec![tray_task("a", 1)?],
                ..DaemonSnapshot::default()
            });
            hub.publish(AgentEvent::DaemonConnectionChanged(true));
            assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
            acknowledge_current(&hub, &[]);
            hub.publish(AgentEvent::DaemonConnectionChanged(false));
            hub.replace_daemon_snapshot(DaemonSnapshot {
                tasks: vec![tray_task("a", status)?],
                ..DaemonSnapshot::default()
            });
            assert_eq!(hub.tray_download_dot(), None);
            hub.publish(AgentEvent::DaemonConnectionChanged(true));
            assert_eq!(hub.tray_download_dot(), Some(expected));
        }
        Ok(())
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn tray_dot_ignores_old_paused_work_until_resume() -> Result<(), serde_json::Error> {
        use crate::shell::DownloadDot;
        use fluxdown_protocol::AgentEvent;

        let hub = AgentEventHub::new(AgentSnapshot::default());
        hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![tray_task("old", 2)?, tray_task("today", 1)?],
            ..DaemonSnapshot::default()
        });
        hub.publish(AgentEvent::DaemonConnectionChanged(true));
        hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![tray_task("old", 2)?, tray_task("today", 3)?],
            ..DaemonSnapshot::default()
        });
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Completed));
        acknowledge_current(&hub, &[]);

        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 1)?));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 2)?));
        acknowledge_current(&hub, &[]);
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 1)?));
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        acknowledge_current(&hub, &[]);
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 1)?));
        assert_eq!(hub.tray_download_dot(), None);

        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("old", 2)?));
        hub.apply_daemon_event(DaemonEvent::TaskChanged(tray_task("later", 1)?));
        hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![tray_task("old", 2)?, tray_task("later", 3)?],
            ..DaemonSnapshot::default()
        });
        assert_eq!(hub.tray_download_dot(), Some(DownloadDot::Downloading));
        Ok(())
    }

    #[cfg(feature = "desktop")]
    fn tray_task(id: &str, status: i32) -> Result<TaskDto, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "taskId": id,
            "url": "https://example.com/file",
            "fileName": "file.bin",
            "saveDir": "/tmp",
            "status": status,
            "downloadedBytes": 0,
            "totalBytes": 100,
            "createdAt": "1"
        }))
    }

    use fluxdown_protocol::{
        AgentSnapshot, DaemonEvent, DaemonSnapshot, SnapshotBody, TaskDto, WsServerMsg,
    };

    use super::AgentEventHub;

    #[test]
    fn daemon_snapshot_then_delta_share_agent_sequence_and_preserve_metadata()
    -> Result<(), serde_json::Error> {
        let task = serde_json::from_value::<TaskDto>(serde_json::json!({
            "taskId": "task-1",
            "url": "https://example.com/file",
            "fileName": "file.bin",
            "saveDir": "/tmp",
            "status": 0,
            "downloadedBytes": 0,
            "totalBytes": 100,
            "errorMessage": "",
            "createdAt": "1",
            "proxyUrl": "",
            "queueId": "main",
            "checksum": ""
        }))?;
        let hub = AgentEventHub::new(AgentSnapshot::default());
        let replaced = hub.replace_daemon_snapshot(DaemonSnapshot {
            tasks: vec![task],
            ..DaemonSnapshot::default()
        });
        let delta = hub.apply_daemon_event(DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: "task-1".to_owned(),
            status: 5,
            downloaded_bytes: 1,
            total_bytes: 100,
            speed: 0,
            file_name: String::new(),
            save_dir: String::new(),
            upload_speed: 0,
            url: String::new(),
            error_message: String::new(),
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        }));
        assert_eq!(delta.sequence, replaced.sequence + 1);
        assert_eq!(delta.epoch, replaced.epoch);
        let SnapshotBody::Agent(snapshot) = hub.snapshot().body else {
            panic!("agent hub returned daemon root snapshot");
        };
        assert_eq!(snapshot.daemon.tasks[0].url, "https://example.com/file");
        assert_eq!(snapshot.daemon.tasks[0].save_dir, "/tmp");
        assert_eq!(snapshot.daemon.tasks[0].status, 5);
        Ok(())
    }
}
