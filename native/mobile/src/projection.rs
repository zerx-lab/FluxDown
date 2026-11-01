//! 权威投影：持有一份 `AgentSnapshot`，按 epoch/sequence 游标接受事件帧，
//! 经协议层 `apply_agent_event` 推进投影，并把被接受的事件归一成 `HostSignalDto`。
//!
//! 端口自 `crates/app/src/agent_client.rs`（`forward_event`）与 `web/src/lib/rpc/cursor.ts`；
//! 纯同步、无 IO，便于单测。

use std::collections::HashMap;

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, CUSTOM_CATEGORIES_PREF_KEY, CategoryRouteDto, DaemonEvent,
    EventFrame, FrameVerdict, ServiceEvent, Snapshot, SnapshotBody, TableSync, WsServerMsg,
    accepted_runtime_status, apply_agent_event, judge_frame,
};

use crate::dto::{
    CategoryDto, CloudDeviceDto, GroupDto, HostEventDto, HostInfoDto, HostSignalDto,
    HostSnapshotDto, LinkDeviceDto, QueueDto, QueueOrderDto, RssSourceDto, RuntimeStatsDto,
    SelectionRequestDto, queue_order_dtos, string_map,
};
use crate::sections::{self, Section};

/// 游标判定为必须重同步的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResyncReason {
    /// 帧的 epoch 与快照不同（服务重启 / 事件源替换）。
    EpochChanged,
    /// 序号不连续（丢帧）。
    Gap,
    /// 帧不是 agent 事件（连接对端不是 agent）。
    ForeignService,
}

/// 单帧判定结果。
#[derive(Debug, PartialEq)]
pub(crate) enum FrameOutcome {
    /// 重复或过期帧：丢弃。
    Skip,
    /// 需要重新拉取快照。
    Resync(ResyncReason),
    /// 已接受并推进投影；按序下发 0..n 个信号：类型化事件 → 分区变化 → 一次性通知
    /// （移动端不渲染也无分区 / 通知的事件为空）。
    Applied(Vec<HostSignalDto>),
}

/// 快照被拒绝（不是 agent 角色的快照）。
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SnapshotRejected;

pub(crate) struct Projection {
    info: HostInfoDto,
    snapshot: AgentSnapshot,
    epoch: String,
    sequence: u64,
    /// 最近一次下发的分类，用于只在变化时发 `CategoriesChanged`。
    categories: Vec<CategoryDto>,
    /// 各分区最近一次下发的 JSON（按 [`Section::index`]），用于只在变化时发 `SectionChanged`。
    sections: Vec<String>,
}

impl Projection {
    /// 以 `system.snapshot` 的结果建立投影；客户端必须拿到 agent 角色的快照。
    pub(crate) fn from_snapshot(
        info: HostInfoDto,
        snapshot: Snapshot,
    ) -> Result<Self, SnapshotRejected> {
        let SnapshotBody::Agent(agent) = snapshot.body else {
            return Err(SnapshotRejected);
        };
        let categories = CategoryDto::from_preferences(&agent.preferences);
        let sections = sections::render_all(&agent);
        Ok(Self {
            info,
            snapshot: *agent,
            epoch: snapshot.epoch,
            sequence: snapshot.sequence,
            categories,
            sections,
        })
    }

    pub(crate) fn epoch(&self) -> &str {
        &self.epoch
    }

    /// 当前偏好 `custom_categories` 构建的分类路由；没有任何分类配置目录时为 `None`。
    pub(crate) fn category_route(&self) -> Option<CategoryRouteDto> {
        CategoryRouteDto::from_preference(
            self.snapshot
                .preferences
                .values
                .get(CUSTOM_CATEGORIES_PREF_KEY),
        )
    }

    #[cfg(test)]
    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn snapshot_dto(&self) -> HostSnapshotDto {
        let sections: HashMap<String, String> = Section::ALL
            .iter()
            .zip(&self.sections)
            .map(|(section, json)| (section.key().to_owned(), json.clone()))
            .collect();
        HostSnapshotDto::from_agent(&self.info, &self.snapshot, sections)
    }

    /// 整表替换后的下发信号：分类 / 分区基线同步刷新，避免随后的增量事件误判为变化。
    /// 投影处于加载态时 DTO 里 `tasks` 为空、`tasks_loading = true`；载入完成后由
    /// [`Self::finish_load`] 再发一次完整快照。
    fn snapshot_signal(&mut self) -> HostSignalDto {
        self.categories = CategoryDto::from_preferences(&self.snapshot.preferences);
        self.sections = sections::render_all(&self.snapshot);
        HostSignalDto::Snapshot {
            snapshot: self.snapshot_dto(),
        }
    }

    /// 投影处于加载态时，以当前游标为基准帧建立一轮分页载入；否则 `None`。
    pub(crate) fn table_sync(&self) -> Option<TableSync> {
        TableSync::for_snapshot(self.epoch.as_str(), self.sequence, &self.snapshot.daemon)
    }

    /// 载入完成：把表装入投影，返回完整快照信号与需按游标回放的缓冲帧。
    pub(crate) fn finish_load(&mut self, sync: TableSync) -> (HostSignalDto, Vec<EventFrame>) {
        let backlog = sync.finish(&mut self.snapshot.daemon);
        (self.snapshot_signal(), backlog)
    }

    /// 游标规则统一走 `judge_frame`：epoch 不同 → 重同步；`seq <= cursor` → 跳过；
    /// 覆盖区间 `(previous, seq]` 与游标相接（含越过游标的合并帧）→ 应用；其余（断档）→ 重同步。
    pub(crate) fn accept(&mut self, frame: EventFrame) -> FrameOutcome {
        match judge_frame(&self.epoch, self.sequence, &frame) {
            FrameVerdict::Resync if frame.epoch != self.epoch => {
                return FrameOutcome::Resync(ResyncReason::EpochChanged);
            }
            FrameVerdict::Resync => return FrameOutcome::Resync(ResyncReason::Gap),
            FrameVerdict::Skip => return FrameOutcome::Skip,
            FrameVerdict::Accept => {}
        }
        let ServiceEvent::Agent(event) = &frame.event else {
            return FrameOutcome::Resync(ResyncReason::ForeignService);
        };
        self.sequence = frame.sequence;
        FrameOutcome::Applied(self.process(event))
    }

    /// 应用事件并产出该帧的全部信号。
    fn process(&mut self, event: &AgentEvent) -> Vec<HostSignalDto> {
        let mut signals = self.translate(event);
        for section in sections::affected(event) {
            let json = section.json(&self.snapshot);
            let Some(sent) = self.sections.get_mut(section.index()) else {
                continue;
            };
            if *sent != json {
                sent.clone_from(&json);
                signals.push(event_signal(HostEventDto::SectionChanged {
                    name: section.key().to_owned(),
                    json,
                }));
            }
        }
        if let Some((name, json)) = sections::notice(event) {
            signals.push(event_signal(HostEventDto::Notice { name, json }));
        }
        signals
    }

    /// 应用事件并归一为信号。无法逐项表达的整表替换直接下发新快照；
    /// 批量事件（`TasksChanged`）展开为逐任务信号，Kotlin / Swift 端归约器无需感知批量。
    fn translate(&mut self, event: &AgentEvent) -> Vec<HostSignalDto> {
        match event {
            AgentEvent::Daemon(daemon) => self.translate_daemon(event, daemon),
            AgentEvent::DaemonSnapshotReplaced(_) => {
                apply_agent_event(&mut self.snapshot, event);
                vec![self.snapshot_signal()]
            }
            AgentEvent::DaemonConnectionChanged(connected) => {
                apply_agent_event(&mut self.snapshot, event);
                vec![event_signal(HostEventDto::DaemonConnectionChanged {
                    connected: *connected,
                })]
            }
            AgentEvent::SessionChanged(_)
            | AgentEvent::CloudConnectionChanged(_)
            | AgentEvent::CloudDevicesChanged(_) => {
                let before = self.snapshot.cloud_devices.clone();
                apply_agent_event(&mut self.snapshot, event);
                let explicit = matches!(event, AgentEvent::CloudDevicesChanged(_));
                (explicit || before != self.snapshot.cloud_devices)
                    .then(|| {
                        event_signal(HostEventDto::CloudDevicesChanged {
                            devices: self
                                .snapshot
                                .cloud_devices
                                .iter()
                                .map(CloudDeviceDto::from)
                                .collect(),
                        })
                    })
                    .into_iter()
                    .collect()
            }
            AgentEvent::LinkedDevicesChanged(_) => {
                apply_agent_event(&mut self.snapshot, event);
                vec![event_signal(HostEventDto::LinkedDevicesChanged {
                    devices: self
                        .snapshot
                        .linked_devices
                        .iter()
                        .map(LinkDeviceDto::from)
                        .collect(),
                })]
            }
            AgentEvent::PreferencesChanged(_) => {
                apply_agent_event(&mut self.snapshot, event);
                let categories = CategoryDto::from_preferences(&self.snapshot.preferences);
                if categories == self.categories {
                    return Vec::new();
                }
                self.categories.clone_from(&categories);
                vec![event_signal(HostEventDto::CategoriesChanged { categories })]
            }
            // 无类型化 DTO 的 agent 状态（同步 / 网关 / 远程任务 / 捕获 / 外壳 / 电源 /
            // 局域网配对 / 一次性通知）：这里只推进投影，分区变化与通知由 `process` 经
            // `sections` 通道下发。
            AgentEvent::SyncChanged(_)
            | AgentEvent::GatewayChanged(_)
            | AgentEvent::LinkPairingRequestsChanged(_)
            | AgentEvent::LinkDiscoveredChanged(_)
            | AgentEvent::RemoteTasksChanged(_)
            | AgentEvent::PendingCapturesChanged(_)
            | AgentEvent::ShellChanged(_)
            | AgentEvent::PowerChanged(_)
            | AgentEvent::CloudNotifyChanged(_)
            | AgentEvent::UpdateChanged(_)
            | AgentEvent::CaptureTasksStarted(_)
            | AgentEvent::CaptureTasksResumed(_)
            | AgentEvent::SessionRevoked(_) => {
                apply_agent_event(&mut self.snapshot, event);
                Vec::new()
            }
        }
    }

    fn translate_daemon(&mut self, outer: &AgentEvent, event: &DaemonEvent) -> Vec<HostSignalDto> {
        // 落后于已投影序号 / 未知任务的运行时采样不下发（与 GPUI 一致），游标照常推进。
        if let DaemonEvent::TaskRuntimeChanged(runtime) = event
            && accepted_runtime_status(&self.snapshot.daemon, runtime).is_none()
        {
            return Vec::new();
        }
        // 分段几何只在没有真实采样时写入：用前后对比决定是否下发。
        let geometry_before = match event {
            DaemonEvent::Engine(WsServerMsg::SegmentProgress { task_id, .. }) => Some((
                task_id.as_str(),
                self.snapshot.daemon.task_runtime.get(task_id).cloned(),
            )),
            _ => None,
        };
        // 先借出 task_id，再变更投影。
        let geometry_task = geometry_before.as_ref().map(|(id, _)| (*id).to_owned());
        let geometry_prev = geometry_before.and_then(|(_, runtime)| runtime);
        apply_agent_event(&mut self.snapshot, outer);
        let daemon = &self.snapshot.daemon;

        let single = match event {
            DaemonEvent::TaskRuntimeChanged(runtime) => {
                daemon.task_runtime.get(&runtime.task_id).map(|merged| {
                    event_signal(HostEventDto::TaskRuntimeChanged {
                        runtime: merged.into(),
                    })
                })
            }
            DaemonEvent::TaskActivityAdded(_)
            | DaemonEvent::TaskNotice(_)
            // 插件可移植视图事件：v1 移动端不渲染插件 UI，不改变投影。
            | DaemonEvent::PluginViewFrame(_)
            | DaemonEvent::PluginViewClosed { .. }
            | DaemonEvent::PluginToast { .. }
            | DaemonEvent::PluginsChanged(_)
            | DaemonEvent::ComponentsChanged(_)
            | DaemonEvent::WebhooksChanged(_)
            | DaemonEvent::WebhooksCleared
            // BT 防吸血：状态变化经 `Notice` 通知宿主刷新；云名单 / 证据事件只由 agent 消费。
            | DaemonEvent::BtGuardChanged
            | DaemonEvent::BtGuardCloudRefreshWanted { .. }
            | DaemonEvent::BtGuardEvidenceReady { .. }
            | DaemonEvent::RssChanged { .. } => None,
            // 整表替换 / 宣告重载：投影已进入加载态，下发「加载态」快照（`tasks_loading = true`）。
            DaemonEvent::SnapshotReplaced(_) | DaemonEvent::TablesReloading { .. } => {
                Some(self.snapshot_signal())
            }
            DaemonEvent::Engine(message) => {
                self.translate_engine(message, geometry_task, geometry_prev)
            }
            DaemonEvent::TaskChanged(task) => Some(event_signal(HostEventDto::TaskChanged {
                task: task.into(),
            })),
            // 与逐条 `TaskChanged` 等价：投影已整批应用，宿主端按任务归约。
            DaemonEvent::TasksChanged(tasks) => {
                return tasks
                    .iter()
                    .map(|task| event_signal(HostEventDto::TaskChanged { task: task.into() }))
                    .collect();
            }
            DaemonEvent::TaskDeleted { task_id } => Some(event_signal(HostEventDto::TaskDeleted {
                task_id: task_id.clone(),
            })),
            // 与逐条 `TaskDeleted` 等价：投影已整批移除，宿主端按任务归约。
            DaemonEvent::TasksDeleted { task_ids } => {
                return task_ids
                    .iter()
                    .map(|task_id| {
                        event_signal(HostEventDto::TaskDeleted {
                            task_id: task_id.clone(),
                        })
                    })
                    .collect();
            }
            DaemonEvent::QueuesChanged(queues) => Some(queues_signal(queues)),
            DaemonEvent::GroupsChanged(groups) => Some(groups_signal(groups)),
            DaemonEvent::ConfigChanged(config) => Some(event_signal(HostEventDto::ConfigChanged {
                values: string_map(&config.values),
                revision: config.revision,
            })),
            DaemonEvent::RuntimeStatsChanged(stats) => {
                Some(event_signal(HostEventDto::RuntimeStatsChanged {
                    stats: RuntimeStatsDto::from(stats),
                }))
            }
            DaemonEvent::SelectionPending(request) => {
                Some(event_signal(HostEventDto::SelectionPending {
                    request: SelectionRequestDto::from(request),
                }))
            }
            DaemonEvent::SelectionResolved { request_id } => {
                Some(event_signal(HostEventDto::SelectionResolved {
                    request_id: request_id.clone(),
                }))
            }
        };
        single.into_iter().collect()
    }

    fn translate_engine(
        &mut self,
        message: &WsServerMsg,
        geometry_task: Option<String>,
        geometry_prev: Option<fluxdown_protocol::TaskRuntimeDto>,
    ) -> Option<HostSignalDto> {
        let daemon = &self.snapshot.daemon;
        match message {
            WsServerMsg::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                total_bytes,
                speed,
                upload_speed,
                file_name,
                error_message,
                uploaded_bytes,
                seeding_status,
                ..
            } => Some(event_signal(HostEventDto::TaskProgress {
                task_id: task_id.clone(),
                status: *status,
                downloaded_bytes: *downloaded_bytes,
                total_bytes: *total_bytes,
                speed: *speed,
                upload_speed: *upload_speed,
                file_name: file_name.clone(),
                error_message: error_message.clone(),
                uploaded_bytes: *uploaded_bytes,
                seeding_status: *seeding_status,
            })),
            WsServerMsg::TasksSnapshot { .. } => Some(self.snapshot_signal()),
            WsServerMsg::SegmentProgress { .. } => {
                let task_id = geometry_task?;
                let current = daemon.task_runtime.get(&task_id)?;
                (geometry_prev.as_ref() != Some(current)).then(|| {
                    event_signal(HostEventDto::TaskRuntimeChanged {
                        runtime: current.into(),
                    })
                })
            }
            WsServerMsg::TaskMetaProbed { task_id, .. }
            | WsServerMsg::TaskQueueChanged { task_id, .. }
            | WsServerMsg::TaskRouteChanged { task_id, .. } => daemon
                .tasks
                .iter()
                .find(|task| task.task_id == *task_id)
                .map(|task| event_signal(HostEventDto::TaskChanged { task: task.into() })),
            WsServerMsg::QueuesChanged { queues } => Some(queues_signal(queues)),
            WsServerMsg::QueuePositionsChanged { positions } => {
                Some(event_signal(HostEventDto::QueuePositionsChanged {
                    order: queue_order_dtos(positions),
                }))
            }
            WsServerMsg::QueueOrderChanged { removed, added } => {
                Some(event_signal(HostEventDto::QueueOrderChanged {
                    removed: removed.clone(),
                    added: added.iter().map(QueueOrderDto::from).collect(),
                }))
            }
            WsServerMsg::GroupsChanged { groups } => Some(groups_signal(groups)),
            WsServerMsg::RssSourcesChanged { sources } => {
                Some(event_signal(HostEventDto::RssSourcesChanged {
                    sources: sources.iter().map(RssSourceDto::from).collect(),
                }))
            }
            WsServerMsg::FileMissingChanged { updates } => {
                Some(event_signal(HostEventDto::FileMissingChanged {
                    updates: updates
                        .iter()
                        .map(|update| (update.task_id.clone(), update.missing))
                        .collect(),
                }))
            }
            WsServerMsg::PriorityTaskChanged {
                priority_task_id, ..
            } => Some(event_signal(HostEventDto::PriorityTaskChanged {
                task_id: (!priority_task_id.is_empty()).then(|| priority_task_id.clone()),
            })),
            // 其余引擎消息：状态类由分区覆盖，一次性类（插件熔断 / 重复种子 / RSS 条目 / 组件进度……）
            // 由 `sections::notice` 转发；分段拆分 / `Pong` 不转发；旧版选择请求走
            // `DaemonEvent::SelectionPending`。
            _ => None,
        }
    }
}

fn event_signal(event: HostEventDto) -> HostSignalDto {
    HostSignalDto::Event { event }
}

fn queues_signal(queues: &[fluxdown_protocol::QueueDto]) -> HostSignalDto {
    event_signal(HostEventDto::QueuesChanged {
        queues: queues.iter().map(QueueDto::from).collect(),
    })
}

fn groups_signal(groups: &[fluxdown_protocol::GroupDto]) -> HostSignalDto {
    event_signal(HostEventDto::GroupsChanged {
        groups: groups.iter().map(GroupDto::from).collect(),
    })
}

/// 只为测试 / 调试：一份投影里的任务 DTO。
#[cfg(test)]
pub(crate) fn task_dtos(projection: &Projection) -> Vec<crate::dto::TaskDto> {
    projection
        .snapshot
        .daemon
        .tasks
        .iter()
        .map(crate::dto::TaskDto::from)
        .collect()
}

#[cfg(test)]
pub(crate) fn runtime_dtos(projection: &Projection) -> Vec<crate::dto::TaskRuntimeDto> {
    projection
        .snapshot
        .daemon
        .task_runtime
        .values()
        .map(crate::dto::TaskRuntimeDto::from)
        .collect()
}

#[cfg(test)]
pub(crate) mod fixtures {
    use fluxdown_protocol::{
        AgentEvent, AgentSnapshot, DaemonEvent, DaemonSnapshot, EventFrame, ServiceEvent, Snapshot,
        SnapshotBody, TaskDto,
    };
    use serde_json::json;

    pub(crate) const EPOCH: &str = "epoch-a";

    pub(crate) fn task(id: &str, status: i32) -> TaskDto {
        serde_json::from_value(json!({
            "taskId": id, "url": format!("https://example.com/{id}"), "fileName": format!("{id}.bin"),
            "saveDir": "/data", "status": status, "downloadedBytes": 0, "totalBytes": 100,
            "errorMessage": "", "createdAt": "1700000000", "proxyUrl": "", "queueId": "main",
            "checksum": ""
        }))
        .expect("valid task fixture")
    }

    pub(crate) fn snapshot(sequence: u64, tasks: Vec<TaskDto>) -> Snapshot {
        Snapshot {
            epoch: EPOCH.to_owned(),
            sequence,
            body: SnapshotBody::Agent(Box::new(AgentSnapshot {
                daemon: DaemonSnapshot {
                    tasks,
                    ..Default::default()
                },
                daemon_connected: true,
                ..Default::default()
            })),
        }
    }

    /// 快照头：任务表 / 队列位置表待载入，`tasks` / `queuePositions` 为空。
    pub(crate) fn header(sequence: u64, task_total: u64, queue_total: Option<u64>) -> Snapshot {
        Snapshot {
            epoch: EPOCH.to_owned(),
            sequence,
            body: SnapshotBody::Agent(Box::new(AgentSnapshot {
                daemon: DaemonSnapshot {
                    task_total: Some(task_total),
                    queue_total,
                    ..Default::default()
                },
                daemon_connected: true,
                ..Default::default()
            })),
        }
    }

    pub(crate) fn frame(sequence: u64, event: AgentEvent) -> EventFrame {
        EventFrame {
            epoch: EPOCH.to_owned(),
            sequence,
            previous: None,
            event: ServiceEvent::Agent(event),
        }
    }

    pub(crate) fn daemon_frame(sequence: u64, event: DaemonEvent) -> EventFrame {
        frame(sequence, AgentEvent::Daemon(event))
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        AgentEvent, DaemonEvent, EventFrame, ServiceEvent, TaskRuntimeDto as WireRuntime,
        TaskSegmentDto, WsServerMsg,
    };

    use super::fixtures::{EPOCH, daemon_frame, frame, snapshot, task};
    use super::{FrameOutcome, Projection, ResyncReason, Section, runtime_dtos, task_dtos};
    use crate::dto::{HostEventDto, HostInfoDto, HostSignalDto, HostSnapshotDto, QueueOrderDto};

    fn info() -> HostInfoDto {
        HostInfoDto {
            service_name: "fluxdown-agent".to_owned(),
            service_version: "1.0.0".to_owned(),
            protocol_version: fluxdown_protocol::PROTOCOL_VERSION,
            capabilities: Vec::new(),
        }
    }

    fn projection(sequence: u64, tasks: Vec<fluxdown_protocol::TaskDto>) -> Projection {
        Projection::from_snapshot(info(), snapshot(sequence, tasks)).expect("agent snapshot")
    }

    /// 该帧下发的全部事件（必须是 `Applied` 且只含 `Event` 信号）。
    fn events(outcome: FrameOutcome) -> Vec<HostEventDto> {
        let FrameOutcome::Applied(signals) = outcome else {
            panic!("expected Applied, got {outcome:?}");
        };
        signals
            .into_iter()
            .map(|signal| match signal {
                HostSignalDto::Event { event } => event,
                other => panic!("unexpected signal {other:?}"),
            })
            .collect()
    }

    fn only_event(outcome: FrameOutcome) -> HostEventDto {
        let mut all = events(outcome);
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }

    fn progress(task_id: &str, status: i32, speed: i64, error: &str) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: task_id.to_owned(),
            status,
            downloaded_bytes: 40,
            total_bytes: 100,
            speed,
            upload_speed: 7,
            file_name: String::new(),
            save_dir: String::new(),
            url: String::new(),
            error_message: error.to_owned(),
            uploaded_bytes: 3,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        })
    }

    #[test]
    fn rejects_non_agent_snapshot() {
        let mut daemon = snapshot(1, Vec::new());
        daemon.body = fluxdown_protocol::SnapshotBody::Daemon(Box::default());
        assert!(Projection::from_snapshot(info(), daemon).is_err());
    }

    #[test]
    fn cursor_skips_duplicates_and_stale_frames() {
        let mut projection = projection(5, vec![task("a", 1)]);
        for sequence in [3, 4, 5] {
            let outcome = projection.accept(daemon_frame(
                sequence,
                DaemonEvent::TaskDeleted {
                    task_id: "a".to_owned(),
                },
            ));
            assert_eq!(outcome, FrameOutcome::Skip, "sequence {sequence}");
        }
        assert_eq!(projection.sequence(), 5);
        assert_eq!(
            task_dtos(&projection).len(),
            1,
            "skipped frames must not apply"
        );
    }

    #[test]
    fn cursor_applies_next_sequence_and_advances() {
        let mut projection = projection(5, vec![task("a", 1)]);
        let outcome = projection.accept(daemon_frame(
            6,
            DaemonEvent::TaskDeleted {
                task_id: "a".to_owned(),
            },
        ));
        assert_eq!(
            outcome,
            FrameOutcome::Applied(vec![HostSignalDto::Event {
                event: HostEventDto::TaskDeleted {
                    task_id: "a".to_owned()
                }
            }])
        );
        assert_eq!(projection.sequence(), 6);
        assert!(task_dtos(&projection).is_empty());
    }

    #[test]
    fn cursor_gap_requests_resync_without_applying() {
        let mut projection = projection(5, vec![task("a", 1)]);
        let outcome = projection.accept(daemon_frame(
            7,
            DaemonEvent::TaskDeleted {
                task_id: "a".to_owned(),
            },
        ));
        assert_eq!(outcome, FrameOutcome::Resync(ResyncReason::Gap));
        assert_eq!(projection.sequence(), 5);
        assert_eq!(task_dtos(&projection).len(), 1);
    }

    #[test]
    fn epoch_change_requests_resync_even_for_old_sequence() {
        let mut projection = projection(5, vec![task("a", 1)]);
        let mut other = daemon_frame(
            1,
            DaemonEvent::TaskDeleted {
                task_id: "a".to_owned(),
            },
        );
        other.epoch = "epoch-b".to_owned();
        assert_eq!(
            projection.accept(other),
            FrameOutcome::Resync(ResyncReason::EpochChanged)
        );
        assert_eq!(projection.epoch(), EPOCH);
    }

    #[test]
    fn non_agent_service_event_requests_resync() {
        let mut projection = projection(1, Vec::new());
        let foreign = EventFrame {
            epoch: EPOCH.to_owned(),
            sequence: 2,
            previous: None,
            event: ServiceEvent::Daemon(DaemonEvent::SelectionResolved {
                request_id: "r".to_owned(),
            }),
        };
        assert_eq!(
            projection.accept(foreign),
            FrameOutcome::Resync(ResyncReason::ForeignService)
        );
    }

    #[test]
    fn task_progress_carries_speed_and_patches_projection() {
        let mut projection = projection(1, vec![task("a", 1)]);
        let outcome = projection.accept(daemon_frame(2, progress("a", 1, 1234, "")));
        let HostEventDto::TaskProgress {
            task_id,
            status,
            downloaded_bytes,
            speed,
            upload_speed,
            uploaded_bytes,
            ..
        } = only_event(outcome)
        else {
            panic!("expected a TaskProgress event");
        };
        assert_eq!(task_id, "a");
        assert_eq!((status, downloaded_bytes), (1, 40));
        assert_eq!((speed, upload_speed, uploaded_bytes), (1234, 7, 3));
        assert_eq!(task_dtos(&projection)[0].downloaded_bytes, 40);
    }

    #[test]
    fn deleted_sentinel_passes_through_and_removes_task() {
        let mut projection = projection(1, vec![task("a", 1)]);
        let outcome = projection.accept(daemon_frame(2, progress("a", 4, 0, "deleted")));
        let HostEventDto::TaskProgress {
            status,
            error_message,
            ..
        } = only_event(outcome)
        else {
            panic!("sentinel must be forwarded as TaskProgress");
        };
        assert_eq!((status, error_message.as_str()), (4, "deleted"));
        assert!(
            task_dtos(&projection).is_empty(),
            "projection drops the task"
        );
    }

    fn runtime(task_id: &str, sequence: u64, segments: usize) -> WireRuntime {
        WireRuntime {
            task_id: task_id.to_owned(),
            sample_sequence: sequence,
            active_transfers: Some(2),
            total_bytes: 100,
            segments: (0..segments)
                .map(|index| TaskSegmentDto {
                    index: i32::try_from(index).unwrap_or(0),
                    start_byte: 0,
                    end_byte: 10,
                    downloaded_bytes: 1,
                    active: Some(true),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn runtime_sample_is_merged_and_stale_sample_is_dropped() {
        let mut projection = projection(1, vec![task("a", 1)]);
        let first = projection.accept(daemon_frame(
            2,
            DaemonEvent::TaskRuntimeChanged(runtime("a", 5, 2)),
        ));
        assert_eq!(events(first).len(), 1);
        // 同序号 / 更旧序号：不下发，但游标推进。
        let stale = projection.accept(daemon_frame(
            3,
            DaemonEvent::TaskRuntimeChanged(runtime("a", 5, 1)),
        ));
        assert_eq!(stale, FrameOutcome::Applied(Vec::new()));
        assert_eq!(projection.sequence(), 3);
        // 空分段保留上一次分段。
        let merged = projection.accept(daemon_frame(
            4,
            DaemonEvent::TaskRuntimeChanged(runtime("a", 6, 0)),
        ));
        let HostEventDto::TaskRuntimeChanged { runtime } = only_event(merged) else {
            panic!("expected merged runtime");
        };
        assert_eq!((runtime.sample_sequence, runtime.segments.len()), (6, 2));
        assert_eq!(runtime_dtos(&projection)[0].segments.len(), 2);
    }

    #[test]
    fn unknown_task_runtime_is_not_forwarded() {
        let mut projection = projection(1, Vec::new());
        let outcome = projection.accept(daemon_frame(
            2,
            DaemonEvent::TaskRuntimeChanged(runtime("ghost", 1, 1)),
        ));
        assert_eq!(outcome, FrameOutcome::Applied(Vec::new()));
    }

    #[test]
    fn queue_order_delta_forwards_only_changes_and_advances_projection() {
        use fluxdown_protocol::{QueueOrderEntryDto, QueuePositionDto};
        let position = |id: &str, order: i64| QueuePositionDto {
            task_id: id.to_owned(),
            position: 0,
            order,
        };
        let mut projection = projection(1, vec![task("a", 3), task("b", 3), task("c", 3)]);
        projection.accept(daemon_frame(
            2,
            DaemonEvent::Engine(WsServerMsg::QueuePositionsChanged {
                positions: vec![position("a", 10), position("b", 20), position("c", 30)],
            }),
        ));
        let outcome = projection.accept(daemon_frame(
            3,
            DaemonEvent::Engine(WsServerMsg::QueueOrderChanged {
                removed: vec!["a".to_owned()],
                added: vec![QueueOrderEntryDto {
                    task_id: "d".to_owned(),
                    order: 25,
                }],
            }),
        ));
        assert_eq!(
            outcome,
            FrameOutcome::Applied(vec![HostSignalDto::Event {
                event: HostEventDto::QueueOrderChanged {
                    removed: vec!["a".to_owned()],
                    added: vec![QueueOrderDto {
                        task_id: "d".to_owned(),
                        order: 25
                    }],
                }
            }])
        );
        // 快照侧：增量后整表按键升序，队首出队不改其余任务的键。
        let HostSnapshotDto { queue_order, .. } = projection.snapshot_dto();
        let ids: Vec<(&str, i64)> = queue_order
            .iter()
            .map(|entry| (entry.task_id.as_str(), entry.order))
            .collect();
        assert_eq!(ids, [("b", 20), ("d", 25), ("c", 30)]);
    }

    #[test]
    fn merged_frame_overlapping_cursor_is_accepted_but_gap_is_not() {
        let mut projection = projection(5, vec![task("a", 1)]);
        // 覆盖 (3, 7]，游标 5 落在区间内：接受并推进到 7。
        let mut merged = daemon_frame(
            7,
            DaemonEvent::TaskDeleted {
                task_id: "a".to_owned(),
            },
        );
        merged.previous = Some(3);
        assert!(matches!(
            projection.accept(merged),
            FrameOutcome::Applied(_)
        ));
        assert_eq!(projection.sequence(), 7);

        // 覆盖 (8, 10]，游标 7 与区间之间缺 8：重同步且不推进。
        let mut gapped = daemon_frame(
            10,
            DaemonEvent::TaskDeleted {
                task_id: "a".to_owned(),
            },
        );
        gapped.previous = Some(8);
        assert_eq!(
            projection.accept(gapped),
            FrameOutcome::Resync(ResyncReason::Gap)
        );
        assert_eq!(projection.sequence(), 7);
    }

    #[test]
    fn queue_positions_priority_and_file_missing_map_to_dtos() {
        let mut projection = projection(1, vec![task("a", 3)]);
        let positions = projection.accept(daemon_frame(
            2,
            DaemonEvent::Engine(WsServerMsg::QueuePositionsChanged {
                positions: vec![fluxdown_protocol::QueuePositionDto {
                    task_id: "a".to_owned(),
                    position: 2,
                    order: 2,
                }],
            }),
        ));
        assert_eq!(
            positions,
            FrameOutcome::Applied(vec![HostSignalDto::Event {
                event: HostEventDto::QueuePositionsChanged {
                    order: vec![QueueOrderDto {
                        task_id: "a".to_owned(),
                        order: 2
                    }]
                }
            }])
        );
        let priority = projection.accept(daemon_frame(
            3,
            DaemonEvent::Engine(WsServerMsg::PriorityTaskChanged {
                priority_task_id: String::new(),
                auto_paused_count: 0,
            }),
        ));
        assert_eq!(
            priority,
            FrameOutcome::Applied(vec![HostSignalDto::Event {
                event: HostEventDto::PriorityTaskChanged { task_id: None }
            }])
        );
        let missing = projection.accept(daemon_frame(
            4,
            DaemonEvent::Engine(WsServerMsg::FileMissingChanged {
                updates: vec![fluxdown_protocol::FileMissingUpdateDto {
                    task_id: "a".to_owned(),
                    missing: true,
                }],
            }),
        ));
        let FrameOutcome::Applied(signals) = missing else {
            panic!("expected Applied");
        };
        assert!(matches!(
            signals.as_slice(),
            [HostSignalDto::Event {
                event: HostEventDto::FileMissingChanged { .. }
            }]
        ));
        assert!(task_dtos(&projection)[0].file_missing);
    }

    #[test]
    fn tasks_changed_applies_the_batch_and_expands_to_per_task_events() {
        let mut projection = projection(1, vec![task("a", 1)]);
        let outcome = projection.accept(daemon_frame(
            2,
            DaemonEvent::TasksChanged(vec![task("a", 3), task("b", 0)]),
        ));
        let ids: Vec<String> = events(outcome)
            .into_iter()
            .map(|event| match event {
                HostEventDto::TaskChanged { task } => task.task_id,
                other => panic!("expected TaskChanged, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, ["a", "b"]);
        let tasks = task_dtos(&projection);
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks
                .iter()
                .find(|task| task.task_id == "a")
                .map(|task| task.status),
            Some(3)
        );
        assert_eq!(projection.sequence(), 2);
    }

    #[test]
    fn tasks_deleted_removes_the_batch_and_expands_to_per_task_events_in_order() {
        let mut projection = projection(1, vec![task("a", 1), task("b", 1), task("c", 1)]);
        let outcome = projection.accept(daemon_frame(
            2,
            DaemonEvent::TasksDeleted {
                task_ids: vec!["c".to_owned(), "a".to_owned()],
            },
        ));
        let ids: Vec<String> = events(outcome)
            .into_iter()
            .map(|event| match event {
                HostEventDto::TaskDeleted { task_id } => task_id,
                other => panic!("expected TaskDeleted, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, ["c", "a"]);
        let remaining: Vec<String> = task_dtos(&projection)
            .into_iter()
            .map(|task| task.task_id)
            .collect();
        assert_eq!(remaining, ["b"]);
        assert_eq!(projection.sequence(), 2);
    }

    #[test]
    fn replaced_snapshots_are_forwarded_as_fresh_snapshot() {
        let mut projection = projection(1, vec![task("a", 1)]);
        let replacement = fluxdown_protocol::DaemonSnapshot {
            tasks: vec![task("b", 2)],
            ..Default::default()
        };
        let outcome = projection.accept(frame(2, AgentEvent::DaemonSnapshotReplaced(replacement)));
        let FrameOutcome::Applied(signals) = outcome else {
            panic!("expected Applied");
        };
        let [HostSignalDto::Snapshot { snapshot }] = signals.as_slice() else {
            panic!("expected a single Snapshot signal");
        };
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].task_id, "b");
    }

    #[test]
    fn preferences_change_emits_categories_only_when_they_change() {
        let mut projection = projection(1, Vec::new());
        let unchanged = projection.accept(frame(
            2,
            AgentEvent::PreferencesChanged(fluxdown_protocol::AgentPreferencesDto {
                revision: 2,
                values: [("theme".to_owned(), serde_json::json!("dark"))]
                    .into_iter()
                    .collect(),
            }),
        ));
        // 偏好变化但分类不变：只有 `agent.preferences` 分区变化，没有 `CategoriesChanged`。
        let events_unchanged = events(unchanged);
        assert!(matches!(
            events_unchanged.as_slice(),
            [HostEventDto::SectionChanged { name, .. }] if name == "agent.preferences"
        ));

        let custom = serde_json::json!([{ "id": "x", "name": "X", "position": 0 }]).to_string();
        let changed = projection.accept(frame(
            3,
            AgentEvent::PreferencesChanged(fluxdown_protocol::AgentPreferencesDto {
                revision: 3,
                values: [(
                    fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY.to_owned(),
                    serde_json::Value::String(custom),
                )]
                .into_iter()
                .collect(),
            }),
        ));
        let events = events(changed);
        let [
            HostEventDto::CategoriesChanged { categories },
            HostEventDto::SectionChanged { name, .. },
        ] = events.as_slice()
        else {
            panic!("expected CategoriesChanged + SectionChanged, got {events:?}");
        };
        assert_eq!(name, "agent.preferences");
        assert_eq!(categories.len(), 1);
        assert_eq!(categories[0].id, "x");
    }

    #[test]
    fn capture_tasks_started_is_forwarded_as_notice_and_advances_cursor() {
        let mut projection = projection(1, Vec::new());
        let outcome = projection.accept(frame(
            2,
            AgentEvent::CaptureTasksStarted(vec!["t1".to_owned()]),
        ));
        assert_eq!(
            only_event(outcome),
            HostEventDto::Notice {
                name: "captureTasksStarted".to_owned(),
                json: r#"["t1"]"#.to_owned(),
            }
        );
        assert_eq!(projection.sequence(), 2);
    }

    #[test]
    fn events_without_dto_section_or_notice_advance_cursor_silently() {
        let mut projection = projection(1, Vec::new());
        let outcome = projection.accept(frame(2, AgentEvent::LinkedDevicesChanged(Vec::new())));
        assert_eq!(
            only_event(outcome),
            HostEventDto::LinkedDevicesChanged {
                devices: Vec::new()
            }
        );
        let outcome = projection.accept(daemon_frame(3, DaemonEvent::Engine(WsServerMsg::Pong {})));
        assert_eq!(outcome, FrameOutcome::Applied(Vec::new()));
        assert_eq!(projection.sequence(), 3);
    }

    fn section_events(outcome: FrameOutcome) -> Vec<(String, String)> {
        events(outcome)
            .into_iter()
            .filter_map(|event| match event {
                HostEventDto::SectionChanged { name, json } => Some((name, json)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn snapshot_carries_every_section_key() {
        let projection = projection(1, Vec::new());
        let snapshot = projection.snapshot_dto();
        assert_eq!(snapshot.sections.len(), Section::ALL.len());
        for section in Section::ALL {
            assert!(
                snapshot.sections.contains_key(section.key()),
                "{}",
                section.key()
            );
        }
        assert_eq!(snapshot.sections["agent.session"], "null");
        assert_eq!(snapshot.sections["daemon.plugins"], "[]");
    }

    #[test]
    fn section_is_emitted_on_change_and_not_on_noop() {
        let mut projection = projection(1, Vec::new());
        let gateway = fluxdown_protocol::GatewayStatusDto {
            takeover_enabled: true,
            ..Default::default()
        };
        let changed = projection.accept(frame(2, AgentEvent::GatewayChanged(gateway.clone())));
        let sections = section_events(changed);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "agent.gateway");
        let decoded: serde_json::Value =
            serde_json::from_str(&sections[0].1).expect("section json");
        assert_eq!(decoded["takeoverEnabled"], true);
        assert_eq!(
            projection.snapshot_dto().sections["agent.gateway"],
            sections[0].1,
            "snapshot reflects the emitted section"
        );

        // 同值重放：序列化结果不变，不下发，游标照常推进。
        let noop = projection.accept(frame(3, AgentEvent::GatewayChanged(gateway)));
        assert_eq!(noop, FrameOutcome::Applied(Vec::new()));
        assert_eq!(projection.sequence(), 3);
    }

    #[test]
    fn daemon_sections_follow_daemon_events() {
        let mut projection = projection(1, Vec::new());
        let revision = projection.accept(daemon_frame(
            2,
            DaemonEvent::RssChanged {
                source_id: "s1".to_owned(),
                item_revision: 4,
            },
        ));
        assert_eq!(
            section_events(revision),
            vec![(
                "daemon.rssItemRevisions".to_owned(),
                r#"{"s1":4}"#.to_owned()
            )]
        );
        let cleared = projection.accept(daemon_frame(3, DaemonEvent::WebhooksCleared));
        assert_eq!(cleared, FrameOutcome::Applied(Vec::new()));
        let plugins = projection.accept(daemon_frame(4, DaemonEvent::PluginsChanged(Vec::new())));
        assert_eq!(plugins, FrameOutcome::Applied(Vec::new()));
    }

    #[test]
    fn cloud_notify_state_is_a_section_and_task_notice_is_ignored() {
        let mut projection = projection(1, Vec::new());
        let state = fluxdown_protocol::CloudNotifyStateDto {
            reporting: true,
            ..Default::default()
        };
        let changed = projection.accept(frame(2, AgentEvent::CloudNotifyChanged(state.clone())));
        let sections = section_events(changed);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "agent.cloudNotify");
        let decoded: serde_json::Value =
            serde_json::from_str(&sections[0].1).expect("section json");
        assert_eq!(decoded["reporting"], true);
        assert_eq!(
            projection.snapshot_dto().sections["agent.cloudNotify"],
            sections[0].1
        );
        // 同值重放不下发。
        let noop = projection.accept(frame(3, AgentEvent::CloudNotifyChanged(state)));
        assert_eq!(noop, FrameOutcome::Applied(Vec::new()));

        let notice = projection.accept(daemon_frame(
            4,
            DaemonEvent::TaskNotice(fluxdown_protocol::TaskNoticeDto {
                delivery_id: "d".to_owned(),
                event: "task.completed".to_owned(),
                timestamp_ms: 1,
                queue_id: String::new(),
                queue_name: String::new(),
                task: None,
            }),
        ));
        assert_eq!(notice, FrameOutcome::Applied(Vec::new()));
    }

    #[test]
    fn logout_clears_account_sections_in_one_frame() {
        let mut projection = projection(1, Vec::new());
        let session: fluxdown_protocol::AgentSessionDto =
            serde_json::from_value(serde_json::json!({
                "user": { "id": "u1", "email": "a@example.com" },
                "currentPlan": null,
                "device": { "id": "d1", "deviceId": "dev-1" }
            }))
            .expect("session fixture");
        let login = projection.accept(frame(
            2,
            AgentEvent::SessionChanged(Box::new(Some(session))),
        ));
        let names: Vec<String> = section_events(login).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["agent.session"]);

        let logout = projection.accept(frame(3, AgentEvent::SessionChanged(Box::new(None))));
        let sections = section_events(logout);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0], ("agent.session".to_owned(), "null".to_owned()));
    }

    #[test]
    fn engine_notifications_are_forwarded_with_variant_name_and_flat_json() {
        let mut projection = projection(1, Vec::new());
        let duplicate = projection.accept(daemon_frame(
            2,
            DaemonEvent::Engine(WsServerMsg::DuplicateTorrent {
                task_id: "n".to_owned(),
                existing_task_id: "e".to_owned(),
                existing_name: "Ubuntu".to_owned(),
            }),
        ));
        let HostEventDto::Notice { name, json } = only_event(duplicate) else {
            panic!("expected a Notice");
        };
        assert_eq!(name, "duplicateTorrent");
        let decoded: serde_json::Value = serde_json::from_str(&json).expect("notice json");
        assert_eq!(decoded["existingTaskId"], "e");
        assert_eq!(decoded["existingName"], "Ubuntu");

        let revoked = projection.accept(frame(
            3,
            AgentEvent::SessionRevoked(fluxdown_protocol::ErrorReason::SessionExpired),
        ));
        assert_eq!(
            only_event(revoked),
            HostEventDto::Notice {
                name: "sessionRevoked".to_owned(),
                json: r#""sessionExpired""#.to_owned(),
            }
        );
    }

    #[test]
    fn plugin_auto_disable_notice_is_accompanied_by_plugins_section_change() {
        let mut snapshot = snapshot(1, Vec::new());
        if let fluxdown_protocol::SnapshotBody::Agent(agent) = &mut snapshot.body {
            agent.daemon.plugins = vec![
                serde_json::from_value(serde_json::json!({
                    "identity": "p1", "name": "P1", "version": "1.0.0", "enabled": true,
                    "devMode": false, "disabledReason": "None", "settings": [],
                    "settingsValues": {}
                }))
                .expect("plugin fixture"),
            ];
        }
        let mut projection = Projection::from_snapshot(info(), snapshot).expect("agent snapshot");
        let outcome = projection.accept(daemon_frame(
            2,
            DaemonEvent::Engine(WsServerMsg::PluginAutoDisabled {
                identity: "p1".to_owned(),
                reason: "CircuitBreaker".to_owned(),
            }),
        ));
        let events = events(outcome);
        assert!(matches!(
            events.as_slice(),
            [
                HostEventDto::SectionChanged { name, .. },
                HostEventDto::Notice { name: notice, .. }
            ] if name == "daemon.plugins" && notice == "pluginAutoDisabled"
        ));
    }

    #[test]
    fn snapshot_replacement_resets_section_baseline() {
        let mut projection = projection(1, Vec::new());
        projection.accept(frame(
            2,
            AgentEvent::GatewayChanged(fluxdown_protocol::GatewayStatusDto {
                takeover_enabled: true,
                ..Default::default()
            }),
        ));
        // 整表替换后基线与新快照一致：同值事件不再下发。
        let replaced = projection.accept(daemon_frame(
            3,
            DaemonEvent::SnapshotReplaced(fluxdown_protocol::DaemonSnapshot::default()),
        ));
        let FrameOutcome::Applied(signals) = replaced else {
            panic!("expected Applied");
        };
        let [HostSignalDto::Snapshot { snapshot }] = signals.as_slice() else {
            panic!("expected only a Snapshot");
        };
        assert_eq!(snapshot.sections.len(), Section::ALL.len());
        let same = projection.accept(frame(
            4,
            AgentEvent::GatewayChanged(fluxdown_protocol::GatewayStatusDto {
                takeover_enabled: true,
                ..Default::default()
            }),
        ));
        assert_eq!(same, FrameOutcome::Applied(Vec::new()));
    }
}
