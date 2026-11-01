//! 嵌入式 agent：在宿主进程内（Android / iOS 应用）运行官方客户端后端。
//!
//! 与 `fluxdown-agent` 进程的区别——daemon 由宿主在同一进程内提供（地址与 token 由宿主注入，
//! 不读令牌文件、不拉起子进程），UI 不经 WebSocket 而经 [`LocalConnection`] 直连网关服务。
//! 不装配：UI Gateway TCP 监听与兼容 API、NMH、开机自启迁移、桌面诊断检查、托盘、
//! 完成通知与休眠抑制（移动端由宿主应用的前台服务 / 通知承担）、匿名统计。
//! 所有路径与参数来自 [`EmbeddedConfig`]，从不读取环境变量。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    AgentSnapshot, ApplicationErrorCode, RpcErrorData, RpcErrorObject, RpcRequest,
    TrayUnavailableReason,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::daemon_client::{DaemonClient, DaemonClientConfig};
use crate::event_hub::AgentEventHub;
use crate::gateway::{GatewayService, GatewayShell, LocalConnection};
use crate::lifecycle::Lifecycle;
use crate::power::PowerService;
use crate::runtime::{
    CloudServices, finalize_device_identity, fluxcloud_base_url, spawn_daemon_projection,
    start_cloud_services,
};
use crate::shell::{ShellHost, ShellServices, ShellState, TrayAvailability};
use crate::state::{AgentState, StateError, StateStore};
use crate::supervisor::DaemonSupervisor;

/// 等 daemon 首次连上并投影进快照的上限；超时说明宿主提供的 daemon 不可用。
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// 嵌入式 agent 的启动参数。
#[derive(Clone, Debug)]
pub struct EmbeddedConfig {
    /// agent 自己的数据目录（登录令牌、云同步状态、互联身份）。
    pub agent_data_dir: PathBuf,
    /// 嵌入式 daemon 使用的引擎数据目录（Lifecycle 据此判断 daemon 是否已释放进程租约）。
    pub engine_data_dir: PathBuf,
    /// daemon 控制面地址，`ws://127.0.0.1:<port>/rpc`（必须是回环地址）。
    pub daemon_url: String,
    /// daemon 控制 token（内存传入，不要求令牌文件）。
    pub daemon_token: String,
    /// 设备平台名（`android` / `ios`），首次启动时写入设备身份。
    pub client_platform: String,
    /// 宿主读到的系统设备名（Android「设备名称」/ 机型、iOS `UIDevice.name`）。设备名缺失或仍是
    /// 占位名时用它命名本机；`None` / 空白则回落主机名探测（移动端沙盒通常得到占位名 `FluxDown`）。
    pub client_device_name: Option<String>,
    /// 是否装配局域网互联服务（只有出站发现 / 配对，宿主不开放监听端口）。
    pub enable_link: bool,
}

/// 嵌入式 agent 启动失败。
#[derive(Debug, thiserror::Error)]
pub enum AgentStartError {
    /// 数据目录已被另一个 agent 持有。
    #[error("agent data directory {} is owned by another fluxdown-agent", .0.display())]
    DataDirLocked(PathBuf),
    /// 持久化状态读写失败。
    #[error("agent state: {0}")]
    State(#[from] StateError),
    /// daemon 地址不合法或不是回环地址。
    #[error("invalid daemon endpoint: {0}")]
    DaemonEndpoint(String),
    /// FluxCloud 客户端或 daemon 专用 HTTP 客户端无法构建。
    #[error("agent services: {0}")]
    Services(String),
    /// daemon 在上限内没有连上。
    #[error("daemon did not become ready within {0:?}")]
    DaemonNotReady(Duration),
    /// 启动期间被取消。
    #[error("agent start cancelled")]
    Cancelled,
}

/// 运行中的嵌入式 agent。
pub struct EmbeddedAgent {
    service: Arc<GatewayService>,
    /// 宿主自身下载（App 自更新）经它取 daemon 的统一出口方案。
    daemon: Arc<DaemonClient>,
    cancel: CancellationToken,
    tasks: Vec<(&'static str, JoinHandle<()>)>,
    /// 持有 `agent.lock` 直到关停。
    store: Arc<StateStore>,
}

/// 启动嵌入式 agent 并等待它连上 daemon。`cancel` 触发（或 daemon 协议不兼容等致命错误）
/// 时 agent 停止，所有 [`LocalConnection`] 的事件流以 `Closed` 结束。
pub async fn start_embedded(
    config: EmbeddedConfig,
    cancel: CancellationToken,
) -> Result<EmbeddedAgent, AgentStartError> {
    let EmbeddedConfig {
        agent_data_dir,
        engine_data_dir,
        daemon_url,
        daemon_token,
        client_platform,
        client_device_name,
        enable_link,
    } = config;
    let daemon_config = DaemonClientConfig::new(daemon_url, daemon_token);
    daemon_config
        .validate()
        .map_err(|error| AgentStartError::DaemonEndpoint(format!("{error:#}")))?;

    let store = match StateStore::open(agent_data_dir.clone()).await {
        Ok(store) => Arc::new(store),
        Err(StateError::Locked) => return Err(AgentStartError::DataDirLocked(agent_data_dir)),
        Err(error) => return Err(error.into()),
    };
    let mut state = store.load().await?;
    // 嵌入式宿主没有对外监听端口：不论持久化状态如何都不是局域网可达。
    state.gateway.lan_active = false;
    let new_device = state.device_id.is_empty();
    if new_device {
        state.device_id = uuid::Uuid::new_v4().to_string();
    }
    finalize_device_identity(
        &mut state,
        &store,
        &client_platform,
        client_device_name.as_deref(),
        new_device,
    )
    .await?;

    // 子令牌：启动失败或 daemon 致命错误只停止本 agent，不波及宿主的取消令牌。
    let cancel = cancel.child_token();
    // daemon 由宿主提供：监管器从一开始就处于「已停止」，连接被拒时只重连、永不拉起进程。
    let supervisor = Arc::new(DaemonSupervisor::new(None));
    supervisor.stop();
    let (daemon, daemon_events) =
        match DaemonClient::start_scoped(daemon_config.clone(), supervisor.clone(), cancel.clone())
        {
            Ok(started) => started,
            Err(error) => return Err(AgentStartError::DaemonEndpoint(format!("{error:#}"))),
        };
    let daemon = Arc::new(daemon);

    let availability = TrayAvailability::Unavailable(TrayUnavailableReason::NotBuilt);
    let initial = AgentSnapshot {
        daemon_connected: false,
        session: state
            .credentials
            .as_ref()
            .and_then(|credentials| credentials.session.clone()),
        sync: state.sync.clone(),
        preferences: state.preferences.clone(),
        gateway: state.gateway.clone(),
        linked_devices: crate::link::public_devices(&state),
        remote_tasks: state.remote_tasks.clone(),
        shell: crate::shell::shell_status(availability, &state.preferences),
        update: crate::update::initial_status(&crate::update::unsupported_target()),
        cloud_notify: crate::cloud_notify::initial_state(state.cloud_notify),
        ..AgentSnapshot::default()
    };
    let events = AgentEventHub::new(initial);
    let lifecycle = Arc::new(
        Lifecycle::new(cancel.clone(), daemon.clone(), supervisor, engine_data_dir)
            .with_shutdown_config(daemon_config.clone()),
    );
    let shell = ShellState::new(availability, daemon.clone(), events.clone());
    let power = Arc::new(PowerService::new(events.clone()));
    let notifier = Arc::new(crate::notification::Notifier::new(agent_data_dir));
    let shared_state: Arc<tokio::sync::Mutex<AgentState>> =
        Arc::new(tokio::sync::Mutex::new(state));

    let blobs = match crate::capture::DaemonBlobClient::new(&daemon_config) {
        Ok(blobs) => Arc::new(blobs),
        Err(error) => {
            cancel.cancel();
            return Err(AgentStartError::Services(format!("{error:#}")));
        }
    };
    let (notice_tx, notice_rx) = crate::cloud_notify::notice_channel();
    let CloudServices {
        auth,
        api: cloud_api,
        sync,
        remote,
        notify,
        sync_task,
        remote_task,
        device_meta_task,
        cdn_task,
        bt_guard_task,
        notify_task,
    } = match start_cloud_services(
        // 宿主进程的环境不属于 agent：地址只取构建期注入值，缺省为本地默认。
        fluxcloud_base_url(None, option_env!("FLUXCLOUD_BASE_URL")),
        &daemon,
        &events,
        &shared_state,
        &store,
        notice_rx,
        &cancel,
    )
    .await
    {
        Ok(services) => services,
        Err(error) => {
            cancel.cancel();
            return Err(AgentStartError::Services(format!("{error:#}")));
        }
    };

    let mut tasks: Vec<(&'static str, JoinHandle<()>)> = vec![
        ("CDN", cdn_task),
        ("BT guard", bt_guard_task),
        ("cloud notify", notify_task),
        ("cloud sync", sync_task),
        ("remote tasks", remote_task),
        ("device metadata", device_meta_task),
        ("power", tokio::spawn(power.clone().run(cancel.clone()))),
        (
            "daemon event projection",
            // 嵌入式宿主（移动端）由平台自己处理提示音：不接入 agent 提示音。
            spawn_daemon_projection(
                daemon_events,
                events.clone(),
                notice_tx,
                None,
                cancel.clone(),
            ),
        ),
    ];

    let link = enable_link.then(|| {
        crate::link::LinkService::new(crate::link::LinkServiceParts {
            events: events.clone(),
            state: shared_state.clone(),
            store: store.clone(),
            tasks: Arc::new(crate::link::DaemonTaskCreator::new(daemon.clone())),
            // 没有监听端口：互联只做出站（`reachable = false`）。
            bound: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            server_mode: false,
            network: crate::link::LinkNetworkOptions::from_env(),
        })
    });
    if let Some(link) = &link {
        tasks.push((
            "device link",
            tokio::spawn(link.clone().run(cancel.clone())),
        ));
    }

    let capture = Arc::new(crate::capture::CaptureService::new(
        daemon.clone(),
        events.clone(),
        shell.clone(),
    ));
    // 兼容 API 不存在：开关恒关、令牌为空，诊断省略网关 / 兼容 API / 硬盘休眠检查。
    let api_switches = Arc::new(fluxdown_api::server::ApiRuntimeSwitches::new(
        false, false, false, false, false,
    ));
    let api_token = fluxdown_api::auth::TokenCell::new("");
    let diagnostics = Arc::new(
        crate::diagnostics::DiagnosticsService::new(
            daemon.clone(),
            daemon_config,
            events.clone(),
            shared_state.clone(),
            store.clone(),
            api_switches.clone(),
            api_token.clone(),
        )
        .for_embedded_host(),
    );
    let update = Arc::new(crate::update::UpdateService::unsupported(
        events.clone(),
        store.data_dir().to_path_buf(),
        crate::egress::Egress::daemon(daemon.clone()),
    ));
    let mut gateway = GatewayService::new(
        daemon.clone(),
        events.clone(),
        auth,
        Arc::new(cloud_api),
        sync,
        remote,
        notify,
        capture,
        blobs,
        diagnostics,
        update,
        shared_state,
        store.clone(),
        api_switches,
        api_token,
        GatewayShell {
            shell: shell.clone(),
            power: power.clone(),
            lifecycle: lifecycle.clone(),
            notifier,
        },
    )
    // 与 headless 宿主相同的能力面：桌面平台集成与宿主诊断修复返回 Unsupported。
    .with_server_mode(true)
    // 只有同进程 App 能连到嵌入式网关：日志导出写入 App 自己给出的沙盒路径。
    .with_local_log_export();
    if let Some(link) = &link {
        gateway = gateway.with_link(link.clone());
    }
    let service = Arc::new(gateway);
    tasks.push((
        "shell",
        tokio::spawn(crate::shell::run_controller(
            ShellServices {
                state: shell,
                lifecycle,
                power,
                daemon: daemon.clone(),
                events: events.clone(),
                gateway: service.clone(),
                agent_data_dir: store.data_dir().to_path_buf(),
            },
            ShellHost::without_tray(TrayUnavailableReason::NotBuilt, false),
            cancel.clone(),
        )),
    ));

    let agent = EmbeddedAgent {
        service,
        daemon,
        cancel,
        tasks,
        store,
    };
    if let Err(error) = wait_daemon_connected(&events, &agent.cancel).await {
        agent.shutdown().await;
        return Err(error);
    }
    if let Some(link) = &link
        && let Err(error) = link.start().await
    {
        tracing::warn!(error = %error, "device link could not start");
    }
    Ok(agent)
}

impl EmbeddedAgent {
    /// 进程内等价于「WebSocket UI 客户端发完 `system.hello`」：规则与 WebSocket 首帧完全相同
    /// （JSON-RPC 版本、角色 `agent`、协议版本区间），声明 `client.selections` 的连接计为官方 UI。
    pub async fn connect(&self, hello: RpcRequest) -> Result<LocalConnection, RpcErrorObject> {
        if self.cancel.is_cancelled() {
            return Err(RpcErrorObject::application(
                "agent stopped",
                RpcErrorData::new(ApplicationErrorCode::Unavailable, true),
            ));
        }
        self.service.connect_local(hello, self.cancel.clone()).await
    }

    /// 宿主自身下载（原生 App 自更新）的统一出口来源：本机 daemon 的 `daemon.egress.plan`。
    /// agent 停止后调用失败，调用方按系统默认网络处理。
    #[must_use]
    pub fn egress_source(&self) -> Arc<dyn crate::egress::EgressSource> {
        self.daemon.clone()
    }

    /// 停止 agent 并等待全部后台任务结束。已有的 [`LocalConnection`] 的事件流以 `Closed`
    /// 结束；它们持有的服务引用随连接丢弃而释放，重新启动前必须先丢弃所有连接，否则
    /// 数据目录锁（`agent.lock`）会保持到最后一个连接释放。
    pub async fn shutdown(self) {
        let Self {
            service,
            daemon,
            cancel,
            tasks,
            store,
        } = self;
        drop(daemon);
        cancel.cancel();
        for (task, handle) in tasks {
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {
                    tracing::debug!(task, "embedded agent task cancelled during shutdown");
                }
                Err(error) => {
                    tracing::error!(task, error = %error, "embedded agent task panicked");
                }
            }
        }
        drop(service);
        drop(store);
    }
}

/// 等 daemon 首次连上并已投影进 agent 快照（之后 `system.snapshot` 才带 daemon 数据）。
async fn wait_daemon_connected(
    events: &AgentEventHub,
    cancel: &CancellationToken,
) -> Result<(), AgentStartError> {
    // 先订阅再检查：连接恰好建立在两者之间也不会漏掉通知。
    let (mut receiver, _) = events.subscribe_and_snapshot();
    let deadline = tokio::time::sleep(DAEMON_READY_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        if events.inspect(|snapshot| snapshot.daemon_connected) {
            return Ok(());
        }
        tokio::select! {
            () = cancel.cancelled() => return Err(AgentStartError::Cancelled),
            () = &mut deadline => return Err(AgentStartError::DaemonNotReady(DAEMON_READY_TIMEOUT)),
            frame = receiver.recv() => match frame {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(AgentStartError::Cancelled);
                }
            },
        }
    }
}
