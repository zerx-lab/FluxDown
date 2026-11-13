//! GPUI composition root 唯一 agent JSON-RPC 会话。

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fluxdown_protocol::method;
use fluxdown_protocol::{
    AgentEvent, ApplicationErrorCode, CLOSE_REASON_SERVICE_QUIT, DaemonEvent, EventFrame,
    FrameVerdict, QueueOrderEntryDto, RequestId, RpcErrorData, RpcNotification, RpcRequest,
    RpcResponse, ServiceEvent, ServiceHello, ServiceRole, Snapshot, SnapshotBody, TablePage,
    TableSync, TaskDto, judge_frame,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::preference_writes::PreferenceWrites;
use crate::service_bootstrap::{
    Blocked, EnsureOutcome, ListenerProbe, PROBE_TIMEOUT, ServiceBootstrap, connect_listener,
};
use crate::service_failure::ServiceFailure;

pub type AgentFuture<T> = Pin<Box<dyn Future<Output = Result<T, RpcErrorData>> + Send + 'static>>;

/// 握手期（订阅到拿到快照之间）缓冲的事件帧上限。大批量导入时这段窗口里可以积压上千帧；
/// 超限才放弃本次握手重连。
const HANDSHAKE_EVENT_BUFFER_LIMIT: usize = 65_536;

/// 任务表分页载入里单页请求的超时：每收到一页即重新计时，所以载入总时长随表大小自然伸缩
/// （23 万任务约 115 页，不会被握手的 15s 总时限误杀），只有「一页迟迟不来」才判定失败并
/// 整体重连重新载入。
const TABLE_PAGE_TIMEOUT: Duration = Duration::from_secs(30);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub enum AgentClientEvent {
    /// 连接 / 重连后的全量快照（带 `epoch`/`sequence` 游标）。
    Snapshot(Box<Snapshot>),
    Event(Box<EventFrame>),
    Stale,
    /// 本进程刚拉起了 agent（后台服务冷启动）：首个快照要等 agent 装配完成。
    ServiceStarting,
    Fatal(RpcErrorData),
    /// agent 执行了完全退出（`system.shutdown`）：不再重连，界面随之退出。
    ServiceStopped,
    /// agent 无法被拉起或反复启动即退出：不再空转拉起，界面据此展示原因。成功连接后才会再次
    /// 报告新的失败。
    ServiceFailed(ServiceFailure),
    /// 分页载入完成：任务表 / 队列位置表已全部到齐，会话据此装入投影并广播完整快照。
    /// 载入期间的后续帧由连接循环缓冲，本事件之后才按序回放。
    TablesLoaded(Box<TablesLoaded>),
}

/// [`AgentClientEvent::TablesLoaded`] 载荷：页是基准帧 `sequence` 之后某时刻的状态，装入后由
/// 其后回放的帧收敛。
pub struct TablesLoaded {
    pub epoch: String,
    /// 基准帧序号（载入触发时的投影游标）。
    pub sequence: u64,
    /// `Some` = 任务表已载入（按 id 升序）。
    pub tasks: Option<Vec<TaskDto>>,
    /// `Some` = 队列位置表已载入（按 `order` 升序）。
    pub queue: Option<Vec<QueueOrderEntryDto>>,
}

/// 一轮在途的表分页载入（握手后，或连接中收到 `reloads_tables()` 帧）。
struct TableLoad {
    sync: TableSync,
    /// 在途页请求的 id；`None` = 尚未发出。
    request: Option<i64>,
    /// 在途页请求的超时时刻。
    deadline: tokio::time::Instant,
}

impl TableLoad {
    fn new(sync: TableSync) -> Self {
        Self {
            sync,
            request: None,
            deadline: tokio::time::Instant::now() + TABLE_PAGE_TIMEOUT,
        }
    }
}

#[derive(Clone)]
pub struct AgentClientConfig {
    pub rpc_url: Option<String>,
    pub bearer_path: PathBuf,
}

struct ClientCommand {
    method: String,
    params: Option<Value>,
    ack: oneshot::Sender<Result<Value, RpcErrorData>>,
}

impl ClientCommand {
    fn is_submission(&self) -> bool {
        match self.method.as_str() {
            method::DAEMON_TASK_CREATE
            | method::DAEMON_TASK_CREATE_MANY
            | method::DAEMON_TASK_SEED_LOCAL
            | method::DAEMON_GROUP_CREATE
            | method::AGENT_CAPTURE_CREATE_GROUP
            | method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE => true,
            method::AGENT_CAPTURE_RESOLVE => {
                self.params
                    .as_ref()
                    .and_then(|params| params.get("accepted"))
                    .and_then(Value::as_bool)
                    == Some(true)
            }
            _ => false,
        }
    }
}

/// Unsent creates survive reconnect without becoming ambiguous requests.
struct SubmissionTransport {
    gate: Arc<crate::downloads_port::SubmissionGate>,
    unsent: VecDeque<ClientCommand>,
}

impl SubmissionTransport {
    fn new(gate: Arc<crate::downloads_port::SubmissionGate>) -> Self {
        Self {
            gate,
            unsent: VecDeque::new(),
        }
    }
}

struct PendingCommand {
    ack: oneshot::Sender<Result<Value, RpcErrorData>>,
    _admission: Option<tokio::sync::OwnedMutexGuard<()>>,
}

pub struct AgentClient {
    commands: mpsc::Sender<ClientCommand>,
    runtime: Arc<tokio::runtime::Runtime>,
    bootstrap: Arc<ServiceBootstrap>,
    pub(crate) submissions: Arc<crate::downloads_port::SubmissionGate>,
}

impl AgentClient {
    pub fn start(
        config: AgentClientConfig,
        bootstrap: Arc<ServiceBootstrap>,
    ) -> Result<(Arc<Self>, mpsc::Receiver<AgentClientEvent>), AgentClientError> {
        if let Some(url) = config.rpc_url.as_deref() {
            validate_url(url)?;
        }
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("fluxdown-agent-client")
                .build()
                .map_err(AgentClientError::Runtime)?,
        );
        let (commands, command_rx) = mpsc::channel(64);
        let (events, event_rx) = mpsc::channel(1024);
        let submissions = Arc::new(crate::downloads_port::SubmissionGate::default());
        runtime.spawn(run_client(
            config,
            bootstrap.clone(),
            command_rx,
            events,
            submissions.clone(),
        ));
        Ok((
            Arc::new(Self {
                commands,
                runtime,
                bootstrap,
                submissions,
            }),
            event_rx,
        ))
    }

    /// 界面发起完全退出后调用：连接断开时不再拉起新的 agent。
    pub fn stop_service_bootstrap(&self) {
        self.bootstrap.stop();
    }

    pub fn call<P, R>(&self, method_name: &str, params: Option<P>) -> AgentFuture<R>
    where
        P: Serialize + Send + 'static,
        R: DeserializeOwned + Send + 'static,
    {
        let commands = self.commands.clone();
        let method_name = method_name.to_owned();
        Box::pin(async move {
            let params = params
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| internal_error())?;
            let (ack, response) = oneshot::channel();
            commands
                .send(ClientCommand {
                    method: method_name,
                    params,
                    ack,
                })
                .await
                .map_err(|_| unavailable_error())?;
            let value = response.await.map_err(|_| unavailable_error())??;
            serde_json::from_value(value).map_err(|_| internal_error())
        })
    }

    /// 在客户端自有 tokio 运行时上运行后台任务（单实例激活监听等）。
    pub fn spawn_background<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.runtime.spawn(future);
    }
}

async fn run_client(
    config: AgentClientConfig,
    bootstrap: Arc<ServiceBootstrap>,
    mut commands: mpsc::Receiver<ClientCommand>,
    events: mpsc::Sender<AgentClientEvent>,
    submissions: Arc<crate::downloads_port::SubmissionGate>,
) {
    let mut attempt = 0_usize;
    // 每个桌面进程只尝试替换一次协议不兼容的 agent，避免同目录二进制错配时反复互杀。
    let mut replaced_incompatible = false;
    // 同理只替换一次「应用版本旧于本桌面」的 agent：例如 macOS App Translocation 只读副本里
    // 残留的旧版 agent，被 /Applications 里的新桌面复用会显示错误的更新状态、无法拉起界面。
    let mut replaced_outdated = false;
    let mut link_log = ConnectionLog::default();
    let mut submission_transport = SubmissionTransport::new(submissions.clone());
    let mut connect_timeout = ConnectTimeout::new();
    let mut no_bearer = NoBearerGrace::default();
    // 同一失败期只通知一次，成功连接后复位。
    let mut failure_reported = false;
    loop {
        submissions.set_ready(false);
        let rpc_url = match crate::agent_endpoint::discover_rpc_url(
            config.rpc_url.as_deref(),
            &config.bearer_path,
        )
        .await
        {
            Ok(url) => url,
            Err(error) => {
                log::error!("could not discover local agent endpoint: {error}");
                if events
                    .send(AgentClientEvent::Fatal(internal_error()))
                    .await
                    .is_err()
                {
                    log::debug!("agent event receiver closed during desktop shutdown");
                }
                return;
            }
        };
        let result = connect(
            &config,
            &rpc_url,
            connect_timeout.current(),
            HANDSHAKE_TIMEOUT,
            !replaced_outdated,
        )
        .await;
        connect_timeout.observe(result.as_ref().err());
        if !matches!(result, Err(ConnectError::NoBearer)) {
            no_bearer.reset();
        }
        match result {
            Ok((socket, snapshot, buffered)) => {
                attempt = 0;
                failure_reported = false;
                link_log.connected();
                bootstrap.note_connected().await;
                let cursor = (snapshot.epoch.clone(), snapshot.sequence);
                let (preferences_revision, tables) = match &snapshot.body {
                    SnapshotBody::Agent(body) => (
                        body.preferences.revision,
                        TableSync::for_snapshot(
                            snapshot.epoch.clone(),
                            snapshot.sequence,
                            &body.daemon,
                        ),
                    ),
                    SnapshotBody::Daemon(_) => (0, None),
                };
                if events
                    .send(AgentClientEvent::Snapshot(Box::new(snapshot)))
                    .await
                    .is_err()
                {
                    return;
                }
                match run_connected(
                    socket,
                    &mut commands,
                    &events,
                    Bootstrap {
                        cursor,
                        buffered,
                        tables,
                    },
                    PreferenceWrites::new(preferences_revision),
                    &mut submission_transport,
                )
                .await
                {
                    Ok(SessionEnd::ClientDropped) => return,
                    Ok(SessionEnd::ServiceQuit) => {
                        log::info!("agent closed the connection with service-quit (full shutdown)");
                        bootstrap.stop();
                        if events.send(AgentClientEvent::ServiceStopped).await.is_err() {
                            log::debug!("agent event receiver closed during desktop shutdown");
                        }
                        return;
                    }
                    Err(()) => {
                        log::warn!("agent connection lost; reconnecting");
                        if events.send(AgentClientEvent::Stale).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Err(
                error
                @ (ConnectError::NoBearer | ConnectError::Refused | ConnectError::TimedOut(_)),
            ) => {
                // 尚无 bearer 时端口可能已被另一桌面进程 / 直接启动的 agent / 其他用户的 agent
                // 监听：宽限内先探测、有人监听就不拉起；宽限后无条件拉起。被拒与超时都按「可能
                // 无人监听」走冷启动路径（Windows 对未监听回环端口约 2s 才返回拒绝）。
                let probe_listener = matches!(error, ConnectError::NoBearer)
                    && no_bearer.within_grace(Instant::now());
                link_log.failed(&error);
                if spawn_agent_if_needed(
                    &bootstrap,
                    &config,
                    &rpc_url,
                    probe_listener,
                    &mut failure_reported,
                    &events,
                )
                .await
                .is_err()
                {
                    return;
                }
            }
            Err(error @ ConnectError::Unauthorized) => {
                // 监听者不是本用户的 agent：拉起本用户的 agent（端口被占用时它回退到系统分配
                // 端口并重新发布端点），下一轮重读端点与 bearer 即可连上。
                link_log.failed(&error);
                if spawn_agent_if_needed(
                    &bootstrap,
                    &config,
                    &rpc_url,
                    false,
                    &mut failure_reported,
                    &events,
                )
                .await
                .is_err()
                    || events.send(AgentClientEvent::Stale).await.is_err()
                {
                    return;
                }
            }
            Err(ConnectError::Outdated(version)) => {
                replaced_outdated = true;
                log::warn!(
                    "running agent is v{version}, older than this desktop (v{}); asking it to shut down",
                    fluxdown_protocol::APP_VERSION
                );
                if request_shutdown(&config, &rpc_url).await {
                    crate::service_bootstrap::wait_until_stopped(&rpc_url, Duration::from_secs(30))
                        .await;
                }
                // 关停失败也照常重连：下一轮不再比较版本，沿用正在运行的 agent。
                attempt = 0;
                continue;
            }
            Err(ConnectError::Incompatible) if !replaced_incompatible => {
                replaced_incompatible = true;
                log::warn!("running agent speaks an incompatible protocol; asking it to shut down");
                if request_shutdown(&config, &rpc_url).await {
                    // 旧 agent 关停 daemon 后退出；连接被拒时由 bootstrap 拉起同级新版本。
                    crate::service_bootstrap::wait_until_stopped(&rpc_url, Duration::from_secs(30))
                        .await;
                    attempt = 0;
                    continue;
                }
                log::error!("incompatible agent refused to shut down; giving up");
                if events
                    .send(AgentClientEvent::Fatal(protocol_error()))
                    .await
                    .is_err()
                {
                    log::debug!("agent event receiver closed during desktop shutdown");
                }
                return;
            }
            Err(ConnectError::Incompatible) => {
                log::error!("agent protocol still incompatible after replacement; giving up");
                if events
                    .send(AgentClientEvent::Fatal(protocol_error()))
                    .await
                    .is_err()
                {
                    log::debug!("agent event receiver closed during desktop shutdown");
                }
                return;
            }
            Err(ConnectError::Fatal(error)) => {
                log::error!("fatal agent connection error: {:?}", error.code);
                if events.send(AgentClientEvent::Fatal(error)).await.is_err() {
                    log::debug!("agent event receiver closed during desktop shutdown");
                }
                return;
            }
            Err(error @ ConnectError::Transient(_)) => {
                link_log.failed(&error);
                if events.send(AgentClientEvent::Stale).await.is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(retry_delay(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

/// 必要时拉起 agent，并把结果转成事件；`Err(())` 表示事件接收端已关闭（桌面退出中）。
async fn spawn_agent_if_needed(
    bootstrap: &ServiceBootstrap,
    config: &AgentClientConfig,
    rpc_url: &str,
    probe_listener: bool,
    failure_reported: &mut bool,
    events: &mpsc::Sender<AgentClientEvent>,
) -> Result<(), ()> {
    let event = match bootstrap.ensure_running(rpc_url, probe_listener).await {
        Ok(EnsureOutcome::Spawned) => AgentClientEvent::ServiceStarting,
        Ok(EnsureOutcome::NotNeeded) => return Ok(()),
        Ok(EnsureOutcome::Blocked(blocked)) => {
            if std::mem::replace(failure_reported, true) {
                return Ok(());
            }
            AgentClientEvent::ServiceFailed(service_failure(blocked, &config.bearer_path).await)
        }
        Err(error) => {
            log::warn!("could not start fluxdown-agent: {error:#}");
            AgentClientEvent::Stale
        }
    };
    events.send(event).await.map_err(|_| ())
}

async fn service_failure(blocked: Blocked, bearer_path: &Path) -> ServiceFailure {
    match blocked {
        Blocked::Launch { program, detail } => {
            log::error!("fluxdown-agent could not be launched ({program}): {detail}");
            ServiceFailure::Launch { program, detail }
        }
        Blocked::CrashLoop {
            status,
            spawned_at_ms,
        } => {
            let startup =
                crate::service_failure::read_startup_failure(bearer_path, spawned_at_ms).await;
            log::error!(
                "fluxdown-agent keeps exiting right after launch (last: {status}); startup record: {startup:?}"
            );
            ServiceFailure::Exited { status, startup }
        }
    }
}

/// 连接诊断：只记状态迁移（失败原因变化、恢复连接）与按 2 的幂递增的持续失败提醒，
/// 不逐次记录 100ms 一次的快速重试。
#[derive(Default)]
struct ConnectionLog {
    failures: u64,
    since: Option<Instant>,
    last_reason: String,
}

impl ConnectionLog {
    fn failed(&mut self, error: &ConnectError) {
        self.failures = self.failures.saturating_add(1);
        let since = *self.since.get_or_insert_with(Instant::now);
        let reason = error.describe();
        // 冷启动期间「尚无 bearer / 端口未监听」是预期状态，只作 info。
        let expected = matches!(
            error,
            ConnectError::NoBearer | ConnectError::Refused | ConnectError::TimedOut(_)
        );
        if reason != self.last_reason {
            if expected {
                log::info!("agent not reachable yet: {reason}");
            } else {
                log::warn!("agent connection attempt failed: {reason}");
            }
            self.last_reason = reason;
        } else if self.failures >= 16 && self.failures.is_power_of_two() {
            log::warn!(
                "agent still unreachable after {} attempts over {}s: {reason}",
                self.failures,
                since.elapsed().as_secs()
            );
        }
    }

    fn connected(&mut self) {
        match self.since.take() {
            Some(since) => log::info!(
                "connected to agent after {} failed attempts ({}ms)",
                self.failures,
                since.elapsed().as_millis()
            ),
            None => log::info!("connected to agent"),
        }
        self.failures = 0;
        self.last_reason.clear();
    }
}

/// 连续快速重试的次数（100ms 间隔，约 5s）：覆盖回环重连、agent 冷启动与重启，让界面在
/// 连接宽限内恢复而不显示连接态；之后指数退避，避免后台长期不可用时空转。
const FAST_RETRY_ATTEMPTS: usize = 50;
const FAST_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const BACKOFF_SECS: [u64; 5] = [1, 2, 5, 15, 30];

/// 第 `attempt` 次（从 0 起）连接失败后的等待时间。
fn retry_delay(attempt: usize) -> Duration {
    match attempt.checked_sub(FAST_RETRY_ATTEMPTS) {
        None => FAST_RETRY_INTERVAL,
        Some(slow) => Duration::from_secs(BACKOFF_SECS[slow.min(BACKOFF_SECS.len() - 1)]),
    }
}

/// 握手总超时：WS 升级、`system.hello`、`system.snapshot` 整体的上限。agent 已 bind 但尚未开始
/// serve、或已挂死时，没有它桌面会永久停在「正在连接」。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// 真正建连的超时上限：连续超时时逐轮加倍，封顶于此。
const CONNECT_TIMEOUT_MAX: Duration = Duration::from_secs(5);
/// 无 bearer 但端口有人监听时，不拉起新 agent 的宽限：监听者可能是本用户 agent 的初始化期，
/// 也可能是其他用户的 agent；超过宽限仍无 bearer 就拉起（本用户 agent 已在时新进程会因数据
/// 目录锁立即以成功码退出，无害）。
const NO_BEARER_LISTENER_GRACE: Duration = Duration::from_secs(3);

/// 真正建连的超时：从 [`PROBE_TIMEOUT`] 起，连续超时逐轮加倍并封顶；被拒或连上即复位。
struct ConnectTimeout {
    current: Duration,
}

impl ConnectTimeout {
    fn new() -> Self {
        Self {
            current: PROBE_TIMEOUT,
        }
    }

    fn current(&self) -> Duration {
        self.current
    }

    /// 记录一轮连接的失败（成功为 `None`）。没读到 bearer 的轮次没有触及网络，不改变超时。
    fn observe(&mut self, failure: Option<&ConnectError>) {
        match failure {
            Some(ConnectError::TimedOut(_)) => {
                self.current = self.current.saturating_mul(2).min(CONNECT_TIMEOUT_MAX);
            }
            Some(ConnectError::NoBearer) => {}
            _ => self.current = PROBE_TIMEOUT,
        }
    }
}

/// 连续「无 bearer」的时间：决定是否仍在不拉起的宽限内。
#[derive(Default)]
struct NoBearerGrace {
    since: Option<Instant>,
}

impl NoBearerGrace {
    /// 本轮仍在宽限内（应先探测监听者，有人监听就不拉起）。
    fn within_grace(&mut self, now: Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) < NO_BEARER_LISTENER_GRACE
    }

    fn reset(&mut self) {
        self.since = None;
    }
}

/// 握手前 `system.shutdown`：只有支持该首帧的 agent（协议 v4 起）会受理。
async fn request_shutdown(config: &AgentClientConfig, rpc_url: &str) -> bool {
    let Ok((request, stream)) = dial(config, rpc_url, CONNECT_TIMEOUT_MAX).await else {
        return false;
    };
    let call = async {
        let mut socket = upgrade(request, stream).await?;
        let mut buffered = Vec::new();
        call_on_socket(&mut socket, 1, method::SYSTEM_SHUTDOWN, None, &mut buffered).await
    };
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, call).await {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => {
            log::warn!("shutdown request to the agent failed: {}", error.describe());
            false
        }
        Err(_) => {
            log::warn!("shutdown request to the agent timed out after {HANDSHAKE_TIMEOUT:?}");
            false
        }
    }
}

/// 读 bearer 并完成 TCP 连接（超时 `connect_timeout`）。先自行建连：回环端口无人监听时按超时
/// 判定，不等 Windows 约 2s 的拒绝。
async fn dial(
    config: &AgentClientConfig,
    rpc_url: &str,
    connect_timeout: Duration,
) -> Result<(Request, TcpStream), ConnectError> {
    let bearer = tokio::fs::read_to_string(&config.bearer_path)
        .await
        .map_err(|_| ConnectError::NoBearer)?;
    let bearer = bearer.trim();
    if bearer.is_empty() {
        return Err(ConnectError::NoBearer);
    }
    let mut request = rpc_url
        .into_client_request()
        .map_err(|_| ConnectError::Fatal(protocol_error()))?;
    let authorization = HeaderValue::from_str(&format!("Bearer {bearer}"))
        .map_err(|_| ConnectError::Fatal(protocol_error()))?;
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, authorization);
    let stream = match connect_listener(rpc_url, connect_timeout).await {
        Ok(ListenerProbe::Listening(stream)) => stream,
        Ok(ListenerProbe::Refused) => return Err(ConnectError::Refused),
        Ok(ListenerProbe::TimedOut) => return Err(ConnectError::TimedOut(connect_timeout)),
        Err(error) => return Err(ConnectError::Transient(format!("connect: {error}"))),
    };
    Ok((request, stream))
}

async fn upgrade(request: Request, stream: TcpStream) -> Result<Socket, ConnectError> {
    // 单帧上限 [`MAX_INBOUND_RPC_MESSAGE_BYTES`](32 MiB)：快照与替换事件只带头，任务表按页
    // （约 1.6 MB）拉取，仍保留余量给大页与插件 / 配置等大字段。
    let limit = Some(fluxdown_protocol::MAX_INBOUND_RPC_MESSAGE_BYTES);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_frame_size(limit)
        .max_message_size(limit);
    let (socket, _) = tokio_tungstenite::client_async_with_config(
        request,
        MaybeTlsStream::Plain(stream),
        Some(config),
    )
    .await
    .map_err(classify_connect_error)?;
    Ok(socket)
}

async fn connect(
    config: &AgentClientConfig,
    rpc_url: &str,
    connect_timeout: Duration,
    handshake_timeout: Duration,
    reject_outdated: bool,
) -> Result<(Socket, Snapshot, Vec<EventFrame>), ConnectError> {
    let (request, stream) = dial(config, rpc_url, connect_timeout).await?;
    match tokio::time::timeout(
        handshake_timeout,
        handshake(request, stream, reject_outdated),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ConnectError::Transient(format!(
            "handshake timed out after {handshake_timeout:?} (agent accepted the connection but did not complete the handshake)"
        ))),
    }
}

/// WS 升级 + `system.hello` + `system.snapshot`；总时长由调用方限制。
async fn handshake(
    request: Request,
    stream: TcpStream,
    reject_outdated: bool,
) -> Result<(Socket, Snapshot, Vec<EventFrame>), ConnectError> {
    let mut socket = upgrade(request, stream).await?;
    let mut buffered = Vec::new();
    let hello = serde_json::json!({
        "clientName": "fluxdown-desktop",
        "clientVersion": fluxdown_protocol::APP_VERSION,
        "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
        "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
        "requestedRole": "agent",
        "capabilities": [method::CAPABILITY_CLIENT_SELECTIONS]
    });
    let hello_value = call_on_socket(
        &mut socket,
        1,
        method::SYSTEM_HELLO,
        Some(hello),
        &mut buffered,
    )
    .await
    .map_err(|error| match error {
        ConnectError::Fatal(data) if data.code == ApplicationErrorCode::ProtocolIncompatible => {
            ConnectError::Incompatible
        }
        other => other,
    })?;
    let service = serde_json::from_value::<ServiceHello>(hello_value)
        .map_err(|_| ConnectError::Fatal(protocol_error()))?;
    if service.role != ServiceRole::Agent {
        return Err(ConnectError::Fatal(protocol_error()));
    }
    if service.protocol_version != fluxdown_protocol::PROTOCOL_VERSION {
        return Err(ConnectError::Incompatible);
    }
    if reject_outdated
        && fluxdown_protocol::is_older_app_version(
            &service.service_version,
            fluxdown_protocol::APP_VERSION,
        )
    {
        return Err(ConnectError::Outdated(service.service_version));
    }
    let snapshot_value =
        call_on_socket(&mut socket, 2, method::SYSTEM_SNAPSHOT, None, &mut buffered).await?;
    let snapshot = serde_json::from_value::<Snapshot>(snapshot_value)
        .map_err(|_| ConnectError::Fatal(protocol_error()))?;
    if !matches!(snapshot.body, SnapshotBody::Agent(_)) {
        return Err(ConnectError::Fatal(protocol_error()));
    }
    Ok((socket, snapshot, buffered))
}

/// 连接正常结束的原因；`Err(())` 表示断线（需重连）。
enum SessionEnd {
    /// 本进程已丢弃客户端（命令通道关闭）。
    ClientDropped,
    /// agent 以 `service-quit` 关闭：完全退出，不再重连。
    ServiceQuit,
}

/// 握手结果：连接循环从这里起步。
struct Bootstrap {
    /// 快照游标 `(epoch, sequence)`。
    cursor: (String, u64),
    /// 握手期缓冲的事件帧。
    buffered: Vec<EventFrame>,
    /// 快照头带待载入标记时的分页载入；`None` = 表已完整。
    tables: Option<TableSync>,
}

impl Bootstrap {
    /// 无缓冲帧、表已完整的起步（测试用）。
    #[cfg(test)]
    fn at(epoch: &str, sequence: u64) -> Self {
        Self {
            cursor: (epoch.to_owned(), sequence),
            buffered: Vec::new(),
            tables: None,
        }
    }
}

async fn run_connected(
    mut socket: Socket,
    commands: &mut mpsc::Receiver<ClientCommand>,
    events: &mpsc::Sender<AgentClientEvent>,
    bootstrap: Bootstrap,
    mut preference_writes: PreferenceWrites,
    submissions: &mut SubmissionTransport,
) -> Result<SessionEnd, ()> {
    let Bootstrap {
        cursor: snapshot_cursor,
        buffered,
        tables,
    } = bootstrap;
    let mut next_id = 10_i64;
    let mut pending = HashMap::<i64, PendingCommand>::new();
    let mut cursor = snapshot_cursor;
    // 握手期缓冲帧里换了 epoch 的帧不属于本快照：丢弃（载入期间到达的帧则按 `judge_frame`
    // 严格判定，epoch 变化 = 整体重连）。
    let buffered = buffered
        .into_iter()
        .filter(|frame| frame.epoch == cursor.0)
        .collect::<Vec<_>>();
    let mut load = match tables {
        // 握手拿到的是不含表的头：握手期事件进入缓冲，翻页载入完成后再回放。
        Some(sync) => {
            let mut started = TableLoad::new(sync);
            for frame in buffered {
                started.sync.push_frame(frame).map_err(|error| {
                    log::warn!("table load backlog rejected a handshake frame: {error:?}");
                })?;
            }
            Some(started)
        }
        None => replay(buffered, &mut cursor, events, &mut preference_writes).await?,
    };
    advance_load(
        &mut socket,
        &mut load,
        &mut next_id,
        &mut cursor,
        events,
        &mut preference_writes,
    )
    .await?;
    loop {
        tokio::select! {
            admission = submissions.gate.admit(), if !submissions.unsent.is_empty() => {
                let admission = admission.map_err(|error| {
                    log::error!("submission transport admission failed: {:?}", error.code);
                })?;
                let Some(command) = submissions.unsent.pop_front() else { continue; };
                if command.ack.is_closed() {
                    // 调用方已取消（future 被丢弃）：未发出的提交不再发送，释放准入。
                    log::debug!("skipping cancelled unsent submission {}", command.method);
                    continue;
                }
                let id = next_id;
                next_id = next_id.saturating_add(1);
                if send_command(&mut socket, &mut pending, &mut preference_writes, id, command, Some(admission)).await.is_err() {
                    break;
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(SessionEnd::ClientDropped); };
                if command.is_submission() {
                    submissions.unsent.push_back(command);
                    continue;
                }
                let id = next_id;
                next_id = next_id.saturating_add(1);
                if send_command(&mut socket, &mut pending, &mut preference_writes, id, command, None).await.is_err() {
                    break;
                }
            }
            () = table_load_timeout(load.as_ref().map(|active| active.deadline)) => {
                log::warn!("table page request timed out after {TABLE_PAGE_TIMEOUT:?}; resyncing");
                break;
            }
            incoming = socket.next() => {
                let text = match incoming {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(Some(frame))))
                        if frame.reason.as_str() == CLOSE_REASON_SERVICE_QUIT =>
                    {
                        for (_, command) in pending.drain() {
                            if command.ack.send(Err(unavailable_error())).is_err() {
                                log::trace!("agent request receiver released before service shutdown");
                            }
                        }
                        return Ok(SessionEnd::ServiceQuit);
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    _ => break,
                };
                if let Ok(notification) = serde_json::from_str::<RpcNotification>(&text)
                    && notification.method == method::SERVICE_EVENT
                {
                    let Some(params) = notification.params else { break; };
                    let Ok(mut frame) = serde_json::from_value::<EventFrame>(params) else { break; };
                    // 载入期间：只缓冲、不应用（载入完成后按序回放）。
                    if let Some(active) = load.as_mut() {
                        if let Err(error) = active.sync.push_frame(frame) {
                            log::warn!("table load backlog rejected an event frame: {error:?}");
                            break;
                        }
                        continue;
                    }
                    preference_writes.overlay_frame(&mut frame);
                    let reload = reload_totals(&frame.event);
                    let accepted = forward_event(frame, &mut cursor, events).await?;
                    if accepted
                        && let Some((tasks, queue)) = reload
                    {
                        // 以本帧为新基准分页重载：本帧已应用（头里的加载态落地），其后帧缓冲。
                        load = TableSync::new(cursor.0.clone(), cursor.1, tasks, queue)
                            .map(TableLoad::new);
                        if advance_load(&mut socket, &mut load, &mut next_id, &mut cursor, events, &mut preference_writes).await.is_err() {
                            break;
                        }
                    }
                    continue;
                }
                let response = serde_json::from_str::<RpcResponse>(&text).map_err(|_| ())?;
                match response {
                    RpcResponse::Success(success) => {
                        if let RequestId::Integer(id) = success.id {
                            if let Some(active) = load.as_mut().filter(|active| active.request == Some(id)) {
                                active.request = None;
                                let accepted = serde_json::from_value::<TablePage>(success.result)
                                    .map_err(|error| {
                                        log::warn!("malformed table page: {error}");
                                    })
                                    .and_then(|page| {
                                        active.sync.accept(page).map_err(|error| {
                                            log::warn!("table page rejected: {error:?}");
                                        })
                                    });
                                if accepted.is_err()
                                    || advance_load(&mut socket, &mut load, &mut next_id, &mut cursor, events, &mut preference_writes).await.is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            preference_writes.settle(id, Some(&success.result));
                            if let Some(command) = pending.remove(&id)
                                && command.ack.send(Ok(success.result)).is_err()
                            {
                                log::trace!("agent request receiver released before success reply");
                            }
                        }
                    }
                    RpcResponse::Failure(failure) => {
                        if let Some(RequestId::Integer(id)) = failure.id {
                            if load.as_ref().is_some_and(|active| active.request == Some(id)) {
                                log::warn!("table page request failed: {:?}", failure.error.data.as_ref().map(|data| data.code));
                                break;
                            }
                            preference_writes.settle(id, None);
                            if let Some(command) = pending.remove(&id)
                                && command.ack.send(Err(failure.error.data.unwrap_or_else(internal_error))).is_err()
                            {
                                log::trace!("agent request receiver released before failure reply");
                            }
                        }
                    }
                }
            }
        }
    }
    for (_, command) in pending {
        if command.ack.send(Err(unavailable_error())).is_err() {
            log::trace!("agent request response receiver already released");
        }
    }
    Err(())
}

async fn send_command(
    socket: &mut Socket,
    pending: &mut HashMap<i64, PendingCommand>,
    preference_writes: &mut PreferenceWrites,
    id: i64,
    command: ClientCommand,
    admission: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<(), ()> {
    preference_writes.stage(id, &command.method, command.params.as_ref());
    let request = RpcRequest::new(RequestId::Integer(id), command.method, command.params);
    let text = serde_json::to_string(&request).map_err(|error| {
        log::error!("could not encode agent command: {error}");
    })?;
    pending.insert(
        id,
        PendingCommand {
            ack: command.ack,
            _admission: admission,
        },
    );
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| {
            log::debug!("agent command send failed: {error}");
        })
}

/// 本事件要求分页重载表时的 `(任务总数, 队列位置总数)`（见 [`ServiceEvent::reloads_tables`]）。
fn reload_totals(event: &ServiceEvent) -> Option<(Option<u64>, Option<u64>)> {
    if !event.reloads_tables() {
        return None;
    }
    match event {
        ServiceEvent::Daemon(DaemonEvent::SnapshotReplaced(header))
        | ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(header))
        | ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(header))) => {
            Some((header.task_total, header.queue_total))
        }
        ServiceEvent::Daemon(DaemonEvent::TablesReloading { tasks, queue })
        | ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::TablesReloading { tasks, queue })) => {
            Some((*tasks, *queue))
        }
        _ => None,
    }
}

async fn table_load_timeout(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// 把缓冲帧按序回放给会话（`judge_frame` 以 `cursor` 判定）。回放中遇到要求重载表的帧：应用它，
/// 其后的帧作为新一轮载入的缓冲，返回新一轮载入。
async fn replay(
    frames: Vec<EventFrame>,
    cursor: &mut (String, u64),
    events: &mpsc::Sender<AgentClientEvent>,
    preference_writes: &mut PreferenceWrites,
) -> Result<Option<TableLoad>, ()> {
    let mut frames = frames.into_iter();
    while let Some(mut frame) = frames.next() {
        preference_writes.overlay_frame(&mut frame);
        let reload = reload_totals(&frame.event);
        let accepted = forward_event(frame, cursor, events).await?;
        let Some((tasks, queue)) = reload.filter(|_| accepted) else {
            continue;
        };
        let Some(mut sync) = TableSync::new(cursor.0.clone(), cursor.1, tasks, queue) else {
            continue;
        };
        for rest in frames {
            sync.push_frame(rest).map_err(|error| {
                log::warn!("table load backlog rejected a replayed frame: {error:?}");
            })?;
        }
        return Ok(Some(TableLoad::new(sync)));
    }
    Ok(None)
}

/// 推进分页载入：有下一页就发请求并返回（页应答由连接循环路由回来）；全部页到齐则通知会话
/// 装入表并回放缓冲帧，回放中遇到新的重载帧则继续新一轮。
async fn advance_load(
    socket: &mut Socket,
    load: &mut Option<TableLoad>,
    next_id: &mut i64,
    cursor: &mut (String, u64),
    events: &mpsc::Sender<AgentClientEvent>,
    preference_writes: &mut PreferenceWrites,
) -> Result<(), ()> {
    while let Some(mut current) = load.take() {
        if let Some(params) = current.sync.next_request() {
            let id = *next_id;
            *next_id = next_id.saturating_add(1);
            let params = serde_json::to_value(&params).map_err(|error| {
                log::error!("could not encode table page params: {error}");
            })?;
            let request = RpcRequest::new(
                RequestId::Integer(id),
                method::SYSTEM_TASK_PAGE,
                Some(params),
            );
            let text = serde_json::to_string(&request).map_err(|error| {
                log::error!("could not encode table page request: {error}");
            })?;
            socket
                .send(Message::Text(text.into()))
                .await
                .map_err(|error| {
                    log::debug!("table page request send failed: {error}");
                })?;
            current.request = Some(id);
            current.deadline = tokio::time::Instant::now() + TABLE_PAGE_TIMEOUT;
            *load = Some(current);
            return Ok(());
        }
        let epoch = current.sync.epoch().to_owned();
        let sequence = current.sync.base_sequence();
        let loaded = current.sync.into_parts();
        events
            .send(AgentClientEvent::TablesLoaded(Box::new(TablesLoaded {
                epoch,
                sequence,
                tasks: loaded.tasks,
                queue: loaded.queue,
            })))
            .await
            .map_err(|_| ())?;
        *load = replay(loaded.backlog, cursor, events, preference_writes).await?;
    }
    Ok(())
}

/// 把一帧交给会话；返回帧是否被接受（旧帧 / 已被快照覆盖的帧返回 `false`）。
async fn forward_event(
    frame: EventFrame,
    cursor: &mut (String, u64),
    events: &mpsc::Sender<AgentClientEvent>,
) -> Result<bool, ()> {
    match judge_frame(&cursor.0, cursor.1, &frame) {
        FrameVerdict::Skip => return Ok(false),
        FrameVerdict::Resync => return Err(()),
        FrameVerdict::Accept => cursor.1 = frame.sequence,
    }
    events
        .send(AgentClientEvent::Event(Box::new(frame)))
        .await
        .map(|()| true)
        .map_err(|_| ())
}

async fn call_on_socket(
    socket: &mut Socket,
    id: i64,
    method_name: &str,
    params: Option<Value>,
    buffered: &mut Vec<EventFrame>,
) -> Result<Value, ConnectError> {
    let request = RpcRequest::new(RequestId::Integer(id), method_name, params);
    let text = serde_json::to_string(&request)
        .map_err(|error| ConnectError::Transient(format!("encode {method_name}: {error}")))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| ConnectError::Transient(format!("send {method_name}: {error}")))?;
    while let Some(message) = socket.next().await {
        let message = message
            .map_err(|error| ConnectError::Transient(format!("receive {method_name}: {error}")))?;
        let Message::Text(text) = message else {
            continue;
        };
        if let Ok(notification) = serde_json::from_str::<RpcNotification>(&text)
            && notification.method == method::SERVICE_EVENT
        {
            let Some(params) = notification.params else {
                return Err(ConnectError::Fatal(protocol_error()));
            };
            let frame = serde_json::from_value::<EventFrame>(params)
                .map_err(|_| ConnectError::Fatal(protocol_error()))?;
            if buffered.len() >= HANDSHAKE_EVENT_BUFFER_LIMIT {
                return Err(ConnectError::Transient(format!(
                    "event backlog overflow during {method_name}"
                )));
            }
            buffered.push(frame);
            continue;
        }
        let response = serde_json::from_str::<RpcResponse>(&text).map_err(|_| {
            ConnectError::Transient(format!("unexpected frame during {method_name}"))
        })?;
        match response {
            RpcResponse::Success(success) if success.id == RequestId::Integer(id) => {
                return Ok(success.result);
            }
            RpcResponse::Failure(failure) if failure.id == Some(RequestId::Integer(id)) => {
                return Err(ConnectError::Fatal(
                    failure.error.data.unwrap_or_else(internal_error),
                ));
            }
            _ => {}
        }
    }
    Err(ConnectError::Transient(format!(
        "connection closed during {method_name}"
    )))
}

/// 401 不致命：每次连接都重读 bearer，读完仍被拒说明监听者不是本用户的 agent，交给重连循环
/// 拉起本用户的 agent（它会回退到系统分配端口并重新发布端点）。
fn classify_connect_error(error: tokio_tungstenite::tungstenite::Error) -> ConnectError {
    match &error {
        tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401 => {
            ConnectError::Unauthorized
        }
        _ => ConnectError::Transient(format!("handshake: {error}")),
    }
}

enum ConnectError {
    /// 本机还没有可用 bearer：agent 未启动，或已监听但仍在初始化。
    NoBearer,
    /// 连接 agent 端口被拒：此刻确定无人监听。
    Refused,
    /// TCP 建连在给定时长内未完成：无法确定是否有人监听（杀软 WFP 过滤、高负载、Windows 对
    /// 未监听回环端口约 2s 才返回拒绝）。
    TimedOut(Duration),
    /// 握手被 401 拒绝：监听者不是本用户 / 本数据目录的 agent，或端点文件已陈旧。
    Unauthorized,
    /// 握手 / 传输失败，携带诊断原因。
    Transient(String),
    /// 对端协议版本不兼容：可尝试让旧 agent 退出后由 bootstrap 拉起同级新版本。
    Incompatible,
    /// 对端协议兼容但应用版本旧于本桌面：让它退出，由 bootstrap 拉起同级（同版本）agent。
    Outdated(String),
    Fatal(RpcErrorData),
}

impl ConnectError {
    /// 连接诊断日志用的原因描述。
    fn describe(&self) -> String {
        match self {
            Self::NoBearer => {
                "no bearer token yet (agent not started or still initializing)".to_owned()
            }
            Self::Refused => "connection refused (agent not listening)".to_owned(),
            Self::TimedOut(timeout) => format!(
                "connection attempt timed out after {timeout:?} (agent not accepting loopback connections yet)"
            ),
            Self::Unauthorized => {
                "agent rejected the bearer token (listener is not this user's agent, or the endpoint is stale)"
                    .to_owned()
            }
            Self::Transient(reason) => reason.clone(),
            Self::Incompatible => "incompatible protocol version".to_owned(),
            Self::Outdated(version) => format!("agent v{version} is older than this desktop"),
            Self::Fatal(error) => format!("fatal: {:?}", error.code),
        }
    }
}

fn validate_url(url: &str) -> Result<(), AgentClientError> {
    crate::agent_endpoint::socket_target(url)
        .map(|_| ())
        .map_err(|error| AgentClientError::Configuration(error.to_string()))
}

fn unavailable_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
}

fn internal_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Internal, false)
}

fn protocol_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::ProtocolIncompatible, false)
}

#[derive(Debug, thiserror::Error)]
pub enum AgentClientError {
    #[error("agent client configuration is invalid: {0}")]
    Configuration(String),
    #[error("could not create agent client runtime: {0}")]
    Runtime(std::io::Error),
}

#[cfg(test)]
#[path = "agent_client_submission_tests.rs"]
mod submission_tests;

#[cfg(test)]
#[path = "agent_client_connect_tests.rs"]
mod connect_tests;

#[cfg(test)]
#[path = "agent_client_table_tests.rs"]
mod table_tests;
