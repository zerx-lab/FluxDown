//! BT 防吸血的 agent 侧：云名单拉取 / 验签 / 缓存，与证据上报。
//!
//! 两件事都由 daemon 事件驱动，**没有周期定时器**（AGENTS「空闲静默」）：
//!
//! - `BtGuardCloudRefreshWanted`（BT 会话启动时 + 会话存活期间每 6 h）→ 经统一出口拉
//!   `${FLUXCLOUD_BASE}/api/v1/bt-guard/list`，验签后交给 `daemon.bt.guard.cloud.apply`（见
//!   [`sync`]）。拉取失败后的退避重试是唯一的自有计时，且只在「尚未成功、daemon 仍有 BT 会话」
//!   时才会真正联网。
//! - `BtGuardEvidenceReady` → 已登录且用户开启 `bt_guard_report_enabled` 时弹出证据并批量上报
//!   （见 [`evidence`]）。
//!
//! `daemon.bt.guard.cloud.apply` / `daemon.bt.guard.evidence.drain` 只有本模块调用，UI Gateway 与
//! 远端 `/rpc` 一律拒绝（`gateway::internal_daemon_method`）。

mod daemon;
mod evidence;
mod keys;
mod list;
mod sync;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use fluxdown_protocol::{AgentEvent, DaemonEvent, EventFrame, ServiceEvent, daemon_config_default};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::cloud::CloudApi;
use crate::daemon_client::DaemonClient;
use crate::egress::Egress;
use crate::event_hub::AgentEventHub;
use daemon::GuardDaemon;
use evidence::{EvidenceUploader, UploadTiming};
use list::KeySet;
use sync::{CloudListSync, RetryTiming, SyncOutcome};

/// 云名单缓存目录名（`<agent data_dir>/bt_guard/cloud.json`）。
const CACHE_DIR: &str = "bt_guard";
const KEY_CLOUD_ENABLED: &str = "bt_guard_cloud_enabled";
const KEY_REPORT_ENABLED: &str = "bt_guard_report_enabled";

/// 装配参数。生产经 [`BtGuardWorker::new`]；测试注入假 daemon、测试公钥与毫秒级退避。
pub(crate) struct WorkerParts {
    pub(crate) daemon: Arc<dyn GuardDaemon>,
    pub(crate) cloud: CloudApi,
    pub(crate) events: AgentEventHub,
    pub(crate) egress: Egress,
    pub(crate) cache_dir: std::path::PathBuf,
    pub(crate) keys: KeySet,
    pub(crate) list_retry: RetryTiming,
    pub(crate) upload_retry: UploadTiming,
}

pub struct BtGuardWorker {
    daemon: Arc<dyn GuardDaemon>,
    cloud: CloudApi,
    events: AgentEventHub,
    /// 构造时即订阅，不会漏掉 `run` 被调度之前发出的事件。
    frames: broadcast::Receiver<EventFrame>,
    sync: CloudListSync,
    uploader: EvidenceUploader,
    signals: Arc<Signals>,
}

impl BtGuardWorker {
    /// 生产构造：内置公钥、统一出口经 daemon 取方案；缓存在 `<data_dir>/bt_guard/`。
    #[must_use]
    pub fn new(
        cloud: CloudApi,
        daemon: &Arc<DaemonClient>,
        events: AgentEventHub,
        data_dir: &Path,
    ) -> Self {
        Self::build(WorkerParts {
            daemon: daemon.clone(),
            cloud,
            events,
            egress: Egress::daemon(daemon.clone()),
            cache_dir: data_dir.join(CACHE_DIR),
            keys: KeySet::production(),
            list_retry: RetryTiming::PRODUCTION,
            upload_retry: UploadTiming::PRODUCTION,
        })
    }

    pub(crate) fn build(parts: WorkerParts) -> Self {
        Self {
            frames: parts.events.subscribe(),
            sync: CloudListSync::new(parts.cache_dir, parts.keys, parts.egress, parts.list_retry),
            uploader: EvidenceUploader::new(parts.upload_retry),
            daemon: parts.daemon,
            cloud: parts.cloud,
            events: parts.events,
            signals: Arc::default(),
        }
    }

    /// 运行到 `cancel` 取消。事件分发独立成一个分支：拉取 / 上报期间 agent 事件广播不会积压。
    pub async fn run(self, cancel: CancellationToken) {
        let Self {
            daemon,
            cloud,
            events,
            frames,
            sync,
            uploader,
            signals,
        } = self;
        // agent 晚于 daemon 的 BT 会话启动连上时，错过了会话启动那次刷新请求：连上后主动问一次。
        if events.inspect(|snapshot| snapshot.daemon_connected) {
            signals.request_probe();
        }
        tokio::join!(
            dispatch(frames, &signals, &cancel),
            cloud_loop(sync, &*daemon, &cloud, &events, &signals, &cancel),
            evidence_loop(uploader, &*daemon, &cloud, &events, &signals, &cancel),
        );
    }
}

/// 事件分发 → 两个工作循环的信号。
#[derive(Default)]
struct Signals {
    cloud: Mutex<CloudRequest>,
    cloud_wake: Notify,
    evidence_wake: Notify,
    /// 两个工作循环已完成的轮数（每轮开始时 +1）：测试据此确定性地等待「事件已处理完」，
    /// 断言「什么都没发生」时无需睡眠猜测。
    #[cfg(test)]
    cloud_rounds: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    evidence_rounds: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct CloudRequest {
    /// daemon 请求刷新时报告的当前序号；多次请求合并为最新一次。
    have_sequence: Option<u64>,
    /// 需要向 daemon 确认 BT 会话是否在请求刷新（agent 刚连上 daemon）。
    probe: bool,
}

impl Signals {
    fn request_refresh(&self, have_sequence: u64) {
        self.cloud
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .have_sequence = Some(have_sequence);
        self.cloud_wake.notify_one();
    }

    fn request_probe(&self) {
        self.cloud
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .probe = true;
        self.cloud_wake.notify_one();
    }

    fn take_cloud(&self) -> CloudRequest {
        std::mem::take(&mut *self.cloud.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

async fn dispatch(
    mut frames: broadcast::Receiver<EventFrame>,
    signals: &Signals,
    cancel: &CancellationToken,
) {
    loop {
        let frame = tokio::select! {
            () = cancel.cancelled() => return,
            frame = frames.recv() => frame,
        };
        match frame {
            Ok(frame) => match frame.event {
                ServiceEvent::Agent(AgentEvent::Daemon(
                    DaemonEvent::BtGuardCloudRefreshWanted { have_sequence },
                )) => signals.request_refresh(have_sequence),
                ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::BtGuardEvidenceReady {
                    ..
                })) => signals.evidence_wake.notify_one(),
                ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(true)) => {
                    signals.request_probe();
                }
                _ => {}
            },
            // 积压丢帧：错过的事件无从得知。上报侧唤醒一次（条件不满足时什么都不做）；
            // 云名单靠 daemon 的下一次周期请求。
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::debug!(skipped, "BT guard event stream lagged");
                signals.evidence_wake.notify_one();
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// 布尔设置键的当前值；daemon 快照里没有（尚未载入）时取协议默认值。
fn flag(events: &AgentEventHub, key: &str) -> bool {
    let value = events.inspect(|snapshot| snapshot.daemon.config.values.get(key).cloned());
    match value.as_deref().map(str::trim) {
        Some("true" | "1") => true,
        Some("false" | "0") => false,
        _ => daemon_config_default(key) == "true",
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending().await,
    }
}

async fn cloud_loop(
    mut sync: CloudListSync,
    daemon: &dyn GuardDaemon,
    cloud: &CloudApi,
    events: &AgentEventHub,
    signals: &Signals,
    cancel: &CancellationToken,
) {
    // 失败后的退避重试时刻；成功 / 关闭开关 / daemon 不再请求时清除。
    let mut retry_at: Option<Instant> = None;
    loop {
        #[cfg(test)]
        signals
            .cloud_rounds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let retrying = tokio::select! {
            () = cancel.cancelled() => return,
            () = signals.cloud_wake.notified() => false,
            () = sleep_until(retry_at) => true,
        };
        if retrying {
            retry_at = None;
        }
        let request = signals.take_cloud();
        if !flag(events, KEY_CLOUD_ENABLED) {
            retry_at = None;
            continue;
        }
        let have = match request.have_sequence {
            Some(have) => have,
            None if retrying || request.probe => match daemon.state().await {
                Ok(state) if state.session_active => state.cloud.sequence,
                Ok(_) => continue,
                Err(error) => {
                    tracing::debug!(error = %error, "BT guard state query failed");
                    continue;
                }
            },
            None => continue,
        };
        let base_url = cloud.endpoint().base_url;
        retry_at = match sync.refresh(&base_url, have, daemon).await {
            SyncOutcome::Failed { retry_in, .. } | SyncOutcome::BackedOff { retry_in } => {
                Some(Instant::now() + retry_in)
            }
            SyncOutcome::Applied { .. } | SyncOutcome::Unchanged | SyncOutcome::Rejected { .. } => {
                None
            }
        };
    }
}

async fn evidence_loop(
    mut uploader: EvidenceUploader,
    daemon: &dyn GuardDaemon,
    cloud: &CloudApi,
    events: &AgentEventHub,
    signals: &Signals,
    cancel: &CancellationToken,
) {
    let mut session = cloud.session_changes();
    let mut session_open = true;
    loop {
        #[cfg(test)]
        signals
            .evidence_rounds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::select! {
            () = cancel.cancelled() => return,
            () = signals.evidence_wake.notified() => {}
            () = sleep_until(uploader.retry_at()) => {}
            changed = session.changed(), if session_open => {
                // 登出 / 切换账号：上一个会话的证据不再属于当前账号，一律作废。
                session_open = changed.is_ok();
                uploader.clear();
                continue;
            }
        }
        if !flag(events, KEY_REPORT_ENABLED) || !cloud.is_authenticated().await {
            // 未开启上报或未登录：不弹出（daemon 侧上限自然丢旧），已保留的也不再发送。
            uploader.clear();
            continue;
        }
        uploader.flush(cloud, daemon).await;
    }
}
