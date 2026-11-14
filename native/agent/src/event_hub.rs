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
