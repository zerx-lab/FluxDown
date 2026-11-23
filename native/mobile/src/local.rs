//! 本机主机：进程内 `fluxdownd`（`fluxdown_daemon::runtime::run_with`）+ 嵌入式 agent
//! （`fluxdown_agent::start_embedded`），UI 经 [`LocalConnection`] 直连网关服务，不走
//! WebSocket。生命周期由 [`crate::FluxCore`] 持有：启动一次，`shutdown_local` 统一停止。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use fluxdown_agent::{
    EmbeddedAgent, EmbeddedConfig, LocalConnection, LocalEventError, start_embedded,
};
use fluxdown_daemon::config::DaemonConfig;
use fluxdown_daemon::runtime::{DaemonReady, run_with};
use fluxdown_protocol::{RequestId, RpcRequest, RpcResponse};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::error::{ErrorCodeDto, FluxError, HostErrorDto};
use crate::link::{
    Caller, CloseInfo, ConnectFailure, Connector, Established, EventSource, LinkEvent,
};

/// daemon 首次启动（含数据库迁移）到开始监听的上限。
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(120);
/// 停止时等待 daemon 优雅退出的上限。
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// 停止时等待会话驱动释放连接的上限。
const SESSION_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// 本机主机启动参数（来自 Kotlin `LocalHostConfig`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocalHostConfig {
    /// 应用私有数据目录（引擎 DB、agent 状态都在其下）。
    pub data_dir: String,
    /// 默认保存目录（仅在库里尚无 `default_save_dir` 时播种）。
    pub save_dir: String,
    /// 设备平台名：`android` / `ios`。
    pub platform: String,
    /// 系统设备名（Android「设备名称」或厂商 + 机型、iOS `UIDevice.name`），用于云端设备列表里的
    /// 默认名；`None` / 空白时回落主机名探测。只在本机设备名缺失或仍是占位名时生效，用户改过的不动。
    pub device_name: Option<String>,
}

type DaemonTask = JoinHandle<Result<(), String>>;

pub(crate) struct LocalHost {
    agent: Arc<EmbeddedAgent>,
    /// daemon 与 agent 的父令牌。
    host_cancel: CancellationToken,
    /// 所有本机会话驱动的父令牌：停止主机前先让它们放开连接。
    sessions_cancel: CancellationToken,
    sessions: Mutex<Vec<JoinHandle<()>>>,
    daemon: DaemonTask,
}

impl LocalHost {
    /// 在当前（`FluxCore` 的）runtime 上启动 daemon 与嵌入式 agent。
    pub(crate) async fn start(config: &LocalHostConfig) -> Result<Self, FluxError> {
        let data_dir = config.data_dir.trim();
        if data_dir.is_empty() {
            return Err(FluxError::invalid_argument("data_dir is empty"));
        }
        let data_dir = PathBuf::from(data_dir);
        let agent_data_dir = data_dir.join("agent");
        // 先于 daemon 装订阅者：同进程的 daemon / agent 事件都落到 `<agent>/logs/agent.log`
        // （诊断「日志目录」检查与日志导出读这里）；移动端没有可读的 stderr。
        // 日志启动配置与内嵌网关共用同一个引擎数据根（不是 agent 子目录）。
        fluxdown_agent::logging::init_embedded(&agent_data_dir, &data_dir);
        let host_cancel = CancellationToken::new();

        let (ready_tx, ready_rx) = oneshot::channel::<DaemonReady>();
        let daemon_config = DaemonConfig::embedded(
            data_dir.clone(),
            Some(config.save_dir.clone()).filter(|dir| !dir.trim().is_empty()),
        );
        let daemon_cancel = host_cancel.child_token();
        let mut daemon: DaemonTask = tokio::spawn(async move {
            run_with(daemon_config, daemon_cancel, Some(ready_tx))
                .await
                .map_err(|error| format!("{error:#}"))
        });

        let ready = tokio::select! {
            ready = ready_rx => match ready {
                Ok(ready) => ready,
                Err(_) => return Err(daemon_failure(&mut daemon).await),
            },
            exited = &mut daemon => {
                return Err(match exited {
                    Ok(Ok(())) => FluxError::transport("local daemon exited before it was ready"),
                    Ok(Err(message)) => FluxError::transport(format!("local daemon failed: {message}")),
                    Err(error) => FluxError::transport(format!("local daemon task failed: {error}")),
                });
            },
            () = sleep(DAEMON_START_TIMEOUT) => {
                host_cancel.cancel();
                stop_daemon(daemon).await;
                return Err(FluxError::transport("local daemon did not start in time"));
            }
        };

        let agent = match start_embedded(
            EmbeddedConfig {
                agent_data_dir,
                engine_data_dir: data_dir,
                daemon_url: format!("ws://{}/rpc", ready.addr),
                daemon_token: ready.token,
                client_platform: config.platform.clone(),
                client_device_name: config.device_name.clone(),
                // 只做出站（发现 + 发起配对）：局域网配对添加远程主机时本机会话调 `agent.link.*`。
                enable_link: true,
            },
            host_cancel.child_token(),
        )
        .await
        {
            Ok(agent) => agent,
            Err(error) => {
                host_cancel.cancel();
                stop_daemon(daemon).await;
                return Err(FluxError::transport(format!(
                    "local agent failed to start: {error}"
                )));
            }
        };

        Ok(Self {
            agent: Arc::new(agent),
            sessions_cancel: host_cancel.child_token(),
            host_cancel,
            sessions: Mutex::new(Vec::new()),
            daemon,
        })
    }

    pub(crate) fn connector(&self) -> Arc<dyn Connector> {
        Arc::new(LocalConnector {
            agent: Arc::downgrade(&self.agent),
        })
    }

    /// App 自更新的统一出口来源（本机 daemon 的全局代理 / 插件改写方案）。
    pub(crate) fn egress_source(&self) -> Arc<dyn fluxdown_agent::egress::EgressSource> {
        self.agent.egress_source()
    }

    /// 会话驱动的取消令牌（本机会话取其子令牌）。
    pub(crate) fn session_token(&self) -> CancellationToken {
        self.sessions_cancel.child_token()
    }

    pub(crate) fn track_session(&self, driver: JoinHandle<()>) {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.retain(|handle| !handle.is_finished());
        sessions.push(driver);
    }

    /// 先放开全部会话连接，再停 agent（释放 `agent.lock`），最后停 daemon。
    pub(crate) async fn shutdown(self) {
        let Self {
            agent,
            host_cancel,
            sessions_cancel,
            sessions,
            daemon,
        } = self;
        sessions_cancel.cancel();
        let drivers = std::mem::take(&mut *sessions.lock().unwrap_or_else(PoisonError::into_inner));
        for driver in drivers {
            match timeout(SESSION_STOP_TIMEOUT, driver).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "local session driver ended abnormally"),
                Err(_) => tracing::warn!("local session driver did not stop in time"),
            }
        }

        // 连接中的 `connect()` 短暂持有 `Weak` 升级出的引用：等其归还独占所有权。
        let mut agent = agent;
        let mut attempts = 0_u32;
        loop {
            match Arc::try_unwrap(agent) {
                Ok(owned) => {
                    owned.shutdown().await;
                    break;
                }
                Err(shared) if attempts < 50 => {
                    agent = shared;
                    attempts += 1;
                    sleep(Duration::from_millis(100)).await;
                }
                Err(shared) => {
                    tracing::warn!("embedded agent is still shared; stopping it via cancellation");
                    drop(shared);
                    break;
                }
            }
        }
        host_cancel.cancel();
        stop_daemon(daemon).await;
    }
}

/// ready 通道关闭而任务没有产出：取任务结果作为失败原因。
async fn daemon_failure(daemon: &mut DaemonTask) -> FluxError {
    match daemon.await {
        Ok(Ok(())) => FluxError::transport("local daemon exited before it was ready"),
        Ok(Err(message)) => FluxError::transport(format!("local daemon failed: {message}")),
        Err(error) => FluxError::transport(format!("local daemon task failed: {error}")),
    }
}

async fn stop_daemon(daemon: DaemonTask) {
    match timeout(DAEMON_STOP_TIMEOUT, daemon).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(message))) => tracing::warn!(%message, "local daemon stopped with an error"),
        Ok(Err(error)) => tracing::warn!(%error, "local daemon task failed"),
        Err(_) => tracing::warn!("local daemon did not stop in time"),
    }
}

struct LocalConnector {
    /// 弱引用：主机停止后 `connect` 立即得到「已停止」而不是复活 agent。
    agent: Weak<EmbeddedAgent>,
}

#[async_trait]
impl Connector for LocalConnector {
    async fn connect(&self, hello: RpcRequest) -> Result<Established, ConnectFailure> {
        let Some(agent) = self.agent.upgrade() else {
            return Err(ConnectFailure::Fatal(HostErrorDto::new(
                ErrorCodeDto::Unavailable,
                "local host stopped",
            )));
        };
        // 进程内握手只会因 hello 被拒或 agent 已停止而失败，二者都不可恢复。
        let connection = agent
            .connect(hello)
            .await
            .map_err(|error| ConnectFailure::Fatal(FluxError::from(error).into()))?;
        let connection = Arc::new(connection);
        Ok(Established {
            hello: connection.hello().clone(),
            caller: Arc::new(LocalCaller(Arc::clone(&connection))),
            events: Box::new(LocalEvents(connection)),
        })
    }
}

struct LocalCaller(Arc<LocalConnection>);

#[async_trait]
impl Caller for LocalCaller {
    async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, FluxError> {
        // 连接会改写请求 id 并在应答中还原，这里的 id 只是占位。
        let request = RpcRequest::new(RequestId::Integer(0), method, params);
        match self.0.call(request).await {
            RpcResponse::Success(success) => Ok(success.result),
            RpcResponse::Failure(failure) => Err(failure.error.into()),
        }
    }
}

struct LocalEvents(Arc<LocalConnection>);

#[async_trait]
impl EventSource for LocalEvents {
    async fn next(&mut self) -> LinkEvent {
        match self.0.next_event().await {
            Ok(frame) => LinkEvent::Frame(Box::new(frame)),
            Err(LocalEventError::Lagged { .. }) => LinkEvent::Lagged,
            // agent 已停止：不会再有事件，也无从重连。
            Err(LocalEventError::Closed) => LinkEvent::Closed(CloseInfo::ServiceQuit),
        }
    }
}
