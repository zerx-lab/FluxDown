//! 单一 agent 会话实体：所有快照 / 事件的唯一入口，向任意数量窗口的任意能力视图广播。
//!
//! 视图通过 [attach] 订阅：从持续折叠的同源快照秒开首帧，后续沿单会话
//! 的严格事件游标更新；订阅随视图销毁自动解除。
//!
//! 连接态对视图有宽限：启动时首个快照、断线后的重连快照只要在 [`OFFLINE_NOTICE_GRACE`]
//! 内到达，视图就不会进入「正在连接」只读态（期间发出的命令由客户端排队，重连后送达）。
//! 本进程冷启动了后台服务时，首个快照的宽限放宽到 [`COLD_START_GRACE`]。超过宽限仍未连上才
//! 广播 [`SessionSignal::Stale`]。

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, DaemonEvent, EventFrame, FrameVerdict, RpcErrorData, ServiceEvent,
    Snapshot, SnapshotBody, accepted_runtime_status, apply_agent_event, judge_frame,
};
use gpui::{App, Context, Entity, EventEmitter};

use crate::agent_client::{AgentClient, AgentClientEvent, TablesLoaded};
use crate::service_failure::ServiceFailure;

/// 断线（含启动时尚未连上）多久仍未恢复才告知视图。本机回环重连与热启动首个快照都远低于此值。
const OFFLINE_NOTICE_GRACE: Duration = Duration::from_millis(800);
/// 本进程拉起了 agent 时首个快照的宽限：拉起 agent 并完成装配通常在 1s 内，但落在
/// [`OFFLINE_NOTICE_GRACE`] 附近；界面在此期间不开窗 / 不显示连接态，而不是先闪「正在连接」。
/// 到期仍未连上才按离线呈现。
const COLD_START_GRACE: Duration = Duration::from_secs(5);

/// 会话向订阅者广播的信号。
pub enum SessionSignal {
    /// 连接 / 重连后的全量快照。
    Snapshot(Arc<Snapshot>),
    /// 单个事件帧。
    Event(Arc<EventFrame>),
    /// 连接断开超过宽限仍未恢复，视图进入只读的「正在连接」态。
    Stale,
    /// 不可恢复错误（协议不兼容 / 未授权）。
    Fatal(RpcErrorData),
    /// agent 已完全退出（`system.shutdown`）：界面应随之退出。
    ServiceStopped,
    /// agent 无法被拉起或反复启动即退出：界面应展示原因；视图随之进入只读态。
    ServiceFailed(Arc<ServiceFailure>),
}

/// agent 会话状态：最近一次全量快照与连接健康度。
pub struct AgentSession {
    _client: Arc<AgentClient>,
    latest: Option<Arc<Snapshot>>,
    /// 连接当前不可用（事件帧不再并入快照）。
    stale: bool,
    /// 已向视图广播离线（宽限已过或不可恢复）。
    offline_notified: bool,
    /// 宽限计时器代际：新快照到达即作废在途计时。
    offline_generation: u64,
    /// 冷启动宽限已启用过：agent 反复拉起失败时不无限顺延离线告知。
    cold_start_grace_armed: bool,
}

impl EventEmitter<SessionSignal> for AgentSession {}

impl AgentSession {
    /// 创建会话并开始启动宽限计时：首个快照在宽限内到达则视图全程不见连接态。
    pub fn new(client: Arc<AgentClient>, cx: &mut Context<Self>) -> Self {
        let mut session = Self {
            _client: client,
            latest: None,
            stale: true,
            offline_notified: false,
            offline_generation: 0,
            cold_start_grace_armed: false,
        };
        session.schedule_offline_notice(OFFLINE_NOTICE_GRACE, cx);
        session
    }

    /// 最近一次同源完整投影（已折叠有序事件）。
    #[must_use]
    pub fn latest(&self) -> Option<&Arc<Snapshot>> {
        self.latest.as_ref()
    }

    /// 最近快照的 agent 主体。
    #[must_use]
    pub fn agent_snapshot(&self) -> Option<&AgentSnapshot> {
        self.latest.as_deref().and_then(agent_body)
    }

    /// 任务表 / 队列位置表仍在分页载入：投影里的 `tasks` 只是占位，不是完整列表，调用方
    /// 不得据它推断「任务不存在 / 已删除 / 全部」。
    #[must_use]
    pub fn tables_loading(&self) -> bool {
        self.agent_snapshot()
            .is_some_and(|body| body.daemon.tables_loading())
    }

    /// 已可呈现界面：拿到快照，或已确认离线（宽限已过 / 不可恢复），不必再等。
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.latest.is_some() || self.offline_notified
    }

    /// 把一批客户端事件逐个转为 [`SessionSignal`] 广播。
    pub fn ingest(&mut self, events: Vec<AgentClientEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                AgentClientEvent::Snapshot(snapshot) => {
                    self._client
                        .submissions
                        .set_ready(agent_body(&snapshot).is_some_and(|body| body.daemon_connected));
                    let snapshot = Arc::new(*snapshot);
                    self.latest = Some(Arc::clone(&snapshot));
                    self.stale = false;
                    self.offline_notified = false;
                    self.offline_generation = self.offline_generation.wrapping_add(1);
                    cx.emit(SessionSignal::Snapshot(snapshot));
                }
                AgentClientEvent::Event(mut frame) => {
                    match self
                        .latest
                        .as_mut()
                        .map(|snapshot| apply_frame(Arc::make_mut(snapshot), &mut frame))
                    {
                        Some(Ok(true)) if !self.stale => {
                            match &frame.event {
                                ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(ready)) => {
                                    self._client.submissions.set_ready(*ready);
                                }
                                ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(_)) => {
                                    self._client.submissions.set_ready(true);
                                }
                                _ => {}
                            }
                            cx.emit(SessionSignal::Event(Arc::new(*frame)));
                        }
                        Some(Ok(_)) => {}
                        _ => self.go_stale(cx),
                    }
                }
                AgentClientEvent::Stale => self.go_stale(cx),
                AgentClientEvent::TablesLoaded(loaded) => {
                    let installed = self
                        .latest
                        .as_mut()
                        .is_some_and(|snapshot| install_tables(Arc::make_mut(snapshot), *loaded));
                    match self.latest.as_ref() {
                        Some(snapshot) if installed => {
                            if !self.stale {
                                cx.emit(SessionSignal::Snapshot(Arc::clone(snapshot)));
                            }
                        }
                        // 载入的表与会话投影对不上（epoch / 游标不符）：投影不可信，等重连。
                        _ => self.go_stale(cx),
                    }
                }
                // 冷启动的首连：尚未拿到过快照、也未宣告离线时放宽一次宽限（重连期间的重新拉起
                // 不放宽；反复拉起也只放宽一次）。
                AgentClientEvent::ServiceStarting => {
                    if self.latest.is_none()
                        && !self.offline_notified
                        && !self.cold_start_grace_armed
                    {
                        self.cold_start_grace_armed = true;
                        self.schedule_offline_notice(COLD_START_GRACE, cx);
                    }
                }
                AgentClientEvent::Fatal(error) => {
                    self.notify_offline_now();
                    cx.emit(SessionSignal::Fatal(error));
                }
                AgentClientEvent::ServiceStopped => {
                    self.notify_offline_now();
                    cx.emit(SessionSignal::ServiceStopped);
                }
                AgentClientEvent::ServiceFailed(failure) => {
                    self.notify_offline_now();
                    cx.emit(SessionSignal::ServiceFailed(Arc::new(failure)));
                }
            }
        }
        cx.notify();
    }

    /// 连接中断：立即停止并帧，但只在宽限内仍未恢复时才告知视图。
    fn go_stale(&mut self, cx: &mut Context<Self>) {
        self._client.submissions.set_ready(false);
        if self.stale {
            return;
        }
        self.stale = true;
        self.schedule_offline_notice(OFFLINE_NOTICE_GRACE, cx);
    }

    /// 重新计时离线告知：作废在途计时，`grace` 后仍未恢复才广播 [`SessionSignal::Stale`]。
    fn schedule_offline_notice(&mut self, grace: Duration, cx: &mut Context<Self>) {
        self.offline_generation = self.offline_generation.wrapping_add(1);
        let generation = self.offline_generation;
        cx.spawn(async move |session, cx| {
            cx.background_executor().timer(grace).await;
            if let Err(error) = session.update(cx, |session, cx| {
                if session.stale
                    && !session.offline_notified
                    && session.offline_generation == generation
                {
                    session.notify_offline_now();
                    cx.emit(SessionSignal::Stale);
                    cx.notify();
                }
            }) {
                log::debug!("view or window released before lifecycle update: {error:#}");
            }
        })
        .detach();
    }

    /// 视图即将进入离线态：快照中的运行态作废（晚打开的视图也不显示过期速度）。
    fn notify_offline_now(&mut self) {
        self._client.submissions.set_ready(false);
        self.stale = true;
        self.offline_notified = true;
        self.offline_generation = self.offline_generation.wrapping_add(1);
        if let Some(snapshot) = self.latest.as_mut()
            && let SnapshotBody::Agent(body) = &mut Arc::make_mut(snapshot).body
        {
            body.daemon_connected = false;
            body.daemon.task_runtime.clear();
        }
    }
}

/// 快照 / 事件消费者：每个能力视图实现一次，由 [`attach`] 驱动。
pub trait SessionConsumer: Sized + 'static {
    fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>);
    fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>);
    fn mark_stale(&mut self, cx: &mut Context<Self>);
}

/// 快照主体（agent 角色）。
#[must_use]
pub fn agent_body(snapshot: &Snapshot) -> Option<&AgentSnapshot> {
    match &snapshot.body {
        SnapshotBody::Agent(body) => Some(body),
        SnapshotBody::Daemon(_) => None,
    }
}

/// 仅订阅唯一 AgentSession；同线程同步首帧，没有额外窗口连接或异步快照回放。
pub fn attach<V: SessionConsumer>(session: &Entity<AgentSession>, view: &Entity<V>, cx: &mut App) {
    view.update(cx, |_, cx| {
        cx.subscribe(session, move |view, _, signal, cx| match signal {
            SessionSignal::Event(frame) => view.apply_event(&frame.event, cx),
            SessionSignal::Snapshot(snapshot) => {
                if let Some(body) = agent_body(snapshot) {
                    view.replace_snapshot(body, cx);
                }
            }
            SessionSignal::Stale
            | SessionSignal::Fatal(_)
            | SessionSignal::ServiceStopped
            | SessionSignal::ServiceFailed(_) => {
                view.mark_stale(cx);
            }
        })
        .detach();
    });
    let (latest, offline) = {
        let session = session.read(cx);
        (session.latest.clone(), session.offline_notified)
    };
    if let Some(body) = latest.as_deref().and_then(agent_body) {
        view.update(cx, |view, cx| view.replace_snapshot(body, cx));
    }
    if offline {
        view.update(cx, |view, cx| view.mark_stale(cx));
    }
}

/// 把帧并入同一 epoch 的快照：旧帧（含快照已覆盖的）不可倒灌，缺口不可静默跳过；合并帧的
/// 覆盖区间可以越过快照游标，按 [`judge_frame`] 判定。
fn apply_frame(snapshot: &mut Snapshot, frame: &mut EventFrame) -> Result<bool, ()> {
    match judge_frame(&snapshot.epoch, snapshot.sequence, frame) {
        FrameVerdict::Skip => return Ok(false),
        FrameVerdict::Resync => return Err(()),
        FrameVerdict::Accept => {}
    }
    let (SnapshotBody::Agent(body), ServiceEvent::Agent(event)) =
        (&mut snapshot.body, &frame.event)
    else {
        return Err(());
    };
    let deliver = match event {
        AgentEvent::Daemon(DaemonEvent::TaskRuntimeChanged(runtime)) => {
            accepted_runtime_status(&body.daemon, runtime).is_some()
        }
        _ => true,
    };
    apply_agent_event(body, event);
    if deliver
        && let ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TaskRuntimeChanged(runtime))) =
            &mut frame.event
        && let Some(projected) = body.daemon.task_runtime.get(&runtime.task_id)
    {
        runtime.clone_from(projected);
    }
    snapshot.sequence = frame.sequence;
    Ok(deliver)
}

/// 把分页载入完成的表装入会话投影（与 [`AgentClientEvent::TablesLoaded`] 配对）。载入期间不
/// 应用任何帧，所以投影游标必须仍是基准帧；对不上返回 `false`。
fn install_tables(snapshot: &mut Snapshot, loaded: TablesLoaded) -> bool {
    if snapshot.epoch != loaded.epoch || snapshot.sequence != loaded.sequence {
        return false;
    }
    let SnapshotBody::Agent(body) = &mut snapshot.body else {
        return false;
    };
    if let Some(tasks) = loaded.tasks {
        body.daemon.install_tasks(tasks);
    }
    if let Some(queue) = loaded.queue {
        body.daemon.install_queue(queue);
    }
    true
}

macro_rules! forward_consumer {
    ($($view:ty),* $(,)?) => {
        $(
            impl SessionConsumer for $view {
                fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
                    <$view>::replace_snapshot(self, snapshot, cx);
                }
                fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
                    <$view>::apply_event(self, event, cx);
                }
                fn mark_stale(&mut self, cx: &mut Context<Self>) {
                    <$view>::mark_stale(self, cx);
                }
            }
        )*
    };
}

forward_consumer!(
    fluxdown_ui_downloads::DownloadView,
    fluxdown_ui_downloads::QueueManagerView,
    fluxdown_ui_downloads::TaskDetailView,
    fluxdown_ui_downloads::GroupDetailView,
    fluxdown_ui_downloads::ProgressWindowView,
    fluxdown_ui_downloads::FileConflictView,
    fluxdown_ui_settings::SettingsStore,
    fluxdown_ui_account::AccountHost,
    fluxdown_ui_rss::RssView,
    fluxdown_ui_extensions::ExtensionsView,
);

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        AgentEvent, DaemonEvent, EventFrame, ServiceEvent, Snapshot, SnapshotBody, TaskRuntimeDto,
    };

    use super::{apply_frame, install_tables};
    use crate::agent_client::TablesLoaded;

    fn runtime_frame(epoch: &str, sequence: u64, active: u32) -> EventFrame {
        EventFrame {
            epoch: epoch.to_owned(),
            sequence,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TaskRuntimeChanged(
                TaskRuntimeDto {
                    task_id: "task".into(),
                    sample_sequence: sequence,
                    active_transfers: Some(active),
                    ..Default::default()
                },
            ))),
        }
    }

    fn source_snapshot(epoch: &str, sequence: u64) -> Snapshot {
        let task = serde_json::from_value(serde_json::json!({
            "taskId":"task", "url":"https://example.com/x", "fileName":"x", "saveDir":"/tmp",
            "status":1, "downloadedBytes":0, "totalBytes":100, "errorMessage":"",
            "createdAt":"1", "proxyUrl":"", "queueId":"main", "checksum":""
        }))
        .expect("task fixture");
        let mut agent = fluxdown_protocol::AgentSnapshot::default();
        agent.daemon.tasks.push(task);
        Snapshot {
            epoch: epoch.into(),
            sequence,
            body: SnapshotBody::Agent(Box::new(agent)),
        }
    }

    #[test]
    fn late_view_sees_live_runtime_and_old_or_gapped_frames_never_rewind_it() {
        let mut snapshot = source_snapshot("a", 5);
        assert_eq!(
            apply_frame(&mut snapshot, &mut runtime_frame("a", 6, 3)),
            Ok(true)
        );
        assert_eq!(
            apply_frame(&mut snapshot, &mut runtime_frame("a", 6, 0)),
            Ok(false)
        );
        assert_eq!(
            apply_frame(&mut snapshot, &mut runtime_frame("b", 7, 0)),
            Err(())
        );
        assert_eq!(
            apply_frame(&mut snapshot, &mut runtime_frame("a", 8, 0)),
            Err(())
        );
        let SnapshotBody::Agent(ref body) = snapshot.body else {
            panic!("agent snapshot")
        };
        assert_eq!(body.daemon.task_runtime["task"].active_transfers, Some(3));
        assert_eq!(snapshot.sequence, 6);
        let mut older = runtime_frame("a", 7, 9);
        if let ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TaskRuntimeChanged(runtime))) =
            &mut older.event
        {
            runtime.sample_sequence = 5;
        }
        assert_eq!(apply_frame(&mut snapshot, &mut older), Ok(false));
        assert_eq!(snapshot.sequence, 7);
        let SnapshotBody::Agent(ref body) = snapshot.body else {
            panic!("agent snapshot")
        };
        assert_eq!(body.daemon.task_runtime["task"].active_transfers, Some(3));
        let mut complete = body.daemon.tasks[0].clone();
        complete.status = 4;
        let mut terminal = EventFrame {
            epoch: "a".into(),
            sequence: 8,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TaskChanged(complete))),
        };
        assert_eq!(apply_frame(&mut snapshot, &mut terminal), Ok(true));
        let mut late = runtime_frame("a", 9, 4);
        assert_eq!(apply_frame(&mut snapshot, &mut late), Ok(true));
        let ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TaskRuntimeChanged(runtime))) =
            late.event
        else {
            panic!("runtime event")
        };
        assert_eq!(runtime.active_transfers, Some(0));
        // 重连快照作为新 epoch 的唯一权威基线。
        let mut recovered = source_snapshot("b", 20);
        assert_eq!(
            apply_frame(&mut recovered, &mut runtime_frame("b", 21, 4)),
            Ok(true)
        );
        assert_eq!(
            apply_frame(&mut recovered, &mut runtime_frame("a", 7, 0)),
            Err(())
        );
        let SnapshotBody::Agent(body) = recovered.body else {
            panic!("agent snapshot")
        };
        assert_eq!(body.daemon.task_runtime["task"].active_transfers, Some(4));
    }

    #[test]
    fn tasks_changed_batch_applies_like_task_changed_for_later_runtime_samples() {
        let mut snapshot = source_snapshot("a", 5);
        assert_eq!(
            apply_frame(&mut snapshot, &mut runtime_frame("a", 6, 3)),
            Ok(true)
        );
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        let mut complete = body.daemon.tasks[0].clone();
        complete.status = 2;
        let mut added = complete.clone();
        added.task_id = "added".into();
        let mut batch = EventFrame {
            epoch: "a".into(),
            sequence: 7,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TasksChanged(vec![
                complete, added,
            ]))),
        };
        assert_eq!(apply_frame(&mut snapshot, &mut batch), Ok(true));
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        assert_eq!(body.daemon.tasks.len(), 2);
        let mut late = runtime_frame("a", 8, 4);
        assert_eq!(apply_frame(&mut snapshot, &mut late), Ok(true));
        let ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TaskRuntimeChanged(runtime))) =
            late.event
        else {
            panic!("runtime event")
        };
        assert_eq!(runtime.active_transfers, Some(0));
    }

    #[test]
    fn completed_task_drops_runtime_and_late_frames_are_not_delivered() {
        let mut snapshot = source_snapshot("a", 5);
        assert_eq!(
            apply_frame(&mut snapshot, &mut runtime_frame("a", 6, 3)),
            Ok(true)
        );
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        let mut complete = body.daemon.tasks[0].clone();
        complete.status = 3;
        let mut terminal = EventFrame {
            epoch: "a".into(),
            sequence: 7,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TaskChanged(complete))),
        };
        assert_eq!(apply_frame(&mut snapshot, &mut terminal), Ok(true));
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        assert!(body.daemon.task_runtime.is_empty());
        assert_eq!(
            apply_frame(&mut snapshot, &mut runtime_frame("a", 8, 4)),
            Ok(false)
        );
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        assert!(body.daemon.task_runtime.is_empty());
    }

    #[test]
    fn coalesced_frame_overlapping_the_cursor_applies_but_a_gap_still_resyncs() {
        let mut snapshot = source_snapshot("a", 5);
        // 覆盖 (3, 8]，快照已含 4..=5：区间越过游标仍接受，游标推进到 8。
        let mut merged = runtime_frame("a", 8, 3);
        merged.previous = Some(3);
        assert_eq!(apply_frame(&mut snapshot, &mut merged), Ok(true));
        assert_eq!(snapshot.sequence, 8);
        // 覆盖 (9, 12]：与游标 8 之间缺 9。
        let mut gapped = runtime_frame("a", 12, 1);
        gapped.previous = Some(9);
        assert_eq!(apply_frame(&mut snapshot, &mut gapped), Err(()));
        assert_eq!(snapshot.sequence, 8);
    }

    fn task_fixture(id: &str) -> fluxdown_protocol::TaskDto {
        serde_json::from_value(serde_json::json!({
            "taskId": id, "url": "https://example.com/x", "fileName": "x", "saveDir": "/tmp",
            "status": 1, "downloadedBytes": 0, "totalBytes": 100, "createdAt": "1"
        }))
        .expect("task fixture")
    }

    fn daemon_event_frame(sequence: u64, event: DaemonEvent) -> EventFrame {
        EventFrame {
            epoch: "a".to_owned(),
            sequence,
            previous: None,
            event: ServiceEvent::Agent(AgentEvent::Daemon(event)),
        }
    }

    /// 重载宣告让投影进入加载态（空表不被当成完整表）；表装入前游标对不上的载入被拒绝，
    /// 装入后加载态解除、其后的帧照常折叠。
    #[test]
    fn reload_announcement_keeps_projection_loading_until_matching_tables_install() {
        let mut snapshot = source_snapshot("a", 5);
        let mut reloading = daemon_event_frame(
            6,
            DaemonEvent::TablesReloading {
                tasks: Some(3),
                queue: None,
            },
        );
        assert_eq!(apply_frame(&mut snapshot, &mut reloading), Ok(true));
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        assert!(body.daemon.tables_loading());
        assert!(body.daemon.tasks.is_empty());
        assert_eq!(body.daemon.task_count(), 3);

        let wrong_cursor = TablesLoaded {
            epoch: "a".to_owned(),
            sequence: 5,
            tasks: Some(vec![task_fixture("x")]),
            queue: None,
        };
        assert!(!install_tables(&mut snapshot, wrong_cursor));
        let wrong_epoch = TablesLoaded {
            epoch: "b".to_owned(),
            sequence: 6,
            tasks: Some(vec![task_fixture("x")]),
            queue: None,
        };
        assert!(!install_tables(&mut snapshot, wrong_epoch));
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        assert!(
            body.daemon.tables_loading(),
            "rejected tables must not clear the loading state"
        );

        let loaded = TablesLoaded {
            epoch: "a".to_owned(),
            sequence: 6,
            tasks: Some(vec![
                task_fixture("a"),
                task_fixture("b"),
                task_fixture("c"),
            ]),
            queue: None,
        };
        assert!(install_tables(&mut snapshot, loaded));
        let mut later = daemon_event_frame(
            7,
            DaemonEvent::TaskDeleted {
                task_id: "b".to_owned(),
            },
        );
        assert_eq!(apply_frame(&mut snapshot, &mut later), Ok(true));
        let SnapshotBody::Agent(body) = &snapshot.body else {
            panic!("agent snapshot")
        };
        assert!(!body.daemon.tables_loading());
        let mut ids: Vec<_> = body
            .daemon
            .tasks
            .iter()
            .map(|task| task.task_id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["a", "c"]);
    }
}
