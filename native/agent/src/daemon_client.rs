//! agent 到 daemon 的单连接 JSON-RPC 客户端与重连快照恢复。
//!
//! 端点发现：独立 `fluxdownd` 在 `daemon.token` 同目录发布实际监听地址
//! （[`DAEMON_ENDPOINT_FILE`]，默认端口被占用 / 落在系统保留区间时为系统分配端口），每次连接前
//! 重读。显式 `FLUXDOWN_DAEMON_URL` 与嵌入式宿主报告的地址是固定端点，不读该文件。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{PoisonError, RwLock};
use std::time::Duration;

use fluxdown_protocol::handshake::{
    AuthChallengeParams, AuthChallengeResult, AuthProveParams, SYSTEM_AUTH_CHALLENGE,
    SYSTEM_AUTH_PROVE, client_proof, http_credential, verify_server_proof,
};
use fluxdown_protocol::local_service::{DAEMON_ENDPOINT_FILE, LocalEndpoint};
use fluxdown_protocol::method;
use fluxdown_protocol::{
    ApplicationErrorCode, CancelRequestParams, DaemonEvent, DaemonSnapshot, EventFrame,
    FrameVerdict, METHOD_NOT_FOUND_CODE, RequestId, RpcErrorData, RpcErrorObject, RpcNotification,
    RpcRequest, RpcResponse, ServiceEvent, ServiceHello, ServiceRole, Snapshot, SnapshotBody,
    TablePage, TableSync, WsServerMsg, judge_frame,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::supervisor::DaemonSupervisor;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// daemon 默认控制面地址（`fluxdownd` 的 `DEFAULT_DAEMON_BIND`）。只在本数据目录的 daemon
/// 在运行却未发布端点时使用：旧版 daemon 只监听这里，从不发布端点文件。
pub(crate) const DEFAULT_DAEMON_RPC_URL: &str = "ws://127.0.0.1:17801/rpc";

/// daemon 端点来源。
#[derive(Clone)]
enum DaemonEndpoint {
    /// 显式 `FLUXDOWN_DAEMON_URL` / 嵌入式宿主报告的地址：只连它。
    Fixed(String),
    /// 读 daemon 发布的端点文件；文件缺失时按 `daemon.lock` 进程租约判断是否有本数据目录的
    /// daemon 在运行（旧版 / 尚未完成启动）。
    Discovered {
        endpoint_file: PathBuf,
        lease_file: PathBuf,
    },
}

/// `daemon.token` 来源：嵌入式宿主直接给出；独立进程每次连接前从文件读取（首次启动时 daemon
/// 尚未生成，生成前不连接）。
#[derive(Clone)]
enum DaemonToken {
    Fixed(Arc<str>),
    File(PathBuf),
}

/// daemon 客户端配置。
#[derive(Clone)]
pub struct DaemonClientConfig {
    endpoint: DaemonEndpoint,
    /// 只用于握手 MAC 的密钥，永不发送，也不当作 HTTP 凭据。
    token: DaemonToken,
    http: HttpSession,
    /// 最近一次解析出的连接地址（诊断展示）。
    last_url: Arc<RwLock<String>>,
}

impl DaemonClientConfig {
    /// 固定地址与 token（嵌入式宿主、测试）。
    #[must_use]
    pub fn new(rpc_url: impl Into<String>, token: impl Into<String>) -> Self {
        let rpc_url = rpc_url.into();
        Self {
            last_url: Arc::new(RwLock::new(rpc_url.clone())),
            endpoint: DaemonEndpoint::Fixed(rpc_url),
            token: DaemonToken::Fixed(Arc::from(token.into())),
            http: HttpSession::default(),
        }
    }

    /// 由 agent 监管的独立 `fluxdownd`：`explicit_url`（`FLUXDOWN_DAEMON_URL`）优先，否则按
    /// `token_file` 同目录的端点文件发现；token 每次连接前从 `token_file` 读取。
    #[must_use]
    pub fn discovered(
        explicit_url: Option<String>,
        token_file: PathBuf,
        daemon_data_dir: &Path,
    ) -> Self {
        let endpoint = match explicit_url {
            Some(url) => DaemonEndpoint::Fixed(url),
            None => DaemonEndpoint::Discovered {
                endpoint_file: token_file
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(daemon_data_dir)
                    .join(DAEMON_ENDPOINT_FILE),
                lease_file: daemon_data_dir.join("daemon.lock"),
            },
        };
        let label = match &endpoint {
            DaemonEndpoint::Fixed(url) => url.clone(),
            DaemonEndpoint::Discovered { .. } => DEFAULT_DAEMON_RPC_URL.to_owned(),
        };
        Self {
            endpoint,
            token: DaemonToken::File(token_file),
            http: HttpSession::default(),
            last_url: Arc::new(RwLock::new(label)),
        }
    }

    /// 与本配置的 daemon 连接绑定的 HTTP 访问句柄（`/blobs`、`/files`、`/exports` 用）。
    #[must_use]
    pub fn http_session(&self) -> HttpSession {
        self.http.clone()
    }

    /// 最近一次连接（或尝试连接）的 daemon RPC 地址，仅供诊断展示。
    #[must_use]
    pub fn endpoint_label(&self) -> String {
        self.last_url
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 拒绝非 loopback 的固定 daemon URL（发现的端点在读取时校验）。
    pub fn validate(&self) -> Result<(), DaemonClientError> {
        match &self.endpoint {
            DaemonEndpoint::Fixed(url) => {
                validate_loopback_url(url).map_err(DaemonClientError::Configuration)
            }
            DaemonEndpoint::Discovered { .. } => Ok(()),
        }
    }

    /// 本轮连接的目标。`own_child_alive`：本进程监管的 daemon 子进程仍在运行（它启动完成后一定
    /// 会发布端点，发布前不去连默认地址上可能属于别人的进程）。
    async fn resolve(&self, own_child_alive: bool) -> Result<Resolution, ConnectError> {
        let target = match &self.endpoint {
            DaemonEndpoint::Fixed(url) => DaemonTarget {
                url: url.clone(),
                source: TargetSource::Fixed,
            },
            DaemonEndpoint::Discovered {
                endpoint_file,
                lease_file,
            } => match read_published_endpoint(endpoint_file).await? {
                Some(url) => DaemonTarget {
                    url,
                    source: TargetSource::Published,
                },
                None if own_child_alive => return Ok(Resolution::NotReady),
                None if crate::lifecycle::daemon_lease_held(lease_file) => DaemonTarget {
                    url: DEFAULT_DAEMON_RPC_URL.to_owned(),
                    source: TargetSource::Default,
                },
                None => return Ok(Resolution::NotReady),
            },
        };
        target.url.clone_into(
            &mut self
                .last_url
                .write()
                .unwrap_or_else(PoisonError::into_inner),
        );
        Ok(Resolution::Connect(target))
    }

    /// 握手密钥；独立 daemon 尚未生成 token 时为 [`ConnectError::NotReady`]。
    async fn load_token(&self) -> Result<Arc<str>, ConnectError> {
        match &self.token {
            DaemonToken::Fixed(token) => Ok(token.clone()),
            DaemonToken::File(path) => match tokio::fs::read_to_string(path).await {
                Ok(text) if !text.trim().is_empty() => Ok(Arc::from(text.trim())),
                Ok(_) => Err(ConnectError::NotReady),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Err(ConnectError::NotReady)
                }
                Err(error) => Err(ConnectError::Transient(format!(
                    "could not read {}: {error}",
                    path.display()
                ))),
            },
        }
    }
}

/// 一次连接尝试的目标地址及其来源（决定拒绝 / 冒充时的处理方式）。
#[derive(Clone, Debug)]
pub(crate) struct DaemonTarget {
    url: String,
    source: TargetSource,
}

impl DaemonTarget {
    fn port(&self) -> Option<u16> {
        reqwest::Url::parse(&self.url)
            .ok()
            .and_then(|url| url.port_or_known_default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetSource {
    /// 显式 / 嵌入式地址：拒绝即拉起，冒充者致命。
    Fixed,
    /// daemon 发布的端点：连不上或对端不是本数据目录的 daemon，都说明文件已过期。
    Published,
    /// 本数据目录的 daemon 在运行却未发布端点：旧版 daemon，或仍在启动的新版 daemon。
    Default,
}

enum Resolution {
    Connect(DaemonTarget),
    /// 本数据目录暂时没有可连的 daemon：未运行，或本进程拉起的 daemon 尚未发布端点。
    NotReady,
}

/// 读取 daemon 发布的端点；文件不存在为 `None`。内容无效视为暂时性故障（daemon 总是原子
/// 替换该文件，下次启动时会删除重写），按退避重试并记录原因。
async fn read_published_endpoint(path: &Path) -> Result<Option<String>, ConnectError> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ConnectError::Transient(format!(
                "could not read {}: {error}",
                path.display()
            )));
        }
    };
    let endpoint = serde_json::from_slice::<LocalEndpoint>(&bytes).map_err(|error| {
        ConnectError::Transient(format!("{} is not valid: {error}", path.display()))
    })?;
    validate_loopback_url(&endpoint.rpc_url).map_err(|error| {
        ConnectError::Transient(format!("{} is not valid: {error}", path.display()))
    })?;
    Ok(Some(endpoint.rpc_url))
}

/// daemon RPC 地址只能是回环上的 `ws://`。
fn validate_loopback_url(url: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if url.scheme() != "ws" {
        return Err("daemon URL must use ws".to_owned());
    }
    let Some(host) = url.host_str() else {
        return Err("daemon URL has no host".to_owned());
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if loopback {
        Ok(())
    } else {
        Err("daemon URL must be loopback".to_owned())
    }
}

/// 已认证 daemon 会话的 HTTP 访问参数：会话所连 daemon 的 HTTP 基址与会话级凭据。
#[derive(Clone, Debug)]
pub struct DaemonHttpAccess {
    /// `http://<daemon 地址>`，路径为空。
    pub base_url: reqwest::Url,
    pub credential: String,
}

impl DaemonHttpAccess {
    /// `ws://host:port/rpc` → `http://host:port`。
    pub(crate) fn from_rpc_url(rpc_url: &str, credential: String) -> Result<Self, String> {
        let mut base_url = reqwest::Url::parse(rpc_url).map_err(|error| error.to_string())?;
        let scheme = match base_url.scheme() {
            "ws" | "http" => "http",
            "wss" | "https" => "https",
            other => return Err(format!("unsupported daemon URL scheme {other}")),
        };
        base_url
            .set_scheme(scheme)
            .map_err(|()| "daemon URL scheme is not settable".to_owned())?;
        base_url.set_path("");
        base_url.set_query(None);
        base_url.set_fragment(None);
        Ok(Self {
            base_url,
            credential,
        })
    }
}

/// daemon 专用 HTTP 端点使用的会话级访问参数。
///
/// 只在 WebSocket 握手证实对端持有 `daemon.token` 之后才存在，随连接断开而撤销；
/// 没有会话时为 `None`，调用方必须当作 daemon 不可用，而不是退回长期 token。基址随会话
/// 一起给出：daemon 换了端口重连后，HTTP 请求跟着新会话走。
#[derive(Clone, Default)]
pub struct HttpSession {
    access: Arc<RwLock<Option<DaemonHttpAccess>>>,
}

impl HttpSession {
    /// 当前会话的 HTTP 访问参数。
    #[must_use]
    pub fn access(&self) -> Option<DaemonHttpAccess> {
        self.access
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn install(&self, access: DaemonHttpAccess) {
        *self.access.write().unwrap_or_else(PoisonError::into_inner) = Some(access);
    }

    fn clear(&self) {
        *self.access.write().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// daemon 连接产生的有序状态流。
pub enum DaemonClientEvent {
    Snapshot(Snapshot),
    Event(Box<EventFrame>),
    Stale,
    Fatal(RpcErrorData),
}

struct ClientCommand {
    method: String,
    params: Option<Value>,
    ack: oneshot::Sender<Result<Value, RpcErrorObject>>,
}

/// 首次连上 daemon 之前（agent 冷启动，Gateway 已先行服务）的调用等待上限，与 agent 启动
/// 就绪预算一致：期间到达的捕获 / 兼容 API / 迁移等调用等 daemon 就绪后送达，而不是直接失败。
const STARTUP_CALL_WAIT: Duration = Duration::from_secs(30);

/// 可克隆的 daemon 调用入口。
#[derive(Clone)]
pub struct DaemonClient {
    commands: mpsc::Sender<ClientCommand>,
    connected: Arc<AtomicBool>,
    /// 首次连接已有结论（连上过，或重连任务已终止）：此后断线期间的调用立即失败。
    settled: Arc<AtomicBool>,
    ready: Arc<Notify>,
    cancel: CancellationToken,
}

impl DaemonClient {
    /// 启动重连任务与有界事件流；`cancel` 触发后不再为首次连接等待。
    pub fn start(
        config: DaemonClientConfig,
        supervisor: Arc<DaemonSupervisor>,
        cancel: CancellationToken,
    ) -> Result<(Self, mpsc::Receiver<DaemonClientEvent>), DaemonClientError> {
        Self::spawn(config, supervisor, cancel, false)
    }

    /// 嵌入式宿主：与 [`Self::start`] 相同，但 `cancel` 触发时重连任务一并结束（进程不随
    /// agent 退出，不能留下继续重连的后台任务）。
    pub(crate) fn start_scoped(
        config: DaemonClientConfig,
        supervisor: Arc<DaemonSupervisor>,
        cancel: CancellationToken,
    ) -> Result<(Self, mpsc::Receiver<DaemonClientEvent>), DaemonClientError> {
        Self::spawn(config, supervisor, cancel, true)
    }

    fn spawn(
        config: DaemonClientConfig,
        supervisor: Arc<DaemonSupervisor>,
        cancel: CancellationToken,
        stop_on_cancel: bool,
    ) -> Result<(Self, mpsc::Receiver<DaemonClientEvent>), DaemonClientError> {
        config.validate()?;
        let (commands, command_rx) = mpsc::channel(64);
        let (events, event_rx) = mpsc::channel(1024);
        let connected = Arc::new(AtomicBool::new(false));
        let settled = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(Notify::new());
        let http = config.http.clone();
        let client = run_client(
            config,
            supervisor,
            command_rx,
            events,
            connected.clone(),
            SettleOnExit {
                settled: settled.clone(),
                ready: ready.clone(),
            },
        );
        if stop_on_cancel {
            let stop = cancel.clone();
            let connected = connected.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = stop.cancelled() => {
                        connected.store(false, Ordering::Release);
                        http.clear();
                    }
                    () = client => {}
                }
            });
        } else {
            tokio::spawn(client);
        }
        Ok((
            Self {
                commands,
                connected,
                settled,
                ready,
                cancel,
            },
            event_rx,
        ))
    }

    /// 当前是否已连上 daemon（不等待）。
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// 提交类型化 RPC 调用。首次连上之前等待就绪（有上限）；之后断线期间立即失败。
    /// 错误只保留稳定应用错误详情；需要 daemon 错误消息（稳定错误码字符串）的调用用
    /// [`Self::call_detailed`]。
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<P>,
    ) -> Result<R, RpcErrorData> {
        self.call_detailed(method, params)
            .await
            .map_err(|error| error.data.unwrap_or_else(internal_error))
    }

    /// 同 [`Self::call`]，但失败时保留 daemon 返回的完整 JSON-RPC 错误对象（含 `message`）。
    pub async fn call_detailed<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<P>,
    ) -> Result<R, RpcErrorObject> {
        if !self.is_connected() && !self.settled.load(Ordering::Acquire) {
            tokio::select! {
                _ = self.cancel.cancelled() => {}
                _ = self.wait_until(STARTUP_CALL_WAIT, || {
                    self.is_connected() || self.settled.load(Ordering::Acquire)
                }) => {}
            }
        }
        if !self.is_connected() {
            return Err(unavailable_object());
        }
        let params = match params {
            Some(params) => Some(
                serde_json::to_value(params)
                    .map_err(|_| RpcErrorObject::application("invalid params", internal_error()))?,
            ),
            None => None,
        };
        let (ack, response) = oneshot::channel();
        self.commands
            .send(ClientCommand {
                method: method.to_owned(),
                params,
                ack,
            })
            .await
            .map_err(|_| unavailable_object())?;
        let value = response.await.map_err(|_| unavailable_object())??;
        serde_json::from_value(value)
            .map_err(|_| RpcErrorObject::application("invalid daemon result", internal_error()))
    }

    pub async fn wait_ready(&self, timeout: Duration) -> Result<(), RpcErrorData> {
        self.wait_until(timeout, || self.is_connected()).await
    }

    /// 等 `done` 成立；先登记唤醒再检查条件，连接恰在检查与等待之间建立也不会漏掉通知。
    async fn wait_until(
        &self,
        timeout: Duration,
        done: impl Fn() -> bool,
    ) -> Result<(), RpcErrorData> {
        tokio::time::timeout(timeout, async {
            loop {
                let notified = self.ready.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if done() {
                    return;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| unavailable_error())
    }
}

/// 重连任务退出（协议不兼容 / 致命错误）时结束首次连接等待，调用方不再空等到上限。
struct SettleOnExit {
    settled: Arc<AtomicBool>,
    ready: Arc<Notify>,
}

impl SettleOnExit {
    fn settle(&self) {
        self.settled.store(true, Ordering::Release);
        self.ready.notify_waiters();
    }
}

impl Drop for SettleOnExit {
    fn drop(&mut self) {
        self.settle();
    }
}

/// 测试用记录客户端收到的 `(method, params)` 序列。
#[cfg(test)]
pub(crate) type RecordedCalls = Arc<tokio::sync::Mutex<Vec<(String, Option<Value>)>>>;

#[cfg(test)]
impl DaemonClient {
    /// 已断线（首次连接已有结论）的客户端：调用立即失败。
    pub(crate) fn disconnected() -> Self {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        Self {
            commands,
            connected: Arc::new(AtomicBool::new(false)),
            settled: Arc::new(AtomicBool::new(true)),
            ready: Arc::new(Notify::new()),
            cancel: CancellationToken::new(),
        }
    }

    /// 已连接的客户端：记录每次调用的方法与参数并回 `{}`，供断言 agent 发给 daemon 的命令。
    pub(crate) fn recording() -> (Self, RecordedCalls) {
        let (commands, mut receiver) = mpsc::channel::<ClientCommand>(16);
        let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let sink = calls.clone();
        tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                sink.lock().await.push((command.method, command.params));
                if command
                    .ack
                    .send(Ok(Value::Object(serde_json::Map::new())))
                    .is_err()
                {
                    tracing::trace!("recorded daemon caller dropped its response receiver");
                }
            }
        });
        let client = Self {
            commands,
            connected: Arc::new(AtomicBool::new(true)),
            settled: Arc::new(AtomicBool::new(true)),
            ready: Arc::new(Notify::new()),
            cancel: CancellationToken::new(),
        };
        (client, calls)
    }

    /// 已连接的客户端：每次调用都按 `reply` 现算应答，供断言 agent 对 daemon 应答的处理。
    pub(crate) fn answering<F>(reply: F) -> Self
    where
        F: Fn(&str) -> Result<Value, RpcErrorObject> + Send + 'static,
    {
        let (commands, mut receiver) = mpsc::channel::<ClientCommand>(16);
        tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                if command.ack.send(reply(&command.method)).is_err() {
                    tracing::trace!("scripted daemon caller dropped its response receiver");
                }
            }
        });
        Self {
            commands,
            connected: Arc::new(AtomicBool::new(true)),
            settled: Arc::new(AtomicBool::new(true)),
            ready: Arc::new(Notify::new()),
            cancel: CancellationToken::new(),
        }
    }
}

/// 本进程刚拉起、仍存活但尚未就绪（未监听 / 未发布端点 / 未生成 token）的 daemon 按短间隔轮询
/// （约 10s）：冷启动 daemon 通常数百毫秒内就绪，若按 1s 起步的退避等待，整条启动链（界面首个
/// 快照）会被白白拖长约 1s。只对同一个子进程代际生效；子进程退出后被重新拉起则回到指数退避，
/// 避免崩溃循环高频拉起。
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STARTUP_POLL_ATTEMPTS: usize = 200;
/// daemon 启动即崩溃循环时的重拉间隔：避免每 ≤30s 完整初始化一次引擎。
const CRASH_LOOP_RETRY_SECS: u64 = 120;
/// 建连、双向认证、hello 与快照的总时限：daemon 端口被不响应的进程占着、或 daemon 挂死时，
/// 不能永远卡在握手里；超时按暂时性失败退避重连。慢盘 / 大任务表的快照留足余量。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// 握手前 `system.shutdown`（完全退出兜底、协议不兼容替换）的总时限。
const SHUTDOWN_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 握手阶段的请求 ID（连接建立后的调用从 10 起）。
const AUTH_CHALLENGE_REQUEST_ID: i64 = 1;
const AUTH_PROVE_REQUEST_ID: i64 = 2;
const HELLO_REQUEST_ID: i64 = 3;
const SNAPSHOT_REQUEST_ID: i64 = 4;
/// 已认证后、`system.hello` 之前的 `system.shutdown`。
const SHUTDOWN_REQUEST_ID: i64 = 3;

async fn run_client(
    config: DaemonClientConfig,
    supervisor: Arc<DaemonSupervisor>,
    mut commands: mpsc::Receiver<ClientCommand>,
    events: mpsc::Sender<DaemonClientEvent>,
    connected: Arc<AtomicBool>,
    settle: SettleOnExit,
) {
    let backoff = [1_u64, 2, 5, 15, 30];
    let mut attempt = 0_usize;
    // 本轮断连中快速轮询的 daemon 子进程代际与已用次数；连上即清零。
    let mut polled_child: Option<u64> = None;
    let mut startup_polls = 0_usize;
    // 每个 agent 进程只尝试替换一次协议不兼容的 daemon，避免同目录二进制错配时反复互杀。
    let mut replaced_incompatible = false;
    // 同理只替换一次应用版本旧于本 agent 的 daemon：agent 单独退出（SIGTERM、崩溃、被结束）
    // 后 daemon 按设计继续运行，升级后的新 agent 会连到这个旧版 daemon，旧引擎继续干活。
    let mut replaced_outdated = false;
    // 被旧版 daemon（或冒充者）占着端口时已尝试结束同名进程的次数，见 `LEGACY_REPLACE_ATTEMPTS`。
    let mut legacy_attempts = 0_u32;
    loop {
        let (target, outcome) = match config.resolve(supervisor.child_alive().await).await {
            Ok(Resolution::Connect(target)) => {
                let outcome = connect(&config, &target, !replaced_outdated).await;
                (Some(target), outcome)
            }
            Ok(Resolution::NotReady) => (None, Err(ConnectError::NotReady)),
            Err(error) => (None, Err(error)),
        };
        let error = match outcome {
            Ok((socket, snapshot, buffered)) => {
                attempt = 0;
                supervisor.clear_crash_streak();
                polled_child = None;
                startup_polls = 0;
                connected.store(true, Ordering::Release);
                settle.settle();
                let snapshot_cursor = (snapshot.epoch.clone(), snapshot.sequence);
                if events
                    .send(DaemonClientEvent::Snapshot(snapshot))
                    .await
                    .is_err()
                {
                    connected.store(false, Ordering::Release);
                    config.http.clear();
                    return;
                }
                if run_connected(socket, &mut commands, &events, snapshot_cursor, buffered)
                    .await
                    .is_err()
                    && events.send(DaemonClientEvent::Stale).await.is_err()
                {
                    tracing::debug!("daemon event consumer closed during disconnect");
                    connected.store(false, Ordering::Release);
                    config.http.clear();
                    fail_queued_commands(&mut commands);
                    return;
                }
                connected.store(false, Ordering::Release);
                config.http.clear();
                fail_queued_commands(&mut commands);
                None
            }
            Err(error) => Some(error),
        };
        let source = target.as_ref().map(|target| target.source);
        match error {
            None => {}
            Some(ConnectError::Refused) => {
                if relaunch_or_poll(&supervisor, &mut polled_child, &mut startup_polls).await {
                    tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
                    continue;
                }
            }
            Some(ConnectError::NotReady) => {
                if relaunch_or_poll(&supervisor, &mut polled_child, &mut startup_polls).await {
                    tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
                    continue;
                }
                tracing::warn!(
                    endpoint = %config.endpoint_label(),
                    "fluxdownd is not ready yet: daemon.token or its published endpoint is missing"
                );
            }
            // 端点文件指向的端口上已不是本数据目录的 daemon（daemon 崩溃后端口被别的进程
            // 复用）：文件已过期，按「连接被拒」拉起 daemon，新 daemon 启动时会重写它。
            Some(ConnectError::LegacyPeer) if source == Some(TargetSource::Published) => {
                tracing::warn!(
                    endpoint = %config.endpoint_label(),
                    "the published fluxdownd endpoint is held by another process; treating it as stale"
                );
                if relaunch_or_poll(&supervisor, &mut polled_child, &mut startup_polls).await {
                    tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
                    continue;
                }
            }
            Some(ConnectError::Fatal(data))
                if data.code == ApplicationErrorCode::Unauthorized
                    && source == Some(TargetSource::Published) =>
            {
                tracing::warn!(
                    endpoint = %config.endpoint_label(),
                    "the process on the published fluxdownd endpoint cannot prove daemon.token; treating the endpoint as stale"
                );
                if relaunch_or_poll(&supervisor, &mut polled_child, &mut startup_polls).await {
                    tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
                    continue;
                }
            }
            // 本数据目录的 daemon 持有租约但尚未发布端点，默认端口上却是另一个数据目录 / 用户的
            // daemon：等本数据目录的 daemon 发布端点，不当作冒充者而退出。
            Some(ConnectError::Fatal(data))
                if data.code == ApplicationErrorCode::Unauthorized
                    && source == Some(TargetSource::Default) =>
            {
                tracing::warn!(
                    "the default daemon port belongs to another fluxdownd (another user or data directory); waiting for this data directory's fluxdownd to publish its endpoint"
                );
            }
            Some(ConnectError::Outdated(version)) => {
                replaced_outdated = true;
                tracing::warn!(
                    running = %version,
                    own = fluxdown_protocol::APP_VERSION,
                    "fluxdownd is older than this agent; asking it to exit"
                );
                if let Some(target) = target.as_ref() {
                    match shutdown_target(&config, target).await {
                        Ok(()) => wait_until_stopped(target, Duration::from_secs(30)).await,
                        Err(error) => tracing::warn!(
                            %error,
                            "outdated fluxdownd refused shutdown; keeping it"
                        ),
                    }
                }
                // 关停失败也照常重连：下一轮不再比较版本，沿用正在运行的 daemon。
                attempt = 0;
                continue;
            }
            Some(ConnectError::Incompatible) if !replaced_incompatible => {
                replaced_incompatible = true;
                if let Some(target) = target.as_ref() {
                    match shutdown_target(&config, target).await {
                        Ok(()) => {
                            tracing::info!("asked protocol-incompatible fluxdownd to exit");
                            wait_until_stopped(target, Duration::from_secs(30)).await;
                            attempt = 0;
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                "protocol-incompatible fluxdownd refused shutdown"
                            );
                            if events
                                .send(DaemonClientEvent::Fatal(protocol_error()))
                                .await
                                .is_err()
                            {
                                tracing::debug!(
                                    "daemon event consumer closed before fatal protocol error"
                                );
                            }
                            connected.store(false, Ordering::Release);
                            return;
                        }
                    }
                }
            }
            Some(ConnectError::Incompatible) => {
                if events
                    .send(DaemonClientEvent::Fatal(protocol_error()))
                    .await
                    .is_err()
                {
                    tracing::debug!("daemon event consumer closed before fatal protocol error");
                }
                connected.store(false, Ordering::Release);
                return;
            }
            Some(ConnectError::LegacyPeer) => match next_legacy_step(legacy_attempts) {
                LegacyStep::Replace => {
                    legacy_attempts += 1;
                    tracing::warn!(
                        attempt = legacy_attempts,
                        "the process on the daemon port does not speak the challenge-response handshake (legacy fluxdownd); no credentials are sent to it, stopping the stale fluxdownd instead"
                    );
                    let port = target.as_ref().and_then(DaemonTarget::port);
                    let stopped = match port {
                        Some(port) => supervisor.terminate_stale_daemon(port).await,
                        None => 0,
                    };
                    if stopped > 0
                        && let Some(target) = target.as_ref()
                    {
                        tracing::info!(
                            stopped,
                            "signalled stale fluxdownd; waiting for it to release the port"
                        );
                        wait_until_stopped(target, Duration::from_secs(30)).await;
                        attempt = 0;
                        continue;
                    }
                    tracing::warn!(
                        "no stale fluxdownd process could be identified; retrying with backoff"
                    );
                }
                LegacyStep::GiveUp => {
                    tracing::error!(
                        attempts = legacy_attempts,
                        "the daemon port is still held by a process that does not speak the handshake; stop the old fluxdownd manually and restart FluxDown"
                    );
                    if events
                        .send(DaemonClientEvent::Fatal(protocol_error()))
                        .await
                        .is_err()
                    {
                        tracing::debug!("daemon event consumer closed before legacy-peer failure");
                    }
                    connected.store(false, Ordering::Release);
                    return;
                }
            },
            Some(ConnectError::Fatal(error)) => {
                if events.send(DaemonClientEvent::Fatal(error)).await.is_err() {
                    tracing::debug!("daemon event consumer closed before fatal connection error");
                }
                connected.store(false, Ordering::Release);
                return;
            }
            Some(ConnectError::Transient(error)) => {
                tracing::warn!(%error, endpoint = %config.endpoint_label(), "daemon connection failed");
            }
        }
        connected.store(false, Ordering::Release);
        let mut delay = backoff[attempt.min(backoff.len() - 1)];
        if supervisor.in_crash_loop() {
            delay = CRASH_LOOP_RETRY_SECS;
            tracing::error!(
                retry_secs = delay,
                "fluxdownd keeps exiting right after launch; see fluxdownd.stderr.log"
            );
        }
        attempt = attempt.saturating_add(1);
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

/// daemon 不可连（拒绝 / 未就绪 / 端点过期）：确保本进程监管的 daemon 在运行。同一个刚拉起、
/// 仍存活的子进程在启动预算内返回 true，调用方短间隔后立即重试；否则走正常退避。
async fn relaunch_or_poll(
    supervisor: &DaemonSupervisor,
    polled_child: &mut Option<u64>,
    startup_polls: &mut usize,
) -> bool {
    match supervisor.ensure_running().await {
        Ok(Some(child))
            if *startup_polls < STARTUP_POLL_ATTEMPTS
                && *polled_child.get_or_insert(child) == child =>
        {
            *startup_polls += 1;
            true
        }
        Ok(_) => false,
        Err(error) => {
            tracing::warn!(error = %error, "could not supervise fluxdownd");
            false
        }
    }
}

fn fail_queued_commands(commands: &mut mpsc::Receiver<ClientCommand>) {
    while let Ok(command) = commands.try_recv() {
        if command.ack.send(Err(unavailable_object())).is_err() {
            tracing::trace!("queued daemon caller dropped its response receiver");
        }
    }
}

async fn open_socket(target: &DaemonTarget) -> Result<Socket, ConnectError> {
    // 升级请求不带任何凭据：对端在证明持有 token 之前，agent 不交出任何可复用的东西。
    let request = target
        .url
        .as_str()
        .into_client_request()
        .map_err(|error| ConnectError::Fatal(invalid_argument(error.to_string())))?;
    let (socket, _) =
        tokio_tungstenite::connect_async_with_config(request, Some(inbound_ws_config()), false)
            .await
            .map_err(classify_connect_error)?;
    Ok(socket)
}

/// 全量快照随任务数增长且单帧发出：帧上限与消息上限一同放宽（见
/// [`fluxdown_protocol::MAX_INBOUND_RPC_MESSAGE_BYTES`]）。
fn inbound_ws_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    let limit = Some(fluxdown_protocol::MAX_INBOUND_RPC_MESSAGE_BYTES);
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_frame_size(limit)
        .max_message_size(limit)
}

/// 双向挑战应答：先校验 daemon 对 token 持有权的证明，通过后才提交自己的证明。
/// 返回本会话派生出的 HTTP 凭据（从不经 WebSocket 传输）。`token` 本身不会离开本进程。
async fn authenticate(
    socket: &mut Socket,
    token: &str,
    buffered: &mut Vec<EventFrame>,
) -> Result<String, ConnectError> {
    let client_nonce = fresh_nonce();
    let params = serde_json::to_value(AuthChallengeParams {
        client_nonce: client_nonce.clone(),
    })
    .map_err(|error| ConnectError::Transient(error.to_string()))?;
    let challenge = call_on_socket(
        socket,
        AUTH_CHALLENGE_REQUEST_ID,
        SYSTEM_AUTH_CHALLENGE,
        Some(params),
        buffered,
    )
    .await
    .map_err(|error| match error {
        ConnectError::Fatal(data) if is_unrecognized_handshake(&data) => ConnectError::LegacyPeer,
        other => other,
    })?;
    let challenge = serde_json::from_value::<AuthChallengeResult>(challenge)
        .map_err(|_| ConnectError::Fatal(protocol_error()))?;
    if !verify_server_proof(
        token,
        &client_nonce,
        &challenge.server_nonce,
        &challenge.server_proof,
    ) {
        tracing::error!(
            "the process on the daemon port could not prove it holds daemon.token; refusing to authenticate"
        );
        return Err(ConnectError::Fatal(unauthorized_error()));
    }
    let (Some(proof), Some(credential)) = (
        client_proof(token, &client_nonce, &challenge.server_nonce),
        http_credential(token, &client_nonce, &challenge.server_nonce),
    ) else {
        return Err(ConnectError::Fatal(protocol_error()));
    };
    let params = serde_json::to_value(AuthProveParams {
        client_proof: proof,
    })
    .map_err(|error| ConnectError::Transient(error.to_string()))?;
    call_on_socket(
        socket,
        AUTH_PROVE_REQUEST_ID,
        SYSTEM_AUTH_PROVE,
        Some(params),
        buffered,
    )
    .await?;
    Ok(credential)
}

fn describe_connect_error(error: ConnectError) -> String {
    match error {
        ConnectError::Fatal(data) => format!("{:?}", data.code),
        ConnectError::Transient(message) => message,
        ConnectError::Refused | ConnectError::Incompatible | ConnectError::Outdated(_) => {
            "daemon unreachable".to_owned()
        }
        ConnectError::NotReady => "daemon not ready".to_owned(),
        ConnectError::LegacyPeer => "legacy daemon".to_owned(),
    }
}

/// 握手前 `system.shutdown`（完全退出的兜底路径）：按当前端点解析出本数据目录的 daemon。
pub(crate) async fn request_shutdown(config: &DaemonClientConfig) -> Result<(), String> {
    match config
        .resolve(false)
        .await
        .map_err(describe_connect_error)?
    {
        Resolution::Connect(target) => shutdown_target(config, &target).await,
        Resolution::NotReady => Err("no running fluxdownd for this data directory".to_owned()),
    }
}

/// 已认证后、`system.hello` 之前的 `system.shutdown`：daemon 只在握手通过后受理，
/// 协议版本不兼容的新旧进程替换与完全退出都走这里。
async fn shutdown_target(config: &DaemonClientConfig, target: &DaemonTarget) -> Result<(), String> {
    let shutdown = async {
        let token = config.load_token().await.map_err(describe_connect_error)?;
        let mut socket = open_socket(target)
            .await
            .map_err(|_| "daemon unreachable".to_owned())?;
        let mut buffered = Vec::new();
        authenticate(&mut socket, &token, &mut buffered)
            .await
            .map_err(describe_connect_error)?;
        call_on_socket(
            &mut socket,
            SHUTDOWN_REQUEST_ID,
            method::SYSTEM_SHUTDOWN,
            None,
            &mut buffered,
        )
        .await
        .map(|_| ())
        .map_err(describe_connect_error)
    };
    tokio::time::timeout(SHUTDOWN_HANDSHAKE_TIMEOUT, shutdown)
        .await
        .map_err(|_| "daemon shutdown handshake timed out".to_owned())?
}

/// 等旧 daemon 关闭监听（连接被拒）；超时后交回重连循环自愈。
async fn wait_until_stopped(target: &DaemonTarget, timeout: Duration) {
    let Ok(url) = reqwest::Url::parse(&target.url) else {
        return;
    };
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return;
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        match TcpStream::connect((host.as_str(), port)).await {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => return,
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// 建立到 daemon 的已认证、已握手连接并载入任务表；握手（建连 / 认证 / hello / 快照头）整体
/// 受 [`HANDSHAKE_TIMEOUT`] 约束，任务表分页载入只受每页 [`TABLE_PAGE_TIMEOUT`] 约束（页数随
/// 任务数增长）。任何一步失败都撤销会话 HTTP 访问参数。
async fn connect(
    config: &DaemonClientConfig,
    target: &DaemonTarget,
    reject_outdated: bool,
) -> Result<(Socket, Snapshot, Vec<EventFrame>), ConnectError> {
    let result = match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        establish(config, target, reject_outdated),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ConnectError::Transient(format!(
            "handshake with {} timed out after {}s",
            target.url,
            HANDSHAKE_TIMEOUT.as_secs()
        ))),
    };
    let result = match result {
        Ok((mut socket, snapshot, mut buffered)) => {
            load_snapshot_tables(&mut socket, snapshot, &mut buffered)
                .await
                .map(|snapshot| (socket, snapshot, buffered))
        }
        Err(error) => Err(error),
    };
    if result.is_err() {
        config.http.clear();
    }
    result
}

/// 快照头带 `taskTotal` / `queueTotal` 时经 `system.taskPage` 分页载入整表（握手期间到达的
/// 事件帧留在 `buffered`，由 [`run_connected`] 按快照序号回放），返回表已装入的完整快照。
async fn load_snapshot_tables(
    socket: &mut Socket,
    mut snapshot: Snapshot,
    buffered: &mut Vec<EventFrame>,
) -> Result<Snapshot, ConnectError> {
    let SnapshotBody::Daemon(daemon) = &mut snapshot.body else {
        return Err(ConnectError::Fatal(protocol_error()));
    };
    let Some(mut sync) = TableSync::for_snapshot(snapshot.epoch.clone(), snapshot.sequence, daemon)
    else {
        return Ok(snapshot);
    };
    while let Some(params) = sync.next_request() {
        let params = serde_json::to_value(params)
            .map_err(|error| ConnectError::Transient(error.to_string()))?;
        let value = tokio::time::timeout(
            TABLE_PAGE_TIMEOUT,
            call_on_socket(
                socket,
                TABLE_PAGE_REQUEST_ID,
                method::SYSTEM_TASK_PAGE,
                Some(params),
                buffered,
            ),
        )
        .await
        .map_err(|_| ConnectError::Transient("daemon task page timed out".to_owned()))??;
        let page = serde_json::from_value::<TablePage>(value)
            .map_err(|_| ConnectError::Fatal(protocol_error()))?;
        sync.accept(page).map_err(|error| {
            ConnectError::Transient(format!("daemon task paging failed: {error:?}"))
        })?;
    }
    let loaded = sync.into_parts();
    if let Some(tasks) = loaded.tasks {
        daemon.install_tasks(tasks);
    }
    if let Some(queue) = loaded.queue {
        daemon.install_queue(queue);
    }
    Ok(snapshot)
}

async fn establish(
    config: &DaemonClientConfig,
    target: &DaemonTarget,
    reject_outdated: bool,
) -> Result<(Socket, Snapshot, Vec<EventFrame>), ConnectError> {
    let token = config.load_token().await?;
    let mut socket = open_socket(target).await?;
    let mut buffered = Vec::new();
    let credential = authenticate(&mut socket, &token, &mut buffered).await?;
    let access = DaemonHttpAccess::from_rpc_url(&target.url, credential)
        .map_err(|error| ConnectError::Fatal(invalid_argument(error)))?;
    config.http.install(access);
    let hello = serde_json::json!({
        "clientName": "fluxdown-agent",
        "clientVersion": fluxdown_protocol::APP_VERSION,
        "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
        "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
        "requestedRole": "daemon",
        "capabilities": []
    });
    let hello_value = call_on_socket(
        &mut socket,
        HELLO_REQUEST_ID,
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
    if service.role != ServiceRole::Daemon {
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
    let snapshot_value = call_on_socket(
        &mut socket,
        SNAPSHOT_REQUEST_ID,
        method::SYSTEM_SNAPSHOT,
        None,
        &mut buffered,
    )
    .await?;
    let snapshot = serde_json::from_value::<Snapshot>(snapshot_value)
        .map_err(|_| ConnectError::Fatal(protocol_error()))?;
    if !matches!(snapshot.body, SnapshotBody::Daemon(_)) {
        return Err(ConnectError::Fatal(protocol_error()));
    }
    Ok((socket, snapshot, buffered))
}

/// 连接建立后一条链路的可变状态：事件游标、待处理帧队列与在途命令。
struct Link {
    cursor: (String, u64),
    /// 待处理的事件帧（握手窗口内缓冲的、表重载期间到达的）；先于套接字里的新帧处理。
    backlog: VecDeque<EventFrame>,
    pending: HashMap<i64, oneshot::Sender<Result<Value, RpcErrorObject>>>,
    /// 在途的可中止慢调用（`CANCELLABLE_DAEMON_METHODS`）：调用方放弃等待（丢弃接收端）时向
    /// daemon 发 `system.cancel` 中止它，而不是让测速等只读探测跑完。
    slow: HashSet<i64>,
    next_id: i64,
}

async fn run_connected(
    mut socket: Socket,
    commands: &mut mpsc::Receiver<ClientCommand>,
    events: &mpsc::Sender<DaemonClientEvent>,
    snapshot_cursor: (String, u64),
    buffered: Vec<EventFrame>,
) -> Result<(), ()> {
    let mut link = Link {
        backlog: buffered
            .into_iter()
            .filter(|frame| frame.epoch == snapshot_cursor.0)
            .collect(),
        cursor: snapshot_cursor,
        pending: HashMap::new(),
        slow: HashSet::new(),
        next_id: 10,
    };
    let outcome = drive_link(&mut socket, commands, events, &mut link).await;
    for (id, ack) in link.pending {
        if ack.send(Err(unavailable_object())).is_err() {
            tracing::trace!(
                id,
                "pending daemon caller dropped its response receiver during disconnect"
            );
        }
    }
    outcome
}

/// 链路主循环：命令下发、响应路由、事件帧转发。返回 `Ok(())` = 命令通道关闭的正常收尾，
/// `Err(())` = 链路已断或需要重新握手。
async fn drive_link(
    socket: &mut Socket,
    commands: &mut mpsc::Receiver<ClientCommand>,
    events: &mpsc::Sender<DaemonClientEvent>,
    link: &mut Link,
) -> Result<(), ()> {
    let mut sweep = tokio::time::interval(SLOW_CALL_SWEEP);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        while let Some(frame) = link.backlog.pop_front() {
            handle_frame(socket, link, events, frame).await?;
        }
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                let id = link.next_id;
                link.next_id = link.next_id.saturating_add(1);
                if method::is_cancellable_daemon_method(&command.method) {
                    link.slow.insert(id);
                }
                let request = RpcRequest::new(RequestId::Integer(id), command.method, command.params);
                let text = serde_json::to_string(&request).map_err(|_| ())?;
                link.pending.insert(id, command.ack);
                if let Err(error) = socket.send(Message::Text(text.into())).await {
                    tracing::warn!(%error, "daemon link dropped: request send failed");
                    return Err(());
                }
            }
            incoming = socket.next() => {
                match decode_incoming(incoming)? {
                    Incoming::Event(frame) => handle_frame(socket, link, events, *frame).await?,
                    Incoming::Response(response) => route_response(&mut link.pending, response),
                }
            }
            _ = sweep.tick(), if !link.slow.is_empty() => cancel_abandoned_slow_calls(socket, link).await?,
        }
    }
}

/// 放弃检查的间隔：只在有可中止慢调用在途时计时，空闲时不唤醒。
const SLOW_CALL_SWEEP: Duration = Duration::from_millis(500);

/// 调用方已丢弃接收端的在途可中止调用：从待应答表移除并请 daemon 中止（`system.cancel`）。
/// 旧 daemon 不认识该方法时回错，取消请求不登记待应答，回包按未知 ID 忽略。
async fn cancel_abandoned_slow_calls(socket: &mut Socket, link: &mut Link) -> Result<(), ()> {
    let abandoned: Vec<i64> = link
        .slow
        .iter()
        .copied()
        .filter(|id| link.pending.get(id).is_none_or(oneshot::Sender::is_closed))
        .collect();
    for target in abandoned {
        link.slow.remove(&target);
        // 已应答的调用只需从慢调用集合里清掉。
        if link.pending.remove(&target).is_none() {
            continue;
        }
        let id = link.next_id;
        link.next_id = link.next_id.saturating_add(1);
        let params = serde_json::to_value(CancelRequestParams {
            id: RequestId::Integer(target),
        })
        .map_err(|_| ())?;
        let request = RpcRequest::new(RequestId::Integer(id), method::SYSTEM_CANCEL, Some(params));
        let text = serde_json::to_string(&request).map_err(|_| ())?;
        if let Err(error) = socket.send(Message::Text(text.into())).await {
            tracing::warn!(%error, "daemon link dropped: cancel send failed");
            return Err(());
        }
        tracing::debug!(target, "abandoned slow daemon call cancelled");
    }
    Ok(())
}

/// 套接字上一条已解码的入站消息。
enum Incoming {
    Event(Box<EventFrame>),
    Response(RpcResponse),
}

/// 解码一条入站 WebSocket 消息；链路异常 / 无法解码返回 `Err(())`（调用方重连）。
fn decode_incoming(
    incoming: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Result<Incoming, ()> {
    let text = match incoming {
        Some(Ok(Message::Text(text))) => text,
        Some(Ok(Message::Close(frame))) => {
            let (code, reason) = frame
                .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                .unwrap_or_default();
            tracing::warn!(code, %reason, "daemon link closed by daemon");
            return Err(());
        }
        Some(Ok(other)) => {
            tracing::warn!(kind = ?std::mem::discriminant(&other), "daemon link dropped: unexpected non-text frame");
            return Err(());
        }
        Some(Err(error)) => {
            tracing::warn!(%error, "daemon link dropped: receive failed");
            return Err(());
        }
        None => {
            tracing::warn!("daemon link dropped: socket ended");
            return Err(());
        }
    };
    if let Ok(notification) = serde_json::from_str::<RpcNotification>(&text)
        && notification.method == method::SERVICE_EVENT
    {
        let Some(params) = notification.params else {
            tracing::warn!("daemon link dropped: event notification without params");
            return Err(());
        };
        return match serde_json::from_value::<EventFrame>(params) {
            Ok(frame) => Ok(Incoming::Event(Box::new(frame))),
            Err(error) => {
                tracing::warn!(%error, "daemon link dropped: undecodable event frame");
                Err(())
            }
        };
    }
    serde_json::from_str::<RpcResponse>(&text)
        .map(Incoming::Response)
        .map_err(|_| {
            tracing::warn!("daemon link dropped: undecodable response frame");
        })
}

/// 把响应交给等它的调用方。
fn route_response(
    pending: &mut HashMap<i64, oneshot::Sender<Result<Value, RpcErrorObject>>>,
    response: RpcResponse,
) {
    match response {
        RpcResponse::Success(success) => {
            if let RequestId::Integer(id) = success.id
                && let Some(ack) = pending.remove(&id)
                && ack.send(Ok(success.result)).is_err()
            {
                tracing::trace!(id, "daemon caller dropped its successful response receiver");
            }
        }
        RpcResponse::Failure(failure) => {
            if let Some(RequestId::Integer(id)) = failure.id
                && let Some(ack) = pending.remove(&id)
                && ack.send(Err(failure.error)).is_err()
            {
                tracing::trace!(id, "daemon caller dropped its error response receiver");
            }
        }
    }
}

/// 握手期间 / 表重载期间最多缓冲的事件帧数：大批量导入时 daemon 在窗口内会推送上千帧。
const HANDSHAKE_BACKLOG_LIMIT: usize = fluxdown_protocol::TABLE_SYNC_BACKLOG_LIMIT;
/// 任务表 / 队列位置表每页的请求超时：页数随任务数增长，所以只约束单页，不约束总时长。
const TABLE_PAGE_TIMEOUT: Duration = Duration::from_secs(30);
/// 握手阶段分页请求的 ID（连接建立后的调用从 10 起）。
const TABLE_PAGE_REQUEST_ID: i64 = 5;

/// 快照以前的通知可跳过；快照以后的缺口或 epoch 更换必须重新握手。判定见
/// [`judge_frame`]：转发者合并过的帧覆盖区间可以越过游标，不再要求恰好 `cursor + 1`。
/// `Ok(None)` = 已被覆盖、丢弃；`Ok(Some)` = 接受并已推进游标。
fn admit_frame(cursor: &mut (String, u64), frame: EventFrame) -> Result<Option<EventFrame>, ()> {
    match judge_frame(&cursor.0, cursor.1, &frame) {
        FrameVerdict::Skip => return Ok(None),
        FrameVerdict::Resync => {
            if frame.epoch == cursor.0 {
                tracing::warn!(
                    cursor = cursor.1,
                    covers_from = frame.covers_from(),
                    received = frame.sequence,
                    "daemon link dropped: event sequence gap"
                );
            } else {
                tracing::warn!("daemon link dropped: event epoch changed");
            }
            return Err(());
        }
        FrameVerdict::Accept => {}
    }
    cursor.1 = frame.sequence;
    Ok(Some(frame))
}

/// 判定并转发一帧。宣告表需要重载的帧（`SnapshotReplaced` 头 / `TablesReloading`）在这里
/// 就地分页载入，再把载入结果还原成完整的等价事件转发：下游（agent 投影）只认完整事件，
/// 广播到客户端时再由 hub 转成头。
async fn handle_frame(
    socket: &mut Socket,
    link: &mut Link,
    events: &mpsc::Sender<DaemonClientEvent>,
    frame: EventFrame,
) -> Result<(), ()> {
    let Some(frame) = admit_frame(&mut link.cursor, frame)? else {
        return Ok(());
    };
    if frame.event.reloads_tables() {
        return reload_tables(socket, link, events, frame).await;
    }
    events
        .send(DaemonClientEvent::Event(Box::new(frame)))
        .await
        .map_err(|_| ())
}

/// 分页载入 `frame` 宣告的表。载入期间到达的帧缓冲进 `link.backlog`（排在已有待处理帧之后），
/// 响应照常路由给在途命令。
async fn reload_tables(
    socket: &mut Socket,
    link: &mut Link,
    events: &mpsc::Sender<DaemonClientEvent>,
    frame: EventFrame,
) -> Result<(), ()> {
    let EventFrame {
        epoch,
        sequence,
        event,
        ..
    } = frame;
    let rebuilt = |event: DaemonEvent| {
        DaemonClientEvent::Event(Box::new(EventFrame {
            epoch: epoch.clone(),
            sequence,
            previous: None,
            event: ServiceEvent::Daemon(event),
        }))
    };
    let (sync, header) = match event {
        ServiceEvent::Daemon(DaemonEvent::SnapshotReplaced(header)) => (
            TableSync::for_snapshot(epoch.clone(), sequence, &header),
            Some(header),
        ),
        ServiceEvent::Daemon(DaemonEvent::TablesReloading { tasks, queue }) => {
            (TableSync::new(epoch.clone(), sequence, tasks, queue), None)
        }
        other => {
            return events
                .send(DaemonClientEvent::Event(Box::new(EventFrame {
                    epoch: epoch.clone(),
                    sequence,
                    previous: None,
                    event: other,
                })))
                .await
                .map_err(|_| ());
        }
    };
    let Some(mut sync) = sync else {
        return match header {
            Some(header) => events
                .send(rebuilt(DaemonEvent::SnapshotReplaced(header)))
                .await
                .map_err(|_| ()),
            None => Ok(()),
        };
    };
    while let Some(params) = sync.next_request() {
        let id = link.next_id;
        link.next_id = link.next_id.saturating_add(1);
        let request = RpcRequest::new(
            RequestId::Integer(id),
            method::SYSTEM_TASK_PAGE,
            serde_json::to_value(params).ok(),
        );
        let text = serde_json::to_string(&request).map_err(|_| ())?;
        socket
            .send(Message::Text(text.into()))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "daemon link dropped: table page request failed");
            })?;
        let page = tokio::time::timeout(TABLE_PAGE_TIMEOUT, await_page(socket, link, id))
            .await
            .map_err(|_| {
                tracing::warn!("daemon link dropped: table page timed out");
            })??;
        sync.accept(page).map_err(|error| {
            tracing::warn!(?error, "daemon link dropped: table reload failed");
        })?;
    }
    let loaded = sync.into_parts();
    let mut outgoing = Vec::new();
    match header {
        Some(mut full) => {
            if let Some(tasks) = loaded.tasks {
                full.install_tasks(tasks);
            }
            if let Some(queue) = loaded.queue {
                full.install_queue(queue);
            }
            outgoing.push(rebuilt(DaemonEvent::SnapshotReplaced(full)));
        }
        None => {
            if let Some(tasks) = loaded.tasks {
                outgoing.push(rebuilt(DaemonEvent::Engine(WsServerMsg::TasksSnapshot {
                    tasks,
                })));
            }
            if let Some(queue) = loaded.queue {
                let mut table = DaemonSnapshot::default();
                table.install_queue(queue);
                outgoing.push(rebuilt(DaemonEvent::Engine(
                    WsServerMsg::QueuePositionsChanged {
                        positions: table.queue_positions,
                    },
                )));
            }
        }
    }
    for event in outgoing {
        events.send(event).await.map_err(|_| ())?;
    }
    Ok(())
}

/// 等 `id` 对应的分页应答；其间的事件帧缓冲，其它响应路由给在途命令。
async fn await_page(socket: &mut Socket, link: &mut Link, id: i64) -> Result<TablePage, ()> {
    loop {
        match decode_incoming(socket.next().await)? {
            Incoming::Event(frame) => {
                if link.backlog.len() >= HANDSHAKE_BACKLOG_LIMIT {
                    tracing::warn!("daemon link dropped: table reload event backlog");
                    return Err(());
                }
                link.backlog.push_back(*frame);
            }
            Incoming::Response(RpcResponse::Success(success))
                if success.id == RequestId::Integer(id) =>
            {
                return serde_json::from_value::<TablePage>(success.result).map_err(|error| {
                    tracing::warn!(%error, "daemon link dropped: undecodable table page");
                });
            }
            Incoming::Response(RpcResponse::Failure(failure))
                if failure.id == Some(RequestId::Integer(id)) =>
            {
                tracing::warn!(
                    code = failure.error.code,
                    "daemon link dropped: table page refused"
                );
                return Err(());
            }
            Incoming::Response(other) => route_response(&mut link.pending, other),
        }
    }
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
        .map_err(|error| ConnectError::Transient(error.to_string()))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| ConnectError::Transient(error.to_string()))?;
    while let Some(message) = socket.next().await {
        let message = message.map_err(|error| ConnectError::Transient(error.to_string()))?;
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
            if buffered.len() >= HANDSHAKE_BACKLOG_LIMIT {
                return Err(ConnectError::Transient(
                    "handshake event backlog".to_owned(),
                ));
            }
            buffered.push(frame);
            continue;
        }
        let response = serde_json::from_str::<RpcResponse>(&text)
            .map_err(|error| ConnectError::Transient(error.to_string()))?;
        match response {
            RpcResponse::Success(success) if success.id == RequestId::Integer(id) => {
                return Ok(success.result);
            }
            RpcResponse::Failure(failure) if failure.id == Some(RequestId::Integer(id)) => {
                // method-not-found 不带应用错误详情：归一为 `Unsupported`，调用方据此识别
                // 「对端不认识这个方法」。
                let data = failure.error.data.unwrap_or_else(|| {
                    if failure.error.code == METHOD_NOT_FOUND_CODE {
                        RpcErrorData::new(ApplicationErrorCode::Unsupported, false)
                    } else {
                        internal_error()
                    }
                });
                return Err(ConnectError::Fatal(data));
            }
            _ => {}
        }
    }
    Err(ConnectError::Transient("daemon socket closed".to_owned()))
}

fn classify_connect_error(error: tokio_tungstenite::tungstenite::Error) -> ConnectError {
    match &error {
        tokio_tungstenite::tungstenite::Error::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            ConnectError::Refused
        }
        // agent 的升级请求不带任何凭据：只有仍要求升级头 Bearer 的旧版 daemon（或根本不是
        // fluxdownd 的进程）会回 401。无论哪种，都不会拿到 token。
        tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401 => {
            ConnectError::LegacyPeer
        }
        _ => ConnectError::Transient(format!("{error:#}")),
    }
}

/// 对端是否不认识挑战方法：旧版 daemon 的首帧门禁只认 `system.hello`，对其它方法回
/// 「`method` 字段无效」；更旧 / 其它实现回 method-not-found（已归一为 `Unsupported`）。
fn is_unrecognized_handshake(data: &RpcErrorData) -> bool {
    data.code == ApplicationErrorCode::Unsupported
        || (data.code == ApplicationErrorCode::InvalidArgument
            && data.field.as_deref() == Some("method"))
}

/// 每个 agent 进程最多尝试结束旧 daemon 的次数：之后仍被旧 daemon（或冒充者）占着端口就
/// 停止并报协议不兼容，避免反复互杀。
const LEGACY_REPLACE_ATTEMPTS: u32 = 3;

#[derive(Debug, PartialEq, Eq)]
enum LegacyStep {
    /// 结束占着端口的旧 daemon，随后由监管拉起同版本 daemon。
    Replace,
    GiveUp,
}

fn next_legacy_step(attempts_so_far: u32) -> LegacyStep {
    if attempts_so_far < LEGACY_REPLACE_ATTEMPTS {
        LegacyStep::Replace
    } else {
        LegacyStep::GiveUp
    }
}

enum ConnectError {
    Refused,
    /// daemon 尚未就绪：`daemon.token` 还没生成，或本数据目录暂无发布了端点的 daemon。
    NotReady,
    Transient(String),
    /// 对端协议版本不兼容：可尝试让旧进程退出后由监管拉起同版本 daemon。
    Incompatible,
    /// 对端协议兼容但应用版本旧于本 agent：让它退出，由监管拉起同级（同版本）daemon。
    Outdated(String),
    /// 对端是不认识挑战应答握手的旧版 daemon（或冒充者）：不得向它发送 token，只能结束
    /// 同名旧进程后由监管拉起新 daemon。
    LegacyPeer,
    Fatal(RpcErrorData),
}

/// daemon 客户端启动错误。
#[derive(Debug, thiserror::Error)]
pub enum DaemonClientError {
    #[error("daemon client configuration is invalid: {0}")]
    Configuration(String),
}

fn unavailable_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
}

fn unavailable_object() -> RpcErrorObject {
    RpcErrorObject::application("daemon unavailable", unavailable_error())
}

fn internal_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Internal, false)
}

fn protocol_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::ProtocolIncompatible, false)
}

fn unauthorized_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Unauthorized, false)
}

/// 每次握手一组新的 256 位随机数（两个 v4 UUID 的 244 位随机部分，十六进制 64 字符）。
fn fresh_nonce() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn invalid_argument(_message: String) -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fluxdown_protocol::handshake::{
        AuthChallengeParams, AuthChallengeResult, AuthProveParams, SYSTEM_AUTH_CHALLENGE,
        SYSTEM_AUTH_PROVE, http_credential, server_proof, verify_client_proof,
    };
    use fluxdown_protocol::{
        ApplicationErrorCode, METHOD_NOT_FOUND_CODE, RpcErrorData, RpcErrorObject, RpcRequest,
        RpcResponse, method,
    };
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;

    use super::DaemonClient;

    #[tokio::test]
    async fn disconnected_client_rejects_without_queueing_for_replay() {
        let client = DaemonClient::disconnected();
        let result = client
            .call::<serde_json::Value, serde_json::Value>(
                fluxdown_protocol::method::DAEMON_TASK_CREATE,
                Some(serde_json::json!({})),
            )
            .await;
        let error = result.expect_err("disconnected command must fail");
        assert_eq!(error.code, ApplicationErrorCode::Unavailable);
        assert!(error.retryable);
    }

    fn pending_client() -> (
        DaemonClient,
        tokio::sync::mpsc::Receiver<super::ClientCommand>,
        super::SettleOnExit,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let (commands, receiver) = tokio::sync::mpsc::channel(1);
        let settled = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(tokio::sync::Notify::new());
        let client = DaemonClient {
            commands,
            connected: Arc::new(AtomicBool::new(false)),
            settled: settled.clone(),
            ready: ready.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        (client, receiver, super::SettleOnExit { settled, ready })
    }

    #[tokio::test]
    async fn call_before_first_connection_is_delivered_once_connected() {
        let (client, mut commands, settle) = pending_client();
        let caller = client.clone();
        let call = tokio::spawn(async move {
            caller
                .call::<serde_json::Value, serde_json::Value>("task.create", None)
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !call.is_finished(),
            "call must wait for the first connection"
        );
        client
            .connected
            .store(true, std::sync::atomic::Ordering::Release);
        settle.settle();
        let command = commands.recv().await.expect("command delivered");
        assert_eq!(command.method, "task.create");
        command
            .ack
            .send(Ok(serde_json::json!({"ok": true})))
            .expect("waiting caller receives acknowledgement");
        let result = call.await.expect("join").expect("call succeeds");
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn call_fails_fast_when_client_gives_up_or_agent_cancels() {
        let (client, _commands, settle) = pending_client();
        let caller = client.clone();
        let call = tokio::spawn(async move {
            caller
                .call::<serde_json::Value, serde_json::Value>("task.create", None)
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        drop(settle);
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), call)
            .await
            .expect("must not wait for the startup budget")
            .expect("join")
            .expect_err("never connected");
        assert_eq!(error.code, ApplicationErrorCode::Unavailable);

        let (client, _commands, _settle) = pending_client();
        client.cancel.cancel();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.call::<serde_json::Value, serde_json::Value>("task.create", None),
        )
        .await
        .expect("cancelled agent must not wait")
        .expect_err("never connected");
        assert_eq!(error.code, ApplicationErrorCode::Unavailable);
    }

    const TOKEN: &str = "agent-daemon-client-test-token";

    type ServerSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn read_request(ws: &mut ServerSocket) -> RpcRequest {
        use futures_util::StreamExt;
        loop {
            let message = ws.next().await.expect("client frame").expect("frame ok");
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).expect("json-rpc request");
            }
        }
    }

    async fn send_response(ws: &mut ServerSocket, response: RpcResponse) {
        use futures_util::SinkExt;
        ws.send(Message::Text(
            serde_json::to_string(&response).unwrap().into(),
        ))
        .await
        .unwrap();
    }

    fn fresh_server_nonce() -> String {
        format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        )
    }

    /// daemon 一侧的握手：持有 `token`，先证明自己，再校验客户端。返回 (clientNonce, serverNonce)。
    async fn daemon_handshake(ws: &mut ServerSocket, token: &str) -> (String, String) {
        let challenge = read_request(ws).await;
        assert_eq!(challenge.method, SYSTEM_AUTH_CHALLENGE);
        let params: AuthChallengeParams =
            serde_json::from_value(challenge.params.expect("challenge params")).unwrap();
        let server_nonce = fresh_server_nonce();
        let proof = server_proof(token, &params.client_nonce, &server_nonce).expect("proof");
        send_response(
            ws,
            RpcResponse::success(
                challenge.id,
                serde_json::to_value(AuthChallengeResult {
                    server_nonce: server_nonce.clone(),
                    server_proof: proof,
                })
                .unwrap(),
            ),
        )
        .await;
        let prove = read_request(ws).await;
        assert_eq!(prove.method, SYSTEM_AUTH_PROVE);
        let proved: AuthProveParams =
            serde_json::from_value(prove.params.expect("prove params")).unwrap();
        assert!(verify_client_proof(
            token,
            &params.client_nonce,
            &server_nonce,
            &proved.client_proof
        ));
        send_response(
            ws,
            RpcResponse::success(prove.id, json!({ "authenticated": true })),
        )
        .await;
        (params.client_nonce, server_nonce)
    }

    /// 固定端点目标（显式 URL / 嵌入式宿主）。
    fn fixed(url: &str) -> super::DaemonTarget {
        super::DaemonTarget {
            url: url.to_owned(),
            source: super::TargetSource::Fixed,
        }
    }

    async fn loopback_listener() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/rpc", listener.local_addr().unwrap());
        (listener, url)
    }

    /// 转发者合并过的帧覆盖区间可以越过游标（与快照重叠）：接受并推进到 `sequence`；
    /// 覆盖起点在游标之后才是缺口。
    #[tokio::test]
    async fn admit_frame_accepts_coalesced_frames_overlapping_the_cursor() {
        use fluxdown_protocol::{DaemonEvent, DaemonRuntimeStatsDto, EventFrame, ServiceEvent};

        let frame = |sequence: u64, previous: Option<u64>| EventFrame {
            epoch: "epoch".to_owned(),
            sequence,
            previous,
            event: ServiceEvent::Daemon(DaemonEvent::RuntimeStatsChanged(
                DaemonRuntimeStatsDto::default(),
            )),
        };
        let mut cursor = ("epoch".to_owned(), 10_u64);

        // 已被快照覆盖：跳过，不推进。
        assert!(matches!(
            super::admit_frame(&mut cursor, frame(10, None)),
            Ok(None)
        ));
        assert_eq!(cursor.1, 10);
        // 覆盖 (4, 14]，越过游标 10：接受。
        assert!(matches!(
            super::admit_frame(&mut cursor, frame(14, Some(4))),
            Ok(Some(_))
        ));
        assert_eq!(cursor.1, 14);
        // 覆盖 (15, 20]：游标 14 与 15 之间缺了 15 → 缺口。
        assert!(super::admit_frame(&mut cursor, frame(20, Some(15))).is_err());
        // epoch 更换。
        let mut other = frame(15, None);
        other.epoch = "other".to_owned();
        assert!(super::admit_frame(&mut cursor, other).is_err());
    }

    #[tokio::test]
    async fn handshake_buffers_notifications_filters_old_frames_and_reconnects_after_gap() {
        use fluxdown_protocol::{
            DaemonEvent, EventFrame, RpcNotification, ServiceEvent, ServiceHello, ServiceRole,
            Snapshot, SnapshotBody, TaskRuntimeDto,
        };
        use futures_util::SinkExt;

        fn notification(epoch: &str, sequence: u64) -> Message {
            let frame = EventFrame {
                epoch: epoch.into(),
                sequence,
                previous: None,
                event: ServiceEvent::Daemon(DaemonEvent::TaskRuntimeChanged(TaskRuntimeDto {
                    task_id: "task".into(),
                    active_transfers: Some(sequence as u32),
                    ..Default::default()
                })),
            };
            let notification = RpcNotification::new(
                fluxdown_protocol::method::SERVICE_EVENT,
                Some(serde_json::to_value(frame).unwrap()),
            );
            Message::Text(serde_json::to_string(&notification).unwrap().into())
        }

        async fn serve_once(
            listener: &tokio::net::TcpListener,
            epoch: &str,
            sequence: u64,
            gap: bool,
        ) {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            daemon_handshake(&mut ws, TOKEN).await;
            let hello_request = read_request(&mut ws).await;
            assert_eq!(hello_request.method, method::SYSTEM_HELLO);
            if gap {
                ws.send(notification("previous", 3)).await.unwrap();
            }
            let hello =
                ServiceHello::new(ServiceRole::Daemon, "daemon", "test", "instance", vec![]);
            send_response(
                &mut ws,
                RpcResponse::success(hello_request.id, serde_json::to_value(hello).unwrap()),
            )
            .await;
            let snapshot_request = read_request(&mut ws).await;
            assert_eq!(snapshot_request.method, method::SYSTEM_SNAPSHOT);
            ws.send(notification(epoch, sequence.saturating_sub(1)))
                .await
                .unwrap();
            ws.send(notification(epoch, sequence + 1)).await.unwrap();
            let snapshot = Snapshot {
                epoch: epoch.into(),
                sequence,
                body: SnapshotBody::Daemon(Box::default()),
            };
            send_response(
                &mut ws,
                RpcResponse::success(snapshot_request.id, serde_json::to_value(snapshot).unwrap()),
            )
            .await;
            if gap {
                ws.send(notification(epoch, sequence + 2)).await.unwrap();
                ws.send(notification(epoch, sequence + 4)).await.unwrap();
            }
            ws.close(None).await.unwrap();
        }

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                serve_once(&listener, "a", 5, true).await;
                serve_once(&listener, "b", 20, false).await;
            });
            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            let target = fixed(&url);
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            let (command_tx, mut commands) = tokio::sync::mpsc::channel(1);
            let (socket, snapshot, buffered) = super::connect(&config, &target, true)
                .await
                .unwrap_or_else(|_| panic!("connect"));
            assert!(
                config.http_session().access().is_some(),
                "an authenticated session carries an HTTP credential"
            );
            assert_eq!((snapshot.epoch.as_str(), snapshot.sequence), ("a", 5));
            assert!(
                super::run_connected(
                    socket,
                    &mut commands,
                    &tx,
                    (snapshot.epoch, snapshot.sequence),
                    buffered
                )
                .await
                .is_err()
            );
            let first = rx.recv().await.expect("buffered next event");
            let second = rx.recv().await.expect("live next event");
            let [
                super::DaemonClientEvent::Event(first),
                super::DaemonClientEvent::Event(second),
            ] = [first, second]
            else {
                panic!("events")
            };
            assert_eq!((first.sequence, second.sequence), (6, 7));
            assert!(rx.try_recv().is_err(), "gap event must not be delivered");
            let (socket, snapshot, buffered) = super::connect(&config, &target, true)
                .await
                .unwrap_or_else(|_| panic!("reconnect"));
            assert_eq!((snapshot.epoch.as_str(), snapshot.sequence), ("b", 20));
            super::run_connected(
                socket,
                &mut commands,
                &tx,
                (snapshot.epoch, snapshot.sequence),
                buffered,
            )
            .await
            .expect_err("daemon closes the recovered connection");
            let super::DaemonClientEvent::Event(recovered) =
                rx.recv().await.expect("recovered event")
            else {
                panic!("event")
            };
            assert_eq!((recovered.epoch.as_str(), recovered.sequence), ("b", 21));
            drop(command_tx);
            server.await.unwrap();
        })
        .await
        .expect("websocket handshake and recovery");
    }

    /// 快照头不内嵌任务：握手期间经 `system.taskPage` 分页载入（翻页中途到达的变更留在缓冲里
    /// 回放，不丢不重）；运行中 `TablesReloading` 在同一连接上就地重载，在途命令的应答与翻页期间
    /// 到达的帧都不丢，下游收到的是完整的 `TasksSnapshot` 而不是宣告。
    #[tokio::test]
    async fn snapshot_and_reload_page_the_task_table_without_losing_concurrent_changes() {
        use fluxdown_protocol::{
            DaemonEvent, DaemonSnapshot, EventFrame, RpcNotification, ServiceEvent, ServiceHello,
            ServiceRole, Snapshot, SnapshotBody, TablePage, TablePageParams, TableSection, TaskDto,
            WsServerMsg, apply_daemon_event,
        };
        use futures_util::SinkExt;
        use tokio::sync::oneshot;

        fn task(id: &str, status: i32) -> TaskDto {
            serde_json::from_value(serde_json::json!({
                "taskId": id, "url": "https://example.com/f", "fileName": id,
                "saveDir": "/tmp", "status": status, "downloadedBytes": 0,
                "totalBytes": 0, "createdAt": "1"
            }))
            .unwrap()
        }
        fn frame_message(sequence: u64, event: DaemonEvent) -> Message {
            let frame = EventFrame {
                epoch: "e".into(),
                sequence,
                previous: None,
                event: ServiceEvent::Daemon(event),
            };
            let notification = RpcNotification::new(
                fluxdown_protocol::method::SERVICE_EVENT,
                Some(serde_json::to_value(frame).unwrap()),
            );
            Message::Text(serde_json::to_string(&notification).unwrap().into())
        }
        /// 两个一页的「服务端」：不管客户端要多少都只回 2 行，逼出多次翻页。
        fn page(rows: &[&str], sequence: u64, request: &TablePageParams) -> TablePage {
            let mut ids: Vec<&str> = rows.to_vec();
            ids.sort_unstable();
            let rest: Vec<&str> = ids
                .iter()
                .copied()
                .filter(|id| request.cursor.as_deref().is_none_or(|cursor| *id > cursor))
                .collect();
            let take: Vec<&str> = rest.iter().copied().take(2).collect();
            TablePage {
                epoch: "e".into(),
                sequence,
                section: TableSection::Tasks,
                total: ids.len() as u64,
                tasks: take.iter().map(|id| task(id, 2)).collect(),
                queue: Vec::new(),
                next_cursor: (rest.len() > 2)
                    .then(|| take.last().map(|id| (*id).to_owned()))
                    .flatten(),
            }
        }

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                daemon_handshake(&mut ws, TOKEN).await;
                let hello = read_request(&mut ws).await;
                let reply =
                    ServiceHello::new(ServiceRole::Daemon, "daemon", "test", "instance", vec![]);
                send_response(
                    &mut ws,
                    RpcResponse::success(hello.id, serde_json::to_value(reply).unwrap()),
                )
                .await;
                // 头：5 个任务、序号 10。
                let snapshot_request = read_request(&mut ws).await;
                assert_eq!(snapshot_request.method, method::SYSTEM_SNAPSHOT);
                let header = DaemonSnapshot {
                    task_total: Some(5),
                    ..DaemonSnapshot::default()
                };
                let snapshot = Snapshot {
                    epoch: "e".into(),
                    sequence: 10,
                    body: SnapshotBody::Daemon(Box::new(header)),
                };
                ws.send(frame_message(11, DaemonEvent::TaskChanged(task("t9", 0))))
                    .await
                    .unwrap();
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        snapshot_request.id,
                        serde_json::to_value(snapshot).unwrap(),
                    ),
                )
                .await;
                // 握手期翻页：第 1 页后删掉已翻过的 t1、新增排在游标之前的 a0。
                let mut rows = vec!["t0", "t1", "t2", "t3", "t4"];
                let first = read_request(&mut ws).await;
                assert_eq!(first.method, method::SYSTEM_TASK_PAGE);
                let params: TablePageParams =
                    serde_json::from_value(first.params.clone().unwrap()).unwrap();
                assert_eq!(params.cursor, None);
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        first.id,
                        serde_json::to_value(page(&rows, 11, &params)).unwrap(),
                    ),
                )
                .await;
                rows.retain(|id| *id != "t1");
                rows.push("a0");
                ws.send(frame_message(
                    12,
                    DaemonEvent::TaskDeleted {
                        task_id: "t1".into(),
                    },
                ))
                .await
                .unwrap();
                ws.send(frame_message(13, DaemonEvent::TaskChanged(task("a0", 0))))
                    .await
                    .unwrap();
                loop {
                    let request = read_request(&mut ws).await;
                    assert_eq!(request.method, method::SYSTEM_TASK_PAGE);
                    let params: TablePageParams =
                        serde_json::from_value(request.params.clone().unwrap()).unwrap();
                    let reply = page(&rows, 13, &params);
                    let done = reply.next_cursor.is_none();
                    send_response(
                        &mut ws,
                        RpcResponse::success(request.id, serde_json::to_value(reply).unwrap()),
                    )
                    .await;
                    if done {
                        break;
                    }
                }
                // 运行中：先收到一条在途命令，再宣告表重载；翻页期间命令应答与新事件交错到达。
                let command = read_request(&mut ws).await;
                assert_eq!(command.method, "daemon.task.pause");
                ws.send(frame_message(
                    14,
                    DaemonEvent::TablesReloading {
                        tasks: Some(3),
                        queue: None,
                    },
                ))
                .await
                .unwrap();
                let reload_rows = ["r0", "r1", "r2"];
                let request = read_request(&mut ws).await;
                assert_eq!(request.method, method::SYSTEM_TASK_PAGE);
                ws.send(frame_message(15, DaemonEvent::TaskChanged(task("r3", 0))))
                    .await
                    .unwrap();
                send_response(
                    &mut ws,
                    RpcResponse::success(command.id, serde_json::json!({ "ok": true })),
                )
                .await;
                let params: TablePageParams =
                    serde_json::from_value(request.params.clone().unwrap()).unwrap();
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        request.id,
                        serde_json::to_value(page(&reload_rows, 15, &params)).unwrap(),
                    ),
                )
                .await;
                let request = read_request(&mut ws).await;
                let params: TablePageParams =
                    serde_json::from_value(request.params.clone().unwrap()).unwrap();
                assert_eq!(params.cursor.as_deref(), Some("r1"));
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        request.id,
                        serde_json::to_value(page(&reload_rows, 15, &params)).unwrap(),
                    ),
                )
                .await;
                ws.close(None).await.unwrap();
            });

            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            let (socket, snapshot, buffered) = super::connect(&config, &fixed(&url), true)
                .await
                .unwrap_or_else(|_| panic!("connect"));
            let SnapshotBody::Daemon(mut daemon) = snapshot.body else {
                panic!("daemon snapshot");
            };
            assert_eq!(
                daemon.task_total, None,
                "paging completes before the snapshot is handed over"
            );
            assert_eq!(daemon.tasks.len(), 5);
            // 握手期缓冲的帧（11..13）按快照序号回放后收敛到服务端当前状态。
            let (tx, mut rx) = tokio::sync::mpsc::channel(32);
            let (command_tx, mut commands) = tokio::sync::mpsc::channel(1);
            let (ack, answer) = oneshot::channel();
            command_tx
                .send(super::ClientCommand {
                    method: "daemon.task.pause".to_owned(),
                    params: None,
                    ack,
                })
                .await
                .unwrap();
            let outcome = super::run_connected(
                socket,
                &mut commands,
                &tx,
                (snapshot.epoch, snapshot.sequence),
                buffered,
            )
            .await;
            assert!(outcome.is_err(), "daemon closes the connection at the end");
            assert!(
                answer.await.expect("ack delivered").is_ok(),
                "the in-flight command is answered while the table reloads"
            );
            let mut forwarded = Vec::new();
            while let Ok(event) = rx.try_recv() {
                let super::DaemonClientEvent::Event(frame) = event else {
                    panic!("only events are forwarded");
                };
                forwarded.push(*frame);
            }
            let sequences: Vec<u64> = forwarded.iter().map(|frame| frame.sequence).collect();
            assert_eq!(sequences, [11, 12, 13, 14, 15]);
            for frame in &forwarded[..3] {
                let ServiceEvent::Daemon(event) = &frame.event else {
                    panic!("daemon event");
                };
                apply_daemon_event(&mut daemon, event);
            }
            let mut ids: Vec<&str> = daemon.tasks.iter().map(|t| t.task_id.as_str()).collect();
            ids.sort_unstable();
            assert_eq!(ids, ["a0", "t0", "t2", "t3", "t4", "t9"]);
            // 宣告被还原成完整的整表事件，随后是翻页期间到达的帧。
            let ServiceEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks })) =
                &forwarded[3].event
            else {
                panic!("TablesReloading must be forwarded as a full TasksSnapshot");
            };
            assert_eq!(
                tasks.iter().map(|t| t.task_id.as_str()).collect::<Vec<_>>(),
                ["r0", "r1", "r2"]
            );
            assert!(matches!(
                &forwarded[4].event,
                ServiceEvent::Daemon(DaemonEvent::TaskChanged(task)) if task.task_id == "r3"
            ));
            server.await.unwrap();
        })
        .await
        .expect("paged snapshot and reload");
    }

    /// 插件设置保存失败时 daemon 以 `field` 标出出错的设置项；经 agent 转发给界面时必须原样保留。
    #[tokio::test]
    async fn daemon_error_keeps_field_and_reason_when_forwarded() {
        use fluxdown_protocol::{
            ErrorReason, RpcErrorData, ServiceHello, ServiceRole, Snapshot, SnapshotBody,
        };

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                daemon_handshake(&mut ws, TOKEN).await;
                let hello = read_request(&mut ws).await;
                let reply =
                    ServiceHello::new(ServiceRole::Daemon, "daemon", "test", "instance", vec![]);
                send_response(
                    &mut ws,
                    RpcResponse::success(hello.id, serde_json::to_value(reply).unwrap()),
                )
                .await;
                let snapshot_request = read_request(&mut ws).await;
                let snapshot = Snapshot {
                    epoch: "e".into(),
                    sequence: 1,
                    body: SnapshotBody::Daemon(Box::default()),
                };
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        snapshot_request.id,
                        serde_json::to_value(snapshot).unwrap(),
                    ),
                )
                .await;
                let call = read_request(&mut ws).await;
                assert_eq!(call.method, method::DAEMON_PLUGIN_UPDATE_SETTINGS);
                send_response(
                    &mut ws,
                    RpcResponse::failure(
                        call.id,
                        RpcErrorObject::application(
                            "invalid setting",
                            RpcErrorData {
                                field: Some("maxItems".to_owned()),
                                reason: Some(ErrorReason::PluginPackageInvalid),
                                ..RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
                            },
                        ),
                    ),
                )
                .await;
            });
            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            let (socket, snapshot, buffered) = super::connect(&config, &fixed(&url), true)
                .await
                .unwrap_or_else(|_| panic!("connect"));
            let (tx, _rx) = tokio::sync::mpsc::channel(16);
            let (command_tx, mut commands) = tokio::sync::mpsc::channel(1);
            let runner = tokio::spawn(async move {
                super::run_connected(
                    socket,
                    &mut commands,
                    &tx,
                    (snapshot.epoch, snapshot.sequence),
                    buffered,
                )
                .await
            });
            let (ack, response) = tokio::sync::oneshot::channel();
            command_tx
                .send(super::ClientCommand {
                    method: method::DAEMON_PLUGIN_UPDATE_SETTINGS.to_owned(),
                    params: Some(json!({ "pluginId": "p", "entries": {} })),
                    ack,
                })
                .await
                .unwrap();
            let error = response.await.unwrap().unwrap_err();
            let data = error.data.expect("application error data");
            assert_eq!(data.code, ApplicationErrorCode::InvalidArgument);
            assert_eq!(data.field.as_deref(), Some("maxItems"));
            assert_eq!(data.reason, Some(ErrorReason::PluginPackageInvalid));
            runner
                .await
                .expect("join daemon connection")
                .expect_err("daemon closes after returning its error");
            drop(command_tx);
            server.await.unwrap();
        })
        .await
        .expect("daemon error forwarding");
    }

    /// 调用方放弃等待可中止的慢调用（AI 测速被停止）时，agent 必须请 daemon 中止它，
    /// 否则测速会在后台继续消耗流量。
    #[tokio::test]
    async fn abandoned_cancellable_call_is_cancelled_on_the_daemon() {
        use fluxdown_protocol::{
            CancelRequestParams, ServiceHello, ServiceRole, Snapshot, SnapshotBody,
        };

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                daemon_handshake(&mut ws, TOKEN).await;
                let hello = read_request(&mut ws).await;
                let reply =
                    ServiceHello::new(ServiceRole::Daemon, "daemon", "test", "instance", vec![]);
                send_response(
                    &mut ws,
                    RpcResponse::success(hello.id, serde_json::to_value(reply).unwrap()),
                )
                .await;
                let snapshot_request = read_request(&mut ws).await;
                let snapshot = Snapshot {
                    epoch: "e".into(),
                    sequence: 1,
                    body: SnapshotBody::Daemon(Box::default()),
                };
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        snapshot_request.id,
                        serde_json::to_value(snapshot).unwrap(),
                    ),
                )
                .await;
                let measure = read_request(&mut ws).await;
                assert_eq!(measure.method, method::DAEMON_NET_MEASURE_ROUTES);
                let cancel = read_request(&mut ws).await;
                assert_eq!(cancel.method, method::SYSTEM_CANCEL);
                let params: CancelRequestParams =
                    serde_json::from_value(cancel.params.expect("cancel params")).unwrap();
                assert_eq!(params.id, measure.id);
            });
            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            let (socket, snapshot, buffered) = super::connect(&config, &fixed(&url), true)
                .await
                .unwrap_or_else(|_| panic!("connect"));
            let (tx, _rx) = tokio::sync::mpsc::channel(16);
            let (command_tx, mut commands) = tokio::sync::mpsc::channel(1);
            let runner = tokio::spawn(async move {
                super::run_connected(
                    socket,
                    &mut commands,
                    &tx,
                    (snapshot.epoch, snapshot.sequence),
                    buffered,
                )
                .await
            });
            let (ack, response) = tokio::sync::oneshot::channel();
            command_tx
                .send(super::ClientCommand {
                    method: method::DAEMON_NET_MEASURE_ROUTES.to_owned(),
                    params: Some(json!({})),
                    ack,
                })
                .await
                .unwrap();
            drop(response);
            server.await.unwrap();
            // 命令通道保持打开：链路只能因 daemon 断开而结束（关闭通道会让循环走正常收尾分支）。
            runner
                .await
                .expect("join daemon connection")
                .expect_err("daemon closed the link after the cancel");
            drop(command_tx);
        })
        .await
        .expect("abandoned call cancellation");
    }

    /// 快照随任务数增长且单帧发出；超过 tungstenite 默认 16 MiB 帧上限时连接仍须建立
    /// （现网 38 MB 快照即因此在旧客户端永远重连失败）。
    #[tokio::test]
    async fn snapshot_larger_than_default_frame_limit_is_accepted() {
        use fluxdown_protocol::{
            DaemonConfigSnapshot, DaemonSnapshot, ServiceHello, ServiceRole, Snapshot, SnapshotBody,
        };

        const BIG: usize = 24 * 1024 * 1024;
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                daemon_handshake(&mut ws, TOKEN).await;
                let hello = read_request(&mut ws).await;
                let reply =
                    ServiceHello::new(ServiceRole::Daemon, "daemon", "test", "instance", vec![]);
                send_response(
                    &mut ws,
                    RpcResponse::success(hello.id, serde_json::to_value(reply).unwrap()),
                )
                .await;
                let snapshot_request = read_request(&mut ws).await;
                let snapshot = Snapshot {
                    epoch: "e".into(),
                    sequence: 1,
                    body: SnapshotBody::Daemon(Box::new(DaemonSnapshot {
                        config: DaemonConfigSnapshot {
                            revision: 1,
                            values: [("big".to_owned(), "x".repeat(BIG))].into(),
                        },
                        ..DaemonSnapshot::default()
                    })),
                };
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        snapshot_request.id,
                        serde_json::to_value(snapshot).unwrap(),
                    ),
                )
                .await;
                ws
            });
            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            let (_socket, snapshot, _buffered) = super::connect(&config, &fixed(&url), true)
                .await
                .unwrap_or_else(|_| panic!("oversized snapshot must not break the handshake"));
            let SnapshotBody::Daemon(daemon) = snapshot.body else {
                panic!("daemon snapshot");
            };
            assert_eq!(daemon.config.values.get("big").map(String::len), Some(BIG));
            server.await.unwrap();
        })
        .await
        .expect("oversized snapshot handshake");
    }

    #[tokio::test]
    async fn authentication_derives_the_session_credential_from_a_daemon_that_proves_the_token() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                daemon_handshake(&mut ws, TOKEN).await
            });
            let mut socket = super::open_socket(&fixed(&url))
                .await
                .unwrap_or_else(|_| panic!("open"));
            let mut buffered = Vec::new();
            let credential = super::authenticate(&mut socket, TOKEN, &mut buffered)
                .await
                .unwrap_or_else(|_| panic!("handshake"));
            let (client_nonce, server_nonce) = server.await.unwrap();
            assert_eq!(
                Some(credential),
                http_credential(TOKEN, &client_nonce, &server_nonce)
            );
        })
        .await
        .expect("handshake completes");
    }

    #[tokio::test]
    // tungstenite 的升级回调签名固定返回 `Result<Response, ErrorResponse>`。
    #[allow(clippy::result_large_err)]
    async fn impostor_daemon_learns_no_credential_and_never_sees_the_token() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (listener, url) = loopback_listener().await;
            let upgrade_headers = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let frames = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let server = tokio::spawn({
                let upgrade_headers = upgrade_headers.clone();
                let frames = frames.clone();
                async move {
                    // 一次给 `connect`，一次给 `request_shutdown`：两条路径都不能漏凭据。
                    for _ in 0..2 {
                        let (tcp, _) = listener.accept().await.unwrap();
                        let headers = upgrade_headers.clone();
                        let mut ws = tokio_tungstenite::accept_hdr_async(
                            tcp,
                            move |request: &Request, response: Response| {
                                for (name, value) in request.headers() {
                                    headers.lock().unwrap().push(format!(
                                        "{name}: {}",
                                        value.to_str().unwrap_or_default()
                                    ));
                                }
                                Ok(response)
                            },
                        )
                        .await
                        .unwrap();
                        let challenge = read_request(&mut ws).await;
                        frames
                            .lock()
                            .unwrap()
                            .push(serde_json::to_string(&challenge).unwrap());
                        let params: AuthChallengeParams =
                            serde_json::from_value(challenge.params.expect("params")).unwrap();
                        // 冒充者不知道 token，只能拿猜测值算证明。
                        let server_nonce = fresh_server_nonce();
                        let forged = server_proof(
                            "a-guess-of-the-token",
                            &params.client_nonce,
                            &server_nonce,
                        )
                        .unwrap();
                        send_response(
                            &mut ws,
                            RpcResponse::success(
                                challenge.id,
                                serde_json::to_value(AuthChallengeResult {
                                    server_nonce,
                                    server_proof: forged,
                                })
                                .unwrap(),
                            ),
                        )
                        .await;
                        // 客户端若继续发帧，全部记录下来。
                        while let Ok(Some(Ok(message))) =
                            tokio::time::timeout(std::time::Duration::from_millis(300), ws.next())
                                .await
                        {
                            if let Message::Text(text) = message {
                                frames.lock().unwrap().push(text.to_string());
                            }
                        }
                    }
                }
            });
            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            let Err(super::ConnectError::Fatal(data)) =
                super::connect(&config, &fixed(&url), true).await
            else {
                panic!("an endpoint that cannot prove the token must be rejected");
            };
            assert_eq!(data.code, ApplicationErrorCode::Unauthorized);
            assert!(config.http_session().access().is_none());
            assert!(super::request_shutdown(&config).await.is_err());
            server.await.unwrap();

            let frames = frames.lock().unwrap();
            assert_eq!(
                frames.len(),
                2,
                "only the challenges reach the impostor: {frames:?}"
            );
            for frame in frames.iter() {
                assert!(frame.contains(SYSTEM_AUTH_CHALLENGE), "{frame}");
                assert!(!frame.contains(TOKEN), "{frame}");
                assert!(!frame.contains("clientProof"), "{frame}");
                assert!(!frame.contains(method::SYSTEM_SHUTDOWN), "{frame}");
            }
            let headers = upgrade_headers.lock().unwrap();
            assert!(
                !headers
                    .iter()
                    .any(|header| header.to_ascii_lowercase().starts_with("authorization")),
                "{headers:?}"
            );
            assert!(!headers.iter().any(|header| header.contains(TOKEN)));
        })
        .await
        .expect("impostor is rejected without leaking credentials");
    }

    #[tokio::test]
    async fn daemon_proof_recorded_from_another_session_is_not_accepted() {
        use futures_util::StreamExt;

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let (client_nonce, server_nonce) = daemon_handshake(&mut ws, TOKEN).await;
                let recorded = server_proof(TOKEN, &client_nonce, &server_nonce).unwrap();

                // 第二条连接：攻击者回放录下来的（serverNonce, serverProof），客户端的新随机数不同。
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let challenge = read_request(&mut ws).await;
                send_response(
                    &mut ws,
                    RpcResponse::success(
                        challenge.id,
                        serde_json::to_value(AuthChallengeResult {
                            server_nonce,
                            server_proof: recorded,
                        })
                        .unwrap(),
                    ),
                )
                .await;
                let mut later_frames = 0;
                while let Ok(Some(Ok(_))) =
                    tokio::time::timeout(std::time::Duration::from_millis(300), ws.next()).await
                {
                    later_frames += 1;
                }
                later_frames
            });
            let target = fixed(&url);
            let mut buffered = Vec::new();
            let mut first = super::open_socket(&target)
                .await
                .unwrap_or_else(|_| panic!("open first"));
            assert!(
                super::authenticate(&mut first, TOKEN, &mut buffered)
                    .await
                    .is_ok()
            );
            let mut second = super::open_socket(&target)
                .await
                .unwrap_or_else(|_| panic!("open second"));
            let Err(super::ConnectError::Fatal(data)) =
                super::authenticate(&mut second, TOKEN, &mut buffered).await
            else {
                panic!("replayed proof must be rejected");
            };
            assert_eq!(data.code, ApplicationErrorCode::Unauthorized);
            assert_eq!(
                server.await.unwrap(),
                0,
                "no client proof after a bad server proof"
            );
        })
        .await
        .expect("replay is rejected");
    }

    #[tokio::test]
    async fn shutdown_request_is_sent_only_after_the_daemon_is_authenticated() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                daemon_handshake(&mut ws, TOKEN).await;
                let shutdown = read_request(&mut ws).await;
                send_response(
                    &mut ws,
                    RpcResponse::success(shutdown.id, json!({ "ok": true })),
                )
                .await;
                shutdown.method
            });
            let config = super::DaemonClientConfig::new(url, TOKEN);
            assert!(super::request_shutdown(&config).await.is_ok());
            assert_eq!(server.await.unwrap(), method::SYSTEM_SHUTDOWN);
        })
        .await
        .expect("shutdown after handshake");
    }

    #[tokio::test]
    async fn legacy_daemon_rejecting_the_upgrade_is_identified_without_sending_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let mut received = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = tcp.read(&mut chunk).await.unwrap();
                    assert!(
                        read > 0,
                        "client closed before finishing the upgrade request"
                    );
                    received.extend_from_slice(&chunk[..read]);
                }
                tcp.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
                String::from_utf8_lossy(&received).to_ascii_lowercase()
            });
            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            let Err(super::ConnectError::LegacyPeer) =
                super::connect(&config, &fixed(&url), true).await
            else {
                panic!("an upgrade rejected with 401 marks a legacy daemon");
            };
            let request = server.await.unwrap();
            assert!(!request.contains("authorization"), "{request}");
            assert!(!request.contains(TOKEN), "{request}");
            assert!(config.http_session().access().is_none());
        })
        .await
        .expect("legacy daemon is identified");
    }

    #[tokio::test]
    async fn daemon_that_does_not_know_the_handshake_is_identified_as_legacy() {
        use futures_util::StreamExt;

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (listener, url) = loopback_listener().await;
            let server = tokio::spawn(async move {
                let mut methods = Vec::new();
                for error in [
                    // 旧版 daemon 的首帧门禁：只认 hello，对其它方法回「method 字段无效」。
                    RpcErrorObject::application(
                        "hello rejected",
                        RpcErrorData {
                            field: Some("method".to_owned()),
                            ..RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
                        },
                    ),
                    RpcErrorObject {
                        code: METHOD_NOT_FOUND_CODE,
                        message: "method not found".to_owned(),
                        data: None,
                    },
                ] {
                    let (tcp, _) = listener.accept().await.unwrap();
                    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                    let challenge = read_request(&mut ws).await;
                    methods.push(challenge.method);
                    send_response(&mut ws, RpcResponse::failure(challenge.id, error)).await;
                    while let Ok(Some(Ok(message))) =
                        tokio::time::timeout(std::time::Duration::from_millis(200), ws.next()).await
                    {
                        if let Message::Text(text) = message {
                            methods.push(text.to_string());
                        }
                    }
                }
                methods
            });
            let config = super::DaemonClientConfig::new(url.clone(), TOKEN);
            for _ in 0..2 {
                let Err(super::ConnectError::LegacyPeer) =
                    super::connect(&config, &fixed(&url), true).await
                else {
                    panic!("a peer that does not know the challenge is a legacy daemon");
                };
            }
            // 对端只看到过挑战，没有任何证明、token 或后续帧。
            assert_eq!(
                server.await.unwrap(),
                vec![SYSTEM_AUTH_CHALLENGE, SYSTEM_AUTH_CHALLENGE]
            );
            assert!(config.http_session().access().is_none());
        })
        .await
        .expect("unrecognized handshake is identified");
    }

    #[test]
    fn legacy_peer_detection_is_narrow_and_replacement_is_bounded() {
        use tokio_tungstenite::tungstenite::{Error, http::Response};

        let rejected = |status: u16| {
            Error::Http(Box::new(
                Response::builder().status(status).body(None).unwrap(),
            ))
        };
        assert!(matches!(
            super::classify_connect_error(rejected(401)),
            super::ConnectError::LegacyPeer
        ));
        for status in [403, 404, 500, 503] {
            assert!(
                matches!(
                    super::classify_connect_error(rejected(status)),
                    super::ConnectError::Transient(_)
                ),
                "{status}"
            );
        }

        let field = |field: &str| RpcErrorData {
            field: Some(field.to_owned()),
            ..RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
        };
        assert!(super::is_unrecognized_handshake(&field("method")));
        assert!(super::is_unrecognized_handshake(&RpcErrorData::new(
            ApplicationErrorCode::Unsupported,
            false
        )));
        // 本版 daemon 自己的拒绝（坏随机数 / 证明不符）不是旧版 daemon。
        assert!(!super::is_unrecognized_handshake(&field("clientNonce")));
        assert!(!super::is_unrecognized_handshake(&RpcErrorData::new(
            ApplicationErrorCode::Unauthorized,
            false
        )));
        assert!(!super::is_unrecognized_handshake(&RpcErrorData::new(
            ApplicationErrorCode::InvalidArgument,
            false
        )));

        for attempts in 0..super::LEGACY_REPLACE_ATTEMPTS {
            assert_eq!(
                super::next_legacy_step(attempts),
                super::LegacyStep::Replace
            );
        }
        assert_eq!(
            super::next_legacy_step(super::LEGACY_REPLACE_ATTEMPTS),
            super::LegacyStep::GiveUp
        );
    }

    fn discovery_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-daemon-discovery-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create discovery dir");
        dir
    }

    fn publish(dir: &std::path::Path, rpc_url: &str) {
        std::fs::write(
            dir.join(fluxdown_protocol::local_service::DAEMON_ENDPOINT_FILE),
            serde_json::to_vec(&fluxdown_protocol::local_service::LocalEndpoint {
                rpc_url: rpc_url.to_owned(),
                pipe: None,
            })
            .expect("endpoint json"),
        )
        .expect("publish endpoint");
    }

    fn resolved(
        resolution: Result<super::Resolution, super::ConnectError>,
    ) -> Option<(String, super::TargetSource)> {
        match resolution {
            Ok(super::Resolution::Connect(target)) => Some((target.url, target.source)),
            Ok(super::Resolution::NotReady) => None,
            Err(error) => panic!(
                "unexpected resolution error: {}",
                super::describe_connect_error(error)
            ),
        }
    }

    /// 端点发现：已发布的端点优先；未发布时只有本数据目录的 daemon 持有租约（旧版 daemon /
    /// 仍在启动）才连默认地址，否则不碰默认端口上可能属于别的用户或程序的进程。
    #[tokio::test]
    async fn discovery_prefers_the_published_endpoint_and_guards_the_default_port() {
        let dir = discovery_dir("resolve");
        let config = super::DaemonClientConfig::discovered(None, dir.join("daemon.token"), &dir);

        // 没有端点也没有租约：本数据目录没有在运行的 daemon。
        assert_eq!(resolved(config.resolve(false).await), None);
        // 本进程拉起的 daemon 仍在启动：等它发布端点。
        assert_eq!(resolved(config.resolve(true).await), None);

        // daemon 持有租约却没发布端点（旧版 daemon）：连默认地址。
        let lease = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("daemon.lock"))
            .expect("open lease");
        lease.try_lock().expect("hold lease");
        assert_eq!(
            resolved(config.resolve(false).await),
            Some((
                super::DEFAULT_DAEMON_RPC_URL.to_owned(),
                super::TargetSource::Default
            ))
        );
        drop(lease);

        publish(&dir, "ws://127.0.0.1:45678/rpc");
        assert_eq!(
            resolved(config.resolve(true).await),
            Some((
                "ws://127.0.0.1:45678/rpc".to_owned(),
                super::TargetSource::Published
            ))
        );
        assert_eq!(config.endpoint_label(), "ws://127.0.0.1:45678/rpc");

        // 非回环地址绝不连接：握手证明不能发往别的机器。
        publish(&dir, "ws://192.0.2.10:45678/rpc");
        assert!(matches!(
            config.resolve(false).await,
            Err(super::ConnectError::Transient(_))
        ));

        // 显式地址不读端点文件。
        let explicit = super::DaemonClientConfig::discovered(
            Some("ws://127.0.0.1:9/rpc".to_owned()),
            dir.join("daemon.token"),
            &dir,
        );
        assert_eq!(
            resolved(explicit.resolve(false).await),
            Some((
                "ws://127.0.0.1:9/rpc".to_owned(),
                super::TargetSource::Fixed
            ))
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// 独立 daemon 首次启动、尚未生成 token 时不建连，按「未就绪」等它生成。
    #[tokio::test]
    async fn missing_or_blank_token_file_means_the_daemon_is_not_ready() {
        let dir = discovery_dir("token");
        let token = dir.join("daemon.token");
        let config = super::DaemonClientConfig::discovered(None, token.clone(), &dir);
        assert!(matches!(
            config.load_token().await,
            Err(super::ConnectError::NotReady)
        ));
        std::fs::write(&token, "  \n").expect("blank token");
        assert!(matches!(
            config.load_token().await,
            Err(super::ConnectError::NotReady)
        ));
        std::fs::write(&token, format!("{TOKEN}\n")).expect("token");
        assert_eq!(config.load_token().await.ok().as_deref(), Some(TOKEN));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// 已发布端点上换成了别的进程（daemon 崩溃后端口被复用）：视为端点过期继续重试，而不是
    /// 当作冒充者让 agent 退出；daemon 重新发布后客户端跟着新端点连上。
    #[tokio::test]
    async fn stale_published_endpoint_is_retried_until_the_daemon_republishes() {
        use fluxdown_protocol::{ServiceHello, ServiceRole, Snapshot, SnapshotBody};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let dir = discovery_dir("stale");
            std::fs::write(dir.join("daemon.token"), format!("{TOKEN}\n")).expect("token");
            let (foreign, foreign_url) = loopback_listener().await;
            publish(&dir, &foreign_url);
            let foreign_hit = tokio::spawn(async move {
                let (mut stream, _) = foreign.accept().await.expect("accept foreign");
                let mut buffer = [0_u8; 2048];
                let read = stream.read(&mut buffer).await.expect("read upgrade");
                assert!(read > 0);
                stream
                    .write_all(
                        b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await
                    .expect("reject upgrade");
            });
            let (daemon, daemon_url) = loopback_listener().await;
            let supervisor = Arc::new(crate::supervisor::DaemonSupervisor::new(None));
            supervisor.stop();
            let config =
                super::DaemonClientConfig::discovered(None, dir.join("daemon.token"), &dir);
            let (_client, mut events) = DaemonClient::start(
                config,
                supervisor,
                tokio_util::sync::CancellationToken::new(),
            )
            .expect("start client");
            foreign_hit.await.expect("foreign peer contacted");
            publish(&dir, &daemon_url);

            let (tcp, _) = daemon.accept().await.expect("accept daemon");
            let mut ws = tokio_tungstenite::accept_async(tcp).await.expect("ws accept");
            daemon_handshake(&mut ws, TOKEN).await;
            let hello = read_request(&mut ws).await;
            assert_eq!(hello.method, method::SYSTEM_HELLO);
            let service = ServiceHello::new(ServiceRole::Daemon, "daemon", "test", "instance", vec![]);
            send_response(
                &mut ws,
                RpcResponse::success(hello.id, serde_json::to_value(service).expect("hello")),
            )
            .await;
            let snapshot_request = read_request(&mut ws).await;
            let snapshot = Snapshot {
                epoch: "republished".into(),
                sequence: 1,
                body: SnapshotBody::Daemon(Box::default()),
            };
            send_response(
                &mut ws,
                RpcResponse::success(
                    snapshot_request.id,
                    serde_json::to_value(snapshot).expect("snapshot"),
                ),
            )
            .await;
            match events.recv().await {
                Some(super::DaemonClientEvent::Snapshot(snapshot)) => {
                    assert_eq!(snapshot.epoch, "republished");
                }
                Some(super::DaemonClientEvent::Fatal(error)) => {
                    panic!("a stale endpoint must not be fatal: {:?}", error.code)
                }
                _ => panic!("expected the republished daemon's snapshot"),
            }
            std::fs::remove_dir_all(&dir).expect("cleanup");
        })
        .await
        .expect("client follows the republished endpoint");
    }
}
