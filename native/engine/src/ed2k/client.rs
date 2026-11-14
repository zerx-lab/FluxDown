//! eD2K 客户端核心 —— 进程级共享会话：持久服务器连接、HighID 监听、
//! LowID callback 中转、源发现聚合。
//!
//! 设计动机：HighID 监听 socket、LowID callback 的入站匹配、以及（后续）
//! Kad UDP 都是**进程级单例**资源，不能每个下载任务各建一份。本模块把这些
//! 资源收敛到一个 [`Ed2kClient`]，由 [`shared_client`] 惰性初始化、全局复用。
//!
//! ## 与旧 `find_sources` 的关系
//!
//! 旧路径 [`crate::ed2k::server::find_sources`] 是**一次性**的（连服务器→查一次
//! →断开），且**丢弃所有 LowID 源**。本模块的 [`Ed2kClient::find_sources`]：
//! - 维持**持久**服务器会话（登录一次，后续复用，掉线重连）；
//! - 用**真实监听端口**登录 → 争取 HighID（可被动接收入站连接）；
//! - **保留 LowID 源** —— 通过 [`Ed2kClient::connect_source`] 的 callback 中转
//!   建立连接，不再丢弃（这是「源稀少」的直接修复）。
//!
//! ## 源的两类
//!
//! [`Source::HighId`]：可直连（`TcpStream::connect`）。
//! [`Source::LowId`]：NAT 后 peer，需经服务器 `OP_CALLBACKREQUEST` 请求其回连；
//! 回连的入站流在监听器里按 `client_id` 匹配后交回等待方。

use std::collections::{HashSet, VecDeque};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
use tokio::task::JoinSet;

use crate::downloader::DownloadError;
use crate::ed2k::proto::{
    self, Ed2kMessage, LOWID_THRESHOLD, MAX_SERVER_FRAME, OP_CALLBACK_FAIL, OP_CALLBACKREQUEST,
    OP_GETSOURCES, OP_HELLO, OP_HELLOANSWER, OP_LOGINREQUEST,
};
use crate::ed2k::server::{
    PeerAddr, build_getsources_payload, build_login_payload, id_to_ipv4, read_until_found_sources,
    read_until_id_change,
};
use crate::ed2k::status::{ClientNetwork, IdKind, UpnpState};
use crate::ed2k::upnp::{MappingOutcome, UpnpMapping};
use crate::logger::{log_error, log_info};

/// 单次服务器登录超时。
const SERVER_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

/// 入站 callback 等待超时。
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// 单个 GETSOURCES 查询超时。
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// 对 HighID 源直连的超时（住宅源大量下线是常态，必须快速轮转）。
const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 并发找源时最多同时查询的服务器数（不同服务器索引不同文件，
/// 单服务器常常没有目标文件 → 必须多服务器并发聚合，见 goed2k
/// "多个 ED2K server 并发找源"）。
const MAX_QUERY_SERVERS: usize = 12;

/// HighID / LowID 检测等待映射结果的上限（SSDP 一轮搜索约 10s，之后才取设备描述并映射）。
const UPNP_SETTLE_TIMEOUT: Duration = Duration::from_secs(15);
const UPNP_SETTLE_POLL: Duration = Duration::from_millis(200);
/// 映射迟迟不成功时观察结果的间隔（映射器自身每 60s 重新发现、重试）。
const UPNP_WATCH_POLL: Duration = Duration::from_secs(30);

/// 一个已发现的源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// 可直连的 HighID peer。
    HighId(PeerAddr),
    /// NAT 后 LowID peer：`client_id` 用于向服务器请求 callback 中转。
    LowId(u32),
}

/// 单台服务器的原始应答：`(host:port, 服务器返回的 (client_id, port) 列表 | 失败)`。
type ServerAnswer = (String, Result<Vec<(u32, u16)>, DownloadError>);

/// [`Ed2kClient::check_id`] 的检测目标。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum IdCheckTarget {
    /// 已有服务器会话时报告它，否则同 [`IdCheckTarget::List`]。
    #[default]
    Auto,
    /// 只报告当前会话，不发起连接。
    Session,
    /// 本轮找源会查询的服务器（手填 + 订阅）。
    List,
    /// 指定的一台服务器 `host:port`。
    Server(String),
}

/// 一台服务器的找源结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerQuery {
    /// `host:port`。
    pub addr: String,
    /// 返回的源数（跨服务器去重前），或登录 / 查询失败的原因。
    pub result: Result<u32, String>,
}

/// [`Ed2kClient::find_sources`] 的结果：聚合去重后的源与各服务器的应答情况。
#[derive(Debug, Clone, Default)]
pub struct SourceQuery {
    pub sources: Vec<Source>,
    pub servers: Vec<ServerQuery>,
}

/// 客户端运行期配置（来自 DB config，由引擎宿主注入）。
#[derive(Debug, Clone, Default)]
pub struct ClientConfig {
    /// TCP 监听端口（0 = 让 OS 选，登录时报实际绑定端口争取 HighID）。
    pub listen_port: u16,
    /// 服务器地址列表（`host:port`，手填+订阅合并后，手填在前）。
    pub servers: Vec<String>,
    /// `servers` 开头有多少项来自手填列表（其余来自订阅）。
    pub manual_servers: usize,
    /// 是否启用 UPnP 端口映射（争取 NAT 后 HighID）。
    pub enable_upnp: bool,
    /// 是否启用 Kad DHT 找源。
    pub enable_kad: bool,
}

/// 端口映射的句柄与启停代号。
#[derive(Default)]
struct UpnpSlot {
    /// 运行中的映射；`None` = 未启用或监听器尚未就绪。
    mapping: Option<UpnpMapping>,
    /// 关闭设置时停下的映射器的后台任务：下次启动等它删完旧映射。
    retired: Option<tokio::task::JoinHandle<()>>,
    /// 每次启停递增；上一代的观察任务据此退出。
    generation: u64,
}

impl UpnpSlot {
    fn outcome(&self) -> Option<MappingOutcome> {
        self.mapping.as_ref().map(UpnpMapping::outcome)
    }
}

/// 队列顺序与服务器写入顺序一致；失败包不携带 client_id。
struct PendingCallback {
    id: u32,
    token: u64,
    session: u64,
    sender: oneshot::Sender<Result<TcpStream, DownloadError>>,
}

type PendingCallbacks = Arc<StdMutex<VecDeque<PendingCallback>>>;

struct CallbackRegistration<'a> {
    pending: &'a PendingCallbacks,
    token: u64,
}

impl Drop for CallbackRegistration<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|entry| entry.token != self.token);
        }
    }
}

/// 活跃任务的生命周期决定后台重连是否运行。
pub(crate) struct ActiveTask(Arc<Ed2kClient>);

impl Drop for ActiveTask {
    fn drop(&mut self) {
        self.0.active_tasks.fetch_sub(1, Ordering::AcqRel);
        self.0.session_changed.notify_one();
    }
}

fn next_reconnect_delay(delay: Duration) -> Duration {
    delay.saturating_mul(2).min(RECONNECT_MAX_DELAY)
}

/// 进程级共享 eD2K 客户端。
pub struct Ed2kClient {
    config: StdMutex<ClientConfig>,
    /// 服务器分配的本机 client_id（0 = 未登录/未知；`< LOWID_THRESHOLD` = LowID）。
    client_id: AtomicU32,
    /// 本机实际监听端口（HighID 登录与 callback 中转都用它）。
    listen_port: AtomicU32,
    /// 持久服务器连接（写端；读循环独占，故用 async mutex 串行化发送）。
    server_tx: AsyncMutex<Option<OwnedWriteHalf>>,
    /// 串行化惰性登录与后台登录，防止会话互相覆盖。
    server_connect: AsyncMutex<()>,
    /// 当前服务器会话代号；读循环退出时只清理自己那一代。
    session_gen: AtomicU64,
    callback_token: AtomicU64,
    active_tasks: AtomicU32,
    reconnect_started: AtomicBool,
    session_changed: Notify,
    /// 待匹配的入站 callback。
    pending: PendingCallbacks,
    /// 已连通的服务器地址（重连/日志用）。
    connected_server: StdMutex<Option<String>>,
    /// UPnP 映射状态与句柄；随 `enable_upnp` 设置启停。
    upnp: StdMutex<UpnpSlot>,
}

static SHARED: OnceLock<Arc<Ed2kClient>> = OnceLock::new();

/// 获取进程级共享客户端（惰性初始化，首次调用建监听器）。
pub fn shared_client() -> Arc<Ed2kClient> {
    SHARED.get_or_init(|| Arc::new(Ed2kClient::new())).clone()
}

impl Ed2kClient {
    fn new() -> Self {
        Self {
            config: StdMutex::new(ClientConfig::default()),
            client_id: AtomicU32::new(0),
            listen_port: AtomicU32::new(0),
            server_tx: AsyncMutex::new(None),
            server_connect: AsyncMutex::new(()),
            session_gen: AtomicU64::new(0),
            callback_token: AtomicU64::new(0),
            active_tasks: AtomicU32::new(0),
            reconnect_started: AtomicBool::new(false),
            session_changed: Notify::new(),
            pending: Arc::new(StdMutex::new(VecDeque::new())),
            connected_server: StdMutex::new(None),
            upnp: StdMutex::new(UpnpSlot::default()),
        }
    }

    /// 注入/更新配置（任务起跑、每轮找源前与宿主改设置时调用）。UPnP 开关热生效：
    /// 监听器已就绪时立即建立 / 撤销映射。监听端口只在监听器首次绑定时读取。
    pub fn configure(self: &Arc<Self>, config: ClientConfig) {
        let mut guard = self.config.lock().unwrap_or_else(|poison| {
            log_error!(
                "[ed2k-client] recovering poisoned configuration: {}",
                poison
            );
            self.config.clear_poison();
            poison.into_inner()
        });
        *guard = config;
        drop(guard);
        self.sync_upnp(false);
        self.session_changed.notify_one();
    }

    /// 让端口映射与设置一致：启用且监听器已就绪 → 启动映射器（之后由它自己周期重试、续租）；
    /// 关闭 → 停止并删除映射。`retry_failed` 为真时把已失败的映射器换成新的，立即重新发现
    /// （仅用户主动检测时）。
    fn sync_upnp(self: &Arc<Self>, retry_failed: bool) {
        let port = u16::try_from(self.listen_port.load(Ordering::Relaxed)).unwrap_or(0);
        let enabled = match self.config.lock() {
            Ok(config) => config.enable_upnp,
            Err(error) => {
                log_error!("[ed2k-client] UPnP config lock poisoned: {}", error);
                return;
            }
        };
        let generation = {
            let Ok(mut slot) = self.upnp.lock() else {
                log_error!("[ed2k-client] UPnP state lock poisoned");
                return;
            };
            if !enabled {
                if let Some(mapping) = slot.mapping.take() {
                    slot.retired = mapping.retire();
                    slot.generation += 1;
                    log_info!("[ed2k-client] UPnP mapping disabled");
                }
                return;
            }
            let restart = match slot.outcome() {
                None => true,
                Some(MappingOutcome::Failed(_)) => retry_failed,
                Some(MappingOutcome::Pending | MappingOutcome::Mapped { .. }) => false,
            };
            if port == 0 || !restart {
                return;
            }
            let after = match slot.mapping.take() {
                Some(previous) => previous.retire(),
                None => slot.retired.take(),
            };
            slot.mapping = Some(UpnpMapping::start(port, after));
            slot.generation += 1;
            slot.generation
        };
        let this = Arc::clone(self);
        tokio::spawn(async move { this.watch_upnp(generation).await });
    }

    /// 观察一代映射直到成功：记录失败原因，映射建好后让 LowID 会话重新登录。
    async fn watch_upnp(&self, generation: u64) {
        let started = tokio::time::Instant::now();
        let mut reported = String::new();
        loop {
            let outcome = {
                let Ok(slot) = self.upnp.lock() else {
                    log_error!("[ed2k-client] UPnP state lock poisoned");
                    return;
                };
                if slot.generation != generation {
                    return;
                }
                slot.outcome()
            };
            match outcome {
                None => return,
                Some(MappingOutcome::Mapped { external_ip }) => {
                    log_info!(
                        "[ed2k-client] UPnP mapping active (external ip: {:?})",
                        external_ip
                    );
                    self.relogin_for_high_id().await;
                    return;
                }
                Some(MappingOutcome::Failed(reason)) if reason != reported => {
                    log_info!("[ed2k-client] port mapping failed, will retry: {}", reason);
                    reported = reason;
                }
                Some(MappingOutcome::Failed(_) | MappingOutcome::Pending) => {}
            }
            let poll = if started.elapsed() < UPNP_SETTLE_TIMEOUT {
                UPNP_SETTLE_POLL
            } else {
                UPNP_WATCH_POLL
            };
            tokio::time::sleep(poll).await;
        }
    }

    /// UPnP 映射建好时会话若仍是 LowID（登录早于映射完成），断开会话让重连器用同一监听
    /// 端口重新登录。LowID 会话没有待决回调（LowID 源只在本机 HighID 时使用），在途的
    /// peer 连接不经过服务器，不受影响。
    async fn relogin_for_high_id(&self) {
        let mut guard = self.server_tx.lock().await;
        if guard.is_none() || self.is_high_id() {
            return;
        }
        let session = self.session_gen.load(Ordering::Relaxed);
        *guard = None;
        drop(guard);
        log_info!("[ed2k-client] port mapped while LowID; re-logging in for HighID");
        self.clear_session(session);
    }

    /// 运行期网络状态（不发起任何连接）。
    #[must_use]
    pub fn network(&self) -> ClientNetwork {
        let client_id = self.client_id.load(Ordering::Relaxed);
        let server = self
            .connected_server
            .lock()
            .ok()
            .and_then(|server| server.clone())
            .unwrap_or_default();
        let (upnp, external_ip, upnp_error) = match self.upnp.lock() {
            Ok(slot) => match slot.outcome() {
                None => (UpnpState::Disabled, String::new(), String::new()),
                Some(MappingOutcome::Pending) => (UpnpState::Pending, String::new(), String::new()),
                Some(MappingOutcome::Failed(reason)) => (UpnpState::Failed, String::new(), reason),
                Some(MappingOutcome::Mapped { external_ip }) => (
                    UpnpState::Mapped,
                    external_ip.map(|ip| ip.to_string()).unwrap_or_default(),
                    String::new(),
                ),
            },
            Err(_) => (UpnpState::Disabled, String::new(), String::new()),
        };
        ClientNetwork {
            id: IdKind::from_client_id(client_id),
            client_id,
            server,
            listen_port: u16::try_from(self.listen_port.load(Ordering::Relaxed)).unwrap_or(0),
            upnp,
            external_ip,
            upnp_error,
        }
    }

    /// 当前配置的副本。
    pub(crate) fn config_snapshot(&self) -> ClientConfig {
        match self.config.lock() {
            Ok(config) => config.clone(),
            Err(poison) => poison.into_inner().clone(),
        }
    }

    /// 本轮找源会查询的服务器（配置顺序的前 [`MAX_QUERY_SERVERS`] 台合法地址）。
    pub(crate) fn query_targets(&self) -> Vec<String> {
        let servers = match self.config.lock() {
            Ok(config) => config.servers.clone(),
            Err(_) => Vec::new(),
        };
        servers
            .into_iter()
            .filter(|server| parse_hostport(server).is_some())
            .take(MAX_QUERY_SERVERS)
            .collect()
    }

    /// 等 UPnP 映射出结果（成功 / 失败 / 未启用），最多 [`UPNP_SETTLE_TIMEOUT`]。
    async fn wait_upnp_settled(&self) {
        let deadline = tokio::time::Instant::now() + UPNP_SETTLE_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let pending = self
                .upnp
                .lock()
                .is_ok_and(|slot| slot.outcome() == Some(MappingOutcome::Pending));
            if !pending {
                return;
            }
            tokio::time::sleep(UPNP_SETTLE_POLL).await;
        }
    }

    /// HighID / LowID 检测。[`IdCheckTarget::Session`] 只报告当前会话；其余目标在需要时用真实
    /// 监听端口（启用 UPnP 时先等映射结果，上次映射失败则重试一次）并发登录目标服务器，
    /// 取第一个应答分配的 ID 后全部断开——检测不建立也不改动任务共用的持久会话。
    ///
    /// # Errors
    ///
    /// 监听端口绑定失败、目标地址非法、没有配置服务器或全部服务器登录失败。
    pub async fn check_id(
        self: &Arc<Self>,
        target: IdCheckTarget,
    ) -> Result<ClientNetwork, DownloadError> {
        let has_session = self.server_tx.lock().await.is_some();
        let servers = match target {
            IdCheckTarget::Session => return Ok(self.network()),
            IdCheckTarget::Auto if has_session => return Ok(self.network()),
            IdCheckTarget::Auto | IdCheckTarget::List => self.query_targets(),
            IdCheckTarget::Server(server) => {
                let server = server.trim().to_owned();
                if parse_hostport(&server).is_none() {
                    return Err(DownloadError::Ed2k(format!(
                        "invalid server address: {server}"
                    )));
                }
                vec![server]
            }
        };
        let listen_port = self.ensure_listener().await?;
        self.sync_upnp(true);
        self.wait_upnp_settled().await;

        let mut join: JoinSet<(String, Result<u32, DownloadError>)> = JoinSet::new();
        for server in servers {
            let Some((host, port)) = parse_hostport(&server) else {
                continue;
            };
            join.spawn(async move {
                let result = match tokio::time::timeout(
                    SERVER_CONNECT_TIMEOUT,
                    login_server(&host, port, listen_port),
                )
                .await
                {
                    // 取到 ID 即可：流在这里 drop，连接随之关闭。
                    Ok(Ok((_stream, client_id))) => Ok(client_id),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(DownloadError::Ed2k("login timed out".into())),
                };
                (server, result)
            });
        }
        let mut last_error = None;
        while let Some(joined) = join.join_next().await {
            match joined {
                Ok((server, Ok(client_id))) => {
                    log_info!(
                        "[ed2k-client] id check via {}: client_id={:#x}",
                        server,
                        client_id
                    );
                    // 其余登录随 JoinSet drop 中止。
                    return Ok(ClientNetwork {
                        id: IdKind::from_client_id(client_id),
                        client_id,
                        server,
                        ..self.network()
                    });
                }
                Ok((server, Err(error))) => {
                    log_info!("[ed2k-client] id check login {} failed: {}", server, error);
                    last_error = Some(error);
                }
                Err(error) => {
                    last_error = Some(DownloadError::Ed2k(format!(
                        "id check login task failed: {error}"
                    )));
                }
            }
        }
        Err(last_error.unwrap_or_else(|| DownloadError::Ed2k("no ed2k servers configured".into())))
    }

    pub(crate) fn begin_task(self: &Arc<Self>) -> ActiveTask {
        self.active_tasks.fetch_add(1, Ordering::AcqRel);
        if !self.reconnect_started.swap(true, Ordering::AcqRel) {
            let this = Arc::clone(self);
            tokio::spawn(async move { this.run_reconnector().await });
        }
        self.session_changed.notify_one();
        ActiveTask(Arc::clone(self))
    }

    async fn run_reconnector(self: Arc<Self>) {
        let mut delay = RECONNECT_INITIAL_DELAY;
        let mut retry_at = tokio::time::Instant::now();
        loop {
            let changed = self.session_changed.notified();
            if self.active_tasks.load(Ordering::Acquire) == 0 {
                delay = RECONNECT_INITIAL_DELAY;
                retry_at = tokio::time::Instant::now();
                changed.await;
                continue;
            }
            if self.server_tx.lock().await.is_some() {
                delay = RECONNECT_INITIAL_DELAY;
                retry_at = tokio::time::Instant::now() + delay;
                changed.await;
                continue;
            }
            if tokio::time::Instant::now() < retry_at {
                tokio::select! {
                    () = changed => {},
                    () = tokio::time::sleep_until(retry_at) => {},
                }
                continue;
            }
            tokio::select! {
                biased;
                () = changed => {},
                result = self.ensure_server_session() => {
                    retry_at = tokio::time::Instant::now() + delay;
                    delay = if result.is_err() {
                        next_reconnect_delay(delay)
                    } else {
                        RECONNECT_INITIAL_DELAY
                    };
                }
            }
        }
    }

    fn fail_callbacks(&self, session: u64, oldest_only: bool, message: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            while let Some(pos) = pending.iter().position(|entry| entry.session == session) {
                if let Some(entry) = pending.remove(pos)
                    && entry
                        .sender
                        .send(Err(DownloadError::Ed2k(message.into())))
                        .is_err()
                {
                    // A timed-out or cancelled callback no longer has a receiver.
                    tracing::debug!("ED2K callback receiver closed before session failure");
                }
                if oldest_only {
                    break;
                }
            }
        }
    }

    fn clear_session(&self, session: u64) {
        self.client_id.store(0, Ordering::Relaxed);
        if let Ok(mut server) = self.connected_server.lock() {
            *server = None;
        }
        self.fail_callbacks(session, false, "server session disconnected");
        self.session_changed.notify_one();
    }

    /// 本机是否已取得 HighID（可被动接收入站连接）。
    #[must_use]
    pub fn is_high_id(&self) -> bool {
        self.client_id.load(Ordering::Relaxed) >= LOWID_THRESHOLD
    }

    /// 确保监听器已启动（幂等）。返回实际绑定端口。
    ///
    /// # Errors
    ///
    /// 绑定失败返回 [`DownloadError::Io`]。
    pub async fn ensure_listener(self: &Arc<Self>) -> Result<u16, DownloadError> {
        let existing = self.listen_port.load(Ordering::Relaxed);
        if existing != 0 {
            return Ok(existing as u16);
        }
        let want = self
            .config
            .lock()
            .map_err(|error| {
                DownloadError::Ed2k(format!("listener config lock poisoned: {error}"))
            })?
            .listen_port;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, want))
            .await
            .map_err(DownloadError::Io)?;
        let port = listener.local_addr().map_err(DownloadError::Io)?.port();
        self.listen_port.store(u32::from(port), Ordering::Relaxed);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run_listener(listener).await;
        });
        log_info!("[ed2k-client] listener bound on port {}", port);

        // UPnP：若启用，映射监听端口争取 HighID（best-effort，失败回退 LowID）。
        self.sync_upnp(false);
        Ok(port)
    }

    /// 入站连接接受循环：读对端 HELLO 取其 client_id，匹配待处理 callback。
    async fn run_listener(self: Arc<Self>, listener: TcpListener) {
        loop {
            let (stream, addr) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log_error!("[ed2k-client] listener accept error: {}", e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            let pending = Arc::clone(&self.pending);
            tokio::spawn(async move {
                if let Err(e) = handle_inbound(stream, addr.ip().to_string(), &pending).await {
                    log_info!("[ed2k-client] inbound {} dropped: {}", addr, e);
                }
            });
        }
    }

    /// 向服务器发起 LowID callback 请求，等待该 peer 回连。
    ///
    /// 注册 `client_id → oneshot`，发 `OP_CALLBACKREQUEST`，然后在监听器收到
    /// 匹配 client_id 的入站 HELLO 时通过 oneshot 拿到已连接的流。
    ///
    /// # Errors
    ///
    /// 无服务器会话 / 超时 / socket 失败 → [`DownloadError`]。
    async fn request_callback(&self, low_id: u32) -> Result<TcpStream, DownloadError> {
        let (tx, rx) = oneshot::channel();
        let token = self.callback_token.fetch_add(1, Ordering::Relaxed);
        let _registration = CallbackRegistration {
            pending: &self.pending,
            token,
        };
        // 同一把写锁下入队并发送，失败包按服务器处理顺序匹配。
        {
            let mut guard = self.server_tx.lock().await;
            let session = self.session_gen.load(Ordering::Relaxed);
            let stream = guard
                .as_mut()
                .ok_or_else(|| DownloadError::Ed2k("no server session for callback".into()))?;
            self.pending
                .lock()
                .map_err(|_| DownloadError::Ed2k("pending lock poisoned".into()))?
                .push_back(PendingCallback {
                    id: low_id,
                    token,
                    session,
                    sender: tx,
                });
            let frame = proto::frame(OP_CALLBACKREQUEST, &low_id.to_le_bytes());
            if let Err(error) = stream.write_all(&frame).await {
                *guard = None;
                self.clear_session(session);
                return Err(DownloadError::Io(error));
            }
        }
        match tokio::time::timeout(CALLBACK_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(DownloadError::Ed2k("callback sender dropped".into())),
            Err(_) => Err(DownloadError::Ed2k("callback timed out".into())),
        }
    }

    /// 建立到某个源的连接：HighID 直连（受 [`PEER_CONNECT_TIMEOUT`] 约束，
    /// 避免死源吃满 OS 默认 TCP 超时 ~21s 拖慢调度轮转）；LowID 经服务器
    /// callback 中转。
    ///
    /// # Errors
    ///
    /// 连接失败/超时 / callback 超时 → [`DownloadError`]。
    pub async fn connect_source(&self, source: Source) -> Result<TcpStream, DownloadError> {
        match source {
            Source::HighId(peer) => {
                match tokio::time::timeout(
                    PEER_CONNECT_TIMEOUT,
                    TcpStream::connect((peer.ip, peer.port)),
                )
                .await
                {
                    Ok(r) => r.map_err(DownloadError::Io),
                    Err(_) => Err(DownloadError::Ed2k(format!("connect {peer} timed out"))),
                }
            }
            Source::LowId(low_id) => self.request_callback(low_id).await,
        }
    }

    /// 确保持久服务器会话已建立（登录取得 client_id）。幂等。
    ///
    /// 用真实监听端口登录以争取 HighID。首个成功登录的服务器即固定为会话。
    ///
    /// # Errors
    ///
    /// 全部服务器登录失败 → [`DownloadError::Ed2k`]。
    pub async fn ensure_server_session(self: &Arc<Self>) -> Result<(), DownloadError> {
        let _connecting = self.server_connect.lock().await;
        {
            let guard = self.server_tx.lock().await;
            if guard.is_some() {
                return Ok(());
            }
        }
        let listen_port = self.ensure_listener().await?;
        let servers = self
            .config
            .lock()
            .map_err(|error| DownloadError::Ed2k(format!("server config lock poisoned: {error}")))?
            .servers
            .clone();
        if servers.is_empty() {
            return Err(DownloadError::Ed2k("no ed2k servers configured".into()));
        }

        for server in &servers {
            let Some((host, port)) = parse_hostport(server) else {
                continue;
            };
            match tokio::time::timeout(
                SERVER_CONNECT_TIMEOUT,
                login_server(&host, port, listen_port),
            )
            .await
            {
                Ok(Ok((stream, client_id))) => {
                    let (reader, writer) = stream.into_split();
                    let mut guard = self.server_tx.lock().await;
                    let session = self.session_gen.fetch_add(1, Ordering::Relaxed) + 1;
                    *guard = Some(writer);
                    self.client_id.store(client_id, Ordering::Relaxed);
                    if let Ok(mut g) = self.connected_server.lock() {
                        *g = Some(server.clone());
                    }
                    drop(guard);
                    log_info!(
                        "[ed2k-client] server session up: {} (client_id={:#x}, {})",
                        server,
                        client_id,
                        if client_id >= LOWID_THRESHOLD {
                            "HighID"
                        } else {
                            "LowID"
                        }
                    );
                    self.spawn_server_reader(reader, session);
                    self.session_changed.notify_one();
                    return Ok(());
                }
                Ok(Err(e)) => log_info!("[ed2k-client] login {} failed: {}", server, e),
                Err(_) => log_info!("[ed2k-client] login {} timed out", server),
            }
        }
        Err(DownloadError::Ed2k("all ed2k server logins failed".into()))
    }

    /// 消费 IDCHANGE 与 CALLBACK_FAIL；断线时唤醒等待方和活跃任务重连器。
    fn spawn_server_reader(self: &Arc<Self>, mut reader: OwnedReadHalf, session: u64) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match proto::read_frame(&mut reader, MAX_SERVER_FRAME).await {
                    Ok((proto_byte, opcode, payload)) => {
                        let guard = this.server_tx.lock().await;
                        if this.session_gen.load(Ordering::Relaxed) != session || guard.is_none() {
                            break;
                        }
                        if proto_byte == proto::PROTO_EDONKEY && opcode == OP_CALLBACK_FAIL {
                            this.fail_callbacks(session, true, "server rejected callback request");
                        } else if let Ok(Ed2kMessage::IdChange { client_id }) =
                            proto::dispatch(proto_byte, opcode, &payload, false)
                        {
                            this.client_id.store(client_id, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        log_info!("[ed2k-client] server session closed: {}", e);
                        break;
                    }
                }
            }
            let mut guard = this.server_tx.lock().await;
            if this.session_gen.load(Ordering::Relaxed) == session {
                *guard = None;
                this.clear_session(session);
            }
        });
    }

    /// 向多台服务器并发查询某文件的源，保留 HighID 与 LowID 两类，并报告各服务器的应答情况。
    /// 个别服务器失败只体现在 [`SourceQuery::servers`] 里；全部失败时 `sources` 为空。
    ///
    /// # Errors
    ///
    /// 没有配置服务器 / 查询被取消或查询任务 panic → [`DownloadError`]。
    pub async fn find_sources(
        self: &Arc<Self>,
        file_hash: &[u8; 16],
        total_bytes: u64,
        large_file: bool,
    ) -> Result<SourceQuery, DownloadError> {
        // 保持持久会话（供 LowID callback 中转）+ 监听器就绪，best-effort。
        let listen_port = match self.ensure_listener().await {
            Ok(port) => port,
            Err(error) => {
                log_info!(
                    "[ed2k-client] listener unavailable; querying sources without callbacks: {}",
                    error
                );
                0
            }
        };
        if let Err(error) = self.ensure_server_session().await {
            log_info!(
                "[ed2k-client] persistent session unavailable; using standalone source queries: {}",
                error
            );
        }

        let servers = self.query_targets();
        if servers.is_empty() {
            return Err(DownloadError::Ed2k("no ed2k servers configured".into()));
        }

        // 并发对多台服务器发 GETSOURCES 并聚合：不同服务器索引不同文件，
        // 单服务器常常没有目标文件（实测 45.82.80.155 无此文件而 77.42.68.79 有）。
        let mut join: JoinSet<ServerAnswer> = JoinSet::new();
        for server in servers {
            let Some((host, port)) = parse_hostport(&server) else {
                continue;
            };
            let hash = *file_hash;
            join.spawn(async move {
                let result =
                    query_one_server(&host, port, listen_port, &hash, total_bytes, large_file)
                        .await;
                (server, result)
            });
        }

        let my_id = self.client_id.load(Ordering::Relaxed);
        let am_high = self.is_high_id();
        let mut seen: HashSet<Source> = HashSet::new();
        let mut query = SourceQuery::default();
        while let Some(res) = join.join_next().await {
            let (server, raw) = match res {
                Ok((server, Ok(sources))) => (server, sources),
                Ok((server, Err(error))) => {
                    log_info!(
                        "[ed2k-client] server {} source query failed: {}",
                        server,
                        error
                    );
                    query.servers.push(ServerQuery {
                        addr: server,
                        result: Err(error.to_string()),
                    });
                    continue;
                }
                Err(error) if error.is_cancelled() => {
                    tracing::debug!("ED2K source query cancelled during shutdown");
                    return Err(DownloadError::Cancelled);
                }
                Err(error) => {
                    return Err(DownloadError::Ed2k(format!(
                        "source query task panicked: {error}"
                    )));
                }
            };
            query.servers.push(ServerQuery {
                addr: server,
                result: Ok(u32::try_from(raw.len()).unwrap_or(u32::MAX)),
            });
            for (id, port) in raw {
                if id >= LOWID_THRESHOLD {
                    if port == 0 || id == my_id {
                        continue;
                    }
                    let src = Source::HighId(PeerAddr {
                        ip: id_to_ipv4(id),
                        port,
                    });
                    if seen.insert(src) {
                        query.sources.push(src);
                    }
                } else if am_high {
                    // LowID 源仅在我方 HighID 时可用（经持久会话 callback 中转）。
                    let src = Source::LowId(id);
                    if seen.insert(src) {
                        query.sources.push(src);
                    }
                }
            }
        }
        log_info!(
            "[ed2k-client] find_sources: {} sources aggregated across {} servers",
            query.sources.len(),
            query.servers.len()
        );
        Ok(query)
    }
}

/// 处理入站连接：读首帧 HELLO，取对端声明的 client_id，匹配待处理 callback。
async fn handle_inbound(
    mut stream: TcpStream,
    peer_ip: String,
    pending: &PendingCallbacks,
) -> Result<(), DownloadError> {
    // 读一帧，期望 HELLO（含对端 client_id）。
    let (proto_byte, opcode, payload) = tokio::time::timeout(
        Duration::from_secs(10),
        proto::read_frame(&mut stream, MAX_SERVER_FRAME),
    )
    .await
    .map_err(|_| DownloadError::Ed2k("inbound hello timeout".into()))??;

    // 回 HELLOANSWER（对端据此确认我方存活）。
    let answer = build_hello_answer();
    stream
        .write_all(&proto::frame(OP_HELLOANSWER, &answer))
        .await
        .map_err(DownloadError::Io)?;

    let client_id = parse_hello_client_id(proto_byte, opcode, &payload);
    if let Some(id) = client_id {
        let waiter = {
            let mut queue = pending.lock().map_err(|error| {
                DownloadError::Ed2k(format!("callback queue lock poisoned: {error}"))
            })?;
            queue
                .iter()
                .position(|entry| entry.id == id)
                .and_then(|pos| queue.remove(pos))
        };
        if let Some(entry) = waiter {
            if entry.sender.send(Ok(stream)).is_err() {
                // Cancellation won the race with delivery; the returned socket is closed.
                tracing::debug!("ED2K callback receiver closed before inbound delivery");
            }
            return Ok(());
        }
    }
    // 无匹配 callback（leech-only：我们不做上传服务）——礼貌关闭。
    log_info!("[ed2k-client] inbound from {} unmatched, closing", peer_ip);
    Ok(())
}

/// 从入站首帧提取对端 client_id（HELLO payload：`hashsize(1)+user_hash(16)+client_id(4)+...`）。
fn parse_hello_client_id(proto_byte: u8, opcode: u8, payload: &[u8]) -> Option<u32> {
    if proto_byte != proto::PROTO_EDONKEY || opcode != OP_HELLO {
        return None;
    }
    // payload[0] = hash size (0x10); user_hash = [1..17); client_id = [17..21).
    let b = payload.get(17..21)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// 构造 HELLOANSWER payload（user_hash + 本机 id/port/tag 计数=0）。
fn build_hello_answer() -> Vec<u8> {
    let mut p = Vec::with_capacity(26);
    p.extend_from_slice(&crate::ed2k::identity::user_hash());
    p.extend_from_slice(&0u32.to_le_bytes()); // client id
    p.extend_from_slice(&0u16.to_le_bytes()); // port
    p.extend_from_slice(&0u32.to_le_bytes()); // tag count
    p
}

/// 连接并登录一个服务器，返回 `(已登录流, 分配的 client_id)`。
async fn login_server(
    host: &str,
    port: u16,
    listen_port: u16,
) -> Result<(TcpStream, u32), DownloadError> {
    let mut stream = TcpStream::connect((host, port))
        .await
        .map_err(DownloadError::Io)?;
    let login = proto::frame(OP_LOGINREQUEST, &build_login_payload(listen_port));
    stream.write_all(&login).await.map_err(DownloadError::Io)?;
    let client_id = read_until_id_change(&mut stream).await?;
    Ok((stream, client_id))
}

/// 连接+登录一台服务器并对 `file_hash` 发一次 GETSOURCES，返回原始
/// `(client_id, port)` 源列表。全程受超时约束；任何失败返回 `Err`
/// （调用方 `unwrap_or_default` 容错，不影响其它服务器）。
async fn query_one_server(
    host: &str,
    port: u16,
    listen_port: u16,
    file_hash: &[u8; 16],
    total_bytes: u64,
    large_file: bool,
) -> Result<Vec<(u32, u16)>, DownloadError> {
    let (mut stream, _client_id) = tokio::time::timeout(
        SERVER_CONNECT_TIMEOUT,
        login_server(host, port, listen_port),
    )
    .await
    .map_err(|_| DownloadError::Ed2k("login timed out".into()))??;
    let gs = proto::frame(
        OP_GETSOURCES,
        &build_getsources_payload(file_hash, total_bytes, large_file),
    );
    stream.write_all(&gs).await.map_err(DownloadError::Io)?;
    match tokio::time::timeout(
        QUERY_TIMEOUT,
        read_until_found_sources(&mut stream, file_hash),
    )
    .await
    {
        Ok(Ok(sources)) => Ok(sources),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(DownloadError::Ed2k("getsources timed out".into())),
    }
}

/// 解析 `host:port`。
fn parse_hostport(s: &str) -> Option<(String, u16)> {
    let (host, port) = s.trim().rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::{
        ClientConfig, Ed2kClient, IdCheckTarget, RECONNECT_INITIAL_DELAY, RECONNECT_MAX_DELAY,
        Source, next_reconnect_delay,
    };
    use crate::ed2k::proto::{
        self, MAX_SERVER_FRAME, OP_CALLBACK_FAIL, OP_CALLBACKREQUEST, OP_IDCHANGE, OP_LOGINREQUEST,
        OP_REJECT,
    };
    use crate::ed2k::server::PeerAddr;
    use crate::ed2k::status::IdKind;
    use crate::ed2k::testutil::MockServer;

    async fn read_opcode(stream: &mut TcpStream) -> u8 {
        let (_, opcode, _) = tokio::time::timeout(
            Duration::from_secs(2),
            proto::read_frame(stream, MAX_SERVER_FRAME),
        )
        .await
        .expect("read timeout")
        .expect("read frame");
        opcode
    }

    async fn wait_for_id(client: &Ed2kClient, expected: u32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while client.client_id.load(Ordering::Relaxed) != expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("client ID transition");
    }

    #[tokio::test]
    async fn callback_fail_wakes_oldest_request_without_client_id() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let connection = TcpStream::connect(listener.local_addr().expect("address"))
            .await
            .expect("connect");
        let (mut server, _) = listener.accept().await.expect("accept");
        let client = Arc::new(Ed2kClient::new());
        let (reader, writer) = connection.into_split();
        client.session_gen.store(1, Ordering::Relaxed);
        *client.server_tx.lock().await = Some(writer);
        client.spawn_server_reader(reader, 1);

        let first_client = Arc::clone(&client);
        let first = tokio::spawn(async move { first_client.request_callback(7).await });
        assert_eq!(read_opcode(&mut server).await, OP_CALLBACKREQUEST);
        let second_client = Arc::clone(&client);
        // 同一源的并发请求也必须独立保留，不能覆盖更早的等待方。
        let second = tokio::spawn(async move { second_client.request_callback(7).await });
        assert_eq!(read_opcode(&mut server).await, OP_CALLBACKREQUEST);
        server
            .write_all(&proto::frame(OP_CALLBACK_FAIL, &[]))
            .await
            .expect("failure frame");
        let result = tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .expect("immediate failure")
            .expect("join");
        assert!(matches!(
            result,
            Err(crate::downloader::DownloadError::Ed2k(_))
        ));
        assert!(
            !second.is_finished(),
            "one failure only consumes the oldest request"
        );
        server
            .write_all(&proto::frame(OP_CALLBACK_FAIL, &[]))
            .await
            .expect("second failure");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), second)
                .await
                .expect("second wake")
                .expect("join")
                .is_err()
        );
        assert!(client.pending.lock().expect("pending").is_empty());
        let cancelled_client = Arc::clone(&client);
        let cancelled = tokio::spawn(async move { cancelled_client.request_callback(7).await });
        assert_eq!(read_opcode(&mut server).await, OP_CALLBACKREQUEST);
        cancelled.abort();
        assert!(cancelled.await.expect_err("cancelled join").is_cancelled());
        let next_client = Arc::clone(&client);
        let next = tokio::spawn(async move { next_client.request_callback(9).await });
        assert_eq!(read_opcode(&mut server).await, OP_CALLBACKREQUEST);
        server
            .write_all(&proto::frame(OP_CALLBACK_FAIL, &[]))
            .await
            .expect("failure after cancellation");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), next)
                .await
                .expect("next wake")
                .expect("join")
                .is_err()
        );
        assert!(client.pending.lock().expect("pending").is_empty());
    }

    #[tokio::test]
    async fn disconnected_session_reconnects_only_while_task_is_active() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let client = Arc::new(Ed2kClient::new());
        client.listen_port.store(4662, Ordering::Relaxed);
        client.configure(ClientConfig {
            servers: vec![listener.local_addr().expect("address").to_string()],
            ..ClientConfig::default()
        });
        let active = client.begin_task();
        let (mut first, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("first login")
            .expect("accept");
        assert_eq!(read_opcode(&mut first).await, OP_LOGINREQUEST);
        let id = 0x0100_0001u32;
        first
            .write_all(&proto::frame(OP_IDCHANGE, &id.to_le_bytes()))
            .await
            .expect("ID frame");
        wait_for_id(&client, id).await;
        drop(first);

        let (mut second, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
            .await
            .expect("automatic reconnect")
            .expect("accept");
        assert_eq!(read_opcode(&mut second).await, OP_LOGINREQUEST);
        second
            .write_all(&proto::frame(OP_IDCHANGE, &id.to_le_bytes()))
            .await
            .expect("ID frame");
        wait_for_id(&client, id).await;
        drop(active);
        drop(second);
        wait_for_id(&client, 0).await;
        assert!(
            tokio::time::timeout(
                RECONNECT_INITIAL_DELAY + Duration::from_millis(300),
                listener.accept()
            )
            .await
            .is_err(),
            "idle client must not reconnect"
        );
    }

    #[tokio::test]
    async fn failed_login_waits_before_background_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let client = Arc::new(Ed2kClient::new());
        client.listen_port.store(4662, Ordering::Relaxed);
        client.configure(ClientConfig {
            servers: vec![listener.local_addr().expect("address").to_string()],
            ..ClientConfig::default()
        });
        let active = client.begin_task();
        let (mut first, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("first login")
            .expect("accept");
        assert_eq!(read_opcode(&mut first).await, OP_LOGINREQUEST);
        first
            .write_all(&proto::frame(OP_REJECT, &[]))
            .await
            .expect("reject");
        drop(first);
        assert!(
            tokio::time::timeout(RECONNECT_INITIAL_DELAY / 2, listener.accept())
                .await
                .is_err(),
            "login failure must back off"
        );
        let (mut second, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("retry login")
            .expect("accept");
        assert_eq!(read_opcode(&mut second).await, OP_LOGINREQUEST);
        drop(active);
        drop(second);
    }

    /// 接受一次登录并分配 `id`，返回这条连接。
    async fn answer_login(listener: &TcpListener, id: u32) -> TcpStream {
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("login")
            .expect("accept");
        assert_eq!(read_opcode(&mut stream).await, OP_LOGINREQUEST);
        stream
            .write_all(&proto::frame(OP_IDCHANGE, &id.to_le_bytes()))
            .await
            .expect("ID frame");
        stream
    }

    async fn expect_closed(stream: &mut TcpStream) {
        let mut byte = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .expect("connection must close");
        assert!(matches!(read, Ok(0)), "expected EOF, got {read:?}");
    }

    #[tokio::test]
    async fn id_check_reports_the_assigned_id_and_keeps_no_session_when_idle() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let server = listener.local_addr().expect("address").to_string();
        let client = Arc::new(Ed2kClient::new());
        client.listen_port.store(4662, Ordering::Relaxed);
        client.configure(ClientConfig {
            servers: vec![server.clone()],
            ..ClientConfig::default()
        });

        let check = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.check_id(IdCheckTarget::Auto).await }
        });
        let mut login = answer_login(&listener, 0x0100_0001).await;
        let network = check.await.expect("join").expect("id check");
        assert_eq!(network.id, IdKind::High);
        assert_eq!(network.client_id, 0x0100_0001);
        assert_eq!(network.server, server);
        assert_eq!(network.listen_port, 4662);
        // 没有活跃任务：检测用的连接已断开，客户端自身仍是未登录。
        expect_closed(&mut login).await;
        assert!(client.server_tx.lock().await.is_none());
        assert_eq!(client.network().id, IdKind::None);

        let check = tokio::spawn({
            let client = Arc::clone(&client);
            let target = IdCheckTarget::Server(server.clone());
            async move { client.check_id(target).await }
        });
        let _login = answer_login(&listener, 77).await;
        let network = check.await.expect("join").expect("id check");
        assert_eq!((network.id, network.client_id), (IdKind::Low, 77));
    }

    #[tokio::test]
    async fn id_check_fails_when_no_server_accepts_the_login() {
        let client = Arc::new(Ed2kClient::new());
        client.listen_port.store(4662, Ordering::Relaxed);
        assert!(
            client.check_id(IdCheckTarget::List).await.is_err(),
            "no servers configured"
        );
        assert!(
            client
                .check_id(IdCheckTarget::Server("not-an-address".to_owned()))
                .await
                .is_err()
        );
        // 只看当前会话：没有会话不是错误，也不发起任何连接。
        let idle = client
            .check_id(IdCheckTarget::Session)
            .await
            .expect("session report");
        assert_eq!(idle.id, IdKind::None);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        client.configure(ClientConfig {
            servers: vec![listener.local_addr().expect("address").to_string()],
            ..ClientConfig::default()
        });
        let check = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.check_id(IdCheckTarget::List).await }
        });
        let (mut login, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("login")
            .expect("accept");
        assert_eq!(read_opcode(&mut login).await, OP_LOGINREQUEST);
        login
            .write_all(&proto::frame(OP_REJECT, &[]))
            .await
            .expect("reject");
        assert!(check.await.expect("join").is_err());
    }

    #[tokio::test]
    async fn port_mapping_upgrade_drops_only_a_low_id_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let connection = TcpStream::connect(listener.local_addr().expect("address"))
            .await
            .expect("connect");
        let (mut server, _) = listener.accept().await.expect("accept");
        let client = Arc::new(Ed2kClient::new());
        let (reader, writer) = connection.into_split();
        client.session_gen.store(1, Ordering::Relaxed);
        *client.server_tx.lock().await = Some(writer);
        client.spawn_server_reader(reader, 1);

        client.client_id.store(0x0100_0001, Ordering::Relaxed);
        client.relogin_for_high_id().await;
        assert!(
            client.server_tx.lock().await.is_some(),
            "a HighID session is kept"
        );

        client.client_id.store(77, Ordering::Relaxed);
        client.relogin_for_high_id().await;
        assert!(client.server_tx.lock().await.is_none());
        assert_eq!(client.network().id, IdKind::None);
        expect_closed(&mut server).await;
    }

    #[tokio::test]
    async fn find_sources_reports_every_server_outcome() {
        let peer = PeerAddr {
            ip: std::net::Ipv4Addr::new(10, 1, 2, 3),
            port: 4662,
        };
        let good = MockServer::spawn(vec![peer]).await.expect("mock server");
        // 端口 1 无人监听：登录立即失败。
        let dead = "127.0.0.1:1".to_owned();
        let client = Arc::new(Ed2kClient::new());
        client.listen_port.store(4662, Ordering::Relaxed);
        client.configure(ClientConfig {
            servers: vec![good.server_string(), dead.clone()],
            ..ClientConfig::default()
        });

        let query = client
            .find_sources(&[7u8; 16], 1_000_000, false)
            .await
            .expect("one server answering is enough");
        assert_eq!(query.sources, [Source::HighId(peer)]);
        let result_of = |addr: &str| {
            query
                .servers
                .iter()
                .find(|server| server.addr == addr)
                .map(|server| server.result.clone())
                .expect("server row")
        };
        assert_eq!(result_of(&good.server_string()), Ok(1));
        assert!(result_of(&dead).is_err());
    }

    #[test]
    fn reconnect_backoff_is_exponential_and_capped() {
        let mut delay = RECONNECT_INITIAL_DELAY;
        for expected in [2, 4, 8, 16, 30, 30] {
            delay = next_reconnect_delay(delay);
            assert_eq!(delay, Duration::from_secs(expected));
        }
        assert_eq!(next_reconnect_delay(Duration::MAX), RECONNECT_MAX_DELAY);
    }
}
