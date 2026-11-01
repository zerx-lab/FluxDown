//! 跨设备任务全量投影、本地执行路由、绑定评分与缺失宽限。
//!
//! 角色：
//! - 发起端：`dispatch` 经 FluxCloud 下发任务，`command` 控制远端任务；
//! - 执行端：SSE 收到 `task.dispatch` 后逐条接单建 daemon 任务，并上报状态 / 进度；
//!   收到 `task.command` 后映射到本机 daemon。
//!
//! 执行端的「云端任务 → 本机 daemon 任务」绑定持久化在 `AgentState::remote_bindings`，
//! 重启后不会为同一个云端任务重复建任务。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fluxdown_protocol::{
    AgentEvent, CloudConnectionDto, CloudConnectionState, CloudDevice, CreateTaskRequest,
    DaemonCreateTaskParams, DaemonEvent, ErrorReason, RemoteCommandAction, RemoteCommandParams,
    RemoteDispatchParams, RemoteDispatchResult, RemoteTaskDto, RemoteTaskStatus, RpcErrorData,
    ServiceEvent, WsServerMsg,
};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tokio_tungstenite::tungstenite::Message;

use crate::cloud::{CloudApi, CloudError, CloudSocket};
use crate::daemon_client::DaemonClient;
use crate::event_hub::AgentEventHub;
use crate::link::{resolve_receive_dir, valid_file_name};
use crate::state::{AgentState, StateStore};

const MISSING_GRACE_ROUNDS: u8 = 3;
const SSE_STABLE: Duration = Duration::from_secs(60);
const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(75);
const SSE_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// WebSocket 握手被判定「不支持」后，直接走 SSE 的粘性时长。
const WS_FALLBACK_STICKY: Duration = Duration::from_secs(60 * 60);
/// 连续这么多次「握手成功但通道不可用」后进入粘性回退。
const WS_UNUSABLE_THRESHOLD: usize = 3;
/// 客户端 → 服务端心跳帧（线协议 `{"type":"ping"}`）。
const SOCKET_PING_FRAME: &str = r#"{"type":"ping"}"#;
/// 服务端 `error` 帧里 message 的展示上限。
const MAX_SOCKET_ERROR_CHARS: usize = 200;
/// 周期性重试接单：daemon 暂时不可用时任务保持 pending，就绪后自动接上。
const ACCEPT_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
];
/// `resync` 后立即重连前的最短停顿，避免云端持续 lag 时空转。
const RESYNC_PAUSE: Duration = Duration::from_secs(1);
/// 已处理命令 id 的去重窗口（FIFO）。
const CONFIRMED_COMMAND_CAPACITY: usize = 512;
/// SSE 单行上限；超过说明流已损坏。
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
// Preserve the existing per-event wire limit, but cap queued payloads at 16 MiB.
// Backpressure retains non-replayable commands instead of dropping them for a resync.
const SSE_QUEUE_CAPACITY: usize = 16;
/// 失败上报里的错误文本上限。
const MAX_REPORTED_ERROR_CHARS: usize = 512;
/// 在线设备名额已满（或本机被另一台设备踢下线）后的固定重试间隔；不走指数退避，
/// 用户手动让别的设备下线后可由 `request_reconnect` 立即重试。
const ONLINE_LIMIT_RETRY: Duration = Duration::from_secs(30);

/// 一次 SSE 会话结束的原因。
#[derive(Debug, Eq, PartialEq)]
enum SseEnd {
    Cancelled,
    // Internal business-loop signal: its sender closed and all accepted events were applied.
    Drained,
    /// 云端广播滞后：需要重新全量拉取后重连。
    Resync,
    /// 登录会话已结束（登出 / 撤销）。
    SessionEnded,
    /// 本机被同账号的另一台设备手动下线（`presence.kicked`）：断开 SSE，进入在线名额受限的等待。
    Kicked,
}

/// 一条已建立的任务实时通道：优先 WebSocket（心跳在连接内），回退 SSE + HTTP 心跳。
enum EventChannel {
    Socket(Box<CloudSocket>),
    Stream(reqwest::Response),
}

type TransportFuture<'a> =
    std::pin::Pin<Box<dyn Future<Output = Result<SseEnd, RemoteError>> + Send + 'a>>;

/// 有界 FIFO 去重集合。
#[derive(Default)]
struct BoundedIds {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl BoundedIds {
    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    fn insert(&mut self, id: String) {
        if !self.set.insert(id.clone()) {
            return;
        }
        self.order.push_back(id);
        while self.order.len() > CONFIRMED_COMMAND_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }
}

/// 只属于当前登录会话的运行期状态；登出时整体丢弃。
#[derive(Default)]
struct Runtime {
    missing_rounds: HashMap<String, u8>,
    local_missing_rounds: HashMap<String, u8>,
    reported_statuses: HashMap<String, RemoteTaskStatus>,
    confirmed_commands: BoundedIds,
    /// daemon 任务的实时速度（快照不携带，取自 `TaskProgress` 事件）。
    local_speeds: HashMap<String, i64>,
    /// 本进程内建立绑定的时刻。快照请求发出之后才建立的绑定不能凭「快照里没有」判定记录已删除
    /// （任务可能在该快照之后才下发）；没有条目 = 启动前持久化的旧绑定。
    bound_at: HashMap<String, Instant>,
}

pub struct RemoteTaskService {
    cloud: CloudApi,
    daemon: Arc<DaemonClient>,
    events: AgentEventHub,
    state: Arc<Mutex<AgentState>>,
    store: Arc<StateStore>,
    sse_idle_timeout: Duration,
    sse_connect_timeout: Duration,
    heartbeat_interval: Duration,
    reconnect: Notify,
    /// WebSocket 握手被判定「不支持」后的粘性回退截止时刻；锁中毒时沿用内部值（只是个时间戳）。
    ws_fallback_until: std::sync::Mutex<Option<Instant>>,
    /// 连续「握手可达但通道不可用」的次数；收到 pong 清零，达到阈值进入粘性回退。
    ws_unusable_streak: AtomicUsize,
    /// 当前 WebSocket 会话是否已收到过 pong。
    ws_pong_seen: AtomicBool,
    /// 正处于 WebSocket 握手阶段（整体握手超时据此判断超时落在哪一段）。
    ws_handshaking: AtomicBool,
    /// 云端 SSE 里推送相关的事件（`notify.changed` / `notify.delivery` / 重连 / `resync`）转交云端推送模块。
    notify_feed: Option<crate::cloud_notify::NotifyFeed>,
    runtime: Mutex<Runtime>,
}

impl RemoteTaskService {
    #[must_use]
    pub fn new(
        cloud: CloudApi,
        daemon: Arc<DaemonClient>,
        events: AgentEventHub,
        state: Arc<Mutex<AgentState>>,
        store: Arc<StateStore>,
    ) -> Self {
        Self {
            cloud,
            daemon,
            events,
            state,
            store,
            sse_idle_timeout: SSE_IDLE_TIMEOUT,
            sse_connect_timeout: SSE_CONNECT_TIMEOUT,
            heartbeat_interval: HEARTBEAT_INTERVAL,
            reconnect: Notify::new(),
            ws_fallback_until: std::sync::Mutex::new(None),
            ws_unusable_streak: AtomicUsize::new(0),
            ws_pong_seen: AtomicBool::new(false),
            ws_handshaking: AtomicBool::new(false),
            notify_feed: None,
            runtime: Mutex::new(Runtime::default()),
        }
    }

    /// 接入云端推送：收到 `notify.changed` 时唤醒其概览刷新。
    #[must_use]
    pub fn with_notify_feed(mut self, feed: crate::cloud_notify::NotifyFeed) -> Self {
        self.notify_feed = Some(feed);
        self
    }

    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let mut retry_attempt = 0_usize;
        // 上一轮因在线名额受限而等待：下一轮保留 `Reconnecting + OnlineDeviceLimit` 展示，不闪回空原因。
        let mut limit_shown = false;
        let (mut session_events, _) = self.events.subscribe_and_snapshot();
        let mut session_changes = self.cloud.session_changes();
        loop {
            session_changes.borrow_and_update();
            let keep_limit_state = std::mem::take(&mut limit_shown);
            if cancel.is_cancelled() {
                break;
            }
            let epoch = self.cloud.request_epoch();
            let Ok(state) = self.cloud.lock_epoch(epoch).await else {
                retry_attempt = 0;
                continue;
            };
            let authenticated = state.credentials.as_ref().is_some_and(|credentials| {
                !credentials.access_token.is_empty() && !credentials.refresh_token.is_empty()
            });
            let uid = state
                .credentials
                .as_ref()
                .and_then(|credentials| credentials.session.as_ref())
                .map(|session| session.user.id.clone());
            let device_id = state.device_id.clone();
            if !authenticated {
                self.connection(CloudConnectionState::Disconnected, None);
                *self.runtime.lock().await = Runtime::default();
                drop(state);
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                    _ = wait_for_session(&mut session_events) => {},
                    changed = session_changes.changed() => {
                        if changed.is_err() { break; }
                    },
                    _ = self.reconnect.notified() => {},
                }
                continue;
            }
            if !keep_limit_state {
                self.connection(
                    if retry_attempt == 0 {
                        CloudConnectionState::Connecting
                    } else {
                        CloudConnectionState::Reconnecting
                    },
                    None,
                );
            }
            drop(state);
            let connected_at = Instant::now();
            // 外层监督覆盖请求响应头、业务 await 与退避；退出即丢弃所有借用 future，
            // 不遗留跨账号回写的 detached task。
            let attempt = async {
                let channel = tokio::time::timeout(
                    self.sse_connect_timeout,
                    self.open_event_channel(epoch, &device_id),
                )
                .await
                .map_err(|_| {
                    // 整体握手超时发生在 WebSocket 探测阶段：算一次「握手成功路径不可用」。
                    if self.ws_handshaking.swap(false, Ordering::Relaxed) {
                        self.note_ws_unusable("handshake response headers timed out");
                    }
                    RemoteError::Protocol(
                        "remote event channel response headers timeout".to_owned(),
                    )
                })??;
                // 通道已建立：重连期间错过的投递变化由第一页全量拉取纠正。
                if let Some(feed) = &self.notify_feed {
                    feed.connected();
                }
                let is_socket = matches!(channel, EventChannel::Socket(_));
                let (mut events, _) = self.events.subscribe_and_snapshot();
                let result = self
                    .consume_events_epoch(channel, &cancel, &mut events, epoch)
                    .await;
                // 101 成功却在收到第一个 pong 前就以读错误 / Close / 空闲超时结束：握手能通但通道不可用。
                // 服务端 error 帧、kicked、resync 等是有效应答，不算。
                if is_socket
                    && matches!(result, Err(RemoteError::Protocol(_)))
                    && !self.ws_pong_seen.load(Ordering::Relaxed)
                {
                    self.note_ws_unusable("session ended before the first pong");
                }
                result
            };
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                changed = session_changes.changed() => {
                    if changed.is_err() { break; }
                    *self.runtime.lock().await = Runtime::default();
                    // 会话失效可能由被取消的 HTTP/流处理自己触发；完成最新状态的落盘，
                    // 不让取消停在 clear_session 的临时文件写入中途。
                    if let Err(error) = self.store.persist(&self.state).await {
                        tracing::error!(error = %format!("{error:#}"), "persisting changed remote session failed");
                    }
                    retry_attempt = 0;
                    continue;
                },
                _ = self.session_ended(&mut session_events, uid.as_deref(), epoch) => {
                    *self.runtime.lock().await = Runtime::default();
                    retry_attempt = 0;
                    continue;
                },
                _ = self.reconnect.notified() => {
                    retry_attempt = 0;
                    continue;
                },
                result = attempt => result,
            };
            let Ok(state) = self.cloud.lock_epoch(epoch).await else {
                retry_attempt = 0;
                continue;
            };
            let delay = match result {
                Ok(SseEnd::Cancelled) => break,
                Ok(SseEnd::SessionEnded) => continue,
                Ok(SseEnd::Resync | SseEnd::Drained) => {
                    self.connection(CloudConnectionState::Reconnecting, None);
                    RESYNC_PAUSE
                }
                Ok(SseEnd::Kicked) => {
                    tracing::warn!("this device was taken offline by another device");
                    self.connection(
                        CloudConnectionState::Reconnecting,
                        Some(&RemoteError::Kicked),
                    );
                    limit_shown = true;
                    retry_attempt = 0;
                    ONLINE_LIMIT_RETRY
                }
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "remote task connection failed");
                    self.connection(CloudConnectionState::Reconnecting, Some(&error));
                    if error.is_online_limit() {
                        limit_shown = true;
                        retry_attempt = 0;
                        ONLINE_LIMIT_RETRY
                    } else {
                        if connected_at.elapsed() >= SSE_STABLE {
                            retry_attempt = 0;
                        }
                        RETRY_DELAYS[retry_attempt.min(RETRY_DELAYS.len() - 1)]
                    }
                }
            };
            drop(state);
            retry_attempt = retry_attempt.saturating_add(1);
            tokio::select! {
                _ = cancel.cancelled() => break,
                changed = session_changes.changed() => {
                    if changed.is_err() { break; }
                    *self.runtime.lock().await = Runtime::default();
                    retry_attempt = 0;
                },
                _ = self.session_ended(&mut session_events, uid.as_deref(), epoch) => {
                    retry_attempt = 0;
                },
                _ = self.reconnect.notified() => { retry_attempt = 0; },
                _ = tokio::time::sleep(delay) => {},
            }
        }
        let _state = self.state.lock().await;
        self.connection(CloudConnectionState::Disconnected, None);
    }

    /// 回退期内为 `true`；过期顺手清除。
    fn ws_fallback_active(&self) -> bool {
        let mut until = self
            .ws_fallback_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match *until {
            Some(deadline) if Instant::now() < deadline => true,
            Some(_) => {
                *until = None;
                false
            }
            None => false,
        }
    }

    /// 进入 1 小时粘性回退（SSE + HTTP 心跳）。
    fn start_ws_fallback(&self) {
        *self
            .ws_fallback_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Instant::now() + WS_FALLBACK_STICKY);
        self.ws_unusable_streak.store(0, Ordering::Relaxed);
    }

    /// 记一次「握手可达但通道不可用」；连续 `WS_UNUSABLE_THRESHOLD` 次进入粘性回退，收到 pong 清零。
    /// 网络连接错误与 401 / 403 / 429 / 5xx 不计入（这些 SSE 也会失败）。
    fn note_ws_unusable(&self, why: &str) {
        let streak = self.ws_unusable_streak.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::warn!(streak, why, "remote task WebSocket unusable");
        if streak >= WS_UNUSABLE_THRESHOLD {
            self.start_ws_fallback();
            tracing::info!(
                "remote task WebSocket repeatedly unusable on this network path; falling back to SSE for 1h"
            );
        }
    }

    /// 建立任务实时通道：未处于回退期先试 WebSocket；握手被判定「不支持」则记录粘性回退并在
    /// 同一次尝试内改走 SSE。其它错误（含 403 / 429 / 5xx / 网络错误）原样返回，不回退；
    /// 握手等响应头超时另记一次「不可用」。
    async fn open_event_channel(
        &self,
        epoch: crate::cloud::RequestEpoch,
        device_id: &str,
    ) -> Result<EventChannel, RemoteError> {
        let api = self.cloud.at_epoch(epoch);
        if !self.ws_fallback_active() {
            self.ws_handshaking.store(true, Ordering::Relaxed);
            let opened = api.remote_socket(device_id).await;
            self.ws_handshaking.store(false, Ordering::Relaxed);
            match opened {
                Ok(socket) => {
                    self.ws_pong_seen.store(false, Ordering::Relaxed);
                    return Ok(EventChannel::Socket(Box::new(socket)));
                }
                Err(error) if error.is_websocket_unsupported() => {
                    self.start_ws_fallback();
                    tracing::info!(
                        status = ?error.status,
                        reason = %error.message,
                        "remote task WebSocket unsupported on this network path; falling back to SSE for 1h"
                    );
                }
                Err(error) => {
                    if error.is_websocket_handshake_timeout() {
                        self.note_ws_unusable("handshake response headers timed out");
                    }
                    return Err(error.into());
                }
            }
        }
        Ok(EventChannel::Stream(api.remote_events(device_id).await?))
    }

    pub fn request_reconnect(&self) {
        self.reconnect.notify_one();
    }

    fn connection(&self, state: CloudConnectionState, error: Option<&RemoteError>) {
        let connection = CloudConnectionDto {
            state,
            last_error: error.map(|error| format!("{error:#}")),
            last_error_reason: error.map(|error| match error {
                RemoteError::Cloud(error) => {
                    error.reason().unwrap_or(ErrorReason::CloudUnreachable)
                }
                RemoteError::Kicked => ErrorReason::OnlineDeviceLimit,
                _ => ErrorReason::CloudUnreachable,
            }),
        };
        let unchanged = self
            .events
            .inspect(|snapshot| snapshot.cloud_connection == connection);
        if !unchanged {
            self.events
                .publish(AgentEvent::CloudConnectionChanged(connection));
        }
    }

    async fn session_ended(
        &self,
        events: &mut broadcast::Receiver<fluxdown_protocol::EventFrame>,
        uid: Option<&str>,
        epoch: crate::cloud::RequestEpoch,
    ) {
        loop {
            match events.recv().await {
                Ok(frame) => {
                    if self.observe_agent_event(&frame.event, uid, epoch).await {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let Ok(state) = self.cloud.lock_epoch(epoch).await else {
                        return;
                    };
                    let current_uid = state
                        .credentials
                        .as_ref()
                        .and_then(|credentials| credentials.session.as_ref())
                        .map(|session| session.user.id.as_str());
                    if current_uid != uid {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    #[cfg(test)]
    async fn consume_events(
        &self,
        response: reqwest::Response,
        cancel: &CancellationToken,
        agent_events: &mut broadcast::Receiver<fluxdown_protocol::EventFrame>,
    ) -> Result<SseEnd, RemoteError> {
        self.consume_events_epoch(
            EventChannel::Stream(response),
            cancel,
            agent_events,
            self.cloud.request_epoch(),
        )
        .await
    }

    async fn consume_events_epoch(
        &self,
        channel: EventChannel,
        cancel: &CancellationToken,
        agent_events: &mut broadcast::Receiver<fluxdown_protocol::EventFrame>,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<SseEnd, RemoteError> {
        let (sender, receiver) = mpsc::channel(SSE_QUEUE_CAPACITY);
        let roster = Notify::new();
        let presence_ready = Notify::new();
        let transport_alive = AtomicBool::new(true);
        let uid = self
            .cloud
            .lock_epoch(epoch)
            .await?
            .credentials
            .as_ref()
            .and_then(|credentials| credentials.session.as_ref())
            .map(|session| session.user.id.clone());
        let business =
            self.business_loop(receiver, &roster, &presence_ready, epoch, &transport_alive);
        tokio::pin!(business);
        let stop_reading = CancellationToken::new();
        let fallback_until = *self
            .ws_fallback_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let transport = async {
            let (mut reader, heartbeat): (TransportFuture<'_>, TransportFuture<'_>) = match channel
            {
                EventChannel::Socket(socket) => {
                    let (sink, stream) = (*socket).split();
                    (
                        Box::pin(self.read_socket(
                            stream,
                            sender,
                            &roster,
                            &presence_ready,
                            epoch,
                            &stop_reading,
                        )),
                        Box::pin(self.socket_heartbeat_loop(sink)),
                    )
                }
                EventChannel::Stream(response) => (
                    Box::pin(self.read_stream(response, sender, &roster, epoch, &stop_reading)),
                    Box::pin(self.heartbeat_loop(&presence_ready, epoch, fallback_until)),
                ),
            };
            tokio::select! {
                result = &mut reader => result,
                result = heartbeat => {
                    transport_alive.store(false, Ordering::Relaxed);
                    {
                        let _state = self.cloud.lock_epoch(epoch).await?;
                        self.connection(CloudConnectionState::Reconnecting, result.as_ref().err());
                    }
                    // Finish enqueueing the chunk already received, including a pending send.
                    // The business future keeps running while the reader applies backpressure.
                    stop_reading.cancel();
                    match reader.await {
                        Ok(SseEnd::SessionEnded) => Ok(SseEnd::SessionEnded),
                        Ok(_) => result,
                        Err(error) => Err(error),
                    }
                },
            }
        };
        let transport_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(SseEnd::Cancelled),
            _ = self.session_ended(agent_events, uid.as_deref(), epoch) => return Ok(SseEnd::SessionEnded),
            result = transport => result,
            result = &mut business => return result,
        };
        if matches!(
            transport_result,
            Ok(SseEnd::Cancelled | SseEnd::SessionEnded)
        ) {
            return transport_result;
        }
        transport_alive.store(false, Ordering::Relaxed);
        {
            let _state = self.cloud.lock_epoch(epoch).await?;
            let kicked = RemoteError::Kicked;
            let error = match &transport_result {
                Err(error) => Some(error),
                Ok(SseEnd::Kicked) => Some(&kicked),
                Ok(_) => None,
            };
            self.connection(CloudConnectionState::Reconnecting, error);
        }
        // Transport dropped its sender. Drain accepted commands in FIFO order;
        // unlike task state, pause/resume and deleteFiles cannot be replayed by a snapshot.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(SseEnd::Cancelled),
            _ = self.session_ended(agent_events, uid.as_deref(), epoch) => Ok(SseEnd::SessionEnded),
            result = &mut business => match result {
                Ok(SseEnd::Drained) => transport_result,
                other => other,
            },
        }
    }

    /// SSE 回退路径的 HTTP 心跳。名册不在此刷新：只在首次 presence 建立、`presence` /
    /// `device.updated` 事件与失败重试 tick 时拉取。`fallback_until` 是本连接建立时的
    /// WebSocket 粘性回退截止时刻：心跳成功后若已过期则返回 `Resync`，重连并重新探测 WebSocket。
    async fn heartbeat_loop(
        &self,
        ready: &Notify,
        epoch: crate::cloud::RequestEpoch,
        fallback_until: Option<Instant>,
    ) -> Result<SseEnd, RemoteError> {
        let mut heartbeat = tokio::time::interval(self.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            heartbeat.tick().await;
            match self.cloud.at_epoch(epoch).ping_presence().await {
                Err(error)
                    if error.status == Some(409)
                        && error.code.as_deref() == Some("presence_connection_missing") =>
                {
                    return Ok(SseEnd::Resync);
                }
                result => {
                    result?;
                }
            }
            ready.notify_one();
            if fallback_until.is_some_and(|deadline| Instant::now() >= deadline) {
                tracing::info!(
                    "WebSocket fallback period elapsed; reconnecting to probe WebSocket again"
                );
                return Ok(SseEnd::Resync);
            }
        }
    }

    /// WebSocket 心跳：写半边独立循环，第一个 `ping` 立即发出，之后每 `heartbeat_interval` 一个。
    /// 应答 `pong` 由读半边转成 presence 就绪信号。
    async fn socket_heartbeat_loop(
        &self,
        mut sink: SplitSink<CloudSocket, Message>,
    ) -> Result<SseEnd, RemoteError> {
        let mut heartbeat = tokio::time::interval(self.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            heartbeat.tick().await;
            sink.send(Message::text(SOCKET_PING_FRAME))
                .await
                .map_err(|error| {
                    RemoteError::Protocol(format!("remote socket ping failed: {error}"))
                })?;
        }
    }

    async fn business_loop(
        &self,
        mut payloads: mpsc::Receiver<Vec<u8>>,
        roster: &Notify,
        presence_ready: &Notify,
        epoch: crate::cloud::RequestEpoch,
        transport_alive: &AtomicBool,
    ) -> Result<SseEnd, RemoteError> {
        // 名册只在在线租约建立后抓取；首次成功前不把旧缓存标成实时连接。
        let mut presence_established = false;
        let mut snapshot_pending = true;
        let mut roster_pending = true;
        let mut retry = tokio::time::interval(ACCEPT_RETRY_INTERVAL);
        let mut report = tokio::time::interval(REPORT_INTERVAL);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = presence_ready.notified(), if !presence_established && transport_alive.load(Ordering::Relaxed) => {
                    presence_established = true;
                    roster_pending = !self.refresh_connected_roster(epoch, transport_alive).await;
                }
                _ = retry.tick(), if transport_alive.load(Ordering::Relaxed) => {
                    if roster_pending && presence_established {
                        roster_pending = !self.refresh_connected_roster(epoch, transport_alive).await;
                    }
                    if snapshot_pending {
                        match self.refresh_snapshot_epoch(epoch).await {
                            Ok(_) => {
                                snapshot_pending = false;
                                self.rebuild_bindings(epoch).await;
                            }
                            Err(error) => tracing::warn!(error = %format!("{error:#}"), "remote task snapshot refresh failed; retry scheduled"),
                        }
                    }
                    self.accept_pending_dispatches_epoch(epoch).await;
                }
                _ = roster.notified(), if presence_established && transport_alive.load(Ordering::Relaxed) => {
                    roster_pending = !self.refresh_connected_roster(epoch, transport_alive).await;
                }
                _ = report.tick(), if transport_alive.load(Ordering::Relaxed) => {
                    if let Err(error) = self.report_local_progress_epoch(epoch).await {
                        tracing::warn!(error = %format!("{error:#}"), "remote progress report failed");
                    }
                }
                payload = payloads.recv() => {
                    let Some(payload) = payload else { return Ok(SseEnd::Drained); };
                    if let Some(end) = self.handle_event_payload(&payload, epoch).await {
                        return Ok(end);
                    }
                }
            }
        }
    }

    async fn refresh_connected_roster(
        &self,
        epoch: crate::cloud::RequestEpoch,
        transport_alive: &AtomicBool,
    ) -> bool {
        let result = self.refresh_devices_epoch(epoch).await;
        let _state = match self.cloud.lock_epoch(epoch).await {
            Ok(state) => state,
            Err(error) => {
                tracing::debug!(%error, "discarding stale roster connection status");
                return false;
            }
        };
        match result {
            Ok(()) => {
                if transport_alive.load(Ordering::Relaxed) {
                    self.connection(CloudConnectionState::Connected, None);
                }
                true
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "cloud device roster refresh failed; retry scheduled");
                self.connection(CloudConnectionState::Reconnecting, Some(&error));
                false
            }
        }
    }

    async fn read_stream(
        &self,
        response: reqwest::Response,
        sender: mpsc::Sender<Vec<u8>>,
        roster: &Notify,
        epoch: crate::cloud::RequestEpoch,
        stop_reading: &CancellationToken,
    ) -> Result<SseEnd, RemoteError> {
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        // 空闲看门狗：周期性定时器不重置截止时间。主动等业务队列排空时暂停读流，
        // 不把本机背压误报成网络静默；其间心跳与会话取消仍独立运行。
        let idle = tokio::time::sleep(self.sse_idle_timeout);
        tokio::pin!(idle);
        loop {
            tokio::select! {
                biased;
                _ = stop_reading.cancelled() => return Ok(SseEnd::Resync),
                () = &mut idle => {
                    return Err(RemoteError::Protocol("remote SSE idle timeout".to_owned()));
                }
                chunk = stream.next() => {
                    let chunk = chunk
                        .ok_or_else(|| RemoteError::Protocol("remote SSE disconnected".to_owned()))?
                        .map_err(|error| RemoteError::Protocol(format!("remote SSE read failed: {error:#}")))?;
                    idle.as_mut().reset(tokio::time::Instant::now() + self.sse_idle_timeout);
                    // Copy only one bounded line, not the entire remaining transport chunk.
                    for part in chunk.split_inclusive(|byte| *byte == b'\n') {
                        if buffer.len().saturating_add(part.len()) > MAX_SSE_LINE_BYTES {
                            return Err(RemoteError::Protocol("remote SSE line too long".to_owned()));
                        }
                        buffer.extend_from_slice(part);
                        if !buffer.ends_with(b"\n") {
                            continue;
                        }
                        let line = std::mem::take(&mut buffer);
                        let Some(payload) = line.strip_prefix(b"data:") else {
                            continue;
                        };
                        let payload = payload.trim_ascii().to_vec();
                        let event = serde_json::from_slice::<Value>(&payload).ok();
                        if let Some(end) = self
                            .route_event_payload(payload, event, &sender, roster, epoch)
                            .await?
                        {
                            return Ok(end);
                        }
                        // Time spent intentionally applying backpressure is not network silence.
                        idle.as_mut().reset(tokio::time::Instant::now() + self.sse_idle_timeout);
                    }
                }
            }
        }
    }

    /// WebSocket 读半边：空闲看门狗、背压与 `stop_reading` 语义与 [`Self::read_stream`] 一致。
    /// `pong` 即 presence 就绪；`error` 帧等价原 HTTP 心跳的 403 / 409。
    async fn read_socket(
        &self,
        mut stream: SplitStream<CloudSocket>,
        sender: mpsc::Sender<Vec<u8>>,
        roster: &Notify,
        ready: &Notify,
        epoch: crate::cloud::RequestEpoch,
        stop_reading: &CancellationToken,
    ) -> Result<SseEnd, RemoteError> {
        let idle = tokio::time::sleep(self.sse_idle_timeout);
        tokio::pin!(idle);
        loop {
            tokio::select! {
                biased;
                _ = stop_reading.cancelled() => return Ok(SseEnd::Resync),
                () = &mut idle => {
                    return Err(RemoteError::Protocol("remote socket idle timeout".to_owned()));
                }
                frame = stream.next() => {
                    let message = frame
                        .ok_or_else(|| RemoteError::Protocol("remote socket closed".to_owned()))?
                        .map_err(|error| RemoteError::Protocol(format!("remote socket read failed: {error}")))?;
                    idle.as_mut().reset(tokio::time::Instant::now() + self.sse_idle_timeout);
                    match message {
                        Message::Text(text) => {
                            let payload = text.as_str().as_bytes().to_vec();
                            let event = serde_json::from_slice::<Value>(&payload).ok();
                            let kind = event
                                .as_ref()
                                .and_then(|event| event.get("type"))
                                .and_then(Value::as_str);
                            if kind == Some("pong") {
                                self.ws_pong_seen.store(true, Ordering::Relaxed);
                                self.ws_unusable_streak.store(0, Ordering::Relaxed);
                                ready.notify_one();
                                continue;
                            }
                            if kind == Some("error")
                                && let Some(frame) = &event
                            {
                                return socket_error_end(frame);
                            }
                            if let Some(end) = self
                                .route_event_payload(payload, event, &sender, roster, epoch)
                                .await?
                            {
                                return Ok(end);
                            }
                            // Time spent intentionally applying backpressure is not network silence.
                            idle.as_mut().reset(tokio::time::Instant::now() + self.sse_idle_timeout);
                        }
                        Message::Close(frame) => {
                            return Err(RemoteError::Protocol(format!(
                                "remote socket closed by server: {frame:?}"
                            )));
                        }
                        // 二进制帧忽略；协议层 Ping / Pong 由库自动处理。
                        Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
            }
        }
    }

    /// 一条 JSON 事件的入口，SSE 与 WebSocket 共用（读取侧只解析一次，`event` 为已解析结果）：
    /// `presence` / `device.updated` 只触发名册刷新；`session.revoked` / `presence.kicked` 立即应用
    /// （可能直接结束连接）；其余事件把原始 `payload` 入业务队列。
    /// 业务队列满时读取在此暂停——pause / resume 等命令无法由任务快照重建，所以背压而不丢弃；
    /// 心跳与会话取消由调用方独立轮询。队列接收端已关闭返回 `Resync`。
    async fn route_event_payload(
        &self,
        payload: Vec<u8>,
        event: Option<Value>,
        sender: &mpsc::Sender<Vec<u8>>,
        roster: &Notify,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Option<SseEnd>, RemoteError> {
        if let Some(event) = event {
            let kind = event.get("type").and_then(Value::as_str);
            let roster_only = matches!(kind, Some("presence" | "device.updated"));
            let apply_now = matches!(kind, Some("session.revoked" | "presence.kicked"));
            if roster_only {
                roster.notify_one();
                return Ok(None);
            }
            if apply_now && let Some(end) = self.apply_remote_event_epoch(event, epoch).await? {
                return Ok(Some(end));
            }
        }
        if sender.send(payload).await.is_err() {
            return Ok(Some(SseEnd::Resync));
        }
        Ok(None)
    }

    /// 观察 agent 事件：跟踪 daemon 任务的实时速度；返回 `true` 表示会话已结束或换成了别的账号。
    async fn observe_agent_event(
        &self,
        event: &ServiceEvent,
        stream_uid: Option<&str>,
        epoch: crate::cloud::RequestEpoch,
    ) -> bool {
        let Ok(_state) = self.cloud.lock_epoch(epoch).await else {
            return true;
        };
        let ServiceEvent::Agent(event) = event else {
            return false;
        };
        match event {
            AgentEvent::SessionChanged(session) => match &**session {
                None => true,
                Some(session) => stream_uid.is_some_and(|uid| uid != session.user.id),
            },
            AgentEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                status,
                speed,
                ..
            })) => {
                let mut runtime = self.runtime.lock().await;
                if matches!(*status, 1 | 5) {
                    runtime.local_speeds.insert(task_id.clone(), *speed);
                } else {
                    runtime.local_speeds.remove(task_id);
                }
                false
            }
            AgentEvent::DaemonConnectionChanged(true) => {
                self.runtime.lock().await.local_missing_rounds.clear();
                false
            }
            AgentEvent::Daemon(DaemonEvent::TaskDeleted { task_id }) => {
                self.runtime.lock().await.local_speeds.remove(task_id);
                false
            }
            AgentEvent::Daemon(DaemonEvent::TasksDeleted { task_ids }) => {
                let mut runtime = self.runtime.lock().await;
                for task_id in task_ids {
                    runtime.local_speeds.remove(task_id);
                }
                false
            }
            _ => false,
        }
    }

    /// 单个 JSON 事件的容错处理（SSE `data:` 与 WebSocket 文本帧共用）：
    /// 坏 UTF-8 / 非 JSON / 单个事件处理失败只记录，不拆整条通道。
    async fn handle_event_payload(
        &self,
        payload: &[u8],
        epoch: crate::cloud::RequestEpoch,
    ) -> Option<SseEnd> {
        let event = match serde_json::from_slice::<Value>(payload) {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(error = %error, "remote event is not JSON; skipped");
                return None;
            }
        };
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match self.apply_remote_event_epoch(event, epoch).await {
            Ok(end) => end,
            Err(error) => {
                tracing::warn!(event = %kind, error = %error, "remote event handling failed");
                None
            }
        }
    }

    #[cfg(test)]
    async fn apply_remote_event(&self, event: Value) -> Result<Option<SseEnd>, RemoteError> {
        self.apply_remote_event_epoch(event, self.cloud.request_epoch())
            .await
    }

    async fn apply_remote_event_epoch(
        &self,
        event: Value,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Option<SseEnd>, RemoteError> {
        drop(self.cloud.lock_epoch(epoch).await?);
        match event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "task.dispatch" | "task.status" => {
                let task = serde_json::from_value::<RemoteTaskDto>(event)?;
                let canceled_here = task.status == RemoteTaskStatus::Canceled
                    && task.to_device == self.local_device_id().await;
                let remote_id = task.id.clone();
                self.upsert_task(task, epoch).await?;
                if canceled_here {
                    self.discard_local_task_epoch(&remote_id, epoch).await;
                }
                self.rebuild_bindings(epoch).await;
                self.accept_pending_dispatches_epoch(epoch).await;
            }
            "task.progress" => {
                let items = event
                    .get("items")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                // 进度只合并进内存投影，不落盘（约 1 次 / 秒）。
                let mut tasks = self.tasks().await;
                if apply_progress_items(&mut tasks, items) {
                    self.replace_tasks(tasks, false, epoch).await?;
                }
            }
            "task.command" => {
                let target = event
                    .get("toDevice")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if target == self.local_device_id().await {
                    let command = IncomingCommand::from_event(&event)?;
                    self.execute_local_command(command, epoch).await?;
                }
            }
            "task.removed" => {
                // 记录被删除（任意状态）：从投影移除；本设备正在执行它时删除本机任务。
                if let Some(id) = event.get("taskId").and_then(Value::as_str) {
                    let mut tasks = self.tasks().await;
                    let before = tasks.len();
                    tasks.retain(|task| task.id != id);
                    if tasks.len() != before {
                        self.replace_tasks(tasks, true, epoch).await?;
                    }
                    self.discard_local_task_epoch(id, epoch).await;
                }
            }
            "presence.kicked" => {
                // 同账号的另一台设备手动让某设备下线；只有指向本机时才结束 SSE，
                // 其余设备的下线由随后的 `presence` 事件刷新名册。
                let target = event.get("deviceId").and_then(Value::as_str);
                if target == Some(self.local_device_id().await.as_str()) {
                    return Ok(Some(SseEnd::Kicked));
                }
            }
            "presence" | "device.updated" => {
                if let Err(error) = self.refresh_devices_epoch(epoch).await {
                    tracing::warn!(error = %error, "cloud device roster refresh failed");
                }
            }
            "session.revoked" => {
                let target = event.get("deviceId").and_then(Value::as_str);
                let local = self.local_device_id().await;
                if target.is_none_or(|target| target == local) {
                    // 指向本设备（被删除 / 被替换）= 设备不再受信任；未指明设备（管理员撤销全部会话等）
                    // = 会话过期。`revoke_session` 先发 `SessionRevoked` 再投影 `SessionChanged(None)`。
                    let password_changed =
                        event.get("reason").and_then(Value::as_str) == Some("passwordChanged");
                    let reason = if password_changed {
                        ErrorReason::PasswordChanged
                    } else if target.is_some() {
                        ErrorReason::DeviceUntrusted
                    } else {
                        ErrorReason::SessionExpired
                    };
                    if let Err(error) = self.cloud.revoke_session_epoch(reason, epoch).await {
                        tracing::warn!(error = %error, "clearing revoked session failed");
                    }
                }
            }
            "notify.changed" => {
                if let Some(feed) = &self.notify_feed {
                    feed.changed();
                }
            }
            "notify.delivery" => {
                let items = event
                    .get("items")
                    .cloned()
                    .map(serde_json::from_value::<Vec<fluxdown_protocol::CloudNotifyDeliveryDto>>)
                    .transpose()?
                    .unwrap_or_default();
                if let Some(feed) = &self.notify_feed
                    && !items.is_empty()
                {
                    feed.deliveries(items, epoch);
                }
            }
            "resync" => {
                if let Some(feed) = &self.notify_feed {
                    feed.resync();
                }
                return Ok(Some(SseEnd::Resync));
            }
            _ => {}
        }
        Ok(None)
    }

    /// 替换任务投影；`persist` 为 `false` 时只更新内存并发布事件。
    async fn replace_tasks(
        &self,
        tasks: Vec<RemoteTaskDto>,
        persist: bool,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            state.remote_tasks.clone_from(&tasks);
            self.events.publish(AgentEvent::RemoteTasksChanged(tasks));
        }
        if persist {
            self.store.persist(&self.state).await?;
        }
        Ok(())
    }

    async fn upsert_task(
        &self,
        task: RemoteTaskDto,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        let mut state = self.cloud.lock_epoch(epoch).await?;
        let tasks = &mut state.remote_tasks;
        if let Some(existing) = tasks.iter_mut().find(|existing| existing.id == task.id) {
            existing.clone_from(&task);
        } else {
            tasks.push(task);
        }
        self.events
            .publish(AgentEvent::RemoteTasksChanged(tasks.clone()));
        drop(state);
        self.store.persist(&self.state).await?;
        Ok(())
    }

    async fn binding(
        &self,
        remote_id: &str,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Option<String>, RemoteError> {
        Ok(self
            .cloud
            .lock_epoch(epoch)
            .await?
            .remote_bindings
            .get(remote_id)
            .cloned())
    }

    async fn set_binding(
        &self,
        remote_id: &str,
        local_id: &str,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        let mut state = self.cloud.lock_epoch(epoch).await?;
        state
            .remote_bindings
            .insert(remote_id.to_owned(), local_id.to_owned());
        self.runtime
            .lock()
            .await
            .bound_at
            .insert(remote_id.to_owned(), Instant::now());
        drop(state);
        if let Err(error) = self.store.persist(&self.state).await {
            tracing::warn!(error = %error, "persisting remote task binding failed");
        }
        Ok(())
    }

    async fn remove_binding_epoch(&self, remote_id: &str, epoch: crate::cloud::RequestEpoch) {
        let removed = {
            let mut state = match self.cloud.lock_epoch(epoch).await {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(%error, "discarding stale binding removal");
                    return;
                }
            };
            let removed = state.remote_bindings.remove(remote_id).is_some();
            self.runtime.lock().await.bound_at.remove(remote_id);
            removed
        };
        if removed && let Err(error) = self.store.persist(&self.state).await {
            tracing::warn!(error = %error, "persisting remote task binding removal failed");
        }
    }

    /// 已有绑定，或按 URL / 文件名重建出的绑定（未接单 / 已终态的任务不会被重建）。
    async fn resolve_binding(
        &self,
        remote_id: &str,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Option<String>, RemoteError> {
        if let Some(local_id) = self.binding(remote_id, epoch).await? {
            return Ok(Some(local_id));
        }
        self.rebuild_bindings(epoch).await;
        self.binding(remote_id, epoch).await
    }

    /// 云端已取消 / 删除本设备执行的任务：删除绑定的本机 daemon 任务并解除绑定。
    ///
    /// 保留已下载文件——目标离线期间发起端的 `deleteFiles` 选择送不到这里；本机已完成的任务不动。
    /// daemon 暂时不可用时保留绑定，由下一次状态上报（404 / 409）或快照对照重试。
    async fn discard_local_task_epoch(&self, remote_id: &str, epoch: crate::cloud::RequestEpoch) {
        let local_id = match self.cloud.lock_epoch(epoch).await {
            Ok(state) => state.remote_bindings.get(remote_id).cloned(),
            Err(error) => {
                tracing::debug!(%error, "discarding stale local-task removal");
                return;
            }
        };
        let Some(local_id) = local_id else {
            return;
        };
        // daemon 状态 3 = 已完成。
        let completed = daemon_tasks(&self.events).is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task.task_id == local_id && task.status == 3)
        });
        if !completed {
            tracing::info!(task = %remote_id, local = %local_id, "remote task was canceled or deleted in FluxCloud; removing the local task");
            if let Err(error) = self
                .daemon_task_call(
                    fluxdown_protocol::method::DAEMON_TASK_DELETE,
                    &local_id,
                    false,
                    epoch,
                )
                .await
            {
                if error.code == fluxdown_protocol::ApplicationErrorCode::Unavailable {
                    tracing::warn!(task = %remote_id, "daemon is unavailable; will retry removing the local task");
                    return;
                }
                tracing::warn!(task = %remote_id, error = ?error, "removing the local task of a canceled remote task failed");
            }
        }
        self.remove_binding_epoch(remote_id, epoch).await;
    }

    async fn rebuild_bindings(&self, epoch: crate::cloud::RequestEpoch) {
        let local_device = self.local_device_id().await;
        let Some(local_tasks) = daemon_tasks(&self.events) else {
            return;
        };
        let remote_tasks = self.tasks().await;
        let mut newly_bound = Vec::new();
        {
            let mut state = match self.cloud.lock_epoch(epoch).await {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(%error, "discarding stale binding rebuild");
                    return;
                }
            };
            let mut claimed = state
                .remote_bindings
                .values()
                .cloned()
                .collect::<HashSet<_>>();
            for remote in &remote_tasks {
                if remote.to_device != local_device
                    || remote.status == RemoteTaskStatus::Pending
                    || remote.status == RemoteTaskStatus::Unknown
                    || remote.status.is_terminal()
                    || state.remote_bindings.contains_key(&remote.id)
                {
                    continue;
                }
                if let Some(local) = local_tasks
                    .iter()
                    .filter(|task| !claimed.contains(&task.task_id))
                    .filter_map(|task| {
                        let score = Self::rebind_score(remote, task);
                        (score >= 11).then_some((score, task))
                    })
                    .max_by_key(|(score, _)| *score)
                    .map(|(_, task)| task)
                {
                    state
                        .remote_bindings
                        .insert(remote.id.clone(), local.task_id.clone());
                    claimed.insert(local.task_id.clone());
                    newly_bound.push(remote.id.clone());
                }
            }
        }
        if newly_bound.is_empty() {
            return;
        }
        {
            let _state = match self.cloud.lock_epoch(epoch).await {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(%error, "discarding stale binding timestamps");
                    return;
                }
            };
            let now = Instant::now();
            let mut runtime = self.runtime.lock().await;
            for remote_id in newly_bound {
                runtime.bound_at.insert(remote_id, now);
            }
        }
        if let Err(error) = self.store.persist(&self.state).await {
            tracing::warn!(error = %error, "persisting rebuilt remote task bindings failed");
        }
    }

    /// 接单：逐条容错，任何一条失败都不影响后面的任务，也不中断 SSE。
    #[cfg(test)]
    async fn accept_pending_dispatches(&self) {
        self.accept_pending_dispatches_epoch(self.cloud.request_epoch())
            .await;
    }

    async fn accept_pending_dispatches_epoch(&self, epoch: crate::cloud::RequestEpoch) {
        if let Err(error) = self.cloud.lock_epoch(epoch).await {
            tracing::debug!(%error, "discarding stale pending dispatches");
            return;
        }
        let local_device = self.local_device_id().await;
        let pending = self
            .tasks()
            .await
            .into_iter()
            .filter(|task| {
                task.to_device == local_device && task.status == RemoteTaskStatus::Pending
            })
            .collect::<Vec<_>>();
        for task in pending {
            match self.binding(&task.id, epoch).await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%error, "discarding stale dispatch binding lookup");
                    return;
                }
            }
            self.accept_one(&task, epoch).await;
        }
    }

    async fn accept_one(&self, task: &RemoteTaskDto, epoch: crate::cloud::RequestEpoch) {
        if task.url.trim().is_empty() {
            self.report_accept_failure_epoch(task, "dispatched task has no URL", epoch)
                .await;
            return;
        }
        let requested_dir = accept_save_dir(task.save_dir.as_deref());
        if requested_dir.is_none()
            && task
                .save_dir
                .as_deref()
                .is_some_and(|dir| !dir.trim().is_empty())
        {
            tracing::info!(
                task = %task.id,
                save_dir = ?task.save_dir,
                "dispatched save directory is not usable on this device; using the default directory"
            );
        }
        let created = create_with_directory_fallback(requested_dir, |dir| {
            self.create_local_task(task, dir, epoch)
        })
        .await;
        match created {
            Ok(local_id) => {
                // 先持久化绑定再上报：上报失败 / 进程重启都不会重复建任务。
                if let Err(error) = self.set_binding(&task.id, &local_id, epoch).await {
                    tracing::debug!(%error, "discarding stale accepted binding");
                    return;
                }
                match self
                    .cloud
                    .at_epoch(epoch)
                    .report_remote_status(&task.id, &json!({"status": "accepted"}))
                    .await
                {
                    Ok(_) => {
                        let _state = match self.cloud.lock_epoch(epoch).await {
                            Ok(state) => state,
                            Err(error) => {
                                tracing::debug!(%error, "discarding stale accepted status");
                                return;
                            }
                        };
                        self.runtime
                            .lock()
                            .await
                            .reported_statuses
                            .insert(task.id.clone(), RemoteTaskStatus::Accepted);
                    }
                    // 接单期间记录已被删除：删除刚建的本机任务。
                    Err(error) if error.status == Some(404) => {
                        tracing::info!(task = %task.id, "accepted task no longer exists in FluxCloud");
                        self.discard_local_task_epoch(&task.id, epoch).await;
                    }
                    // 接单期间已被取消。
                    Err(error) if error.status == Some(409) => {
                        self.resolve_status_conflict_epoch(&task.id, epoch).await;
                    }
                    // 其余失败：下一轮进度上报会带上真实状态。
                    Err(error) => {
                        tracing::warn!(task = %task.id, error = %error, "reporting accepted status failed");
                    }
                }
            }
            Err(failure) if failure.transient => {
                // daemon 暂时不可用（启动中 / 重连）：不是任务本身的问题，保持 pending，稍后重试。
                tracing::info!(task = %task.id, error = %failure.message, "daemon is unavailable; will retry accepting the task");
            }
            Err(failure) => {
                tracing::warn!(task = %task.id, error = %failure.message, "accepting dispatched task failed");
                self.report_accept_failure_epoch(task, &failure.message, epoch)
                    .await;
            }
        }
    }

    async fn create_local_task(
        &self,
        task: &RemoteTaskDto,
        save_dir: Option<String>,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<String, CreateFailure> {
        let _state = self
            .cloud
            .lock_epoch(epoch)
            .await
            .map_err(|error| CreateFailure {
                transient: true,
                message: error.to_string(),
            })?;
        drop(_state);
        let mut request = json!({ "url": task.url });
        let file_name = task.file_name.trim();
        if !file_name.is_empty() {
            if !valid_file_name(file_name) {
                return Err(CreateFailure::permanent(
                    "dispatched file name is not a plain file name".to_owned(),
                ));
            }
            request["fileName"] = json!(file_name);
        }
        if let Some(dir) = &save_dir {
            // daemon 不校验目录：先确认本机确实能用（能创建 / 已存在），否则交给上层去掉目录重试。
            tokio::fs::create_dir_all(dir).await.map_err(|error| {
                CreateFailure::permanent(format!("save directory `{dir}` is unavailable: {error}"))
            })?;
            request["saveDir"] = json!(dir);
        }
        let request = serde_json::from_value::<CreateTaskRequest>(request).map_err(|error| {
            CreateFailure::permanent(format!("invalid dispatched task: {error}"))
        })?;
        drop(
            self.cloud
                .lock_epoch(epoch)
                .await
                .map_err(|error| CreateFailure {
                    transient: true,
                    message: error.to_string(),
                })?,
        );
        let category_route = if save_dir.is_none() {
            self.events.inspect(|snapshot| {
                crate::category_dir::category_route(&snapshot.preferences.values)
            })
        } else {
            None
        };
        let result = self
            .daemon
            .call_detailed::<DaemonCreateTaskParams, Value>(
                fluxdown_protocol::method::DAEMON_TASK_CREATE,
                Some(DaemonCreateTaskParams {
                    request,
                    torrent_blob_id: None,
                    unattended: true,
                    hint_file_size: None,
                    name_hint: None,
                    category_route,
                }),
            )
            .await
            .map_err(|error| CreateFailure {
                transient: error.data.as_ref().is_some_and(|data| {
                    data.code == fluxdown_protocol::ApplicationErrorCode::Unavailable
                }),
                message: error.message,
            })?;
        result
            .get("taskId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CreateFailure::permanent("daemon returned no taskId".to_owned()))
    }

    /// 接单失败：向云端上报 `failed` + 错误，让发起端看到原因而不是永远 pending。
    #[cfg(test)]
    async fn report_accept_failure(&self, task: &RemoteTaskDto, message: &str) {
        self.report_accept_failure_epoch(task, message, self.cloud.request_epoch())
            .await;
    }

    async fn report_accept_failure_epoch(
        &self,
        task: &RemoteTaskDto,
        message: &str,
        epoch: crate::cloud::RequestEpoch,
    ) {
        let error: String = message.chars().take(MAX_REPORTED_ERROR_CHARS).collect();
        match self
            .cloud
            .at_epoch(epoch)
            .report_remote_status(&task.id, &json!({"status": "failed", "error": error}))
            .await
        {
            Ok(_) => {
                let _state = match self.cloud.lock_epoch(epoch).await {
                    Ok(state) => state,
                    Err(error) => {
                        tracing::debug!(%error, "discarding stale failure report");
                        return;
                    }
                };
                self.runtime
                    .lock()
                    .await
                    .reported_statuses
                    .insert(task.id.clone(), RemoteTaskStatus::Failed);
            }
            Err(report_error) => {
                tracing::warn!(task = %task.id, error = %report_error, "reporting failed accept status failed");
            }
        }
    }

    #[cfg(test)]
    async fn report_local_progress(&self) -> Result<(), RemoteError> {
        self.report_local_progress_epoch(self.cloud.request_epoch())
            .await
    }

    async fn report_local_progress_epoch(
        &self,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        // daemon 冷启动时快照为空：此时判定「本机任务消失」会把仍在下载的任务误报为 failed。
        let Some(local_tasks) = daemon_tasks(&self.events) else {
            return Ok(());
        };
        let bindings = self.cloud.lock_epoch(epoch).await?.remote_bindings.clone();
        let mut progress = Vec::new();
        for (remote_id, local_id) in bindings {
            let Some(task) = local_tasks.iter().find(|task| task.task_id == local_id) else {
                let expired = {
                    let _state = self.cloud.lock_epoch(epoch).await?;
                    let mut runtime = self.runtime.lock().await;
                    let count = runtime
                        .local_missing_rounds
                        .entry(remote_id.clone())
                        .or_default();
                    *count = count.saturating_add(1);
                    *count >= MISSING_GRACE_ROUNDS
                };
                if expired {
                    self.report_status_epoch(
                        &remote_id,
                        RemoteTaskStatus::Failed,
                        Some("local task disappeared"),
                        None,
                        epoch,
                    )
                    .await?;
                    self.remove_binding_epoch(&remote_id, epoch).await;
                }
                continue;
            };
            let state = self.cloud.lock_epoch(epoch).await?;
            self.runtime
                .lock()
                .await
                .local_missing_rounds
                .remove(&remote_id);
            drop(state);
            let status = local_status(task.status);
            self.report_status_epoch(
                &remote_id,
                status,
                (!task.error_message.is_empty()).then_some(task.error_message.as_str()),
                Some(task),
                epoch,
            )
            .await?;
            if matches!(task.status, 1 | 5) {
                let speed = self
                    .runtime
                    .lock()
                    .await
                    .local_speeds
                    .get(&task.task_id)
                    .copied()
                    .unwrap_or(0);
                progress.push(progress_item(
                    &remote_id,
                    task.downloaded_bytes,
                    task.total_bytes,
                    speed,
                ));
            }
        }
        if !progress.is_empty() {
            self.cloud
                .at_epoch(epoch)
                .report_remote_progress(&json!({"items": progress}))
                .await?;
        }
        Ok(())
    }

    #[cfg(test)]
    async fn report_status_if_changed(
        &self,
        remote_id: &str,
        status: RemoteTaskStatus,
        error: Option<&str>,
        task: Option<&fluxdown_protocol::TaskDto>,
    ) -> Result<(), RemoteError> {
        let epoch = self.cloud.request_epoch();
        self.report_status_epoch(remote_id, status, error, task, epoch)
            .await
    }

    async fn report_status_epoch(
        &self,
        remote_id: &str,
        status: RemoteTaskStatus,
        error: Option<&str>,
        task: Option<&fluxdown_protocol::TaskDto>,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        if self.runtime.lock().await.reported_statuses.get(remote_id) == Some(&status) {
            return Ok(());
        }
        // `downloadedBytes`：云端在终态转换时冻结它（进度快照随终态回收），失败 / 取消的任务
        // 在发起端仍显示停在哪里；旧版云端忽略该字段。
        let body = json!({
            "status": remote_status_wire(status),
            "totalBytes": task.map(|task| task.total_bytes),
            "downloadedBytes": task.map(|task| task.downloaded_bytes.max(0)),
            "fileName": task.map(|task| task.file_name.clone()),
            "error": error,
        });
        match self
            .cloud
            .at_epoch(epoch)
            .report_remote_status(remote_id, &body)
            .await
        {
            Ok(_) => {
                let _state = self.cloud.lock_epoch(epoch).await?;
                self.runtime
                    .lock()
                    .await
                    .reported_statuses
                    .insert(remote_id.to_owned(), status);
                drop(_state);
                if status.is_terminal() {
                    self.remove_binding_epoch(remote_id, epoch).await;
                }
                Ok(())
            }
            Err(error) if error.status == Some(409) => {
                self.resolve_status_conflict_epoch(remote_id, epoch).await;
                Ok(())
            }
            Err(error) if error.status == Some(404) => {
                // 云端已删除该任务（离线期间被删除等）：删除本机任务并解除绑定。
                tracing::info!(task = %remote_id, "remote task no longer exists in FluxCloud");
                self.discard_local_task_epoch(remote_id, epoch).await;
                Ok(())
            }
            Err(error) if error.status == Some(403) => {
                // 本设备不是它的目标：解除绑定，不再上报。
                tracing::info!(task = %remote_id, "remote task no longer accepts reports from this device");
                self.remove_binding_epoch(remote_id, epoch).await;
                Ok(())
            }
            Err(error) => Err(RemoteError::Cloud(error)),
        }
    }

    /// 上报状态遭遇 409：云端已经把任务终态化（例如被发起端取消）。云端为 `canceled` 或记录已
    /// 不存在时删除本机任务（保留已下载文件），其余终态只解除绑定。
    async fn resolve_status_conflict_epoch(
        &self,
        remote_id: &str,
        epoch: crate::cloud::RequestEpoch,
    ) {
        if let Err(error) = self.refresh_snapshot_epoch(epoch).await {
            tracing::warn!(task = %remote_id, error = %error, "refreshing tasks after a status conflict failed");
            return;
        }
        let cloud_status = self
            .tasks()
            .await
            .into_iter()
            .find(|task| task.id == remote_id)
            .map(|task| task.status);
        if matches!(cloud_status, Some(RemoteTaskStatus::Canceled) | None) {
            self.discard_local_task_epoch(remote_id, epoch).await;
        } else {
            self.remove_binding_epoch(remote_id, epoch).await;
        }
        // 之后不再重复上报同一状态。
        let _state = match self.cloud.lock_epoch(epoch).await {
            Ok(state) => state,
            Err(error) => {
                tracing::debug!(%error, "discarding stale status conflict");
                return;
            }
        };
        self.runtime.lock().await.reported_statuses.insert(
            remote_id.to_owned(),
            cloud_status.unwrap_or(RemoteTaskStatus::Canceled),
        );
    }

    /// 首次或重连后用完整 `/tasks/remote` 快照替换投影。
    ///
    /// 执行端同时据快照收敛本机任务：云端已 `canceled`，或快照请求发出前就已绑定、快照里却没有
    /// （记录已被删除；云端快照从不截断未终态任务）的，删除本机任务——目标离线期间被取消 / 删除的
    /// 任务在重连时收敛，不会继续在后台下载。
    pub async fn refresh_snapshot(&self) -> Result<Vec<RemoteTaskDto>, RemoteError> {
        let epoch = self.cloud.request_epoch();
        self.refresh_snapshot_epoch(epoch).await
    }

    async fn refresh_snapshot_epoch(
        &self,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Vec<RemoteTaskDto>, RemoteError> {
        let requested_at = Instant::now();
        let value = self.cloud.at_epoch(epoch).remote_tasks().await?;
        drop(self.cloud.lock_epoch(epoch).await?);
        let tasks = parse_task_list(&value);
        let (stale, deleted) = self.stale_bindings(&tasks, requested_at, epoch).await?;
        for remote_id in &stale {
            self.discard_local_task_epoch(remote_id, epoch).await;
        }
        let mut merged = self.apply_missing_grace_epoch(tasks, epoch).await?;
        merged.retain(|task| !deleted.contains(&task.id));
        let bound = {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            state.remote_tasks.clone_from(&merged);
            // 已终态 / 已被云端删除的任务不再需要绑定；本机任务删除失败（daemon 暂不可用）的保留重试。
            let live = merged
                .iter()
                .filter(|task| !task.status.is_terminal())
                .map(|task| task.id.as_str())
                .chain(stale.iter().map(String::as_str))
                .collect::<HashSet<_>>();
            state
                .remote_bindings
                .retain(|remote_id, _| live.contains(remote_id.as_str()));
            state
                .remote_bindings
                .keys()
                .cloned()
                .collect::<HashSet<_>>()
        };
        {
            let _state = self.cloud.lock_epoch(epoch).await?;
            self.runtime
                .lock()
                .await
                .bound_at
                .retain(|remote_id, _| bound.contains(remote_id));
            self.events
                .publish(AgentEvent::RemoteTasksChanged(merged.clone()));
        }
        self.store.persist(&self.state).await?;
        Ok(merged)
    }

    /// 快照对照本机绑定：返回（需要删除本机任务的绑定，其中记录已被删除的那部分）。
    /// 快照请求发出之后才建立的绑定不凭缺失判定——任务可能在该快照之后才下发。
    async fn stale_bindings(
        &self,
        snapshot: &[RemoteTaskDto],
        requested_at: Instant,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(Vec<String>, HashSet<String>), RemoteError> {
        let state = self.cloud.lock_epoch(epoch).await?;
        let bound = state.remote_bindings.keys().cloned().collect::<Vec<_>>();
        let runtime = self.runtime.lock().await;
        let mut stale = Vec::new();
        let mut deleted = HashSet::new();
        for remote_id in bound {
            match snapshot.iter().find(|task| task.id == remote_id) {
                Some(task) if task.status == RemoteTaskStatus::Canceled => stale.push(remote_id),
                Some(_) => {}
                None if runtime
                    .bound_at
                    .get(&remote_id)
                    .is_none_or(|bound_at| *bound_at < requested_at) =>
                {
                    deleted.insert(remote_id.clone());
                    stale.push(remote_id);
                }
                None => {}
            }
        }
        Ok((stale, deleted))
    }

    pub async fn local_device_id(&self) -> String {
        self.state.lock().await.device_id.clone()
    }

    /// 拉取受信任设备名册并投影 `CloudDevicesChanged`（会话建立后与 presence 事件共用）。
    pub async fn refresh_devices(&self) -> Result<(), RemoteError> {
        let epoch = self.cloud.request_epoch();
        self.refresh_devices_epoch(epoch).await
    }

    async fn refresh_devices_epoch(
        &self,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        let value = self
            .cloud
            .at_epoch(epoch)
            .devices(&self.local_device_id().await)
            .await?;
        let devices = parse_device_list(&value);
        let _state = self.cloud.lock_epoch(epoch).await?;
        self.events
            .publish(AgentEvent::CloudDevicesChanged(devices));
        Ok(())
    }

    pub async fn tasks(&self) -> Vec<RemoteTaskDto> {
        self.state.lock().await.remote_tasks.clone()
    }

    /// 经 FluxCloud 把下载下发到本账号的另一台设备。
    ///
    /// `save_dir` 省略 = 目标设备的默认目录；给出时必须是目标设备路径风格下的绝对路径。
    pub async fn dispatch(
        &self,
        params: RemoteDispatchParams,
    ) -> Result<RemoteDispatchResult, RemoteError> {
        let epoch = self.cloud.request_epoch();
        let local_device = self.local_device_id().await;
        if params.to_device == local_device {
            return Err(RemoteError::InvalidArgument {
                field: "toDevice",
                reason: None,
                message: "cannot dispatch a download to this device".to_owned(),
            });
        }
        if params.url.trim().is_empty() {
            return Err(RemoteError::InvalidArgument {
                field: "url",
                reason: None,
                message: "url is empty".to_owned(),
            });
        }
        let save_dir = params
            .save_dir
            .as_deref()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
            .map(str::to_owned);
        if let Some(dir) = &save_dir {
            let target = self.events.inspect(|snapshot| {
                snapshot
                    .cloud_devices
                    .iter()
                    .find(|device| device.device_id == params.to_device)
                    .cloned()
            });
            validate_target_save_dir(target.as_ref(), dir)?;
        }
        let file_name = params
            .file_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let value = self
            .cloud
            .at_epoch(epoch)
            .dispatch_remote(&json!({
                "deviceId": local_device,
                "toDevice": params.to_device,
                "url": params.url,
                "fileName": file_name,
                "saveDir": save_dir,
            }))
            .await?;
        let task_value = value.get("task").cloned().unwrap_or(value);
        let task = serde_json::from_value::<RemoteTaskDto>(task_value)?;
        // 立即进入投影：UI 不必等云端推送。
        {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            if let Some(existing) = state
                .remote_tasks
                .iter_mut()
                .find(|existing| existing.id == task.id)
            {
                existing.clone_from(&task);
            } else {
                state.remote_tasks.push(task.clone());
            }
            self.events
                .publish(AgentEvent::RemoteTasksChanged(state.remote_tasks.clone()));
        }
        if let Err(error) = self.store.persist(&self.state).await {
            tracing::warn!(error = %error, "projecting the dispatched task failed");
        }
        Ok(RemoteDispatchResult { task })
    }

    /// 远程任务控制。
    ///
    /// - pause / resume：目标是本机时直接映射到 daemon；否则经云端转发（目标离线时云端 409）。
    /// - cancel / delete：目标是本机且已接单时先处理本机任务（delete 按 `delete_files`），再交给云端
    ///   落库。云端不依赖目标在线判定：cancel 置 `canceled`，delete 删除任何状态的记录；执行端据
    ///   持久态收敛本机任务。delete 成功后立即从投影移除该行。
    /// - `command_id` 缺省生成唯一值，同一动作可重复下发；相同 id 只执行一次。
    pub async fn command(&self, params: RemoteCommandParams) -> Result<(), RemoteError> {
        let epoch = self.cloud.request_epoch();
        let command_id = params
            .command_id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        if self
            .runtime
            .lock()
            .await
            .confirmed_commands
            .contains(&command_id)
        {
            return Ok(());
        }
        let task = self
            .tasks()
            .await
            .into_iter()
            .find(|task| task.id == params.task_id)
            .ok_or_else(|| RemoteError::NotFound(params.task_id.clone()))?;
        if task.status == RemoteTaskStatus::Unknown {
            return Err(RemoteError::TaskUnavailable(task.id));
        }
        let local_target = task.to_device == self.local_device_id().await;
        let local_command = IncomingCommand {
            task_id: task.id.clone(),
            action: params.action,
            command_id: command_id.clone(),
            delete_files: params.delete_files,
        };
        match params.action {
            RemoteCommandAction::Pause | RemoteCommandAction::Resume if local_target => {
                return self.execute_local_command(local_command, epoch).await;
            }
            // 同一个 command id 已记入去重窗口：云端回显给本机的 `task.command` 不会再执行一次。
            RemoteCommandAction::Cancel | RemoteCommandAction::Delete if local_target => {
                self.execute_local_command(local_command, epoch).await?;
            }
            _ => {}
        }
        let sent = self
            .cloud
            .at_epoch(epoch)
            .command_remote(
                &task.id,
                &json!({
                    "action": action_wire(params.action),
                    "commandId": command_id,
                    "deleteFiles": params.delete_files,
                }),
            )
            .await;
        match sent {
            Ok(_) => {}
            // 记录已被别的设备删除：删除的目标已达成。
            Err(error)
                if params.action == RemoteCommandAction::Delete && error.status == Some(404) => {}
            Err(error) => return Err(RemoteError::Cloud(error)),
        }
        {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            if params.action == RemoteCommandAction::Delete {
                state.remote_tasks.retain(|existing| existing.id != task.id);
                self.events
                    .publish(AgentEvent::RemoteTasksChanged(state.remote_tasks.clone()));
            }
            self.runtime
                .lock()
                .await
                .confirmed_commands
                .insert(command_id);
        }
        self.store.persist(&self.state).await?;
        Ok(())
    }

    /// 执行端：把命令映射到本机 daemon；命令 id 只在执行成功后记入去重窗口。
    /// cancel / delete 遇到没有本机任务的远程任务（尚未接单 / 已处理过）直接成功：
    /// 云端记录由指令发起方落库，这里没有要删的东西。
    async fn execute_local_command(
        &self,
        command: IncomingCommand,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        drop(self.cloud.lock_epoch(epoch).await?);
        if self
            .runtime
            .lock()
            .await
            .confirmed_commands
            .contains(&command.command_id)
        {
            return Ok(());
        }
        let finalizes = matches!(
            command.action,
            RemoteCommandAction::Cancel | RemoteCommandAction::Delete
        );
        match self.resolve_binding(&command.task_id, epoch).await? {
            Some(local_id) => {
                let (method, delete_files) = match command.action {
                    RemoteCommandAction::Pause => {
                        (fluxdown_protocol::method::DAEMON_TASK_PAUSE, false)
                    }
                    RemoteCommandAction::Resume => {
                        (fluxdown_protocol::method::DAEMON_TASK_RESUME, false)
                    }
                    RemoteCommandAction::Cancel => {
                        (fluxdown_protocol::method::DAEMON_TASK_DELETE, false)
                    }
                    RemoteCommandAction::Delete => (
                        fluxdown_protocol::method::DAEMON_TASK_DELETE,
                        command.delete_files,
                    ),
                };
                self.daemon_task_call(method, &local_id, delete_files, epoch)
                    .await
                    .map_err(RemoteError::Daemon)?;
                if finalizes {
                    // 先解绑再上报：记录已被删除时上报得到 404，不会再去删一次本机任务。
                    self.remove_binding_epoch(&command.task_id, epoch).await;
                    if let Err(error) = self
                        .report_status_epoch(
                            &command.task_id,
                            RemoteTaskStatus::Canceled,
                            None,
                            None,
                            epoch,
                        )
                        .await
                    {
                        tracing::warn!(task = %command.task_id, error = %error, "reporting canceled status failed");
                    }
                }
            }
            None if finalizes => {
                tracing::debug!(task = %command.task_id, "no local task to remove for this remote task");
            }
            None => {
                return Err(RemoteError::Protocol(format!(
                    "no local task is bound to remote task {}",
                    command.task_id
                )));
            }
        }
        let _state = self.cloud.lock_epoch(epoch).await?;
        self.runtime
            .lock()
            .await
            .confirmed_commands
            .insert(command.command_id);
        Ok(())
    }

    async fn daemon_task_call(
        &self,
        method: &str,
        local_id: &str,
        delete_files: bool,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Value, RpcErrorData> {
        let _state = self.cloud.lock_epoch(epoch).await.map_err(|error| {
            tracing::debug!(%error, "discarding stale daemon command");
            RpcErrorData::new(fluxdown_protocol::ApplicationErrorCode::Unavailable, false)
        })?;
        drop(_state);
        let params = if method == fluxdown_protocol::method::DAEMON_TASK_DELETE {
            json!({ "taskId": local_id, "deleteFiles": delete_files })
        } else {
            json!({ "taskId": local_id })
        };
        self.daemon.call(method, Some(params)).await
    }

    /// URL 为主键，文件名与保存目录提供稳定加分。
    #[must_use]
    pub fn rebind_score(remote: &RemoteTaskDto, local: &fluxdown_protocol::TaskDto) -> i32 {
        if remote.url != local.url && remote.url != local.origin_url {
            return -1;
        }
        let mut score = 10;
        if !remote.file_name.is_empty() && remote.file_name == local.file_name {
            score += 4;
        }
        if remote
            .save_dir
            .as_deref()
            .is_some_and(|dir| dir == local.save_dir)
        {
            score += 2;
        }
        score
    }

    async fn apply_missing_grace_epoch(
        &self,
        incoming: Vec<RemoteTaskDto>,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Vec<RemoteTaskDto>, RemoteError> {
        let mut state = self.cloud.lock_epoch(epoch).await?;
        let previous = state.remote_tasks.clone();
        let incoming_ids = incoming
            .iter()
            .map(|task| task.id.clone())
            .collect::<HashSet<_>>();
        let mut expired = Vec::new();
        let mut merged = incoming;
        {
            let mut runtime = self.runtime.lock().await;
            for task in previous {
                if incoming_ids.contains(task.id.as_str()) || task.status.is_terminal() {
                    runtime.missing_rounds.remove(&task.id);
                    continue;
                }
                let round = runtime.missing_rounds.entry(task.id.clone()).or_default();
                *round = round.saturating_add(1);
                if *round < MISSING_GRACE_ROUNDS {
                    merged.push(task);
                } else {
                    runtime.missing_rounds.remove(&task.id);
                    expired.push(task.id);
                }
            }
        }
        if !expired.is_empty() {
            for id in expired {
                state.remote_bindings.remove(&id);
            }
        }
        Ok(merged)
    }
}

/// 云端下发的命令（SSE `task.command` 或本机直接控制）。
struct IncomingCommand {
    task_id: String,
    action: RemoteCommandAction,
    command_id: String,
    delete_files: bool,
}

impl IncomingCommand {
    /// `commandId` 缺省时生成唯一值——不能用「任务:动作」作幂等键，否则同一动作第二次会被吞掉。
    fn from_event(event: &Value) -> Result<Self, RemoteError> {
        let task_id = event
            .get("taskId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if task_id.is_empty() {
            return Err(RemoteError::Protocol(
                "task.command without taskId".to_owned(),
            ));
        }
        let action = match event
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "pause" => RemoteCommandAction::Pause,
            "resume" => RemoteCommandAction::Resume,
            "cancel" => RemoteCommandAction::Cancel,
            "delete" => RemoteCommandAction::Delete,
            other => return Err(RemoteError::InvalidAction(other.to_owned())),
        };
        let command_id = event
            .get("commandId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);
        Ok(Self {
            task_id: task_id.to_owned(),
            action,
            command_id,
            delete_files: event
                .get("deleteFiles")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// 接单时使用的保存目录：与局域网互联入口共用 [`resolve_receive_dir`]（本机风格绝对路径，
/// 拒绝 `..`、UNC 与设备路径）；其余（缺省 / 对端风格 / 相对路径）一律用本机默认目录。
fn accept_save_dir(requested: Option<&str>) -> Option<String> {
    resolve_receive_dir(requested?)
}

/// 建任务失败；`transient` = daemon 暂时不可用（不是任务本身的问题，不应上报为失败）。
#[derive(Debug, Eq, PartialEq)]
struct CreateFailure {
    message: String,
    transient: bool,
}

impl CreateFailure {
    fn permanent(message: String) -> Self {
        Self {
            message,
            transient: false,
        }
    }
}

/// 带目录建任务；目录类失败时去掉目录重试，仍失败返回最后一次的错误。
async fn create_with_directory_fallback<F, Fut>(
    directory: Option<String>,
    mut create: F,
) -> Result<String, CreateFailure>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<String, CreateFailure>>,
{
    match create(directory.clone()).await {
        Ok(id) => Ok(id),
        Err(error) if directory.is_some() && !error.transient => {
            tracing::warn!(error = %error.message, "creating the task in the requested directory failed; retrying with the default directory");
            create(None).await
        }
        Err(error) => Err(error),
    }
}

/// 发起端校验：保存目录必须是目标设备路径风格下的绝对路径。
fn validate_target_save_dir(
    target: Option<&CloudDevice>,
    save_dir: &str,
) -> Result<(), RemoteError> {
    let Some(style) = target.and_then(CloudDevice::effective_path_style) else {
        // 目标未上报风格且平台未知：无法判断，交给云端与执行端兜底。
        return Ok(());
    };
    if style.is_absolute(save_dir) {
        Ok(())
    } else {
        Err(RemoteError::InvalidArgument {
            field: "saveDir",
            reason: Some(ErrorReason::SaveDirUnavailable),
            message: format!("saveDir must be an absolute {style:?} path on the target device"),
        })
    }
}

/// 把 `task.progress` 的 `items[]` 合并进任务列表；返回是否有任务变化。
/// 云端字段是 `taskId`（不是 `id`）；终态任务不被进度覆盖。
fn apply_progress_items(tasks: &mut [RemoteTaskDto], items: &[Value]) -> bool {
    let mut changed = false;
    for item in items {
        let Some(id) = item.get("taskId").and_then(Value::as_str) else {
            continue;
        };
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            continue;
        };
        if task.status.is_terminal() || task.status == RemoteTaskStatus::Unknown {
            continue;
        }
        if task.status != RemoteTaskStatus::Downloading {
            task.status = RemoteTaskStatus::Downloading;
        }
        if let Some(value) = item.get("downloadedBytes").and_then(Value::as_i64) {
            task.downloaded_bytes = value.max(0);
        }
        if let Some(value) = item.get("speed").and_then(Value::as_i64) {
            task.speed = value.max(0);
        }
        if let Some(value) = item.get("progress").and_then(Value::as_f64) {
            task.progress = clamp_progress(value);
        }
        if let Some(value) = item.get("totalBytes").and_then(Value::as_i64) {
            task.total_bytes = Some(value.max(0));
        }
        changed = true;
    }
    changed
}

fn clamp_progress(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 进度上报条目；`progress` 夹到 [0,1]（云端校验 `0..=1`，越界会拒绝整批）。
fn progress_item(remote_id: &str, downloaded: i64, total: i64, speed: i64) -> Value {
    let progress = if total > 0 {
        clamp_progress(downloaded as f64 / total as f64)
    } else {
        0.0
    };
    json!({
        "taskId": remote_id,
        "downloadedBytes": downloaded.max(0),
        "speed": speed.max(0),
        "progress": progress,
    })
}

/// daemon 任务快照；daemon 未连接时为 `None`——此时空列表不代表「任务不存在」。
fn daemon_tasks(events: &AgentEventHub) -> Option<Vec<fluxdown_protocol::TaskDto>> {
    match events.snapshot().body {
        fluxdown_protocol::SnapshotBody::Agent(snapshot) if snapshot.daemon_connected => {
            Some(snapshot.daemon.tasks)
        }
        _ => None,
    }
}

/// daemon 任务状态 → 云端状态：0 排队 → accepted，1 下载中 / 5 准备中 → downloading。
fn local_status(status: i32) -> RemoteTaskStatus {
    match status {
        1 | 5 => RemoteTaskStatus::Downloading,
        2 => RemoteTaskStatus::Paused,
        3 => RemoteTaskStatus::Completed,
        4 => RemoteTaskStatus::Failed,
        _ => RemoteTaskStatus::Accepted,
    }
}

fn remote_status_wire(status: RemoteTaskStatus) -> &'static str {
    match status {
        RemoteTaskStatus::Pending | RemoteTaskStatus::Unknown => "pending",
        RemoteTaskStatus::Accepted => "accepted",
        RemoteTaskStatus::Downloading => "downloading",
        RemoteTaskStatus::Paused => "paused",
        RemoteTaskStatus::Completed => "completed",
        RemoteTaskStatus::Failed => "failed",
        RemoteTaskStatus::Canceled => "canceled",
    }
}

fn action_wire(action: RemoteCommandAction) -> &'static str {
    match action {
        RemoteCommandAction::Pause => "pause",
        RemoteCommandAction::Resume => "resume",
        RemoteCommandAction::Cancel => "cancel",
        RemoteCommandAction::Delete => "delete",
    }
}

trait RemoteStatusExt {
    fn is_terminal(&self) -> bool;
}

impl RemoteStatusExt for RemoteTaskStatus {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

/// 逐条解析：单条畸形任务只被跳过并记录，不让整份快照（以及设备名册 / 接单 / SSE）停摆。
fn parse_task_list(value: &Value) -> Vec<RemoteTaskDto> {
    parse_list(value, &["tasks", "value"], "remote task")
}

fn parse_device_list(value: &Value) -> Vec<CloudDevice> {
    parse_list(value, &["devices", "value"], "cloud device")
}

fn parse_list<T: serde::de::DeserializeOwned>(value: &Value, keys: &[&str], what: &str) -> Vec<T> {
    let list = keys
        .iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    list.iter()
        .filter_map(|item| match serde_json::from_value::<T>(item.clone()) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                tracing::warn!(error = %error, item = %item, "skipping malformed {what}");
                None
            }
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error(transparent)]
    Cloud(#[from] CloudError),
    #[error("daemon remote command failed: {0:?}")]
    Daemon(RpcErrorData),
    /// 本机被同账号的另一台设备手动下线（`presence.kicked`）。
    #[error("this device was taken offline by another device")]
    Kicked,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    State(#[from] crate::state::StateError),
    #[error("invalid remote action: {0}")]
    InvalidAction(String),
    #[error("remote task protocol error: {0}")]
    Protocol(String),
    #[error("remote task not found: {0}")]
    NotFound(String),
    /// 云端给出了本端不认识的任务状态，不可控制。
    #[error("remote task {0} is in a state this agent cannot control")]
    TaskUnavailable(String),
    #[error("invalid {field}: {message}")]
    InvalidArgument {
        field: &'static str,
        reason: Option<ErrorReason>,
        message: String,
    },
}

impl RemoteError {
    /// 云端以 `online_device_limit` 拒绝（名额已满），或本机被手动下线：不是故障，30 秒后重试即可。
    fn is_online_limit(&self) -> bool {
        match self {
            Self::Kicked => true,
            Self::Cloud(error) => error.reason() == Some(ErrorReason::OnlineDeviceLimit),
            _ => false,
        }
    }
}

/// 服务端 `error` 帧 → 连接结果：`online_device_limit` 等价原心跳 403（走 30s 名额等待），
/// `presence_connection_missing` 等价原 409（快速重连），其它码是协议错误。
fn socket_error_end(event: &Value) -> Result<SseEnd, RemoteError> {
    let code = event
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = event
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(MAX_SOCKET_ERROR_CHARS)
        .collect::<String>();
    match code {
        "online_device_limit" => Err(RemoteError::Cloud(CloudError {
            status: Some(403),
            code: Some(code.to_owned()),
            message: if message.is_empty() {
                "online device limit reached".to_owned()
            } else {
                message
            },
            retryable: false,
            unreachable: false,
        })),
        "presence_connection_missing" => Ok(SseEnd::Resync),
        other => Err(RemoteError::Protocol(format!(
            "remote socket error {other}: {message}"
        ))),
    }
}

/// 等到下一条 `SessionChanged(Some)`；接收端 lag 时也返回，由调用方重新检查登录态。
async fn wait_for_session(events: &mut broadcast::Receiver<fluxdown_protocol::EventFrame>) {
    loop {
        match events.recv().await {
            Ok(frame) => {
                if let fluxdown_protocol::ServiceEvent::Agent(AgentEvent::SessionChanged(session)) =
                    &frame.event
                    && session.is_some()
                {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => return,
            Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use fluxdown_protocol::{
        AgentEvent, CloudDevice, PathStyle, RemoteCommandAction, RemoteCommandParams,
        RemoteDispatchParams, RemoteTaskDto, RemoteTaskStatus, TaskDto,
    };
    use serde_json::{Value, json};
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use super::{
        BoundedIds, CreateFailure, IncomingCommand, MISSING_GRACE_ROUNDS, RemoteError,
        RemoteTaskService, SseEnd, accept_save_dir, apply_progress_items,
        create_with_directory_fallback, local_status, parse_task_list, progress_item,
        validate_target_save_dir,
    };
    use crate::state::{AgentState, CloudCredentials, StateStore};

    fn remote_task(value: Value) -> RemoteTaskDto {
        serde_json::from_value(value).expect("remote task")
    }

    #[test]
    fn rebind_score_requires_url_and_rewards_filename_and_directory() {
        let remote = serde_json::from_value::<RemoteTaskDto>(json!({
            "id": "r1", "url": "https://example.com/a", "fileName": "a.bin",
            "saveDir": "/tmp", "status": "pending"
        }))
        .expect("remote");
        let local = serde_json::from_value::<TaskDto>(json!({
            "taskId":"t1","url":"https://example.com/a","fileName":"a.bin",
            "saveDir":"/tmp","status":1,"downloadedBytes":0,"totalBytes":0,
            "errorMessage":"","createdAt":"1","proxyUrl":"","queueId":"main","checksum":""
        }))
        .expect("local");
        assert_eq!(RemoteTaskService::rebind_score(&remote, &local), 16);
        let unrelated = RemoteTaskDto {
            url: "https://other".to_owned(),
            status: RemoteTaskStatus::Pending,
            ..remote
        };
        assert_eq!(RemoteTaskService::rebind_score(&unrelated, &local), -1);
    }

    #[test]
    fn progress_events_read_task_id_clamp_values_and_skip_terminal_tasks() {
        let mut tasks = vec![
            remote_task(json!({"id": "r1", "status": "accepted"})),
            remote_task(json!({"id": "r2", "status": "completed", "progress": 1.0})),
        ];
        let items = [
            json!({"taskId": "r1", "downloadedBytes": 50, "speed": 7, "progress": 1.7}),
            // 旧字段名 `id` 不再被识别。
            json!({"id": "r1", "downloadedBytes": 999}),
            json!({"taskId": "r2", "downloadedBytes": 5, "progress": 0.1}),
            json!({"taskId": "missing", "downloadedBytes": 5}),
        ];
        assert!(apply_progress_items(&mut tasks, &items));
        assert_eq!(tasks[0].status, RemoteTaskStatus::Downloading);
        assert_eq!(tasks[0].downloaded_bytes, 50);
        assert_eq!(tasks[0].speed, 7);
        assert!(
            (tasks[0].progress - 1.0).abs() < f64::EPSILON,
            "progress is clamped to 1"
        );
        assert_eq!(tasks[1].status, RemoteTaskStatus::Completed);
        assert!((tasks[1].progress - 1.0).abs() < f64::EPSILON);

        assert!(!apply_progress_items(
            &mut tasks,
            &[json!({"taskId": "nope"})]
        ));
        assert!(!apply_progress_items(&mut tasks, &[json!({"id": "r1"})]));
    }

    #[test]
    fn reported_progress_is_clamped_and_never_negative() {
        let item = progress_item("r1", 200, 100, 5);
        assert!((item["progress"].as_f64().expect("progress") - 1.0).abs() < f64::EPSILON);
        let empty = progress_item("r1", -5, 0, -1);
        assert_eq!(empty["downloadedBytes"], 0);
        assert_eq!(empty["speed"], 0);
        assert!((empty["progress"].as_f64().expect("progress")).abs() < f64::EPSILON);
    }

    #[test]
    fn local_status_maps_preparing_to_downloading_and_queued_to_accepted() {
        assert_eq!(local_status(0), RemoteTaskStatus::Accepted);
        assert_eq!(local_status(1), RemoteTaskStatus::Downloading);
        assert_eq!(local_status(5), RemoteTaskStatus::Downloading);
        assert_eq!(local_status(2), RemoteTaskStatus::Paused);
        assert_eq!(local_status(3), RemoteTaskStatus::Completed);
        assert_eq!(local_status(4), RemoteTaskStatus::Failed);
    }

    #[test]
    fn malformed_task_entries_are_skipped_and_null_file_names_tolerated() {
        let value = json!({"tasks": [
            {"id": "ok", "fileName": null, "status": "pending"},
            {"noId": true},
            {"id": "future", "status": "brand-new-status"},
        ]});
        let tasks = parse_task_list(&value);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].file_name, "");
        assert_eq!(tasks[1].status, RemoteTaskStatus::Unknown);
    }

    #[test]
    fn accept_directory_must_be_an_absolute_local_style_path_without_parent_segments() {
        assert_eq!(accept_save_dir(None), None);
        assert_eq!(accept_save_dir(Some("  ")), None);
        assert_eq!(accept_save_dir(Some("relative/dir")), None);
        assert_eq!(accept_save_dir(Some("/data/../etc")), None);
        if cfg!(windows) {
            assert_eq!(
                accept_save_dir(Some(r"D:\Downloads")),
                Some(r"D:\Downloads".to_owned())
            );
            assert_eq!(accept_save_dir(Some("/mnt/data")), None);
        } else {
            assert_eq!(
                accept_save_dir(Some("/mnt/data")),
                Some("/mnt/data".to_owned())
            );
            assert_eq!(accept_save_dir(Some(r"D:\Downloads")), None);
        }
    }

    #[test]
    fn accept_directory_rejects_unc_and_device_paths() {
        assert_eq!(accept_save_dir(Some(r"\\host\share\dl")), None);
        assert_eq!(accept_save_dir(Some(r"\\?\C:\dl")), None);
        assert_eq!(accept_save_dir(Some(r"\\.\pipe\x")), None);
        assert_eq!(accept_save_dir(Some("//host/share")), None);
    }

    #[tokio::test]
    async fn bound_tasks_are_not_failed_while_the_daemon_snapshot_is_unavailable() {
        let harness = Harness::new("cold_start", |router| router).await;
        bind(&harness, "r1", "l1").await;
        for _ in 0..(MISSING_GRACE_ROUNDS + 2) {
            harness
                .service
                .report_local_progress()
                .await
                .expect("progress round");
        }
        assert_eq!(bound_remote_ids(&harness).await, ["r1".to_owned()]);
        harness.finish().await;
    }

    #[tokio::test]
    async fn directory_failure_retries_without_directory_and_reports_the_last_error() {
        // 带目录失败 → 去掉目录重试成功。
        let attempts = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let seen = attempts.clone();
        let created = create_with_directory_fallback(Some("/bad".to_owned()), move |dir| {
            let seen = seen.clone();
            async move {
                seen.lock().await.push(dir.clone());
                match dir {
                    Some(_) => Err(CreateFailure::permanent(
                        "cannot create directory".to_owned(),
                    )),
                    None => Ok("local-1".to_owned()),
                }
            }
        })
        .await;
        assert_eq!(created, Ok("local-1".to_owned()));
        assert_eq!(*attempts.lock().await, vec![Some("/bad".to_owned()), None]);

        // 不带目录也失败 → 返回最后一次错误（调用方据此上报 failed）。
        let failed = create_with_directory_fallback(Some("/bad".to_owned()), |dir| async move {
            Err::<String, _>(CreateFailure::permanent(format!("failed with {dir:?}")))
        })
        .await;
        assert_eq!(
            failed,
            Err(CreateFailure::permanent("failed with None".to_owned()))
        );

        // daemon 暂时不可用是瞬时错误：不会拿去掉目录再试一遍。
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let transient = create_with_directory_fallback(Some("/bad".to_owned()), move |_| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<String, _>(CreateFailure {
                    message: "daemon unavailable".to_owned(),
                    transient: true,
                })
            }
        })
        .await;
        assert!(transient.is_err_and(|failure| failure.transient));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 没有目录时不做多余重试。
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let failed = create_with_directory_fallback(None, move |_| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<String, _>(CreateFailure::permanent("no".to_owned()))
            }
        })
        .await;
        assert!(failed.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn device(value: Value) -> CloudDevice {
        serde_json::from_value(value).expect("cloud device")
    }

    #[test]
    fn dispatch_directory_is_validated_against_the_target_path_style() {
        let windows = device(json!({"id": "1", "deviceId": "w", "platform": "windows"}));
        assert!(validate_target_save_dir(Some(&windows), r"D:\Downloads").is_ok());
        let rejected = validate_target_save_dir(Some(&windows), "/mnt/data");
        assert!(matches!(
            rejected,
            Err(RemoteError::InvalidArgument {
                field: "saveDir",
                reason: Some(_),
                ..
            })
        ));

        // 目标自报风格优先于平台推断。
        let posix_reported = device(
            json!({"id": "2", "deviceId": "p", "platform": "windows", "pathStyle": "posix"}),
        );
        assert_eq!(
            posix_reported.effective_path_style(),
            Some(PathStyle::Posix)
        );
        assert!(validate_target_save_dir(Some(&posix_reported), "/srv/dl").is_ok());
        assert!(validate_target_save_dir(Some(&posix_reported), r"C:\dl").is_err());

        // 风格未知：放行，由执行端兜底。
        let unknown = device(json!({"id": "3", "deviceId": "u", "platform": "web"}));
        assert!(validate_target_save_dir(Some(&unknown), "anything").is_ok());
        assert!(validate_target_save_dir(None, "anything").is_ok());
    }

    #[test]
    fn command_id_window_is_bounded() {
        let mut ids = BoundedIds::default();
        for index in 0..(super::CONFIRMED_COMMAND_CAPACITY + 50) {
            ids.insert(format!("c{index}"));
        }
        assert!(!ids.contains("c0"));
        assert!(ids.contains(&format!("c{}", super::CONFIRMED_COMMAND_CAPACITY + 49)));
        assert_eq!(ids.order.len(), super::CONFIRMED_COMMAND_CAPACITY);
    }

    #[test]
    fn incoming_command_without_command_id_gets_a_unique_id_per_event() {
        let event = json!({"type": "task.command", "taskId": "r1", "action": "pause"});
        let first = IncomingCommand::from_event(&event).expect("first");
        let second = IncomingCommand::from_event(&event).expect("second");
        assert_ne!(first.command_id, second.command_id);
        assert!(!first.delete_files);
        let delete = IncomingCommand::from_event(
            &json!({"taskId": "r1", "action": "delete", "commandId": "c9", "deleteFiles": true}),
        )
        .expect("delete");
        assert_eq!(delete.command_id, "c9");
        assert!(delete.delete_files);
        assert_eq!(delete.action, RemoteCommandAction::Delete);
        assert!(
            IncomingCommand::from_event(&json!({"taskId": "r1", "action": "explode"})).is_err()
        );
    }

    #[derive(Default)]
    struct RemoteMockState {
        snapshots: AtomicUsize,
        events: AtomicUsize,
        presence: AtomicUsize,
        rosters: AtomicUsize,
        snapshot_ready: tokio::sync::Notify,
        presence_ready: tokio::sync::Notify,
        commands: Mutex<Vec<Value>>,
        statuses: Mutex<Vec<(String, Value)>>,
        /// `/api/v1/tasks/ws` 握手（鉴权通过后）的次数与请求信息 `(Authorization, deviceId)`。
        ws_connects: AtomicUsize,
        ws_requests: Mutex<Vec<(Option<String>, Option<String>)>>,
        /// 服务端收到的 `ping` 帧数。
        ws_pings: AtomicUsize,
        ws_unauthorized: AtomicUsize,
        refreshes: AtomicUsize,
        /// 通知 mock 连接向客户端推一条 `presence` 帧。
        ws_release: tokio::sync::Notify,
        ws_mode: std::sync::Mutex<WsMode>,
    }

    /// mock WebSocket 服务端的行为脚本。
    #[derive(Clone, Copy, Default, Eq, PartialEq)]
    enum WsMode {
        /// 每个 `ping` 回 `pong`。
        #[default]
        Healthy,
        /// 第一个 `ping` 回 `online_device_limit` error 帧后关闭。
        LimitOnPing,
        /// 第一个 `ping` 回 `presence_connection_missing` error 帧后关闭。
        MissingOnPing,
        /// 建连后立刻推本机的 `presence.kicked`。
        KickedOnConnect,
        /// 回第一个 `pong` 后服务端主动 Close。
        CloseAfterFirstPong,
        /// 从不应答。
        Silent,
        /// 一帧二进制 + 三条命令，然后 Close。
        CommandsThenClose,
        /// 一帧二进制 + 三条命令 + `resync`，然后 Close。
        CommandsThenResync,
        /// 建连后立刻 Close，不发 pong。
        CloseBeforePong,
        /// 突发超过队列容量的 pause / resume 命令，之后只回 pong。
        Burst,
    }

    async fn mock_remote_snapshot(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
        state.snapshots.fetch_add(1, Ordering::SeqCst);
        state.snapshot_ready.notify_one();
        axum::Json(json!({"tasks": []}))
    }

    async fn mock_remote_events(
        State(state): State<Arc<RemoteMockState>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer access")
        );
        state.events.fetch_add(1, Ordering::SeqCst);
        let body = Body::from_stream(futures_util::stream::once(async move {
            state.snapshot_ready.notified().await;
            state.presence_ready.notified().await;
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b": end\n\n"))
        }));
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
    }

    async fn mock_presence(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
        state.presence.fetch_add(1, Ordering::SeqCst);
        state.presence_ready.notify_one();
        StatusCode::NO_CONTENT
    }

    async fn mock_command(
        State(state): State<Arc<RemoteMockState>>,
        axum::extract::Path(id): axum::extract::Path<String>,
        axum::Json(mut body): axum::Json<Value>,
    ) -> impl IntoResponse {
        body["taskId"] = json!(id);
        state.commands.lock().await.push(body);
        StatusCode::NO_CONTENT
    }

    async fn mock_status(
        State(state): State<Arc<RemoteMockState>>,
        axum::extract::Path(id): axum::extract::Path<String>,
        axum::Json(body): axum::Json<Value>,
    ) -> impl IntoResponse {
        state.statuses.lock().await.push((id, body));
        StatusCode::NO_CONTENT
    }

    type DaemonCalls = Arc<Mutex<Vec<(String, Option<Value>)>>>;

    #[tokio::test]
    async fn ended_stream_epoch_cannot_commit_tasks_or_revoke_replacement_session() {
        let harness = Harness::new("stale_stream_epoch", |app| app).await;
        let epoch = harness.service.cloud.request_epoch();
        let replacement = harness.state.lock().await.credentials.clone();
        harness
            .service
            .cloud
            .clear_session()
            .await
            .expect("end old stream");
        let task: RemoteTaskDto =
            serde_json::from_value(json!({"id": "new-account-task"})).expect("task");
        {
            let mut state = harness.state.lock().await;
            state.credentials = replacement;
            state.remote_tasks = vec![task.clone()];
        }
        harness
            .service
            .events
            .publish(AgentEvent::RemoteTasksChanged(vec![task.clone()]));
        for event in [
            json!({"type": "task.dispatch", "id": "old-task"}),
            json!({"type": "task.progress", "items": []}),
            json!({"type": "task.removed", "taskId": "new-account-task"}),
            json!({"type": "session.revoked"}),
        ] {
            assert!(
                harness
                    .service
                    .apply_remote_event_epoch(event, epoch)
                    .await
                    .is_err()
            );
        }
        // Even a decoded event paused immediately before its state-lock commit
        // must be rejected, independently of the stream's outer cancellation.
        assert!(harness.service.upsert_task(task, epoch).await.is_err());
        assert!(
            harness
                .service
                .replace_tasks(Vec::new(), true, epoch)
                .await
                .is_err()
        );
        let state = harness.state.lock().await;
        assert!(state.credentials.is_some());
        assert_eq!(state.remote_tasks[0].id, "new-account-task");
        drop(state);
        harness.finish().await;
    }

    struct Harness {
        service: Arc<RemoteTaskService>,
        state: Arc<Mutex<AgentState>>,
        store: Arc<StateStore>,
        mock: Arc<RemoteMockState>,
        /// 发给 daemon 的调用（仅 [`Harness::with_daemon`]；其余 harness 的 daemon 永远未连接）。
        daemon_calls: DaemonCalls,
        dir: std::path::PathBuf,
        server: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    impl Harness {
        async fn new(
            label: &str,
            app: impl FnOnce(Router<Arc<RemoteMockState>>) -> Router<Arc<RemoteMockState>>,
        ) -> Self {
            Self::build(label, None, false, app).await
        }

        /// daemon 已连接：调用被记录并成功返回。
        async fn with_daemon(
            label: &str,
            app: impl FnOnce(Router<Arc<RemoteMockState>>) -> Router<Arc<RemoteMockState>>,
        ) -> Self {
            Self::build(label, None, true, app).await
        }

        async fn with_idle_timeout(
            label: &str,
            idle_timeout: Option<std::time::Duration>,
            app: impl FnOnce(Router<Arc<RemoteMockState>>) -> Router<Arc<RemoteMockState>>,
        ) -> Self {
            Self::build(label, idle_timeout, false, app).await
        }

        async fn build(
            label: &str,
            idle_timeout: Option<std::time::Duration>,
            daemon_connected: bool,
            app: impl FnOnce(Router<Arc<RemoteMockState>>) -> Router<Arc<RemoteMockState>>,
        ) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind remote mock");
            let address = listener.local_addr().expect("remote mock address");
            let mock = Arc::new(RemoteMockState::default());
            let router = app(Router::new()).with_state(mock.clone());
            let server = tokio::spawn(async move { axum::serve(listener, router).await });
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_remote_{label}_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let store = Arc::new(
                StateStore::open(dir.clone())
                    .await
                    .expect("remote state store"),
            );
            let initial = AgentState {
                device_id: "device-1".to_owned(),
                credentials: Some(CloudCredentials {
                    access_token: "access".to_owned(),
                    refresh_token: "refresh".to_owned(),
                    expires_at_unix: i64::MAX,
                    session: None,
                }),
                ..Default::default()
            };
            store.save(&initial).await.expect("save remote state");
            let state = Arc::new(Mutex::new(initial));
            let events =
                crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
            let cloud = crate::cloud::CloudApi::new(
                crate::cloud::CloudClient::new(
                    format!("http://{address}"),
                    state.clone(),
                    store.clone(),
                )
                .expect("remote cloud client")
                .with_events(events.clone()),
            );
            let (daemon, daemon_calls) = if daemon_connected {
                crate::daemon_client::DaemonClient::recording()
            } else {
                (
                    crate::daemon_client::DaemonClient::disconnected(),
                    DaemonCalls::default(),
                )
            };
            let mut service = RemoteTaskService::new(
                cloud,
                Arc::new(daemon),
                events,
                state.clone(),
                store.clone(),
            );
            if let Some(idle_timeout) = idle_timeout {
                service.sse_idle_timeout = idle_timeout;
            }
            service.sse_connect_timeout = std::time::Duration::from_secs(2);
            service.heartbeat_interval = std::time::Duration::from_millis(100);
            let service = Arc::new(service);
            Self {
                service,
                state,
                store,
                mock,
                daemon_calls,
                dir,
                server,
            }
        }

        async fn finish(self) {
            let Self {
                service,
                state,
                store,
                dir,
                server,
                ..
            } = self;
            drop(service);
            server.abort();
            match server.await {
                Ok(result) => result.expect("remote mock server completes successfully"),
                Err(error) if error.is_cancelled() => tracing::debug!("remote mock server stopped"),
                Err(error) => panic!("remote mock server panicked: {error}"),
            }
            drop(state);
            drop(store);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), %error, "remote test cleanup failed");
            }
        }
    }

    #[tokio::test]
    async fn worker_reconnects_sse_and_renews_presence_after_disconnect() {
        let harness = Harness::new("worker", |router| {
            router
                .route("/api/v1/tasks/remote", get(mock_remote_snapshot))
                .route("/api/v1/tasks/events", get(mock_remote_events))
                .route("/api/v1/tasks/presence", post(mock_presence))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        let mock = harness.mock.clone();
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while mock.snapshots.load(Ordering::SeqCst) < 2
                || mock.events.load(Ordering::SeqCst) < 2
                || mock.presence.load(Ordering::SeqCst) < 2
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("remote worker reconnected SSE and heartbeat");
        cancel.cancel();
        worker.await.expect("join remote worker");
        harness.finish().await;
    }

    /// 只发响应头、之后一直沉默的 SSE：模拟合盖唤醒 / NAT 静默断流的半开连接。
    async fn silent_events() -> impl IntoResponse {
        let body = Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::convert::Infallible>,
        >());
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
    }

    async fn live_events(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
        state.events.fetch_add(1, Ordering::SeqCst);
        silent_events().await
    }

    async fn online_roster(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
        state.rosters.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({"devices": [{
            "id": "d1", "deviceId": "device-1", "isCurrent": true,
            "isOnline": state.presence.load(Ordering::SeqCst) > 0
        }]}))
    }

    fn connection_state(service: &RemoteTaskService) -> fluxdown_protocol::CloudConnectionDto {
        let fluxdown_protocol::SnapshotBody::Agent(snapshot) = service.events.snapshot().body
        else {
            panic!("agent snapshot");
        };
        snapshot.cloud_connection
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while !condition() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("lifecycle condition");
    }

    #[tokio::test]
    async fn snapshot_failure_does_not_block_sse_and_snapshot_recovers_without_reconnect() {
        async fn snapshot(State(state): State<Arc<RemoteMockState>>) -> axum::response::Response {
            if state.snapshots.fetch_add(1, Ordering::SeqCst) == 0 {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            axum::Json(json!({"tasks": [{"id": "recovered", "url": "https://example.test/a", "status": "completed"}]})).into_response()
        }
        let harness = Harness::new("snapshot_recovery", |router| {
            router
                .route("/api/v1/tasks/events", get(live_events))
                .route("/api/v1/tasks/remote", get(snapshot))
                .route("/api/v1/tasks/presence", post(mock_presence))
                .route("/api/v1/devices", get(online_roster))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service).state
                == fluxdown_protocol::CloudConnectionState::Connected
        })
        .await;
        wait_until(|| {
            let fluxdown_protocol::SnapshotBody::Agent(snapshot) =
                harness.service.events.snapshot().body
            else {
                return false;
            };
            snapshot
                .remote_tasks
                .iter()
                .any(|task| task.id == "recovered")
                && snapshot
                    .cloud_devices
                    .iter()
                    .any(|device| device.is_current && device.is_online)
        })
        .await;
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 1);
        assert!(harness.mock.snapshots.load(Ordering::SeqCst) >= 2);
        cancel.cancel();
        worker.await.expect("worker joined");
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Disconnected
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn slow_snapshot_does_not_block_heartbeat_or_logout_and_cannot_write_back() {
        async fn blocked_snapshot(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
            state.snapshots.fetch_add(1, Ordering::SeqCst);
            state.snapshot_ready.notified().await;
            axum::Json(
                json!({"tasks": [{"id": "old-account", "url": "https://example.test/a", "status": "completed"}]}),
            )
        }
        let harness = Harness::new("slow_business", |router| {
            router
                .route("/api/v1/tasks/events", get(live_events))
                .route("/api/v1/tasks/remote", get(blocked_snapshot))
                .route("/api/v1/tasks/presence", post(mock_presence))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            harness.mock.snapshots.load(Ordering::SeqCst) == 1
                && harness.mock.presence.load(Ordering::SeqCst) >= 3
        })
        .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            harness.service.cloud.clear_session(),
        )
        .await
        .expect("bounded logout")
        .expect("logout");
        wait_until(|| {
            connection_state(&harness.service).state
                == fluxdown_protocol::CloudConnectionState::Disconnected
        })
        .await;
        harness.mock.snapshot_ready.notify_one();
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), worker)
            .await
            .expect("bounded cancellation")
            .expect("joined worker");
        assert!(harness.service.tasks().await.is_empty());
        let persisted = harness.store.load().await.expect("read persisted state");
        assert!(persisted.credentials.is_none());
        assert!(persisted.remote_tasks.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn missing_response_headers_timeout_and_manual_retry_wakes_backoff() {
        async fn no_headers(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
            state.events.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
            StatusCode::OK
        }
        let harness = Harness::new("headers_timeout", |router| {
            router.route("/api/v1/tasks/events", get(no_headers))
        })
        .await;
        // 本用例只测 SSE 响应头超时：预先进入 WebSocket 回退期，不探测 ws。
        *harness
            .service
            .ws_fallback_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(std::time::Instant::now() + super::WS_FALLBACK_STICKY);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| connection_state(&harness.service).last_error.is_some()).await;
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 1);
        assert_eq!(harness.mock.presence.load(Ordering::SeqCst), 0);
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Reconnecting
        );
        assert!(
            connection_state(&harness.service)
                .last_error
                .unwrap()
                .contains("headers timeout")
        );
        harness.service.request_reconnect();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while harness.mock.events.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("manual retry bypasses 5s backoff");
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), worker)
            .await
            .expect("cancel headers wait")
            .expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn missing_presence_connection_requests_resync_without_network_error() {
        async fn missing_presence() -> impl IntoResponse {
            (
                StatusCode::CONFLICT,
                axum::Json(json!({
                    "code": "presence_connection_missing",
                    "message": "Reconnect the task event stream"
                })),
            )
        }
        let harness = Harness::new("presence_missing", |router| {
            router.route("/api/v1/tasks/presence", post(missing_presence))
        })
        .await;
        let result = harness
            .service
            .heartbeat_loop(
                &tokio::sync::Notify::new(),
                harness.service.cloud.request_epoch(),
                None,
            )
            .await;
        assert!(matches!(result, Ok(SseEnd::Resync)));
        harness.finish().await;
    }

    #[tokio::test]
    async fn full_event_queue_backpressures_without_discarding_commands() {
        async fn burst() -> impl IntoResponse {
            let mut payload = "data: {\"type\":\"ignored\"}\n\n".repeat(super::SSE_QUEUE_CAPACITY);
            for action in ["pause", "resume", "delete"] {
                payload.push_str(&format!(
                    "data: {{\"type\":\"task.command\",\"action\":\"{action}\",\"taskId\":\"r1\",\"commandId\":\"{action}\",\"deleteFiles\":true}}\n\n"
                ));
            }
            let stream = futures_util::StreamExt::chain(
                futures_util::stream::once(async move {
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(payload))
                }),
                futures_util::stream::pending(),
            );
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(stream),
            )
        }
        let harness = Harness::new("queue_backpressure", |router| {
            router.route("/api/v1/tasks/events", get(burst))
        })
        .await;
        let response = harness
            .service
            .cloud
            .remote_events("device-1")
            .await
            .expect("SSE");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(super::SSE_QUEUE_CAPACITY);
        let capacity = sender.clone();
        let roster = tokio::sync::Notify::new();
        let stop_reading = CancellationToken::new();
        let mut reader = Box::pin(harness.service.read_stream(
            response,
            sender,
            &roster,
            harness.service.cloud.request_epoch(),
            &stop_reading,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::select! {
                result = &mut reader => panic!("queue overflow must not discard commands: {result:?}"),
                () = async {
                    while capacity.capacity() != 0 {
                        tokio::task::yield_now().await;
                    }
                } => {}
            }
        }).await.expect("queue filled");
        let max_queued_payload = receiver.len() * super::MAX_SSE_LINE_BYTES;
        assert_eq!(max_queued_payload, 16 * 1024 * 1024);
        // A failed heartbeat asks the reader to stop, but cannot drop its pending
        // send or the remaining commands from the chunk it has already received.
        stop_reading.cancel();
        let mut reader_ended = false;
        for index in 0..super::SSE_QUEUE_CAPACITY + 3 {
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        result = &mut reader, if !reader_ended => {
                            assert!(matches!(result, Ok(SseEnd::Resync)));
                            reader_ended = true;
                        }
                        line = receiver.recv() => break line.expect("queued event"),
                    }
                }
            })
            .await
            .expect("business queue drains");
            if index >= super::SSE_QUEUE_CAPACITY {
                let command: Value = serde_json::from_slice(&line).expect("command");
                let action = ["pause", "resume", "delete"][index - super::SSE_QUEUE_CAPACITY];
                assert_eq!(command["commandId"], action);
                assert_eq!(command["action"], action);
                assert_eq!(command["deleteFiles"], true);
            }
        }
        if !reader_ended {
            assert!(matches!(reader.as_mut().await, Ok(SseEnd::Resync)));
        }
        drop(reader);
        drop(capacity);
        drop(receiver);
        harness.finish().await;
    }

    #[tokio::test]
    async fn consume_drains_commands_before_resync_or_eof_from_the_same_chunk() {
        for ending in ["", "data: {\"type\":\"resync\"}\n\n"] {
            let mut payload = String::new();
            for action in ["pause", "resume", "delete"] {
                payload.push_str(&format!(
                    "data: {{\"type\":\"task.command\",\"action\":\"{action}\",\"taskId\":\"r1\",\"toDevice\":\"device-1\",\"commandId\":\"{action}\",\"deleteFiles\":true}}\n\n"
                ));
            }
            payload.push_str(ending);
            let harness =
                Harness::with_daemon("terminal_fifo", move |router| {
                    command_mock(router)
                    .route("/api/v1/tasks/events", get(move || {
                        let payload = payload.clone();
                        async move { ([(header::CONTENT_TYPE, "text/event-stream")], payload) }
                    }))
                    .route("/api/v1/tasks/presence", post(mock_presence))
                })
                .await;
            seed_daemon_tasks(&harness, vec![local_task("l1", 1)]);
            bind(&harness, "r1", "l1").await;
            let response = harness
                .service
                .cloud
                .remote_events("device-1")
                .await
                .expect("SSE");
            let cancel = CancellationToken::new();
            let (mut events, _) = harness.service.events.subscribe_and_snapshot();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                harness
                    .service
                    .consume_events(response, &cancel, &mut events),
            )
            .await
            .expect("accepted commands drain before reconnect");
            if ending.is_empty() {
                assert!(matches!(result, Err(RemoteError::Protocol(_))));
            } else {
                assert!(matches!(result, Ok(SseEnd::Resync)));
            }
            let calls = harness.daemon_calls.lock().await.clone();
            let methods: Vec<_> = calls.iter().map(|(method, _)| method.as_str()).collect();
            assert_eq!(
                methods,
                [
                    fluxdown_protocol::method::DAEMON_TASK_PAUSE,
                    fluxdown_protocol::method::DAEMON_TASK_RESUME,
                    fluxdown_protocol::method::DAEMON_TASK_DELETE,
                ]
            );
            assert_eq!(daemon_deletes(&harness).await, [("l1".to_owned(), true)]);
            harness.finish().await;
        }
    }

    fn ws_text(value: Value) -> axum::extract::ws::Message {
        axum::extract::ws::Message::text(value.to_string())
    }

    async fn mock_ws(
        State(state): State<Arc<RemoteMockState>>,
        headers: HeaderMap,
        axum::extract::Query(query): axum::extract::Query<
            std::collections::HashMap<String, String>,
        >,
        ws: axum::extract::ws::WebSocketUpgrade,
    ) -> axum::response::Response {
        state.ws_connects.fetch_add(1, Ordering::SeqCst);
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        state
            .ws_requests
            .lock()
            .await
            .push((bearer, query.get("deviceId").cloned()));
        let mode = *state
            .ws_mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ws.on_upgrade(move |socket| run_mock_socket(socket, state, mode))
    }

    /// 旧令牌 401，刷新后的令牌 `fresh` 才升级。
    async fn mock_ws_fresh_only(
        State(state): State<Arc<RemoteMockState>>,
        headers: HeaderMap,
        query: axum::extract::Query<std::collections::HashMap<String, String>>,
        ws: axum::extract::ws::WebSocketUpgrade,
    ) -> axum::response::Response {
        let fresh = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some("Bearer fresh");
        if !fresh {
            state.ws_unauthorized.fetch_add(1, Ordering::SeqCst);
            return StatusCode::UNAUTHORIZED.into_response();
        }
        mock_ws(State(state), headers, query, ws).await
    }

    async fn mock_ws_missing(State(state): State<Arc<RemoteMockState>>) -> StatusCode {
        state.ws_connects.fetch_add(1, Ordering::SeqCst);
        StatusCode::NOT_FOUND
    }

    async fn mock_refresh(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "accessToken": "fresh",
            "refreshToken": "fresh-refresh",
            "expiresIn": 3600,
            "user": {
                "id": "u1", "email": "user@example.com", "nickname": "User",
                "plan": "free", "status": "active", "createdAt": ""
            },
            "entitlements": {},
            "device": {
                "id": "row1", "deviceId": "device-1", "name": "Desktop",
                "createdAt": "", "lastSeenAt": ""
            }
        }))
    }

    /// 发完 Close 后与真实服务端一样等对端 Close 或短超时再丢弃 socket，避免提前 RST。
    async fn linger_until_peer_closes(socket: &mut axum::extract::ws::WebSocket) {
        let drained = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while let Some(Ok(_)) = socket.recv().await {}
        })
        .await;
        tracing::debug!(timed_out = drained.is_err(), "mock socket linger finished");
    }

    async fn mock_ws_hang(State(state): State<Arc<RemoteMockState>>) -> StatusCode {
        state.ws_connects.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<()>().await;
        StatusCode::OK
    }

    async fn run_mock_socket(
        mut socket: axum::extract::ws::WebSocket,
        state: Arc<RemoteMockState>,
        mode: WsMode,
    ) {
        use axum::extract::ws::Message as WsMessage;
        let mut opening = Vec::new();
        match mode {
            WsMode::KickedOnConnect => opening.push(ws_text(
                json!({"type": "presence.kicked", "deviceId": "device-1"}),
            )),
            WsMode::CommandsThenClose | WsMode::CommandsThenResync => {
                opening.push(WsMessage::Binary(vec![1, 2, 3].into()));
                for action in ["pause", "resume", "delete"] {
                    opening.push(ws_text(json!({
                        "type": "task.command", "action": action, "taskId": "r1",
                        "toDevice": "device-1", "commandId": action, "deleteFiles": true
                    })));
                }
                if mode == WsMode::CommandsThenResync {
                    opening.push(ws_text(json!({"type": "resync"})));
                }
                opening.push(WsMessage::Close(None));
            }
            WsMode::CloseBeforePong => opening.push(WsMessage::Close(None)),
            WsMode::Burst => {
                for index in 0..super::SSE_QUEUE_CAPACITY + 8 {
                    opening.push(ws_text(json!({
                        "type": "task.command",
                        "action": if index % 2 == 0 { "pause" } else { "resume" },
                        "taskId": "r1", "toDevice": "device-1",
                        "commandId": format!("burst-{index}")
                    })));
                }
            }
            _ => {}
        }
        for message in opening {
            if socket.send(message).await.is_err() {
                return;
            }
        }
        if matches!(
            mode,
            WsMode::CommandsThenClose | WsMode::CommandsThenResync | WsMode::CloseBeforePong
        ) {
            linger_until_peer_closes(&mut socket).await;
            return;
        }
        let mut pings_on_connection = 0_usize;
        loop {
            tokio::select! {
                () = state.ws_release.notified() => {
                    let presence = ws_text(json!({"type": "presence", "deviceId": "device-2", "online": true}));
                    if socket.send(presence).await.is_err() {
                        return;
                    }
                }
                message = socket.recv() => {
                    let Some(Ok(WsMessage::Text(text))) = message else {
                        if matches!(message, Some(Ok(_))) {
                            continue;
                        }
                        return;
                    };
                    let is_ping = serde_json::from_str::<Value>(text.as_str())
                        .is_ok_and(|frame| frame["type"] == "ping");
                    if !is_ping {
                        continue;
                    }
                    state.ws_pings.fetch_add(1, Ordering::SeqCst);
                    pings_on_connection += 1;
                    let pong = ws_text(json!({"type": "pong"}));
                    let (replies, finished) = match mode {
                        WsMode::Silent => (Vec::new(), false),
                        WsMode::LimitOnPing => (
                            vec![
                                ws_text(json!({"type": "error", "code": "online_device_limit", "message": "too many online devices"})),
                                WsMessage::Close(None),
                            ],
                            true,
                        ),
                        WsMode::MissingOnPing => (
                            vec![
                                ws_text(json!({"type": "error", "code": "presence_connection_missing", "message": "reconnect"})),
                                WsMessage::Close(None),
                            ],
                            true,
                        ),
                        WsMode::CloseAfterFirstPong if pings_on_connection == 1 => {
                            (vec![pong, WsMessage::Close(None)], true)
                        }
                        _ => (vec![pong], false),
                    };
                    for reply in replies {
                        if socket.send(reply).await.is_err() {
                            return;
                        }
                    }
                    if finished {
                        linger_until_peer_closes(&mut socket).await;
                        return;
                    }
                }
            }
        }
    }

    /// 除 `/tasks/ws` 外的共用路由；`events` / `presence` 是回退通道的探针，WebSocket 可用时不应被触碰。
    fn ws_probes(router: Router<Arc<RemoteMockState>>) -> Router<Arc<RemoteMockState>> {
        router
            .route("/api/v1/tasks/remote", get(mock_remote_snapshot))
            .route("/api/v1/devices", get(online_roster))
            .route("/api/v1/tasks/events", get(live_events))
            .route("/api/v1/tasks/presence", post(mock_presence))
    }

    fn ws_mock(router: Router<Arc<RemoteMockState>>) -> Router<Arc<RemoteMockState>> {
        ws_probes(router).route("/api/v1/tasks/ws", get(mock_ws))
    }

    fn set_ws_mode(harness: &Harness, mode: WsMode) {
        *harness
            .mock
            .ws_mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
    }

    async fn wait_connected(harness: &Harness) {
        wait_until(|| {
            connection_state(&harness.service).state
                == fluxdown_protocol::CloudConnectionState::Connected
        })
        .await;
    }

    #[tokio::test]
    async fn socket_channel_heartbeats_inside_the_connection_without_http_requests() {
        let harness = Harness::new("ws_healthy", ws_mock).await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_connected(&harness).await;
        // 心跳间隔 100ms：多个周期内全部走连接内的 ping / pong。
        wait_until(|| harness.mock.ws_pings.load(Ordering::SeqCst) >= 4).await;
        let mock = &harness.mock;
        assert_eq!(mock.ws_connects.load(Ordering::SeqCst), 1);
        assert_eq!(mock.events.load(Ordering::SeqCst), 0, "no SSE fallback");
        assert_eq!(mock.presence.load(Ordering::SeqCst), 0, "no HTTP heartbeat");
        assert_eq!(
            mock.rosters.load(Ordering::SeqCst),
            1,
            "roster is fetched once when presence is established, not per heartbeat"
        );
        assert_eq!(
            *mock.ws_requests.lock().await,
            [(
                Some("Bearer access".to_owned()),
                Some("device-1".to_owned())
            )]
        );
        // `presence` 事件帧才会再触发名册刷新。
        mock.ws_release.notify_one();
        wait_until(|| mock.rosters.load(Ordering::SeqCst) >= 2).await;
        assert_eq!(mock.presence.load(Ordering::SeqCst), 0);
        assert_eq!(mock.events.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness.service.ws_unusable_streak.load(Ordering::Relaxed),
            0
        );
        assert_eq!(mock.ws_connects.load(Ordering::SeqCst), 1);
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Connected
        );
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_online_device_limit_frame_waits_for_a_free_slot() {
        let harness = Harness::new("ws_limit", ws_mock).await;
        set_ws_mode(&harness, WsMode::LimitOnPing);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service).last_error_reason
                == Some(fluxdown_protocol::ErrorReason::OnlineDeviceLimit)
        })
        .await;
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Reconnecting
        );
        // 固定 30 秒等待，不指数退避；手动重连才提前重试。
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.mock.ws_connects.load(Ordering::SeqCst), 1);
        harness.service.request_reconnect();
        wait_until(|| harness.mock.ws_connects.load(Ordering::SeqCst) >= 2).await;
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 0);
        // 服务端 error 帧是有效应答，不计入「通道不可用」。
        assert_eq!(
            harness.service.ws_unusable_streak.load(Ordering::Relaxed),
            0
        );
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_presence_missing_frame_reconnects_quickly_without_error() {
        let harness = Harness::new("ws_missing", ws_mock).await;
        set_ws_mode(&harness, WsMode::MissingOnPing);
        let (mut frames, _) = harness.service.events.subscribe_and_snapshot();
        let started = std::time::Instant::now();
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        // RESYNC_PAUSE（1s）后自动重连，无需手动唤醒，更不能走 RETRY_DELAYS[0]（5s）。
        wait_until(|| harness.mock.ws_connects.load(Ordering::SeqCst) >= 2).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "reconnected after {:?}",
            started.elapsed()
        );
        // 窗口内从未投影过带错误的连接状态。
        let mut projected = 0_usize;
        loop {
            match frames.try_recv() {
                Ok(frame) => {
                    if let fluxdown_protocol::ServiceEvent::Agent(
                        AgentEvent::CloudConnectionChanged(connection),
                    ) = &frame.event
                    {
                        projected += 1;
                        assert_eq!(connection.last_error, None, "{connection:?}");
                        assert_eq!(connection.last_error_reason, None, "{connection:?}");
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
        assert!(projected >= 1, "connection states were projected");
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn expired_sse_fallback_reconnects_and_probes_websocket_again() {
        let harness = Harness::new("ws_fallback_expiry", ws_mock).await;
        // 注入很短的回退期：先走 SSE，到期后的下一次心跳成功即 Resync 并重新探测 WebSocket。
        *harness
            .service
            .ws_fallback_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(400));
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| harness.mock.events.load(Ordering::SeqCst) >= 1).await;
        assert_eq!(harness.mock.ws_connects.load(Ordering::SeqCst), 0);
        wait_until(|| harness.mock.ws_connects.load(Ordering::SeqCst) >= 1).await;
        wait_connected(&harness).await;
        wait_until(|| harness.mock.ws_pings.load(Ordering::SeqCst) >= 1).await;
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 1);
        assert!(!harness.service.ws_fallback_active());
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    /// 逐次等到第 `failures` 次「通道不可用」被记账后手动重连（避免退避等待），直到进入粘性回退。
    async fn drive_to_unusable_fallback(harness: &Harness) {
        for failure in 1..super::WS_UNUSABLE_THRESHOLD {
            wait_until(|| harness.mock.ws_connects.load(Ordering::SeqCst) >= failure).await;
            wait_until(|| harness.service.ws_unusable_streak.load(Ordering::Relaxed) >= failure)
                .await;
            harness.service.request_reconnect();
        }
        wait_until(|| {
            harness.mock.ws_connects.load(Ordering::SeqCst) >= super::WS_UNUSABLE_THRESHOLD
        })
        .await;
        wait_until(|| harness.service.ws_fallback_active()).await;
        assert_eq!(
            harness.service.ws_unusable_streak.load(Ordering::Relaxed),
            0,
            "entering fallback resets the streak"
        );
        harness.service.request_reconnect();
        wait_until(|| harness.mock.events.load(Ordering::SeqCst) >= 1).await;
        assert_eq!(
            harness.mock.ws_connects.load(Ordering::SeqCst),
            super::WS_UNUSABLE_THRESHOLD,
            "sticky fallback stops probing WebSocket"
        );
    }

    #[tokio::test]
    async fn repeated_socket_sessions_ending_before_a_pong_fall_back_to_sse() {
        let harness = Harness::new("ws_unusable_close", ws_mock).await;
        set_ws_mode(&harness, WsMode::CloseBeforePong);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        drive_to_unusable_fallback(&harness).await;
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn repeated_handshake_header_timeouts_fall_back_to_sse() {
        let mut harness = Harness::new("ws_unusable_timeout", |router| {
            ws_probes(router).route("/api/v1/tasks/ws", get(mock_ws_hang))
        })
        .await;
        Arc::get_mut(&mut harness.service)
            .expect("unshared service")
            .sse_connect_timeout = std::time::Duration::from_millis(300);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        drive_to_unusable_fallback(&harness).await;
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn unusable_streak_needs_consecutive_failures_and_resets_on_pong() {
        let harness = Harness::new("ws_streak_unit", |router| router).await;
        let service = &harness.service;
        service.note_ws_unusable("test");
        service.note_ws_unusable("test");
        assert!(!service.ws_fallback_active());
        // 收到 pong 把连续计数清零（read_socket 里的同一操作）。
        service.ws_unusable_streak.store(0, Ordering::Relaxed);
        service.note_ws_unusable("test");
        service.note_ws_unusable("test");
        assert!(!service.ws_fallback_active());
        service.note_ws_unusable("test");
        assert!(service.ws_fallback_active());
        assert_eq!(service.ws_unusable_streak.load(Ordering::Relaxed), 0);
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_backpressure_keeps_every_command_without_tripping_the_idle_watchdog() {
        let total = super::SSE_QUEUE_CAPACITY + 8;
        // 云端快照里要有这条任务，否则业务循环的首次快照对账会清掉绑定、后续命令就无处执行。
        async fn snapshot_with_task() -> impl IntoResponse {
            axum::Json(json!({"tasks": [{
                "id": "r1", "toDevice": "device-1", "fromDevice": "device-2", "status": "downloading"
            }]}))
        }
        let mut harness = Harness::with_daemon("ws_burst", |router| {
            command_mock(router)
                .route("/api/v1/tasks/ws", get(mock_ws))
                .route("/api/v1/tasks/remote", get(snapshot_with_task))
                .route("/api/v1/devices", get(online_roster))
        })
        .await;
        Arc::get_mut(&mut harness.service)
            .expect("unshared service")
            .sse_idle_timeout = std::time::Duration::from_millis(300);
        set_ws_mode(&harness, WsMode::Burst);
        seed_daemon_tasks(&harness, vec![local_task("l1", 1)]);
        bind(&harness, "r1", "l1").await;
        let socket = harness
            .service
            .cloud
            .remote_socket("device-1")
            .await
            .expect("WebSocket handshake");
        let cancel = CancellationToken::new();
        let (mut events, _) = harness.service.events.subscribe_and_snapshot();
        let controls = |calls: &[(String, Option<Value>)]| {
            calls
                .iter()
                .filter(|(method, _)| {
                    method == fluxdown_protocol::method::DAEMON_TASK_PAUSE
                        || method == fluxdown_protocol::method::DAEMON_TASK_RESUME
                })
                .map(|(method, _)| method.clone())
                .collect::<Vec<_>>()
        };
        {
            let consume = harness.service.consume_events_epoch(
                super::EventChannel::Socket(Box::new(socket)),
                &cancel,
                &mut events,
                harness.service.cloud.request_epoch(),
            );
            tokio::pin!(consume);
            let all_applied = async {
                loop {
                    if controls(&harness.daemon_calls.lock().await).len() >= total {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            };
            tokio::select! {
                result = &mut consume => panic!("channel ended while draining the burst: {result:?}"),
                applied = tokio::time::timeout(std::time::Duration::from_secs(5), all_applied) => {
                    applied.expect("every command is applied");
                }
            }
            // 突发之后只剩 pong：空闲看门狗（300ms）不能被背压期间的停读触发。
            tokio::select! {
                result = &mut consume => panic!("idle watchdog fired after the burst: {result:?}"),
                () = tokio::time::sleep(std::time::Duration::from_millis(900)) => {}
            }
            cancel.cancel();
            assert!(matches!(consume.await, Ok(SseEnd::Cancelled)));
        }
        let methods = controls(&harness.daemon_calls.lock().await);
        let expected: Vec<String> = (0..total)
            .map(|index| {
                if index % 2 == 0 {
                    fluxdown_protocol::method::DAEMON_TASK_PAUSE
                } else {
                    fluxdown_protocol::method::DAEMON_TASK_RESUME
                }
                .to_owned()
            })
            .collect();
        assert_eq!(methods, expected, "FIFO order, nothing dropped");
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_presence_kicked_frame_waits_for_a_free_slot() {
        let harness = Harness::new("ws_kicked", ws_mock).await;
        set_ws_mode(&harness, WsMode::KickedOnConnect);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service).last_error_reason
                == Some(fluxdown_protocol::ErrorReason::OnlineDeviceLimit)
        })
        .await;
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Reconnecting
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.mock.ws_connects.load(Ordering::SeqCst), 1);
        harness.service.request_reconnect();
        wait_until(|| harness.mock.ws_connects.load(Ordering::SeqCst) >= 2).await;
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_handshake_401_refreshes_once_and_replays() {
        let harness = Harness::new("ws_refresh", |router| {
            ws_probes(router)
                .route("/api/v1/tasks/ws", get(mock_ws_fresh_only))
                .route("/api/v1/auth/refresh", post(mock_refresh))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_connected(&harness).await;
        let mock = &harness.mock;
        assert_eq!(mock.ws_unauthorized.load(Ordering::SeqCst), 1);
        assert_eq!(mock.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(mock.ws_connects.load(Ordering::SeqCst), 1);
        assert_eq!(
            mock.ws_requests.lock().await[0].0.as_deref(),
            Some("Bearer fresh")
        );
        assert_eq!(mock.events.load(Ordering::SeqCst), 0);
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_404_falls_back_to_sse_stickily_without_per_heartbeat_roster_fetches() {
        let harness = Harness::new("ws_fallback", |router| {
            ws_probes(router).route("/api/v1/tasks/ws", get(mock_ws_missing))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_connected(&harness).await;
        // SSE 回退的 HTTP 心跳多轮成功，名册仍只在 presence 建立时拉了一次。
        wait_until(|| harness.mock.presence.load(Ordering::SeqCst) >= 4).await;
        let mock = &harness.mock;
        assert_eq!(mock.ws_connects.load(Ordering::SeqCst), 1);
        assert_eq!(mock.events.load(Ordering::SeqCst), 1);
        assert_eq!(mock.rosters.load(Ordering::SeqCst), 1);
        // 下一次重连直接走 SSE，不再探测 WebSocket。
        harness.service.request_reconnect();
        wait_until(|| mock.events.load(Ordering::SeqCst) >= 2).await;
        assert_eq!(mock.ws_connects.load(Ordering::SeqCst), 1);
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_server_close_reconnects() {
        let harness = Harness::new("ws_close", ws_mock).await;
        set_ws_mode(&harness, WsMode::CloseAfterFirstPong);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service)
                .last_error
                .is_some_and(|error| error.contains("closed"))
        })
        .await;
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Reconnecting
        );
        harness.service.request_reconnect();
        wait_until(|| harness.mock.ws_connects.load(Ordering::SeqCst) >= 2).await;
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 0);
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn silent_socket_hits_the_idle_watchdog() {
        let harness = Harness::with_idle_timeout(
            "ws_idle",
            Some(std::time::Duration::from_millis(400)),
            ws_mock,
        )
        .await;
        set_ws_mode(&harness, WsMode::Silent);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service)
                .last_error
                .is_some_and(|error| error.contains("idle timeout"))
        })
        .await;
        assert!(harness.mock.ws_pings.load(Ordering::SeqCst) >= 1);
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn socket_consume_drains_commands_before_resync_or_close() {
        for mode in [WsMode::CommandsThenClose, WsMode::CommandsThenResync] {
            let harness =
                Harness::with_daemon("ws_terminal_fifo", |router| command_mock(ws_mock(router)))
                    .await;
            set_ws_mode(&harness, mode);
            seed_daemon_tasks(&harness, vec![local_task("l1", 1)]);
            bind(&harness, "r1", "l1").await;
            let socket = harness
                .service
                .cloud
                .remote_socket("device-1")
                .await
                .expect("WebSocket handshake");
            let cancel = CancellationToken::new();
            let (mut events, _) = harness.service.events.subscribe_and_snapshot();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                harness.service.consume_events_epoch(
                    super::EventChannel::Socket(Box::new(socket)),
                    &cancel,
                    &mut events,
                    harness.service.cloud.request_epoch(),
                ),
            )
            .await
            .expect("accepted commands drain before reconnect");
            if mode == WsMode::CommandsThenClose {
                assert!(matches!(result, Err(RemoteError::Protocol(_))));
            } else {
                assert!(matches!(result, Ok(SseEnd::Resync)));
            }
            let calls = harness.daemon_calls.lock().await.clone();
            let methods: Vec<_> = calls.iter().map(|(method, _)| method.as_str()).collect();
            assert_eq!(
                methods,
                [
                    fluxdown_protocol::method::DAEMON_TASK_PAUSE,
                    fluxdown_protocol::method::DAEMON_TASK_RESUME,
                    fluxdown_protocol::method::DAEMON_TASK_DELETE,
                ]
            );
            assert_eq!(daemon_deletes(&harness).await, [("l1".to_owned(), true)]);
            harness.finish().await;
        }
    }

    #[test]
    fn socket_error_frames_map_to_the_http_heartbeat_outcomes() {
        let limit = super::socket_error_end(
            &json!({"type": "error", "code": "online_device_limit", "message": "full"}),
        )
        .expect_err("limit is an error");
        assert!(limit.is_online_limit());
        let RemoteError::Cloud(cloud) = &limit else {
            panic!("online limit must be a cloud error");
        };
        assert_eq!(cloud.status, Some(403));
        assert_eq!(cloud.message, "full");
        assert!(matches!(
            super::socket_error_end(
                &json!({"type": "error", "code": "presence_connection_missing"})
            ),
            Ok(SseEnd::Resync)
        ));
        assert!(matches!(
            super::socket_error_end(&json!({"type": "error", "code": "boom"})),
            Err(RemoteError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn heartbeat_failure_tears_down_sse_and_projects_retryable_error() {
        async fn rejected_presence(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
            let round = state.presence.fetch_add(1, Ordering::SeqCst);
            if round == 0 {
                StatusCode::NO_CONTENT
            } else {
                state.presence_ready.notified().await;
                StatusCode::CONFLICT
            }
        }
        let harness = Harness::new("heartbeat_failure", |router| {
            router
                .route("/api/v1/tasks/events", get(live_events))
                .route("/api/v1/tasks/presence", post(rejected_presence))
                .route("/api/v1/tasks/remote", get(mock_remote_snapshot))
                .route("/api/v1/devices", get(online_roster))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service).state
                == fluxdown_protocol::CloudConnectionState::Connected
        })
        .await;
        harness.mock.presence_ready.notify_one();
        wait_until(|| {
            connection_state(&harness.service).state
                == fluxdown_protocol::CloudConnectionState::Reconnecting
        })
        .await;
        assert_eq!(
            connection_state(&harness.service).last_error_reason,
            Some(fluxdown_protocol::ErrorReason::CloudUnreachable)
        );
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn online_device_limit_waits_without_error_escalation_and_retries_on_request() {
        async fn full(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
            state.events.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::FORBIDDEN,
                axum::Json(json!({
                    "code": "online_device_limit",
                    "message": "too many online devices"
                })),
            )
        }
        let harness = Harness::new("online_limit", |router| {
            router.route("/api/v1/tasks/events", get(full))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service).last_error_reason
                == Some(fluxdown_protocol::ErrorReason::OnlineDeviceLimit)
        })
        .await;
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Reconnecting
        );
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 1);
        // 受限等待是固定 30 秒；手动重连立即再试一次，且重试期间不闪回空原因。
        harness.service.request_reconnect();
        wait_until(|| harness.mock.events.load(Ordering::SeqCst) >= 2).await;
        wait_until(|| {
            connection_state(&harness.service).last_error_reason
                == Some(fluxdown_protocol::ErrorReason::OnlineDeviceLimit)
        })
        .await;
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Reconnecting
        );
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn presence_kicked_for_this_device_drops_the_stream_and_waits_for_a_free_slot() {
        async fn kicked_events(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
            state.events.fetch_add(1, Ordering::SeqCst);
            let first =
                futures_util::stream::once(std::future::ready(Ok::<_, std::convert::Infallible>(
                    axum::body::Bytes::from_static(
                        b"data: {\"type\":\"presence.kicked\",\"deviceId\":\"device-1\"}\n\n",
                    ),
                )));
            let body = Body::from_stream(futures_util::StreamExt::chain(
                first,
                futures_util::stream::pending(),
            ));
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                body,
            )
        }
        let harness = Harness::new("kicked", |router| {
            router
                .route("/api/v1/tasks/events", get(kicked_events))
                .route("/api/v1/tasks/presence", post(mock_presence))
                .route("/api/v1/tasks/remote", get(mock_remote_snapshot))
                .route("/api/v1/devices", get(online_roster))
        })
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service).last_error_reason
                == Some(fluxdown_protocol::ErrorReason::OnlineDeviceLimit)
        })
        .await;
        assert_eq!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Reconnecting
        );
        // 被踢后不立刻抢回名额：进入 30 秒等待，只有手动重连才提前重试。
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 1);
        harness.service.request_reconnect();
        wait_until(|| harness.mock.events.load(Ordering::SeqCst) >= 2).await;
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn presence_kicked_event_only_ends_the_stream_for_the_targeted_device() {
        let harness = Harness::new("kicked_target", command_mock).await;
        let other = harness
            .service
            .apply_remote_event(json!({"type": "presence.kicked", "deviceId": "device-2"}))
            .await;
        assert!(matches!(other, Ok(None)));
        let own = harness
            .service
            .apply_remote_event(json!({"type": "presence.kicked", "deviceId": "device-1"}))
            .await;
        assert!(matches!(own, Ok(Some(SseEnd::Kicked))));
        harness.finish().await;
    }

    #[tokio::test]
    async fn initial_roster_failure_retries_without_another_presence_event() {
        async fn flaky_roster(
            State(state): State<Arc<RemoteMockState>>,
        ) -> axum::response::Response {
            if state.rosters.fetch_add(1, Ordering::SeqCst) < 1 {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            axum::Json(json!({"devices": [{"id": "d1", "deviceId": "device-1", "isCurrent": true, "isOnline": true}]})).into_response()
        }
        let mut harness = Harness::new("roster_retry", |router| {
            router
                .route("/api/v1/tasks/events", get(live_events))
                .route("/api/v1/tasks/presence", post(mock_presence))
                .route("/api/v1/tasks/remote", get(mock_remote_snapshot))
                .route("/api/v1/devices", get(flaky_roster))
        })
        .await;
        Arc::get_mut(&mut harness.service)
            .expect("unshared service")
            .heartbeat_interval = std::time::Duration::from_secs(60);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| harness.mock.rosters.load(Ordering::SeqCst) >= 1).await;
        assert_ne!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Connected
        );
        wait_until(|| {
            connection_state(&harness.service).state
                == fluxdown_protocol::CloudConnectionState::Connected
        })
        .await;
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 1);
        assert_eq!(harness.mock.presence.load(Ordering::SeqCst), 1);
        let fluxdown_protocol::SnapshotBody::Agent(snapshot) =
            harness.service.events.snapshot().body
        else {
            panic!("agent snapshot");
        };
        assert!(snapshot.cloud_devices[0].is_online);
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn worker_revocation_cancels_the_stream_and_finishes_persisting_logout() {
        async fn revoked_events() -> impl IntoResponse {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"type\":\"session.revoked\",\"deviceId\":\"device-1\"}\n\n",
            )
        }
        let harness = Harness::new("worker_revocation", |router| {
            router.route("/api/v1/tasks/events", get(revoked_events))
        })
        .await;
        let (mut events, _) = harness.service.events.subscribe_and_snapshot();
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let frame = events.recv().await.expect("agent event");
                if matches!(frame.event, fluxdown_protocol::ServiceEvent::Agent(AgentEvent::SessionChanged(ref session)) if session.is_none()) {
                    break;
                }
            }
        }).await.expect("revocation disconnects");
        assert!(harness.state.lock().await.credentials.is_none());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while harness
                .store
                .load()
                .await
                .expect("persisted logout")
                .credentials
                .is_some()
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("logout persisted after cancelling revoked scope");
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn idle_watchdog_still_expires_while_snapshot_business_is_blocked() {
        async fn blocked_snapshot(State(state): State<Arc<RemoteMockState>>) -> impl IntoResponse {
            state.snapshots.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
            StatusCode::OK
        }
        let harness = Harness::with_idle_timeout(
            "blocked_business_idle",
            Some(std::time::Duration::from_millis(500)),
            |router| {
                router
                    .route("/api/v1/tasks/events", get(live_events))
                    .route("/api/v1/tasks/remote", get(blocked_snapshot))
                    .route("/api/v1/tasks/presence", post(mock_presence))
                    .route("/api/v1/devices", get(online_roster))
            },
        )
        .await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        wait_until(|| {
            connection_state(&harness.service)
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("idle timeout"))
        })
        .await;
        assert_eq!(harness.mock.snapshots.load(Ordering::SeqCst), 1);
        assert!(harness.mock.presence.load(Ordering::SeqCst) > 1);
        cancel.cancel();
        worker.await.expect("joined worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn silent_sse_hits_the_idle_watchdog_even_while_periodic_timers_fire() {
        // 空闲阈值大于 1s 的进度上报周期：旧实现每个 tick 都会重建超时而永不触发。
        let harness = Harness::with_idle_timeout(
            "idle",
            Some(std::time::Duration::from_millis(1500)),
            |router| {
                router
                    .route("/api/v1/tasks/events", get(silent_events))
                    .route("/api/v1/tasks/presence", post(mock_presence))
            },
        )
        .await;
        let service = harness.service.clone();
        let response = service
            .cloud
            .remote_events("device-1")
            .await
            .expect("connect silent SSE");
        let cancel = CancellationToken::new();
        let (mut events, _) = service.events.subscribe_and_snapshot();
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            service.consume_events(response, &cancel, &mut events),
        )
        .await
        .expect("watchdog must end the stream before the test deadline");
        match outcome {
            Err(RemoteError::Protocol(message)) => assert!(message.contains("idle timeout")),
            other => panic!("expected idle timeout, got {other:?}"),
        }
        assert!(started.elapsed() >= std::time::Duration::from_millis(1400));
        drop(service);
        harness.finish().await;
    }

    #[tokio::test]
    async fn resync_event_ends_the_stream_for_a_full_reload() {
        async fn resync_events() -> impl IntoResponse {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"type\":\"resync\"}\n\n",
            )
        }
        let harness = Harness::new("resync", |router| {
            router.route("/api/v1/tasks/events", get(resync_events))
        })
        .await;
        let response = harness
            .service
            .cloud
            .remote_events("device-1")
            .await
            .expect("connect SSE");
        let cancel = CancellationToken::new();
        let (mut events, _) = harness.service.events.subscribe_and_snapshot();
        let end = harness
            .service
            .consume_events(response, &cancel, &mut events)
            .await
            .expect("resync is a clean end");
        assert_eq!(end, SseEnd::Resync);
        harness.finish().await;
    }

    #[tokio::test]
    async fn malformed_sse_lines_do_not_tear_down_the_stream() {
        async fn noisy_events() -> impl IntoResponse {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: not-json\n\ndata: {\"type\":\"task.status\",\"nope\":1}\n\ndata: {\"type\":\"resync\"}\n\n",
            )
        }
        let harness = Harness::new("noisy", |router| {
            router.route("/api/v1/tasks/events", get(noisy_events))
        })
        .await;
        let response = harness
            .service
            .cloud
            .remote_events("device-1")
            .await
            .expect("connect SSE");
        let cancel = CancellationToken::new();
        let (mut events, _) = harness.service.events.subscribe_and_snapshot();
        let end = harness
            .service
            .consume_events(response, &cancel, &mut events)
            .await
            .expect("bad events are skipped");
        assert_eq!(end, SseEnd::Resync);
        harness.finish().await;
    }

    /// 依次取出已发布的 agent 事件（只保留会话相关的，便于断言顺序）。
    fn session_events(
        receiver: &mut tokio::sync::broadcast::Receiver<fluxdown_protocol::EventFrame>,
    ) -> Vec<String> {
        let mut seen = Vec::new();
        while let Ok(frame) = receiver.try_recv() {
            if let fluxdown_protocol::ServiceEvent::Agent(event) = frame.event {
                match event {
                    fluxdown_protocol::AgentEvent::SessionRevoked(reason) => {
                        seen.push(format!("revoked:{reason:?}"));
                    }
                    fluxdown_protocol::AgentEvent::SessionChanged(session) => {
                        seen.push(format!("session:{}", session.is_some()));
                    }
                    _ => {}
                }
            }
        }
        seen
    }

    #[tokio::test]
    async fn session_revoked_for_this_device_notifies_device_untrusted_before_the_session_clears() {
        let harness = Harness::new("revoked_local", command_mock).await;
        let (mut receiver, _) = harness.service.events.subscribe_and_snapshot();
        harness
            .service
            .apply_remote_event(json!({"type": "session.revoked", "deviceId": "device-1"}))
            .await
            .expect("handle session.revoked");
        assert_eq!(
            session_events(&mut receiver),
            ["revoked:DeviceUntrusted", "session:false"]
        );
        assert!(harness.state.lock().await.credentials.is_none());
        harness.finish().await;
    }

    #[tokio::test]
    async fn session_revoked_with_password_changed_reason_notifies_password_changed() {
        let harness = Harness::new("revoked_password", command_mock).await;
        let (mut receiver, _) = harness.service.events.subscribe_and_snapshot();
        harness
            .service
            .apply_remote_event(json!({
                "type": "session.revoked",
                "deviceId": "someone-else",
                "reason": "passwordChanged"
            }))
            .await
            .expect("foreign device revoked");
        assert!(session_events(&mut receiver).is_empty());
        assert!(harness.state.lock().await.credentials.is_some());
        harness
            .service
            .apply_remote_event(json!({"type": "session.revoked", "reason": "passwordChanged"}))
            .await
            .expect("revoke all");
        assert_eq!(
            session_events(&mut receiver),
            ["revoked:PasswordChanged", "session:false"]
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn session_revoked_without_a_device_means_session_expired_and_other_devices_are_ignored()
    {
        let harness = Harness::new("revoked_all", command_mock).await;
        let (mut receiver, _) = harness.service.events.subscribe_and_snapshot();
        harness
            .service
            .apply_remote_event(json!({"type": "session.revoked", "deviceId": "someone-else"}))
            .await
            .expect("foreign device revoked");
        assert!(session_events(&mut receiver).is_empty());
        assert!(harness.state.lock().await.credentials.is_some());
        harness
            .service
            .apply_remote_event(json!({"type": "session.revoked"}))
            .await
            .expect("revoke all");
        assert_eq!(
            session_events(&mut receiver),
            ["revoked:SessionExpired", "session:false"]
        );
        // 已经登出后再收到撤销：不重复通知。
        harness
            .service
            .apply_remote_event(json!({"type": "session.revoked"}))
            .await
            .expect("revoke again");
        assert!(!session_events(&mut receiver).contains(&"revoked:SessionExpired".to_owned()));
        harness.finish().await;
    }

    fn command_mock(router: Router<Arc<RemoteMockState>>) -> Router<Arc<RemoteMockState>> {
        router
            .route("/api/v1/tasks/{id}/command", post(mock_command))
            .route("/api/v1/tasks/{id}/status", post(mock_status))
    }

    async fn seed_remote_task(harness: &Harness, status: &str) {
        harness
            .state
            .lock()
            .await
            .remote_tasks
            .push(remote_task(json!({
                "id": "r1", "toDevice": "device-2", "fromDevice": "device-1", "status": status
            })));
    }

    #[tokio::test]
    async fn repeating_the_same_action_reaches_the_cloud_every_time_but_a_reused_command_id_only_once()
     {
        let harness = Harness::new("commands", command_mock).await;
        seed_remote_task(&harness, "downloading").await;
        let pause = |command_id: Option<&str>| RemoteCommandParams {
            task_id: "r1".to_owned(),
            action: RemoteCommandAction::Pause,
            command_id: command_id.map(str::to_owned),
            delete_files: false,
        };
        // pause → pause：缺省 commandId 时每次都是新命令。
        harness
            .service
            .command(pause(None))
            .await
            .expect("first pause");
        harness
            .service
            .command(pause(None))
            .await
            .expect("second pause");
        assert_eq!(harness.mock.commands.lock().await.len(), 2);
        let commands = harness.mock.commands.lock().await.clone();
        assert_ne!(commands[0]["commandId"], commands[1]["commandId"]);
        assert_eq!(commands[0]["action"], "pause");

        // 显式 commandId 幂等：同一个 id 只下发一次。
        harness
            .service
            .command(pause(Some("fixed")))
            .await
            .expect("fixed once");
        harness
            .service
            .command(pause(Some("fixed")))
            .await
            .expect("fixed again");
        assert_eq!(harness.mock.commands.lock().await.len(), 3);
        harness.finish().await;
    }

    fn local_task(task_id: &str, status: i32) -> TaskDto {
        serde_json::from_value(json!({
            "taskId": task_id, "url": "https://example.com/a", "fileName": "a.bin",
            "saveDir": "/tmp", "status": status, "downloadedBytes": 0, "totalBytes": 0,
            "errorMessage": "", "createdAt": "1", "proxyUrl": "", "queueId": "main", "checksum": ""
        }))
        .expect("local task")
    }

    fn seed_daemon_tasks(harness: &Harness, tasks: Vec<TaskDto>) {
        harness
            .service
            .events
            .replace_daemon_snapshot(fluxdown_protocol::DaemonSnapshot {
                tasks,
                ..Default::default()
            });
        harness
            .service
            .events
            .publish(AgentEvent::DaemonConnectionChanged(true));
    }

    async fn bind(harness: &Harness, remote_id: &str, local_id: &str) {
        harness
            .state
            .lock()
            .await
            .remote_bindings
            .insert(remote_id.to_owned(), local_id.to_owned());
    }

    async fn bound_remote_ids(harness: &Harness) -> Vec<String> {
        let mut ids = harness
            .state
            .lock()
            .await
            .remote_bindings
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    /// daemon 收到的删除调用：(本机任务 id, deleteFiles)，按任务 id 排序。
    async fn daemon_deletes(harness: &Harness) -> Vec<(String, bool)> {
        let mut deletes = harness
            .daemon_calls
            .lock()
            .await
            .iter()
            .filter(|(method, _)| method == fluxdown_protocol::method::DAEMON_TASK_DELETE)
            .map(|(_, params)| {
                let params = params.clone().unwrap_or_default();
                (
                    params["taskId"].as_str().unwrap_or_default().to_owned(),
                    params["deleteFiles"].as_bool().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        deletes.sort();
        deletes
    }

    fn delete_params(command_id: Option<&str>) -> RemoteCommandParams {
        RemoteCommandParams {
            task_id: "r1".to_owned(),
            action: RemoteCommandAction::Delete,
            command_id: command_id.map(str::to_owned),
            delete_files: true,
        }
    }

    async fn not_found() -> impl IntoResponse {
        (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"code": "not_found", "message": "gone"})),
        )
    }

    #[tokio::test]
    async fn delete_goes_through_the_cloud_command_for_any_status_and_drops_the_row_at_once() {
        let harness = Harness::new("delete", command_mock).await;
        for status in ["completed", "pending"] {
            seed_remote_task(&harness, status).await;
            harness
                .service
                .command(delete_params(None))
                .await
                .expect(status);
            assert!(harness.service.tasks().await.is_empty(), "{status}");
        }
        let commands = harness.mock.commands.lock().await.clone();
        assert_eq!(commands.len(), 2);
        assert!(
            commands
                .iter()
                .all(|command| command["action"] == "delete" && command["deleteFiles"] == true)
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn deleting_a_record_another_device_already_removed_still_succeeds() {
        let harness = Harness::new("delete_gone", |router| {
            router.route("/api/v1/tasks/{id}/command", post(not_found))
        })
        .await;
        seed_remote_task(&harness, "downloading").await;
        let pause = harness
            .service
            .command(RemoteCommandParams {
                action: RemoteCommandAction::Pause,
                delete_files: false,
                ..delete_params(None)
            })
            .await;
        assert!(matches!(pause, Err(RemoteError::Cloud(_))));
        harness
            .service
            .command(delete_params(None))
            .await
            .expect("an already deleted record counts as deleted");
        assert!(harness.service.tasks().await.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn deleting_a_task_this_device_runs_honours_delete_files_and_ignores_the_echoed_command()
    {
        let harness = Harness::with_daemon("local_delete", command_mock).await;
        seed_daemon_tasks(&harness, vec![local_task("l1", 1)]);
        harness
            .state
            .lock()
            .await
            .remote_tasks
            .push(remote_task(json!({
                "id": "r1", "toDevice": "device-1", "fromDevice": "device-2", "status": "downloading"
            })));
        bind(&harness, "r1", "l1").await;

        harness
            .service
            .command(delete_params(Some("c1")))
            .await
            .expect("delete own task");
        assert_eq!(daemon_deletes(&harness).await, [("l1".to_owned(), true)]);
        let commands = harness.mock.commands.lock().await.clone();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["action"], "delete");
        let statuses = harness.mock.statuses.lock().await.clone();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].1["status"], "canceled");
        assert!(harness.service.tasks().await.is_empty());
        assert!(bound_remote_ids(&harness).await.is_empty());

        // 云端把同一条指令回显给本机：去重，不再触碰 daemon。
        harness
            .service
            .apply_remote_event(json!({
                "type": "task.command", "taskId": "r1", "toDevice": "device-1",
                "action": "delete", "commandId": "c1", "deleteFiles": true
            }))
            .await
            .expect("echoed command");
        assert_eq!(daemon_deletes(&harness).await.len(), 1);
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_cancel_for_a_task_not_yet_accepted_succeeds_without_touching_the_daemon() {
        let harness = Harness::with_daemon("pending_cancel", command_mock).await;
        harness
            .state
            .lock()
            .await
            .remote_tasks
            .push(remote_task(json!({
                "id": "r1", "toDevice": "device-1", "fromDevice": "device-2",
                "url": "https://example.com/a", "status": "pending"
            })));
        seed_daemon_tasks(&harness, vec![local_task("unrelated", 1)]);
        harness
            .service
            .apply_remote_event(json!({
                "type": "task.command", "taskId": "r1", "toDevice": "device-1", "action": "cancel"
            }))
            .await
            .expect("nothing to cancel locally");
        assert!(harness.daemon_calls.lock().await.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_task_this_device_runs_is_removed_locally_when_the_cloud_cancels_or_deletes_it() {
        let harness = Harness::with_daemon("executor_events", command_mock).await;
        seed_daemon_tasks(
            &harness,
            vec![
                local_task("l1", 1),
                local_task("l2", 2),
                local_task("l3", 3),
            ],
        );
        bind(&harness, "r1", "l1").await;
        bind(&harness, "r2", "l2").await;
        bind(&harness, "r3", "l3").await;

        for event in [
            json!({"type": "task.status", "id": "r1", "toDevice": "device-1",
                   "fromDevice": "device-2", "status": "canceled"}),
            json!({"type": "task.removed", "taskId": "r2"}),
            // 本机已下载完成的任务被删除记录：文件与本机任务都保留。
            json!({"type": "task.removed", "taskId": "r3"}),
        ] {
            harness
                .service
                .apply_remote_event(event)
                .await
                .expect("apply event");
        }
        assert_eq!(
            daemon_deletes(&harness).await,
            [("l1".to_owned(), false), ("l2".to_owned(), false)]
        );
        assert!(bound_remote_ids(&harness).await.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_status_report_rejected_with_404_removes_the_local_task() {
        let harness = Harness::with_daemon("report_gone", |router| {
            router.route("/api/v1/tasks/{id}/status", post(not_found))
        })
        .await;
        seed_daemon_tasks(&harness, vec![local_task("l1", 1)]);
        bind(&harness, "r1", "l1").await;
        harness
            .service
            .report_status_if_changed("r1", RemoteTaskStatus::Downloading, None, None)
            .await
            .expect("a deleted record is not a report failure");
        assert_eq!(daemon_deletes(&harness).await, [("l1".to_owned(), false)]);
        assert!(bound_remote_ids(&harness).await.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn reconnect_snapshot_removes_local_tasks_canceled_or_deleted_while_offline() {
        async fn snapshot() -> impl IntoResponse {
            axum::Json(json!({"tasks": [
                {"id": "r-canceled", "toDevice": "device-1", "fromDevice": "device-2", "status": "canceled"},
                {"id": "r-live", "toDevice": "device-1", "fromDevice": "device-2", "status": "downloading"}
            ]}))
        }
        let harness = Harness::with_daemon("executor_snapshot", |router| {
            router.route("/api/v1/tasks/remote", get(snapshot))
        })
        .await;
        seed_daemon_tasks(
            &harness,
            vec![
                local_task("l1", 1),
                local_task("l2", 1),
                local_task("l3", 1),
                local_task("l4", 0),
            ],
        );
        {
            let mut state = harness.state.lock().await;
            for id in ["r-deleted", "r-fresh"] {
                state.remote_tasks.push(remote_task(json!({
                    "id": id, "toDevice": "device-1", "fromDevice": "device-2", "status": "downloading"
                })));
            }
        }
        bind(&harness, "r-canceled", "l1").await;
        bind(&harness, "r-deleted", "l2").await;
        bind(&harness, "r-live", "l3").await;
        bind(&harness, "r-fresh", "l4").await;
        // r-fresh 在快照请求发出之后才下发并接单：快照里没有它不代表记录被删除。
        harness.service.runtime.lock().await.bound_at.insert(
            "r-fresh".to_owned(),
            std::time::Instant::now() + std::time::Duration::from_secs(3600),
        );

        let merged = harness
            .service
            .refresh_snapshot()
            .await
            .expect("refresh snapshot");
        assert_eq!(
            daemon_deletes(&harness).await,
            [("l1".to_owned(), false), ("l2".to_owned(), false)]
        );
        assert_eq!(bound_remote_ids(&harness).await, ["r-fresh", "r-live"]);
        let mut ids = merged
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, ["r-canceled", "r-fresh", "r-live"]);
        harness.finish().await;
    }

    #[tokio::test]
    async fn cloud_receive_uses_preference_category_route_only_without_explicit_directory() {
        let harness = Harness::with_daemon("receive_category_route", command_mock).await;
        harness
            .service
            .events
            .publish(AgentEvent::PreferencesChanged(
                fluxdown_protocol::AgentPreferencesDto {
                    revision: 1,
                    values: [(
                        fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY.to_owned(),
                        json!([
                            {
                                "id": "binary",
                                "name": "Binary",
                                "extensions": ["bin"],
                                "saveDir": "/category/bin"
                            }
                        ]),
                    )]
                    .into(),
                },
            ));
        let task = remote_task(json!({
            "id": "r-category",
            "url": "https://example.com/file.bin",
            "fileName": "file.bin",
            "status": "pending"
        }));
        let epoch = harness.service.cloud.request_epoch();

        assert!(
            harness
                .service
                .create_local_task(&task, None, epoch)
                .await
                .is_err()
        );
        let explicit_dir = harness.dir.join("explicit").to_string_lossy().into_owned();
        assert!(
            harness
                .service
                .create_local_task(&task, Some(explicit_dir), epoch)
                .await
                .is_err()
        );
        let calls = harness.daemon_calls.lock().await;
        let create_params = calls
            .iter()
            .filter(|(method, _)| method == fluxdown_protocol::method::DAEMON_TASK_CREATE)
            .map(|(_, params)| {
                serde_json::from_value::<fluxdown_protocol::DaemonCreateTaskParams>(
                    params.clone().expect("task creation params"),
                )
                .expect("decode task creation params")
            })
            .collect::<Vec<_>>();
        assert_eq!(create_params.len(), 2);
        assert!(create_params[0].category_route.is_some());
        assert!(create_params[1].category_route.is_none());
        drop(calls);
        harness.finish().await;
    }

    #[tokio::test]
    async fn unknown_status_tasks_cannot_be_controlled() {
        let harness = Harness::new("unknown_status", command_mock).await;
        seed_remote_task(&harness, "brand-new-status").await;
        let result = harness
            .service
            .command(RemoteCommandParams {
                task_id: "r1".to_owned(),
                action: RemoteCommandAction::Pause,
                command_id: None,
                delete_files: false,
            })
            .await;
        assert!(matches!(result, Err(RemoteError::TaskUnavailable(_))));
        assert!(harness.mock.commands.lock().await.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn accept_failure_is_reported_to_the_cloud_as_failed_with_the_error() {
        let harness = Harness::new("accept_failure", command_mock).await;
        let task = remote_task(json!({
            "id": "r1", "toDevice": "device-1", "fromDevice": "device-2",
            "url": "https://example.com/a.bin", "status": "pending"
        }));
        harness
            .service
            .report_accept_failure(&task, "save directory `/x` is unavailable: denied")
            .await;
        let statuses = harness.mock.statuses.lock().await.clone();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].0, "r1");
        assert_eq!(statuses[0].1["status"], "failed");
        assert!(
            statuses[0].1["error"]
                .as_str()
                .is_some_and(|error| error.contains("unavailable"))
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn an_unavailable_daemon_leaves_the_dispatch_pending_instead_of_failing_it() {
        let harness = Harness::new("accept_transient", command_mock).await;
        let task = remote_task(json!({
            "id": "r1", "toDevice": "device-1", "fromDevice": "device-2",
            "url": "https://example.com/a.bin", "status": "pending"
        }));
        harness.state.lock().await.remote_tasks.push(task);
        // 测试里的 daemon 客户端永远未连接。
        harness.service.accept_pending_dispatches().await;
        assert!(
            harness.mock.statuses.lock().await.is_empty(),
            "no failed report for a transient error"
        );
        assert!(harness.state.lock().await.remote_bindings.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_persisted_binding_prevents_accepting_the_same_task_twice() {
        let harness = Harness::new("binding", command_mock).await;
        let task = remote_task(json!({
            "id": "r1", "toDevice": "device-1", "fromDevice": "device-2",
            "url": "https://example.com/a.bin", "status": "pending"
        }));
        {
            let mut state = harness.state.lock().await;
            state.remote_tasks.push(task);
            state
                .remote_bindings
                .insert("r1".to_owned(), "local-1".to_owned());
        }
        harness.service.accept_pending_dispatches().await;
        assert!(
            harness.mock.statuses.lock().await.is_empty(),
            "an already accepted task must not be created or reported again"
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn dispatch_rejects_the_local_device_and_foreign_style_directories_before_calling_the_cloud()
     {
        let harness = Harness::new("dispatch", command_mock).await;
        let to_self = harness
            .service
            .dispatch(RemoteDispatchParams {
                to_device: "device-1".to_owned(),
                url: "https://example.com/a".to_owned(),
                file_name: None,
                save_dir: None,
            })
            .await;
        assert!(matches!(
            to_self,
            Err(RemoteError::InvalidArgument {
                field: "toDevice",
                ..
            })
        ));

        harness
            .service
            .events
            .publish(fluxdown_protocol::AgentEvent::CloudDevicesChanged(vec![
                device(json!({"id": "1", "deviceId": "win", "platform": "windows"})),
            ]));
        let wrong_style = harness
            .service
            .dispatch(RemoteDispatchParams {
                to_device: "win".to_owned(),
                url: "https://example.com/a".to_owned(),
                file_name: None,
                save_dir: Some("/mnt/data".to_owned()),
            })
            .await;
        assert!(matches!(
            wrong_style,
            Err(RemoteError::InvalidArgument {
                field: "saveDir",
                reason: Some(_),
                ..
            })
        ));
        harness.finish().await;
    }

    #[tokio::test]
    async fn slow_daemon_response_does_not_block_logout_or_commit_to_replacement() {
        let harness = Harness::with_daemon("slow_daemon_epoch", |router| router).await;
        let epoch = harness.service.cloud.request_epoch();
        let replacement = harness.state.lock().await.credentials.clone();
        harness
            .state
            .lock()
            .await
            .remote_bindings
            .insert("a-task".to_owned(), "local-a".to_owned());
        // The recording daemon must acquire this mutex before replying.
        let response_barrier = harness.daemon_calls.lock().await;
        {
            let command = harness.service.execute_local_command(
                IncomingCommand {
                    task_id: "a-task".to_owned(),
                    action: RemoteCommandAction::Pause,
                    command_id: "a-command".to_owned(),
                    delete_files: false,
                },
                epoch,
            );
            tokio::pin!(command);
            assert!(futures_util::poll!(&mut command).is_pending());
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                harness.service.cloud.clear_session(),
            )
            .await
            .expect("logout must not wait for daemon response")
            .expect("logout");
            {
                let mut state = harness.state.lock().await;
                state.credentials = replacement;
                state
                    .remote_bindings
                    .insert("b-task".to_owned(), "local-b".to_owned());
            }
            drop(response_barrier);
            assert!(
                command.await.is_err(),
                "old completion cannot commit command dedup"
            );
        }
        assert!(
            !harness
                .service
                .runtime
                .lock()
                .await
                .confirmed_commands
                .contains("a-command")
        );
        assert!(
            harness
                .state
                .lock()
                .await
                .remote_bindings
                .contains_key("b-task")
        );
        assert_eq!(
            harness.daemon_calls.lock().await.len(),
            1,
            "already-issued local command may finish"
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn stale_scope_rejects_both_streams_before_http() {
        let harness = Harness::new("stale_stream_http", |router| {
            router
                .route("/api/v1/tasks/events", get(live_events))
                .route("/api/v1/sync/events", get(live_events))
        })
        .await;
        let epoch = harness.service.cloud.request_epoch();
        let replacement = harness.state.lock().await.credentials.clone();
        harness
            .service
            .cloud
            .clear_session()
            .await
            .expect("logout A");
        harness.state.lock().await.credentials = replacement;
        let old = harness.service.cloud.at_epoch(epoch);
        assert!(old.remote_events("device-1").await.is_err());
        assert!(old.sync_events("device-1").await.is_err());
        assert_eq!(harness.mock.events.load(Ordering::SeqCst), 0);
        harness.finish().await;
    }

    #[tokio::test]
    async fn stale_business_scope_and_post_commit_children_cannot_touch_replacement() {
        let harness = Harness::with_daemon("stale_business", |router| {
            router
                .route("/api/v1/tasks/remote", get(mock_remote_snapshot))
                .route("/api/v1/devices", get(online_roster))
                .route("/api/v1/tasks/{id}/status", post(mock_status))
        })
        .await;
        let epoch = harness.service.cloud.request_epoch();
        let replacement = harness.state.lock().await.credentials.clone();
        harness
            .service
            .cloud
            .clear_session()
            .await
            .expect("logout A");
        let pending = remote_task(json!({
            "id":"b-pending", "toDevice":"device-1", "status":"pending",
            "url":"https://example.com/b", "fileName":"b.bin"
        }));
        {
            let mut state = harness.state.lock().await;
            state.credentials = replacement;
            state.remote_tasks = vec![pending.clone()];
            state
                .remote_bindings
                .insert("b-bound".to_owned(), "local-b".to_owned());
        }
        let (_sender, lines) = tokio::sync::mpsc::channel(1);
        let roster = tokio::sync::Notify::new();
        let presence_ready = tokio::sync::Notify::new();
        presence_ready.notify_one();
        let transport_alive = std::sync::atomic::AtomicBool::new(true);
        let business =
            harness
                .service
                .business_loop(lines, &roster, &presence_ready, epoch, &transport_alive);
        // Exercise the stale worker itself, not its outer select/cancellation guard.
        match tokio::time::timeout(std::time::Duration::from_millis(500), business).await {
            Ok(Err(_)) | Err(_) => {}
            Ok(Ok(end)) => panic!("unexpected business exit: {end:?}"),
        }
        harness.service.rebuild_bindings(epoch).await;
        harness.service.accept_pending_dispatches_epoch(epoch).await;
        harness
            .service
            .resolve_status_conflict_epoch("b-bound", epoch)
            .await;
        harness
            .service
            .discard_local_task_epoch("b-bound", epoch)
            .await;
        assert!(
            harness
                .service
                .create_local_task(&pending, None, epoch)
                .await
                .is_err()
        );
        assert!(
            harness
                .service
                .report_local_progress_epoch(epoch)
                .await
                .is_err()
                || harness.mock.statuses.lock().await.is_empty()
        );
        assert_eq!(harness.mock.rosters.load(Ordering::SeqCst), 0);
        assert_eq!(harness.mock.snapshots.load(Ordering::SeqCst), 0);
        assert!(harness.mock.statuses.lock().await.is_empty());
        assert!(harness.daemon_calls.lock().await.is_empty());
        assert_ne!(
            connection_state(&harness.service).state,
            fluxdown_protocol::CloudConnectionState::Connected
        );
        let state = harness.state.lock().await;
        assert_eq!(state.remote_tasks[0].id, "b-pending");
        assert_eq!(
            state.remote_bindings.get("b-bound").map(String::as_str),
            Some("local-b")
        );
        drop(state);
        harness.finish().await;
    }
}
