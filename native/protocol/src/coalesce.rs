//! 落后订阅者的事件帧合并队列。
//!
//! 转发者（daemon / agent 的 WebSocket 连接）把广播帧先放进 [`FrameQueue`]，套接字可写时再
//! 逐帧取出。订阅者跟得上时队列始终近空、不发生任何合并；落后时，同一实体的「后帧完整取代
//! 前帧」的事件在队列内就地合并，积压量被实体数而非事件数封顶，不必再以 4009 断线重拉快照。
//!
//! 取出的帧带 `previous`（覆盖区间 `(previous, sequence]`），消费方用
//! [`crate::judge_frame`] 判定。合并只在两帧之间没有其它触及同一任务的帧时发生（全局重置帧
//! 阻断一切合并），因此合并结果与逐帧应用等价，与快照重叠时重放也不改变结果。

use std::collections::{HashMap, HashSet, VecDeque};

use crate::daemon::{QueueOrderEntryDto, QueuePositionDto, WsServerMsg};
use crate::event::{AgentEvent, DaemonEvent, EventFrame, ServiceEvent, apply_queue_order};

/// 可合并实体。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Key {
    Progress(String),
    Runtime(String),
    Segments(String),
    RuntimeStats,
    Queue,
}

/// 一帧对合并的影响。
enum Touch<'a> {
    /// 不影响任何可合并实体（活动日志、插件视图、agent 自有状态等）。
    None,
    /// 触及这些任务：之后同一任务的可合并帧不能越过它与之前的帧合并。
    Tasks(Vec<&'a str>),
    /// 整体替换（快照替换、连接断开）：之前的帧一律不可再合并。
    All,
    /// 可合并实体本身。
    Keyed(Key),
}

/// 帧合并队列。见模块文档。
#[derive(Debug, Default)]
pub struct FrameQueue {
    slots: VecDeque<Option<EventFrame>>,
    /// `slots[0]` 的绝对下标。
    head: u64,
    /// 实体 → 其最近一帧的绝对下标。
    keys: HashMap<Key, u64>,
    /// 任务 → 最近一帧触及它的绝对下标。
    touched: HashMap<String, u64>,
    /// 最近一次整体替换帧的绝对下标 + 1（之前的帧不可合并）。
    barrier: u64,
    live: usize,
    /// 已取出的最后一帧序号；首帧以首个入队帧的覆盖起点为准。
    last_sent: Option<u64>,
    first_from: Option<u64>,
}

impl FrameQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 待发送的帧数（合并后）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.live
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// 入队；能与同一实体尚未发送的前帧合并时取代前帧。
    pub fn push(&mut self, frame: EventFrame) {
        if self.first_from.is_none() && self.last_sent.is_none() {
            self.first_from = Some(frame.covers_from());
        }
        let index = self.head + self.slots.len() as u64;
        let mut frame = frame;
        match touch(&frame) {
            Touch::None => {}
            Touch::All => {
                self.barrier = index + 1;
                self.keys.clear();
                self.touched.clear();
            }
            Touch::Tasks(ids) => {
                for id in ids {
                    self.touched.insert(id.to_owned(), index);
                }
            }
            Touch::Keyed(key) => {
                if let Some(earlier) = self.mergeable(&key) {
                    let slot = (earlier - self.head) as usize;
                    let merged = self
                        .slots
                        .get(slot)
                        .and_then(Option::as_ref)
                        .is_some_and(|previous| merge(previous, &mut frame));
                    if merged && let Some(slot) = self.slots.get_mut(slot) {
                        *slot = None;
                        self.live -= 1;
                    }
                }
                if let Some(task) = key_task(&key) {
                    self.touched.insert(task.to_owned(), index);
                }
                self.keys.insert(key, index);
            }
        }
        self.slots.push_back(Some(frame));
        self.live += 1;
    }

    /// 取出下一帧，`previous` 写成本队列上一帧的序号（与原序号相接时省略）。
    pub fn pop(&mut self) -> Option<EventFrame> {
        while let Some(slot) = self.slots.pop_front() {
            self.head += 1;
            let Some(mut frame) = slot else {
                continue;
            };
            self.live -= 1;
            let previous = self
                .last_sent
                .or(self.first_from)
                .unwrap_or_else(|| frame.covers_from());
            frame.previous = (previous != frame.sequence.saturating_sub(1)).then_some(previous);
            self.last_sent = Some(frame.sequence);
            if self.live == 0 {
                self.slots.clear();
                self.keys.clear();
                self.touched.clear();
            }
            return Some(frame);
        }
        None
    }

    /// 该实体最近一帧仍在队列中、其后既无整体替换也无触及同一任务的帧时，返回其下标。
    fn mergeable(&self, key: &Key) -> Option<u64> {
        let earlier = *self.keys.get(key)?;
        if earlier < self.head || earlier < self.barrier {
            return None;
        }
        if let Some(task) = key_task(key)
            && self.touched.get(task) != Some(&earlier)
        {
            return None;
        }
        Some(earlier)
    }
}

fn key_task(key: &Key) -> Option<&str> {
    match key {
        Key::Progress(task) | Key::Runtime(task) | Key::Segments(task) => Some(task),
        Key::RuntimeStats | Key::Queue => None,
    }
}

fn daemon_event(frame: &EventFrame) -> Option<&DaemonEvent> {
    match &frame.event {
        ServiceEvent::Daemon(event) | ServiceEvent::Agent(AgentEvent::Daemon(event)) => Some(event),
        ServiceEvent::Agent(_) => None,
    }
}

fn daemon_event_mut(frame: &mut EventFrame) -> Option<&mut DaemonEvent> {
    match &mut frame.event {
        ServiceEvent::Daemon(event) | ServiceEvent::Agent(AgentEvent::Daemon(event)) => Some(event),
        ServiceEvent::Agent(_) => None,
    }
}

fn touch(frame: &EventFrame) -> Touch<'_> {
    match &frame.event {
        ServiceEvent::Daemon(event) | ServiceEvent::Agent(AgentEvent::Daemon(event)) => {
            daemon_touch(event)
        }
        ServiceEvent::Agent(event) => match event {
            AgentEvent::DaemonSnapshotReplaced(_) | AgentEvent::DaemonConnectionChanged(_) => {
                Touch::All
            }
            AgentEvent::Daemon(_)
            | AgentEvent::SessionChanged(_)
            | AgentEvent::SyncChanged(_)
            | AgentEvent::CloudConnectionChanged(_)
            | AgentEvent::PreferencesChanged(_)
            | AgentEvent::GatewayChanged(_)
            | AgentEvent::CloudDevicesChanged(_)
            | AgentEvent::LinkedDevicesChanged(_)
            | AgentEvent::LinkPairingRequestsChanged(_)
            | AgentEvent::LinkDiscoveredChanged(_)
            | AgentEvent::RemoteTasksChanged(_)
            | AgentEvent::PendingCapturesChanged(_)
            | AgentEvent::ShellChanged(_)
            | AgentEvent::PowerChanged(_)
            | AgentEvent::UpdateChanged(_)
            | AgentEvent::CloudNotifyChanged(_)
            | AgentEvent::CaptureTasksStarted(_)
            | AgentEvent::CaptureTasksResumed(_)
            | AgentEvent::SessionRevoked(_) => Touch::None,
        },
    }
}

fn daemon_touch(event: &DaemonEvent) -> Touch<'_> {
    match event {
        DaemonEvent::TaskRuntimeChanged(runtime) => {
            Touch::Keyed(Key::Runtime(runtime.task_id.clone()))
        }
        DaemonEvent::RuntimeStatsChanged(_) => Touch::Keyed(Key::RuntimeStats),
        DaemonEvent::SnapshotReplaced(_) => Touch::All,
        // 任务表重载是整体替换；只重载队列位置表则与队列整表 / 增量同键。
        DaemonEvent::TablesReloading { tasks: Some(_), .. } => Touch::All,
        DaemonEvent::TablesReloading { tasks: None, .. } => Touch::Keyed(Key::Queue),
        DaemonEvent::Engine(message) => engine_touch(message),
        DaemonEvent::TaskChanged(task) => Touch::Tasks(vec![task.task_id.as_str()]),
        DaemonEvent::TasksChanged(tasks) => {
            Touch::Tasks(tasks.iter().map(|task| task.task_id.as_str()).collect())
        }
        DaemonEvent::TaskDeleted { task_id } => Touch::Tasks(vec![task_id.as_str()]),
        DaemonEvent::TasksDeleted { task_ids } => {
            Touch::Tasks(task_ids.iter().map(String::as_str).collect())
        }
        DaemonEvent::TaskActivityAdded(_)
        | DaemonEvent::QueuesChanged(_)
        | DaemonEvent::GroupsChanged(_)
        | DaemonEvent::ConfigChanged(_)
        | DaemonEvent::RssChanged { .. }
        | DaemonEvent::PluginsChanged(_)
        | DaemonEvent::ComponentsChanged(_)
        | DaemonEvent::WebhooksChanged(_)
        | DaemonEvent::WebhooksCleared
        | DaemonEvent::SelectionPending(_)
        | DaemonEvent::SelectionResolved { .. }
        | DaemonEvent::TaskNotice(_)
        | DaemonEvent::PluginViewFrame(_)
        | DaemonEvent::PluginViewClosed { .. }
        | DaemonEvent::PluginToast { .. }
        | DaemonEvent::BtGuardChanged
        | DaemonEvent::BtGuardCloudRefreshWanted { .. }
        | DaemonEvent::BtGuardEvidenceReady { .. } => Touch::None,
    }
}

fn engine_touch(message: &WsServerMsg) -> Touch<'_> {
    match message {
        WsServerMsg::TaskProgress {
            task_id,
            status,
            error_message,
            ..
        } => {
            if *status == 4 && error_message == "deleted" {
                Touch::Tasks(vec![task_id.as_str()])
            } else {
                Touch::Keyed(Key::Progress(task_id.clone()))
            }
        }
        WsServerMsg::SegmentProgress { task_id, .. } => {
            Touch::Keyed(Key::Segments(task_id.clone()))
        }
        WsServerMsg::QueuePositionsChanged { .. } | WsServerMsg::QueueOrderChanged { .. } => {
            Touch::Keyed(Key::Queue)
        }
        WsServerMsg::TasksSnapshot { .. } => Touch::All,
        WsServerMsg::SegmentSplit { task_id, .. }
        | WsServerMsg::TaskCdnEvent { task_id, .. }
        | WsServerMsg::TaskMetaProbed { task_id, .. }
        | WsServerMsg::TaskQueueChanged { task_id, .. }
        | WsServerMsg::TaskRouteChanged { task_id, .. }
        | WsServerMsg::HlsSelectionRequest { task_id, .. }
        | WsServerMsg::BtSelectionRequest { task_id, .. }
        | WsServerMsg::ResolveVariantRequest { task_id, .. }
        | WsServerMsg::PluginHookActivity { task_id, .. } => Touch::Tasks(vec![task_id.as_str()]),
        WsServerMsg::DuplicateTorrent {
            task_id,
            existing_task_id,
            ..
        } => Touch::Tasks(vec![task_id.as_str(), existing_task_id.as_str()]),
        WsServerMsg::FileMissingChanged { updates } => Touch::Tasks(
            updates
                .iter()
                .map(|update| update.task_id.as_str())
                .collect(),
        ),
        WsServerMsg::QueuesChanged { .. }
        | WsServerMsg::PriorityTaskChanged { .. }
        | WsServerMsg::Pong {}
        | WsServerMsg::PluginAutoDisabled { .. }
        | WsServerMsg::PluginsChanged {}
        | WsServerMsg::GroupsChanged { .. }
        | WsServerMsg::RssSourcesChanged { .. }
        | WsServerMsg::RssItemsChanged { .. }
        | WsServerMsg::RssFeedValidated { .. }
        | WsServerMsg::ComponentProgress { .. }
        | WsServerMsg::ComponentResult { .. }
        | WsServerMsg::LinkIncomingPairing { .. }
        | WsServerMsg::LinkDevicesChanged {}
        | WsServerMsg::WebhookDeliveriesChanged { .. } => Touch::None,
    }
}

/// 把同一实体的前帧 `earlier` 并入后帧 `later`（原地改写后帧）；返回 `true` 表示后帧已
/// 完整代表二者，前帧可丢弃——结果与先后应用二者等价。返回 `false` 时后帧未被改动。
fn merge(earlier: &EventFrame, later: &mut EventFrame) -> bool {
    let (Some(previous), Some(next)) = (daemon_event(earlier), daemon_event_mut(later)) else {
        return false;
    };
    match (previous, next) {
        (DaemonEvent::RuntimeStatsChanged(_), DaemonEvent::RuntimeStatsChanged(_)) => true,
        (DaemonEvent::TaskRuntimeChanged(old), DaemonEvent::TaskRuntimeChanged(new)) => {
            // 新采样自带分段且序号更新时完整取代旧采样（空分段会继承上一采样，不能越过）。
            !new.segments.is_empty()
                && old.sample_sequence != 0
                && new.sample_sequence > old.sample_sequence
        }
        (DaemonEvent::Engine(old), DaemonEvent::Engine(new)) => merge_engine(old, new),
        // 队列表重载宣告取代之前同键（队列）的任何整表 / 增量：重载后的表已含它们。
        (_, DaemonEvent::TablesReloading { tasks: None, .. }) => true,
        _ => false,
    }
}

fn merge_engine(old: &WsServerMsg, new: &mut WsServerMsg) -> bool {
    match (old, new) {
        (
            WsServerMsg::TaskProgress {
                status: old_status,
                file_name: old_name,
                save_dir: old_dir,
                url: old_url,
                ..
            },
            WsServerMsg::TaskProgress {
                status,
                file_name,
                save_dir,
                url,
                ..
            },
        ) => {
            // 状态不变时后帧覆盖全部字段；空串字段表示「不变」，从前帧继承。
            if old_status != status {
                return false;
            }
            if file_name.is_empty() {
                file_name.clone_from(old_name);
            }
            if save_dir.is_empty() {
                save_dir.clone_from(old_dir);
            }
            if url.is_empty() {
                url.clone_from(old_url);
            }
            true
        }
        (WsServerMsg::SegmentProgress { .. }, WsServerMsg::SegmentProgress { .. }) => true,
        (_, WsServerMsg::QueuePositionsChanged { .. }) => true,
        (
            WsServerMsg::QueuePositionsChanged { positions: base },
            next @ WsServerMsg::QueueOrderChanged { .. },
        ) => {
            let WsServerMsg::QueueOrderChanged { removed, added } = &*next else {
                return false;
            };
            let mut positions: Vec<QueuePositionDto> = base.clone();
            apply_queue_order(&mut positions, removed, added);
            *next = WsServerMsg::QueuePositionsChanged { positions };
            true
        }
        (
            WsServerMsg::QueueOrderChanged {
                removed: old_removed,
                added: old_added,
            },
            WsServerMsg::QueueOrderChanged { removed, added },
        ) => {
            let (composed_removed, composed_added) =
                compose_queue_order(old_removed, old_added, removed, added);
            *removed = composed_removed;
            *added = composed_added;
            true
        }
        _ => false,
    }
}

/// 两个队列增量的复合（先 `d1` 后 `d2`），语义同 [`apply_queue_order`] 的「先删后插」。
fn compose_queue_order(
    old_removed: &[String],
    old_added: &[QueueOrderEntryDto],
    removed: &[String],
    added: &[QueueOrderEntryDto],
) -> (Vec<String>, Vec<QueueOrderEntryDto>) {
    let overridden: HashSet<&str> = removed
        .iter()
        .map(String::as_str)
        .chain(added.iter().map(|entry| entry.task_id.as_str()))
        .collect();
    let mut seen = HashSet::with_capacity(old_removed.len() + removed.len());
    let composed_removed: Vec<String> = old_removed
        .iter()
        .chain(removed)
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect();
    let composed_added: Vec<QueueOrderEntryDto> = old_added
        .iter()
        .filter(|entry| !overridden.contains(entry.task_id.as_str()))
        .chain(added)
        .cloned()
        .collect();
    (composed_removed, composed_added)
}

#[cfg(test)]
mod tests {
    use super::FrameQueue;
    use crate::daemon::{QueueOrderEntryDto, QueuePositionDto, TaskDto, WsServerMsg};
    use crate::event::{
        AgentEvent, DaemonEvent, DaemonSnapshot, EventFrame, FrameVerdict, ServiceEvent,
        apply_daemon_event, judge_frame,
    };
    use crate::{DaemonRuntimeStatsDto, TaskRuntimeDto, TaskSegmentDto};

    fn frame(sequence: u64, event: DaemonEvent) -> EventFrame {
        EventFrame {
            epoch: "e".to_owned(),
            sequence,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(event)),
        }
    }

    fn task(id: &str) -> TaskDto {
        serde_json::from_value(serde_json::json!({
            "taskId": id, "url": "https://example.com/f", "fileName": "", "saveDir": "/tmp",
            "status": 0, "downloadedBytes": 0, "totalBytes": 0, "errorMessage": "",
            "createdAt": "1", "proxyUrl": "", "queueId": "main", "checksum": ""
        }))
        .expect("task")
    }

    fn progress(id: &str, status: i32, downloaded: i64, name: &str) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: id.to_owned(),
            status,
            downloaded_bytes: downloaded,
            total_bytes: 100,
            speed: downloaded,
            upload_speed: 0,
            file_name: name.to_owned(),
            save_dir: String::new(),
            url: String::new(),
            error_message: String::new(),
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        })
    }

    fn runtime(id: &str, sample: u64, active: u32) -> DaemonEvent {
        DaemonEvent::TaskRuntimeChanged(TaskRuntimeDto {
            task_id: id.to_owned(),
            sample_sequence: sample,
            active_transfers: Some(active),
            segments: vec![TaskSegmentDto {
                index: 0,
                start_byte: 0,
                end_byte: 99,
                downloaded_bytes: i64::try_from(sample).unwrap_or(0),
                active: Some(true),
            }],
            ..Default::default()
        })
    }

    fn order(removed: &[&str], added: &[(&str, i64)]) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::QueueOrderChanged {
            removed: removed.iter().map(|id| (*id).to_owned()).collect(),
            added: added
                .iter()
                .map(|(id, order)| QueueOrderEntryDto {
                    task_id: (*id).to_owned(),
                    order: *order,
                })
                .collect(),
        })
    }

    fn drain(queue: &mut FrameQueue) -> Vec<EventFrame> {
        std::iter::from_fn(|| queue.pop()).collect()
    }

    fn apply_all(base: &DaemonSnapshot, frames: &[EventFrame]) -> serde_json::Value {
        let mut snapshot = base.clone();
        for frame in frames {
            let ServiceEvent::Agent(AgentEvent::Daemon(event)) = &frame.event else {
                panic!("daemon frame expected");
            };
            apply_daemon_event(&mut snapshot, event);
        }
        serde_json::to_value(&snapshot).expect("snapshot json")
    }

    fn base() -> DaemonSnapshot {
        let mut snapshot = DaemonSnapshot {
            tasks: vec![task("a"), task("b"), task("c")],
            ..Default::default()
        };
        apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::Engine(WsServerMsg::QueuePositionsChanged {
                positions: vec![
                    QueuePositionDto {
                        task_id: "b".to_owned(),
                        position: 1,
                        order: 10,
                    },
                    QueuePositionDto {
                        task_id: "c".to_owned(),
                        position: 2,
                        order: 20,
                    },
                ],
            }),
        );
        snapshot
    }

    /// 一段混合事件：逐帧应用与合并后应用得到同一快照；合并后的帧链满足 judge_frame。
    #[test]
    fn coalesced_stream_applies_to_the_same_snapshot() {
        let events = vec![
            progress("a", 1, 10, "a.bin"),
            runtime("a", 1, 2),
            DaemonEvent::RuntimeStatsChanged(DaemonRuntimeStatsDto {
                active_tasks: 1,
                ..Default::default()
            }),
            progress("a", 1, 20, ""),
            order(&["b"], &[("a", 30)]),
            DaemonEvent::TaskActivityAdded(Default::default()),
            runtime("a", 2, 3),
            progress("b", 1, 5, ""),
            DaemonEvent::TaskChanged(task("b")),
            progress("b", 1, 7, ""),
            progress("a", 3, 100, ""),
            order(&["c"], &[("d", 40)]),
            DaemonEvent::RuntimeStatsChanged(DaemonRuntimeStatsDto {
                active_tasks: 0,
                ..Default::default()
            }),
            progress("a", 3, 100, ""),
        ];
        let frames: Vec<EventFrame> = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| frame(index as u64 + 1, event))
            .collect();
        let mut queue = FrameQueue::new();
        for frame in frames.iter().cloned() {
            queue.push(frame);
        }
        let coalesced = drain(&mut queue);
        assert!(
            coalesced.len() < frames.len(),
            "落后时应合并：{}",
            coalesced.len()
        );
        assert_eq!(apply_all(&base(), &coalesced), apply_all(&base(), &frames));

        // 每一个帧边界上的快照都能接上合并流的后续帧。
        for cut in 0..=frames.len() {
            let cursor = cut as u64;
            let mut snapshot = base();
            for frame in &frames[..cut] {
                let ServiceEvent::Agent(AgentEvent::Daemon(event)) = &frame.event else {
                    unreachable!()
                };
                apply_daemon_event(&mut snapshot, event);
            }
            let mut position = cursor;
            let mut accepted = Vec::new();
            for frame in &coalesced {
                match judge_frame("e", position, frame) {
                    FrameVerdict::Accept => {
                        position = frame.sequence;
                        accepted.push(frame.clone());
                    }
                    FrameVerdict::Skip => {}
                    FrameVerdict::Resync => panic!("cut {cut}: 帧 {} 不应断档", frame.sequence),
                }
            }
            assert_eq!(
                apply_all(&snapshot, &accepted),
                apply_all(&base(), &frames),
                "cut {cut}"
            );
        }
    }

    #[test]
    fn status_change_and_task_barrier_keep_frames_apart() {
        let mut queue = FrameQueue::new();
        queue.push(frame(1, progress("a", 1, 10, "")));
        queue.push(frame(2, progress("a", 2, 10, "")));
        queue.push(frame(3, DaemonEvent::TaskChanged(task("a"))));
        queue.push(frame(4, progress("a", 2, 10, "")));
        assert_eq!(drain(&mut queue).len(), 4);
    }

    #[test]
    fn caught_up_subscriber_sees_untouched_frames() {
        let mut queue = FrameQueue::new();
        for sequence in 1..=3 {
            queue.push(frame(sequence, progress("a", 1, sequence as i64, "")));
            let sent = queue.pop().expect("frame");
            assert_eq!((sent.sequence, sent.previous), (sequence, None));
        }
    }

    #[test]
    fn merged_frame_declares_the_skipped_range() {
        let mut queue = FrameQueue::new();
        queue.push(frame(5, progress("a", 1, 1, "")));
        queue.push(frame(6, progress("b", 1, 1, "")));
        queue.push(frame(7, progress("a", 1, 2, "")));
        let sent = drain(&mut queue);
        assert_eq!(
            sent.iter().map(|frame| frame.sequence).collect::<Vec<_>>(),
            [6, 7]
        );
        assert_eq!(sent[0].previous, Some(4));
        assert_eq!(sent[1].previous, None);
        assert_eq!(judge_frame("e", 4, &sent[0]), FrameVerdict::Accept);
        assert_eq!(judge_frame("e", 3, &sent[0]), FrameVerdict::Resync);
    }

    #[test]
    fn judge_frame_rules() {
        let at = |sequence, previous| EventFrame {
            epoch: "e".to_owned(),
            sequence,
            previous,
            event: ServiceEvent::Daemon(DaemonEvent::WebhooksCleared),
        };
        assert_eq!(judge_frame("e", 5, &at(6, None)), FrameVerdict::Accept);
        assert_eq!(judge_frame("e", 5, &at(5, None)), FrameVerdict::Skip);
        assert_eq!(judge_frame("e", 5, &at(7, None)), FrameVerdict::Resync);
        assert_eq!(judge_frame("e", 5, &at(9, Some(5))), FrameVerdict::Accept);
        assert_eq!(judge_frame("e", 5, &at(9, Some(2))), FrameVerdict::Accept);
        assert_eq!(judge_frame("e", 5, &at(9, Some(6))), FrameVerdict::Resync);
        assert_eq!(judge_frame("f", 5, &at(6, None)), FrameVerdict::Resync);
    }
}
