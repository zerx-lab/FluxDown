//! agent 进程生命周期：完全退出（先关停 daemon，再退出 agent）与退出原因。
//!
//! 两种退出互不混淆：
//! - **完全退出**（`system.shutdown`、托盘「退出」、非驻留模式闲置、系统注销）：停止监管
//!   → 请求 daemon 优雅关停 → 等 daemon 释放进程租约 → 取消 agent；UI 连接以
//!   [`CLOSE_REASON_SERVICE_QUIT`](fluxdown_protocol::CLOSE_REASON_SERVICE_QUIT) 关闭，
//!   客户端据此停止重连与重拉。
//! - **仅 agent 退出**（SIGTERM / Ctrl-C）：只取消 agent，daemon 继续运行，UI 可重拉 agent。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::daemon_client::{DaemonClient, DaemonClientConfig};
use crate::supervisor::DaemonSupervisor;
use fluxdown_protocol::{CLOSE_REASON_SERVICE_QUIT, CLOSE_REASON_SERVICE_RESTART};

/// 等 daemon 收尾（actor 关停、日志 flush）并释放 `daemon.lock` 的上限。
const DAEMON_EXIT_BUDGET: Duration = Duration::from_secs(20);
/// daemon 未连接或无响应时，`system.shutdown` 调用本身的上限。
const DAEMON_SHUTDOWN_CALL_BUDGET: Duration = Duration::from_secs(5);

pub struct Lifecycle {
    cancel: CancellationToken,
    quit_requested: AtomicBool,
    restart_requested: AtomicBool,
    daemon: Arc<DaemonClient>,
    supervisor: Arc<DaemonSupervisor>,
    daemon_data_dir: PathBuf,
    shutdown_config: Option<DaemonClientConfig>,
}

impl Lifecycle {
    #[must_use]
    pub fn new(
        cancel: CancellationToken,
        daemon: Arc<DaemonClient>,
        supervisor: Arc<DaemonSupervisor>,
        daemon_data_dir: PathBuf,
    ) -> Self {
        Self {
            cancel,
            quit_requested: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            daemon,
            supervisor,
            daemon_data_dir,
            shutdown_config: None,
        }
    }

    /// 提供握手前关停所需的连接配置，启用退出时的兜底路径。
    #[must_use]
    pub fn with_shutdown_config(mut self, config: DaemonClientConfig) -> Self {
        self.shutdown_config = Some(config);
        self
    }

    /// 是否处于完全退出流程（决定 UI 连接的关闭原因）。
    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.quit_requested.load(Ordering::Acquire)
    }

    /// 发起完全退出；幂等，立即返回。
    pub fn request_quit(self: &Arc<Self>) {
        if self.quit_requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let lifecycle = Arc::clone(self);
        tokio::spawn(async move {
            lifecycle.run_quit().await;
        });
    }

    /// 重启式完全退出（应用更新、托盘「重启」）：流程与 [`Self::request_quit`] 相同，另外标记为
    /// 重启——调用方须先经 `update::restart::schedule` 登记计划，agent 运行期结束后由
    /// `update::restart::run_pending` 重新拉起程序。幂等，立即返回。
    pub fn request_restart(self: &Arc<Self>) {
        self.restart_requested.store(true, Ordering::Release);
        self.request_quit();
    }

    /// UI 连接在 agent 退出时的 WebSocket 关闭原因。
    ///
    /// 完全退出为 `service-quit`（客户端停止重连）；headless 服务器为更新而重启时为
    /// `service-restart`（浏览器保持重连，服务版本变化后重新加载页面）——桌面形态的重启（更新或
    /// 托盘「重启」）仍是 `service-quit`，由 agent 退出后重新拉起的桌面程序接管界面；其余为
    /// `agent-shutdown`。
    #[must_use]
    pub fn close_reason(&self) -> &'static str {
        if !self.quit_requested() {
            "agent-shutdown"
        } else if self.restart_requested.load(Ordering::Acquire)
            && crate::update::restart::scheduled_server_restart()
        {
            CLOSE_REASON_SERVICE_RESTART
        } else {
            CLOSE_REASON_SERVICE_QUIT
        }
    }

    async fn run_quit(&self) {
        tracing::info!("full quit requested: stopping fluxdownd before fluxdown-agent");
        self.supervisor.stop();
        let call = self
            .daemon
            .call::<Value, Value>(fluxdown_protocol::method::SYSTEM_SHUTDOWN, None);
        match tokio::time::timeout(DAEMON_SHUTDOWN_CALL_BUDGET, call).await {
            Ok(Ok(_)) => wait_daemon_released(&self.daemon_data_dir, DAEMON_EXIT_BUDGET).await,
            Ok(Err(error)) => {
                tracing::warn!(code = ?error.code, "fluxdownd did not accept shutdown");
                self.shutdown_before_handshake().await;
            }
            Err(_) => {
                tracing::warn!("fluxdownd shutdown request timed out");
                self.shutdown_before_handshake().await;
            }
        }
        self.cancel.cancel();
    }

    /// 重连窗口 / 冷启动期间客户端连接不可用：走握手前 `system.shutdown` 兜底，
    /// 否则退出后会留下无界面无托盘的孤儿 daemon。
    async fn shutdown_before_handshake(&self) {
        let Some(config) = &self.shutdown_config else {
            return;
        };
        match crate::daemon_client::request_shutdown(config).await {
            Ok(()) => wait_daemon_released(&self.daemon_data_dir, DAEMON_EXIT_BUDGET).await,
            Err(error) => {
                tracing::warn!(%error, "fallback fluxdownd shutdown was not accepted");
            }
        }
    }
}

/// 轮询 daemon 进程租约，直到 daemon 进程退出或超时。
///
/// 租约文件不存在即视为已释放；拿到锁后立刻释放，不与后续启动的 daemon 竞争。
async fn wait_daemon_released(data_dir: &Path, budget: Duration) {
    let lock_path = data_dir.join("daemon.lock");
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if daemon_lease_released(&lock_path) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(path = %lock_path.display(), "fluxdownd still holds its lease");
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 本数据目录是否有 daemon 持有进程租约（`daemon.lock`）。探测拿到锁会立刻释放；租约文件
/// 不存在或无法探测时按「未持有」处理。
pub(crate) fn daemon_lease_held(lock_path: &Path) -> bool {
    !daemon_lease_released(lock_path)
}

fn daemon_lease_released(lock_path: &Path) -> bool {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
    {
        Ok(file) => file,
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    match file.try_lock() {
        Ok(()) => true,
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(std::fs::TryLockError::Error(error)) => {
            tracing::warn!(error = %error, "could not probe fluxdownd lease");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::daemon_lease_released;

    #[test]
    fn lease_probe_tracks_holder_lifetime() {
        let dir = std::env::temp_dir().join(format!("fluxdown-lease-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create lease dir");
        let path = dir.join("daemon.lock");
        assert!(
            daemon_lease_released(&path),
            "missing lease file is released"
        );

        let holder = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .expect("open lease");
        holder.try_lock().expect("hold lease");
        assert!(!daemon_lease_released(&path), "held lease must block");
        drop(holder);
        assert!(daemon_lease_released(&path), "dropped lease is released");
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), %error, "lease test cleanup failed");
        }
    }
}
