use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{FutureExt, StreamExt};
use reqwest::Client;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;

use crate::bt_config::BtConfig;
#[cfg(feature = "bt")]
use crate::bt_downloader::{self, BtDownloadParams, SharedBtSession, TorrentSource};
#[cfg(feature = "bt")]
use crate::bt_seeding::{
    SEED_TIME_FALLBACK_PERSIST_INTERVAL, SEEDING_QUEUED_MESSAGE, SeedLimitOverrides,
    SeedingLimitConfig, SeedingRegistration, SeedingUploadSnapshot, seed_times_persist_due,
};
use crate::bt_seeding::{SEEDING_STATUS_ACTIVE, SEEDING_STATUS_QUEUED, SeedingStopReason};
use crate::dash_downloader;
use crate::db::{CreateBatchRecord, Db, DbError, NewTaskRow};
use crate::downloader::{self, DownloadParams, ProgressUpdate, SegmentProgressInfo};
use crate::events::{EngineEvent, EventSink};
use crate::ftp_downloader;
use crate::hls_downloader;
use crate::logger::{log_info, log_warn};
use crate::model::{
    MAIN_QUEUE_ID, QueueInfo, SegmentDetail, TaskInfo, is_always_running_queue, is_builtin_queue,
};
use crate::naming;
use crate::proxy_config::{ProxyConfig, ProxyMode};
use crate::queue_order::{QueueOrderDelta, QueueOrderPublisher};
use crate::segment_coordinator::is_single_conn_domain;
use crate::selection::HostSelection;
use crate::speed_limiter::SpeedLimiter;
use crate::transfer_activity::TaskRuntime;

mod boost;
mod group_dirs;
pub use boost::BoostError;
#[cfg(test)]
mod egress_tests;
#[cfg(test)]
mod import_tests;
#[cfg(test)]
mod scaling_tests;

/// 文件已存在时的处理策略（config `file_exists_behavior`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileExistsBehavior {
    /// 自动追加序号重命名（默认，现状）。
    Rename,
    /// 冲突仅来自磁盘上已存在的最终文件时保留原名，完成时覆盖旧文件；
    /// `.fdownloading` 临时文件与并发任务的预订名仍按编号改名，绝不
    /// 覆盖其他任务的在途/产物。
    Overwrite,
    /// 磁盘上已存在同名最终文件时直接跳过本次下载（不重命名、不覆盖、
    /// 不产生任何网络流量），任务立即标记为已完成并采纳已有文件（任务不
    /// 拥有它）。仅对文件名在任务启动序幕即可确定的协议生效（HTTP/FTP）；
    /// HLS 的落盘名在归一化前是临时占位、BT 的最终产物要到完成期才确定
    /// 布局，两者都无法在启动序幕安全判定"最终文件已存在"，故不参与此策略。
    Skip,
    /// 每次询问：有可作答的界面时让出并发槽挂起任务并询问（重命名 / 覆盖 /
    /// 跳过 / 取消）；无人值守、无界面、协议不支持询问（HLS、BT、DASH 轨对）
    /// 时按重命名处理。BT 完成期按重命名处理。
    Ask,
}

impl FileExistsBehavior {
    /// 解析持久化的 config 字符串值；未识别的值回退到默认的 `Rename`。
    pub fn from_config_str(value: &str) -> Self {
        match value {
            "overwrite" => Self::Overwrite,
            "skip" => Self::Skip,
            "ask" => Self::Ask,
            _ => Self::Rename,
        }
    }
}

/// Extract a human-readable message from a panic payload.
fn panic_message(panic_info: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic_info.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_info.downcast_ref::<String>() {
        s.clone()
    } else {
        "internal panic".to_string()
    }
}

/// Handle a panicked download task: persist error status and send an error
/// progress update to the host. The process-wide panic hook owns the root log event;
/// the fallback below covers engine consumers that did not initialize it.
async fn handle_task_panic(
    task_id: &str,
    msg: String,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
) {
    if !crate::logger::panic_hook_installed() {
        crate::log_error!("[download] PANIC in task {}: {}", task_id, msg);
    }
    persist_task_failure(task_id, msg, db, progress_tx).await;
}

async fn persist_task_failure(
    task_id: &str,
    msg: String,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
) {
    if let Err(db_error) = db.update_task_status(task_id, 4, &msg).await {
        crate::logger::report_error("download", "persist task error status", &db_error);
    }
    publish_task_failure(task_id, msg, progress_tx).await;
}

async fn publish_task_failure(
    task_id: &str,
    msg: String,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
) {
    if progress_tx
        .send(ProgressUpdate {
            task_id: task_id.to_string(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: 4,
            error_message: msg,
            file_name: String::new(),
            segment_details: None,
            ..Default::default()
        })
        .await
        .is_err()
    {
        // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
        tracing::debug!("download-manager notification receiver closed");
    }
}

#[cfg(feature = "bt")]
async fn handle_bt_worker_error(
    task_id: &str,
    error: &downloader::DownloadError,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
) {
    match db.load_task_by_id(task_id).await {
        Ok(Some(task)) if matches!(task.status, 3 | 4) => {
            // 下载器已经发出终态（包括重复种子哨兵）；不能被外层包装覆盖。
            tracing::debug!(
                task_id,
                status = task.status,
                "preserving BT worker terminal state"
            );
        }
        Ok(Some(_)) => {
            crate::logger::report_error("download-manager", "run BT download worker", error);
            persist_task_failure(task_id, error.to_string(), db, progress_tx).await;
        }
        Ok(None) => {
            // 用户已经删除任务，不能再用旧 worker 的错误帧把它复活。
            tracing::debug!(task_id, "BT worker exited after task deletion");
        }
        Err(db_error) => {
            crate::logger::report_error(
                "download-manager",
                "read BT terminal state after worker error",
                &db_error,
            );
            crate::logger::report_error("download-manager", "run BT download worker", error);
            // 终态无法查明时保留持久化记录，避免误覆盖完成态/重复种子哨兵；
            // 同时仍向当前消费者报告真实故障，不返回虚假成功。
            publish_task_failure(task_id, error.to_string(), progress_tx).await;
        }
    }
}

#[cfg(feature = "bt")]
async fn rollback_seeding_registration(
    bt: &SharedBtSession,
    db: &Db,
    task_id: &str,
) -> Result<(), downloader::DownloadError> {
    bt.pause_task(task_id).await?;
    if let Some(seed) = bt.unregister_seeder(task_id).await {
        db.set_task_seeding_time(task_id, seed.seed_time_secs)
            .await?;
    }
    Ok(())
}

async fn wait_for_download_exit(
    handle: &mut JoinHandle<()>,
    deadline: tokio::time::Instant,
    operation: &'static str,
) -> bool {
    match tokio::time::timeout_at(deadline, &mut *handle).await {
        Ok(Ok(())) => false,
        Ok(Err(error)) => {
            if error.is_cancelled() {
                tracing::debug!(operation, "download worker cancelled");
            } else {
                crate::logger::report_error("download-manager", operation, &error);
            }
            false
        }
        Err(error) => {
            crate::logger::report_warning("download-manager", operation, &error);
            handle.abort();
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Auto-retry constants
// ---------------------------------------------------------------------------

/// 用户主动取消任务时写入 DB 的 error_message 字面量。
///
/// 取消复用了 error 状态码（status=4），与真实网络错误共用同一状态。
/// 自动重试守卫 `is_task_in_error` 依据此字面量把"取消"与"可重试错误"
/// 区分开，避免用户取消的任务被自动重试逻辑重新启动。
/// `cancel_task` 写入、`is_task_in_error` 读取，两处必须保持一致，
/// 故提取为具名常量。
const CANCELLED_ERROR_MESSAGE: &str = "cancelled";

/// 任务级自动重试最大次数的默认值。网络 stall、连接重置等瞬时错误触发后，
/// 自动延迟恢复下载，避免大文件下载中途停止需要用户手动操作。
///
/// 运行时值由用户在设置中配置（config 表 `max_auto_retries`，经
/// [`DownloadManager::set_max_auto_retries`] 注入）：
/// `-1` = 无限重试，`0` = 关闭自动重试，`1..=10` = 重试次数上限。
const DEFAULT_MAX_TASK_AUTO_RETRIES: i32 = 3;

/// failed 阶段插件重试的独立额度（每个手动周期），不占用 `max_auto_retries`。
#[cfg(feature = "plugins")]
const INTERCEPT_RETRY_LIMIT: u32 = 5;

/// Auto 模式最大连接数上限的默认值（config `auto_max_connections`，经
/// [`DownloadManager::set_auto_max_connections`] 注入）。
///
/// 语义：advisor 推荐值经此裁剪——`effective = min(advisor, cap)`。
/// 默认 16 而非 advisor 的绝对上限 64：避免对连接敏感的服务器/CDN 上来就
/// 32/64 并发触发风控；需要更高并发的用户可在设置中显式调大。
pub(crate) const DEFAULT_AUTO_MAX_CONNECTIONS: i32 = 16;

/// 批量暂停对失败任务（status 4）的作用范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedPause {
    /// 用户显式选中：任何失败任务都落为暂停。
    Any,
    /// 全部暂停 / 停止队列 / 组暂停：只撤销仍在等待自动重试的失败任务，
    /// 已终止的失败保留错误态。
    RetryPending,
}

/// 自动重试基础延迟（秒）的默认值。实际延迟 = base × attempt，即 5s / 10s / 15s 递增。
///
/// 运行时值由用户在设置中配置（config 表 `auto_retry_delay_secs`，经
/// [`DownloadManager::set_auto_retry_delay_secs`] 注入）。`0` 表示无延迟立即重试。
const DEFAULT_AUTO_RETRY_BASE_DELAY_SECS: u64 = 5;

/// 单次自动重试延迟的上限（秒）。
///
/// 实际延迟按 `base × attempt` 线性递增，在无限重试模式（`max == -1`）下
/// `attempt` 会一直累加，若不封顶会让退避无界增长（例如 base=5 时第 1000 次
/// 重试要等 5000s），与用户对"无限重试=持续尝试"的预期相悖。钳到 5 分钟，
/// 既保留递增退避避免对故障源猛冲，又保证无限模式下仍会稳定地持续尝试。
const MAX_AUTO_RETRY_DELAY_SECS: u64 = 300;

/// BT 会话仅因「暂停中的未完成任务」而保活时的空闲宽限。
///
/// 保留会话让暂停任务的恢复只是 unpause（零校验、秒级）；但会话存活意味着 DHT
/// 心跳、tracker 重连等定时器常驻，NAS 上会一直唤醒休眠的硬盘。暂停通常是
/// 「晚点再继续」，故给 15 分钟宽限：期间恢复仍保持秒级；超时后释放会话，
/// 恢复走既有重建路径（`ensure_bt_session` + add_torrent + fastresume）。
/// 宽限从会话「除暂停任务外再无保活来源」那一刻起算，任何 BT 活动都会重置。
#[cfg(feature = "bt")]
const BT_SESSION_PAUSED_IDLE_GRACE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// `invalidate_bt_session` 在关停前等待 inflight `add_torrent` 任务归零的
/// 总上限。BT 监听端口由这些 detached 任务持有的 `Arc<Session>` 绑定，
/// 超时后即便仍有 inflight 也强行继续关停（避免无限等待挂死配置变更）。
/// 5s 取自 magnet DHT 元数据解析的典型耗时上界。
#[cfg(feature = "bt")]
const INVALIDATE_INFLIGHT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `invalidate_bt_session` 轮询 inflight `add_torrent` 状态的间隔。
/// 200ms 足够细以快速响应归零，又不会空耗 CPU。
#[cfg(feature = "bt")]
const INVALIDATE_INFLIGHT_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(200);

/// [`decide_bt_session_release`] 的输入：当前各 BT 会话保活来源是否存在。
#[cfg(feature = "bt")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BtSessionHolds {
    /// 有 BT 任务正在下载。
    active_download: bool,
    /// 有做种者（含排队中的）。
    seeders: bool,
    /// pending 队列里有 BT URL 任务。
    queued: bool,
    /// 仍有 detached `add_torrent` 在途。
    inflight_add: bool,
    /// 存在暂停中（或初检中已请求暂停）的未完成 torrent，持有恢复态。
    paused_incomplete: bool,
}

/// 硬保活来源：只要存在就不能释放会话，也不计入暂停宽限。
#[cfg(feature = "bt")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BtSessionKeep {
    ActiveDownload,
    Seeders,
    QueuedTask,
    InflightAdd,
}

#[cfg(feature = "bt")]
impl BtSessionHolds {
    /// 第一个命中的硬保活来源（不含暂停任务）。
    fn busy_reason(self) -> Option<BtSessionKeep> {
        if self.active_download {
            Some(BtSessionKeep::ActiveDownload)
        } else if self.seeders {
            Some(BtSessionKeep::Seeders)
        } else if self.queued {
            Some(BtSessionKeep::QueuedTask)
        } else if self.inflight_add {
            Some(BtSessionKeep::InflightAdd)
        } else {
            None
        }
    }
}

/// [`decide_bt_session_release`] 的判定结果。
#[cfg(feature = "bt")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BtSessionVerdict {
    /// 有硬保活来源，会话必须保留。
    Keep(BtSessionKeep),
    /// 仅因暂停任务保活，仍在宽限内。`just_armed` 表示本次判定刚起算宽限。
    KeepPausedGrace {
        just_armed: bool,
        remaining: std::time::Duration,
    },
    /// 可以释放。`after_paused_grace` 为 `true` 表示是暂停宽限到期后的释放；
    /// `false` 表示根本没有任何保活来源。
    Release { after_paused_grace: bool },
}

/// BT 会话释放判定（纯函数，时间注入便于测试）。
///
/// `paused_idle_since` 是「会话仅因暂停中的未完成任务而保活」的空闲起点：
/// - 存在任何硬保活来源（下载/做种/排队/在途 add）→ 清除起点并保留会话，因此
///   期间的任何 BT 活动都会让下一次空闲重新起算完整宽限；
/// - 只剩暂停任务 → 首次观察时起算，`now - 起点 >= grace` 才释放；
/// - 什么都不剩 → 立即释放（沿用原语义）。
#[cfg(feature = "bt")]
fn decide_bt_session_release(
    holds: BtSessionHolds,
    paused_idle_since: &mut Option<std::time::Instant>,
    now: std::time::Instant,
    grace: std::time::Duration,
) -> BtSessionVerdict {
    if let Some(reason) = holds.busy_reason() {
        *paused_idle_since = None;
        return BtSessionVerdict::Keep(reason);
    }
    if !holds.paused_incomplete {
        *paused_idle_since = None;
        return BtSessionVerdict::Release {
            after_paused_grace: false,
        };
    }
    let just_armed = paused_idle_since.is_none();
    let since = *paused_idle_since.get_or_insert(now);
    let idle = now.saturating_duration_since(since);
    if idle >= grace {
        *paused_idle_since = None;
        BtSessionVerdict::Release {
            after_paused_grace: true,
        }
    } else {
        BtSessionVerdict::KeepPausedGrace {
            just_armed,
            remaining: grace - idle,
        }
    }
}

/// 从错误信息里提取 reqwest 的 HTTP 状态码。匹配 reqwest 的固定措辞
/// `HTTP status client/server error (NNN ...)`（大小写不敏感查找 `http status`
/// 定位，随后在其后文本中扫描首个三位数字串）。未命中该措辞或数字串返回
/// `None`——不会误伤消息中其他偶然出现的三位数字（如文件大小）。
fn extract_http_status(msg: &str) -> Option<u16> {
    let lower = msg.to_lowercase();
    let idx = lower.find("http status")?;
    // 在 lower 上继续扫描：to_lowercase 可能改变非 ASCII 字符的字节长度，
    // 把 lower 的下标套回 msg 会错位；数字串不受大小写影响。
    let rest = lower.get(idx..)?;
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start == 3 {
                return rest.get(start..i)?.parse::<u16>().ok();
            }
        } else {
            i += 1;
        }
    }
    None
}

/// 判断 HTTP 状态码是否属于可自动重试的瞬时错误。仅 500/502/504（服务端
/// 故障，通常自愈）与 503（服务不可用，常带临时限流语义）、408（请求超时）
/// 放行；501（未实现）/505（HTTP 版本不支持）是永久性错误，重试无意义。
/// 403/404/429 不在此列——403/429 由 `is_server_rejection` 以不同语义
/// （降速/换链路而非定时重试）另行处理，404 是资源永久不存在。
fn is_retriable_http_status(status: u16) -> bool {
    matches!(status, 500 | 502 | 503 | 504 | 408)
}

/// 去掉消息里的 URL 词元（任何含 `://` 的以空白分隔的片段，含括号包裹者）。
/// reqwest 的状态错误形如 `HTTP status client error (404 Not Found) for url (https://…)`，
/// URL 里恰好含 `eof` / `timeout` 之类字样时，不能因此改变可重试判定。
fn without_urls(msg: &str) -> String {
    msg.split_inclusive(char::is_whitespace)
        .filter(|token| !token.contains("://"))
        .collect()
}

/// 判断错误信息是否属于可自动重试的瞬时网络错误。
/// 排除永久性错误（404、403、checksum 等），仅重试网络层问题。HTTP 下载器
/// 的文案带类型化分类标签（平台 / 语言无关），有标签即以标签为准；无标签
/// 的文案（其余协议、旧库存量）回退英文关键词。
pub(crate) fn is_retriable_error(msg: &str) -> bool {
    if let Some(failure) = crate::net_failure::tag_of(msg) {
        return failure.retriable_same_path();
    }
    if let Some(status) = extract_http_status(msg)
        && is_retriable_http_status(status)
    {
        return true;
    }
    let lower = without_urls(msg).to_lowercase();
    lower.contains("stalled")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("broken pipe")
        || lower.contains("network unreachable")
        || lower.contains("network is down")
        || lower.contains("no route to host")
        || lower.contains("eof")
        || lower.contains("connection closed")
        || lower.contains("connection abort")
        || lower.contains("incomplete download")
        // reqwest Kind::Decode：TCP 连接在 body 传输中途被服务端/中间节点切断，大文件尤其常见
        || lower.contains("error decoding response body")
        // Content-Encoding on Range response — retry will use single-stream mode
        || lower.contains("content-encoding")
        // BT 完成前逐 piece 校验失败（BUG-BT-PHANTOM-PIECES）：重试会重新
        // add_torrent，触发 librqbit 全量校验并只补齐损坏的 piece。
        || lower.contains("piece verification failed")
        // 轨对 resume 时的轨长探测失败（dash_downloader::download_track_best_effort
        // 的 fail-loud 保留段行路径）：多为 ephemeral 直链过期/瞬时网络错，自动
        // 重试会重新 resolve 拿新直链后自愈——不重试就只能等用户手动恢复。
        || lower.contains("track probe failed")
}

/// `ProxyMode::Auto` 以代理起飞所需的先验领先倍数：最快代理的跨任务单连接
/// 速率先验须 ≥ 直连先验 × 该值。起飞路径只影响首批连接与单流下载，多段
/// 下载中其余路径仍由 coordinator 同窗实测调度，误判代价有界。
const AUTO_START_PROXY_RATIO: f64 = 1.5;

/// 纯函数：Auto 起飞路径选择。`paths[0]` 恒为直连，其余为候选代理（按候选
/// 顺序：手动在前、系统在后）；`blocked[i]` = 该路径近期断在传输层
/// （[`crate::route_health::path_blocked`]）。
///
/// - 直连可用：最快的可用代理先验 ≥ 直连先验 × [`AUTO_START_PROXY_RATIO`]
///   （直连无先验视为 0）才以代理起飞，否则直连——保留多 CDN 聚合资格；
/// - 直连近期走不通：在可用代理里按先验择优（无先验视为 0，同分取候选顺序
///   靠前者）——不再每个任务都先撞一次被墙的直连；
/// - 全部路径都走不通：失败记录不可区分优劣，回到无阻断时的先验规则。
pub(crate) fn pick_auto_takeoff(
    paths: &[crate::auto_proxy::PathCandidate],
    blocked: &[bool],
) -> usize {
    let is_blocked = |i: usize| blocked.get(i).copied().unwrap_or(false);
    let all_blocked = (0..paths.len()).all(is_blocked);
    let usable = |i: usize| all_blocked || !is_blocked(i);
    let best_proxy = |require_prior: bool| {
        paths
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(i, _)| usable(*i))
            .filter_map(|(i, p)| match p.prior_bps {
                Some(bps) => Some((i, bps)),
                None if !require_prior => Some((i, 0.0)),
                None => None,
            })
            // 同分保留候选顺序靠前者：max_by 在相等时取后者，故反向比较索引。
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
    };
    if usable(0) {
        let direct_prior = paths.first().and_then(|p| p.prior_bps).unwrap_or(0.0);
        return best_proxy(true)
            .filter(|(_, bps)| *bps >= direct_prior * AUTO_START_PROXY_RATIO)
            .map_or(0, |(i, _)| i);
    }
    best_proxy(false).map_or(0, |(i, _)| i)
}

/// `ProxyMode::Auto` 一次性备用链路的目标。手动代理、系统代理和直连在
/// 一个自动恢复周期内各尝试至多一次，避免坏链路之间无限震荡。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoFailoverTarget {
    Proxy(crate::auto_proxy::CandidateSource),
    Direct,
}

impl AutoFailoverTarget {
    #[cfg(feature = "plugins")]
    fn route(self) -> crate::auto_proxy::RoutePath {
        match self {
            Self::Proxy(source) => crate::auto_proxy::RoutePath::Proxy(source),
            Self::Direct => crate::auto_proxy::RoutePath::Direct,
        }
    }

    /// Auto 计划路径 → 目标（网卡链路不进 Auto 候选，按直连归类）。
    #[cfg(feature = "plugins")]
    fn from_route(route: crate::auto_proxy::RoutePath) -> Self {
        match route {
            crate::auto_proxy::RoutePath::Proxy(source) => Self::Proxy(source),
            crate::auto_proxy::RoutePath::Direct | crate::auto_proxy::RoutePath::Link(_) => {
                Self::Direct
            }
        }
    }

    /// 作为一次性备用链路起跑时的路由标签。
    fn failover_label(self) -> &'static str {
        use crate::auto_proxy::route;
        match self {
            Self::Proxy(source) => route::with_source(route::PROXY_FAILOVER, source),
            Self::Direct => route::DIRECT_FAILOVER,
        }
    }
}

/// 起跑时强制的单一路径：一次性备用链路，或插件解析钉定的出口（媒体直链常绑定解析时的
/// 出口 IP，下载必须走同一出口、不做多路径）。`label` 是起飞路由标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ForcedRoute {
    target: AutoFailoverTarget,
    label: &'static str,
}

impl ForcedRoute {
    fn failover(target: AutoFailoverTarget) -> Self {
        Self {
            target,
            label: target.failover_label(),
        }
    }
}

#[derive(Debug, Default)]
struct AutoFailoverAttempts {
    direct: bool,
    manual: bool,
    system: bool,
}

impl AutoFailoverAttempts {
    fn mark_source(&mut self, source: crate::auto_proxy::CandidateSource) {
        match source {
            crate::auto_proxy::CandidateSource::ManualFields => self.manual = true,
            crate::auto_proxy::CandidateSource::System => self.system = true,
        }
    }

    fn source_attempted(&self, source: crate::auto_proxy::CandidateSource) -> bool {
        match source {
            crate::auto_proxy::CandidateSource::ManualFields => self.manual,
            crate::auto_proxy::CandidateSource::System => self.system,
        }
    }

    /// 插件解析在本周期内断过的路径同样计入（不再作为下载备用链路的目标）。
    #[cfg(feature = "plugins")]
    fn mark_route(&mut self, route: crate::auto_proxy::RoutePath) {
        match route {
            crate::auto_proxy::RoutePath::Proxy(source) => self.mark_source(source),
            crate::auto_proxy::RoutePath::Direct | crate::auto_proxy::RoutePath::Link(_) => {
                self.direct = true;
            }
        }
    }

    /// 本周期已尝试的路径（插件解析计划不再把它们排为换路目标）。
    #[cfg(feature = "plugins")]
    fn tried_routes(&self) -> Vec<crate::auto_proxy::RoutePath> {
        use crate::auto_proxy::{CandidateSource, RoutePath};
        [
            (self.direct, RoutePath::Direct),
            (self.manual, RoutePath::Proxy(CandidateSource::ManualFields)),
            (self.system, RoutePath::Proxy(CandidateSource::System)),
        ]
        .into_iter()
        .filter_map(|(tried, route)| tried.then_some(route))
        .collect()
    }
}

fn auto_failover_target(
    auto_route: &str,
    error: &str,
    candidate_sources: &[crate::auto_proxy::CandidateSource],
    network_reachable: bool,
    attempts: &mut AutoFailoverAttempts,
) -> Option<AutoFailoverTarget> {
    if !crate::auto_proxy::is_route_transport_error(error) {
        return None;
    }

    if auto_route.starts_with("proxy") {
        if auto_route.ends_with(":manual") {
            attempts.mark_source(crate::auto_proxy::CandidateSource::ManualFields);
        } else if auto_route.ends_with(":system") {
            attempts.mark_source(crate::auto_proxy::CandidateSource::System);
        } else if candidate_sources.len() == 1 {
            attempts.mark_source(candidate_sources[0]);
        }
    } else {
        attempts.direct = true;
    }

    if let Some(source) = candidate_sources
        .iter()
        .copied()
        .find(|source| !attempts.source_attempted(*source))
    {
        attempts.mark_source(source);
        return Some(AutoFailoverTarget::Proxy(source));
    }
    if network_reachable && !attempts.direct {
        attempts.direct = true;
        return Some(AutoFailoverTarget::Direct);
    }
    None
}

/// Determine if a URL uses the FTP protocol (case-insensitive).
pub(crate) fn is_ftp_url(url: &str) -> bool {
    url.get(..6)
        .map(|prefix| prefix.eq_ignore_ascii_case("ftp://"))
        .unwrap_or(false)
}

/// Determine if a URL is a magnet link.
fn is_magnet(url: &str) -> bool {
    crate::bt_config::is_magnet_url(url)
}

/// Determine if a URL is a torrent-file sentinel (task created from .torrent file).
fn is_torrent_file_url(url: &str) -> bool {
    url.starts_with("torrent-file://")
}

/// Determine if a URL represents any kind of BT download (magnet or .torrent file).
fn is_bt_url(url: &str) -> bool {
    is_magnet(url) || is_torrent_file_url(url)
}

/// Determine if a URL is an `ed2k://` link (case-insensitive prefix; no parsing).
///
/// 无 `ed2k` feature 时 `ed2k` 模块不存在，这里仍要能识别该协议，才能明确
/// 拒绝而不是把它当 HTTP 链接去请求。
fn is_ed2k_url(url: &str) -> bool {
    url.trim_start()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ed2k://"))
}

/// 本构建是否会因 eD2K 被编译期剔除而拒绝该新建任务（含解出 ed2k 地址的
/// thunder 链接）。有 `ed2k` feature 时恒为 `false`。
///
/// 供 daemon 在建任务前预检，以便把 [`UNSUPPORTED_PROTOCOL_MESSAGE`] 回报给
/// 客户端，而不是等 `create_task` 返回无原因的 `None`。BT 的对应预检是
/// [`DownloadManager::rejects_new_bt_task`]。
#[must_use]
pub fn rejects_unsupported_protocol(url: &str) -> bool {
    if cfg!(feature = "ed2k") {
        return false;
    }
    is_ed2k_url(url)
        || (crate::thunder::is_thunder_url(url)
            && crate::thunder::decode_thunder_url(url).is_ok_and(|real| is_ed2k_url(&real)))
}

/// BT 被用户在设置中禁用时，所有拒绝路径（建任务 / 恢复 / 会话初始化）共用的原因文案。
#[cfg(feature = "bt")]
pub const BT_DISABLED_MESSAGE: &str = "BitTorrent is disabled in settings";

/// 编译期没有 BT 后端（无 `bt` feature）时 magnet / 种子任务的拒绝原因：
/// 与其它不支持的协议共用通用文案，不暴露具体协议品牌。
#[cfg(not(feature = "bt"))]
pub const BT_DISABLED_MESSAGE: &str = UNSUPPORTED_PROTOCOL_MESSAGE;

/// 本构建不支持的链接协议（BT / eD2K 被编译期剔除）的通用拒绝文案。
pub const UNSUPPORTED_PROTOCOL_MESSAGE: &str =
    "unsupported link: protocol not available in this build";

/// URL 去重比较前的归一化：只剥离 fragment（`#` 之后的部分）与首尾空白。
/// 下载链接的 fragment 极少携带语义（网页锚点惯例），刻意不处理 query——
/// 部分 CDN 签名 URL 用不同 query token 分发同一份内容，一并折叠会把两个
/// 真正不同的签名链接误判为重复。
fn normalize_url_for_dedup(url: &str) -> String {
    url.split('#').next().unwrap_or(url).trim().to_string()
}

/// 任务地址的换源协议分类：仅 http(s)/ftp 允许 [`DownloadManager::change_task_url`]
/// 换源，其余协议（ed2k/磁力/本地哨兵等）返回 `None`。
fn task_url_protocol(url: &str) -> Option<&'static str> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Some("http")
    } else if is_ftp_url(url) {
        Some("ftp")
    } else {
        None
    }
}

/// webhook 载荷的临时文件名：探测前任务还没有 `file_name` 时，从来源 URL 推断。
/// http(s)/ftp 取路径末段，磁力取 `dn=`；其余协议（本地哨兵等）给不出可靠名字，
/// 返回空串。ed2k 在建任务时已从链接解析出文件名，不会走到这里。
fn webhook_provisional_file_name(url: &str) -> String {
    let name = if is_magnet(url) {
        #[cfg(feature = "bt")]
        {
            bt_downloader::magnet_display_name(url)
        }
        #[cfg(not(feature = "bt"))]
        {
            None
        }
    } else if task_url_protocol(url).is_some() {
        naming::extract_from_url(url)
    } else {
        None
    };
    name.unwrap_or_default()
}

/// 文件跟踪扫描的并发上限。`try_exists` 内部走 tokio blocking 线程池，限流以
/// bound 该共享池占用，防慢盘/网络盘扫描饿死并发下载 IO。
const FILE_SCAN_CONCURRENCY: usize = 64;

/// 单次文件存在性探测的超时（秒），防失联网络盘把整批扫描拖住到 OS 默认
/// 重试时长。
const FILE_SCAN_STAT_TIMEOUT_SECS: u64 = 5;

/// 文件丢失自动清理批次上限：`missing_cleanup_tx` 容量只有 8，一次投递几万
/// 个 id 会让宿主 actor 在一个命令周期里删空整张表。分块投递让删除按轮摊开。
const MISSING_CLEANUP_BATCH: usize = 500;

/// 文件跟踪：构造 completed 任务的目标磁盘路径。`file_name` 为空或不安全
/// （未命名 magnet、路径穿越等）时返回 `None`——无法可靠判定存在性，跳过。
fn task_target_path(save_dir: &str, file_name: &str) -> Option<PathBuf> {
    if file_name.is_empty() || !is_safe_file_name(file_name) {
        return None;
    }
    Some(PathBuf::from(save_dir).join(file_name))
}

/// BT 启动清理：目录中是否仍有任一非空文件。无法确定内容时保守视为有数据，
/// 防止临时权限/设备错误被误当成空目录而删除可恢复的下载数据。
#[cfg(feature = "bt")]
async fn directory_has_real_data(path: &Path) -> bool {
    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => {
            crate::logger::report_warning(
                "download-manager",
                "inspect startup staging directory",
                &error,
            );
            return true;
        }
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => match entry.metadata().await {
                Ok(metadata) if metadata.len() > 0 => return true,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    crate::logger::report_warning(
                        "download-manager",
                        "inspect startup staging entry",
                        &error,
                    );
                    return true;
                }
            },
            Ok(None) => return false,
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "enumerate startup staging contents",
                    &error,
                );
                return true;
            }
        }
    }
}

/// 文件跟踪：探测单个路径是否已丢失。`Some(true)`=确证不存在、`Some(false)`=
/// 存在、`None`=不可判定（I/O 错误 / 超时 / 权限）。调用方对 `None` 保持原
/// 标志不变，避免把「临时不可访问」误判为「已删除」（防误报）；掉盘等瞬时
/// 误报由「双向自愈」（下轮探到存在即翻回）兜底。
async fn probe_missing(path: &Path) -> Option<bool> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(FILE_SCAN_STAT_TIMEOUT_SECS),
        tokio::fs::try_exists(path),
    )
    .await
    {
        Ok(Ok(true)) => Some(false),
        Ok(Ok(false)) => Some(true),
        _ => None,
    }
}

/// 文件跟踪：并发探测所有 completed 任务的目标文件是否仍在磁盘上，把变化
/// 落库并通过 [`EngineEvent::FileMissingChanged`] 上报。仅由
/// [`DownloadManager::spawn_file_scan`] 在 detached task 中调用；`scanning`
/// 标志确保同一时刻只有一个扫描在跑。双向判定（探到存在即把标志翻回 false），
/// 无棘轮，文件移回后自愈。
async fn scan_missing_files(
    db: Db,
    sink: Arc<dyn EventSink>,
    scanning: Arc<AtomicBool>,
    auto_delete: bool,
    cleanup_tx: mpsc::Sender<Vec<String>>,
) {
    // 防重叠：已有扫描在跑就直接返回。
    if scanning.swap(true, Ordering::SeqCst) {
        return;
    }
    // RAII 复位守卫：无论正常返回还是 panic 都把标志清回 false。
    struct ScanGuard(Arc<AtomicBool>);
    impl Drop for ScanGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _guard = ScanGuard(scanning);

    let rows = match db.load_file_tracking_rows().await {
        Ok(r) => r,
        Err(e) => {
            log_info!("[file-scan] load_file_tracking_rows error: {}", e);
            return;
        }
    };

    // 活跃任务（pending/downloading/preparing）占用的目标路径：避免正在重下
    // 同名文件时把旧的 completed 任务误判为丢失。
    let active_paths: HashSet<PathBuf> = rows
        .iter()
        .filter(|t| matches!(t.status, 0 | 1 | 5))
        .filter_map(|t| task_target_path(&t.save_dir, &t.file_name))
        .collect();

    // 只让固定数量的 stat future 同时存活。旧实现先为全部 completed 行构造
    // future，再用信号量限制实际探测；历史任务很多时，等待中的 future 本身
    // 会造成与任务数线性相关的瞬时分配。`buffered` 仍保持输入顺序。
    let probe_futures = rows
        .into_iter()
        .filter(|t| t.status == 3)
        .filter_map(move |t| {
            let path = task_target_path(&t.save_dir, &t.file_name)?;
            if active_paths.contains(&path) {
                return None;
            }
            Some(async move {
                let missing = probe_missing(&path).await?;
                (missing != t.file_missing).then_some((t.task_id, missing))
            })
        });

    // 先收齐全部探测结果，再一次事务写回。逐条独立 UPDATE 在「外置盘掉线」
    // 这类上万条同时翻转的场景下是上万次独立事务（写锁往返 + WAL 追加）。
    let probed: Vec<(String, bool)> = futures_util::stream::iter(probe_futures)
        .buffered(FILE_SCAN_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    // 已离开 status=3 的行（扫描期间被删/重下）在库层被跳过，不进 changes。
    let changes = match db.update_tasks_file_missing(&probed).await {
        Ok(c) => c,
        Err(e) => {
            log_info!("[file-scan] batch update error: {}", e);
            return;
        }
    };

    if !changes.is_empty() {
        // `file_missing_action == "delete"`：把本轮新判定为丢失的任务回流给
        // 宿主 actor 删除记录。通道满 = 上一批还没被消费，剩余分块直接丢弃，
        // 下一轮扫描会重新报告（无棘轮，不阻塞扫描）。
        if auto_delete {
            let gone: Vec<&str> = changes
                .iter()
                .filter(|(_, missing)| *missing)
                .map(|(id, _)| id.as_str())
                .collect();
            for batch in gone.chunks(MISSING_CLEANUP_BATCH) {
                if let Err(e) = cleanup_tx.try_send(batch.iter().map(|s| s.to_string()).collect()) {
                    log_info!("[file-scan] missing cleanup channel send failed: {}", e);
                    break;
                }
            }
        }
        // 事件照常发：UI 先看到 file_missing 状态变化，随后收到删除确认。
        sink.emit(EngineEvent::FileMissingChanged(changes));
    }
}

/// Returns true only when `name` is safe to join onto a base directory for
/// deletion purposes.  Rejects every value that would make `save_dir.join(name)`
/// resolve to anything other than a direct child of `save_dir`:
///   1. empty string    → `save_dir.join("")` == `save_dir` itself
///   2. absolute path    → `PathBuf::join` silently replaces `save_dir` entirely
///   3. `..` component    → path traversal that escapes `save_dir`
///   4. `.` (CurDir)      → `save_dir.join(".")` normalises back to `save_dir`,
///      so `name == "."` would target the save directory
///      itself (e.g. the user's Downloads folder).  Without
///      this guard the BT delete path could `remove_dir_all`
///      the entire save directory.
///   5. Windows `Prefix`  → drive-relative names like `C:foo` would replace the
///      `save_dir` drive component.
fn is_safe_file_name(name: &str) -> bool {
    use std::path::Component;
    if name.is_empty() {
        return false;
    }
    let p = std::path::Path::new(name);
    !p.is_absolute()
        && !p.components().any(|c| {
            matches!(
                c,
                Component::ParentDir
                    | Component::RootDir
                    | Component::CurDir
                    | Component::Prefix(_)
            )
        })
}

/// aria2 `out` 语义：`file_name` 可带相对 `save_dir` 的子目录（`folder/file.zip`）。
/// 目录部分逐段经 [`naming::sanitize_filename`] 清洗后并入 `save_dir`，返回的
/// `file_name` 只剩末段。空段、`.`、`..` 直接丢弃，根 / 盘符前缀（`C:`）被清洗成普通
/// 目录名，结果恒落在 `save_dir` 之内。不含分隔符或 `save_dir` 为空时原样返回。
fn split_relative_file_name(save_dir: &str, file_name: &str) -> (String, String) {
    if save_dir.trim().is_empty() || !file_name.contains(['/', '\\']) {
        return (save_dir.to_owned(), file_name.to_owned());
    }
    let mut parts: Vec<&str> = file_name
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .collect();
    let Some(base) = parts.pop() else {
        return (save_dir.to_owned(), String::new());
    };
    let mut dir = PathBuf::from(save_dir);
    for part in parts {
        dir.push(naming::sanitize_filename(part));
    }
    (dir.to_string_lossy().into_owned(), base.to_owned())
}

/// 「删除任务并删除文件」/「重新下载」是否可以删除 `save_dir/file_name`
/// 指向的最终产物。
///
/// 所有协议的最终文件都在**完成期**才由 temp/staging rename 产生，而
/// `file_name` 的 dedup 改名落库时机是：非 BT 在启动序幕
/// （`finalize_start_file_name`），BT 更晚、在完成期
/// （`bt_downloader::compute_completion_layout`）。因此未完成任务的
/// `save_dir/file_name` 要么不存在，要么指向**别的来源**的同名文件。
/// 典型误删场景：下载完成 A → 删任务保留文件 → 同链接重新添加为稍后
/// 下载（从未启动，`file_name` 未 dedup）→「删除任务和文件」删掉的是
/// 早前的成品 A。只有完成（status=3）的任务才认领最终路径。
fn task_owns_final_file(status: i32) -> bool {
    status == 3
}

/// 任务是否启动过（进入过启动序幕，`file_name` 已 dedup 落库）。
///
/// 用于 DASH 音轨 sidecar（`<stem>.audio.m4a`）的删除守卫：sidecar 可能
/// 在任务完成前就已 rename 到位，启动过的任务其派生路径归本任务命名
/// 空间，删除文件时应当清理；从未启动的任务连 sidecar 也不曾产生，
/// 跳过以免撞上同名旧任务遗留的文件。
fn task_has_started(status: i32, downloaded_bytes: i64) -> bool {
    task_owns_final_file(status) || downloaded_bytes > 0
}

/// 「重新下载」的磁盘清理原语：删掉 `path`，`NotFound` 视为成功。
///
/// `contended = true`（刚取消过在途 spawn）时最多重试 10 次、每次间隔
/// 100 ms：`pause_task_silent` 只 cancel token 不等 spawned task 退出，
/// 而 Windows 上被写入方持有的文件 `remove_file` 会直接失败——不给这个
/// 窗口留重试，残留的 `.fdownloading` 会被下一轮下载当成可续传数据，
/// 「从零重下」的承诺就破了。仍失败只记日志，不阻断重启流程。
async fn remove_file_retrying(task_id: &str, path: &Path, contended: bool) {
    const MAX_ATTEMPTS: u32 = 10;
    let attempts = if contended { MAX_ATTEMPTS } else { 1 };
    for attempt in 1..=attempts {
        match tokio::fs::remove_file(path).await {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                if attempt == attempts {
                    log_info!(
                        "[manager] restart_task {}: remove {} failed after {} attempt(s): {}",
                        task_id,
                        path.display(),
                        attempt,
                        e
                    );
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// 延迟二次清理：删除任务时若等待 spawned 下载 handle 超时，下载任务可能
/// 在首次清理之后才落盘临时/分段文件。本函数 sleep 一段时间后再次删除残留。
///
/// 单任务（`delete_task`）与批量（`delete_tasks_batch`）两条删除路径共用此
/// 逻辑，确保批量删除活跃任务时不会泄漏孤立文件（F010）。
///
/// 行为与历史单任务 deferred 兜底保持一致：
///   - BT：删除最终路径（文件或目录）+ task-scoped staging 目录；
///   - 其它协议：删除 `.fdownloading` 临时文件 + 最终文件。
///
/// 所有删除均为 best-effort，缺失路径静默忽略。
async fn deferred_file_cleanup(
    save_dir: String,
    file_name: String,
    url: String,
    delete_files: bool,
    task_id: String,
) {
    // 给仍在退出的下载任务留出时间落盘后再清理；2s 与单任务路径一致，
    // 配合下载器内新增的早期 cancel 检查已能覆盖绝大多数残留窗口。
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let path = PathBuf::from(&save_dir).join(&file_name);
    if is_bt_url(&url) {
        if delete_files && is_safe_file_name(&file_name) {
            if path.is_dir() {
                if let Err(error) = tokio::fs::remove_dir_all(&path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("download-manager", "remove task files", &error);
                }
            } else {
                if let Err(error) = tokio::fs::remove_file(&path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("download-manager", "remove task files", &error);
                }
            }
        }
        #[cfg(feature = "bt")]
        {
            let stage_dir = bt_downloader::bt_stage_dir(&save_dir, &task_id);
            if stage_dir.exists() {
                log_info!(
                    "[manager] delete {} deferred: removing staging dir {}",
                    task_id,
                    stage_dir.display()
                );
                if let Err(error) = tokio::fs::remove_dir_all(&stage_dir).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logger::report_warning("download-manager", "remove task files", &error);
                }
            }
        }
    } else {
        let temp_path = PathBuf::from(format!("{}{}", path.display(), downloader::TEMP_EXT));
        if let Err(e) = tokio::fs::remove_file(&temp_path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log_info!(
                "[manager] delete {} deferred: remove temp {} failed: {}",
                task_id,
                temp_path.display(),
                e
            );
        }
        // DASH/轨对 audio sidecar（<stem>.audio.m4a）及其临时文件：无条件
        // best-effort 清理——非轨对任务该路径不存在，remove 静默失败即可，
        // 免去在此异步上下文里查 DB 判定任务类型。
        let audio_path = dash_downloader::build_audio_path(&path);
        let audio_temp = PathBuf::from(format!("{}{}", audio_path.display(), downloader::TEMP_EXT));
        if let Err(error) = tokio::fs::remove_file(&audio_temp).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("download-manager", "remove task files", &error);
        }
        if delete_files && is_safe_file_name(&file_name) {
            if let Err(error) = tokio::fs::remove_file(&audio_path).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("download-manager", "remove task files", &error);
            }
            if let Err(error) = tokio::fs::remove_file(&path).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("download-manager", "remove task files", &error);
            }
        }
    }
}

/// `save_dir/file_name` 是否同时被 `exclude` 以外的任务登记为产物名。
///
/// 删除任务文件前的守卫：两条任务的 `file_name` 指向同一磁盘名（手工改名、历史数据、
/// 插件登记的产物名撞上别的任务）时，删除其一不得带走另一条任务的产物。
/// 查询失败按「被占用」处理——破坏性操作宁可少删。
async fn file_name_claimed_by_others(
    db: &crate::db::Db,
    save_dir: &str,
    file_name: &str,
    exclude: &[String],
) -> bool {
    if file_name.is_empty() {
        return false;
    }
    match db.list_task_ids_by_file(save_dir, file_name).await {
        Ok(owners) => owners.iter().any(|id| !exclude.contains(id)),
        Err(e) => {
            log_info!(
                "[manager] file owner lookup failed for {}/{}: {} — skipping delete",
                save_dir,
                file_name,
                e
            );
            true
        }
    }
}

/// 删除任务已登记的衍生产物文件（插件 onDone 经 `flux.task.recordArtifact`
/// 登记，如转码 mp4）。仅在「删除任务并删除文件」时调用；文件名经
/// `is_safe_file_name` 二次校验后限定在 `save_dir` 内 best-effort 删除，
/// 同时是其它任务（`exclude` 之外）`file_name` 的名字跳过。
async fn delete_task_artifact_files(
    db: &crate::db::Db,
    task_id: &str,
    save_dir: &str,
    exclude: &[String],
) -> Result<(), downloader::DownloadError> {
    let names = db.load_task_artifacts(task_id).await?;
    let mut first_error = None;
    for name in names {
        if is_safe_file_name(&name)
            && !file_name_claimed_by_others(db, save_dir, &name, exclude).await
            && let Err(error) = tokio::fs::remove_file(PathBuf::from(save_dir).join(&name)).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("download-manager", "remove task artifacts", &error);
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if let Some(error) = first_error {
        Err(error.into())
    } else {
        Ok(())
    }
}

/// 批量删除时等待单个被删下载退出的上限；超时后 abort 并安排延迟二次清理。
const DELETE_HANDLE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// 批量删除中单个任务磁盘清理的上限：挂死的网络盘不能让整批永远卡在 actor 上，
/// 超时的任务按清理失败保留。
const DELETE_FILE_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 批量删除同时清理磁盘文件的任务数。
const DELETE_FILE_CONCURRENCY: usize = 64;
/// 已完成记录保留期清理的最短间隔：被动触发（任务完成 / 重扫）在此窗口内只执行一次；
/// 设置变更与进程启动不受限。
const RETENTION_PURGE_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// 保留期清理每轮删除的任务数上限（逐块调用 `delete_tasks_batch`，避免一次占住 actor 过久）。
const RETENTION_PURGE_CHUNK: i64 = 200;

/// [`DownloadManager::delete_tasks_batch`] 的结果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct BatchDeleteOutcome {
    /// 已删除记录的任务数。
    pub deleted: usize,
    /// 文件清理失败而保留的任务数。
    pub failed: usize,
}

/// 批量删除中单个任务要清理的磁盘文件；在 actor 上按整批预读的库内事实算好，
/// spawn 之后无需再访问 manager 或数据库。
struct DeleteFiles {
    save_dir: String,
    file_name: String,
    url: String,
    is_bt: bool,
    delete_files: bool,
    /// 最终产物归本任务所有（已完成、未被批外任务登记、非「采纳」既有文件）。
    owns_final: bool,
    /// 任务启动过（且非采纳）：独立音轨成品可随任务删除。
    has_started: bool,
    /// 同名产物同时登记在批外任务名下。
    claimed_by_other: bool,
    has_audio_sidecar: bool,
    /// 随任务删除的衍生产物（已排除批外任务登记的同名文件）。
    artifacts: Vec<PathBuf>,
}

/// 一个被删任务的清理作业。
struct DeleteCleanup {
    task_id: String,
    /// 被取消下载的 JoinHandle（不在下载中则为 `None`）。
    handle: Option<JoinHandle<()>>,
    /// `None` = 库里已无此任务，只等在途下载退出。
    files: Option<DeleteFiles>,
    #[cfg(feature = "bt")]
    bt_session: Option<Arc<SharedBtSession>>,
    #[cfg(feature = "bt")]
    app_data_dir: Arc<str>,
    file_permits: Arc<Semaphore>,
}

/// 批量删除中单个任务的清理：等下载退出 → 移出 BT 会话 → 限时删除磁盘文件 →
/// 下载未按时退出时安排延迟二次清理。
async fn run_delete_cleanup(job: DeleteCleanup) -> Result<(), downloader::DownloadError> {
    let DeleteCleanup {
        task_id,
        handle,
        files,
        #[cfg(feature = "bt")]
        bt_session,
        #[cfg(feature = "bt")]
        app_data_dir,
        file_permits,
    } = job;
    // 超时后 abort 外层 future，加速纯 async 任务释放连接/句柄（F011）。
    let handle_timed_out = match handle {
        Some(mut handle) => {
            wait_for_download_exit(
                &mut handle,
                tokio::time::Instant::now() + DELETE_HANDLE_WAIT,
                "wait for batch deleted download",
            )
            .await
        }
        None => false,
    };
    let Some(files) = files else {
        if handle_timed_out {
            tracing::debug!("orphan download aborted after deletion deadline");
        }
        return Ok(());
    };
    #[cfg(feature = "bt")]
    if files.is_bt {
        remove_from_bt_session(
            &task_id,
            bt_session.as_deref(),
            &app_data_dir,
            files.delete_files,
        )
        .await?;
    }
    let result = {
        let _permit = file_permits.acquire().await.map_err(|error| {
            downloader::DownloadError::Other(format!("file cleanup semaphore closed: {error}"))
        })?;
        match tokio::time::timeout(
            DELETE_FILE_CLEANUP_TIMEOUT,
            remove_deleted_task_files(&task_id, &files),
        )
        .await
        {
            Ok(result) => result,
            Err(elapsed) => Err(downloader::DownloadError::Other(format!(
                "removing files of deleted task {task_id}: {elapsed}"
            ))),
        }
    };
    // F010：下载未按时退出时可能仍在写盘，延迟二次清理兜底孤立文件，与单任务路径一致。
    if handle_timed_out {
        let deferred_delete_files = if files.is_bt {
            files.delete_files && files.owns_final
        } else {
            files.delete_files && !files.claimed_by_other
        };
        tokio::spawn(deferred_file_cleanup(
            files.save_dir,
            files.file_name,
            files.url,
            deferred_delete_files,
            task_id,
        ));
    }
    result
}

#[cfg(feature = "bt")]
/// 把 BT 任务移出 librqbit 会话（会话持久化与可选的文件一并清理）。句柄尚未落位
/// （磁力解析中 / 重新做种校验中）时登记待删，由落位方清理；无会话时直接删 parts 边车。
async fn remove_from_bt_session(
    task_id: &str,
    bt_session: Option<&SharedBtSession>,
    app_data_dir: &str,
    delete_files: bool,
) -> Result<(), downloader::DownloadError> {
    let Some(bt) = bt_session else {
        crate::bt_partfile::remove_sidecar(&crate::bt_partfile::sidecar_path(
            app_data_dir,
            task_id,
        ));
        return Ok(());
    };
    if !bt.delete_task(task_id, delete_files).await? {
        bt.register_pending_delete(task_id, delete_files).await;
        // 二次检查（同单任务删除路径）：miss 与 pending 写入之间句柄可能落位，
        // 消费自己的 pending 走正常删除，防幽灵做种条目。
        if bt.cached_handle(task_id).await.is_some()
            && let Some(pending_delete_files) = bt.take_pending_delete(task_id).await
            && !bt.delete_task(task_id, pending_delete_files).await?
        {
            tracing::debug!("late BT handle already removed");
        }
    }
    Ok(())
}

/// 记录首个非 NotFound 的清理错误（其余只记日志），清理继续进行。
fn keep_first_cleanup_error(first: &mut Option<std::io::Error>, result: std::io::Result<()>) {
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("download-manager", "remove task files", &error);
        if first.is_none() {
            *first = Some(error);
        }
    }
}

/// 删除一个被删任务的磁盘文件：BT 的最终产物与 staging 目录；其余协议的临时文件、
/// 独立音轨与最终产物；以及登记的衍生产物。
async fn remove_deleted_task_files(
    #[cfg_attr(not(feature = "bt"), allow(unused_variables))] task_id: &str,
    files: &DeleteFiles,
) -> Result<(), downloader::DownloadError> {
    let mut first_error = None;
    let path = PathBuf::from(&files.save_dir).join(&files.file_name);
    let removes_final =
        files.delete_files && files.owns_final && is_safe_file_name(&files.file_name);
    if files.is_bt {
        // 跨会话（重启后句柄不在会话里）时 librqbit 删不到最终产物，这里按路径兜底。
        if removes_final {
            let result = if path.is_dir() {
                tokio::fs::remove_dir_all(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            };
            keep_first_cleanup_error(&mut first_error, result);
        }
        #[cfg(feature = "bt")]
        {
            // 任务级 staging 目录无论是否删除文件都清理：未完成时里面是残缺数据。
            let stage_dir = bt_downloader::bt_stage_dir(&files.save_dir, task_id);
            if stage_dir.exists() {
                log_info!(
                    "[manager] delete_tasks_batch {}: removing staging dir {}",
                    task_id,
                    stage_dir.display()
                );
                keep_first_cleanup_error(
                    &mut first_error,
                    tokio::fs::remove_dir_all(&stage_dir).await,
                );
            }
        }
    } else {
        let temp_path = PathBuf::from(format!("{}{}", path.display(), downloader::TEMP_EXT));
        keep_first_cleanup_error(&mut first_error, tokio::fs::remove_file(&temp_path).await);
        if files.has_audio_sidecar {
            let audio_path = dash_downloader::build_audio_path(&path);
            let audio_temp =
                PathBuf::from(format!("{}{}", audio_path.display(), downloader::TEMP_EXT));
            keep_first_cleanup_error(&mut first_error, tokio::fs::remove_file(&audio_temp).await);
            if files.delete_files && files.has_started {
                keep_first_cleanup_error(
                    &mut first_error,
                    tokio::fs::remove_file(&audio_path).await,
                );
            }
        }
        if removes_final {
            keep_first_cleanup_error(&mut first_error, tokio::fs::remove_file(&path).await);
        }
    }
    for artifact in &files.artifacts {
        keep_first_cleanup_error(&mut first_error, tokio::fs::remove_file(artifact).await);
    }
    match first_error {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

/// `dedup_filename` 的同步版本，供任务启动序幕与 HLS 误判回退使用。
///
/// Checks both the on-disk state and the `reserved` in-flight set so that
/// the chosen name does not collide with files already being downloaded by
/// sibling tasks in the same batch.
///
/// **本函数不持任何锁、不整目录扫描**：磁盘冲突逐候选 `symlink_metadata`
/// 探测（由文件系统裁决大小写；重度冲突才退化为一次扫描，见
/// [`downloader::dedup_name_by_probe`]），`reserved` 由调用方传入快照。需要
/// 「选名 + 预订」原子性的调用方用 [`reserve_deduped_name`]。
/// `overwrite` 允许替换 `name` 时:磁盘上**仅最终文件**存在不算冲突——保留
/// 原名,完成时由 finalize 覆盖旧文件;`.fdownloading` 临时文件(在途下载)与
/// `reserved` 预订命中仍是硬冲突,照旧编号改名,绝不覆盖其他任务的在途/产物。
/// 目录同名也照旧改名(文件不能覆盖目录)。
///
/// `companion_mp4`:HLS 的成品是 `<stem>.mp4`(由 `.ts` remux 而来),同 stem 的
/// `.mp4` 已存在时同样视为冲突(除非 `overwrite` 允许替换),选出的 stem 对
/// `.ts` 与 `.mp4` 同时空闲。
///
/// 探测遇到无法判定的 I/O 错误时返回 UUID 后缀名（保守：绝不可能覆盖已有文件）。
fn dedup_filename_sync(
    dir: &std::path::Path,
    name: &str,
    reserved: &HashSet<std::path::PathBuf>,
    overwrite: &crate::file_exists::OverwritePolicy,
    companion_mp4: bool,
) -> String {
    let rules = downloader::DedupRules {
        reserved: Some(reserved),
        avoid: None,
        allow_overwrite: overwrite.permits(name),
        companion_mp4,
    };
    match downloader::dedup_name_by_probe(dir, name, &rules) {
        Ok(chosen) => chosen,
        Err(error) => {
            crate::logger::report_warning("download-manager", "probe filename collisions", &error);
            downloader::uuid_suffixed_name(name)
        }
    }
}

/// 选出不冲突的名字并在 `reserved` 里预订其 `.fdownloading` 路径（同步，须在阻塞
/// 线程上调用）。
///
/// **锁内无任何文件 I/O**，只做 `HashSet::insert` / `remove`：
///   1. 锁外用 [`downloader::DedupCursor`] 按编号升序探测磁盘，取下一个磁盘空闲候选；
///   2. 锁内 `insert`——已被兄弟任务抢先预订（`insert` 返回 false）就在同一游标上
///      继续取下一个候选，不从头重探，因此任意并发度下都线性收敛；
///   3. 预订公开后锁外复核磁盘：兄弟任务在「探测 → 预订」窗口内完成并释放预订
///      时其最终文件已在磁盘上，复核命中则撤销预订、继续取下一个候选。
///
/// 编号耗尽或磁盘探测出错时用 UUID 后缀名（不可能与任何条目冲突）兜底。
fn reserve_deduped_name_blocking(
    reserved: &Mutex<HashSet<std::path::PathBuf>>,
    dir: &std::path::Path,
    name: &str,
    overwrite: &crate::file_exists::OverwritePolicy,
    companion_mp4: bool,
) -> (String, std::path::PathBuf) {
    let temp_of = |chosen: &str| dir.join(format!("{chosen}{}", downloader::TEMP_EXT));
    let allow_overwrite = overwrite.permits(name);
    let mut cursor = downloader::DedupCursor::new(
        dir,
        name,
        downloader::DedupRules {
            reserved: None,
            avoid: None,
            allow_overwrite,
            companion_mp4,
        },
    );
    loop {
        let chosen = match cursor.next_free() {
            Ok(Some(chosen)) => chosen,
            Ok(None) => break,
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "probe filename collisions",
                    &error,
                );
                break;
            }
        };
        let temp = temp_of(&chosen);
        if !lock_reserved(reserved).insert(temp.clone()) {
            continue;
        }
        let overwrite_final = allow_overwrite && chosen == name;
        match downloader::name_free_on_disk(dir, &chosen, overwrite_final, companion_mp4) {
            Ok(true) => return (chosen, temp),
            Ok(false) => {
                lock_reserved(reserved).remove(&temp);
            }
            Err(error) => {
                // 刚刚探测成功过，复核失败属瞬时故障：保守地改选 UUID 名。
                crate::logger::report_warning(
                    "download-manager",
                    "recheck reserved filename",
                    &error,
                );
                lock_reserved(reserved).remove(&temp);
                break;
            }
        }
    }
    let unique = downloader::uuid_suffixed_name(name);
    let temp = temp_of(&unique);
    lock_reserved(reserved).insert(temp.clone());
    (unique, temp)
}

/// [`reserve_deduped_name_blocking`] 的 async 包装：放进阻塞池执行，磁盘探测
/// 不占 async 工作线程，也不在持锁状态下做任何 I/O。
async fn reserve_deduped_name(
    reserved: &Arc<Mutex<HashSet<std::path::PathBuf>>>,
    dir: &std::path::Path,
    name: &str,
    overwrite: &crate::file_exists::OverwritePolicy,
    companion_mp4: bool,
) -> Result<(String, std::path::PathBuf), tokio::task::JoinError> {
    let reserved = Arc::clone(reserved);
    let dir = dir.to_path_buf();
    let name = name.to_string();
    let overwrite = overwrite.clone();
    tokio::task::spawn_blocking(move || {
        reserve_deduped_name_blocking(&reserved, &dir, &name, &overwrite, companion_mp4)
    })
    .await
}

/// 取出文件名预订集合的快照（只读判定与选名探测用；锁内仅克隆）。
fn reserved_snapshot(set: &Mutex<HashSet<std::path::PathBuf>>) -> HashSet<std::path::PathBuf> {
    lock_reserved(set).clone()
}

/// 取出文件名预订集合的互斥锁；锁中毒时恢复内层数据继续（集合仅存
/// 路径，无跨条目不变式，中毒后继续使用是安全的）。
fn lock_reserved(
    set: &Mutex<HashSet<std::path::PathBuf>>,
) -> std::sync::MutexGuard<'_, HashSet<std::path::PathBuf>> {
    set.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// HLS 分类误判恢复：`hls_downloader::run_hls_download` 返回
/// `Some(params)` 时，`params.file_name` 仍是 `finalize_start_file_name`
/// Step 3 强制归一化过的 `.ts` 名字——但内容其实根本不是 HLS 播放列表
/// （典型场景：WebDAV 把普通文件恰好命名为 `.m3u8`）。这里把文件名换回
/// 从 URL 重新解出的原始扩展名（`.m3u8`/`.m3u`），使随后交棒的普通 HTTP
/// 下载器把文件按字面原样保存，而不是继续顶着一个暗示"视频文件"的
/// `.ts` 名字。
///
/// 对新名字做一次只读冲突检查（磁盘 + 当前预订集合），避免撞车覆盖同名
/// 文件；但不注册新预订——该任务在 spawn 前经历的 `.ts` 预订仍会在
/// `TaskDone` 时按原样释放，这里只是这一极窄场景下的极小概率竞态，可
/// 接受（对称于 `run_hls_download_inner` 本身在此错误分支不写任何字节，
/// 不存在"半成品文件"需要清理）。
async fn restore_non_hls_file_name(
    mut params: DownloadParams,
    reserved: &Mutex<HashSet<std::path::PathBuf>>,
) -> DownloadParams {
    let restored =
        naming::extract_from_url(&params.url).unwrap_or_else(|| "download.m3u8".to_string());
    let save_path = std::path::PathBuf::from(&params.save_dir);
    let snapshot = reserved_snapshot(reserved);
    let overwrite = params.overwrite.clone();
    let probe_name = restored.clone();
    // 磁盘探测放进阻塞池，不占 async 工作线程。
    let deduped = match tokio::task::spawn_blocking(move || {
        dedup_filename_sync(&save_path, &probe_name, &snapshot, &overwrite, false)
    })
    .await
    {
        Ok(chosen) => chosen,
        Err(error) => {
            crate::logger::report_warning("download-manager", "join filename dedup probe", &error);
            downloader::uuid_suffixed_name(&restored)
        }
    };
    log_info!(
        "[download] task {} not an HLS playlist, falling back to plain HTTP download as {}",
        params.task_id,
        deduped
    );
    params.file_name = deduped;
    params
}

/// [`finalize_start_file_name`] 的结果：调用方据此决定是否继续启动下载器。
enum StartPrelude {
    /// 文件名持久化失败：不启动下载器，也不发布完成态。
    Failed { error: downloader::DownloadError },
    /// 继续启动下载器；`Some(path)` 是已预订的 `.fdownloading` 临时路径。
    Proceed(Option<std::path::PathBuf>),
    /// 磁盘上已存在同名最终文件且策略/决定为跳过：序幕在 dedup 之前就地判定
    /// 完成——不产生任何网络流量、不改名、不触碰已有文件。调用方应跳过下载器，
    /// 把任务标记为已完成（采纳已有文件，任务不拥有它）。
    SkipExisting { size: i64 },
    /// 需要询问用户：未预订任何临时路径，调用方让出并发槽后等待答复。
    AwaitDecision(crate::selection::FileConflict),
}

/// 启动序幕所需的「文件已存在」上下文（spawn 时快照）。
#[derive(Clone, Copy)]
struct PreludeCtx {
    global: FileExistsBehavior,
    support: crate::file_exists::AskSupport,
}

/// 协议对「文件已存在」询问/跳过的支持程度。
fn ask_support(
    url: &str,
    has_audio_track: bool,
    non_get_request: bool,
) -> crate::file_exists::AskSupport {
    use crate::file_exists::AskSupport;
    if hls_downloader::is_hls_url(url) || is_bt_url(url) || has_audio_track {
        AskSupport::None
    } else if dash_downloader::is_dash_url(url) || is_ed2k_url(url) {
        AskSupport::NoSkip
    } else if non_get_request {
        // 答复后以恢复路径重入，而非 GET 任务的恢复会被拒绝（请求体不落库），不能询问。
        AskSupport::None
    } else {
        AskSupport::WithSkip
    }
}

/// 启动序幕落库引擎选定的名字及其来源（`name_inferred`，见 db schema 注释）。
async fn persist_start_name(
    db: &crate::db::Db,
    task_id: &str,
    file_name: &str,
    inferred: bool,
) -> Result<(), crate::db::DbError> {
    db.set_task_file_name_with_source(task_id, file_name, inferred)
        .await
}

/// 序幕的探测能否直接用下载器的完整探测代替 `meta_prober` 的裸 HEAD：仅限 fresh、
/// 非 hint、无音轨对的 HTTP(S) GET 类任务——恰好是 `run_download` 会自己探测的那一类。
/// FTP / HLS / DASH / BT / ed2k、非 GET（POST 等重放有副作用）与 hint（探测会作废
/// 一次性 URL）仍走 `meta_prober` 或不探测，行为不变。
fn prelude_shares_downloader_probe(params: &DownloadParams) -> bool {
    downloader::single_get_probe_applies(params)
}

/// 启动序幕的名称 / 大小探测。
///
/// 满足 [`prelude_shares_downloader_probe`] 的任务用与下载相同的 client / spec 跑
/// `resolve_file_info_held_get`：只发【一个】plain GET（无 HEAD、无 Range 0-0，一次性
/// URL 的令牌不被探测消耗），响应头给出名称 / 大小，响应本身保留为第一条数据连接；
/// 含传输抖动重试与 UA / Referer 自适应。成功后：
/// - 名称 / 大小按 [`crate::meta_prober::ProbedMeta::from_file_info`] 派生（HTML
///   页面不产出名字）；
/// - 自适应后的 spec 与下载器一样即时落库，并连同 `FileInfo` 与保留的响应存入
///   `params.preprobed`，`run_download_inner` 据此跳过第二轮探测。
///
/// 探测失败返回空结果并把错误存入 `params.preprobed`：序幕按 URL 兜底命名，下载器
/// 直接上报该错误，不再重跑同一套重试阶梯。被取消时不设 `preprobed`。其余任务走
/// `meta_prober`（裸 HEAD / FTP / magnet / ed2k）。
async fn probe_start_meta(params: &mut DownloadParams) -> crate::meta_prober::ProbedMeta {
    if !prelude_shares_downloader_probe(params) {
        return tokio::select! {
            _ = params.cancel_token.cancelled() => crate::meta_prober::ProbedMeta::default(),
            r = crate::meta_prober::probe_task_meta(
                &params.url,
                &params.file_name,
                &params.client,
                &params.proxy_config,
                &params.spec,
            ) => r,
        };
    }
    let resolved = if crate::auto_probe::applies(params) {
        // Auto 多路径：起飞路径不及时应答 / 断在传输层时并行放出备选路径，
        // 胜出路径就地接管 params（client、代理配置、路由上下文与标签）。
        match crate::auto_probe::race_probe(params, downloader::ProbeKind::HeldGet).await {
            Err(downloader::DownloadError::Cancelled) => {
                return crate::meta_prober::ProbedMeta::default();
            }
            Ok(downloader::PreprobedInfo {
                info,
                adapted,
                held: Some(held),
            }) => Ok((info, adapted, held)),
            Ok(_) => Err(downloader::DownloadError::Other(
                "single GET probe returned no response".to_string(),
            )),
            Err(error) => Err(error),
        }
    } else {
        tokio::select! {
            _ = params.cancel_token.cancelled() => return crate::meta_prober::ProbedMeta::default(),
            r = downloader::resolve_file_info_held_get(
                &params.client,
                &params.url,
                &params.spec,
            ) => r,
        }
    };
    match resolved {
        Ok((info, adapted, held)) => {
            if let Some(adapted_spec) = &adapted {
                downloader::persist_adapted_spec(&params.db, &params.task_id, adapted_spec).await;
            }
            let meta = crate::meta_prober::ProbedMeta::from_file_info(&info);
            params.preprobed = Some(Ok(downloader::PreprobedInfo {
                info,
                adapted,
                held: Some(held),
            }));
            meta
        }
        Err(error) => {
            log_info!(
                "[download] task {} prelude probe failed: {}",
                params.task_id,
                error
            );
            params.preprobed = Some(Err(error));
            crate::meta_prober::ProbedMeta::default()
        }
    }
}

/// spawned task 启动序幕：文件名最终决策。manager 仍是唯一决策链——本函数
/// 是 `do_start_task` 派生任务的第一步，下载器自身不变更文件名；决策执行
/// 从 actor 内联挪到各任务自己的序幕，probe 的网络往返不再阻塞 actor
/// （期间暂停/删除等信号照常处理，创建任务后的 AllTasks 快照即刻送达）。
///
/// 流程：
///   1. 名称未知 → 先复读 DB（任务在 pending 队列等待期间，背景 probe
///      可能已把文件名写库；复用可避免对一次性 CDN URL 重复 probe 消耗 token）
///   2. 仍未知 → await probe（携带任务鉴权上下文，与真正下载同源，避免
///      鉴权站点把缺鉴权的裸 HEAD 重定向到登录页、用错误页的
///      Content-Disposition 污染文件名）；取消令牌可中断等待——下载器随后
///      会在自己的取消检查点走正常终态路径，此处不重复终态处理
///   3. HLS 归一化为 .ts / DASH 空名兜底为 .mp4（与各下载器最终落盘名
///      一致，否则不同前缀名会在下载器内塌缩为同一路径，绕过 dedup
///      导致两个任务静默覆盖同一文件）
///   4. `skip_if_exists` 命中且磁盘上已有同名最终文件 → 直接返回
///      `SkipExisting`，不进入 dedup（调用方据此把任务标记为已完成）
///   5. 临界区（锁内无 `.await`）：dedup → 预订 insert。共享互斥锁把各
///      任务序幕与删除路径的释放串行化，等价于旧 actor 同步段的原子性；
///      每任务 dedup 恰好一次，预订集合此刻只含兄弟任务，无自我冲突
///   6. dedup 改名时落库（预订已在锁内完成，兄弟任务经预订集合感知冲突，
///      不依赖 DB 中的名字）
///
/// `Proceed` 携带预订的临时路径，经 `TaskDone.reserved_temp_path` 在
/// `on_task_done` 释放；`Proceed(None)` = 名称最终仍未知（probe 失败），
/// 下载器内部按响应头/URL 兜底命名，但不参与 dedup 协调（极端情况，正常
/// 路径不会到此）。
async fn finalize_start_file_name(
    params: &mut DownloadParams,
    reserved: &Arc<Mutex<HashSet<std::path::PathBuf>>>,
    ctx: PreludeCtx,
) -> StartPrelude {
    // Step 1: DB 复读；存储故障不能被当成「尚无名字」继续落盘。
    if params.file_name.is_empty() {
        match params.db.load_task_by_id(&params.task_id).await {
            Ok(Some(task)) => params.file_name = task.file_name,
            Ok(None) => {
                return StartPrelude::Failed {
                    error: downloader::DownloadError::Other("task no longer exists".to_string()),
                };
            }
            Err(error) => {
                return StartPrelude::Failed {
                    error: error.into(),
                };
            }
        }
    }

    // Step 2: probe（名称仍未知时）。hint 任务（浏览器扩展 / 插件给了大小）不探测：
    // 一次性签名 URL 会被任何请求消耗，下载器也承诺跳过 probe；名字由下方 URL 兜底
    // 占位，完成期再按实际 GET 响应精修。探到的大小留给「文件已存在」询问展示。
    // 普通 HTTP(S) GET 任务直接跑下载器的完整探测（HEAD ∥ Range GET）并把结果交给
    // 下载器复用，整个启动只发一轮探测（见 [`probe_start_meta`]）。
    let mut probed_total: Option<i64> = None;
    if params.file_name.is_empty() && params.hint_file_size == 0 {
        let probed = probe_start_meta(params).await;
        probed_total = Some(probed.total_bytes);
        if !probed.file_name.is_empty() {
            params.file_name = probed.file_name;
            if let Err(error) = persist_start_name(
                &params.db,
                &params.task_id,
                &params.file_name,
                probed.name_inferred,
            )
            .await
            {
                return StartPrelude::Failed {
                    error: error.into(),
                };
            }
            params.sink.emit(EngineEvent::TaskMetaProbed {
                task_id: params.task_id.clone(),
                file_name: params.file_name.clone(),
                total_bytes: 0,
            });
        }
    } else if params.hint_file_size == 0
        && naming::needs_type_extension(&params.file_name)
        && prelude_shares_downloader_probe(params)
    {
        // Step 2b：显式名（新建对话框重命名 / API `out=`）缺类型扩展名。下载器本来就要
        // 探测，序幕代跑同一轮并经 `preprobed` 交给它复用，不多发请求。扩展名取自探测
        // 得出的完整名（CD > 重定向后的 CDN 路径 > 脚本端点名按 Content-Type 换扩展名），
        // 不是请求链接的后缀；拿不到可信扩展名（无扩展名 CDN 对象 + octet-stream）时
        // 保持原名，完成期再按实际 GET 证据与文件头魔数补。必须在「文件已存在」判断与
        // dedup 之前完成，二者都按最终名工作。
        let probed = probe_start_meta(params).await;
        probed_total = Some(probed.total_bytes);
        if let Some(name) =
            naming::complete_extension_from_name(&params.file_name, &probed.file_name)
        {
            params.file_name = name;
            if let Err(error) = params
                .db
                .set_task_file_name(&params.task_id, &params.file_name)
                .await
            {
                return StartPrelude::Failed {
                    error: error.into(),
                };
            }
            params.sink.emit(EngineEvent::TaskMetaProbed {
                task_id: params.task_id.clone(),
                file_name: params.file_name.clone(),
                total_bytes: 0,
            });
        }
    }

    // Step 3: HLS 归一化为 .ts。force_ts_extension 幂等；HLS 下载器内仍保留
    // 幂等的 force_ts 作为兜底/续传安全网。即使 probe 后仍空名，也用 URL
    // 末段兜底出与 HLS 下载器空名分支一致的名称，使空名 HLS 任务同样纳入
    // dedup + 预订协调——否则两个同源、均探测不到名的并发 HLS 任务会各自
    // 塌缩为同一 .ts 并互相 truncate/交错写入而损坏内容。
    if hls_downloader::is_hls_url(&params.url) {
        let base = if params.file_name.is_empty() {
            naming::extract_from_url(&params.url).unwrap_or_else(|| "download.ts".to_string())
        } else {
            params.file_name.clone()
        };
        let ts_name = hls_downloader::force_ts_extension(&base);
        if ts_name != params.file_name {
            params.file_name = ts_name;
            if let Err(error) = params
                .db
                .set_task_file_name(&params.task_id, &params.file_name)
                .await
            {
                return StartPrelude::Failed {
                    error: error.into(),
                };
            }
        }
    }

    // DASH：成品是 MP4，名称统一归一化为 .mp4。空名用 URL 末段兜底（与 DASH
    // 下载器空名分支一致），使空名 DASH 任务也纳入 dedup + 预订协调；探测得到
    // 的 manifest 名（`manifest.mpd`）同样要把 .mpd 换成 .mp4，否则成品是
    // .mpd 后缀的 MP4。其余扩展名的非空名原样使用。必须在 dedup/预订之前完成。
    if dash_downloader::is_dash_url(&params.url) {
        let is_mpd = params.file_name.to_ascii_lowercase().ends_with(".mpd");
        if params.file_name.is_empty() || is_mpd {
            let base = if params.file_name.is_empty() {
                naming::extract_from_url(&params.url).unwrap_or_else(|| "download.mpd".to_string())
            } else {
                params.file_name.clone()
            };
            let new_name = match base.rfind('.') {
                Some(pos) => format!("{}.mp4", &base[..pos]),
                None => format!("{}.mp4", base),
            };
            if new_name != params.file_name {
                params.file_name = new_name;
                if let Err(error) = params
                    .db
                    .set_task_file_name(&params.task_id, &params.file_name)
                    .await
                {
                    return StartPrelude::Failed {
                        error: error.into(),
                    };
                }
            }
        }
    } else if params.file_name.is_empty()
        && (params.url.starts_with("http://") || params.url.starts_with("https://"))
        && !hls_downloader::is_hls_url(&params.url)
        && !is_torrent_file_url(&params.url)
        && let Some(url_name) = naming::name_from_url(&params.url)
    {
        // 探测失败（超时/连接错误/HEAD 被挡）时名称仍空：若放任 Proceed(None)，
        // 同名任务（常见于同一 URL 重复添加）会在下载器内得到同一个名字而共享
        // 同一个 .fdownloading。用 URL 末段兜底，使其同样纳入 dedup + 预订；
        // 路径段只是占位（标记为推断名），完成期按实际 GET 响应精修。
        params.file_name = url_name.name;
        if let Err(error) = persist_start_name(
            &params.db,
            &params.task_id,
            &params.file_name,
            url_name.source != naming::NameSource::QueryDisposition,
        )
        .await
        {
            return StartPrelude::Failed {
                error: error.into(),
            };
        }
    }

    // Step 3.2：分类目录路由（外部捕获）。名字此刻已尽量确定（服务器文件名 / 类型推断 /
    // 插件 resolve），尚未写任何文件：按它重选目录，使后续「文件已存在」判断、去重、
    // 临时文件都落在最终目录，完成时通常无需移动。
    if let Err(error) = crate::category_route::route_before_start(params).await {
        return StartPrelude::Failed {
            error: error.into(),
        };
    }

    // Step 3.5：文件已存在决策——磁盘上已有同名最终文件时，在 dedup 之前按
    // 契约优先级选择动作。必须在 Step 4 之前判断：dedup 一旦介入就会把
    // file_name 改成不冲突的新名字，届时就再也对不上磁盘上这份文件了。
    if params.file_name.is_empty() {
        return StartPrelude::Proceed(None);
    }
    let save_path = std::path::PathBuf::from(&params.save_dir);
    let is_hls = hls_downloader::is_hls_url(&params.url);
    if let Some(candidate) = task_target_path(&params.save_dir, &params.file_name) {
        let existing = match tokio::fs::metadata(&candidate).await {
            Ok(metadata) if metadata.is_file() => Some(metadata),
            Ok(_) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                crate::logger::report_warning("download-manager", "stat existing file", &error);
                None
            }
        };
        if let Some(metadata) = existing {
            let temp_path = save_path.join(format!("{}{}", params.file_name, downloader::TEMP_EXT));
            let reserved_hit = lock_reserved(reserved).contains(&temp_path);
            let temp_on_disk = match tokio::fs::try_exists(&temp_path).await {
                Ok(found) => found,
                Err(error) => {
                    crate::logger::report_warning("download-manager", "probe temp file", &error);
                    true
                }
            };
            let decision = match params.db.get_exists_decision(&params.task_id).await {
                Ok(value) => crate::file_exists::ExistsDecision::from_db(&value),
                Err(error) => {
                    return StartPrelude::Failed {
                        error: error.into(),
                    };
                }
            };
            let facts = crate::file_exists::ExistsFacts {
                final_file_exists: true,
                temp_conflict: reserved_hit || temp_on_disk,
                decision,
                global: ctx.global,
                support: ctx.support,
                unattended: params.unattended,
                can_prompt: params.selector.can_prompt(),
            };
            match crate::file_exists::decide(&facts) {
                crate::file_exists::ExistsAction::Skip => {
                    return StartPrelude::SkipExisting {
                        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                    };
                }
                crate::file_exists::ExistsAction::Overwrite => {
                    if params.overwrite == crate::file_exists::OverwritePolicy::Never {
                        params.overwrite =
                            crate::file_exists::OverwritePolicy::Only(params.file_name.clone());
                    }
                }
                crate::file_exists::ExistsAction::Ask { allow_skip } => {
                    // 来源优先级：hint（扩展 / 插件给的大小）> 本次序幕探测 > 库内已知大小
                    // > 现场补一次探测（仅非 hint 任务：一次性 URL 由 hint 承诺不探测）。
                    let mut incoming_size = if params.hint_file_size > 0 {
                        Some(params.hint_file_size)
                    } else if let Some(total) = probed_total.filter(|total| *total > 0) {
                        Some(total)
                    } else {
                        match params.db.load_task_by_id(&params.task_id).await {
                            Ok(Some(task)) if task.total_bytes > 0 => Some(task.total_bytes),
                            Ok(_) => None,
                            Err(error) => {
                                return StartPrelude::Failed {
                                    error: error.into(),
                                };
                            }
                        }
                    };
                    if incoming_size.is_none()
                        && params.hint_file_size == 0
                        && probed_total.is_none()
                    {
                        let probed = tokio::select! {
                            _ = params.cancel_token.cancelled() => {
                                crate::meta_prober::ProbedMeta::default()
                            }
                            r = crate::meta_prober::probe_task_meta(
                                &params.url,
                                &params.file_name,
                                &params.client,
                                &params.proxy_config,
                                &params.spec,
                            ) => r,
                        };
                        if probed.total_bytes > 0 {
                            incoming_size = Some(probed.total_bytes);
                        }
                    }
                    let rename_preview = {
                        let snapshot = reserved_snapshot(reserved);
                        let preview_dir = save_path.clone();
                        let preview_name = params.file_name.clone();
                        match tokio::task::spawn_blocking(move || {
                            dedup_filename_sync(
                                &preview_dir,
                                &preview_name,
                                &snapshot,
                                &crate::file_exists::OverwritePolicy::Never,
                                is_hls,
                            )
                        })
                        .await
                        {
                            Ok(chosen) => chosen,
                            Err(error) => {
                                return StartPrelude::Failed {
                                    error: downloader::DownloadError::Other(format!(
                                        "preview renamed file name: {error}"
                                    )),
                                };
                            }
                        }
                    };
                    let existing_modified_unix_ms = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok());
                    return StartPrelude::AwaitDecision(crate::selection::FileConflict {
                        file_name: params.file_name.clone(),
                        save_dir: params.save_dir.clone(),
                        existing_size: Some(metadata.len()),
                        existing_modified_unix_ms,
                        incoming_size,
                        rename_preview,
                        allow_skip,
                    });
                }
                crate::file_exists::ExistsAction::Dedup => {}
            }
        }
    }

    // Step 4: dedup + 预订。磁盘探测在锁外（阻塞池）完成，锁内只核对 / 插入预订集合，
    // 冲突自动重试（见 [`reserve_deduped_name_blocking`]）。答复后磁盘状态可能已变，
    // dedup 在此重新探测：旧文件已消失则保留原名。
    let (deduped, temp) = match reserve_deduped_name(
        reserved,
        &save_path,
        &params.file_name,
        &params.overwrite,
        is_hls,
    )
    .await
    {
        Ok(reservation) => reservation,
        Err(error) => {
            return StartPrelude::Failed {
                error: downloader::DownloadError::Other(format!(
                    "reserve download file name: {error}"
                )),
            };
        }
    };
    // Step 5: dedup 改名落库。
    if deduped != params.file_name {
        params.file_name = deduped;
        if let Err(error) = params
            .db
            .set_task_file_name(&params.task_id, &params.file_name)
            .await
        {
            lock_reserved(reserved).remove(&temp);
            return StartPrelude::Failed {
                error: error.into(),
            };
        }
    }
    StartPrelude::Proceed(Some(temp))
}

/// [`TaskDone`] 的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskDonePhase {
    /// spawned task 已结束（成功/失败/取消）：释放槽位与预订，走完成收尾。
    Finished,
    /// 启动序幕要询问「文件已存在」：任务让出并发槽挂起（DB 状态保持 0），
    /// spawned task 仍在等待答复。宿主只需转交 [`DownloadManager::on_task_done`]。
    AwaitingDecision,
    /// 答复已到（Cancel 表示取消下载 → 任务暂停）：manager 持久化决定并把任务
    /// 重新排到队首作为全新起跑。
    DecisionReady(crate::selection::FileExistsChoice),
}

/// Notification sent from a spawned download task when it finishes (or parks).
pub struct TaskDone {
    pub task_id: String,
    /// Generation counter — must match `active_tokens` entry to allow cleanup.
    /// Prevents a stale TaskDone from an old spawn removing a newer token.
    pub generation: u64,
    /// 本次任务在启动序幕（`finalize_start_file_name`）中预订的临时文件
    /// 路径（`.fdownloading`）。`on_task_done` 收到后从 `reserved_temp_paths`
    /// 中移除，释放预订。BT 任务与名称最终仍未知（probe 失败）的任务为 `None`。
    pub reserved_temp_path: Option<std::path::PathBuf>,
    /// 通知阶段；宿主只有 `Finished` 才算任务结束。
    pub phase: TaskDonePhase,
}

impl TaskDone {
    /// spawned task 结束通知。
    pub fn finished(
        task_id: String,
        generation: u64,
        reserved_temp_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            task_id,
            generation,
            reserved_temp_path,
            phase: TaskDonePhase::Finished,
        }
    }
}

/// 「文件已存在」询问的等待上限；超时按重命名处理。
const FILE_EXISTS_PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// 非 BT 下载 spawned task 的共享输入（start 与 resume 两条入口共用）。
struct HttpFamilyRun {
    params: DownloadParams,
    use_ftp: bool,
    use_hls: bool,
    use_dash: bool,
    use_ed2k: bool,
    /// `Some` = 从未起跑过：先跑启动序幕（探测/命名/文件已存在决策/dedup 预订）；
    /// `None` = 续传，名字已定，直接交给下载器。
    prelude: Option<PreludeCtx>,
    reserved_set: Arc<Mutex<HashSet<std::path::PathBuf>>>,
    done_tx: mpsc::Sender<TaskDone>,
    spawn_gen: u64,
    /// resolver 下发的请求补丁；每次运行起跑前应用。
    resolver_patch: Option<(String, crate::intercept::RequestPatch)>,
    /// 系统 PAC 路由（见 [`crate::pac_route`]）；每次运行起跑前按链接修正出口。
    pac_routing: crate::pac_route::PacRouting,
}

async fn send_task_done(done_tx: &mpsc::Sender<TaskDone>, done: TaskDone) -> bool {
    if done_tx.send(done).await.is_err() {
        // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
        tracing::debug!("download-manager notification receiver closed");
        return false;
    }
    true
}

/// 序幕判定跳过：记下逐任务「跳过」决定（任务采纳已有文件而不拥有它），再标记完成。
async fn complete_skipped_task(params: &DownloadParams, size: i64) {
    log_info!(
        "[download] task {} skipped: file already exists ({})",
        params.task_id,
        params.file_name
    );
    let persisted = async {
        params
            .db
            .set_exists_decision(
                &params.task_id,
                crate::file_exists::ExistsDecision::Skip.as_db(),
            )
            .await?;
        params.db.update_task_status(&params.task_id, 3, "").await
    }
    .await;
    match persisted {
        Ok(()) => {
            if params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id.clone(),
                    downloaded_bytes: size,
                    // Adopting a local file transfers no body bytes.
                    download_baseline: Some(size),
                    total_bytes: size,
                    status: 3,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                tracing::debug!("download-manager notification receiver closed");
            }
        }
        Err(db_error) => {
            crate::logger::report_error(
                "download-manager",
                "persist skip-existing completion status",
                &db_error,
            );
            persist_task_failure(
                &params.task_id,
                format!("persist completion status: {db_error}"),
                &params.db,
                &params.progress_tx,
            )
            .await;
        }
    }
}

/// 起跑前的请求改写：resolver 补丁（失败即任务失败）→ `prepare` 阶段 → 镜像字段回填。
/// `first_run` 为真（有启动序幕、尚未定名）时允许改写文件名。
async fn prepare_request(
    params: &mut DownloadParams,
    first_run: bool,
    resolver_patch: Option<(String, crate::intercept::RequestPatch)>,
) -> Result<(), String> {
    use crate::intercept::{InterceptStage, drive, protocol_kind, wants};
    let run_prepare = wants(params.interceptor.as_ref(), InterceptStage::Prepare);
    if resolver_patch.is_some() || run_prepare {
        let interceptor = Arc::clone(&params.interceptor);
        let cancel = params.cancel_token.clone();
        let run_meta = params.run_meta.clone();
        let auto_max = params.auto_max_connections;
        let kind = protocol_kind(&params.url, params.audio_url.is_some());
        let has_audio_track = params.audio_url.is_some();
        let task_ctx = crate::intercept::ctx::task_ctx(params, kind);
        let notes = crate::intercept::ActivityNotes::of(params);
        let is_resume = params.is_resume;
        let mut target = crate::intercept::RequestTarget {
            url: &mut params.url,
            spec: &mut params.spec,
            file_name: first_run.then_some(&mut params.file_name),
            segment_count: Some(&mut params.segment_count),
            connection_cap: Some(&mut params.connection_cap),
            hint_file_size: Some(&mut params.hint_file_size),
            range_verified: Some(&mut params.range_verified),
            is_resume,
            kind,
            has_audio_track,
        };
        if let Some((identity, patch)) = &resolver_patch {
            drive::apply_resolver_patch(&notes, identity, patch, &mut target).await?;
        }
        if run_prepare
            && let Err(abort) = drive::run_prepare(
                interceptor.as_ref(),
                &notes,
                &cancel,
                &task_ctx,
                &run_meta,
                auto_max,
                &mut target,
            )
            .await
        {
            return Err(abort.to_string());
        }
        params.sync_request_mirrors();
        // 续传声明 ephemeral 却未给大小：沿用库中已知大小，避免以「大小未知」续传。
        if params.is_resume && params.hint_file_size == -1 {
            match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(task)) if task.total_bytes > 0 => params.hint_file_size = task.total_bytes,
                Ok(_) => {}
                Err(e) => log_warn!(
                    "[intercept] task {} ephemeral resume: failed to load known size: {e}",
                    params.task_id
                ),
            }
        }
    }
    Ok(())
}

async fn run_http_family_task(run: HttpFamilyRun) {
    let HttpFamilyRun {
        params,
        use_ftp,
        use_hls,
        use_dash,
        use_ed2k,
        mut prelude,
        reserved_set,
        done_tx,
        spawn_gen,
        resolver_patch,
        pac_routing,
    } = run;
    let task_id = params.task_id.clone();
    let panic_db = params.db.clone();
    let panic_progress_tx = params.progress_tx.clone();
    let mut params = params;
    // 拦截阶段的插件请求跟随任务出口（Prepare 用起跑出口；改道 / PAC 后再同步一次）。
    crate::intercept::sync_egress(&params);
    if !use_ed2k
        && let Err(message) = prepare_request(&mut params, prelude.is_some(), resolver_patch).await
    {
        persist_task_failure(&task_id, message, &panic_db, &panic_progress_tx).await;
        send_task_done(&done_tx, TaskDone::finished(task_id, spawn_gen, None)).await;
        return;
    }
    // 系统 PAC：在 resolver 补丁定下最终链接之后、任何网络请求之前修正出口。
    // 被取消时直接放行，由下载器在自己的取消检查点走正常终态。
    // 站点规则：最终链接若命中另一条规则则整体改道，此时出口已由规则决定，不再套 PAC。
    let rule_decided = !use_ed2k && crate::rule_route::apply(&mut params).await;
    let pac_proxy = if use_ed2k || rule_decided {
        None
    } else {
        let cancel = params.cancel_token.clone();
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            used = crate::pac_route::apply(&mut params, pac_routing) => used,
        }
    };
    crate::intercept::sync_egress(&params);
    if let Some(ctx) = prelude.as_mut() {
        ctx.support = ask_support(
            &params.url,
            params.audio_url.is_some(),
            params.spec.method != reqwest::Method::GET || params.spec.body.is_some(),
        );
    }
    let prelude = match prelude {
        Some(ctx) => finalize_start_file_name(&mut params, &reserved_set, ctx).await,
        None => StartPrelude::Proceed(None),
    };
    let reserved_temp_path = match prelude {
        StartPrelude::Failed { error } => {
            crate::logger::report_error("download-manager", "finalize start file name", &error);
            persist_task_failure(&task_id, error.to_string(), &panic_db, &panic_progress_tx).await;
            None
        }
        StartPrelude::SkipExisting { size } => {
            complete_skipped_task(&params, size).await;
            None
        }
        StartPrelude::AwaitDecision(conflict) => {
            // 挂起期间不占着单 GET 探测保留的响应连接；答复后任务重新启动、重新探测。
            params.preprobed = None;
            // 让出并发槽后在同一 task 内等待答复；暂停/删除经取消令牌打断等待。
            if !send_task_done(
                &done_tx,
                TaskDone {
                    task_id: task_id.clone(),
                    generation: spawn_gen,
                    reserved_temp_path: None,
                    phase: TaskDonePhase::AwaitingDecision,
                },
            )
            .await
            {
                return;
            }
            let answer = tokio::select! {
                _ = params.cancel_token.cancelled() => None,
                outcome = params
                    .selector
                    .select_file_exists(&task_id, &conflict, FILE_EXISTS_PROMPT_TIMEOUT) => Some(outcome),
            };
            let phase = match answer {
                Some(outcome) if !params.cancel_token.is_cancelled() => {
                    use crate::selection::{FileExistsChoice, SelectionOutcome};
                    let choice = match outcome {
                        SelectionOutcome::UserChose(choice) => choice,
                        SelectionOutcome::TimedOutDefaulted(_)
                        | SelectionOutcome::NoSelectorConfigured(_) => FileExistsChoice::Rename,
                    };
                    TaskDonePhase::DecisionReady(choice)
                }
                _ => TaskDonePhase::Finished,
            };
            send_task_done(
                &done_tx,
                TaskDone {
                    task_id,
                    generation: spawn_gen,
                    reserved_temp_path: None,
                    phase,
                },
            )
            .await;
            return;
        }
        StartPrelude::Proceed(reserved_temp_path) => {
            let result = if use_ftp {
                std::panic::AssertUnwindSafe(ftp_downloader::run_ftp_download(params))
                    .catch_unwind()
                    .await
            } else if use_hls {
                let hls_result =
                    std::panic::AssertUnwindSafe(hls_downloader::run_hls_download(params))
                        .catch_unwind()
                        .await;
                match hls_result {
                    Ok(Some(fallback_params)) => {
                        // 误判为 HLS 的普通文件——退回普通 HTTP 下载器，
                        // 文件名恢复为 URL 派生的原始扩展名。
                        let fallback_params =
                            restore_non_hls_file_name(fallback_params, &reserved_set).await;
                        std::panic::AssertUnwindSafe(downloader::run_download(fallback_params))
                            .catch_unwind()
                            .await
                    }
                    Ok(None) => Ok(()),
                    Err(panic_info) => Err(panic_info),
                }
            } else if use_dash {
                std::panic::AssertUnwindSafe(dash_downloader::run_dash_download(params))
                    .catch_unwind()
                    .await
            } else if use_ed2k {
                #[cfg(feature = "ed2k")]
                {
                    std::panic::AssertUnwindSafe(crate::ed2k::run_ed2k_download(params))
                        .catch_unwind()
                        .await
                }
                // 无 eD2K 后端：明确失败，绝不把 ed2k:// 当 HTTP 链接去请求。
                #[cfg(not(feature = "ed2k"))]
                {
                    drop(params);
                    persist_task_failure(
                        &task_id,
                        UNSUPPORTED_PROTOCOL_MESSAGE.to_string(),
                        &panic_db,
                        &panic_progress_tx,
                    )
                    .await;
                    Ok(())
                }
            } else {
                std::panic::AssertUnwindSafe(downloader::run_download(params))
                    .catch_unwind()
                    .await
            };

            if let Err(panic_info) = result {
                let msg = panic_message(&panic_info);
                handle_task_panic(&task_id, msg, &panic_db, &panic_progress_tx).await;
            }
            if let Some(used) = &pac_proxy {
                crate::pac_route::report_outcome(&panic_db, &task_id, used).await;
            }
            reserved_temp_path
        }
    };
    send_task_done(
        &done_tx,
        TaskDone::finished(task_id, spawn_gen, reserved_temp_path),
    )
    .await;
}

/// Per-task state tracked by the progress reporter for fixed-window speed
/// sampling.
///
/// Uses a fixed time window (`SPEED_SAMPLE_INTERVAL_MS`) instead of
/// per-update EMA: speed is computed once per window from the accumulated
/// byte delta, which naturally aggregates multi-segment updates and
/// eliminates noise from interleaved worker reports.
struct TaskSpeedState {
    /// EMA-smoothed speed in bytes/sec.
    ema_speed: f64,
    /// Speed counter at the start of the current sampling window (see
    /// `speed_from_transfer`).
    sample_bytes: i64,
    /// Timestamp of the current sampling window start.
    sample_time: std::time::Instant,
    /// Latest speed counter seen (for non-monotonic detection).
    latest_speed_bytes: i64,
    /// 测速计数器是否为传输层接收字节（`ProgressUpdate::transfer_bytes`），否则为
    /// 落盘字节。两种计数不可相减：来源切换时重设测量基线。
    speed_from_transfer: bool,
    /// 最近一次观测到的 `transfer_bytes`（同一次运行内单调）。
    last_transfer_bytes: Option<i64>,
    /// 已观测、尚未随转发帧交给宿主（`EngineEvent::TaskTransferred`）的接收字节。
    unsent_transfer_bytes: i64,
    /// Latest downloaded_bytes seen (transfer-baseline settlement).
    latest_bytes: i64,
    /// Resolved file_name (latched from the first non-empty update).
    file_name: String,
    /// Cached segment snapshot — updated on every incoming update that
    /// carries segment_details, regardless of rate-limiting.  This ensures
    /// the next send always has the latest segment data available.
    cached_segments: Option<Vec<SegmentProgressInfo>>,
    /// Latest observed transfer sample, retained until a terminal frame clears activity.
    cached_runtime: Option<TaskRuntime>,
    /// Last status sent to the host.  Used to detect status transitions so that
    /// they are always forwarded immediately (not rate-limited).
    last_sent_status: i32,
    /// Last raw status observed from downloader updates.
    last_raw_status: i32,
    /// 进入下载态后是否仍在等待第一个「有增长」的更新。该更新可能携带
    /// resume 基线跳变（先收 status=1/bytes=0，下一条直接跳到已恢复字节），
    /// 其 delta 不代表真实传输——只用作测量基线、不计入速度。取代旧的
    /// 「整窗 warmup 跳过」：首个速度值从 ~2s 提前到 <1s。
    awaiting_first_growth: bool,
    /// 本次下载态内是否已推送过非零速度。首个非零速度视同状态变更，
    /// 绕过 500ms 节流立即推送（UI 的速度与 ETA 即刻出现）。
    sent_nonzero_speed: bool,
    /// Whether the "no cached segments" anomaly has already been logged for
    /// this task — it indicates a real problem (segment visualization will
    /// be empty) but repeats on every update, so log it only once.
    logged_missing_segments: bool,
    /// Latest upload speed (bytes/sec) reported by the downloader.
    /// Non-zero only for BT tasks (librqbit stats); latched from every
    /// incoming update so throttled emits carry the freshest value.
    upload_bps: i64,
    /// Last raw `uploaded_bytes` snapshot from the downloader (librqbit
    /// session counter). Used to compute deltas for cumulative upload
    /// accounting across pause/resume and session rebuilds.
    last_uploaded_snapshot: i64,
    /// Cumulative uploaded bytes for BT tasks. Kept in memory so the UI
    /// never shows the librqbit counter reset to zero after pause/resume.
    cumulative_uploaded: i64,
}

/// 解析 `HH:MM` 为当日分钟数（0..1440）。非法输入返回 `None`。
fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// 任务/队列两级上传限速的合成核心（纯函数，B/s，0 = 不设 torrent 级限制）。
///
/// 优先级：任务级 > 队列级。任务级 `task_bps` > 0 直接生效；否则队列级
/// `queue_kbps` > 0 时折算 ×1024；`queue_kbps` 为 `None` 表示任务不属于
/// 任何已知队列。全局 `upload_limit_bytes` 是 librqbit 会话级的第二层
/// 限速，恒作为上限叠加，不在此参与计算。
#[cfg(feature = "bt")]
fn effective_upload_bps(task_bps: i64, queue_kbps: Option<i64>) -> u64 {
    if task_bps > 0 {
        return task_bps as u64;
    }
    (queue_kbps.unwrap_or(0).max(0) as u64) * 1024
}

/// 做种上传采样发进度帧所需的任务行事实（已落库的累计值）。求值 tick 写库后
/// 刷新；采样帧的累计上传 = `uploaded_bytes` + 做种者未落库的增量。
#[cfg(feature = "bt")]
#[derive(Debug, Clone, Copy)]
struct SeedRowTotals {
    downloaded_bytes: i64,
    total_bytes: i64,
    uploaded_bytes: i64,
}

#[cfg(feature = "bt")]
impl SeedRowTotals {
    fn of(task: &TaskInfo) -> Self {
        Self {
            downloaded_bytes: task.downloaded_bytes,
            total_bytes: task.total_bytes,
            uploaded_bytes: task.uploaded_bytes,
        }
    }
}

/// 定时边沿标识：`(queue_id, 是否启动边沿)`。启动/停止各是独立边沿，
/// 分别按天记账（见 `DownloadManager::schedule_fired`）。
type ScheduleEdge = (String, bool);

/// 定时调度的边沿判定核心（纯函数）。
///
/// 对每个启用定时且 `day_bit` 命中的队列，找出「今天时刻已过（`now_min`）
/// 且 `fired` 账本里今天尚未处理」的启动/停止边沿：
/// - 返回值 `.0` = 本次新越过的全部边沿及其应记账的日期（通常是 `today`；
///   跨午夜窗口在凌晨补触发的「昨日启动边沿」记为昨天，不能占掉今天 23:00
///   的正常启动），保证每边沿每天至多处理一次——含手动启停后的不重复触发；
/// - 返回值 `.1` = 应执行的动作 `(queue_id, 是否启动)`；同队列同天两个
///   边沿都新近越过时只保留时间靠后的那个，平局（start == stop）取停止。
fn due_schedule_actions<'a>(
    queues: impl Iterator<Item = &'a QueueInfo>,
    fired: &HashMap<ScheduleEdge, chrono::NaiveDate>,
    today: chrono::NaiveDate,
    day_bit: i32,
    now_min: u32,
) -> (Vec<(ScheduleEdge, chrono::NaiveDate)>, Vec<ScheduleEdge>) {
    let mut passed_edges: Vec<(ScheduleEdge, chrono::NaiveDate)> = Vec::new();
    let mut actions: Vec<ScheduleEdge> = Vec::new();
    for q in queues {
        if !q.schedule_enabled || (q.schedule_days & day_bit) == 0 {
            continue;
        }
        // (边沿时刻, 是否启动边沿)；未配置/非法的边沿跳过。
        let mut newest: Option<(u32, bool)> = None;
        for (time, is_start) in [
            (parse_hhmm(&q.schedule_start), true),
            (parse_hhmm(&q.schedule_stop), false),
        ] {
            let Some(minute) = time else { continue };
            if now_min < minute {
                continue; // 今天尚未到点
            }
            if fired.get(&(q.queue_id.clone(), is_start)) == Some(&today) {
                continue; // 今天已处理过该边沿
            }
            passed_edges.push(((q.queue_id.clone(), is_start), today));
            // 平局取停止边沿：迭代顺序 start 在前，`>=` 让后者覆盖——宁停不启。
            if newest.is_none_or(|(m, _)| minute >= m) {
                newest = Some((minute, is_start));
            }
        }
        if let Some((_, is_start)) = newest {
            actions.push((q.queue_id.clone(), is_start));
        }
        // 跨午夜窗口（stop < start，如 23:00-07:00）：凌晨（now < stop）时昨日的
        // 启动边沿已过，睡眠/重启错过时需补触发，否则整晚都不会下载。
        if newest.is_none()
            && let (Some(start), Some(stop)) =
                (parse_hhmm(&q.schedule_start), parse_hhmm(&q.schedule_stop))
            && stop < start
            && now_min < stop
        {
            let prev_bit = if day_bit == 1 { 1 << 6 } else { day_bit >> 1 };
            let yesterday = today.pred_opt().unwrap_or(today);
            let key = (q.queue_id.clone(), true);
            let handled = matches!(fired.get(&key), Some(d) if *d == yesterday || *d == today);
            if (q.schedule_days & prev_bit) != 0 && !handled {
                passed_edges.push((key, yesterday));
                actions.push((q.queue_id.clone(), true));
            }
        }
    }
    (passed_edges, actions)
}

/// 新建下载任务的完整描述（[`DownloadManager::create_task`] 入参）。
///
/// 全字段带默认值，调用方只填关心的字段，后续新增字段不再震荡全部调用点：
///
/// ```
/// use fluxdown_engine::download_manager::NewTaskSpec;
///
/// let spec = NewTaskSpec {
///     url: "https://example.com/file.bin".to_string(),
///     save_dir: "/tmp".to_string(),
///     ..Default::default()
/// };
/// assert!(!spec.start_paused);
/// ```
#[derive(Clone, Default)]
pub struct NewTaskSpec {
    /// 下载 URL（http/https/ftp/magnet/ed2k/…）。
    pub url: String,
    /// 保存目录。
    pub save_dir: String,
    /// 文件名（空 = 探测/URL 推断）。
    pub file_name: String,
    /// 参照文件名（浏览器捕获时浏览器定下的原始名；空 = 无）。`file_name` 是缺类型
    /// 扩展名的显式名时借它的扩展名补全，见 [`naming::complete_extension_offline`]。
    pub name_hint: String,
    /// 分段数（<=0 = 自动）。
    pub segments: i32,
    /// 浏览器 cookies（空 = 无）。
    pub cookies: String,
    /// Referrer（空 = 无）。
    pub referrer: String,
    /// 已知文件大小 hint（0 = 未知；-1 = 未知且跳过 probe）。
    pub hint_file_size: i64,
    /// `.torrent` 文件内容（非空 = 种子任务）。
    pub torrent_file_bytes: Vec<u8>,
    /// 单任务代理（空 = 全局）。
    pub proxy_url: String,
    /// 单任务 User-Agent（空 = 队列/全局）。
    pub user_agent: String,
    /// 目标队列 ID；空 = 内置主队列（[`MAIN_QUEUE_ID`]）。
    pub queue_id: String,
    /// Checksum spec `algo=hexhash`（空 = 跳过校验）。
    pub checksum: String,
    /// 忽略 HTTPS 证书错误（默认 false；仅由用户为当前任务显式启用）。
    pub ignore_tls_errors: bool,
    /// 额外请求头。
    pub extra_headers: std::collections::HashMap<String, String>,
    /// BT 文件预选（空 = 全部文件）。
    pub selected_file_indices: Vec<i32>,
    /// 无人值守创建：跳过一切需要用户介入的二次选择——BT 任务直接按
    /// 「全部文件」落库（**不弹文件选择框**），HLS/DASH 画质与插件 resolve
    /// 变体在 start/resume 时静默取默认值（持久化于 `tasks.unattended`）。
    ///
    /// RSS 订阅这类自动化入口必须置 true——半夜抓到 5 集就弹 5 次对话框是
    /// 不可接受的，而且用户点「取消」后条目已被标记「已下载」，状态就撒谎了。
    /// 外部接管入口（浏览器扩展/脚本/aria2 免打扰路径）由宿主按
    /// 「免打扰跳过二次选择」设置（config `silent_skip_selection`）决定。
    /// 手动新建下载保持 false，用户仍然自己挑文件/画质。
    pub unattended_selection: bool,
    /// 自定义 HTTP method（None = GET）。
    pub method: Option<String>,
    /// 捕获的请求体（POST 接管）。
    pub body: Option<downloader::CapturedRequestBody>,
    /// 音频轨 URL（「视频轨+音频轨」离散下载对）。
    pub audio_url: Option<String>,
    /// 稍后下载：true = 建任务后不启动（paused 入库），待「启动队列」
    /// 按序恢复或用户手动恢复。
    pub start_paused: bool,
    /// 所属任务组 ID（空 = 不属于任何组）。由 [`DownloadManager::create_task_group`]
    /// 传入；`create_task` 落库后调用 [`crate::db::Db::set_task_group`]。
    pub group_id: String,
    /// 二段解析标识（不透明字符串，对应清单条目 `id`/`id@variantId`）。非空但
    /// 未命中 resolver 插件 → fail-closed，任务直接 status=4（不发起下载
    /// `source_url`，绝不把网页 HTML 当直链保存）。
    pub resolver_item: String,
    /// 显式 resolver 绑定（任务组成员：预览时产出清单的插件）。非空 = 只绑定该插件；它当前
    /// 未启用或不带 resolver 时任务 fail-closed（status=4，「解析插件不可用」），绝不回退到
    /// 其它插件。空 = 自动路由：普通任务按 `route_resolver`，带 `resolver_item` 的成员按
    /// `match_multi_resolver`（与预览同一规则）。
    pub resolver_plugin_id: String,
    /// HTTP Basic 认证用户名（空 = 未提供；非空时引擎生成
    /// `Authorization: Basic` 头注入 extra_headers，覆盖同名捕获头）。
    pub http_user: String,
    /// HTTP Basic 认证密码（仅 `http_user` 非空时有意义，允许为空串）。
    pub http_password: String,
    /// 为此网站保存凭据：true 且 `http_user` 非空时按站点键存入
    /// config（[`crate::site_auth::SITE_AUTH_CONFIG_KEY`]），后续同站点
    /// 建任务未显式提供凭据时自动套用。
    pub save_site_auth: bool,
    /// 分类目录路由（外部捕获未替用户选定保存目录时由宿主传入）：建任务按初始名选初始
    /// 目录、完成前按最终文件名复核，`save_dir` 是未命中时的基准目录。BT / eD2K、任务组
    /// 成员与 `out=子目录/名字` 不路由，见 [`crate::category_route`]。
    pub category_route: Option<crate::category_route::CategoryRoute>,
}

/// 单个任务被拒绝创建的原因（逐项拒绝，不影响同批其它任务；也是批量创建
/// 幂等记录的一部分，故可序列化）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CreateRejection {
    /// BT 被用户禁用（magnet / 种子字节 / 解出 BT 地址的 thunder 链接）。
    BtDisabled,
    /// 链接协议在本构建中不可用（eD2K 被编译期剔除）。BT 被编译期剔除时沿用
    /// [`Self::BtDisabled`]（其文案即通用「不支持的链接」）。
    UnsupportedProtocol,
    /// thunder:// 链接解码失败。
    InvalidThunderLink(String),
    /// checksum spec 畸形（未知算法 / 哈希非十六进制 / 长度不符）。
    InvalidChecksum(String),
    /// 请求头无法序列化落库。
    InvalidHeaders(String),
}

impl std::fmt::Display for CreateRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BtDisabled => formatter.write_str(BT_DISABLED_MESSAGE),
            Self::UnsupportedProtocol => formatter.write_str(UNSUPPORTED_PROTOCOL_MESSAGE),
            Self::InvalidThunderLink(reason) => write!(formatter, "invalid thunder link: {reason}"),
            Self::InvalidChecksum(reason) => write!(formatter, "invalid checksum: {reason}"),
            Self::InvalidHeaders(reason) => write!(formatter, "invalid request headers: {reason}"),
        }
    }
}

/// [`DownloadManager::create_tasks_batch`] 的单项结果（与入参同序）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BatchItemOutcome {
    /// 新建成功的任务 ID；`dedup_same_url` 命中时为已有活跃任务的 ID。
    Created(String),
    /// 该项被拒绝，未创建任务。
    Rejected(CreateRejection),
}

/// 「解析插件不可用」fail-closed 任务的错误文案。
const RESOLVER_UNAVAILABLE_MESSAGE: &str = "解析插件不可用";

/// 后台元数据探测的并发上限。批量导入会一次登记上千个探测，不设闸门会同时
/// 发起上千个 HEAD 请求。
const META_PROBE_CONCURRENCY: usize = 8;

/// 元数据探测窗口：只对排队前这么多位的任务（以及同时在途不超过这么多的「稍后下载」
/// 任务）发起探测。批量导入上万任务时，其余任务不再各自发 HEAD + Range GET，
/// 名字与大小在下载开始时自然得到。
const META_PROBE_WINDOW: usize = 64;

/// 一个在途元数据探测占用的窗口名额；释放（含探测提前返回）时归还。
struct ProbeSlot(Arc<AtomicUsize>);

impl ProbeSlot {
    fn acquire(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(counter))
    }
}

impl Drop for ProbeSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// [`DownloadManager::task_http_context`] 的产物：任务出口及其逐请求规则路由。
pub(crate) struct TaskEgress {
    pub(crate) client: Client,
    pub(crate) proxy: ProxyConfig,
    /// 路由标签：Auto 决策标签、站点规则标签（`rule:*`），或空串。
    pub(crate) route: &'static str,
    pub(crate) auto_ctx: Option<Arc<crate::auto_proxy::AutoProxyCtx>>,
    /// 站点规则的逐请求路由器；任务显式指定了代理或没有启用规则时为 `None`。
    pub(crate) router: Option<Arc<crate::rule_route::EgressRouter>>,
    pub(crate) pac: crate::pac_route::PacRouting,
}

/// 创建流水线「准备」阶段的产物：落库行与发布动作所需的全部内存事实。
struct PreparedTask {
    task_id: String,
    /// 真实下载地址（thunder 已解码）。
    url: String,
    /// 落库 URL：种子文件任务为 `torrent-file://local` 哨兵。
    db_url: String,
    /// 展示用原始来源（thunder 原链接 / 种子任务的 http 来源）；空 = 无。
    origin_url: String,
    save_dir: String,
    file_name: String,
    /// hint 稍后下载任务的 URL 推断占位名及其「推断」标记；随创建行一并落库。
    provisional_name: Option<(String, bool)>,
    segments: i32,
    hint_file_size: i64,
    torrent_file_bytes: Vec<u8>,
    proxy_url: String,
    user_agent: String,
    queue_id: String,
    checksum: String,
    ignore_tls_errors: bool,
    cookies: String,
    referrer: String,
    extra_headers: HashMap<String, String>,
    extra_headers_json: String,
    selected_file_indices: Vec<i32>,
    method: Option<String>,
    body: Option<downloader::CapturedRequestBody>,
    audio_url: Option<String>,
    start_paused: bool,
    group_id: String,
    resolver_item: String,
    unattended_selection: bool,
    resolver_plugin_id: String,
    /// 多插件同时匹配且站点无确认过的默认插件（见 `plugin::routing`）。
    resolver_choice_pending: bool,
    /// 显式绑定的插件建档时不可用（见 [`DownloadManager::plugin_bind_resolver`]）。
    resolver_fail_closed: bool,
    is_bt: bool,
    /// `tasks.category_route` 列值（空串 = 不路由）。
    category_route: String,
    /// `tasks.request_method` 列值（空串 = GET；见 `downloader::persisted_request_method`）。
    request_method: String,
}

impl PreparedTask {
    fn has_resolver(&self) -> bool {
        !self.resolver_plugin_id.is_empty()
    }

    /// fail-closed 建档：`resolver_item` 非空但未命中插件，或显式绑定的插件当前不可用。
    /// 任务直接以 error 落库（显式绑定的插件 ID 仍保留，插件恢复后手动恢复即可解析）。
    fn resolver_unavailable(&self) -> bool {
        self.resolver_fail_closed || (!self.resolver_item.is_empty() && !self.has_resolver())
    }

    /// 稍后下载以 paused(2) 落库；正常创建 pending(0)。
    fn initial_status(&self) -> i32 {
        if self.start_paused { 2 } else { 0 }
    }

    fn row(&self) -> NewTaskRow<'_> {
        let (status, error_message) = if self.resolver_unavailable() {
            (4, RESOLVER_UNAVAILABLE_MESSAGE)
        } else {
            (self.initial_status(), "")
        };
        let (file_name, name_inferred) = match &self.provisional_name {
            Some((name, inferred)) => (name.as_str(), *inferred),
            None => (self.file_name.as_str(), false),
        };
        NewTaskRow {
            id: &self.task_id,
            url: &self.db_url,
            file_name,
            name_inferred,
            save_dir: &self.save_dir,
            segments: self.segments,
            status,
            error_message,
            proxy_url: &self.proxy_url,
            queue_id: &self.queue_id,
            checksum: &self.checksum,
            ignore_tls_errors: self.ignore_tls_errors,
            cookies: &self.cookies,
            referrer: &self.referrer,
            extra_headers_json: &self.extra_headers_json,
            origin_url: &self.origin_url,
            audio_url: self
                .audio_url
                .as_deref()
                .filter(|audio| !audio.is_empty())
                .unwrap_or(""),
            resolver_plugin_id: &self.resolver_plugin_id,
            resolver_choice_pending: self.resolver_choice_pending,
            group_id: &self.group_id,
            resolver_item: &self.resolver_item,
            unattended: self.unattended_selection,
            // 无人值守的 BT 任务按「全部文件」落库（三态语义见 `save_bt_selected_files`）。
            bt_selected_files: if self.unattended_selection && self.is_bt {
                "all"
            } else {
                ""
            },
            category_route: &self.category_route,
            request_method: &self.request_method,
            torrent_file_bytes: &self.torrent_file_bytes,
        }
    }
}

/// 单项「准备」结果。
enum PrepareOutcome {
    Ready(Box<PreparedTask>),
    /// `dedup_same_url` 命中：返回已有活跃任务 ID，不创建。
    Deduped(String),
    Rejected(CreateRejection),
}

/// 一次创建调用内共享的只读/累积上下文：去重候选与站点凭据库各只读一次。
struct CreateBatchContext {
    /// 归一化 URL → 活跃任务 ID；`None` = 去重关闭（或本批无可比较 URL）。
    /// 新接受的任务即时并入，批内重复链接因此解析为首项的 ID。
    dedup: Option<HashMap<String, String>>,
    site_auth: SiteAuthSession,
}

impl CreateBatchContext {
    /// 登记已接受的任务，使同批后续重复项命中它。创建即失败（status=4）
    /// 的任务不属于活跃集合，与逐条创建时 `active_task_urls` 的状态过滤一致。
    fn note_accepted(&mut self, task: &PreparedTask) {
        if let Some(active) = self.dedup.as_mut()
            && !task.resolver_unavailable()
        {
            active
                .entry(normalize_url_for_dedup(&task.db_url))
                .or_insert_with(|| task.task_id.clone());
        }
    }
}

/// 站点 Basic 认证凭据库的批内会话：首次需要时读取一次，新保存的凭据在
/// 内存中累积、批末最多写回一次。
#[derive(Default)]
struct SiteAuthSession {
    store: Option<std::collections::BTreeMap<String, crate::site_auth::SiteCredential>>,
    /// 读取失败后本批不再重试（已记录一次警告）；失败时绝不能用空库覆盖。
    unavailable: bool,
    /// 有新保存的凭据待写回。
    dirty: bool,
}

/// 创建事实的发布形态。
#[derive(Clone, Copy)]
enum CreationAnnounce {
    /// 单个创建：每个任务一条 [`EngineEvent::TaskChanged`]（建组/裂变抑制时不发）。
    Single,
    /// 批量创建：整批一条 [`EngineEvent::TasksChanged`]。
    Batch,
}

/// [`DownloadManager::create_task_group`] 的单个组成员条目（清单条目的引擎侧
/// 投影；`resolver_item` 拼接规则见设计文档 D5/§7.2，由调用方按
/// `<itemId>` / `<itemId>@<variantId>` 固定规则拼接）。
#[derive(Clone, Default)]
pub struct GroupItemSpec {
    /// 二段解析标识（不透明字符串）。
    pub resolver_item: String,
    /// 文件名。
    pub file_name: String,
    /// 相对组根目录的子路径（空 = 组根）。
    pub rel_path: String,
    /// 已知大小（字节，0 = 未知），透传为 [`NewTaskSpec::hint_file_size`]。
    pub size: i64,
}

/// [`DownloadManager::create_task_group`] 的建组请求（B3 契约）。
#[derive(Clone, Default)]
pub struct CreateGroupSpec {
    /// 原始分享/清单链接（组行 `source_url`，展示/复制用）。
    pub source_url: String,
    /// 组名（`task_groups.name`；空 = 组根目录直接用 `base_save_dir`）。
    pub group_name: String,
    /// 基础保存目录（组根目录 = `base_save_dir/sanitize(group_name)`）。
    pub base_save_dir: String,
    pub queue_id: String,
    pub segments: i32,
    pub cookies: String,
    pub referrer: String,
    pub user_agent: String,
    pub proxy_url: String,
    pub extra_headers: std::collections::HashMap<String, String>,
    pub ignore_tls_errors: bool,
    /// 稍后下载：透传给每个成员的 [`NewTaskSpec::start_paused`]。
    pub start_paused: bool,
    pub items: Vec<GroupItemSpec>,
    /// 预览时产出清单的插件 identity（`ResolvePreviewOutcome::plugin_id`），逐成员透传为
    /// [`NewTaskSpec::resolver_plugin_id`]。空 = 旧客户端，成员按预览同款规则
    /// （`match_multi_resolver`）选插件。
    pub resolver_plugin_id: String,
}

/// [`DownloadManager::spawn_resolve_preview`] 的一次性结果，经 `oneshot`
/// 回传给调用方（actor 内 [`DownloadManager::begin_resolve_preview`] 转发为
/// [`crate::events::EngineEvent::ResolvePreviewReady`]；管理 API 宿主
/// 直接消费本结构的字段组装预解析响应）。
pub struct ResolvePreviewOutcome {
    pub name: String,
    pub items: Vec<crate::model::ManifestItemInfo>,
    /// 无错误时为空。
    pub error: String,
    /// 产出清单的插件 identity（`match_multi_resolver` 的选择）；无清单时为空。
    /// 客户端据此建组，把成员绑定到同一个插件，保证预览与实际下载同源。
    pub plugin_id: String,
}

impl ResolvePreviewOutcome {
    /// 插件未返回清单 / 未命中 `multi` resolver / `plugins` feature 关闭时
    /// 的空结果。
    fn empty() -> Self {
        Self {
            name: String::new(),
            items: Vec::new(),
            error: String::new(),
            plugin_id: String::new(),
        }
    }

    /// 插件调用失败（`panic` 或 `PluginError`）时的错误结果。
    fn failed(error: String) -> Self {
        Self {
            name: String::new(),
            items: Vec::new(),
            error,
            plugin_id: String::new(),
        }
    }
}

/// Information needed to start a queued task later.
struct QueuedTask {
    task_id: String,
    url: String,
    save_dir: String,
    file_name: String,
    segments: i32,
    is_resume: bool,
    cookies: String,
    /// HTTP Referer header value. Empty = do not send Referer.
    referrer: String,
    /// File size hint from the browser extension. 0 = no hint / use probe.
    hint_file_size: i64,
    /// Raw .torrent file bytes (empty for magnet/HTTP/FTP tasks).
    torrent_file_bytes: Vec<u8>,
    /// Per-task proxy URL override (e.g. "socks5://user:pass@host:port").
    /// Empty = use global proxy setting.
    proxy_url: String,
    /// Per-task user-agent override. Empty = use global UA setting.
    user_agent: String,
    /// Named queue ID this task belongs to. Empty = default queue.
    queue_id: String,
    /// Checksum spec for post-download integrity verification.
    /// Format: "algo=hexhash". Empty = skip verification.
    checksum: String,
    /// Per-task HTTPS certificate policy. False = strict verification.
    ignore_tls_errors: bool,
    /// 浏览器扩展捕获的额外 HTTP 请求头（如 Authorization）。
    extra_headers: std::collections::HashMap<String, String>,
    /// Pre-selected file indices for BT downloads (from the new-download dialog).
    /// Non-empty = skip the BtFilesInfo dialog.
    #[cfg_attr(not(feature = "bt"), allow(dead_code))]
    selected_file_indices: Vec<i32>,
    /// 浏览器扩展捕获的原始 HTTP method（如 "POST"）。`None` 视为 "GET"。
    /// 配合 `body` 字段一起重建 form-POST 等触发的下载请求事务。
    method: Option<String>,
    /// 浏览器扩展捕获的原始请求体（仅非 GET 时有意义）。
    body: Option<downloader::CapturedRequestBody>,
    /// 音频轨 URL（离散音视频轨对下载）。`Some` 时 `url` 为视频轨、此为音频轨，
    /// 引擎分别下载后 mux 合并；`None` 为普通单 URL 下载。
    audio_url: Option<String>,
    /// 命中的 resolver 插件 ID（空 = 无插件）。始终存在（feature 关时恒空且不读取）。
    #[cfg_attr(not(feature = "plugins"), allow(dead_code))]
    resolver_plugin_id: String,
    /// 是否已完成惰性解析（off-actor resolve 回流后置 true，避免重复解析）。
    #[cfg_attr(not(feature = "plugins"), allow(dead_code))]
    resolved: bool,
    /// resolver 插件担保直链支持 Range（`rangeSupported: true`）：跳过 probe 的
    /// 同时按已验证 Range 规划多段，不落入配额型端点式的保守单流启动。
    range_supported: bool,
    /// 二段解析标识（透传给 [`crate::plugin::ResolveRequest::resolver_item`]）。
    /// 空 = 初段解析。始终存在（feature 关时恒空且不读取）。
    #[cfg_attr(not(feature = "plugins"), allow(dead_code))]
    resolver_item: String,
    /// 任务持久化的原始 URL（插件拦截上下文的 `sourceUrl`；resolver 改写前）。
    source_url: String,
    /// resolver 下发的请求补丁（`(插件 ID, 补丁)`），每次运行起跑前应用。
    resolver_patch: Option<(String, crate::intercept::RequestPatch)>,
}

impl QueuedTask {
    /// 由 DB 行构造「恢复」队列项：请求上下文、分段数、音频轨等由
    /// `do_resume_task` 从 DB 重新读取，这里只带调度所需的最小字段。
    fn for_resume(task: TaskInfo) -> Self {
        Self {
            task_id: task.task_id,
            url: task.url,
            save_dir: task.save_dir,
            file_name: task.file_name,
            segments: 0,
            is_resume: true,
            cookies: String::new(),
            referrer: String::new(),
            hint_file_size: 0,
            torrent_file_bytes: Vec::new(),
            proxy_url: task.proxy_url,
            user_agent: String::new(),
            queue_id: task.queue_id,
            checksum: task.checksum,
            ignore_tls_errors: false,
            extra_headers: std::collections::HashMap::new(),
            selected_file_indices: Vec::new(),
            method: None,
            body: None,
            audio_url: None,
            resolver_plugin_id: String::new(),
            resolved: false,
            range_supported: false,
            resolver_item: String::new(),
            source_url: String::new(),
            resolver_patch: None,
        }
    }
}

/// [`PendingQueue`] 的增量聚合计数。
#[derive(Default)]
struct PendingCounts {
    /// 队列 ID → 排队项数（计数为 0 的队列不留键）。
    per_queue: HashMap<String, usize>,
    /// BT 项数（`is_bt_url` 或携带 .torrent 字节）。
    bt_items: usize,
    /// 仅按 URL 判定的 BT 项数（磁力 / .torrent 链接）。
    bt_url_items: usize,
}

impl PendingCounts {
    fn add(&mut self, queued: &QueuedTask) {
        match self.per_queue.get_mut(queued.queue_id.as_str()) {
            Some(count) => *count += 1,
            None => {
                self.per_queue.insert(queued.queue_id.clone(), 1);
            }
        }
        let url_bt = is_bt_url(&queued.url);
        self.bt_url_items += usize::from(url_bt);
        self.bt_items += usize::from(url_bt || !queued.torrent_file_bytes.is_empty());
    }

    fn sub(&mut self, queued: &QueuedTask) {
        match self.per_queue.get_mut(queued.queue_id.as_str()) {
            Some(count) if *count > 1 => *count -= 1,
            Some(_) => {
                self.per_queue.remove(queued.queue_id.as_str());
            }
            None => debug_assert!(false, "pending queue count underflow"),
        }
        let url_bt = is_bt_url(&queued.url);
        self.bt_url_items = self.bt_url_items.saturating_sub(usize::from(url_bt));
        self.bt_items = self
            .bt_items
            .saturating_sub(usize::from(url_bt || !queued.torrent_file_bytes.is_empty()));
    }
}

/// 排队等待启动的任务（FIFO）及增量维护的聚合计数。
///
/// 所有变更都经本类型的方法，入队 / 出队 / 删除 / 清空 / 队列迁移同步更新计数，
/// 因此「队列是否占用」「是否含 BT 项」「某队列排队项数」都是 O(队列数) 或 O(1)，
/// 不再随排队任务数（可达数十万）线性扫描。
#[derive(Default)]
struct PendingQueue {
    items: VecDeque<QueuedTask>,
    counts: PendingCounts,
}

impl PendingQueue {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn iter(&self) -> std::collections::vec_deque::Iter<'_, QueuedTask> {
        self.items.iter()
    }

    fn get(&self, index: usize) -> Option<&QueuedTask> {
        self.items.get(index)
    }

    fn push_back(&mut self, queued: QueuedTask) {
        self.counts.add(&queued);
        self.items.push_back(queued);
    }

    fn push_front(&mut self, queued: QueuedTask) {
        self.counts.add(&queued);
        self.items.push_front(queued);
    }

    /// 队首（仅测试：断言 FIFO 头部）。
    #[cfg(test)]
    fn front(&self) -> Option<&QueuedTask> {
        self.items.front()
    }

    /// 底层缓冲区切片（仅测试：断言满额 drain 不重建缓冲区）。
    #[cfg(test)]
    fn as_slices(&self) -> (&[QueuedTask], &[QueuedTask]) {
        self.items.as_slices()
    }

    /// 弹出队首（仅测试：模拟队列项被取走）。
    #[cfg(test)]
    fn pop_front(&mut self) -> Option<QueuedTask> {
        let queued = self.items.pop_front()?;
        self.counts.sub(&queued);
        Some(queued)
    }

    fn remove(&mut self, index: usize) -> Option<QueuedTask> {
        let queued = self.items.remove(index)?;
        self.counts.sub(&queued);
        Some(queued)
    }

    fn retain(&mut self, mut keep: impl FnMut(&QueuedTask) -> bool) {
        let counts = &mut self.counts;
        self.items.retain(|queued| {
            let kept = keep(queued);
            if !kept {
                counts.sub(queued);
            }
            kept
        });
    }

    fn clear(&mut self) {
        self.items.clear();
        self.counts = PendingCounts::default();
    }

    /// 有排队项的队列 ID（O(队列数)）。
    fn queue_ids(&self) -> impl Iterator<Item = &String> {
        self.counts.per_queue.keys()
    }

    /// 队列里是否含 BT 项（磁力 / .torrent 链接或携带 .torrent 字节）。
    fn has_bt(&self) -> bool {
        self.counts.bt_items > 0
    }

    /// 队列里是否含磁力 / .torrent 链接项（仅按 URL 判定）。
    fn has_bt_url(&self) -> bool {
        self.counts.bt_url_items > 0
    }

    /// 属于 `queue_id` 的排队项数。
    fn queued_in(&self, queue_id: &str) -> usize {
        self.counts.per_queue.get(queue_id).copied().unwrap_or(0)
    }

    /// 整体摘出 BT 项（保持相对顺序），其余项保留原顺序。
    fn drain_bt(&mut self) -> Vec<QueuedTask> {
        let mut taken = Vec::with_capacity(self.counts.bt_items);
        let mut rest = VecDeque::with_capacity(self.items.len() - self.counts.bt_items);
        for queued in std::mem::take(&mut self.items) {
            if is_bt_url(&queued.url) || !queued.torrent_file_bytes.is_empty() {
                self.counts.sub(&queued);
                taken.push(queued);
            } else {
                rest.push_back(queued);
            }
        }
        self.items = rest;
        taken
    }

    /// 把仍在排队的 `task_id` 迁到 `queue_id`；命中返回 `true`（`task_id` 在队列中唯一）。
    fn set_task_queue(&mut self, task_id: &str, queue_id: &str) -> bool {
        let Some(index) = self.items.iter().position(|q| q.task_id == task_id) else {
            return false;
        };
        let Some(entry) = self.items.get_mut(index) else {
            return false;
        };
        if entry.queue_id != queue_id {
            self.counts.sub(entry);
            entry.queue_id = queue_id.to_owned();
            self.counts.add(entry);
        }
        true
    }

    /// 把属于 `from` 的全部排队项改归 `to`（删除队列时回落主队列）。
    fn reassign_queue(&mut self, from: &str, to: &str) {
        if from == to || self.queued_in(from) == 0 {
            return;
        }
        for entry in &mut self.items {
            if entry.queue_id == from {
                self.counts.sub(entry);
                entry.queue_id = to.to_owned();
                self.counts.add(entry);
            }
        }
    }

    /// 全量重算计数（仅测试：与增量计数对账）。
    #[cfg(test)]
    fn counts_match_recount(&self) -> bool {
        let mut recount = PendingCounts::default();
        for queued in &self.items {
            recount.add(queued);
        }
        recount.per_queue == self.counts.per_queue
            && recount.bt_items == self.counts.bt_items
            && recount.bt_url_items == self.counts.bt_url_items
    }
}

/// All state associated with a single actively-running download task.
///
/// Consolidates the five parallel maps that previously tracked per-task state
/// (`active_tokens`, `active_handles`, `bt_task_ids`, `hls_quality_senders`,
/// `active_task_queue`) into one place so every insert/remove is atomic.
struct ActiveTaskEntry {
    /// Cancellation token — call `.cancel()` to request graceful shutdown.
    token: CancellationToken,
    /// Monotonic spawn generation — used to ignore stale `TaskDone` signals.
    generation: u64,
    /// JoinHandle for the spawned tokio task.  `None` until the task is
    /// spawned (the field is filled in at the very end of `do_start_task` /
    /// `do_resume_task` after the `tokio::spawn` call).
    handle: Option<JoinHandle<()>>,
    /// `true` when this is a BitTorrent download (magnet / .torrent).
    /// Used to exclude BT tasks from the HTTP/FTP concurrency counter.
    is_bt: bool,
    /// Named queue this task belongs to (empty string = default queue).
    /// Used for per-queue concurrency counting.
    queue_id: String,
}
/// A task that has been asked to pause but whose spawned downloader has not
/// finished flushing its buffered bytes and final progress yet.
///
/// Resume requests are latched here so a new generation cannot open the same
/// temporary file until the cancelled generation has fully stopped.
struct PendingPause {
    generation: u64,
    notify: bool,
    resume_requested: bool,
}

/// off-actor resolve 的种类（start 或 resume 侧再入）。
#[cfg(feature = "plugins")]
#[derive(Debug, Clone, Copy)]
pub enum ResolveKind {
    Start,
    Resume,
}

/// 起跑前（off-actor resolve worker）是否要为任务重新选择 resolver 插件。
///
/// 绑定为空串的任务（`忽略插件重试` 逃生舱的结果）不会走到这里：无绑定即不 resolve，
/// 也就绝不会被自动重新绑定。
#[cfg(feature = "plugins")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolverReselect {
    /// 沿用已绑定插件。
    No,
    /// 多插件冲突待选（`resolver_choice_pending`）：无条件定案。
    Pending,
    /// 从未起跑（无落盘进度、无 `resolver_item`）的任务：若已绑定插件不再命中
    /// （被禁用 / 待授权 / 熔断 / 规则不再匹配）而其它插件命中，则按当前路由重新选择；
    /// 无其它候选时仍 fail-closed。有进度的任务不属此类——中途换 resolver 可能写坏文件。
    IfSuperseded,
}

/// off-actor resolve 的回流结果。worker 无条件发送（含 panic 归一），交
/// `on_resolve_ready` 兜底，杜绝 pending_resolve/active_tasks 泄漏。
///
/// `generation` = 发起本次 resolve 时占位 `ActiveTaskEntry` 的 generation。回流时与
/// 当前 pending 条目的世代比对：不一致即 stale（resolve 窗口内发生过
/// pause/cancel/resume，本 outcome 已被新世代取代），一律丢弃。
#[cfg(feature = "plugins")]
pub struct ResolveOutcome {
    pub task_id: String,
    pub identity: String,
    pub kind: ResolveKind,
    pub generation: u64,
    pub result: Result<Option<crate::plugin::ResolveResult>, crate::plugin::PluginError>,
    /// 用户关闭变体 / 插件选择框 → 以该原因取消任务（而非回退默认项）。
    pub cancelled: Option<crate::plugin::CancelReason>,
    /// 解析的源链接（出口计划的匹配对象）。
    pub subject_url: String,
    /// 出口计划执行结果：换过路的路径与产出结果的一步。
    pub egress: crate::plugin::call_egress::EgressOutcome,
}

/// 解析成功后交给起跑的出口（见 `DownloadManager::resolved_egress`）。
#[cfg(feature = "plugins")]
#[derive(Debug, Clone)]
struct ResolvedEgress {
    /// 下载按它匹配站点规则 / 做 Auto 决策（解析产物再由 `rule_route::apply` 重匹配）。
    subject_url: String,
    /// Auto 计划下解析所用的路径：下载只走这一条。
    pin: Option<ForcedRoute>,
}

/// resolve 等待中的任务状态。Start 携 `QueuedTask`（再入覆盖 res 后分派）；
/// Resume 为标记（do_resume_task 从 DB 重读）。`generation` 与占位/outcome 同源，
/// 用于识别并丢弃被取代的 stale outcome（见 [`ResolveOutcome`]）。
#[cfg(feature = "plugins")]
enum PendingResolve {
    Start {
        queued: Box<QueuedTask>,
        generation: u64,
    },
    Resume {
        generation: u64,
    },
}

#[cfg(feature = "plugins")]
impl PendingResolve {
    fn generation(&self) -> u64 {
        match self {
            PendingResolve::Start { generation, .. } | PendingResolve::Resume { generation } => {
                *generation
            }
        }
    }
}

/// 多变体收敛（resolve worker 内、off-actor）：经 `HostSelection` 让用户选择，
/// 选中变体的非空字段覆盖顶层字段。单变体跳过弹框；超时/免打扰/headless 回退
/// `default_variant_index`（越界按 0）。
#[cfg(feature = "plugins")]
async fn collapse_resolve_variants(
    task_id: &str,
    res: &mut crate::plugin::ResolveResult,
    selector: &dyn HostSelection,
) -> bool {
    const VARIANT_SELECTION_TIMEOUT_SECS: u64 = 60;
    let variants = std::mem::take(&mut res.variants);
    let default_idx = if (res.default_variant_index as usize) < variants.len() {
        res.default_variant_index
    } else {
        0
    };
    let idx = if variants.len() <= 1 {
        0
    } else {
        let options: Vec<crate::model::ResolveVariantOption> = variants
            .iter()
            .enumerate()
            .map(|(i, v)| crate::model::ResolveVariantOption {
                index: i as i32,
                label: v.label.clone(),
                container: v.container.clone(),
                bandwidth: v.bandwidth,
                width: v.width,
                height: v.height,
                total_bytes: v.total_bytes.unwrap_or(0),
            })
            .collect();
        let outcome = selector
            .select_resolve_variant(
                task_id,
                &options,
                default_idx,
                std::time::Duration::from_secs(VARIANT_SELECTION_TIMEOUT_SECS),
            )
            .await;
        log_info!(
            "[plugin-resolve] task {} variant selection outcome: {:?}",
            task_id,
            outcome
        );
        let chosen = outcome.into_inner();
        // -1 = 用户在弹窗点关闭/取消 → 取消任务（不收敛、不回退默认）。
        if chosen < 0 {
            return true;
        }
        if (chosen as usize) < variants.len() {
            chosen
        } else {
            default_idx
        }
    };
    apply_chosen_variant(res, variants, idx);
    false
}

/// 把选中变体的非空字段覆盖到顶层（url/audioUrl/fileName/totalBytes）。
/// [`collapse_resolve_variants`]（用户选择）与
/// [`collapse_resolve_variants_silent`]（二段静默收敛）共用。
#[cfg(feature = "plugins")]
fn apply_chosen_variant(
    res: &mut crate::plugin::ResolveResult,
    variants: Vec<crate::plugin::ResolveVariant>,
    idx: i32,
) {
    if let Some(v) = variants.into_iter().nth(idx.max(0) as usize) {
        res.url = v.url;
        if v.audio_url.is_some() {
            res.audio_url = v.audio_url;
        }
        if v.file_name.is_some() {
            res.file_name = v.file_name;
        }
        if v.total_bytes.is_some() {
            res.total_bytes = v.total_bytes;
        }
    }
}

/// 二段解析（[`crate::plugin::ResolveRequest::resolver_item`] 非空）场景的
/// 静默变体收敛：不经 [`HostSelection`]，直接取 `default_variant_index`
/// （越界按 0）——绝不为 N 个裂变子任务弹 N 个选择框（A1 契约）。
#[cfg(feature = "plugins")]
fn collapse_resolve_variants_silent(res: &mut crate::plugin::ResolveResult) {
    let variants = std::mem::take(&mut res.variants);
    let idx = if (res.default_variant_index as usize) < variants.len() {
        res.default_variant_index
    } else {
        0
    };
    apply_chosen_variant(res, variants, idx);
}

/// 清单裂变（外部无 UI 入口自动展开，D6）自动启动的总大小阈值：Σsize 超过
/// 此值时全员（含母任务）静默转 paused，不自动占用带宽/磁盘。未知大小的
/// 条目按 0 计（保守放行，记录在案，见设计文档 §6.3/§7.8）。
#[cfg(feature = "plugins")]
const FISSION_AUTO_START_MAX_TOTAL_BYTES: i64 = 10 * 1024 * 1024 * 1024;

/// 清单 item 的解析标识拼接：无规格 = `<itemId>`；有规格 = `<itemId>@<variantId>`
/// （引擎自动取首个规格——初段清单无 default 语义）。两个 id 均为插件自定义
/// token，引擎本身不解释语义（D5 契约）。
#[cfg(feature = "plugins")]
fn manifest_item_resolver_token(item: &crate::plugin::ManifestItem) -> String {
    match item.variants.first() {
        Some(v) => format!("{}@{}", item.id, v.id),
        None => item.id.clone(),
    }
}

/// 清单 item 的相对子目录拼进 `base`（`rel_path` 空 = 根，落盘目标不变）。
#[cfg(feature = "plugins")]
fn join_manifest_path(base: &str, rel_path: &str) -> String {
    if rel_path.is_empty() {
        base.to_string()
    } else {
        PathBuf::from(base)
            .join(rel_path)
            .to_string_lossy()
            .into_owned()
    }
}

/// 把插件清单条目转换为宿主可展示的 [`crate::model::ManifestItemInfo`]
/// （[`DownloadManager::begin_resolve_preview`] 用）。
#[cfg(feature = "plugins")]
fn manifest_item_to_info(item: crate::plugin::ManifestItem) -> crate::model::ManifestItemInfo {
    crate::model::ManifestItemInfo {
        id: item.id,
        name: item.name,
        path: item.path,
        size: item.size.unwrap_or(0).max(0),
        variants: item
            .variants
            .into_iter()
            .map(|v| crate::model::ManifestVariantInfo {
                id: v.id,
                label: v.label,
                size: v.size.unwrap_or(0).max(0),
            })
            .collect(),
    }
}

/// 把 resolve 结果应用到 QueuedTask（再入前）。非 ephemeral 保持
/// `hint_file_size=0`，正常 probe 取得供 resume 后验校验的 validator；
/// ephemeral 走 skip-probe hint 路径。
#[cfg(feature = "plugins")]
fn apply_resolve_to_queued(queued: &mut QueuedTask, res: crate::plugin::ResolveResult) {
    if !res.url.is_empty() {
        queued.url = res.url;
    }
    if let Some(name) = res.file_name
        && !name.is_empty()
    {
        queued.file_name = name;
    }
    if res.extra_headers.is_some() || res.request.is_some() {
        let patch = crate::intercept::RequestPatch {
            replace_headers: res.extra_headers,
            ..res.request.unwrap_or_default()
        };
        queued.resolver_patch = Some((queued.resolver_plugin_id.clone(), patch));
    }
    // 显式空串 = 本次结果没有音轨（插件按档位切换到「仅音频」或单文件时以此清掉
    // 上一次的音轨），不能让 `Some("")` 把任务当成轨对交给 DASH 下载器。
    if let Some(audio) = res.audio_url {
        queued.audio_url = (!audio.is_empty()).then_some(audio);
    }
    // ephemeral（一次性/签名直链）→ 跳过 probe：知大小走 hint，未知走 -1；
    // 否则正常 probe。
    queued.hint_file_size = if res.ephemeral {
        match res.total_bytes {
            Some(n) if n > 0 => n,
            _ => -1,
        }
    } else {
        0
    };
    queued.range_supported = res.range_supported;
}
pub struct DownloadManager {
    db: Db,
    client: Client,
    /// Current proxy configuration — used to rebuild Client on config change.
    proxy_config: ProxyConfig,
    /// All state for every actively-running download, keyed by task_id.
    /// Replaces the five separate maps that previously tracked the same set:
    ///   • active_tokens   (CancellationToken + generation)
    ///   • active_handles  (JoinHandle)
    ///   • bt_task_ids     (HashSet membership flag)
    ///   • active_task_queue   (queue_id string)
    active_tasks: HashMap<String, ActiveTaskEntry>,
    /// Cancelled generations still flushing their final on-disk progress.
    ///
    /// The entry is removed only by the matching [`TaskDone`]. This keeps a
    /// rapid pause→resume from overlapping two writers for the same temp file
    /// and lets the old generation publish one authoritative paused frame.
    pending_pauses: HashMap<String, PendingPause>,
    /// 启动序幕正在等待「文件已存在」答复的任务（task_id → spawn generation）。
    /// 这些任务仍留在 `active_tasks`（暂停/删除/恢复守卫沿用同一套世代与令牌），
    /// 但不占并发槽，DB 状态保持 0（待处理）。
    awaiting_decision: HashMap<String, u64>,
    /// Monotonically increasing counter to distinguish different spawns of
    /// the same task_id.  Prevents a stale `TaskDone` from an old spawn
    /// from accidentally removing the token of a newer spawn.
    generation: u64,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    progress_rx: Option<mpsc::Receiver<ProgressUpdate>>,
    done_tx: mpsc::Sender<TaskDone>,
    done_rx: Option<mpsc::Receiver<TaskDone>>,
    /// Maximum number of concurrent active HTTP/FTP downloads.  0 = unlimited.
    /// BT tasks are excluded from this limit because each BT download is
    /// inherently multi-peer concurrent and managed by the shared librqbit
    /// session (which has its own `concurrent_init_limit`).
    max_concurrent: usize,
    /// FIFO queue of tasks waiting for a free slot (HTTP/FTP only — BT tasks
    /// bypass the queue entirely).
    pending_queue: PendingQueue,
    /// 高频创建/完成只标脏；宿主节拍合并发布队列顺序差异。
    queue_positions_dirty: bool,
    /// 上次已发布的排队顺序键表；发布时与 `pending_queue` 比对，只发差异。
    queue_order: QueueOrderPublisher,
    /// Global speed limiter shared with all HTTP/FTP download tasks.
    speed_limiter: SpeedLimiter,
    /// 全局 BT 上传限速（B/s，0 = 不限）。会话已存在时热同步到
    /// `SharedBtSession`；会话惰性创建时作为初始值传入。
    upload_limit_bps: u64,
    /// Shared BT session — lazily initialised on first BT download.
    /// All BT tasks share a single `librqbit::Session` (DHT, trackers,
    /// listening port, speed limits) to avoid per-task resource waste.
    /// Wrapped in `Arc` so spawned download tasks can cache handles.
    #[cfg(feature = "bt")]
    bt_session: Option<Arc<SharedBtSession>>,
    /// BT 防吸血宿主：策略对象在会话创建时注入 librqbit，配置热更新不重建会话。
    #[cfg(feature = "bt")]
    bt_guard: Arc<crate::bt_guard::BtGuardHost>,
    /// Default save directory used to initialise the BT session.
    default_save_dir: String,
    /// Application data directory (exe dir) for BT persistence files.
    #[cfg_attr(not(feature = "bt"), allow(dead_code))]
    app_data_dir: String,
    /// 解析后的引擎数据目录（组件 bin/ 探测用；由 `Engine::new` 注入）。
    data_dir: std::path::PathBuf,
    /// User-configurable BT settings (DHT, UPnP, ports, custom trackers).
    bt_config: BtConfig,
    /// Globally configured user-agent string. Empty = use built-in Chrome UA.
    global_user_agent: String,
    /// Global default segment count from settings. 0 = defer to segment_advisor.
    global_default_segments: i32,
    /// Auto 模式最大连接数上限（config `auto_max_connections`）。
    /// <=0 = 不限（罕见，仅显式配置），默认 [`DEFAULT_AUTO_MAX_CONNECTIONS`]。
    auto_max_connections: i32,
    /// 下载完成后是否把文件修改时间设为服务器提供的原始时间（HTTP
    /// `Last-Modified` / FTP `MDTM`；config `use_server_time`，默认关闭）。
    use_server_time: bool,
    /// 文件已存在时的处理策略（config `file_exists_behavior`，默认
    /// `Rename` = 自动重命名）。`Overwrite`/`Skip` 都只在重名冲突**只**
    /// 来自磁盘上已存在的最终文件时才生效；`.fdownloading` 临时文件与
    /// 并发任务的预订名仍按编号改名，绝不覆盖在途产物。
    file_exists_behavior: FileExistsBehavior,
    /// 多 CDN 节点并发下载全局开关（config `cdn_multi_enabled`，默认关，
    /// 实验性）。任务级还需通过 §3.2 前置条件（https/无代理/Range 已验证/
    /// 域名未学习为单连接等）才会真正聚合。
    cdn_multi_enabled: bool,
    /// 单任务最多钉定的 CDN 节点数（config `cdn_max_nodes`，0..=8；
    /// **0 = 自动**：按文件大小与并发连接数推导，默认值；SYS 兜底节点
    /// 不计入）。
    cdn_max_nodes: i32,
    /// 多网卡聚合下载全局开关（config `multi_nic_enabled`，默认关）。任务级
    /// 还需直连（无代理、无 Auto 多路径）、支持分段，且规划器在本机找到通向
    /// 不同上游的额外网卡才会真正聚合，见 [`crate::multi_nic`]。
    multi_nic_enabled: bool,
    /// In-memory cache of named queue settings (queue_id → QueueInfo).
    /// Kept in sync with the DB on every queue CRUD operation.
    queues: HashMap<String, QueueInfo>,
    /// Per-queue speed limiters (queue_id → SpeedLimiter).
    /// Created on demand for queues that have speed_limit_kbps > 0.
    queue_limiters: HashMap<String, SpeedLimiter>,
    /// 定时调度的边沿触发账本：(queue_id, 是否启动边沿) → 最近处理日期。
    /// 保证每个定时边沿每天至多触发一次（手动启停后同一天不再重复触发）；
    /// 内存级，重启清零——重启后当天已越过的边沿会补触发一次（见
    /// `tick_queue_schedules` 的当日补触发语义）。
    schedule_fired: HashMap<ScheduleEdge, chrono::NaiveDate>,
    /// 是否已完成启动时的 reset_incomplete_tasks_to_paused 矫正。
    /// 该矫正仅需在第一次 load_and_send_all_tasks 时执行一次，
    /// 后续由 create_task / batch_create 触发时不得重复重置。
    startup_reset_done: bool,
    /// 批量建组/清单裂变期间为 true：抑制 create_task 的逐任务 TaskProgress
    /// 与 enqueue_persisted_task 的逐次全量队列位置广播，由批量操作尾部的
    /// 一次 TasksSnapshot + 一次 QueuePositionsChanged 统一覆盖。
    suppress_bulk_broadcasts: bool,
    /// 文件跟踪扫描是否正在进行（防重叠）。内存级；`Arc` 以便 detached 扫描
    /// task 与调用方共享同一标志。
    scanning: Arc<AtomicBool>,
    /// 任务的文件被删除/移动时是否自动删除任务记录（config
    /// `file_missing_action` == `"delete"`，默认 false = 仅标记 `file_missing`
    /// 并保留任务记录）。开启时文件跟踪扫描把新判定为丢失的 task_id 经
    /// `missing_cleanup_tx` 回流给宿主 actor 执行删除。
    missing_file_auto_delete: bool,
    /// 文件丢失自动清理回流通道发送端：detached 扫描把本轮新判定为丢失的
    /// task_id 批次投进来，actor loop 收到后调用 `delete_tasks_batch`。
    missing_cleanup_tx: mpsc::Sender<Vec<String>>,
    /// 文件丢失自动清理回流接收端（仅取一次，交给 actor loop）。
    missing_cleanup_rx: Option<mpsc::Receiver<Vec<String>>>,
    /// 空闲（无活跃/排队任务）时是否仍执行周期性文件跟踪扫描（config
    /// `idle_file_scan`，默认开启）。关闭后，完全空闲期跳过定时扫描，
    /// 避免不必要地唤醒 NAS/网络盘；窗口聚焦、手动重扫等主动触发路径
    /// 不受此项影响，恒会执行。
    idle_file_scan: bool,
    /// 已完成任务记录保留天数（config `completed_record_retention_days`，
    /// 0 = 永不清理）。超期的「已完成」记录（只删记录不删文件，做种中跳过）
    /// 由 [`DownloadManager::purge_expired_completed_records`] 在被动事件上顺带清理，
    /// 无独立定时器（空闲静默契约）。
    completed_record_retention_days: u32,
    /// 上次执行保留期清理的单调时刻，用于去抖（见 `RETENTION_PURGE_DEBOUNCE`）。
    last_retention_purge: Option<std::time::Instant>,
    /// Boost 模式当前优先任务 ID。None = 无优先任务。变化时持久化到 config
    /// `boost_state`，重启后由 `resume_on_start` 处理（见 `boost.rs`）。
    priority_task_id: Option<String>,
    /// 因 Boost 模式自动暂停的任务 ID，按暂停/恢复优先级有序、去重。
    /// 取消 Boost 时这些任务按此顺序自动恢复。
    auto_paused_ids: Vec<String>,
    /// 任务级自动重试：网络 stall / 瞬时错误导致任务失败后，延迟自动恢复。
    /// key = task_id，value = 已自动重试次数。
    /// 超过 `max_auto_retries` 后不再重试，保持 error 状态等用户手动恢复。
    auto_retry_counts: HashMap<String, u32>,
    /// 拦截器 failed 阶段插件重试的独立额度账本（与 `auto_retry_counts` 互不影响）。
    #[cfg(feature = "plugins")]
    intercept_retry_counts: HashMap<String, u32>,
    /// 由 failed 阶段发出、尚未被 `plugin_request_retry` 取走的重试标记。
    #[cfg(feature = "plugins")]
    intercept_retry_marks: Arc<std::sync::Mutex<HashSet<String>>>,
    /// 上次落库做种时长的时刻（单调时钟）。`account_seeding_tick` 据此判定兜底
    /// 落库是否到点，见 [`seed_times_persist_due`]。
    #[cfg(feature = "bt")]
    last_seed_time_persist: Mutex<std::time::Instant>,
    /// 活动做种者的任务行事实缓存，供 [`Self::tick_seeding_upload`] 高频发帧
    /// 而不读库；求值 tick 写库后整体刷新，采样时剔除已不在做种的项。
    #[cfg(feature = "bt")]
    seed_row_totals: Mutex<HashMap<String, SeedRowTotals>>,
    /// BT 会话进入「仅因暂停中的未完成任务而保活」空闲期的起点（内存级）。
    /// `None` = 不在该空闲期。超过 [`BT_SESSION_PAUSED_IDLE_GRACE`] 后释放会话，
    /// 见 [`decide_bt_session_release`]。
    #[cfg(feature = "bt")]
    bt_paused_idle_since: Option<std::time::Instant>,
    /// 已完成 BT 任务的在途完整性校验（删除任务 / 关停时取消）。
    #[cfg(feature = "bt")]
    integrity_checks: crate::bt_verify::IntegrityChecks,
    /// System 代理模式下按系统代理指纹缓存的全局 client（见 `global_client`）。
    system_client: std::sync::Mutex<Option<(String, Client)>>,
    /// 后台元数据探测的并发闸门（见 [`META_PROBE_CONCURRENCY`]）。
    probe_gate: Arc<Semaphore>,
    /// 已发起且未结束的元数据探测数（含排闸门等待者）；「稍后下载」没有队列位置，
    /// 以此给批量导入的探测设窗口上限（见 [`META_PROBE_WINDOW`]）。
    probes_outstanding: Arc<AtomicUsize>,
    /// 用户可配的最大自动重试次数（config `max_auto_retries`）。
    /// `-1` = 无限重试，`0` = 关闭，`1..=10` = 次数上限。
    max_auto_retries: i32,
    /// 用户可配的自动重试基础延迟（秒，config `auto_retry_delay_secs`）。
    /// 实际延迟 = base × attempt（递增）。`0` 表示无延迟立即重试。
    auto_retry_delay_secs: u64,
    /// 延迟重试通道发送端。on_task_done 检测到可重试错误后，spawn 一个
    /// 延迟任务将 task_id 发送到此通道，actor loop 收到后调用 resume_task。
    retry_tx: mpsc::Sender<String>,
    /// 延迟重试通道接收端（仅取一次，交给 actor loop）。
    retry_rx: Option<mpsc::Receiver<String>>,
    /// 已排程的一次性备用链路目标。resume 消费后写入对应 failover 路由标签。
    auto_failover_pending: HashMap<String, AutoFailoverTarget>,
    /// 当前自动恢复周期已尝试过的三条链路。用户手动恢复/重下会清除，
    /// 自动重试不会清除；每条链路至多一次，防止坏链路无限 ping-pong。
    auto_failover_attempts: HashMap<String, AutoFailoverAttempts>,
    /// 当前正在下载（或排队准备启动）的任务已预订的临时文件路径集合。
    ///
    /// 用于解决 `dedup_filename` 的 TOCTOU 竞态：多个并发任务同时调用
    /// `dedup_filename` 时，都可能看到磁盘上同名文件不存在，进而选出相同
    /// 文件名并相互覆盖对方的 `.fdownloading` 临时文件，导致文件内容丢失。
    ///
    /// 修复策略：任务启动序幕（`finalize_start_file_name`，spawned task 内）
    /// 在互斥临界区里完成「dedup → 预订 insert」，并在 `on_task_done` /
    /// 删除路径移除。`dedup_filename_sync` 在锁内消费此集合，检查文件名
    /// 冲突时同时排除已被其他 in-flight 任务预订的路径，彻底消除批量
    /// 下载中的文件名竞态。
    ///
    /// 共享互斥：actor（删除路径的主动释放）与各下载任务（启动序幕的
    /// dedup+预订）都会触碰，锁内只做同步操作，绝无 `.await`。
    reserved_temp_paths: Arc<Mutex<HashSet<std::path::PathBuf>>>,
    /// 引擎事件接收端(进度/队列变化/分段拆分等)——由宿主注入。
    sink: Arc<dyn EventSink>,
    /// 需要宿主介入决策的选择接口(HLS 画质/BT 文件选择)——由宿主注入。
    selector: Arc<dyn HostSelection>,
    /// RSS 订阅子系统（轮询调度 + 条目状态机）。任务创建仍收敛在
    /// [`DownloadManager::create_task`]，`RssManager` 只产出建任务指令。
    pub rss: crate::rss::RssManager,
    /// 任务事件 Webhook 推送（免费自托管）。`emit` 同步返回、网络 IO 自 spawn，
    /// 因此可以直接在生命周期点位上调用，不影响 actor。
    webhook: Arc<crate::webhook::WebhookDispatcher>,
    /// 上一次判定时仍有活跃/待启动任务的队列（`queue.started` / `queue.drained`
    /// 边沿触发用）。
    occupied_queues: HashSet<String>,
    /// 进程启动恢复（`resume_on_start`）进行中：期间登记占用但不发 `queue.started`。
    restoring_on_start: bool,
    /// 已排程自动重试、尚未回流的任务 → 其队列 ID。判定队列是否清空时视为
    /// 仍占用，否则重试间隙会误报一次 `queue.drained`。
    retry_scheduled: HashMap<String, String>,
    /// 上一次向宿主上报的「等待重试」任务数（`retry_scheduled` 大小），仅在变化时再发事件。
    retry_pending_reported: u32,
    /// 插件管理器（Arc 共享）。`None` 直到 `install_plugin_manager` 注入。
    /// 仅 `plugins` feature 下存在；feature 关时无此字段、下载主链路零变化。
    #[cfg(feature = "plugins")]
    plugin_manager: Option<Arc<crate::plugin::PluginManager>>,
    /// 插件网络出口（与 `plugin_manager` 同时注入）：全局代理变更时随之重建。
    #[cfg(feature = "plugins")]
    plugin_bridge: Option<Arc<crate::plugin::bridge::EngineBridge>>,
    /// off-actor resolve 回流通道（worker → actor `on_resolve_ready`）。
    #[cfg(feature = "plugins")]
    resolve_tx: mpsc::UnboundedSender<ResolveOutcome>,
    #[cfg(feature = "plugins")]
    resolve_rx: Option<mpsc::UnboundedReceiver<ResolveOutcome>>,
    /// 插件 onError 命令式重试意图通道（bridge → actor `plugin_request_retry`）。
    #[cfg(feature = "plugins")]
    plugin_retry_tx: mpsc::UnboundedSender<(String, u64)>,
    #[cfg(feature = "plugins")]
    plugin_retry_rx: Option<mpsc::UnboundedReceiver<(String, u64)>>,
    /// resolve 等待中的任务（start 携 QueuedTask，resume 为标记）。
    #[cfg(feature = "plugins")]
    pending_resolve: HashMap<String, PendingResolve>,
    /// resume 侧再入的解析结果（on_resolve_ready → do_resume_task 传递，避免改签名）。
    #[cfg(feature = "plugins")]
    resume_applied: HashMap<String, crate::plugin::ResolveResult>,
    /// 插件解析刚成功的任务的出口（on_resolve_ready 写入、紧随其后的起跑消费）：下载以
    /// 源链接定出口，Auto 下钉定解析所用的那条路径。每次解析回流覆盖，绝不跨运行保留。
    #[cfg(feature = "plugins")]
    resolved_egress: HashMap<String, ResolvedEgress>,
}

/// Configuration parameters for [`DownloadManager::new`].
/// Grouping avoids the `clippy::too_many_arguments` limit and makes
/// call sites self-documenting.
pub struct DownloadManagerConfig {
    pub max_concurrent: usize,
    pub speed_limit_bps: u64,
    pub upload_limit_bps: u64,
    pub default_save_dir: String,
    pub app_data_dir: String,
    pub data_dir: std::path::PathBuf,
    pub bt_config: BtConfig,
    pub proxy_config: ProxyConfig,
    pub user_agent: String,
}
impl DownloadManager {
    pub fn new(
        db: Db,
        config: DownloadManagerConfig,
        sink: Arc<dyn EventSink>,
        selector: Arc<dyn HostSelection>,
    ) -> Result<Self, downloader::DownloadError> {
        let DownloadManagerConfig {
            max_concurrent,
            speed_limit_bps,
            upload_limit_bps,
            default_save_dir,
            app_data_dir,
            data_dir,
            bt_config,
            proxy_config,
            user_agent,
        } = config;
        // 库里残留的非法 UA 不得让整个引擎起不来：回退内置默认 UA 并记日志。
        let user_agent = if user_agent.is_empty()
            || reqwest::header::HeaderValue::from_str(&user_agent).is_ok()
        {
            user_agent
        } else {
            log_info!(
                "[manager] invalid global_user_agent in config ({:?}), falling back to default",
                user_agent
            );
            String::new()
        };
        let client = downloader::build_client(&proxy_config, &user_agent)?;
        let (tx, rx) = mpsc::channel(8192);
        // TaskDone 很小且每个下载 spawn 只发一次；actor 在批量删除/关停时同步
        // 等 JoinHandle，不会消费该通道，容量必须覆盖同时结束的活跃任务数
        // （BT 不受并发上限约束），否则 send 阻塞使 handle 永远不完成。
        let (done_tx, done_rx) = mpsc::channel(8192);
        let (retry_tx, retry_rx) = mpsc::channel(32);
        let (missing_cleanup_tx, missing_cleanup_rx) = mpsc::channel(8);
        #[cfg(feature = "plugins")]
        let (resolve_tx, resolve_rx) = mpsc::unbounded_channel();
        #[cfg(feature = "plugins")]
        let (plugin_retry_tx, plugin_retry_rx) = mpsc::unbounded_channel();
        let limiter = SpeedLimiter::new(speed_limit_bps);
        limiter.spawn_refill_task();
        // RSS 子系统与 manager 共享同一个 DB 池与事件出口（两者都只是句柄
        // 克隆，不额外开连接）。
        let (rss_db, rss_sink) = (db.clone(), sink.clone());
        let webhook = Arc::new(crate::webhook::WebhookDispatcher::new(&proxy_config));
        // 防吸血宿主：持有 guard、策略对象与事件泵（空闲时不占任何唤醒源）。
        #[cfg(feature = "bt")]
        let bt_guard = Arc::new(crate::bt_guard::BtGuardHost::new(
            Arc::new(crate::bt_guard::BtGuard::new(
                crate::bt_guard::BtGuardConfig::default(),
            )),
            db.clone(),
            sink.clone(),
            proxy_config.clone(),
        ));
        // 投递日志变化要能推给宿主，打开着的日志面板才会活着更新
        // （「模拟一次下载完成」按钮全靠这条推送给出反馈）。
        webhook.set_sink(sink.clone());
        Ok(Self {
            db,
            client,
            proxy_config,
            active_tasks: HashMap::new(),
            pending_pauses: HashMap::new(),
            awaiting_decision: HashMap::new(),
            generation: 0,
            progress_tx: tx,
            progress_rx: Some(rx),
            done_tx,
            done_rx: Some(done_rx),
            max_concurrent,
            pending_queue: PendingQueue::default(),
            queue_positions_dirty: false,
            queue_order: QueueOrderPublisher::default(),
            speed_limiter: limiter,
            upload_limit_bps,
            #[cfg(feature = "bt")]
            bt_session: None,
            #[cfg(feature = "bt")]
            bt_guard,
            default_save_dir,
            app_data_dir,
            data_dir,
            bt_config: bt_config.effective(),
            global_user_agent: user_agent,
            global_default_segments: 0,
            auto_max_connections: DEFAULT_AUTO_MAX_CONNECTIONS,
            use_server_time: false,
            file_exists_behavior: FileExistsBehavior::Rename,
            cdn_multi_enabled: false,
            multi_nic_enabled: false,
            cdn_max_nodes: 0, // 0 = 自动档
            queues: HashMap::new(),
            queue_limiters: HashMap::new(),
            schedule_fired: HashMap::new(),
            startup_reset_done: false,
            suppress_bulk_broadcasts: false,
            scanning: Arc::new(AtomicBool::new(false)),
            missing_file_auto_delete: false,
            missing_cleanup_tx,
            missing_cleanup_rx: Some(missing_cleanup_rx),
            idle_file_scan: true,
            completed_record_retention_days: 0,
            last_retention_purge: None,
            priority_task_id: None,
            auto_paused_ids: Vec::new(),
            auto_retry_counts: HashMap::new(),
            #[cfg(feature = "plugins")]
            intercept_retry_counts: HashMap::new(),
            #[cfg(feature = "plugins")]
            intercept_retry_marks: Arc::new(std::sync::Mutex::new(HashSet::new())),
            max_auto_retries: DEFAULT_MAX_TASK_AUTO_RETRIES,
            auto_retry_delay_secs: DEFAULT_AUTO_RETRY_BASE_DELAY_SECS,
            retry_tx,
            retry_rx: Some(retry_rx),
            auto_failover_pending: HashMap::new(),
            auto_failover_attempts: HashMap::new(),
            reserved_temp_paths: Arc::new(Mutex::new(HashSet::new())),
            sink,
            selector,
            rss: crate::rss::RssManager::new(rss_db, rss_sink),
            webhook,
            occupied_queues: HashSet::new(),
            restoring_on_start: false,
            retry_scheduled: HashMap::new(),
            retry_pending_reported: 0,
            #[cfg(feature = "bt")]
            last_seed_time_persist: Mutex::new(std::time::Instant::now()),
            #[cfg(feature = "bt")]
            seed_row_totals: Mutex::new(HashMap::new()),
            #[cfg(feature = "bt")]
            bt_paused_idle_since: None,
            #[cfg(feature = "bt")]
            integrity_checks: crate::bt_verify::IntegrityChecks::default(),
            system_client: std::sync::Mutex::new(None),
            probe_gate: Arc::new(Semaphore::new(META_PROBE_CONCURRENCY)),
            probes_outstanding: Arc::new(AtomicUsize::new(0)),
            #[cfg(feature = "plugins")]
            plugin_manager: None,
            #[cfg(feature = "plugins")]
            plugin_bridge: None,
            #[cfg(feature = "plugins")]
            resolve_tx,
            #[cfg(feature = "plugins")]
            resolve_rx: Some(resolve_rx),
            #[cfg(feature = "plugins")]
            plugin_retry_tx,
            #[cfg(feature = "plugins")]
            plugin_retry_rx: Some(plugin_retry_rx),
            #[cfg(feature = "plugins")]
            pending_resolve: HashMap::new(),
            #[cfg(feature = "plugins")]
            resume_applied: HashMap::new(),
            #[cfg(feature = "plugins")]
            resolved_egress: HashMap::new(),
        })
    }

    pub fn take_progress_rx(&mut self) -> Option<mpsc::Receiver<ProgressUpdate>> {
        self.progress_rx.take()
    }

    /// Take the receiver for task-done notifications.
    /// The actor loop should select on this to clean up `active_tokens`.
    pub fn take_done_rx(&mut self) -> Option<mpsc::Receiver<TaskDone>> {
        self.done_rx.take()
    }

    /// Take the receiver for delayed auto-retry notifications.
    /// The actor loop should select on this to resume stalled tasks.
    pub fn take_retry_rx(&mut self) -> Option<mpsc::Receiver<String>> {
        self.retry_rx.take()
    }

    /// Take the receiver for file-missing auto-cleanup batches.
    /// The actor loop should select on this and delete the reported task
    /// records (config `file_missing_action` == `"delete"`). 宿主不取时
    /// 通道会被填满，扫描侧的 `try_send` 静默失败——行为安全。
    pub fn take_missing_cleanup_rx(&mut self) -> Option<mpsc::Receiver<Vec<String>>> {
        self.missing_cleanup_rx.take()
    }

    // ===================================================================
    // 插件系统（off-actor 惰性 resolve / 通知 / 命令式重试）
    // 仅 `plugins` feature 下编译；feature 关时下载主链路零变化。
    // ===================================================================

    /// 注入插件管理器与其网络出口（Engine::new 构造后调用）。
    #[cfg(feature = "plugins")]
    pub fn install_plugin_manager(
        &mut self,
        pm: Arc<crate::plugin::PluginManager>,
        bridge: Arc<crate::plugin::bridge::EngineBridge>,
    ) {
        pm.attach_egress_source(Arc::clone(&bridge));
        self.plugin_manager = Some(pm);
        self.plugin_bridge = Some(bridge);
    }

    /// 获取插件管理器（供宿主管理接口读操作 + 集成测试）。
    #[cfg(feature = "plugins")]
    pub fn plugin_manager(&self) -> Option<Arc<crate::plugin::PluginManager>> {
        self.plugin_manager.clone()
    }

    /// 构造市场客户端；每次读取市场源，沿用当前已应用的代理配置。
    #[cfg(feature = "plugins")]
    pub async fn market_client(
        &self,
    ) -> Result<Option<crate::plugin::MarketClient>, crate::plugin::MarketError> {
        let Some(pm) = self.plugin_manager.clone() else {
            return Ok(None);
        };
        let all = self.db.get_all_config().await.map_err(|error| {
            crate::plugin::PluginError::Runtime(format!("读取市场配置失败: {error:#}"))
        })?;
        let sources = crate::plugin::MarketClient::source_config(&all);
        crate::plugin::MarketClient::new(pm, self.db.clone(), sources, &self.proxy_config).map(Some)
    }

    /// 暴露 plugin_retry_tx 供 bridge 构造（onError 命令式重试意图通道）。
    #[cfg(feature = "plugins")]
    pub fn plugin_retry_sender(&self) -> mpsc::UnboundedSender<(String, u64)> {
        self.plugin_retry_tx.clone()
    }

    /// 交出 resolve 回流接收端给 actor loop。
    #[cfg(feature = "plugins")]
    pub fn take_resolve_rx(&mut self) -> Option<mpsc::UnboundedReceiver<ResolveOutcome>> {
        self.resolve_rx.take()
    }

    /// 交出 plugin_retry 接收端给 actor loop。
    #[cfg(feature = "plugins")]
    pub fn take_plugin_retry_rx(&mut self) -> Option<mpsc::UnboundedReceiver<(String, u64)>> {
        self.plugin_retry_rx.take()
    }

    /// 建任务时的 resolver 绑定：`(插件 ID, 是否待多插件选择, 是否 fail-closed 建档)`。
    ///
    /// - `explicit` 非空（任务组成员：预览时产出清单的插件）：**无论该插件当前是否可用都绑定
    ///   它**，绝不回退到其它插件。不可用（未启用 / 待授权 / 已卸载 / 无 resolver）时第三项为
    ///   `true`：任务带着绑定以 status=4 落库，等插件恢复后手动恢复即经该插件正常解析，
    ///   期间恢复则由 resolve 的常规错误 fail-closed；
    /// - 其余情形见 [`Self::plugin_auto_resolver`]。
    #[cfg(feature = "plugins")]
    async fn plugin_bind_resolver(
        &self,
        url: &str,
        explicit: &str,
        group_id: &str,
        resolver_item: &str,
    ) -> (String, bool, bool) {
        let Some(pm) = &self.plugin_manager else {
            return (String::new(), false, false);
        };
        if !explicit.is_empty() {
            let available = pm.resolver_available(explicit).await;
            if !available {
                log_info!(
                    "[manager] group resolver {explicit} is not available, members fail closed"
                );
            }
            return (explicit.to_owned(), false, !available);
        }
        let group_member = !group_id.is_empty() && !resolver_item.is_empty();
        let (identity, pending) = self.plugin_auto_resolver(url, group_member).await;
        (identity, pending, false)
    }
    #[cfg(not(feature = "plugins"))]
    async fn plugin_bind_resolver(
        &self,
        _url: &str,
        _explicit: &str,
        _group_id: &str,
        _resolver_item: &str,
    ) -> (String, bool, bool) {
        (String::new(), false, false)
    }

    /// 没有显式绑定时的自动路由：任务组成员按预览同款规则 `match_multi_resolver`（保证
    /// 成员与清单同源），其余任务（含 RSS 二段解析条目）按 `route_resolver`。无命中为空串。
    #[cfg(feature = "plugins")]
    async fn plugin_auto_resolver(&self, url: &str, group_member: bool) -> (String, bool) {
        let Some(pm) = &self.plugin_manager else {
            return (String::new(), false);
        };
        if group_member {
            return (
                pm.match_multi_resolver(url).await.unwrap_or_default(),
                false,
            );
        }
        pm.route_resolver(url).await.map_or_else(
            || (String::new(), false),
            |route| (route.identity, route.choice_pending),
        )
    }

    /// 二段解析任务（`resolver_item` 非空）的绑定为空（旧版 fail-closed 建档、卸载清绑定、
    /// 忽略插件重试遗留）：按创建时同一规则重新绑定并写库。返回绑定的插件；`None` = 没有
    /// 任何插件可用，调用方必须 fail-closed——二段任务的链接是分享页，绝不能直接下载。
    #[cfg(feature = "plugins")]
    async fn rebind_second_stage(&self, task_id: &str) -> Option<String> {
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(task)) => task,
            Ok(None) => return None,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load task for second-stage rebind",
                    &error,
                );
                return None;
            }
        };
        let (identity, _) = self
            .plugin_auto_resolver(&task.url, !task.group_id.is_empty())
            .await;
        if identity.is_empty() {
            return None;
        }
        if let Err(error) = self.db.set_task_resolver(task_id, &identity).await {
            crate::logger::report_error("download-manager", "persist second-stage rebind", &error);
            return None;
        }
        log_info!("[manager] second-stage task {task_id} rebound to {identity}");
        Some(identity)
    }

    /// 二段解析任务无可用 resolver：以 status=4 + 「解析插件不可用」收尾，不发起下载。
    #[cfg(feature = "plugins")]
    async fn fail_second_stage_unavailable(&self, task_id: &str) {
        log_info!("[manager] second-stage task {task_id} has no usable resolver, failing closed");
        persist_task_failure(
            task_id,
            RESOLVER_UNAVAILABLE_MESSAGE.to_string(),
            &self.db,
            &self.progress_tx,
        )
        .await;
    }

    /// 前置预解析（多文件清单，B2 契约）：off-actor、只读，不建任务、不写库。
    /// 薄封装：委托 [`Self::spawn_resolve_preview`] 拿到 oneshot receiver 后
    /// spawn 一个转发任务 await 结果并 emit [`EngineEvent::ResolvePreviewReady`]
    /// （语义与重构前一致；`plugins` feature 关闭时同样立即收到空 outcome，
    /// 宿主无需 `cfg` 分叉）。
    pub async fn begin_resolve_preview(
        &self,
        preview_id: String,
        url: String,
        cookies: String,
        referrer: String,
        user_agent: String,
        extra_headers: HashMap<String, String>,
    ) {
        let rx =
            self.spawn_resolve_preview(url.clone(), cookies, referrer, user_agent, extra_headers);
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let outcome = rx.await.unwrap_or_else(|_| {
                ResolvePreviewOutcome::failed("resolve preview worker dropped".to_string())
            });
            sink.emit(EngineEvent::ResolvePreviewReady {
                preview_id,
                name: outcome.name,
                source_url: url,
                items: outcome.items,
                error: outcome.error,
            });
        });
    }

    /// 前置预解析核心（B2 契约，可复用）：无 `plugin_manager` / 未命中
    /// `multi` resolver → 立即发出空 outcome（不跑 resolve，避免解析昂贵的
    /// 单文件插件白跑一次）；命中则把
    /// [`crate::plugin::PluginManager::match_multi_resolver`] 与初段
    /// `resolve`（`resolver_item` 恒为空，`task_id` 传空串——尚无任务）整段
    /// 放到插件专用 runtime 上执行。本方法（`&self`，同步）立即返回
    /// receiver，不阻塞调用方 actor。
    #[cfg(feature = "plugins")]
    pub fn spawn_resolve_preview(
        &self,
        url: String,
        cookies: String,
        referrer: String,
        user_agent: String,
        extra_headers: HashMap<String, String>,
    ) -> oneshot::Receiver<ResolvePreviewOutcome> {
        let (tx, rx) = oneshot::channel();
        let Some(pm) = self.plugin_manager.clone() else {
            if tx.send(ResolvePreviewOutcome::empty()).is_err() {
                // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                tracing::debug!("download-manager notification receiver closed");
            }
            return rx;
        };
        let handle = pm.runtime_handle();
        handle.spawn(async move {
            let Some(identity) = pm.match_multi_resolver(&url).await else {
                if tx.send(ResolvePreviewOutcome::empty()).is_err() {
                    // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                    tracing::debug!("download-manager notification receiver closed");
                }
                return;
            };
            let req = crate::plugin::ResolveRequest {
                task_id: String::new(),
                url: url.clone(),
                auth_ref: String::new(),
                cookies,
                referrer,
                user_agent,
                extra_headers,
                resolver_item: String::new(),
            };
            let fut = std::panic::AssertUnwindSafe(pm.resolve(&identity, req));
            let result = match fut.catch_unwind().await {
                Ok(r) => r,
                Err(panic) => Err(crate::plugin::PluginError::Runtime(panic_message(&panic))),
            };
            let outcome = match result {
                Ok(Some(res)) => match res.manifest {
                    Some(manifest) => ResolvePreviewOutcome {
                        name: manifest.name,
                        items: manifest
                            .items
                            .into_iter()
                            .map(manifest_item_to_info)
                            .collect(),
                        error: String::new(),
                        plugin_id: identity,
                    },
                    None => ResolvePreviewOutcome::empty(),
                },
                Ok(None) => ResolvePreviewOutcome::empty(),
                Err(e) => ResolvePreviewOutcome::failed(e.to_string()),
            };
            if tx.send(outcome).is_err() {
                // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                tracing::debug!("download-manager notification receiver closed");
            }
        });
        rx
    }

    /// `plugins` feature 关时的同签名占位：立即完成一个空 outcome。
    #[cfg(not(feature = "plugins"))]
    pub fn spawn_resolve_preview(
        &self,
        _url: String,
        _cookies: String,
        _referrer: String,
        _user_agent: String,
        _extra_headers: HashMap<String, String>,
    ) -> oneshot::Receiver<ResolvePreviewOutcome> {
        let (tx, rx) = oneshot::channel();
        if tx.send(ResolvePreviewOutcome::empty()).is_err() {
            tracing::debug!("resolve preview receiver closed");
        }
        rx
    }

    /// off-actor resolve worker：在插件专用 runtime 上 spawn（禁裸 tokio::spawn），
    /// panic 隔离，无条件回流（交 on_resolve_ready 兜底）。
    #[allow(clippy::too_many_arguments)]
    #[cfg(feature = "plugins")]
    fn spawn_resolve_worker(
        &self,
        task_id: String,
        identity: String,
        req: crate::plugin::ResolveRequest,
        plan: crate::plugin::call_egress::EgressPlan,
        kind: ResolveKind,
        generation: u64,
        unattended: bool,
        reselect: ResolverReselect,
        cancel: CancellationToken,
    ) {
        use futures_util::FutureExt;
        let Some(pm) = self.plugin_manager.clone() else {
            return;
        };
        let tx = self.resolve_tx.clone();
        let handle = pm.runtime_handle();
        let selector = self.selector.clone();
        let second_stage = !req.resolver_item.is_empty();
        let subject_url = req.url.clone();
        handle.spawn(async move {
            let mut identity = identity;
            // 多插件冲突待选：首次 resolve 前定案（站点默认 / 无人值守回退 / 询问用户），
            // 写回任务绑定后再用选中的插件解析。二段解析沿用母任务插件，绝不询问。
            let reselect_now = !second_stage
                && match reselect {
                    ResolverReselect::No => false,
                    ResolverReselect::Pending => true,
                    ResolverReselect::IfSuperseded => {
                        pm.bound_resolver_superseded(&req.url, &identity).await
                    }
                };
            if reselect_now {
                let choice = tokio::select! {
                    c = pm.choose_task_resolver(
                        &task_id,
                        &req.url,
                        &identity,
                        unattended,
                        selector.as_ref(),
                    ) => c,
                    _ = cancel.cancelled() => return,
                };
                match choice {
                    Ok(crate::plugin::routing::ResolverChoice::Use(chosen)) => identity = chosen,
                    Ok(crate::plugin::routing::ResolverChoice::Cancelled) => {
                        if tx
                            .send(ResolveOutcome {
                                task_id,
                                identity,
                                kind,
                                generation,
                                result: Ok(None),
                                cancelled: Some(crate::plugin::CancelReason::UserCancelled),
                                subject_url,
                                egress: crate::plugin::call_egress::EgressOutcome::default(),
                            })
                            .is_err()
                        {
                            tracing::debug!("download-manager notification receiver closed");
                        }
                        return;
                    }
                    Err(error) => {
                        if tx
                            .send(ResolveOutcome {
                                task_id,
                                identity,
                                kind,
                                generation,
                                result: Err(error),
                                cancelled: None,
                                subject_url,
                                egress: crate::plugin::call_egress::EgressOutcome::default(),
                            })
                            .is_err()
                        {
                            tracing::debug!("download-manager notification receiver closed");
                        }
                        return;
                    }
                }
            }
            // 同一占位 / 世代 / 取消令牌下按出口计划逐步解析：Auto 断在网络层时换路重跑，
            // 窗口内暂停 / 取消 / 删除照常丢弃整个 future。选择框只在成功后出现，重跑不重复询问。
            let fut = std::panic::AssertUnwindSafe(pm.resolve_planned(&identity, req, Some(plan)));
            // 暂停/取消/删除会 cancel 占位 token：立即丢弃 resolve future（释放
            // resolve/yt-dlp 信号量槽位），且不回流——占位已由触发方清理。
            let resolved = tokio::select! {
                r = fut.catch_unwind() => r,
                _ = cancel.cancelled() => return,
            };
            let (mut result, egress) = match resolved {
                Ok(r) => r,
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "resolver panicked".to_string());
                    (
                        Err(crate::plugin::PluginError::Runtime(msg)),
                        crate::plugin::call_egress::EgressOutcome::default(),
                    )
                }
            };
            // 多变体收敛：在 off-actor worker 内 await 用户选择（不阻塞 actor），
            // 收敛为单一直链后再回流。stale 场景由 on_resolve_ready 世代守卫兜底。
            // 返回 true = 用户点关闭/取消 → 回流标记 cancelled，actor 取消任务。
            // 二段解析（resolver_item 非空）与无人值守任务（tasks.unattended，
            // RSS/免打扰接管）绝不弹选择框：静默取默认变体（引擎自动裂变场景
            // 不为 N 个子任务弹 N 个选择框，A1 契约）。
            let mut cancelled = None;
            if cancel.is_cancelled() {
                return;
            }
            if let Ok(Some(res)) = &mut result
                && !res.variants.is_empty()
            {
                if second_stage || unattended {
                    collapse_resolve_variants_silent(res);
                } else {
                    let selected = tokio::select! {
                        c = collapse_resolve_variants(&task_id, res, selector.as_ref()) => c,
                        _ = cancel.cancelled() => return,
                    };
                    if selected {
                        cancelled = Some(crate::plugin::CancelReason::VariantSelectionCancelled);
                    }
                }
            }
            if tx
                .send(ResolveOutcome {
                    task_id,
                    identity,
                    kind,
                    generation,
                    result,
                    cancelled,
                    subject_url,
                    egress,
                })
                .is_err()
            {
                // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                tracing::debug!("download-manager notification receiver closed");
            }
        });
    }

    /// resolve 回流再入（actor 上下文）。收尾后兜底结束已离场优先任务的 Boost
    /// （解析失败 / 裂变 / 选择取消都会让任务离开在途集合）。
    #[cfg(feature = "plugins")]
    pub async fn on_resolve_ready(&mut self, out: ResolveOutcome) {
        self.handle_resolve_ready(out).await;
        self.reconcile_priority().await;
    }

    /// 任务解析的出口计划（actor 上调用；System / Auto 系统候选检测同 `auto_route_decision`
    /// 在 actor 上进行）。已排程的一次性备用链路作为首步，本周期断过的路径不再排为换路目标。
    /// 新一轮解析开始即作废上一轮交给起跑的出口（清单裂变后的二段解析等）。
    #[cfg(feature = "plugins")]
    fn resolve_egress_plan(
        &mut self,
        task_id: &str,
        source_url: &str,
        proxy_url: &str,
    ) -> crate::plugin::call_egress::EgressPlan {
        self.resolved_egress.remove(task_id);
        let forced = self
            .auto_failover_pending
            .get(task_id)
            .map(|target| target.route());
        let tried = self
            .auto_failover_attempts
            .get(task_id)
            .map(AutoFailoverAttempts::tried_routes)
            .unwrap_or_default();
        crate::plugin::call_egress::plan_call(
            &self.proxy_config,
            source_url,
            crate::plugin::call_egress::PlanTask {
                proxy_url,
                forced,
                tried: &tried,
            },
        )
    }

    /// 解析回流（已过世代守卫）：断在网络层的路径计入本周期备用链路尝试；成功时记下
    /// 交给起跑的出口，失败 / 取消时清掉——每次解析回流覆盖，绝不沿用上一次。
    #[cfg(feature = "plugins")]
    fn absorb_resolve_egress(&mut self, task_id: &str, out: &ResolveOutcome) {
        if !out.egress.failed.is_empty() {
            let attempts = self
                .auto_failover_attempts
                .entry(task_id.to_string())
                .or_default();
            for route in &out.egress.failed {
                attempts.mark_route(*route);
            }
        }
        // 清单裂变不直接起跑（成员 / 改写后的任务各自再解析）：不留出口。
        let fission = matches!(&out.result, Ok(Some(res)) if res.manifest.is_some());
        if out.result.is_err() || out.cancelled.is_some() || fission {
            self.resolved_egress.remove(task_id);
            return;
        }
        let pin = out.egress.used.as_ref().and_then(|step| {
            step.route.map(|route| ForcedRoute {
                target: AutoFailoverTarget::from_route(route),
                label: step.label,
            })
        });
        self.resolved_egress.insert(
            task_id.to_string(),
            ResolvedEgress {
                subject_url: out.subject_url.clone(),
                pin,
            },
        );
    }

    /// 起跑出口输入（一次性消费）：插件解析过的任务以源链接定出口并钉定解析所用路径；
    /// 否则用已排程的一次性备用链路。返回（替代任务链接的出口匹配链接, 强制路径）。
    fn take_start_route(&mut self, task_id: &str) -> (Option<String>, Option<ForcedRoute>) {
        let pending = self
            .auto_failover_pending
            .remove(task_id)
            .map(ForcedRoute::failover);
        #[cfg(feature = "plugins")]
        if let Some(resolved) = self.resolved_egress.remove(task_id) {
            return (Some(resolved.subject_url), resolved.pin.or(pending));
        }
        (None, pending)
    }

    /// 先复查 DB 生命周期，再按结果分派。
    #[cfg(feature = "plugins")]
    async fn handle_resolve_ready(&mut self, out: ResolveOutcome) {
        let task_id = out.task_id.clone();
        // 世代守卫：pending 条目必须与本 outcome 同世代才可再入。
        // - 用户在 resolve 窗口内 pause/cancel/delete 均经 clear_pending_resolve 移除条目
        //   （见 pause_task/cancel_task/delete_task）→ pending 为空，outcome stale；
        // - 窗口内 pause→resume 会插入**新世代**的 pending 条目 → 世代不等，旧 worker 的
        //   outcome stale（老实现按成员资格判定，会让旧 outcome 误消费新条目：Start outcome
        //   吞掉 Resume pending 导致 resume 丢失，或旧 Resume outcome 抢先消费新 Resume）。
        // 不能用 DB status 判定，因为 resume 天然从 paused(2)/error(4) 起步。
        // stale outcome 只允许清理「属于自己世代」的占位——active_tasks 里可能已是新世代
        // 的占位、甚至再入后的真实下载条目，绝不能触碰。
        let pending_gen = self
            .pending_resolve
            .get(&task_id)
            .map(PendingResolve::generation);
        if pending_gen != Some(out.generation) {
            if self
                .active_tasks
                .get(&task_id)
                .is_some_and(|e| e.generation == out.generation)
            {
                self.active_tasks.remove(&task_id);
                self.drain_queue().await;
            }
            return;
        }
        self.absorb_resolve_egress(&task_id, &out);
        // 用户在变体 / 插件选择弹窗点关闭/取消 → 任务暂停（与「文件已存在」取消、BT 文件
        // 选择取消同一语义）：请求上下文都在库里，恢复时 begin_resolve_resume 重新解析并
        // 再次询问。放在世代守卫之后：确认本 outcome 拥有当前 pending 条目才动手。
        if let Some(reason) = out.cancelled {
            log_info!(
                "[plugin-resolve] task {} selection cancelled by user ({:?}), pausing task",
                task_id,
                reason
            );
            self.pause_after_selection_cancelled(&task_id, reason).await;
            return;
        }
        // 任务已从 DB 删除（兜底；delete_task 亦已清 pending_resolve）→ 放弃再入。
        let task = match self.db.load_task_by_id(&task_id).await {
            Ok(Some(t)) => t,
            _ => {
                self.pending_resolve.remove(&task_id);
                self.active_tasks.remove(&task_id);
                self.drain_queue().await;
                return;
            }
        };

        match out.result {
            Ok(applied) => {
                // 清单裂变（外部无 UI 入口自动展开，D6）：resolve 结果携带 manifest
                // 时不进入正常 Start/Resume 分派，改走 apply_manifest_fission（单
                // 条目原地改写 / 多条目单事务裂变为组）。Start 与 Resume 两种 kind
                // 均可能触达——旧任务被用户 resume 时同样应裂变。
                let has_manifest = applied.as_ref().is_some_and(|r| r.manifest.is_some());
                if has_manifest {
                    self.pending_resolve.remove(&task_id);
                    self.active_tasks.remove(&task_id);
                    if let Some(res) = applied {
                        self.apply_manifest_fission(task, out.identity, res).await;
                    }
                    return;
                }
                // Ok(Some) = 改写；Ok(None) = 放行（用原 url）。
                match out.kind {
                    ResolveKind::Start => {
                        if let Some(PendingResolve::Start { mut queued, .. }) =
                            self.pending_resolve.remove(&task_id)
                        {
                            // 以实际解析的插件为准（起跑前重选可能已改绑），请求补丁归属它。
                            queued.resolver_plugin_id = out.identity.clone();
                            // 解析窗口内插件被卸载会清掉 DB 绑定。普通任务：丢弃结果、按原始
                            // 链接继续（卸载语义）。二段解析任务的链接是分享页，绝不能直接下载：
                            // 交回 do_start_task 的守卫重新绑定（无可用插件则 fail-closed）。
                            let unbound = self
                                .db
                                .get_task_resolver(&task_id)
                                .await
                                .is_ok_and(|bound| bound.is_empty());
                            if unbound {
                                queued.resolver_plugin_id.clear();
                                if queued.resolver_item.is_empty() {
                                    queued.resolved = true;
                                } else {
                                    self.active_tasks.remove(&task_id);
                                }
                            } else {
                                if let Some(res) = applied {
                                    apply_resolve_to_queued(&mut queued, res);
                                }
                                queued.resolved = true;
                            }
                            self.do_start_task(*queued).await;
                            self.drain_queue().await;
                        }
                    }
                    ResolveKind::Resume => {
                        self.pending_resolve.remove(&task_id);
                        self.active_tasks.remove(&task_id);
                        // Some(res) 改写；None → 空 ResolveResult 表示放行（用原 url）。
                        // 经 resume_applied 字段传给 do_resume_task（避免改其签名/所有调用点）；
                        // do_resume_task 体首无条件取走该条目（含 DB 绑定已被卸载清空的分支）。
                        self.resume_applied
                            .insert(task_id.clone(), applied.unwrap_or_default());
                        self.do_resume_task(&task_id).await;
                        self.drain_queue().await;
                    }
                }
            }
            Err(e) => {
                // 「[插件]」前缀 = 客户端可提供「忽略插件重试」；二段解析任务（任务组成员 /
                // 插件订阅条目）的原始链接是分享页，忽略插件会被拒绝，换用不同前缀。
                let second_stage = match self.db.get_task_resolver_item(&task_id).await {
                    Ok(item) => !item.is_empty(),
                    Err(error) => {
                        crate::logger::report_error(
                            "download-manager",
                            "read resolver item",
                            &error,
                        );
                        false
                    }
                };
                let prefix = if second_stage {
                    "[解析插件]"
                } else {
                    "[插件]"
                };
                let msg = format!("{prefix} {}: {}", out.identity, e);
                if let Err(error) = self.db.update_task_status(&task_id, 4, &msg).await {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                }
                self.sink.emit(EngineEvent::TaskProgress {
                    task_id: task_id.clone(),
                    status: 4,
                    downloaded_bytes: task.downloaded_bytes,
                    total_bytes: task.total_bytes,
                    speed: 0,
                    file_name: task.file_name.clone(),
                    save_dir: task.save_dir.clone(),
                    url: task.url.clone(),
                    error_message: msg,
                    upload_speed_bps: 0,
                    uploaded_bytes: task.uploaded_bytes,
                    seeding_status: task.seeding_status,
                    seeding_message: task.seeding_message.clone(),
                    seeding_time_secs: task.seeding_time_secs,
                });
                self.pending_resolve.remove(&task_id);
                self.active_tasks.remove(&task_id);
                self.drain_queue().await;
            }
        }
    }

    /// resolve 结果携带 [`crate::plugin::ResolveManifest`] 时的裂变分派（外部无
    /// UI 入口的自动展开，D6）：单条目原地改写落盘目标（不建组）；多条目单事务
    /// 裂变为任务组。`identity` 为命中的 resolver 插件 ID（复制进裂变出的兄弟
    /// 任务的 `resolver_plugin_id`，保证二段解析走同一插件）。
    ///
    /// 守卫：`task.downloaded_bytes != 0`（任务已有下载数据）→ 拒绝改写（避免
    /// 摧毁已下载分段），置 status=4。
    #[cfg(feature = "plugins")]
    async fn apply_manifest_fission(
        &mut self,
        task: TaskInfo,
        identity: String,
        res: crate::plugin::ResolveResult,
    ) {
        let task_id = task.task_id.clone();
        let Some(manifest) = res.manifest else {
            self.drain_queue().await;
            return;
        };
        if task.downloaded_bytes != 0 {
            let msg = "任务已有数据，拒绝清单改写".to_string();
            if let Err(error) = self.db.update_task_status(&task_id, 4, &msg).await {
                crate::logger::report_error("download-manager", "persist task state", &error);
            }
            self.sink.emit(EngineEvent::TaskProgress {
                task_id: task_id.clone(),
                status: 4,
                downloaded_bytes: task.downloaded_bytes,
                total_bytes: task.total_bytes,
                speed: 0,
                file_name: task.file_name.clone(),
                save_dir: task.save_dir.clone(),
                url: task.url.clone(),
                error_message: msg,
                upload_speed_bps: 0,
                uploaded_bytes: task.uploaded_bytes,
                seeding_status: task.seeding_status,
                seeding_message: task.seeding_message.clone(),
                seeding_time_secs: task.seeding_time_secs,
            });
            self.drain_queue().await;
            return;
        }
        if manifest.items.is_empty() {
            // 校验器已 fail-closed 挡空 items；理论不可达，防御性放行不改写。
            self.drain_queue().await;
            return;
        }

        let context = match self.db.load_task_request_context(&task_id).await {
            Ok(context) => context,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load manifest request context",
                    &error,
                );
                self.drain_queue().await;
                return;
            }
        };
        let (cookies, referrer, extra_headers) = match context {
            None => (String::new(), String::new(), HashMap::new()),
            Some((cookies, referrer, headers)) => {
                let headers = if headers.is_empty() {
                    HashMap::new()
                } else {
                    match serde_json::from_str(&headers) {
                        Ok(headers) => headers,
                        Err(error) => {
                            crate::logger::report_error(
                                "download-manager",
                                "decode manifest request headers",
                                &error,
                            );
                            self.drain_queue().await;
                            return;
                        }
                    }
                };
                (cookies, referrer, headers)
            }
        };
        // 分类目录路由（外部捕获）：初始目录是按解析前的来源地址选的，不代表清单内容，
        // 清单一律以基准目录展开。单个无子路径的条目按条目名路由并保留复核；任务组与
        // 带子路径的条目保持来源结构，不再路由。
        let route_column = match self.db.load_task_category_route(&task_id).await {
            Ok(column) => column,
            Err(error) => {
                crate::logger::report_error("download-manager", "load category route", &error);
                self.drain_queue().await;
                return;
            }
        };
        let route = crate::category_route::CategoryRoute::from_column(&task_id, &route_column)
            .filter(|route| !route.base_dir.is_empty());
        let manifest_base = route
            .as_ref()
            .map_or(task.save_dir.as_str(), |route| route.base_dir.as_str());

        if manifest.items.len() == 1 {
            let item = &manifest.items[0];
            let resolver_item = manifest_item_resolver_token(item);
            let (new_save_dir, item_route) = match &route {
                Some(route) if item.path.is_empty() => (
                    route
                        .save_dir_for(
                            &item.name,
                            &task.url,
                            crate::category_route::source_address(&task.origin_url, &task.url),
                        )
                        .unwrap_or(manifest_base)
                        .to_owned(),
                    route_column.as_str(),
                ),
                _ => (join_manifest_path(manifest_base, &item.path), ""),
            };
            if let Err(e) = self
                .db
                .rewrite_task_for_item(
                    &task_id,
                    &new_save_dir,
                    &item.name,
                    item.size.unwrap_or(0).max(0),
                    item_route,
                )
                .await
            {
                crate::logger::report_error("download-manager", "rewrite manifest task", &e);
                self.drain_queue().await;
                return;
            }
            if let Err(error) = self
                .db
                .set_task_resolver_item(&task_id, &resolver_item)
                .await
            {
                crate::logger::report_error("download-manager", "persist task state", &error);
                if let Err(rollback_error) = self
                    .db
                    .rewrite_task_for_item(
                        &task_id,
                        &task.save_dir,
                        &task.file_name,
                        task.total_bytes,
                        &route_column,
                    )
                    .await
                {
                    crate::logger::report_error(
                        "download-manager",
                        "roll back manifest task rewrite",
                        &rollback_error,
                    );
                }
                self.drain_queue().await;
                return;
            }
            let new_queued = QueuedTask {
                task_id: task_id.clone(),
                url: task.url.clone(),
                save_dir: new_save_dir,
                file_name: item.name.clone(),
                segments: task.segments,
                is_resume: false,
                cookies,
                referrer,
                hint_file_size: 0,
                torrent_file_bytes: Vec::new(),
                proxy_url: task.proxy_url.clone(),
                user_agent: String::new(),
                queue_id: task.queue_id.clone(),
                checksum: String::new(),
                ignore_tls_errors: task.ignore_tls_errors,
                extra_headers,
                selected_file_indices: Vec::new(),
                method: None,
                body: None,
                audio_url: None,
                resolver_plugin_id: identity,
                resolved: false,
                range_supported: false,
                resolver_item,
                source_url: task.url.clone(),
                resolver_patch: None,
            };
            self.begin_resolve_start(new_queued).await;
            self.send_tasks_snapshot().await;
            self.broadcast_queue_positions();
            return;
        }

        // 多条目：单事务裂变为组（Db::fission_into_group，失败整体回滚）。
        let group_id = Uuid::new_v4().to_string();
        let group_name = if !manifest.name.is_empty() {
            manifest.name.clone()
        } else if !task.file_name.is_empty() {
            task.file_name.clone()
        } else {
            manifest.items[0].name.clone()
        };
        let group_save_dir =
            join_manifest_path(manifest_base, &naming::sanitize_filename(&group_name));

        let total_size: i64 = manifest
            .items
            .iter()
            .map(|it| it.size.unwrap_or(0).max(0))
            .sum();
        let over_threshold = total_size > FISSION_AUTO_START_MAX_TOTAL_BYTES;
        let status = if over_threshold { 2 } else { 0 };

        let mother_item = &manifest.items[0];
        let mother_resolver_item = manifest_item_resolver_token(mother_item);
        let mother_save_dir = join_manifest_path(&group_save_dir, &mother_item.path);
        let mother_file_name = mother_item.name.clone();
        let mother_total_bytes = mother_item.size.unwrap_or(0).max(0);

        let siblings: Vec<crate::db::GroupSiblingSpec> = manifest.items[1..]
            .iter()
            .map(|item| crate::db::GroupSiblingSpec {
                id: Uuid::now_v7().to_string(),
                file_name: item.name.clone(),
                save_dir: join_manifest_path(&group_save_dir, &item.path),
                resolver_item: manifest_item_resolver_token(item),
                total_bytes: item.size.unwrap_or(0).max(0),
                status,
            })
            .collect();

        let spec = crate::db::FissionSpec {
            group_id,
            group_name,
            group_save_dir,
            group_source_url: task.url.clone(),
            mother_task_id: task_id.clone(),
            mother_resolver_item,
            mother_file_name,
            mother_save_dir,
            mother_total_bytes,
            mother_status: status,
            siblings,
        };

        if let Err(e) = self.db.fission_into_group(&spec).await {
            let msg = format!("裂变失败: {e}");
            if let Err(error) = self.db.update_task_status(&task_id, 4, &msg).await {
                crate::logger::report_error("download-manager", "persist task state", &error);
            }
            self.sink.emit(EngineEvent::TaskProgress {
                task_id: task_id.clone(),
                status: 4,
                downloaded_bytes: 0,
                total_bytes: task.total_bytes,
                speed: 0,
                file_name: task.file_name.clone(),
                save_dir: task.save_dir.clone(),
                url: task.url.clone(),
                error_message: msg,
                upload_speed_bps: 0,
                uploaded_bytes: task.uploaded_bytes,
                seeding_status: task.seeding_status,
                seeding_message: task.seeding_message.clone(),
                seeding_time_secs: task.seeding_time_secs,
            });
            self.drain_queue().await;
            return;
        }

        let crate::db::FissionSpec {
            mother_file_name,
            mother_save_dir,
            mother_resolver_item,
            siblings,
            ..
        } = spec;

        // over_threshold：全员（含母）已由 fission_into_group 落库为 paused，
        // 不启动、也不逐成员广播——尾部 send_tasks_snapshot 的快照一次性
        // 覆盖全部成员状态（消除 N 成员 N 条 TaskProgress 的事件风暴）。
        if !over_threshold {
            let mother_queued = QueuedTask {
                task_id: task_id.clone(),
                url: task.url.clone(),
                save_dir: mother_save_dir,
                file_name: mother_file_name,
                segments: task.segments,
                is_resume: false,
                cookies: cookies.clone(),
                referrer: referrer.clone(),
                hint_file_size: 0,
                torrent_file_bytes: Vec::new(),
                proxy_url: task.proxy_url.clone(),
                user_agent: String::new(),
                queue_id: task.queue_id.clone(),
                checksum: String::new(),
                ignore_tls_errors: task.ignore_tls_errors,
                extra_headers: extra_headers.clone(),
                selected_file_indices: Vec::new(),
                method: None,
                body: None,
                audio_url: None,
                resolver_plugin_id: identity.clone(),
                resolved: false,
                range_supported: false,
                resolver_item: mother_resolver_item,
                source_url: task.url.clone(),
                resolver_patch: None,
            };
            self.begin_resolve_start(mother_queued).await;

            // 抑制逐兄弟入队广播（每入队一个就全量广播一次队列位置 = O(N²)
            // 载荷），尾部统一 broadcast_queue_positions + 快照。
            self.suppress_bulk_broadcasts = true;
            for sib in siblings {
                let sib_queued = QueuedTask {
                    task_id: sib.id,
                    url: task.url.clone(),
                    save_dir: sib.save_dir,
                    file_name: sib.file_name,
                    segments: task.segments,
                    is_resume: false,
                    cookies: cookies.clone(),
                    referrer: referrer.clone(),
                    hint_file_size: 0,
                    torrent_file_bytes: Vec::new(),
                    proxy_url: task.proxy_url.clone(),
                    user_agent: String::new(),
                    queue_id: task.queue_id.clone(),
                    checksum: String::new(),
                    ignore_tls_errors: task.ignore_tls_errors,
                    extra_headers: extra_headers.clone(),
                    selected_file_indices: Vec::new(),
                    method: None,
                    body: None,
                    audio_url: None,
                    resolver_plugin_id: identity.clone(),
                    resolved: false,
                    range_supported: false,
                    resolver_item: sib.resolver_item,
                    source_url: task.url.clone(),
                    resolver_patch: None,
                };
                self.enqueue_persisted_task(sib_queued, true).await;
            }
            self.suppress_bulk_broadcasts = false;
        }

        self.send_tasks_snapshot().await;
        self.send_all_groups().await;
        self.broadcast_queue_positions();
    }

    /// 登记一个已排程的重试（自动重试 / 备用链路 / 插件 `requestRetry` 共用）。
    /// 集合大小变化时向宿主上报 `RetryPendingChanged`。
    fn retry_schedule(&mut self, task_id: &str, queue_id: String) {
        self.retry_scheduled.insert(task_id.to_string(), queue_id);
        self.report_retry_pending();
    }

    /// 撤销任务的重试排程（重试已落地、手动暂停/恢复/删除抢先等）。
    fn retry_unschedule(&mut self, task_id: &str) {
        if self.retry_scheduled.remove(task_id).is_some() {
            self.report_retry_pending();
        }
    }

    /// 「等待重试」集合大小与上次上报不同时发出 `RetryPendingChanged`。
    fn report_retry_pending(&mut self) {
        let count = u32::try_from(self.retry_scheduled.len()).unwrap_or(u32::MAX);
        if count != self.retry_pending_reported {
            self.retry_pending_reported = count;
            self.sink.emit(EngineEvent::RetryPendingChanged { count });
        }
    }

    /// 插件 onError 命令式重试（actor 上下文，复用 auto_retry 账本限流）。
    #[cfg(feature = "plugins")]
    pub async fn plugin_request_retry(&mut self, task_id: &str, delay_ms: u64) {
        let from_interceptor = self
            .intercept_retry_marks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(task_id);
        let max_retries = self.max_auto_retries;
        if !from_interceptor && max_retries == 0 {
            return;
        }
        // 引擎自动重试/备用链路已为这次失败排程：插件再排一次会双倍扣配额并重复恢复。
        if self.retry_scheduled.contains_key(task_id)
            || self.auto_failover_pending.contains_key(task_id)
        {
            return;
        }
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(task) => task,
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "load task for plugin retry",
                    &error,
                );
                return;
            }
        };
        let Some(queue_id) = task.filter(|t| t.status == 4).map(|t| t.queue_id) else {
            return;
        };
        if from_interceptor {
            let count = self
                .intercept_retry_counts
                .entry(task_id.to_string())
                .or_insert(0);
            if *count >= INTERCEPT_RETRY_LIMIT {
                log_info!("[plugin] task {} 拦截器重试已达上限，忽略", task_id);
                return;
            }
            *count += 1;
        } else {
            let count = self
                .auto_retry_counts
                .entry(task_id.to_string())
                .or_insert(0);
            if max_retries != -1 && (*count as i32) >= max_retries {
                log_info!("[plugin] task {} 重试已达上限，忽略 requestRetry", task_id);
                return;
            }
            *count += 1;
        }
        self.retry_schedule(task_id, queue_id);
        let tx = self.retry_tx.clone();
        let tid = task_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            if tx.send(tid).await.is_err() {
                // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                tracing::debug!("download-manager notification receiver closed");
            }
        });
    }

    /// do_start_task 体首守卫调用：占位 + 存 pending_resolve + spawn off-actor worker。
    #[cfg(feature = "plugins")]
    async fn begin_resolve_start(&mut self, queued: QueuedTask) {
        let task_id = queued.task_id.clone();
        let identity = queued.resolver_plugin_id.clone();
        let has_audio_track = queued.audio_url.is_some();
        // 占位插入 active_tasks（用原始 url 算 is_bt，真实 generation/cancel_token）：
        // resolve 期间任务可被 pause/cancel 触达、正确占并发计数；再入时被覆盖。
        self.generation += 1;
        let spawn_gen = self.generation;
        let is_bt = is_magnet(&queued.url)
            || !queued.torrent_file_bytes.is_empty()
            || is_torrent_file_url(&queued.url);
        let resolve_token = CancellationToken::new();
        self.active_tasks.insert(
            task_id.clone(),
            ActiveTaskEntry {
                token: resolve_token.clone(),
                generation: spawn_gen,
                handle: None,
                is_bt,
                queue_id: queued.queue_id.clone(),
            },
        );
        let req = crate::plugin::ResolveRequest {
            task_id: task_id.clone(),
            url: queued.url.clone(),
            auth_ref: String::new(),
            cookies: queued.cookies.clone(),
            referrer: queued.referrer.clone(),
            user_agent: self.plugin_resolve_user_agent(
                &queued.user_agent,
                &queued.extra_headers,
                &queued.queue_id,
            ),
            extra_headers: queued.extra_headers.clone(),
            resolver_item: queued.resolver_item.clone(),
        };
        let plan = self.resolve_egress_plan(&task_id, &req.url, &queued.proxy_url);
        self.pending_resolve.insert(
            task_id.clone(),
            PendingResolve::Start {
                queued: Box::new(queued),
                generation: spawn_gen,
            },
        );
        let unattended = self.db.is_task_unattended(&task_id).await.unwrap_or(false);
        let reselect = self
            .resolver_reselect(&task_id, &req.resolver_item, has_audio_track)
            .await;
        self.mark_resolving(&task_id).await;
        self.spawn_resolve_worker(
            task_id,
            identity,
            req,
            plan,
            ResolveKind::Start,
            spawn_gen,
            unattended,
            reselect,
            resolve_token,
        );
    }

    /// 起跑前是否要为任务重新选择 resolver 插件（见 [`ResolverReselect`]）。读库失败按
    /// 「不重选」处理：沿用已绑定插件，与该插件独占匹配时的行为一致。
    #[cfg(feature = "plugins")]
    async fn resolver_reselect(
        &self,
        task_id: &str,
        resolver_item: &str,
        has_audio_track: bool,
    ) -> ResolverReselect {
        match self.db.is_task_resolver_choice_pending(task_id).await {
            Ok(true) => return ResolverReselect::Pending,
            Ok(false) => {}
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load plugin choice pending flag",
                    &error,
                );
                return ResolverReselect::No;
            }
        }
        // 二段解析任务沿用母任务插件；已有落盘进度的任务中途换 resolver 可能写坏文件。
        if !resolver_item.is_empty() {
            return ResolverReselect::No;
        }
        match self.db.load_task_by_id(task_id).await {
            Ok(Some(task)) if self.never_started(&task, has_audio_track).await => {
                ResolverReselect::IfSuperseded
            }
            Ok(_) => ResolverReselect::No,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load task for resolver reselect",
                    &error,
                );
                ResolverReselect::No
            }
        }
    }

    /// 插件 resolve 上下文的 User-Agent，与该任务下载请求的身份一致：已持久化请求头里的
    /// User-Agent（调用方捕获或单任务覆盖，创建时并入）优先，否则按单任务覆盖 → 队列 →
    /// 全局默认解析。首次起跑与暂停 / 重启后的恢复重解析因此拿到同一个值。
    #[cfg(feature = "plugins")]
    fn plugin_resolve_user_agent(
        &self,
        explicit: &str,
        extra_headers: &HashMap<String, String>,
        queue_id: &str,
    ) -> String {
        extra_headers
            .iter()
            .find_map(|(name, value)| {
                name.eq_ignore_ascii_case("user-agent")
                    .then(|| value.clone())
            })
            .unwrap_or_else(|| self.resolved_task_ua(explicit, queue_id).to_string())
    }

    /// resolve 开始：落库并广播「准备中」(5)，插件解析（yt-dlp 可能要数秒）期间各端
    /// 显示进行中而不是「等待中」。与下载器探测阶段同一约定：重启时
    /// `reset_incomplete_tasks_to_paused` 把 5 归为暂停并交给自动恢复。
    /// 落库失败只记录：状态展示不阻断解析。
    #[cfg(feature = "plugins")]
    async fn mark_resolving(&self, task_id: &str) {
        if let Err(error) = self.db.update_task_status(task_id, 5, "").await {
            crate::logger::report_error("download-manager", "persist resolving state", &error);
            return;
        }
        self.emit_progress_from_db(task_id, 5, 0, "", 0).await;
    }

    /// 变体 / 插件选择弹窗被用户取消：任务暂停（不发 `task.paused` webhook——这是关闭
    /// 弹窗而非点暂停），并照常以 `reason` 通知插件 `onCancel`（593#1）。
    #[cfg(feature = "plugins")]
    async fn pause_after_selection_cancelled(
        &mut self,
        task_id: &str,
        reason: crate::plugin::CancelReason,
    ) {
        self.pause_task_silent(task_id).await;
        let Some(pm) = &self.plugin_manager else {
            return;
        };
        let url = match self.db.load_task_by_id(task_id).await {
            Ok(Some(task)) => task.url,
            Ok(None) => return,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load task for plugin cancel event",
                    &error,
                );
                return;
            }
        };
        pm.notify(crate::plugin::PluginEvent::Cancel {
            task_id: task_id.to_string(),
            url,
            reason,
        })
        .await;
    }

    /// do_resume_task 体首守卫调用：对称占位（防 resumeAll 并发双 resolve）+ spawn。
    #[cfg(feature = "plugins")]
    async fn begin_resolve_resume(&mut self, task_id: &str, identity: String) {
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load task for plugin resume",
                    &error,
                );
                return;
            }
        };
        let (cookies, referrer, headers_json) =
            match self.db.load_task_request_context(task_id).await {
                Ok(Some(context)) => context,
                Ok(None) => return,
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "load plugin resume request context",
                        &error,
                    );
                    persist_task_failure(
                        task_id,
                        format!("load request context: {error}"),
                        &self.db,
                        &self.progress_tx,
                    )
                    .await;
                    return;
                }
            };
        let extra_headers = if headers_json.is_empty() {
            HashMap::new()
        } else {
            match serde_json::from_str(&headers_json) {
                Ok(headers) => headers,
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "decode plugin resume request headers",
                        &error,
                    );
                    persist_task_failure(
                        task_id,
                        format!("decode request headers: {error}"),
                        &self.db,
                        &self.progress_tx,
                    )
                    .await;
                    return;
                }
            }
        };
        let resolver_item = match self.db.get_task_resolver_item(task_id).await {
            Ok(item) => item,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load plugin resume resolver selection",
                    &error,
                );
                persist_task_failure(
                    task_id,
                    format!("load resolver selection: {error}"),
                    &self.db,
                    &self.progress_tx,
                )
                .await;
                return;
            }
        };
        let unattended = match self.db.is_task_unattended(task_id).await {
            Ok(unattended) => unattended,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load plugin resume unattended setting",
                    &error,
                );
                persist_task_failure(
                    task_id,
                    format!("load unattended setting: {error}"),
                    &self.db,
                    &self.progress_tx,
                )
                .await;
                return;
            }
        };
        // 对称占位：resolve-wait 期间 task 须在 active_tasks，否则 resume_task_inner
        // 重入检查恒 false，resumeAll/双击/自动重试会并发 spawn 第二个 resolve。
        self.generation += 1;
        let spawn_gen = self.generation;
        let is_bt = is_bt_url(&task.url);
        let resolve_token = CancellationToken::new();
        self.active_tasks.insert(
            task_id.to_string(),
            ActiveTaskEntry {
                token: resolve_token.clone(),
                generation: spawn_gen,
                handle: None,
                is_bt,
                queue_id: task.queue_id.clone(),
            },
        );
        let req = crate::plugin::ResolveRequest {
            task_id: task_id.to_string(),
            url: task.url.clone(),
            auth_ref: String::new(),
            cookies,
            referrer,
            user_agent: self.plugin_resolve_user_agent("", &extra_headers, &task.queue_id),
            extra_headers,
            resolver_item,
        };
        let plan = self.resolve_egress_plan(task_id, &req.url, &task.proxy_url);
        self.pending_resolve.insert(
            task_id.to_string(),
            PendingResolve::Resume {
                generation: spawn_gen,
            },
        );
        let reselect = self
            .resolver_reselect(task_id, &req.resolver_item, false)
            .await;
        self.mark_resolving(task_id).await;
        self.spawn_resolve_worker(
            task_id.to_string(),
            identity,
            req,
            plan,
            ResolveKind::Resume,
            spawn_gen,
            unattended,
            reselect,
            resolve_token,
        );
    }

    /// 检查任务是否仍处于"可自动重试的 error(4)"状态，供 actor loop 在自动
    /// 重试前确认。如果用户已手动暂停/恢复/删除了该任务，返回 false 跳过重试。
    ///
    /// 关键：取消任务复用了 status=4（见 [`CANCELLED_ERROR_MESSAGE`]）。延迟
    /// 重试任务已 spawn 且无法 abort，若用户在延迟睡眠期间取消任务，actor loop
    /// 仍会收到重试信号。此处显式排除 error_message 为 "cancelled" 的任务，
    /// 防止用户明确取消的下载被自动重启。
    pub async fn is_task_in_error(&self, task_id: &str) -> bool {
        match self.db.load_task_by_id(task_id).await {
            Ok(task) => {
                task.is_some_and(|t| t.status == 4 && t.error_message != CANCELLED_ERROR_MESSAGE)
            }
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "check task retry eligibility",
                    &error,
                );
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // Configuration update methods (called from actor when SaveConfig arrives)
    // -----------------------------------------------------------------------

    /// Update max concurrent tasks limit.  Immediately drains the queue
    /// if the new limit allows more active tasks.
    pub async fn set_max_concurrent(&mut self, max: usize) {
        self.max_concurrent = max;
        #[cfg(feature = "plugins")]
        if let Some(pm) = &self.plugin_manager {
            pm.set_resolve_capacity(max);
        }
        // Try to start queued tasks if we now have capacity.
        self.drain_queue().await;
    }

    /// 更新最大自动重试次数。`-1` = 无限，`0` = 关闭，`1..=10` = 次数上限。
    /// 仅影响后续失败任务的重试判定，不回溯已耗尽计数的任务。
    pub fn set_max_auto_retries(&mut self, v: i32) {
        self.max_auto_retries = v;
    }

    /// 更新自动重试基础延迟（秒）。实际延迟 = base × attempt（递增）。
    pub fn set_auto_retry_delay_secs(&mut self, v: u64) {
        self.auto_retry_delay_secs = v;
    }

    /// Update the default save directory.  This is used when initialising a
    /// new BT session and as the fallback for new tasks.  If the BT session
    /// is already running, it won't move — but new `add_torrent` calls use
    /// per-torrent `output_folder` overrides, so this primarily affects
    /// future session re-creation (e.g. after app restart).
    pub fn set_default_save_dir(&mut self, dir: String) {
        self.default_save_dir = dir;
    }

    /// Update global default segment count. 0 = defer to segment_advisor.
    pub fn set_default_segments(&mut self, v: i32) {
        self.global_default_segments = v;
    }

    /// Update the Auto-mode max connection cap (config `auto_max_connections`).
    /// <=0 = unlimited (advisor value used as-is).
    pub fn set_auto_max_connections(&mut self, v: i32) {
        self.auto_max_connections = v;
    }

    /// Update the Multi-CDN aggregation toggle (config `cdn_multi_enabled`).
    pub fn set_cdn_multi_enabled(&mut self, v: bool) {
        self.cdn_multi_enabled = v;
    }

    /// Update the multi-NIC aggregation toggle (config `multi_nic_enabled`).
    pub fn set_multi_nic_enabled(&mut self, v: bool) {
        self.multi_nic_enabled = v;
    }

    /// Update the Multi-CDN per-task pinned-node cap (config `cdn_max_nodes`).
    /// 0 = 自动档（按文件大小/并发推导）；1..=8 手动值。兜底 clamp 杜绝
    /// 越界配置。
    pub fn set_cdn_max_nodes(&mut self, v: i32) {
        self.cdn_max_nodes = v.clamp(0, crate::cdn::MAX_NODES_LIMIT as i32);
    }

    /// Update the cloud-issued DoH resolver endpoint list (config
    /// `cdn_resolver_endpoints`，JSON 字符串数组)。校验与回退语义见
    /// [`crate::cdn::resolver::set_dynamic_endpoints`]。
    pub fn set_cdn_resolver_endpoints(&mut self, json: &str) {
        crate::cdn::resolver::set_dynamic_endpoints(json);
    }

    /// Update the cloud hints base origin (config `cdn_hints_base`，空 =
    /// 禁用；仅接受 https)。
    pub fn set_cdn_hints_base(&mut self, base: &str) {
        crate::cdn::hints::set_base(base);
    }

    /// Update the cloud-issued ECS probe subnets (config `cdn_ecs_subnets`，
    /// JSON 字符串数组，IPv4 CIDR)。
    pub fn set_cdn_ecs_subnets(&mut self, json: &str) {
        crate::cdn::resolver::set_ecs_subnets(json);
    }

    /// agent `cdn_worker` 遥测上报完成（config `cdn_pending_reports` 写空）→ 清空引擎侧
    /// 待上传样本缓冲。
    pub fn clear_cdn_pending_reports(&mut self) {
        crate::cdn::telemetry::clear();
    }
    /// 同步落盘遥测缓冲（agent `cdn_worker` 读 config 前经 daemon 调用，
    /// 保证上报读到全部内存样本，见 telemetry::flush 文档）。
    pub async fn flush_cdn_pending_reports(&self) {
        crate::cdn::telemetry::flush(&self.db).await;
    }

    /// Update whether completed downloads adopt the server-provided
    /// `Last-Modified` timestamp as the file's modification time
    /// (config `use_server_time`). Takes effect for downloads started
    /// after the change; already-running downloads keep the value they
    /// were spawned with.
    pub fn set_use_server_time(&mut self, v: bool) {
        self.use_server_time = v;
    }

    /// Update the "when file exists" behavior (config `file_exists_behavior`).
    /// Takes effect for downloads started after the change; already-running
    /// downloads keep the value they were spawned with.
    pub fn set_file_exists_behavior(&mut self, v: FileExistsBehavior) {
        self.file_exists_behavior = v;
    }

    /// Update the "file deleted or moved" behavior (config
    /// `file_missing_action`): `true` = automatically delete the task record
    /// once the file-tracking scan finds the file gone (`"delete"`),
    /// `false` = keep the record and only flag it (`"keep"`, default).
    /// Takes effect from the next scan onwards; the scan already in flight
    /// keeps the value it was spawned with.
    pub fn set_missing_file_auto_delete(&mut self, on: bool) {
        self.missing_file_auto_delete = on;
    }

    /// Update whether the periodic idle file-tracking scan is allowed to
    /// run while completely idle (config `idle_file_scan`, default `true`).
    /// `false` skips the scheduled scan when nothing is active or queued;
    /// window-focus / manual rescan triggers are unaffected.
    pub fn set_idle_file_scan(&mut self, on: bool) {
        self.idle_file_scan = on;
    }

    /// 更新已完成记录保留天数（config `completed_record_retention_days`，0 = 永不）。
    /// 同时清除去抖时刻，使随后的 [`DownloadManager::purge_expired_completed_records`]
    /// 立即生效。
    pub fn set_completed_record_retention_days(&mut self, days: u32) {
        self.completed_record_retention_days = days;
        self.last_retention_purge = None;
    }

    /// Update global speed limit (bytes/sec).  Takes effect immediately on
    /// all active and future HTTP/FTP/BT downloads.  0 = unlimited.
    pub fn set_speed_limit(&mut self, bps: u64) {
        self.speed_limiter.set_limit(bps);
        // 队列限速为 0 的队列 limiter（在跑任务仍持有）跟随全局限速。
        for (qid, limiter) in &self.queue_limiters {
            if self.queues.get(qid).is_none_or(|q| q.speed_limit_kbps <= 0) {
                limiter.set_limit(bps);
            }
        }
        // Synchronise the download limit to the shared BT session (if initialised).
        #[cfg(feature = "bt")]
        if let Some(ref bt) = self.bt_session {
            bt.set_speed_limit(bps);
        }
    }

    /// Update global BT upload speed limit (bytes/sec)，覆盖下载期上传与
    /// 做种。已创建的共享 BT 会话立即热生效；未创建时记住取值，供
    /// [`Self::ensure_bt_session`] 惰性创建会话时使用。0 = 不限。
    pub fn set_upload_speed_limit(&mut self, bps: u64) {
        self.upload_limit_bps = bps;
        #[cfg(feature = "bt")]
        if let Some(ref bt) = self.bt_session {
            bt.set_upload_speed_limit(bps);
        }
    }

    fn invalidate_system_client(&self) {
        *self
            .system_client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Update proxy configuration.  Rebuilds the shared HTTP client so that
    /// all **new** downloads use the updated proxy settings.  Already-running
    /// downloads keep their existing client and are unaffected.
    ///
    /// Returns `Err` if the new client cannot be built (e.g. invalid SOCKS URL).
    pub fn set_proxy_config(
        &mut self,
        config: ProxyConfig,
    ) -> Result<(), downloader::DownloadError> {
        log_info!(
            "[manager] updating proxy config: mode={}, type={}, host={}, port={}",
            config.mode.as_str(),
            config.proxy_type.as_str(),
            config.host,
            config.port,
        );
        let new_client = downloader::build_client(&config, &self.global_user_agent)?;
        self.client = new_client;
        self.invalidate_system_client();
        self.proxy_config = config;
        #[cfg(feature = "bt")]
        self.bt_guard.set_proxy(self.proxy_config.clone());
        // 仅 `useProxy` 端点用得上，但出口变了就得跟着重建。
        self.webhook.set_proxy_config(&self.proxy_config);
        // 插件 `flux.fetch` / yt-dlp 与下载任务同一出口（GitHub 加速等测速请求随之换代理）。
        #[cfg(feature = "plugins")]
        if let Some(bridge) = &self.plugin_bridge
            && let Err(error) = bridge.set_proxy_config(&self.proxy_config)
        {
            crate::logger::report_warning("plugin", "rebuild_bridge_proxy", &error);
        }
        // 网络出口变化：域名连接上限是对【旧出口】的服务器策略观察，
        // 换代理后不再可信，清空重学（内存 + 持久化）。
        crate::segment_coordinator::clear_domain_conn_caps(&self.db);
        // ProxyMode::Auto 的路径先验同理：旧估计针对旧候选代理/旧出口，
        // 全部作废（持久化先验 + failover 标记，避免过期标签误导可追溯性；
        // 指纹只覆盖系统代理，手动字段变更必须在此清）。
        self.auto_failover_pending.clear();
        self.auto_failover_attempts.clear();
        crate::route_health::clear_all(&self.db);
        Ok(())
    }

    /// 只替换站点代理规则（`proxy_rules` 单独变化时）。全局出口不变，因此**不**
    /// 重建全局 client，也不清 route_health、域名连接上限与 Auto failover 状态；
    /// 已运行任务沿用启动时的规则快照，之后开始/恢复的任务与新请求使用新规则。
    pub fn set_proxy_rules(&mut self, rules: Option<Arc<crate::proxy_rules::ProxyRuleSet>>) {
        let count = rules.as_ref().map_or(0, |r| r.len());
        self.proxy_config.rules = rules;
        #[cfg(feature = "bt")]
        self.bt_guard.set_proxy(self.proxy_config.clone());
        // 插件 `flux.fetch` / yt-dlp 同样按规则选出口。
        #[cfg(feature = "plugins")]
        if let Some(bridge) = &self.plugin_bridge
            && let Err(error) = bridge.set_proxy_config(&self.proxy_config)
        {
            crate::logger::report_warning("plugin", "rebuild_bridge_proxy", &error);
        }
        log_info!("[manager] proxy rules updated: {} enabled rule(s)", count);
    }

    /// 清空已学习的域名连接上限观察（内存 + 持久化）。
    /// 供用户在设置中手动重置——学习结果与服务器当前策略不符时的逃生门。
    pub fn clear_domain_conn_caps(&self) {
        crate::segment_coordinator::clear_domain_conn_caps(&self.db);
    }

    /// 修改某个任务的分段（线程）数。**已下进度完整保留**：只更新
    /// `tasks.segments`，不动分段行与磁盘临时文件。
    ///
    /// 活跃任务（下载中/准备中）会被**自动暂停 → 改配置 → 自动恢复**，让新
    /// 线程数立即生效，无需用户手动操作；非活跃任务（暂停/错误/等待）只改
    /// 配置，下次恢复时生效。恢复时 coordinator 从 DB 复用全部现有分段布局
    /// （各段从 `start_byte + downloaded_bytes` 续传，零重复下载、不浪费一次性
    /// token），新分段数作为并发上限（worker_cap）：增大 → 对现有段做 IDM 式
    /// 对半拆分逐步 ramp 到新数；减小 → 限制并发但现有段仍全部处理；
    /// 0（自动）→ 复用现有段布局。
    ///
    /// 返回 `Ok(true)` 表示已更新；`Ok(false)` 表示任务不存在或已完成
    /// （已完成任务改线程数无意义）。
    ///
    /// `segments <= 0` 表示恢复为「自动」（交回 segment_advisor）。
    pub async fn set_task_segments(
        &mut self,
        task_id: &str,
        segments: i32,
    ) -> Result<bool, crate::db::DbError> {
        let task = match self.db.load_task_by_id(task_id).await? {
            Some(t) => t,
            None => return Ok(false),
        };
        // 已完成任务改线程数无意义，拒绝。
        if task.status == 3 {
            return Ok(false);
        }
        // 活跃 = 内存中有 spawn，或 DB 状态为下载中(1)/准备中(5)。
        let was_active = self.active_tasks.contains_key(task_id) || matches!(task.status, 1 | 5);
        let seg = if segments <= 0 { 0 } else { segments };

        // 活跃任务：先暂停当前 spawn（取消 + 落 paused），改配置后再恢复，
        // 让新 worker_cap 立即生效。全程在 current_thread actor 内串行，无竞态。
        // 静默暂停——这是实现细节，用户看到的是「改了线程数」，不是「暂停了」。
        if was_active {
            self.pause_task_keep_slot(task_id).await;
        }
        if let Err(e) = self.db.update_task_segments(task_id, seg).await {
            // 放弃恢复：补做暂停时被跳过的 drain，避免空槽位卡住排队任务。
            if was_active {
                self.drain_queue().await;
            }
            return Err(e);
        }
        log_info!(
            "[manager] task {} 分段数已改为 {}（进度保留，was_active={}）",
            task_id,
            seg,
            was_active
        );
        if was_active {
            self.resume_task(task_id).await;
        }
        Ok(true)
    }

    /// Get a reference to the current proxy configuration.
    pub fn proxy_config(&self) -> &ProxyConfig {
        &self.proxy_config
    }

    /// 防吸血宿主（引擎级，随 manager 存亡；会话级状态在 BT 会话建立时挂接）。
    #[cfg(feature = "bt")]
    pub fn bt_guard(&self) -> &Arc<crate::bt_guard::BtGuardHost> {
        &self.bt_guard
    }

    /// 热更新防吸血配置：不重建 BT 会话（受限队列速率、规则模式、云名单 / 订阅开关即时生效）。
    #[cfg(feature = "bt")]
    pub fn set_bt_guard_config(&self, config: crate::bt_guard::BtGuardConfig) {
        self.bt_guard.set_config(config);
    }

    /// Update global user-agent string.  Rebuilds the shared HTTP client so
    /// that all **new** downloads use the updated UA.  Already-running
    /// downloads keep their existing client and are unaffected.
    ///
    /// Empty string = revert to built-in Chrome UA.
    pub fn set_user_agent(&mut self, ua: String) -> Result<(), downloader::DownloadError> {
        log_info!("[manager] updating global_user_agent: {}", ua);
        let new_client = downloader::build_client(&self.proxy_config, &ua)?;
        self.client = new_client;
        self.invalidate_system_client();
        self.global_user_agent = ua;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Webhook 任务事件推送（免费自托管）
    // -----------------------------------------------------------------------

    /// 共享的 webhook 分发器句柄（宿主经 [`crate::Engine`] 的门面方法读取
    /// 投递日志 / 发测试）。
    pub fn webhook(&self) -> Arc<crate::webhook::WebhookDispatcher> {
        self.webhook.clone()
    }

    /// 从 config 表装载端点列表（`Engine::new` 调用一次）。
    pub async fn load_webhook_endpoints(&self) {
        let json = match self
            .db
            .get_config(crate::webhook::CONFIG_KEY_ENDPOINTS)
            .await
        {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                crate::logger::report_warning("download-manager", "load webhook endpoints", &error);
                return;
            }
        };
        self.webhook.reload_endpoints(&json);
    }

    /// 端点表热重载（宿主 `SaveConfig`/`ApplyConfig` 命中
    /// `webhook.endpoints` 时调用；解析失败保留旧表）。
    pub fn set_webhook_endpoints(&self, json: &str) {
        self.webhook.reload_endpoints(json);
    }

    /// 组装一条任务事件的 webhook 载荷。队列名取内存镜像，无需查库。
    ///
    /// `task.created` / `task.started` 早于探测，未指定文件名的任务此时 `file_name`
    /// 还是空的；按来源 URL 补一个临时名，否则预设模板的摘要行只剩空白。
    fn webhook_task_event(
        &self,
        kind: crate::webhook::WebhookEventKind,
        mut task: crate::webhook::WebhookTask,
        queue_id: &str,
    ) -> crate::webhook::WebhookEvent {
        if task.file_name.trim().is_empty() {
            task.file_name = webhook_provisional_file_name(&task.url);
        }
        crate::webhook::WebhookEvent::task(
            kind,
            task,
            queue_id.to_string(),
            self.queue_display_name(queue_id),
        )
    }

    /// 由 [`TaskInfo`] 直接构造事件（终态/暂停点位上任务行已在手）。
    /// `url` 取真实来源：`.torrent` 任务的 `url` 是 `torrent-file://local`
    /// 哨兵，推给用户毫无意义。
    fn webhook_event_from_task(
        &self,
        kind: crate::webhook::WebhookEventKind,
        task: &TaskInfo,
    ) -> crate::webhook::WebhookEvent {
        self.webhook_task_event(
            kind,
            crate::webhook::WebhookTask {
                id: task.task_id.clone(),
                file_name: task.file_name.clone(),
                url: if task.origin_url.is_empty() {
                    task.url.clone()
                } else {
                    task.origin_url.clone()
                },
                save_dir: task.save_dir.clone(),
                total_bytes: task.total_bytes,
                status: task.status,
                error_message: task.error_message.clone(),
            },
            &task.queue_id,
        )
    }

    fn queue_display_name(&self, queue_id: &str) -> String {
        self.queues
            .get(queue_id)
            .map(|q| q.name.clone())
            .unwrap_or_else(|| queue_id.to_string())
    }

    /// 所有语义事件触发点的唯一出口：照旧交给自托管 webhook，同时经 EventSink
    /// 发一条 [`EngineEvent::TaskNotice`] 供 agent 做云端推送（宿主不关心则忽略）。
    /// 返回值沿用 [`crate::webhook::WebhookDispatcher::emit`]（投出的端点数）。
    fn emit_task_event(&self, event: crate::webhook::WebhookEvent) -> usize {
        let notice = crate::webhook::TaskNotice::from_event(&event);
        let delivered = self.webhook.emit(event);
        self.sink.emit(EngineEvent::TaskNotice(notice));
        delivered
    }

    /// 登记某队列已占用；若它此前空闲（且不是启动恢复）则发一条 `queue.started`。
    ///
    /// `queue.started` 与 `queue.drained` 构成一对边沿：同一次忙碌区间只发一次，
    /// 之后到清空为止的入队/恢复都不会再触发。
    fn mark_queue_occupied(&mut self, queue_id: &str) {
        if self.occupied_queues.insert(queue_id.to_string()) {
            self.emit_queue_started(queue_id.to_string());
        }
    }

    /// 发 `queue.started`；启动恢复期间静默（重启接回的占用不算新开始）。
    fn emit_queue_started(&self, queue_id: String) {
        if self.restoring_on_start {
            return;
        }
        let name = self.queue_display_name(&queue_id);
        self.emit_task_event(crate::webhook::WebhookEvent::queue_started(queue_id, name));
    }

    /// 重算每个队列的占用情况：对**由空转为占用**的队列发一条 `queue.started`，
    /// 对**由占用转为空**的队列发一条 `queue.drained`。
    ///
    /// 必须在任务**进入**（`enqueue_persisted_task` / `resume_task_inner`）与
    /// **离开**（`on_task_done` / `pause_task` / `delete_task`）活跃·待启动集合
    /// 的每个汇合点调用：两侧配合构成边沿触发，同一次忙碌/空闲不会重复通知。
    /// 已排程自动重试的任务视为仍占用，否则退避间隙会误报一次清空。
    fn sync_queue_occupancy(&mut self) {
        let mut occupied: HashSet<String> = self
            .active_tasks
            .values()
            .map(|e| e.queue_id.clone())
            .collect();
        occupied.extend(self.pending_queue.queue_ids().cloned());
        occupied.extend(self.retry_scheduled.values().cloned());
        let drained: Vec<String> = self
            .occupied_queues
            .difference(&occupied)
            .cloned()
            .collect();
        let started: Vec<String> = occupied
            .difference(&self.occupied_queues)
            .cloned()
            .collect();
        self.occupied_queues = occupied;
        for queue_id in drained {
            let name = self.queue_display_name(&queue_id);
            self.emit_task_event(crate::webhook::WebhookEvent::queue_drained(queue_id, name));
        }
        for queue_id in started {
            self.emit_queue_started(queue_id);
        }
    }

    // -----------------------------------------------------------------------
    // Concurrency helpers
    // -----------------------------------------------------------------------

    /// Lazily initialise the shared BT session.  Returns an error if the
    /// session cannot be created (e.g. port in use).
    ///
    /// After calling this, `self.bt_session` is guaranteed to be `Some`.
    /// Callers should access `self.bt_session.as_ref()` afterwards to avoid
    /// borrow-checker issues with `&mut self`.
    ///
    /// The session is created on a blocking thread via `spawn_blocking`
    /// because `SharedBtSession::new` internally calls `Runtime::block_on`,
    /// which cannot be invoked from within an existing tokio runtime.
    async fn ensure_bt_session(&mut self) -> Result<(), downloader::DownloadError> {
        if !self.bt_config.enabled {
            return Err(downloader::DownloadError::Other(
                BT_DISABLED_MESSAGE.to_string(),
            ));
        }
        #[cfg(feature = "bt")]
        {
            // 任何经此入口的 BT 活动（新下载 / 恢复 / 重新挂载做种）都结束「仅暂停
            // 任务保活」的空闲期，下一次空闲重新起算完整宽限。
            self.bt_paused_idle_since = None;
            if self.bt_session.is_none() {
                let speed_limit = self.speed_limiter.limit();
                let upload_limit = self.upload_limit_bps;
                let save_dir = self.default_save_dir.clone();
                let data_dir = self.app_data_dir.clone();
                let config = self.bt_config.clone();
                let policy = self.bt_guard.policy();
                let session = tokio::task::spawn_blocking(move || {
                    SharedBtSession::new(
                        &save_dir,
                        &data_dir,
                        speed_limit,
                        upload_limit,
                        &config,
                        Some(policy),
                    )
                })
                .await
                .map_err(|e| {
                    downloader::DownloadError::Other(format!(
                        "BT session init thread panicked: {e}"
                    ))
                })??;
                let session = Arc::new(session);
                // 新会话的 SeedingManager 以当前配置的活动做种上限起步。
                session
                    .seeding_manager()
                    .set_cap(self.bt_config.seed_max_active);
                self.bt_guard
                    .attach(&session.session(), session.closing_token());
                self.bt_session = Some(session);
            }
            Ok(())
        }
        // 无 BT 后端：`bt_config.enabled` 恒为 false（见 `BtConfig::effective`），上面已返回。
        #[cfg(not(feature = "bt"))]
        {
            Ok(())
        }
    }

    /// Update BT configuration. Runtime-read settings (seeding limits, the
    /// active-seeder cap, the extra-tracker list for new torrents) take effect
    /// immediately; listener-level settings (ports, DHT, UPnP, uTP, MSE) take
    /// effect when the session is rebuilt by `invalidate_bt_session`.
    pub fn set_bt_config(&mut self, config: BtConfig) {
        // 活动做种数上限热生效：直接写入 SeedingManager，队列在下一次
        // 做种求值 tick 的 reconcile 中被重新平衡。追加 tracker 同样热生效于
        // 此后加入的种子（对齐 qBittorrent「自动添加到新下载」），不重建会话。
        #[cfg(feature = "bt")]
        if let Some(ref bt) = self.bt_session {
            bt.seeding_manager().set_cap(config.seed_max_active);
            bt.set_extra_trackers(&config);
        }
        self.bt_config = config.effective();
    }

    /// Periodically drive the seeding lifecycle:
    /// 1. sample live uploads and persist every seeder's unpersisted upload,
    /// 2. rebalance active seeders against `seed_max_active`（promote/demote）——
    ///    放在落库之后，降级者已采样的上传不会随条目离开活动表而丢失，
    /// 3. accumulate seeding time in memory — it reaches the DB only alongside
    ///    upload deltas, on seeding state transitions / shutdown, or via the
    ///    30-minute fallback (see [`seed_times_persist_due`]),
    /// 4. stop seeders that reached the configured limits,
    /// 5. release the BT session once only paused incomplete tasks have held
    ///    it for [`BT_SESSION_PAUSED_IDLE_GRACE`].
    ///
    /// 实时上传进度帧由 [`Self::tick_seeding_upload`] 按更快的节奏发出。
    /// This is a cheap no-op (zero I/O) when no BT session exists or nothing seeds.
    #[cfg(feature = "bt")]
    pub async fn tick_seeding_evaluation(&mut self) {
        let rows = self.account_seeding_tick().await;
        self.reconcile_seeding_slots().await;
        let to_stop = self.evaluate_seeding_limits(&rows).await;
        let had_stops = !to_stop.is_empty();
        let then_action =
            crate::bt_seeding::SeedingThenAction::parse(&self.bt_config.seed_then_action);
        for (task_id, reason) in to_stop {
            let short = &task_id[..task_id.len().min(8)];
            log_info!("[manager] stopping seeder {}: {}", short, reason.message());

            let bt = self.bt_session.clone();
            if let Some(bt) = bt {
                if let Err(error) = bt.pause_task(&task_id).await {
                    crate::logger::report_error("download-manager", "stop seeding task", &error);
                    continue;
                }
                if let Some(seed) = bt.unregister_seeder(&task_id).await {
                    // 停止即结算：把本 stint 的做种时长折进累计值。
                    if let Err(error) = self
                        .db
                        .set_task_seeding_time(&task_id, seed.seed_time_secs)
                        .await
                    {
                        crate::logger::report_error(
                            "download-manager",
                            "persist task state",
                            &error,
                        );
                    }
                }
            }

            if let Ok(Some(t)) = self.db.load_task_by_id(&task_id).await {
                match then_action {
                    crate::bt_seeding::SeedingThenAction::DeleteTask => {
                        // 行即将删除，不写只会随行消失的停止原因。
                        self.delete_task(&task_id, false).await;
                        continue;
                    }
                    crate::bt_seeding::SeedingThenAction::DeleteTaskAndFiles => {
                        self.delete_task(&task_id, true).await;
                        continue;
                    }
                    crate::bt_seeding::SeedingThenAction::Stop => {
                        if let Err(error) = self
                            .db
                            .update_task_seeding_status(&task_id, reason.as_i32(), reason.message())
                            .await
                        {
                            crate::logger::report_error(
                                "download-manager",
                                "persist task state",
                                &error,
                            );
                            continue;
                        }
                        self.sink.emit(EngineEvent::TaskProgress {
                            task_id: task_id.clone(),
                            status: 3,
                            downloaded_bytes: t.downloaded_bytes,
                            total_bytes: t.total_bytes,
                            speed: 0,
                            file_name: t.file_name.clone(),
                            save_dir: t.save_dir.clone(),
                            url: t.url.clone(),
                            error_message: String::new(),
                            upload_speed_bps: 0,
                            uploaded_bytes: t.uploaded_bytes,
                            seeding_status: reason.as_i32(),
                            seeding_message: reason.message().to_string(),
                            seeding_time_secs: t.seeding_time_secs,
                        });
                    }
                }
            }
        }
        // 停止释放了槽位——立即再平衡，让排队的做种者补位。
        if had_stops {
            self.reconcile_seeding_slots().await;
        }
        // 会话空闲宽限的周期复查点：复用本 tick，不新增宿主的 select 分支。
        self.recheck_bt_session_paused_grace().await;
    }

    /// 无 BT 后端：没有做种者，周期求值为空操作。
    #[cfg(not(feature = "bt"))]
    pub async fn tick_seeding_evaluation(&mut self) {}

    /// Rebalance active seeders against `seed_max_active`: promote queued
    /// seeds while slots are free (unpause + persist + notify) and park
    /// over-cap seeders back into the queue (pause + persist + notify).
    #[cfg(feature = "bt")]
    async fn reconcile_seeding_slots(&self) {
        let Some(ref bt) = self.bt_session else {
            return;
        };
        let mgr = bt.seeding_manager();
        mgr.set_cap(self.bt_config.seed_max_active);
        let (activated, demoted) = mgr.reconcile().await;
        for task_id in activated {
            if let Err(e) = bt.resume_task(&task_id).await {
                // unpause 失败不得谎报做种中：回滚注册、结算时长、保持停止
                // 态（用户可再次手动恢复），并把槽位让给下一次 reconcile。
                log_info!(
                    "[manager] seeding promote {}: unpause failed: {}",
                    &task_id[..task_id.len().min(8)],
                    e
                );
                if let Some(seed) = mgr.unregister(&task_id).await
                    && let Err(error) = self
                        .db
                        .set_task_seeding_time(&task_id, seed.seed_time_secs)
                        .await
                {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                }
                if let Err(error) = self
                    .db
                    .update_task_seeding_status(
                        &task_id,
                        SeedingStopReason::UserStopped.as_i32(),
                        "seed resume failed",
                    )
                    .await
                {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                    continue;
                }
                self.emit_progress_from_db(
                    &task_id,
                    3,
                    SeedingStopReason::UserStopped.as_i32(),
                    "seed resume failed",
                    0,
                )
                .await;
                continue;
            }
            if let Err(error) = self
                .db
                .set_task_seeding_active(&task_id, chrono::Local::now().timestamp())
                .await
            {
                crate::logger::report_error("download-manager", "persist seeding state", &error);
                if let Err(rollback_error) =
                    rollback_seeding_registration(bt, &self.db, &task_id).await
                {
                    crate::logger::report_error(
                        "download-manager",
                        "roll back seeding activation",
                        &rollback_error,
                    );
                }
                continue;
            }
            self.emit_progress_from_db(&task_id, 3, SEEDING_STATUS_ACTIVE, "", 0)
                .await;
        }
        for (task_id, folded_secs) in demoted {
            if let Err(error) = bt.pause_task(&task_id).await {
                crate::logger::report_error("download-manager", "pause seeding task", &error);
                continue;
            }
            if let Err(error) = self.db.set_task_seeding_time(&task_id, folded_secs).await {
                crate::logger::report_error("download-manager", "persist task state", &error);
            }
            if let Err(error) = self.db.set_task_seeding_queued(&task_id).await {
                crate::logger::report_error("download-manager", "persist task state", &error);
                continue;
            }
            self.emit_progress_from_db(
                &task_id,
                3,
                SEEDING_STATUS_QUEUED,
                SEEDING_QUEUED_MESSAGE,
                0,
            )
            .await;
        }
    }

    /// 本 tick 是否该把做种时长快照一并落库：有上传增量（同事务顺带写）或距上次
    /// 落库满 [`SEED_TIME_FALLBACK_PERSIST_INTERVAL`]。纯判定见
    /// [`seed_times_persist_due`]；落库时刻由写库成功后的
    /// [`Self::mark_seed_times_persisted`] 登记，写库失败时下个 tick 会重试。
    #[cfg(feature = "bt")]
    fn seed_times_due(&self, has_upload_delta: bool) -> bool {
        let last = *self
            .last_seed_time_persist
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        seed_times_persist_due(
            has_upload_delta,
            std::time::Instant::now(),
            last,
            SEED_TIME_FALLBACK_PERSIST_INTERVAL,
        )
    }

    /// 登记做种时长刚刚落库（重置兜底计时）。
    #[cfg(feature = "bt")]
    fn mark_seed_times_persisted(&self) {
        *self
            .last_seed_time_persist
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = std::time::Instant::now();
    }

    /// 正常关机结算：把所有活动做种者已采样未落库的上传与内存累计做种时长落库。
    /// 做种时长平时不按固定周期落库（见 [`SEED_TIME_FALLBACK_PERSIST_INTERVAL`]），
    /// 关机是除状态迁移外最后的落库机会；崩溃/断电最多丢失一个兜底间隔的时长
    /// 与一个 [`crate::bt_seeding::SEEDING_EVAL_INTERVAL`] 的上传。
    #[cfg(feature = "bt")]
    async fn flush_seed_times(&self) {
        let Some(bt) = &self.bt_session else {
            return;
        };
        let seeding_mgr = bt.seeding_manager();
        let uploads = seeding_mgr.unpersisted_uploads().await;
        let times = seeding_mgr.seed_time_snapshot().await;
        if uploads.is_empty() && times.is_empty() {
            return;
        }
        match self.db.apply_seeding_tick(&uploads, &times).await {
            Ok(_) => {
                seeding_mgr.mark_uploads_persisted(&uploads).await;
                self.mark_seed_times_persisted();
            }
            Err(e) => log_info!("[manager] shutdown: persist seeding state error: {}", e),
        }
    }

    /// 读每个活动做种者的 librqbit 上传计数，把增量计入其未落库累计。
    /// 返回 `(任务 id, 实时上传速率 B/s)`；暂停中（无 live 快照）的做种者跳过。
    #[cfg(feature = "bt")]
    async fn sample_seeder_uploads(
        seeding_mgr: &crate::bt_seeding::SeedingManager,
        task_ids: &[String],
    ) -> Vec<(String, i64)> {
        let mut observed = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            let Some(handle) = seeding_mgr.get_handle(task_id).await else {
                continue;
            };
            let stats = handle.stats();
            let Some(live) = stats.live.as_ref() else {
                // 暂停期间没有 live 快照——不能用 0 覆盖。
                continue;
            };
            let snapshot_uploaded = live.snapshot.uploaded_bytes as i64;
            let upload_speed_bps = (live.upload_speed.mbps * 1024.0 * 1024.0) as i64;
            if seeding_mgr
                .apply_upload_snapshot(task_id, snapshot_uploaded, upload_speed_bps)
                .await
                .is_some()
            {
                observed.push((task_id.clone(), upload_speed_bps));
            }
        }
        observed
    }

    /// 做种实时上传帧（每 [`crate::bt_seeding::SEEDING_UPLOAD_SAMPLE_INTERVAL`]）：
    /// 采样各活动做种者的上传，为每个做种者发一条带实时上传速率、累计上传
    /// （已落库 + 未落库）与内存实时做种时长的进度事件。宿主的总上传速率由
    /// 相邻帧的累计增量折算，帧间隔必须与下载进度同为 500ms 级。
    ///
    /// 不写库；任务行事实取自 [`Self::account_seeding_tick`] 刷新的缓存，仅对
    /// 首次出现的做种者批量读一次库。无会话或无做种者时零 I/O。文件名 / 目录 /
    /// 地址留空（各端投影按「空 = 不变」处理）。
    #[cfg(feature = "bt")]
    pub async fn tick_seeding_upload(&self) {
        let Some(ref bt) = self.bt_session else {
            return;
        };
        let seeding_mgr = bt.seeding_manager();
        let task_ids = seeding_mgr.active_task_ids().await;
        let missing: Vec<String> = {
            let mut cache = self
                .seed_row_totals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.retain(|id, _| task_ids.contains(id));
            task_ids
                .iter()
                .filter(|id| !cache.contains_key(*id))
                .cloned()
                .collect()
        };
        if task_ids.is_empty() {
            return;
        }
        if !missing.is_empty() {
            match self.db.load_tasks_by_ids(&missing).await {
                Ok(tasks) => {
                    let mut cache = self
                        .seed_row_totals
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    for task in &tasks {
                        cache.insert(task.task_id.clone(), SeedRowTotals::of(task));
                    }
                }
                Err(e) => log_info!("[manager] seeding upload sample: load rows error: {}", e),
            }
        }

        let observed = Self::sample_seeder_uploads(&seeding_mgr, &task_ids).await;
        let pending: HashMap<String, i64> = seeding_mgr
            .unpersisted_uploads()
            .await
            .into_iter()
            .collect();
        let times: HashMap<String, i64> =
            seeding_mgr.seed_time_snapshot().await.into_iter().collect();
        let cache = self
            .seed_row_totals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (task_id, upload_speed_bps) in observed {
            let Some(row) = cache.get(&task_id) else {
                continue;
            };
            let uploaded_bytes = row
                .uploaded_bytes
                .saturating_add(pending.get(&task_id).copied().unwrap_or(0));
            let seeding_time_secs = times.get(&task_id).copied().unwrap_or(0);
            self.sink.emit(EngineEvent::TaskProgress {
                task_id,
                status: 3,
                downloaded_bytes: row.downloaded_bytes,
                total_bytes: row.total_bytes,
                speed: 0,
                file_name: String::new(),
                save_dir: String::new(),
                url: String::new(),
                error_message: String::new(),
                upload_speed_bps,
                uploaded_bytes,
                seeding_status: 1,
                seeding_message: String::new(),
                seeding_time_secs,
            });
        }
    }

    /// 无 BT 后端：没有做种者，上传采样为空操作。
    #[cfg(not(feature = "bt"))]
    pub async fn tick_seeding_upload(&self) {}

    /// 做种 tick 的统计与落库：先采样一次，再把所有活动做种者未落库的上传合并为
    /// **一个事务**写库，做种时长快照仅在本轮本来就要写库或兜底到点时顺带写入
    /// 同一事务；做种者的任务行用**一次批量读取**取回，并刷新
    /// [`Self::tick_seeding_upload`] 的行缓存。本函数不发进度帧。
    ///
    /// 上传累计用增量法，`tasks.uploaded_bytes` 在 librqbit 计数复位（暂停/恢复或
    /// 会话重建）后仍然正确；写库失败的增量留在内存，下一 tick 重试。
    /// 零上传且未到兜底点时整个 tick 不产生 DB 写（NAS 上休眠的 HDD 不被唤醒）。
    /// 返回的任务行已折入本轮写入的累计上传与内存实时做种时长，供限额求值直接使用。
    #[cfg(feature = "bt")]
    async fn account_seeding_tick(&self) -> HashMap<String, TaskInfo> {
        let Some(ref bt) = self.bt_session else {
            return HashMap::new();
        };
        let seeding_mgr = bt.seeding_manager();
        let task_ids = seeding_mgr.active_task_ids().await;
        if task_ids.is_empty() {
            return HashMap::new();
        }

        let mut rows: HashMap<String, TaskInfo> = match self.db.load_tasks_by_ids(&task_ids).await {
            Ok(tasks) => tasks.into_iter().map(|t| (t.task_id.clone(), t)).collect(),
            Err(e) => {
                log_info!("[manager] seeding tick: load_tasks_by_ids error: {}", e);
                return HashMap::new();
            }
        };

        Self::sample_seeder_uploads(&seeding_mgr, &task_ids).await;
        let deltas: Vec<(String, i64)> = seeding_mgr
            .unpersisted_uploads()
            .await
            .into_iter()
            .filter(|(id, _)| rows.contains_key(id))
            .collect();
        // 实时累计做种时长取内存值（`SeedingEntry` 里的墙钟累计）：UI 进度事件与
        // 限额求值都读这份，不依赖落库频率；DB 行里的值只是上次落库时的快照。
        let time_snapshot = seeding_mgr.seed_time_snapshot().await;
        for (id, secs) in &time_snapshot {
            if let Some(row) = rows.get_mut(id) {
                row.seeding_time_secs = *secs;
            }
        }
        // 时长只在「本 tick 本来就要写上传增量」或兜底到点时落库；零上传的空闲
        // 做种既无增量也未到点，`apply_seeding_tick` 对空输入直接返回、不碰 DB。
        let persist_times = self.seed_times_due(!deltas.is_empty());
        let times: &[(String, i64)] = if persist_times { &time_snapshot } else { &[] };
        match self.db.apply_seeding_tick(&deltas, times).await {
            Ok(totals) => {
                seeding_mgr.mark_uploads_persisted(&deltas).await;
                if persist_times {
                    self.mark_seed_times_persisted();
                }
                for (id, total) in totals {
                    if let Some(row) = rows.get_mut(&id) {
                        row.uploaded_bytes = total;
                    }
                }
            }
            Err(e) => log_info!("[manager] seeding tick persist error: {}", e),
        }

        *self
            .seed_row_totals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = rows
            .iter()
            .map(|(id, row)| (id.clone(), SeedRowTotals::of(row)))
            .collect();
        rows
    }

    /// Evaluate configured seeding limits for every active seeder.
    ///
    /// Uses the persisted cumulative `uploaded_bytes` / `downloaded_bytes` /
    /// `total_bytes` from the DB row so ratio limits are not under-counted
    /// across librqbit session resets. Per-task overrides（跟随全局/不限/
    /// 自定义）在此处解析为生效配置；组合方式与达标动作恒为全局值。
    #[cfg(feature = "bt")]
    async fn evaluate_seeding_limits(
        &self,
        rows: &HashMap<String, TaskInfo>,
    ) -> Vec<(String, SeedingStopReason)> {
        let Some(ref bt) = self.bt_session else {
            return Vec::new();
        };

        let global = SeedingLimitConfig {
            ratio_limit: self.bt_config.seed_ratio_limit,
            post_ratio_limit: self.bt_config.seed_post_ratio_limit,
            seed_time_limit_minutes: self.bt_config.seed_time_limit_minutes,
            inactive_time_limit_minutes: self.bt_config.seed_inactive_time_limit_minutes,
            operator: self.bt_config.seed_limit_operator,
            then_action: crate::bt_seeding::SeedingThenAction::parse(
                &self.bt_config.seed_then_action,
            ),
        };

        let seeding_mgr = bt.seeding_manager();
        let task_ids = seeding_mgr.active_task_ids().await;
        if task_ids.is_empty() {
            return Vec::new();
        }
        // 手动恢复（强制做种）的任务不受任何限制约束，只刷新上传活动。
        let exempt = match self.db.list_seed_limits_exempt_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load seed limits exemption",
                    &error,
                );
                HashSet::new()
            }
        };

        // Build per-task effective configs and live snapshots from DB totals.
        let mut resolved: HashMap<String, (SeedingLimitConfig, SeedingUploadSnapshot)> =
            HashMap::new();
        for task_id in &task_ids {
            let Some(handle) = seeding_mgr.get_handle(task_id).await else {
                continue;
            };
            let stats = handle.stats();
            let upload_speed_bps = stats
                .live
                .as_ref()
                .map(|l| (l.upload_speed.mbps * 1024.0 * 1024.0) as i64)
                .unwrap_or(0);

            let Some(t) = rows.get(task_id) else {
                continue;
            };

            let overrides = SeedLimitOverrides {
                ratio_limit_milli: t.seed_ratio_limit_milli,
                post_ratio_limit_milli: t.seed_post_ratio_limit_milli,
                seed_time_limit_minutes: t.seed_time_limit_minutes,
                inactive_time_limit_minutes: t.seed_inactive_time_limit_minutes,
            };
            resolved.insert(
                task_id.clone(),
                (
                    if exempt.contains(task_id) {
                        SeedingLimitConfig::default()
                    } else {
                        overrides.apply(&global)
                    },
                    SeedingUploadSnapshot {
                        total_uploaded: t.uploaded_bytes,
                        total_downloaded: t.downloaded_bytes,
                        total_size: t.total_bytes,
                        upload_speed_bps,
                    },
                ),
            );
        }

        seeding_mgr
            .evaluate_limits(|id| {
                resolved.get(id).copied().unwrap_or((
                    SeedingLimitConfig::default(),
                    SeedingUploadSnapshot::default(),
                ))
            })
            .await
    }

    /// 写入任务级做种限制覆盖（哨兵：-2 跟随全局、-1 不限、>=0 自定义；
    /// 比率为千分比）。比率/时长热生效：下一次做种求值 tick 即按新值判定，
    /// 无需重建会话或重新注册做种者。`upload_limit_bps`（B/s，0 = 不限）
    /// 在下一次 torrent add 时烘焙生效（恢复下载 / 重启续种 / 重新挂载）。
    pub async fn set_task_seed_limits(
        &self,
        task_id: &str,
        ratio_limit_milli: i64,
        post_ratio_limit_milli: i64,
        seed_time_limit_minutes: i64,
        inactive_time_limit_minutes: i64,
        upload_limit_bps: i64,
    ) {
        if let Err(e) = self
            .db
            .set_task_seed_limits(
                task_id,
                ratio_limit_milli,
                post_ratio_limit_milli,
                seed_time_limit_minutes,
                inactive_time_limit_minutes,
                upload_limit_bps,
            )
            .await
        {
            log_info!("[manager] set_task_seed_limits {}: {}", task_id, e);
        }
    }

    /// 用最新 `bt_config` 重建共享会话，或在 BT 被禁用时释放它。只有监听类
    /// 设置（端口 / DHT / UPnP / uTP / MSE）与启用开关走这里；tracker 列表经
    /// [`Self::set_bt_config`] 热生效，不重建。
    ///
    /// 对齐 qBittorrent「改设置不打断任务」：BT 仍启用时，重建前在跑的 BT 下载
    /// 在新会话上自动继续；做种中 / 排队做种 / 正在校验挂载的任务从磁盘重新
    /// 挂载继续做种。BT 被禁用时下载落回暂停（status 2），做种落「会话释放」
    /// 停止态。
    #[cfg(feature = "bt")]
    pub async fn invalidate_bt_session(&mut self) {
        // BT 刚被禁用时，排队中的 BT 项立即落回暂停（不必等下一次 drain）。
        self.park_pending_bt_tasks().await;
        if self.bt_session.is_none() {
            return;
        }
        let rebuild = self.bt_config.enabled;
        // 已采样未落库的上传与做种时长先落库，随后的注销不丢数据。
        self.flush_seed_times().await;
        let Some(bt) = self.bt_session.take() else {
            return;
        };

        // 1. 宣布关停：在途的重新挂载（启动续种 / 恢复做种的本地数据校验）
        //    不再向旧会话登记。先读在途挂载、再读做种表——挂载任务完成做种
        //    登记后才注销在途登记，按此顺序读取不会漏掉任何任务。
        bt.closing_token().cancel();
        let mut seeders = bt.reseeding_task_ids();
        for id in bt.seeding_manager().all_task_ids().await {
            if !seeders.contains(&id) {
                seeders.push(id);
            }
        }
        let released = crate::bt_seeding::SeedingStopReason::SessionReleased;
        for tid in &seeders {
            // 结算本 stint 的做种时长。
            if let Some(seed) = bt.unregister_seeder(tid).await
                && let Err(error) = self
                    .db
                    .set_task_seeding_time(tid, seed.seed_time_secs)
                    .await
            {
                crate::logger::report_error("download-manager", "persist task state", &error);
            }
            // 落「会话释放」：禁用时就是终态；重建时作为重新挂载失败的回退停止态
            // （挂载开始即改写为排队做种「校验中」，界面不闪停止）。
            if let Err(error) = self
                .db
                .update_task_seeding_status(tid, released.as_i32(), released.message())
                .await
            {
                crate::logger::report_error("download-manager", "persist task state", &error);
                continue;
            }
            if !rebuild {
                self.emit_progress_from_db(tid, 3, released.as_i32(), released.message(), 0)
                    .await;
            }
        }

        // 2. Gracefully pause each active BT download (cancel token, persist
        //    progress, update DB status to paused, notify the host).
        let bt_task_ids: Vec<String> = self
            .active_tasks
            .iter()
            .filter(|(_, e)| e.is_bt)
            .map(|(id, _)| id.clone())
            .collect();
        let mut paused_downloads: Vec<String> = Vec::with_capacity(bt_task_ids.len());
        if !bt_task_ids.is_empty() {
            log_info!(
                "[manager] pausing {} active BT task(s) before session invalidation",
                bt_task_ids.len()
            );
            for tid in &bt_task_ids {
                let Some(entry) = self.active_tasks.remove(tid) else {
                    continue;
                };
                entry.token.cancel();
                // Pause the torrent handle in the session so librqbit
                // flushes its piece-level state to disk.
                if let Err(error) = bt.pause_task(tid).await {
                    crate::logger::report_error("download-manager", "pause BT task", &error);
                }
                if let Err(error) = self.db.update_task_status(tid, 2, "").await {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                    continue;
                }
                paused_downloads.push(tid.clone());
                if let Ok(Some(t)) = self.db.load_task_by_id(tid).await {
                    self.sink.emit(EngineEvent::TaskProgress {
                        task_id: tid.clone(),
                        status: 2,
                        downloaded_bytes: t.downloaded_bytes,
                        total_bytes: t.total_bytes,
                        speed: 0,
                        file_name: t.file_name.clone(),
                        save_dir: t.save_dir.clone(),
                        url: t.url.clone(),
                        error_message: String::new(),
                        upload_speed_bps: 0,
                        uploaded_bytes: t.uploaded_bytes,
                        seeding_status: t.seeding_status,
                        seeding_message: t.seeding_message.clone(),
                        seeding_time_secs: t.seeding_time_secs,
                    });
                    self.send_segments_from_db(tid, t.total_bytes).await;
                }
                // Boost guard: if the paused task was the current priority
                // (Boost) target, cancel Boost and resume other tasks.
                if self.priority_task_id.as_deref() == Some(tid.as_str()) {
                    self.clear_priority().await;
                }
            }
            // Give in-flight BT download loops a moment to detect
            // cancellation and exit cleanly before we tear down the session.
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        }

        // 2b. 等待仍在进行的 detached `add_torrent` 任务（如 magnet 的 DHT
        //     元数据解析）结束后再关停，与 `maybe_release_bt_session` 的
        //     inflight 检查对齐；超时则强制关停（关停会取消会话、释放端口）。
        let deadline = tokio::time::Instant::now() + INVALIDATE_INFLIGHT_TIMEOUT;
        while bt.has_inflight_adds() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(INVALIDATE_INFLIGHT_POLL_INTERVAL).await;
        }
        if bt.has_inflight_adds() {
            log_info!(
                "[manager] invalidate: inflight add_torrent still pending after timeout, forcing shutdown"
            );
        }

        // 3. 关停旧会话：暂停全部 torrent 落盘并取消 librqbit 会话（释放监听
        //    端口）。最后一个 `Arc` 在独立线程释放，runtime 有时间收尾。
        let carry_over = rebuild && (!seeders.is_empty() || !paused_downloads.is_empty());
        log_info!(
            "[manager] {} BT session for config change ({} seeder(s), {} download(s))",
            if carry_over {
                "rebuilding"
            } else {
                "releasing"
            },
            seeders.len(),
            paused_downloads.len()
        );
        if !carry_over {
            // 没有要接回的任务：新会话在下一次 BT 活动时惰性创建。
            std::thread::spawn(move || bt.shutdown());
            return;
        }
        // 旧会话的引用在纯同步线程释放并等它结束：它是最后一个引用时，旧 runtime
        // 限时收尾后 librqbit 会话连同 uTP 套接字一起释放，新会话能绑回同一端口。
        let released_old = tokio::task::spawn_blocking(move || {
            bt.shutdown();
            std::thread::spawn(move || drop(bt)).join().is_ok()
        })
        .await;
        match released_old {
            Ok(true) => {}
            Ok(false) => log_info!("[manager] rebuild BT session: old session drop panicked"),
            Err(error) => {
                crate::logger::report_error("download-manager", "shut down BT session", &error);
                return;
            }
        }

        // 4. 新配置建会话，把重建前的做种与下载接回去。
        if let Err(e) = self.ensure_bt_session().await {
            log_info!(
                "[manager] rebuild BT session: init failed, seeders stay released: {}",
                e
            );
            for tid in &seeders {
                self.emit_progress_from_db(tid, 3, released.as_i32(), released.message(), 0)
                    .await;
            }
            return;
        }
        let Some(bt) = self.bt_session.clone() else {
            return;
        };
        match self.db.load_tasks_by_ids(&seeders).await {
            Ok(tasks) => {
                for task in tasks.iter().filter(|task| task.status == 3) {
                    self.spawn_reseed_from_disk(bt.clone(), task).await;
                }
            }
            Err(error) => crate::logger::report_error(
                "download-manager",
                "load released seeders for reseed",
                &error,
            ),
        }
        self.batch_resume(&paused_downloads).await;
    }

    /// 无 BT 后端：没有会话可释放；仅把排队中的 BT 项落回暂停（沿用禁用路径）。
    #[cfg(not(feature = "bt"))]
    pub async fn invalidate_bt_session(&mut self) {
        self.park_pending_bt_tasks().await;
    }

    /// 立即发布队列顺序；用户批量操作不等待合并节拍。只发相对上次已发布顺序的差异
    /// （队首出队只带被移除的任务），首次与用户调序时发全量。
    fn broadcast_queue_positions(&mut self) {
        self.queue_positions_dirty = false;
        match self
            .queue_order
            .diff(self.pending_queue.iter().map(|q| q.task_id.as_str()))
        {
            QueueOrderDelta::Unchanged => {}
            QueueOrderDelta::Full(positions) => self
                .sink
                .emit(EngineEvent::QueuePositionsChanged(positions)),
            QueueOrderDelta::Incremental { removed, added } => self
                .sink
                .emit(EngineEvent::QueueOrderChanged { removed, added }),
        }
    }

    /// 宿主每 250ms 调用；只有排队顺序可能变化时才做差异计算与发送。
    pub fn flush_queue_positions(&mut self) {
        if self.queue_positions_dirty {
            self.broadcast_queue_positions();
        }
    }

    /// Load all named queues from the database into the in-memory cache.
    /// Must be called once after the manager is created (before the event loop).
    pub async fn load_queues(&mut self) {
        match self.db.load_all_queues().await {
            Ok(qs) => {
                self.queues.clear();
                for q in qs {
                    // Sync the limiter if one already exists.
                    if let Some(limiter) = self.queue_limiters.get(&q.queue_id) {
                        limiter.set_limit(self.queue_limit_bps(q.speed_limit_kbps));
                    }
                    self.queues.insert(q.queue_id.clone(), q);
                }
            }
            Err(e) => log_info!("[manager] load_queues error: {}", e),
        }
    }
    /// 该世代的任务是否正挂起等待「文件已存在」答复（不占并发槽）。
    fn is_awaiting_decision(&self, task_id: &str, generation: u64) -> bool {
        self.awaiting_decision.get(task_id) == Some(&generation)
    }

    /// 任务是否「采纳」了已有文件（逐任务决定为跳过）：采纳的文件不归任务所有。
    /// 读库失败时保守返回 true（宁可留下文件也不误删用户数据）。
    async fn skip_adopted(&self, task_id: &str) -> bool {
        match self.db.get_exists_decision(task_id).await {
            Ok(value) => {
                crate::file_exists::ExistsDecision::from_db(&value)
                    == crate::file_exists::ExistsDecision::Skip
            }
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load file-exists decision",
                    &error,
                );
                true
            }
        }
    }

    /// 全局 overwrite 策略对应的覆盖授权。
    fn global_overwrite_policy(&self) -> crate::file_exists::OverwritePolicy {
        if matches!(self.file_exists_behavior, FileExistsBehavior::Overwrite) {
            crate::file_exists::OverwritePolicy::Any
        } else {
            crate::file_exists::OverwritePolicy::Never
        }
    }

    /// 非 BT 任务是否从未有过任何落盘进度：无已下载字节、无分段行、无本协议的
    /// 临时产物（`<name>.fdownloading`、DASH/HLS 音轨临时、ED2K 旧版临时名），
    /// 且未完成。读库/探盘出错时保守视为已起跑，沿用续传语义。
    async fn never_started(&self, task: &TaskInfo, has_audio_track: bool) -> bool {
        if task.status == 3 || task.downloaded_bytes != 0 {
            return false;
        }
        match self.db.load_segments(&task.task_id).await {
            Ok(rows) if rows.is_empty() => {}
            Ok(_) => return false,
            Err(error) => {
                crate::logger::report_error("download-manager", "load segments", &error);
                return false;
            }
        }
        if task.file_name.is_empty() {
            return true;
        }
        let Some(target) = task_target_path(&task.save_dir, &task.file_name) else {
            return false;
        };
        let temp_ext = downloader::TEMP_EXT;
        let mut temps = vec![PathBuf::from(format!("{}{}", target.display(), temp_ext))];
        if has_audio_track
            || dash_downloader::is_dash_url(&task.url)
            || hls_downloader::is_hls_url(&task.url)
        {
            let audio = dash_downloader::build_audio_path(&target);
            temps.push(PathBuf::from(format!("{}{}", audio.display(), temp_ext)));
        }
        #[cfg(feature = "ed2k")]
        if is_ed2k_url(&task.url)
            && let Ok(link) = crate::ed2k::link::parse_ed2k_link(&task.url)
        {
            temps.push(Path::new(&task.save_dir).join(format!("{}{}", link.file_name, temp_ext)));
        }
        for temp in temps {
            match tokio::fs::try_exists(&temp).await {
                Ok(false) => {}
                Ok(true) => return false,
                Err(error) => {
                    crate::logger::report_warning(
                        "download-manager",
                        "probe temp artifact",
                        &error,
                    );
                    return false;
                }
            }
        }
        true
    }

    /// Whether we have a free slot for a new HTTP/FTP download.
    /// BT tasks are excluded from this count because they are managed by the
    /// shared librqbit session with its own concurrency controls; completed
    /// torrents that keep seeding are capped separately by
    /// `bt_config.seed_max_active` and never consume download slots.
    fn has_capacity(&self) -> bool {
        if self.max_concurrent == 0 {
            return true;
        }
        let http_ftp_active = self
            .active_tasks
            .iter()
            .filter(|(id, e)| !e.is_bt && !self.is_awaiting_decision(id, e.generation))
            .count();
        http_ftp_active < self.max_concurrent
    }

    /// Whether the named queue `queue_id` has room for another task.
    /// Returns true when:
    ///   - The queue has no max_concurrent limit (0), OR
    ///   - The number of active tasks assigned to that queue is below the limit.
    fn has_queue_capacity(&self, queue_id: &str) -> bool {
        // Default/empty queue_id: no queue-level limit.
        if queue_id.is_empty() {
            return true;
        }
        // `max_concurrent <= 0` = unlimited (avoid `-1 as usize` wrapping).
        let queue_max = self
            .queues
            .get(queue_id)
            .map(|q| q.max_concurrent)
            .unwrap_or(0);
        if queue_max <= 0 {
            return true;
        }
        // BT tasks bypass the queue entirely: they neither obey nor consume
        // queue slots (same as `has_capacity`).
        let active_in_queue = self
            .active_tasks
            .iter()
            .filter(|(id, e)| {
                !e.is_bt
                    && e.queue_id.as_str() == queue_id
                    && !self.is_awaiting_decision(id, e.generation)
            })
            .count();
        active_in_queue < queue_max as usize
    }

    /// 队列限速折算为 B/s；队列值 ≤0（= 不限制）回落到全局限速。在跑任务持有的
    /// 队列 limiter 热更新时必须遵循同一语义，否则会绕过全局限速。
    fn queue_limit_bps(&self, speed_limit_kbps: i64) -> u64 {
        if speed_limit_kbps > 0 {
            (speed_limit_kbps as u64) * 1024
        } else {
            self.speed_limiter.limit()
        }
    }

    /// Return the appropriate speed limiter for a task in `queue_id`.
    ///
    /// If the queue has a positive speed_limit_kbps, a dedicated per-queue
    /// `SpeedLimiter` is returned (creating and starting it on first use).
    /// Otherwise the global limiter is used.
    fn queue_limiter_for(&mut self, queue_id: &str) -> SpeedLimiter {
        let limit_bps = if queue_id.is_empty() {
            0u64
        } else {
            self.queues
                .get(queue_id)
                .map(|q| (q.speed_limit_kbps.max(0) as u64) * 1024)
                .unwrap_or(0)
        };
        if limit_bps > 0 {
            self.queue_limiters
                .entry(queue_id.to_string())
                .or_insert_with(|| {
                    let l = SpeedLimiter::new(limit_bps);
                    l.spawn_refill_task();
                    l
                })
                .clone()
        } else {
            self.speed_limiter.clone()
        }
    }

    /// 计算 BT 任务 add/re-add 时生效的 torrent 级上传限速（B/s，0 = 不设）。
    ///
    /// 优先级：任务级 > 队列级 > 全局。任务级 `seed_upload_limit_bps` > 0
    /// 直接生效；否则所属队列的 `upload_limit_kbps` > 0 时折算 ×1024；
    /// 都未设置返回 0（不设 torrent 级限制）。全局 `upload_limit_bytes`
    /// 是 librqbit 会话级的第二层限速，恒作为上限叠加，不在此参与计算。
    /// 生效时机与任务级一致：仅在 add/re-add 时烘焙，live 句柄不热改。
    #[cfg(feature = "bt")]
    fn effective_task_upload_bps(&self, task: &TaskInfo) -> u64 {
        effective_upload_bps(
            task.seed_upload_limit_bps,
            self.queues.get(&task.queue_id).map(|q| q.upload_limit_kbps),
        )
    }

    /// BT 被禁用时把仍停在「排队/下载中/准备中」的任务落回暂停(2)并广播，
    /// 避免 UI 留下永远等不到的假象；已暂停、失败、完成的任务保持原状。
    async fn park_bt_disabled_task(&mut self, task_id: &str) {
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(task)) => task,
            Ok(None) => return,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load task before parking BT-disabled task",
                    &error,
                );
                return;
            }
        };
        if !matches!(task.status, 0 | 1 | 5) {
            return;
        }
        if let Err(error) = self.db.update_task_status(task_id, 2, "").await {
            crate::logger::report_error("download-manager", "persist task state", &error);
            return;
        }
        self.emit_progress_from_db(task_id, 2, 0, "", 0).await;
    }

    /// BT 被禁用时，把 `pending_queue` 里的 BT 项整体摘出并落回暂停。
    /// BT 启用或队列无 BT 项时为空操作；返回被摘出的任务数。
    async fn park_pending_bt_tasks(&mut self) -> usize {
        // 先只读判定（O(1) 计数）：队列里没有 BT 项时绝不重建队列（drain 热路径，
        // 且无 BT 后端的构建里 `enabled` 恒为 false，每次 drain 都会走到这里）。
        if self.bt_config.enabled || !self.pending_queue.has_bt() {
            return 0;
        }
        let bt_items = self.pending_queue.drain_bt();
        for queued in &bt_items {
            log_info!(
                "[manager] queued BT task {} parked as paused: {}",
                queued.task_id,
                BT_DISABLED_MESSAGE
            );
            self.park_bt_disabled_task(&queued.task_id).await;
        }
        if !bt_items.is_empty() {
            self.broadcast_queue_positions();
        }
        bt_items.len()
    }

    /// Try to start tasks from the pending queue until we run out of capacity.
    ///
    /// Queue-aware: tasks blocked only by their queue's concurrent limit are
    /// skipped so that tasks from other queues (or the default queue) can
    /// proceed, rather than blocking the entire pending queue.
    async fn drain_queue(&mut self) {
        // BT 被禁用：排队中的 BT 项不得启动（启动即会因会话拒绝而落成失败），
        // 先整体摘出并落回暂停，与 auto_resume_on_start 的「跳过」语义一致。
        self.park_pending_bt_tasks().await;
        if self.pending_queue.is_empty() || !self.has_capacity() {
            return;
        }
        // 满额早退（O(队列数)）：所有仍有排队项的队列都已达并发上限时，没有任何
        // 排队项可启动，不必逐项扫描——队列级限额饱和时每个完成任务都会走到这里。
        if !self
            .pending_queue
            .queue_ids()
            .any(|queue_id| self.has_queue_capacity(queue_id))
        {
            return;
        }
        let initial_len = self.pending_queue.len();
        // 按序扫描：只摘出实际能启动的项，因队列级限额被跳过的项原位保留
        // （`cursor` 之前全是被跳过的项），保持 FIFO 与队列顺序，不搬运整条队列。
        // 已确认满额的队列记入 `blocked`，其余排队项一旦全属满额队列即停止扫描。
        let mut blocked: HashSet<String> = HashSet::new();
        let mut blocked_items = 0usize;
        let mut cursor = 0usize;
        while self.has_capacity() {
            if self.pending_queue.len().saturating_sub(blocked_items) == 0 {
                break;
            }
            let Some(item) = self.pending_queue.get(cursor) else {
                break;
            };
            // Edge case: task was resumed/cancelled while queued — drop it.
            if self.active_tasks.contains_key(&item.task_id) {
                if blocked.contains(item.queue_id.as_str()) {
                    blocked_items = blocked_items.saturating_sub(1);
                }
                self.pending_queue.remove(cursor);
                continue;
            }
            if blocked.contains(item.queue_id.as_str()) {
                cursor += 1;
                continue;
            }
            // Queue-level concurrency check: keep (don't start) if the
            // target queue is full; it may be drained on a future call.
            if !self.has_queue_capacity(&item.queue_id) {
                let queue_id = item.queue_id.clone();
                blocked_items += self.pending_queue.queued_in(&queue_id);
                blocked.insert(queue_id);
                cursor += 1;
                continue;
            }
            let Some(queued) = self.pending_queue.remove(cursor) else {
                break;
            };
            // Start the task.
            if queued.is_resume {
                self.do_resume_task(&queued.task_id).await;
            } else {
                self.do_start_task(queued).await;
            }
        }

        if self.pending_queue.len() != initial_len {
            if self.pending_queue.is_empty() {
                self.broadcast_queue_positions();
            } else {
                self.queue_positions_dirty = true;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Public task operations
    // -----------------------------------------------------------------------

    /// 启动序幕要询问「文件已存在」：让出并发槽（任务留在 `active_tasks`、
    /// 但不再计入容量），DB 状态落回 0，随后给排队中的任务腾位。世代不匹配
    /// （期间已暂停/删除/换代）的迟到通知忽略。
    async fn on_decision_pending(&mut self, task_id: &str, generation: u64) {
        let live = self
            .active_tasks
            .get(task_id)
            .is_some_and(|entry| entry.generation == generation);
        if !live {
            return;
        }
        self.awaiting_decision
            .insert(task_id.to_string(), generation);
        match self.db.load_task_by_id(task_id).await {
            Ok(Some(task)) if task.status != 0 => {
                if let Err(error) = self.db.update_task_status(task_id, 0, "").await {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                } else {
                    self.emit_progress_from_db(task_id, 0, 0, "", 0).await;
                }
            }
            Ok(_) => {}
            Err(error) => {
                crate::logger::report_error("download-manager", "load task for await", &error);
            }
        }
        self.drain_queue().await;
        self.sync_queue_occupancy();
    }

    /// 「文件已存在」答复到达。仅处理仍挂起且世代匹配的任务（其间被暂停/删除的
    /// 迟到答复忽略）。取消 → 暂停；其余答复持久化为逐任务决定，任务回到队首
    /// 作为从未起跑过的任务重新起跑（序幕据决定执行，不再询问）。
    async fn on_decision_ready(
        &mut self,
        task_id: &str,
        generation: u64,
        choice: crate::selection::FileExistsChoice,
    ) {
        let live = self
            .active_tasks
            .get(task_id)
            .is_some_and(|entry| entry.generation == generation);
        if !live || !self.is_awaiting_decision(task_id, generation) {
            return;
        }
        self.awaiting_decision.remove(task_id);
        self.active_tasks.remove(task_id);
        match crate::file_exists::ExistsDecision::from_choice(choice) {
            None => {
                if let Err(error) = self.db.update_task_status(task_id, 2, "").await {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                } else {
                    self.emit_progress_from_db(task_id, 2, 0, "", 0).await;
                    self.emit_paused_webhook(task_id).await;
                }
            }
            Some(decision) => {
                if let Err(error) = self.db.set_exists_decision(task_id, decision.as_db()).await {
                    persist_task_failure(
                        task_id,
                        format!("persist file-exists decision: {error}"),
                        &self.db,
                        &self.progress_tx,
                    )
                    .await;
                } else {
                    match self.db.load_task_by_id(task_id).await {
                        Ok(Some(task)) => {
                            self.pending_queue.push_front(QueuedTask::for_resume(task));
                            self.broadcast_queue_positions();
                        }
                        Ok(None) => {}
                        Err(error) => {
                            crate::logger::report_error(
                                "download-manager",
                                "load task after file-exists decision",
                                &error,
                            );
                        }
                    }
                }
            }
        }
        self.drain_queue().await;
        self.sync_queue_occupancy();
    }

    /// Remove a finished task from active_tokens (called by actor loop).
    /// Only removes the entry if the generation matches, preventing a stale
    /// `TaskDone` from an old spawn from accidentally removing a newer token.
    /// 收尾后兜底结束已离场优先任务的 Boost（见 `boost.rs`）。
    pub async fn on_task_done(&mut self, done: &TaskDone) {
        self.handle_task_done(done).await;
        self.reconcile_priority().await;
        self.purge_expired_completed_records(false).await;
    }

    async fn handle_task_done(&mut self, done: &TaskDone) {
        let task_id = done.task_id.as_str();
        let generation = done.generation;
        match done.phase {
            TaskDonePhase::Finished => {
                if self.awaiting_decision.get(task_id) == Some(&generation) {
                    self.awaiting_decision.remove(task_id);
                }
            }
            TaskDonePhase::AwaitingDecision => {
                self.on_decision_pending(task_id, generation).await;
                return;
            }
            TaskDonePhase::DecisionReady(choice) => {
                self.on_decision_ready(task_id, generation, choice).await;
                return;
            }
        }

        let generation_matched = self
            .active_tasks
            .get(task_id)
            .map(|e| e.generation == generation)
            .unwrap_or(false);

        // Release the file-name reservation unconditionally (success, error,
        // or cancel) so the slot is freed for the next task that picks the
        // same filename.
        if let Some(ref path) = done.reserved_temp_path {
            lock_reserved(&self.reserved_temp_paths).remove(path);
        }

        if generation_matched {
            self.active_tasks.remove(task_id);

            // Boost 模式：优先任务完成后自动恢复其他任务。
            // 仅在 generation 匹配时触发，防止旧 spawn 发来的 stale TaskDone
            // 误将仍在运行的新 spawn 的 Boost 状态清除。
            if self.priority_task_id.as_deref() == Some(task_id) {
                self.clear_priority().await;
            }
        }
        // A user pause removes the active entry immediately so another task can
        // use the freed slot, but its downloader may still be flushing. Only
        // the matching TaskDone may publish the final paused frame or release a
        // resume request that arrived during that flush window.
        let resume_after_pause = self.finish_pending_pause(task_id, generation).await;

        // A slot freed up — try to start queued tasks.
        // 先处理飞行期间到达的 resume 请求：内部「暂停 + 立即恢复」流程（改分段数 /
        // 重新下载）不在暂停时 drain，槽位必须先归还给它，否则会被排队头部抢走。
        // 用户暂停的槽位在暂停时已 drain，此处 resume 会照常受容量门控。
        // SAFETY (current_thread): `remove` + `drain_queue` have no `.await` between
        // them at this point, so no other task can observe the partially-updated state.
        // If this code is ever ported to a multi-threaded runtime, a lock around
        // `active_tokens` modifications would be required.
        if resume_after_pause {
            self.resume_task_inner(task_id).await;
        }
        self.drain_queue().await;

        // ----- Auto-retry for retriable network errors ----------------------
        // 大文件下载因网络 stall、连接重置等瞬时错误失败后，自动延迟恢复，
        // 避免用户手动操作。重试上限由用户配置 `max_auto_retries` 决定：
        //   -1   = 无限重试（按 `auto_retry_delay_secs` 递增 sleep，封顶 MAX_AUTO_RETRY_DELAY_SECS）
        //    0   = 关闭自动重试，任务直接保持 error 状态
        //  1..n = 最多重试 n 次
        // 仅在 generation 匹配（确实是这一轮 spawn 失败）时触发，防止 stale 信号误触发。
        let max_retries = self.max_auto_retries;
        if generation_matched && let Ok(Some(task)) = self.db.load_task_by_id(task_id).await {
            // 重复种子占位任务：引擎在 add 前 / AlreadyManaged 兜底时打了
            // DB 标记（见 bt_downloader::mark_duplicate_torrent）。不算失败
            // ——删除占位行（不进自动重试 / 插件 onError / task.failed
            // webhook），发 DuplicateTorrentDetected 事件由宿主提示用户并
            // 指向已有任务。
            #[cfg(feature = "bt")]
            if task.status == 4
                && let Some(owner_id) = task
                    .error_message
                    .strip_prefix(bt_downloader::DUPLICATE_TORRENT_MSG_PREFIX)
            {
                let owner_id = owner_id.to_string();
                let existing_name = match self.db.load_task_by_id(&owner_id).await {
                    Ok(Some(t)) => t.file_name,
                    _ => String::new(),
                };
                log_info!(
                    "[manager] duplicate torrent: removing placeholder task {} (existing task {})",
                    task_id,
                    owner_id
                );
                self.auto_retry_counts.remove(task_id);
                #[cfg(feature = "plugins")]
                self.intercept_retry_counts.remove(task_id);
                self.retry_unschedule(task_id);
                self.auto_failover_pending.remove(task_id);
                self.auto_failover_attempts.remove(task_id);
                match self.db.delete_task(task_id).await {
                    Ok(sources) => self.broadcast_deleted_rss_sources(sources).await,
                    Err(e) => log_info!(
                        "[manager] duplicate cleanup {}: DB delete error: {}",
                        task_id,
                        e
                    ),
                }
                self.sink.emit(EngineEvent::DuplicateTorrentDetected {
                    task_id: task_id.to_string(),
                    existing_task_id: owner_id,
                    existing_name,
                });
                self.send_tasks_snapshot().await;
                self.broadcast_queue_positions();
                self.sync_queue_occupancy();
                self.maybe_wal_checkpoint().await;
                self.maybe_release_bt_session().await;
                return;
            }
            // 本轮是否已排程恢复——决定 `task.failed` 是否应立即发出。
            let mut retry_pending = false;
            if task.status == 4
                && self.proxy_config.mode == ProxyMode::Auto
                && task.proxy_url.is_empty()
                // 出口由站点规则决定的任务不参与 Auto 故障转移：否则会被当成「直连已试过」
                // 而排一次切到代理的立即重试，绕过规则。
                && !crate::auto_proxy::route::is_rule_label(&task.auto_route)
                && !is_bt_url(&task.url)
                && !is_ed2k_url(&task.url)
                && let Some(host) = crate::segment_coordinator::extract_host(&task.url)
            {
                let transport_failed =
                    crate::auto_proxy::is_route_transport_error(&task.error_message);
                let network_reachable = crate::route_health::network_reachable();
                // 失败归因到具体路径（整机断网与路径无关，不记）：该路径近期
                // 不再被选作起飞路径，也不再作为备用链路目标。旧库裸 `proxy:*`
                // 标签无法确定是哪条代理，沿用作废两条代理先验的旧语义。
                if transport_failed && network_reachable {
                    match crate::auto_proxy::route_of_label(&task.auto_route) {
                        Some(route) => {
                            log_info!(
                                "[manager] auto-proxy: task {} host {} 路径 {:?} 传输失败，记入路径失败权重",
                                task_id,
                                host,
                                route
                            );
                            crate::route_health::record_path_failure(&host, route, &self.db);
                        }
                        None if task.auto_route.starts_with("proxy") => {
                            crate::route_health::clear_proxy_prior(&host, &self.db);
                        }
                        None => {}
                    }
                }

                // 备用链路顺序：先验更快的代理优先；近期走不通的路径（含探测
                // 竞速里刚失败的）不再作为目标。
                let mut candidates = crate::auto_proxy::resolve_candidates(&self.proxy_config);
                candidates.retain(|c| {
                    !crate::route_health::path_blocked(
                        &host,
                        crate::auto_proxy::RoutePath::Proxy(c.source),
                    )
                });
                candidates.sort_by(|a, b| {
                    let bps = |c: &crate::auto_proxy::ProxyCandidate| {
                        crate::route_health::path_prior(
                            &host,
                            crate::auto_proxy::RoutePath::Proxy(c.source),
                        )
                        .map_or(0.0, |p| p.bps)
                    };
                    bps(b).total_cmp(&bps(a))
                });
                if crate::route_health::no_switch_active(&host) {
                    candidates.clear();
                }
                let candidate_sources: Vec<_> = candidates
                    .iter()
                    .map(|candidate| candidate.source)
                    .collect();
                let direct_usable = network_reachable
                    && !crate::route_health::path_blocked(
                        &host,
                        crate::auto_proxy::RoutePath::Direct,
                    );
                let attempts = self
                    .auto_failover_attempts
                    .entry(task_id.to_string())
                    .or_default();
                let target = auto_failover_target(
                    &task.auto_route,
                    &task.error_message,
                    &candidate_sources,
                    direct_usable,
                    attempts,
                );

                if let Some(target) = target {
                    let label = match target {
                        AutoFailoverTarget::Proxy(
                            crate::auto_proxy::CandidateSource::ManualFields,
                        ) => "手动代理",
                        AutoFailoverTarget::Proxy(crate::auto_proxy::CandidateSource::System) => {
                            "系统代理"
                        }
                        AutoFailoverTarget::Direct => "直连",
                    };
                    log_info!(
                        "[manager] auto-proxy: task {} host {} 传输失败，立即换{}重试（每链路一次，不占通用重试配额）",
                        task_id,
                        host,
                        label
                    );
                    self.auto_failover_pending
                        .insert(task_id.to_string(), target);
                    self.retry_schedule(task_id, task.queue_id.clone());
                    let tx = self.retry_tx.clone();
                    let tid = task_id.to_string();
                    tokio::spawn(async move {
                        if tx.send(tid).await.is_err() {
                            // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                            tracing::debug!("download-manager notification receiver closed");
                        }
                    });
                    retry_pending = true;
                }
            }

            // 通用自动重试只在备用链路没有接管本轮时参与。备用链路始终有
            // 一次机会，即使用户把 max_auto_retries 设为 0 或配额已耗尽。
            if !retry_pending
                && max_retries != 0
                && task.status == 4
                && is_retriable_error(&task.error_message)
            {
                let count = self
                    .auto_retry_counts
                    .entry(task_id.to_string())
                    .or_insert(0);
                if max_retries == -1 || (*count as i32) < max_retries {
                    *count += 1;
                    let attempt = *count;
                    let base = if max_retries == -1 {
                        self.auto_retry_delay_secs.max(1)
                    } else {
                        self.auto_retry_delay_secs
                    };
                    let delay_secs = base
                        .saturating_mul(attempt as u64)
                        .min(MAX_AUTO_RETRY_DELAY_SECS);
                    log_info!(
                        "[manager] auto-retry {}/{} for task {} in {}s (error: {})",
                        attempt,
                        if max_retries == -1 {
                            "∞".to_string()
                        } else {
                            max_retries.to_string()
                        },
                        task_id,
                        delay_secs,
                        task.error_message
                    );
                    let tx = self.retry_tx.clone();
                    let tid = task_id.to_string();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                        if tx.send(tid).await.is_err() {
                            // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                            tracing::debug!("download-manager notification receiver closed");
                        }
                    });
                    retry_pending = true;
                    self.retry_schedule(task_id, task.queue_id.clone());
                } else {
                    log_info!(
                        "[manager] auto-retry exhausted for task {} ({} attempts), staying in error",
                        task_id,
                        max_retries
                    );
                }
            }
            // 跳过采纳的已有文件不是本任务产物：不打来源标记、不触发插件 onDone
            // （webhook task.completed 照常）。
            let adopted = task.status == 3 && self.skip_adopted(task_id).await;
            if task.status == 3 {
                self.retry_unschedule(task_id);
                // 成功完成：结束本轮通用重试与一次性备用链路状态。
                self.auto_retry_counts.remove(task_id);
                #[cfg(feature = "plugins")]
                self.intercept_retry_counts.remove(task_id);
                self.auto_failover_pending.remove(task_id);
                self.auto_failover_attempts.remove(task_id);
                // 来源标记：插件 onDone 可能移动/改写文件，须在通知前打上。后台执行，
                // 不阻塞 actor；BT 多文件产物是目录，由模块递归展开。
                // 开启「使用服务器文件时间」时标记不得刷掉服务器时间（Windows NTFS
                // 写 ADS 会改主文件修改时间）；BT / eD2k 没有服务器时间，不付回写成本。
                if !adopted && let Some(target) = task_target_path(&task.save_dir, &task.file_name)
                {
                    let host_url = if task.origin_url.is_empty() {
                        task.url.clone()
                    } else {
                        task.origin_url.clone()
                    };
                    let keep_mtime =
                        self.use_server_time && !is_bt_url(&task.url) && !is_ed2k_url(&task.url);
                    crate::mark_of_the_web::spawn_mark_downloaded(
                        target,
                        host_url,
                        task.referrer.clone(),
                        keep_mtime,
                    );
                }
            }

            // 通知平面：onDone / onError（fire-and-forget）。onError 内脚本可经
            // flux.task.requestRetry 命令式重试（受 max_auto_retries 约束）。
            #[cfg(feature = "plugins")]
            if let Some(pm) = &self.plugin_manager {
                if task.status == 3 && !adopted {
                    let file_path = format!("{}/{}", task.save_dir, task.file_name);
                    // 轨对任务补充音频 sidecar 信息：mux 成功 → sidecar 已删，
                    // muxed=true；mux 失败降级 → sidecar 独立存在，audio_path=Some。
                    // 非轨对任务两者取默认（None/false）。以磁盘实况为准，不依赖
                    // dash 下载器回传状态。
                    let is_track_pair = matches!(
                        self.db.load_audio_url(task_id).await,
                        Ok(Some(ref a)) if !a.is_empty()
                    );
                    let (audio_path, muxed) = if is_track_pair {
                        let sidecar =
                            dash_downloader::build_audio_path(std::path::Path::new(&file_path));
                        if tokio::fs::try_exists(&sidecar).await.unwrap_or(false) {
                            (Some(sidecar.to_string_lossy().into_owned()), false)
                        } else {
                            (None, true)
                        }
                    } else {
                        (None, false)
                    };
                    pm.notify(crate::plugin::PluginEvent::Done {
                        task_id: task_id.to_string(),
                        url: task.url.clone(),
                        file_path,
                        audio_path,
                        muxed,
                    })
                    .await;
                } else if task.status == 4 {
                    pm.notify(crate::plugin::PluginEvent::Error {
                        task_id: task_id.to_string(),
                        url: task.url.clone(),
                        message: task.error_message.clone(),
                    })
                    .await;
                    if !is_bt_url(&task.url) && !is_ed2k_url(&task.url) {
                        let ic = self.task_interceptor(task_id, &task.url, &task.url).await;
                        if crate::intercept::wants(
                            ic.as_ref(),
                            crate::intercept::InterceptStage::Failed,
                        ) {
                            let retry_tx = self.plugin_retry_tx.clone();
                            let retry_marks = self.intercept_retry_marks.clone();
                            let ctx = crate::intercept::ctx::task_ctx_from_row(
                                &task,
                                &task.url,
                                crate::intercept::protocol_kind(&task.url, false),
                            );
                            let error_message = task.error_message.clone();
                            let http_status = extract_http_status(&task.error_message);
                            let retry_count =
                                self.auto_retry_counts.get(task_id).copied().unwrap_or(0);
                            let notes = crate::intercept::ActivityNotes::new(
                                self.db.clone(),
                                self.sink.clone(),
                                task_id.to_string(),
                            );
                            let retry_task_id = task_id.to_string();
                            pm.runtime_handle().spawn(async move {
                                let delay = crate::intercept::drive::run_failed(
                                    ic.as_ref(),
                                    &notes,
                                    &ctx,
                                    error_message,
                                    http_status,
                                    retry_count,
                                )
                                .await;
                                if let Some(delay) = delay {
                                    retry_marks
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .insert(retry_task_id.clone());
                                    if retry_tx.send((retry_task_id.clone(), delay)).is_err() {
                                        retry_marks
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                                            .remove(&retry_task_id);
                                        tracing::debug!("plugin retry receiver closed");
                                    }
                                }
                            });
                        }
                    }
                }
            }

            // Webhook 语义生命周期事件——与插件通知平面同点位。`task.failed`
            // 只在自动重试**彻底放弃**后发出（重试期间保持静默），否则一次
            // 网络抖动会给用户连发四条失败通知。
            let webhook_kind = if task.status == 3 {
                Some(crate::webhook::WebhookEventKind::TaskCompleted)
            } else if task.status == 4 && !retry_pending {
                Some(crate::webhook::WebhookEventKind::TaskFailed)
            } else {
                None
            };
            if let Some(kind) = webhook_kind {
                let event = self.webhook_event_from_task(kind, &task);
                self.emit_task_event(event);
            }
        }

        self.sync_queue_occupancy();

        self.maybe_wal_checkpoint().await;
        #[cfg(feature = "bt")]
        self.maybe_release_bt_session().await;
    }

    /// Run a WAL checkpoint when all tasks are idle (no active downloads and
    /// nothing queued) so the WAL file doesn't linger and cause sporadic disk
    /// I/O in the background.
    async fn maybe_wal_checkpoint(&self) {
        if self.active_tasks.is_empty()
            && self.pending_queue.is_empty()
            && let Err(e) = self.db.wal_checkpoint().await
        {
            log_info!("[manager] wal_checkpoint error: {e}");
        }
    }

    /// Release the BT session if no BT tasks are currently active or queued.
    ///
    /// Called after a task completes, is paused, cancelled, or deleted.
    /// Shuts down the multi-threaded librqbit runtime (DHT, UPnP, tracker
    /// connections) to eliminate idle CPU overhead.  The session is re-created
    /// transparently on the next BT download via `ensure_bt_session`.
    ///
    /// 仅因「暂停中的未完成任务」保活时不立即释放：拆会话连带丢句柄缓存与
    /// swarm/tracker/DHT 状态，恢复就要付「重建会话 + add_torrent + fastresume
    /// 采样校验 + peer 冷启动」的全额成本；保留会话则恢复只是 unpause。但永久
    /// 保活会让 DHT 等定时器常驻、唤醒 NAS 上休眠的硬盘，所以只给
    /// [`BT_SESSION_PAUSED_IDLE_GRACE`] 宽限，到期由
    /// [`Self::recheck_bt_session_paused_grace`]（挂在做种求值 tick 上）释放。
    /// 已完成任务不计入本判定（做种由 `has_seeders` 保活，做种关闭的完成任务不
    /// 钉住会话），因此全部 BT 任务终态化后会话仍会立即按既有路径释放。
    #[cfg(feature = "bt")]
    async fn maybe_release_bt_session(&mut self) {
        self.release_bt_session_if_idle(false).await;
    }

    /// 宽限复查点：仅当已进入「仅暂停任务保活」的空闲期时才复查，未进入时
    /// （含无会话）是纯内存的 `Option` 判断，零 I/O。只负责宽限到期后的释放——
    /// 其余释放时机仍由各事件驱动路径上的 [`Self::maybe_release_bt_session`] 决定。
    #[cfg(feature = "bt")]
    async fn recheck_bt_session_paused_grace(&mut self) {
        if self.bt_paused_idle_since.is_none() {
            return;
        }
        self.release_bt_session_if_idle(true).await;
    }

    /// 收集 [`decide_bt_session_release`] 所需的保活来源。按代价由低到高短路：
    /// 已有硬保活来源时不再计算后续项（尤其是要遍历句柄取 stats 的
    /// `has_paused_incomplete`）。
    #[cfg(feature = "bt")]
    async fn bt_session_holds(&self) -> BtSessionHolds {
        let mut holds = BtSessionHolds {
            active_download: self.active_tasks.values().any(|e| e.is_bt),
            ..BtSessionHolds::default()
        };
        let Some(bt) = &self.bt_session else {
            return holds;
        };
        if holds.busy_reason().is_none() {
            holds.seeders = bt.has_seeders().await;
        }
        if holds.busy_reason().is_none() {
            // BT tasks bypass the pending queue, so the queue guard is purely
            // defensive in case the invariant changes in the future.
            holds.queued = self.pending_queue.has_bt_url();
            // Detached `add_torrent` tasks hold an `Arc<Session>` that keeps the
            // BT listening port bound; creating a new session while the old port
            // is in use causes the next BT download to fail immediately.
            holds.inflight_add = bt.has_inflight_adds();
        }
        if holds.busy_reason().is_none() {
            holds.paused_incomplete = bt.has_paused_incomplete().await;
        }
        holds
    }

    /// [`Self::maybe_release_bt_session`] / [`Self::recheck_bt_session_paused_grace`]
    /// 的共同实现。`periodic` 为 `true` 时来自 5s tick：不刷日志（日志落盘会成为
    /// 空闲期的周期写），且仅在暂停宽限到期时才释放。
    #[cfg(feature = "bt")]
    async fn release_bt_session_if_idle(&mut self, periodic: bool) {
        if self.bt_session.is_none() {
            self.bt_paused_idle_since = None;
            return;
        }
        let holds = self.bt_session_holds().await;
        let verdict = decide_bt_session_release(
            holds,
            &mut self.bt_paused_idle_since,
            std::time::Instant::now(),
            BT_SESSION_PAUSED_IDLE_GRACE,
        );
        match verdict {
            BtSessionVerdict::Keep(reason) => {
                if !periodic {
                    match reason {
                        BtSessionKeep::Seeders => {
                            log_info!("[manager] deferring BT session release — seeders active");
                        }
                        BtSessionKeep::InflightAdd => log_info!(
                            "[manager] deferring BT session release — detached add_torrent still in flight"
                        ),
                        BtSessionKeep::ActiveDownload | BtSessionKeep::QueuedTask => {}
                    }
                }
                return;
            }
            BtSessionVerdict::KeepPausedGrace {
                just_armed,
                remaining,
            } => {
                if just_armed || !periodic {
                    log_info!(
                        "[manager] deferring BT session release — paused BT task(s) hold resume state ({}s idle grace left)",
                        remaining.as_secs()
                    );
                }
                return;
            }
            BtSessionVerdict::Release { after_paused_grace } => {
                if after_paused_grace {
                    log_info!(
                        "[manager] only paused BT task(s) held the BT session for {}s — releasing BT session",
                        BT_SESSION_PAUSED_IDLE_GRACE.as_secs()
                    );
                } else if periodic {
                    // 周期复查不负责「无任何保活来源」的释放，交事件驱动路径。
                    return;
                } else {
                    log_info!(
                        "[manager] no BT task holds live or resume state — releasing BT session"
                    );
                }
            }
        }
        // Shut down on a background thread (same pattern as Drop) to avoid
        // blocking the actor loop while the librqbit runtime winds down.
        if let Some(bt) = self.bt_session.take() {
            std::thread::spawn(move || match Arc::try_unwrap(bt) {
                Ok(owned) => owned.shutdown(),
                Err(shared) => shared.shutdown(),
            });
        }
    }

    /// 启动一次后台「文件跟踪」扫描：检查所有已完成任务的目标文件是否仍在
    /// 磁盘上，把变化落库并通过 [`crate::events::EngineEvent::FileMissingChanged`]
    /// 上报。detached spawn，立即返回、不阻塞调用方；内部 `scanning` 标志避免
    /// 重叠扫描。由启动流程、桌面窗口聚焦（`RescanFiles`）、headless 定时器触发。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};
    /// # use fluxdown_engine::bt_config::BtConfig;
    /// # use fluxdown_engine::proxy_config::ProxyConfig;
    /// # async fn run() -> Result<(), fluxdown_engine::EngineError> {
    /// # let config = EngineConfig { max_concurrent: 5, speed_limit_bps: 0, upload_limit_bps: 0, default_save_dir: "/tmp/downloads".to_string(), app_data_dir: "/tmp/fluxdown".to_string(), bt_config: BtConfig::default(), proxy_config: ProxyConfig::default(), user_agent: String::new(), data_dir_override: None, database_url: None };
    /// let engine = Engine::new(config, Arc::new(NoopSink), Arc::new(NoopSelection)).await?;
    /// engine.manager.spawn_file_scan();
    /// # Ok(())
    /// # }
    /// ```
    pub fn spawn_file_scan(&self) {
        let db = self.db.clone();
        let sink = self.sink.clone();
        let scanning = self.scanning.clone();
        let auto_delete = self.missing_file_auto_delete;
        let cleanup_tx = self.missing_cleanup_tx.clone();
        tokio::spawn(async move {
            scan_missing_files(db, sink, scanning, auto_delete, cleanup_tx).await;
        });
    }

    /// 周期性文件跟踪扫描是否应当在本次 tick 执行：`idle_file_scan`
    /// 开启时恒执行；关闭时仅在有任务活跃或排队等待时才执行（避免刚
    /// 完成/正在下载期间因扫描被跳过而漏判文件丢失）。只用于 headless
    /// 定时器这一条被动触发路径；启动预热、窗口聚焦、手动重扫 API 都
    /// 直接调用 [`Self::spawn_file_scan`]，不经此判定。
    pub fn should_run_idle_scan(&self) -> bool {
        self.idle_file_scan || !self.active_tasks.is_empty() || !self.pending_queue.is_empty()
    }

    /// 清理超过保留期的「已完成」任务**记录**（`completed_record_retention_days`）：
    /// 只删记录、永不删文件，做种中 / 排队做种的任务跳过。保留期为 0 时什么都不做。
    ///
    /// 无独立定时器：由进程启动、任务完成、`daemon.task.rescan`、设置变更四条被动
    /// 路径调用。`force = false` 时受 [`RETENTION_PURGE_DEBOUNCE`] 去抖；设置变更与
    /// 启动传 `true`。返回本次删除的记录数。
    pub async fn purge_expired_completed_records(&mut self, force: bool) -> usize {
        let days = self.completed_record_retention_days;
        if days == 0 {
            return 0;
        }
        if !force
            && self
                .last_retention_purge
                .is_some_and(|at| at.elapsed() < RETENTION_PURGE_DEBOUNCE)
        {
            return 0;
        }
        self.last_retention_purge = Some(std::time::Instant::now());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let cutoff =
            i64::try_from(now.saturating_sub(u64::from(days) * 86_400)).unwrap_or(i64::MAX);
        let mut purged = 0;
        loop {
            let ids = match self
                .db
                .list_expired_completed_task_ids(cutoff, RETENTION_PURGE_CHUNK)
                .await
            {
                Ok(ids) => ids,
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "list expired completed records",
                        &error,
                    );
                    break;
                }
            };
            if ids.is_empty() {
                break;
            }
            match self.delete_tasks_batch(&ids, false).await {
                Ok(outcome) => {
                    purged += outcome.deleted;
                    // 仍有保留下来的任务会被下一轮重新选中：直接结束，等下次触发再试。
                    if outcome.failed > 0 || outcome.deleted == 0 {
                        break;
                    }
                }
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "purge expired completed records",
                        &error,
                    );
                    break;
                }
            }
        }
        if purged > 0 {
            log_info!(
                "[manager] retention: purged {purged} completed record(s) older than {days} day(s)"
            );
        }
        purged
    }

    /// Normalize seeding state left over from a previous session.
    ///
    /// 启动时会清掉 librqbit 的 session.json，上次会话的做种/排队行在重启后
    /// 没有任何真实 peer 连接。这里先把它们统一归一化为 `UserStopped` 并
    /// 清除持久化起始时间（崩溃安全底线；累计做种时长保留），再把被归一化
    /// 的任务列表返回——`bt_auto_reseed` 开启时由
    /// [`Self::auto_reseed_on_start`] 拿这批任务自动重新挂载做种，避免二次
    /// 查库。
    pub async fn reset_stale_seeding(&self) -> Vec<TaskInfo> {
        let mut stale = Vec::new();
        for status in [SEEDING_STATUS_ACTIVE, SEEDING_STATUS_QUEUED] {
            match self.db.load_tasks_with_seeding_status(status).await {
                Ok(t) => stale.extend(t),
                Err(e) => {
                    log_info!("[manager] load_tasks_with_seeding_status error: {}", e);
                    break;
                }
            }
        }
        // 原地收拢已落库的行；失败行不能作为「已归一化」的候选继续自动做种。
        let mut normalized = 0;
        for index in 0..stale.len() {
            let t = &stale[index];
            let short = &t.task_id[..t.task_id.len().min(8)];
            log_info!(
                "[manager] resetting stale seeding state for task {} to user-stopped",
                short
            );
            if let Err(error) = self
                .db
                .update_task_seeding_status(
                    &t.task_id,
                    crate::bt_seeding::SeedingStopReason::UserStopped.as_i32(),
                    crate::bt_seeding::SeedingStopReason::UserStopped.message(),
                )
                .await
            {
                crate::logger::report_error("download-manager", "persist task state", &error);
                continue;
            }
            self.sink.emit(EngineEvent::TaskProgress {
                task_id: t.task_id.clone(),
                status: t.status,
                downloaded_bytes: t.downloaded_bytes,
                total_bytes: t.total_bytes,
                speed: 0,
                file_name: t.file_name.clone(),
                save_dir: t.save_dir.clone(),
                url: t.url.clone(),
                error_message: String::new(),
                upload_speed_bps: 0,
                uploaded_bytes: t.uploaded_bytes,
                seeding_status: crate::bt_seeding::SeedingStopReason::UserStopped.as_i32(),
                seeding_message: crate::bt_seeding::SeedingStopReason::UserStopped
                    .message()
                    .to_string(),
                seeding_time_secs: t.seeding_time_secs,
            });
            stale.swap(normalized, index);
            normalized += 1;
        }
        stale.truncate(normalized);
        stale
    }

    /// 启动自动续种（config `bt_auto_reseed`，默认开）：上次退出时仍在
    /// 做种/排队、且已完成（status=3）的任务，重启后自动重新挂载进
    /// librqbit 继续做种。挂载失败由 [`Self::spawn_reseed_from_disk`] 的
    /// 现有回退兜底（保持 UserStopped + 失败原因）；无可续任务时不创建
    /// BT 会话，会话创建失败时全部保持停止态。
    #[cfg(feature = "bt")]
    async fn auto_reseed_on_start(&mut self, stale: Vec<TaskInfo>) {
        let enabled = match self.db.get_config("bt_auto_reseed").await {
            Ok(value) => value.is_none_or(|v| v != "0" && v != "false"),
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "read automatic reseeding setting",
                    &error,
                );
                return;
            }
        };
        if !enabled {
            return;
        }
        let candidates: Vec<TaskInfo> = stale.into_iter().filter(|t| t.status == 3).collect();
        if candidates.is_empty() {
            return;
        }
        if let Err(e) = self.ensure_bt_session().await {
            log_info!("[manager] auto reseed: BT session init failed: {}", e);
            return;
        }
        let Some(bt) = self.bt_session.clone() else {
            return;
        };
        let stopped = crate::bt_seeding::SeedingStopReason::UserStopped;
        for t in candidates {
            // 行已被 reset_stale_seeding 归一化为 UserStopped；副本同步该
            // 状态，挂载失败回退时才不会把过期的 active/queued 写回去。
            let mut task = t;
            task.seeding_status = stopped.as_i32();
            task.seeding_message = stopped.message().to_string();
            self.spawn_reseed_from_disk(bt.clone(), &task).await;
        }
    }

    pub async fn load_and_send_all_tasks(&mut self) {
        // 启动时将残留的 downloading/pending 状态矫正为 paused（仅首次执行）
        // 后续由 create_task / batch_create 触发时不重复重置，避免将刚插入的
        // pending 任务误改为 paused 导致前端显示"已暂停"
        let is_first_run = !self.startup_reset_done;
        let mut reset_ids: Vec<String> = Vec::new();
        if is_first_run {
            match self.db.reset_incomplete_tasks_to_paused().await {
                Ok(ids) => reset_ids = ids,
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "reset startup task statuses",
                        &error,
                    );
                    return;
                }
            }
        }

        let order_db = self.db.clone();
        let order = order_db.lock_event_order().await;
        #[cfg_attr(not(feature = "bt"), allow(unused_mut))]
        let mut tasks = match self.db.load_all_tasks().await {
            Ok(t) => t,
            Err(error) => {
                crate::logger::report_error("download-manager", "load task snapshot", &error);
                return;
            }
        };

        // 启动清扫：重复种子占位任务的删除发生在 on_task_done（标记写库
        // 之后）——若进程在两者之间被杀，会残留一条裸标记文案的 error 行。
        // 标记行本就注定删除，这里补删（仅首次执行）。
        if is_first_run {
            self.startup_reset_done = true;
            #[cfg(feature = "bt")]
            {
                let mut orphans: Vec<String> = tasks
                    .iter()
                    .filter(|t| {
                        t.status == 4
                            && t.error_message
                                .starts_with(bt_downloader::DUPLICATE_TORRENT_MSG_PREFIX)
                    })
                    .map(|t| t.task_id.clone())
                    .collect();
                if !orphans.is_empty() {
                    let mut removed = 0;
                    for index in 0..orphans.len() {
                        let tid = &orphans[index];
                        log_info!(
                            "[manager] startup: removing orphan duplicate-torrent placeholder {}",
                            tid
                        );
                        match self.db.delete_task(tid).await {
                            Ok(sources) => {
                                self.broadcast_deleted_rss_sources(sources).await;
                                orphans.swap(removed, index);
                                removed += 1;
                            }
                            Err(e) => {
                                log_info!("[manager] startup duplicate cleanup {}: {}", tid, e)
                            }
                        }
                    }
                    orphans.truncate(removed);
                    tasks.retain(|t| !orphans.contains(&t.task_id));
                }
            }
        }

        // On the very first call (app startup), scan all known save directories
        // for orphaned BT staging directories left behind by a previous session
        // that crashed or was force-killed before cleanup could run.
        //
        // We do this here because:
        //   1. All live task IDs are now known (just loaded from DB above).
        //   2. The BT session has not yet (re-)started any downloads, so no
        //      staging directory is currently being written to.
        //   3. `startup_reset_done` gates this to a single execution per
        //      process lifetime, matching the intent of the startup-only reset.
        #[cfg(feature = "bt")]
        if is_first_run {
            'startup_cleanup: {
                // ---------------------------------------------------------------
                // Startup staging-directory cleanup — three cases handled in one
                // pass over all known save directories:
                //
                // A) staging dir belongs to a COMPLETED BT task
                //    → The real file was already moved to its final location.
                //      The staging dir should be empty (or contain only librqbit
                //      placeholder files).  Delete it unconditionally.
                //      Exception: if the move was interrupted (app crash between
                //      stats.finished and move_path), rescue the file first.
                //
                // B) staging dir belongs to a PENDING/DOWNLOADING/PAUSED task
                //    → Active download in progress (or paused mid-way).
                //      Leave it alone — the downloader needs it.
                //
                // C) staging dir has no matching task in the DB (orphan)
                //    → Left over from a previous session that crashed or was
                //      force-killed before cleanup ran.  Delete it.
                // ---------------------------------------------------------------

                // Build per-task lookups we need during the directory scan.
                // task_id → (status, save_dir, file_name, total_bytes)
                let task_map: std::collections::HashMap<&str, (i32, &str, &str, i64)> = tasks
                    .iter()
                    .filter(|t| is_bt_url(&t.url))
                    .map(|t| {
                        (
                            t.task_id.as_str(),
                            (
                                t.status,
                                t.save_dir.as_str(),
                                t.file_name.as_str(),
                                t.total_bytes,
                            ),
                        )
                    })
                    .collect();

                // Collect every unique save_dir (including the global default so
                // we catch staging dirs whose DB record was hard-deleted).
                let mut save_dirs: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                save_dirs.insert(self.default_save_dir.as_str());
                for t in &tasks {
                    save_dirs.insert(t.save_dir.as_str());
                }

                // Identify completed BT tasks whose staging dir still exists so
                // we can attempt a rescue move before unconditional cleanup.
                // Owned tuples:rescue 内含 move_path(最坏 2s 瞬时锁重试退避),
                // 必须经 spawn_blocking 跑,不能在 current_thread runtime 上同步
                // 阻塞(会冻结进度上报/FFI 响应)。
                let mut rescue_input: Vec<(String, String, String)> = Vec::new();
                for (&id, (status, save_dir, file_name, _)) in &task_map {
                    if *status != 3 {
                        continue;
                    }
                    let stage = bt_downloader::bt_stage_dir(save_dir, id);
                    if tokio::fs::try_exists(stage).await.unwrap_or(false) {
                        rescue_input.push((
                            id.to_string(),
                            save_dir.to_string(),
                            file_name.to_string(),
                        ));
                    }
                }

                // Build total_bytes lookup for DB update after rescue.
                let total_bytes_map: std::collections::HashMap<&str, i64> = task_map
                    .iter()
                    .map(|(&id, (_, _, _, tb))| (id, *tb))
                    .collect();

                if !rescue_input.is_empty() {
                    // 采集**未完成**任务的活跃完成哨兵(bt_completion_top_*),
                    // 按 save_dir 归组(小写折叠)。errored mid-completion 的任务
                    // 重启恢复后会带哨兵重试完成移动,rescue 的 dedup 必须避开这
                    // 些已声明的名字,否则对方重试复用哨兵会 merge/覆盖进 rescue
                    // 出的产物(跨任务哨兵劫持)。status==3 任务的哨兵已在完成
                    // 路径删除,残留即孤儿,无需排除——其名字已落盘,磁盘 dedup
                    // 自然避开。
                    let mut rescue_claims: std::collections::HashMap<
                        String,
                        std::collections::HashSet<String>,
                    > = std::collections::HashMap::new();
                    let rows = match self.db.list_config_with_prefix("bt_completion_top_").await {
                        Ok(rows) => rows,
                        Err(error) => {
                            crate::logger::report_warning(
                                "download-manager",
                                "load staging recovery ownership claims",
                                &error,
                            );
                            break 'startup_cleanup;
                        }
                    };
                    for (key, value) in rows {
                        let Some(tid) = key.strip_prefix("bt_completion_top_") else {
                            continue;
                        };
                        if let Some((_, save_dir, _, _)) = task_map.get(tid) {
                            rescue_claims
                                .entry((*save_dir).to_string())
                                .or_default()
                                .insert(value.to_lowercase());
                        }
                    }
                    let rescued = match tokio::task::spawn_blocking(move || {
                        bt_downloader::rescue_stranded_staging_files(&rescue_input, &rescue_claims)
                    })
                    .await
                    {
                        Ok(rescued) => rescued,
                        Err(error) => {
                            if error.is_cancelled() {
                                tracing::debug!("startup staging recovery worker cancelled");
                            } else {
                                crate::logger::report_error(
                                    "download-manager",
                                    "join startup staging recovery worker",
                                    &error,
                                );
                            }
                            break 'startup_cleanup;
                        }
                    };
                    for (task_id, final_name) in rescued {
                        let tb = total_bytes_map.get(task_id.as_str()).copied().unwrap_or(0);
                        if let Err(e) = self
                            .db
                            .update_task_file_info(&task_id, &final_name, tb)
                            .await
                        {
                            log_info!(
                                "[manager] rescue: failed to update file_name for {}: {}",
                                task_id,
                                e
                            );
                        } else {
                            log_info!(
                                "[manager] rescue: updated file_name → '{}' for task {}",
                                final_name,
                                task_id
                            );
                        }
                    }
                }

                // Now scan all save_dirs for staging dirs and handle each case.
                // Tokio fs keeps directory enumeration/stat/delete off the host's
                // runtime while preserving the existing decisions.
                for save_dir in &save_dirs {
                    let dir = Path::new(save_dir);
                    let mut entries = match tokio::fs::read_dir(dir).await {
                        Ok(entries) => entries,
                        Err(error) => {
                            if error.kind() != std::io::ErrorKind::NotFound {
                                crate::logger::report_warning(
                                    "download-manager",
                                    "open startup staging directory",
                                    &error,
                                );
                            }
                            continue;
                        }
                    };
                    loop {
                        let entry = match entries.next_entry().await {
                            Ok(Some(entry)) => entry,
                            Ok(None) => break,
                            Err(error) => {
                                crate::logger::report_warning(
                                    "download-manager",
                                    "enumerate startup staging directory",
                                    &error,
                                );
                                break;
                            }
                        };
                        let file_name = entry.file_name();
                        let name_str = file_name.to_string_lossy();
                        if !name_str.starts_with(bt_downloader::BT_STAGE_PREFIX) {
                            continue;
                        }
                        let task_id_str = &name_str[bt_downloader::BT_STAGE_PREFIX.len()..];
                        let path = entry.path();

                        match task_map.get(task_id_str) {
                            None => {
                                // Case C: orphan — no matching task in DB.
                                log_info!(
                                    "[manager] startup cleanup: removing orphan staging dir {}",
                                    path.display()
                                );
                                if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                                    log_info!(
                                        "[manager] startup cleanup: failed to remove orphan staging dir {}: {}",
                                        path.display(),
                                        e
                                    );
                                }
                            }
                            Some((3 /* STATUS_COMPLETED */, _, _, _)) => {
                                // Case A: completed task — staging dir 通常应已为空。
                                // rescue_stranded_staging_files 已迁出真实数据,剩下的一般
                                // 只是 librqbit 占位文件(0 字节)或空目录。但若 rescue 因
                                // 部分移动失败(权限/跨盘/IO)而保留了仍含真实数据的目录,
                                // 这里必须同样用 has_real_data 守卫保留,否则无条件
                                // remove_dir_all 会把这些文件永久删除(与 Case B 一致)。
                                if directory_has_real_data(&path).await {
                                    log_info!(
                                        "[manager] startup cleanup: keeping completed-task staging dir {} (still has real data; rescue likely partially failed)",
                                        path.display()
                                    );
                                } else {
                                    log_info!(
                                        "[manager] startup cleanup: removing completed-task staging dir {}",
                                        path.display()
                                    );
                                    if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                                        log_info!(
                                            "[manager] startup cleanup: failed to remove completed staging dir {}: {}",
                                            path.display(),
                                            e
                                        );
                                    }
                                }
                            }
                            Some(_) => {
                                // Case B: active/paused task — keep staging dir only if it
                                // contains real (non-zero-byte) data.  An all-zero-byte
                                // staging dir means librqbit pre-allocated the file but
                                // the task was paused/cancelled before any real data was
                                // written (e.g. the same torrent was re-added, creating a
                                // new task_id and new staging dir, making this one stale).
                                if directory_has_real_data(&path).await {
                                    log_info!(
                                        "[manager] startup cleanup: keeping staging dir {} (task active/paused, has data)",
                                        path.display()
                                    );
                                } else {
                                    log_info!(
                                        "[manager] startup cleanup: removing empty staging dir {} (task active/paused but no real data)",
                                        path.display()
                                    );
                                    if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                                        log_info!(
                                            "[manager] startup cleanup: failed to remove empty staging dir {}: {}",
                                            path.display(),
                                            e
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Snapshot task info before sending AllTasks (which consumes `tasks`).
        let task_snapshots: Vec<(String, i64)> = tasks
            .iter()
            .map(|t| (t.task_id.clone(), t.total_bytes))
            .collect();

        self.sink.emit(EngineEvent::TasksSnapshot(tasks));
        drop(order);

        // Send persisted segment data for each task so the UI can display
        // download distribution immediately after app restart.
        for (task_id, total_bytes) in &task_snapshots {
            self.send_segments_from_db(task_id, *total_bytes).await;
        }
        if is_first_run {
            // 文件跟踪：仅进程启动时扫一次；运行期检测交给 RescanFiles（桌面/
            // 移动聚焦）与 headless 定时器两条专属触发路径。
            self.spawn_file_scan();
            self.purge_expired_completed_records(true).await;
            // 残留做种态先归一化为 UserStopped（崩溃安全底线），随后按
            // `bt_auto_reseed` 决定是否自动重新挂载做种。
            #[cfg(feature = "bt")]
            {
                let stale = self.reset_stale_seeding().await;
                self.auto_reseed_on_start(stale).await;
            }
            // 启动恢复只是把上次的占用状态接回来，不是「队列开始」，静默登记占用。
            self.restoring_on_start = true;
            self.resume_on_start(reset_ids).await;
            self.sync_queue_occupancy();
            self.restoring_on_start = false;
        }
    }

    /// `auto_resume_on_start=true` 时恢复上次退出时处于下载中/准备中/排队的任务。
    /// 只恢复启动矫正时被复位的那批（不碰用户手动暂停的），跳过已停止队列，
    /// 并发上限与队列门控交给 `batch_resume` 的常规准入。
    async fn auto_resume_on_start(&mut self, reset_ids: Vec<String>) {
        if reset_ids.is_empty() || !self.auto_resume_enabled().await {
            return;
        }
        let ids = self.startup_resume_candidates(&reset_ids).await;
        if ids.is_empty() {
            return;
        }
        log_info!(
            "[manager] auto_resume_on_start: resuming {} task(s)",
            ids.len()
        );
        self.batch_resume(&ids).await;
    }

    /// config `auto_resume_on_start` 是否开启（读失败按关闭处理）。
    async fn auto_resume_enabled(&self) -> bool {
        match self.db.get_config("auto_resume_on_start").await {
            Ok(value) => value.as_deref() == Some("true"),
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "read startup resume setting",
                    &error,
                );
                false
            }
        }
    }

    /// 启动时可恢复的任务（保持 `ids` 顺序）：仍存在、未完成、所在队列运行中，
    /// BT 被禁用时跳过 BT 任务。
    async fn startup_resume_candidates(&self, ids: &[String]) -> Vec<String> {
        let rows = match self.db.load_tasks_by_ids(ids).await {
            Ok(r) => r,
            Err(e) => {
                log_info!("[manager] startup resume: load error: {}", e);
                return Vec::new();
            }
        };
        let by_id: HashMap<&str, &TaskInfo> =
            rows.iter().map(|t| (t.task_id.as_str(), t)).collect();
        ids.iter()
            .filter(|id| {
                let Some(task) = by_id.get(id.as_str()) else {
                    return false;
                };
                if task.status == 3 || (!self.bt_config.enabled && is_bt_url(&task.url)) {
                    return false;
                }
                // 孤儿/空 queue_id 视作运行中（与 eligible_resume_task_ids 一致）。
                self.queues
                    .get(task.queue_id.as_str())
                    .map(|q| q.is_running)
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    /// 批量操作（启停队列/组暂停恢复/全局暂停恢复）尾部的单次任务快照广播。
    /// 对比 [`Self::load_and_send_all_tasks`]：不做启动矫正、不逐任务重发分段
    /// 数据——N 任务只产生一条 [`EngineEvent::TasksSnapshot`]。
    pub async fn send_tasks_snapshot(&self) {
        // 读库与发出必须在同一次持门内完成，否则读库后才完成的任务会被这份
        // 旧快照回退成「下载中」，见 `Db::lock_event_order`。
        let _order = self.db.lock_event_order().await;
        match self.db.load_all_tasks().await {
            Ok(tasks) => self.sink.emit(EngineEvent::TasksSnapshot(tasks)),
            Err(e) => log_info!("[manager] send_tasks_snapshot error: {}", e),
        }
    }

    /// Load segment records from DB and emit a `SegmentProgress` event.
    /// Used when pausing and on app startup to restore the download distribution
    /// visualization without requiring an active download.
    ///
    /// 轨对任务（DASH 音视频分离）的段行是【当前轨相对坐标】：音频轨阶段须
    /// 平移 +视频轨大小并合成 100% 前缀段（index=-1，与 coordinator 发射边界
    /// 的 ReportScope 映射一致），否则暂停/重启后分布图会把音频段画到文件头。
    async fn send_segments_from_db(&self, task_id: &str, total_bytes: i64) {
        let db_segs = match self.db.load_segments(task_id).await {
            Ok(segments) => segments,
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "load persisted segment progress",
                    &error,
                );
                return;
            }
        };
        if !db_segs.is_empty() {
            let base = self.track_pair_segment_base(task_id).await;
            let mut segments: Vec<SegmentDetail> = db_segs
                .iter()
                .map(|s| SegmentDetail {
                    index: s.index,
                    start_byte: s.start_byte + base,
                    end_byte: s.end_byte + base,
                    downloaded_bytes: s.downloaded_bytes,
                })
                .collect();
            if base > 0 {
                segments.insert(
                    0,
                    SegmentDetail {
                        index: -1,
                        start_byte: 0,
                        end_byte: base - 1,
                        downloaded_bytes: base,
                    },
                );
            }
            self.sink.emit(EngineEvent::SegmentProgress {
                task_id: task_id.to_string(),
                total_bytes,
                segment_count: segments.len() as i32,
                segments,
            });
        }
    }

    /// 轨对任务段行的任务坐标偏移：`audio_url` 标记存在且视频轨产物已就位
    /// （最终文件存在、无 `.fdownloading` 临时文件）→ 段行属音频轨，偏移 =
    /// 视频轨文件大小；其余情况 0（视频轨段行本就从任务坐标 0 起）。
    async fn track_pair_segment_base(&self, task_id: &str) -> i64 {
        match self.db.load_audio_url(task_id).await {
            Ok(Some(audio)) if !audio.is_empty() => {}
            _ => return 0,
        }
        let Ok(Some(t)) = self.db.load_task_by_id(task_id).await else {
            return 0;
        };
        if t.file_name.is_empty() {
            return 0;
        }
        let dest = std::path::Path::new(&t.save_dir).join(&t.file_name);
        let temp = PathBuf::from(format!("{}{}", dest.display(), downloader::TEMP_EXT));
        if tokio::fs::try_exists(&temp).await.unwrap_or(false) {
            return 0;
        }
        tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len() as i64)
            .unwrap_or(0)
    }

    /// HTTP Basic 认证注入（见 [`crate::site_auth`]）。
    ///
    /// - `http_user` 非空：生成 `Authorization: Basic` 覆盖 extra_headers 中
    ///   既有同名头；`save` 为 true 且 URL 属 http(s) 时按站点键并入会话凭据库，
    ///   由 [`Self::flush_site_auth`] 批末写回。
    /// - `http_user` 为空：extra_headers 无 Authorization 且站点凭据库命中
    ///   该 URL 的站点键时自动注入。
    ///
    /// 凭据库读写失败仅记日志，不阻断建任务。
    async fn apply_site_auth(
        &self,
        session: &mut SiteAuthSession,
        url: &str,
        extra_headers: &mut HashMap<String, String>,
        http_user: &str,
        http_password: &str,
        save: bool,
    ) {
        use crate::site_auth;
        if !http_user.is_empty() {
            site_auth::inject_basic_auth(extra_headers, http_user, http_password);
            if save
                && let Some(key) = site_auth::site_key(url)
                && self.load_site_auth_store(session).await
                && let Some(store) = session.store.as_mut()
            {
                store.insert(
                    key,
                    site_auth::SiteCredential {
                        user: http_user.to_string(),
                        pass: http_password.to_string(),
                    },
                );
                session.dirty = true;
            }
            return;
        }
        if site_auth::has_authorization(extra_headers) {
            return;
        }
        let Some(key) = site_auth::site_key(url) else {
            return;
        };
        if !self.load_site_auth_store(session).await {
            return;
        }
        if let Some(cred) = session.store.as_ref().and_then(|store| store.get(&key)) {
            site_auth::inject_basic_auth(extra_headers, &cred.user, &cred.pass);
            log_info!("[site-auth] applied saved credential for {}", key);
        }
    }

    /// 确保会话已载入凭据库（每个会话至多读一次库）；读取失败返回 `false`。
    async fn load_site_auth_store(&self, session: &mut SiteAuthSession) -> bool {
        if session.store.is_some() {
            return true;
        }
        if session.unavailable {
            return false;
        }
        match self
            .db
            .get_config(crate::site_auth::SITE_AUTH_CONFIG_KEY)
            .await
        {
            Ok(value) => {
                session.store = Some(crate::site_auth::parse_store(&value.unwrap_or_default()));
                true
            }
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "load site credential store",
                    &error,
                );
                session.unavailable = true;
                false
            }
        }
    }

    /// 把本批新保存的站点凭据一次性写回（无变更不写）。
    async fn flush_site_auth(&self, session: &mut SiteAuthSession) {
        if !session.dirty {
            return;
        }
        let Some(store) = session.store.as_ref() else {
            return;
        };
        session.dirty = false;
        if let Err(e) = self
            .db
            .set_config(
                crate::site_auth::SITE_AUTH_CONFIG_KEY,
                &crate::site_auth::serialize_store(store),
            )
            .await
        {
            log_info!("[site-auth] save credential error: {}", e);
        }
    }

    /// BT 被禁用时，新建任务（magnet / 种子字节 / 解出真实地址为 BT 的 thunder 链接）
    /// 是否会被 [`Self::create_task`] 拒绝。BT 启用时恒为 `false`。
    ///
    /// 供 daemon 在建任务前预检，以便把 [`BT_DISABLED_MESSAGE`] 回报给客户端，
    /// 而不是等 `create_task` 返回无原因的 `None`。
    pub fn rejects_new_bt_task(&self, url: &str, torrent_file_bytes: &[u8]) -> bool {
        if self.bt_config.enabled {
            return false;
        }
        if !torrent_file_bytes.is_empty() || is_bt_url(url) {
            return true;
        }
        crate::thunder::is_thunder_url(url)
            && crate::thunder::decode_thunder_url(url).is_ok_and(|real| is_bt_url(&real))
    }

    /// BT 被禁用时，`task_ids` 中会被恢复流程拒绝的 BT 任务 ID 子集（含已完成
    /// 待重新做种的任务）；BT 启用时恒为空。
    ///
    /// 供 daemon 在 resume 前预检：被拒的单任务恢复须向客户端回报
    /// [`BT_DISABLED_MESSAGE`]，而不是返回无信息的成功。
    pub async fn bt_disabled_resume_rejections(
        &self,
        task_ids: &[String],
    ) -> Result<Vec<String>, crate::db::DbError> {
        if self.bt_config.enabled {
            return Ok(Vec::new());
        }
        Ok(self
            .db
            .load_tasks_by_ids(task_ids)
            .await?
            .into_iter()
            .filter(|task| is_bt_url(&task.url))
            .map(|task| task.task_id)
            .collect())
    }

    /// 创建下载任务，返回新任务 ID（被拒绝或落库失败时 `None`）。
    ///
    /// `spec.start_paused` = 稍后下载：任务以 paused(2) 落库，不占并发、
    /// 不进等待队列，由「启动队列」按序恢复或用户手动恢复；后台元数据
    /// 探测照常进行，UI 能立即显示文件名/大小。
    ///
    /// 与 [`Self::create_tasks_batch`] 共用同一条「准备 → 落库 → 发布」流水线，
    /// 单个创建就是只含一项、无幂等记录的批次，创建事实以 `TaskChanged` 发布。
    pub async fn create_task(&mut self, spec: NewTaskSpec) -> Option<String> {
        match self
            .create_tasks_inner(None, vec![spec], CreationAnnounce::Single)
            .await
        {
            Ok(outcomes) => match outcomes.into_iter().next() {
                Some(BatchItemOutcome::Created(task_id)) => Some(task_id),
                Some(BatchItemOutcome::Rejected(reason)) => {
                    log_info!("[manager] task creation rejected: {}", reason);
                    None
                }
                None => None,
            },
            Err(error) => {
                crate::logger::report_error("download-manager", "create task", &error);
                None
            }
        }
    }

    /// 批量创建任务（幂等）。返回与 `specs` 同序的逐项结果；逐项拒绝（thunder
    /// 解码失败 / BT 被禁用 / checksum 畸形…）不影响同批其它项。
    ///
    /// - 同一 `batch_id` 已提交过 → 原样返回首次结果，不创建任何任务；
    ///   客户端可对状态不明的批次原样重发。
    /// - 全部任务行与幂等记录在**一个事务**内落库；失败整体回滚并返回 `Err`。
    /// - 去重候选与站点凭据库每批只读一次；`dedup_same_url` 开启时批内重复
    ///   链接解析为首项的任务 ID（与逐条创建的结果一致）。
    /// - 先发一条 [`EngineEvent::TasksChanged`]（整批创建事实），随后才逐项
    ///   发 webhook / 解析失败进度 / 探测 / 入队；队列位置广播仍由脏标记合并。
    pub async fn create_tasks_batch(
        &mut self,
        batch_id: &str,
        specs: Vec<NewTaskSpec>,
    ) -> Result<Vec<BatchItemOutcome>, DbError> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }
        self.create_tasks_inner(Some(batch_id), specs, CreationAnnounce::Batch)
            .await
    }

    /// 单个 / 批量创建的唯一实现：读上下文 → 逐项准备 → 单事务落库 → 发布。
    async fn create_tasks_inner(
        &mut self,
        batch_id: Option<&str>,
        specs: Vec<NewTaskSpec>,
        announce: CreationAnnounce,
    ) -> Result<Vec<BatchItemOutcome>, DbError> {
        if let Some(batch_id) = batch_id
            && let Some(stored) = self.db.load_create_batch_results(batch_id).await?
        {
            log_info!(
                "[manager] create batch {} already committed — replaying stored results",
                batch_id
            );
            return serde_json::from_str(&stored).map_err(|error| {
                DbError::InvalidConfig(format!("stored task create batch {batch_id}: {error}"))
            });
        }

        // (a) 准备：纯内存判定，不触库（上下文只读一次）。
        let mut context = self.begin_create_context(&specs).await?;
        let mut outcomes = Vec::with_capacity(specs.len());
        let mut prepared: Vec<PreparedTask> = Vec::with_capacity(specs.len());
        for spec in specs {
            match self.prepare_new_task(spec, &mut context).await {
                PrepareOutcome::Ready(task) => {
                    context.note_accepted(&task);
                    outcomes.push(BatchItemOutcome::Created(task.task_id.clone()));
                    prepared.push(*task);
                }
                PrepareOutcome::Deduped(existing_id) => {
                    outcomes.push(BatchItemOutcome::Created(existing_id));
                }
                PrepareOutcome::Rejected(reason) => {
                    outcomes.push(BatchItemOutcome::Rejected(reason));
                }
            }
        }
        self.flush_site_auth(&mut context.site_auth).await;

        // (b) 落库：全部行 + 幂等记录同事务。没有新行且无需记录时不开事务。
        let tasks = if prepared.is_empty() && batch_id.is_none() {
            Vec::new()
        } else {
            let results = match batch_id {
                Some(_) => Some(serde_json::to_string(&outcomes).map_err(|error| {
                    DbError::InvalidConfig(format!("serialize task create outcomes: {error}"))
                })?),
                None => None,
            };
            let rows: Vec<NewTaskRow<'_>> = prepared.iter().map(PreparedTask::row).collect();
            let record = batch_id
                .zip(results.as_deref())
                .map(|(batch_id, results)| CreateBatchRecord { batch_id, results });
            self.db.insert_new_tasks(&rows, record).await?
        };

        // (c) 发布。
        self.publish_created_tasks(prepared, tasks, announce).await;
        Ok(outcomes)
    }

    /// 读取本次创建调用的共享上下文。去重开关与活跃 URL 集合各至多读一次，
    /// 且仅当存在可比较 URL（非种子上传、URL 非空）时才读。
    async fn begin_create_context(
        &self,
        specs: &[NewTaskSpec],
    ) -> Result<CreateBatchContext, DbError> {
        let comparable = specs
            .iter()
            .any(|spec| spec.torrent_file_bytes.is_empty() && !spec.url.is_empty());
        let dedup = if comparable
            && self.db.get_config("dedup_same_url").await?.as_deref() == Some("true")
        {
            let candidates = self.db.active_task_urls().await?;
            let mut active = HashMap::with_capacity(candidates.len());
            for (task_id, url) in candidates {
                active
                    .entry(normalize_url_for_dedup(&url))
                    .or_insert(task_id);
            }
            Some(active)
        } else {
            None
        };
        Ok(CreateBatchContext {
            dedup,
            site_auth: SiteAuthSession::default(),
        })
    }

    /// 「准备」阶段：把一份 [`NewTaskSpec`] 变成可落库/可发布的 [`PreparedTask`]，
    /// 或判定为去重命中 / 被拒绝。除站点凭据库首次读取与 resolver 插件匹配外
    /// 不做任何 I/O。
    async fn prepare_new_task(
        &self,
        spec: NewTaskSpec,
        context: &mut CreateBatchContext,
    ) -> PrepareOutcome {
        let NewTaskSpec {
            mut url,
            save_dir,
            file_name,
            name_hint,
            segments,
            cookies,
            referrer,
            hint_file_size,
            torrent_file_bytes,
            proxy_url,
            user_agent,
            queue_id,
            checksum,
            ignore_tls_errors,
            mut extra_headers,
            selected_file_indices,
            unattended_selection,
            method,
            body,
            audio_url,
            start_paused,
            group_id,
            resolver_item,
            resolver_plugin_id: explicit_resolver,
            http_user,
            http_password,
            save_site_auth,
            category_route,
        } = spec;
        // thunder:// 是迅雷专有的 base64 封装协议，真实下载地址被包裹在
        // `AA`…`ZZ` 之间。先于站点鉴权匹配和协议判定解出真实地址，后续
        // 全部按解出的真实协议处理；显示仍用原始 thunder 链接（写入
        // origin_url）方便用户核对来源。解码失败直接拒绝建任务。
        let thunder_origin_url = if crate::thunder::is_thunder_url(&url) {
            match crate::thunder::decode_thunder_url(&url) {
                Ok(decoded) => Some(std::mem::replace(&mut url, decoded)),
                Err(e) => {
                    log_info!("[manager] thunder link decode failed: {}", e);
                    return PrepareOutcome::Rejected(CreateRejection::InvalidThunderLink(
                        e.to_string(),
                    ));
                }
            }
        } else {
            None
        };
        // BT 禁用门禁：BT 被用户显式禁用时，拒绝创建 magnet 链接或种子文件任务。
        // 调用方（daemon）可用 `rejects_new_bt_task` 预检；这里是兜底，
        // 保证绕过预检的调用方也不会落库。
        if self.rejects_new_bt_task(&url, &torrent_file_bytes) {
            log_info!(
                "[manager] BitTorrent download rejected: {}",
                BT_DISABLED_MESSAGE
            );
            return PrepareOutcome::Rejected(CreateRejection::BtDisabled);
        }
        // 无 eD2K 后端（编译期剔除）：ed2k:// 链接同样明确拒绝，不落库。
        #[cfg(not(feature = "ed2k"))]
        if is_ed2k_url(&url) {
            log_info!(
                "[manager] ed2k download rejected: {}",
                UNSUPPORTED_PROTOCOL_MESSAGE
            );
            return PrepareOutcome::Rejected(CreateRejection::UnsupportedProtocol);
        }
        // URL 去重（config `dedup_same_url`，默认关闭）：种子上传没有真实 URL 可比较。
        if torrent_file_bytes.is_empty()
            && !url.is_empty()
            && let Some(active) = &context.dedup
            && let Some(existing_id) = active.get(&normalize_url_for_dedup(&url))
        {
            log_info!(
                "[manager] create_task: URL already active as task {} — dedup_same_url returns existing task instead of creating a duplicate",
                existing_id
            );
            return PrepareOutcome::Deduped(existing_id.clone());
        }
        // BUG-CHECKSUM-PREFIX：畸形 checksum spec（未知算法 / 哈希非十六进制 /
        // 长度不符）此前要等一次完整下载结束后 verify_checksum 才报错。提前
        // 到任务创建入口校验，用户能立即得到反馈而不是白等一次下载。
        if !checksum.is_empty()
            && let Err(e) = crate::downloader::normalize_checksum_spec(&checksum)
        {
            log_info!(
                "[manager] invalid checksum spec rejected at task creation: {}",
                e
            );
            return PrepareOutcome::Rejected(CreateRejection::InvalidChecksum(e.to_string()));
        }
        // HTTP Basic 认证：显式凭据 → 生成 Authorization 头
        // （覆盖捕获到的同名头）并按需保存到站点凭据库；未显式提供且头中
        // 无 Authorization → 自动套用该站点已保存的凭据。注入发生在请求
        // 上下文持久化之前，resume / probe 全链路自动携带。
        self.apply_site_auth(
            &mut context.site_auth,
            &url,
            &mut extra_headers,
            &http_user,
            &http_password,
            save_site_auth,
        )
        .await;
        // 单任务 UA 也属于请求身份。将显式覆盖值并入已持久化请求头，使暂停、
        // 进程重启后的 resume 与首次请求一致；调用方已捕获的 User-Agent 优先。
        if !user_agent.is_empty()
            && !extra_headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("user-agent"))
        {
            extra_headers.insert("User-Agent".to_string(), user_agent.clone());
        }
        // 任务必属队列：未指定时归入内置主队列（'' 不再是有效归属，统一
        // 覆盖旧客户端信号 / aria2 / REST / CLI 等所有创建入口）。
        let queue_id = if queue_id.is_empty() {
            MAIN_QUEUE_ID.to_string()
        } else {
            queue_id
        };
        // ED2K 链接自带文件名/大小/root hash：调用方未显式给名时从链接回填，
        // 并把 hint_file_size 设为链接声明的大小（run_ed2k_download 以链接为准）。
        #[cfg(feature = "ed2k")]
        let (file_name, hint_file_size) = if is_ed2k_url(&url) {
            match crate::ed2k::link::parse_ed2k_link(&url) {
                Ok(link) => {
                    let name = if file_name.trim().is_empty() {
                        link.file_name.clone()
                    } else {
                        file_name
                    };
                    (name, link.total_bytes as i64)
                }
                Err(_) => (file_name, hint_file_size),
            }
        } else {
            (file_name, hint_file_size)
        };
        // aria2 `out` / REST / 新建对话框 `out=` 允许 `子目录/文件名`（相对 save_dir）。
        // 下载器只把 file_name 当单个组件清洗（防穿越），子目录必须在落库前并入
        // save_dir，否则会被压平成 `子目录_文件名`（#716）。BT 的名字另有语义，不拆。
        let is_bt = is_magnet(&url) || !torrent_file_bytes.is_empty();
        let (save_dir, file_name, has_subdir) = if is_bt {
            (save_dir, file_name, false)
        } else {
            let (split_dir, file_name) = split_relative_file_name(&save_dir, &file_name);
            let has_subdir = split_dir != save_dir;
            // 显式名（用户改名 / `out=`）缺类型扩展名：先用无需联网的确定性参照补全，
            // 建任务即定名——列表、「文件已存在」询问与下载中都显示带扩展名的名字，
            // hint 任务（一次性 URL，启动期不探测）也不必等到完成期。
            let file_name = naming::complete_extension_offline(&file_name, &name_hint, &url)
                .unwrap_or(file_name);
            (split_dir, file_name, has_subdir)
        };
        // When segments <= 0 ("auto"), store 0 in DB and let the downloader
        // dynamically calculate the optimal count after probing file size,
        // CPU cores, and bandwidth.
        let segments = if segments <= 0 { 0 } else { segments };

        // Determine the URL to store in DB.  For .torrent file tasks, use a
        // sentinel URL since the actual content is in torrent_file_bytes.
        let db_url = if !torrent_file_bytes.is_empty() {
            "torrent-file://local".to_string()
        } else {
            url.clone()
        };
        // 展示用原始来源：thunder 原链接优先；其次是被哨兵替换掉的 http(s) 来源。
        let origin_url = if let Some(origin) = thunder_origin_url {
            origin
        } else if db_url != url && (url.starts_with("http://") || url.starts_with("https://")) {
            url.clone()
        } else {
            String::new()
        };
        let extra_headers_json = if extra_headers.is_empty() {
            String::new()
        } else {
            match serde_json::to_string(&extra_headers) {
                Ok(json) => json,
                Err(error) => {
                    return PrepareOutcome::Rejected(CreateRejection::InvalidHeaders(
                        error.to_string(),
                    ));
                }
            }
        };
        let (resolver_plugin_id, choice_pending, resolver_fail_closed) = self
            .plugin_bind_resolver(&url, &explicit_resolver, &group_id, &resolver_item)
            .await;
        // 二段解析任务（resolver_item 非空）不进入多插件选择：组成员由预览同款规则 / 显式
        // 绑定定案，RSS 条目沿用 `route_resolver` 的首选。
        let resolver_choice_pending = choice_pending && resolver_item.is_empty();
        // 稍后下载的 hint 任务（一次性 URL，不探测）：名字为空时按 URL 推断占位名
        // （含预签名 URL 的 `response-content-disposition`），随创建行一并落库，
        // 供 UI 立即显示；HTTP 完成期按实际 GET 响应精修。无网络 I/O。
        let provisional_name = if start_paused
            && resolver_plugin_id.is_empty()
            && resolver_item.is_empty()
            && !is_bt
            && hint_file_size != 0
            && file_name.is_empty()
            && (db_url.starts_with("http://") || db_url.starts_with("https://"))
        {
            naming::name_from_url(&db_url).map(|resolved| {
                let inferred = resolved.source != naming::NameSource::QueryDisposition;
                (resolved.name, inferred)
            })
        } else {
            None
        };
        // 分类目录路由：按初始名选初始目录（常见情形直接写进分类目录，完成时零移动），
        // 开始写盘前与完成前再按更确定的名字复核（`category_route`）。BT / eD2K 要原地
        // 做种 / 共享，组成员与 `out=子目录/名字` 的相对结构由来源决定，均不路由。解析
        // 插件任务的链接是页面地址，只看显式名；HLS / DASH 按成品 `.mp4` 预判。
        let routable = !is_bt && !is_ed2k_url(&url) && group_id.is_empty() && !has_subdir;
        let (save_dir, category_route) = match category_route.filter(|_| routable) {
            None => (save_dir, String::new()),
            Some(mut route) => {
                let link = if resolver_plugin_id.is_empty() {
                    url.as_str()
                } else {
                    ""
                };
                let known = match &provisional_name {
                    Some((name, _)) if file_name.is_empty() => name.as_str(),
                    _ => file_name.as_str(),
                };
                let output = crate::category_route::output_name(known, link);
                let initial = route
                    .save_dir_for(
                        output.as_deref().unwrap_or(known),
                        link,
                        crate::category_route::source_address(&origin_url, &db_url),
                    )
                    .map(str::to_owned);
                route.base_dir = save_dir;
                let column = match route.to_column() {
                    Ok(column) => column,
                    Err(error) => {
                        crate::logger::report_warning(
                            "download-manager",
                            "encode category route",
                            &error,
                        );
                        String::new()
                    }
                };
                (initial.unwrap_or(route.base_dir), column)
            }
        };
        let request_method = downloader::persisted_request_method(method.as_deref());
        PrepareOutcome::Ready(Box::new(PreparedTask {
            task_id: Uuid::now_v7().to_string(),
            url,
            db_url,
            origin_url,
            save_dir,
            file_name,
            provisional_name,
            segments,
            hint_file_size,
            torrent_file_bytes,
            proxy_url,
            user_agent,
            queue_id,
            checksum,
            ignore_tls_errors,
            cookies,
            referrer,
            extra_headers,
            extra_headers_json,
            selected_file_indices,
            method,
            body,
            audio_url,
            start_paused,
            group_id,
            resolver_item,
            unattended_selection,
            resolver_plugin_id,
            resolver_choice_pending,
            resolver_fail_closed,
            is_bt,
            category_route,
            request_method,
        }))
    }

    /// 「发布」阶段。创建事实先于所有进度帧——尚未 spawn/probe，完整事实必定
    /// 先于任何进度帧；不用完成后再读库补发，也就不会把已完成任务退回 pending。
    /// 建组/裂变仍在尾部统一快照（`suppress_bulk_broadcasts`）。
    async fn publish_created_tasks(
        &mut self,
        prepared: Vec<PreparedTask>,
        tasks: Vec<TaskInfo>,
        announce: CreationAnnounce,
    ) {
        match announce {
            CreationAnnounce::Single => {
                if !self.suppress_bulk_broadcasts {
                    for task in tasks {
                        self.sink.emit(EngineEvent::TaskChanged(Box::new(task)));
                    }
                }
            }
            CreationAnnounce::Batch => {
                if !tasks.is_empty() {
                    self.sink.emit(EngineEvent::TasksChanged(tasks));
                }
            }
        }
        for task in prepared {
            self.publish_created_task(task).await;
        }
    }

    /// 单个已落库任务的后续发布：webhook、解析失败进度、稍后下载探测、入队。
    async fn publish_created_task(&mut self, task: PreparedTask) {
        let has_resolver = task.has_resolver();
        let resolver_unavailable = task.resolver_unavailable();
        let initial_status = task.initial_status();
        let PreparedTask {
            task_id,
            url,
            db_url,
            save_dir,
            file_name,
            provisional_name,
            segments,
            hint_file_size,
            torrent_file_bytes,
            proxy_url,
            user_agent,
            queue_id,
            checksum,
            ignore_tls_errors,
            cookies,
            referrer,
            extra_headers,
            selected_file_indices,
            method,
            body,
            audio_url,
            start_paused,
            resolver_item,
            resolver_plugin_id,
            is_bt,
            ..
        } = task;

        // Webhook：task.created（落库成功后，早于任何早退分支；`url` 用真实
        // 来源而非 torrent-file:// 哨兵）。
        let created_event = self.webhook_task_event(
            crate::webhook::WebhookEventKind::TaskCreated,
            crate::webhook::WebhookTask {
                id: task_id.clone(),
                file_name: file_name.clone(),
                url,
                save_dir: save_dir.clone(),
                total_bytes: hint_file_size,
                status: initial_status,
                error_message: String::new(),
            },
            &queue_id,
        );
        self.emit_task_event(created_event);

        // fail-closed：resolver_item 非空但未命中插件（或 plugins feature 关，
        // has_resolver 恒空）→ 任务已以 error 落库，绝不发起对 source_url 的下载
        // （那会把网页 HTML/分享页当直链保存）。
        if resolver_unavailable {
            self.sink.emit(EngineEvent::TaskProgress {
                task_id,
                status: 4,
                downloaded_bytes: 0,
                total_bytes: 0,
                speed: 0,
                file_name,
                save_dir,
                url: db_url,
                error_message: RESOLVER_UNAVAILABLE_MESSAGE.to_string(),
                upload_speed_bps: 0,
                uploaded_bytes: 0,
                seeding_status: 0,
                seeding_message: String::new(),
                seeding_time_secs: 0,
            });
            return;
        }
        if start_paused {
            // 稍后下载：不启动、不排队。后台 probe 让 UI 尽快拿到文件名/
            // 大小；带 resolver（探测原始页面 URL 无意义）或 BT（无 HTTP
            // 元数据可探）任务跳过，语义与排队/直启分支一致。
            if !has_resolver && !is_bt && hint_file_size != 0 {
                // 占位名已随创建行落库，这里只通知宿主刷新显示。
                if let Some((name, _)) = provisional_name {
                    self.sink.emit(EngineEvent::TaskMetaProbed {
                        task_id,
                        file_name: name,
                        total_bytes: 0,
                    });
                }
            } else if !has_resolver
                && !is_bt
                && self.probes_outstanding.load(Ordering::Relaxed) < META_PROBE_WINDOW
            {
                let egress = self.task_http_context(
                    &db_url,
                    &proxy_url,
                    &user_agent,
                    &queue_id,
                    ignore_tls_errors,
                    None,
                );
                let mut probe_spec = downloader::RequestSpec::from_captured(
                    method.as_deref(),
                    cookies,
                    referrer,
                    extra_headers,
                    body,
                );
                probe_spec.egress = egress.router.clone();
                let (probe_client, probe_proxy) = (egress.client, egress.proxy);
                let (interceptor, task_ctx, run_meta) = self
                    .meta_probe_extras(
                        &task_id,
                        &db_url,
                        &file_name,
                        &save_dir,
                        &proxy_url,
                        ignore_tls_errors,
                        &user_agent,
                        &queue_id,
                    )
                    .await;
                self.spawn_meta_probe(
                    task_id,
                    db_url,
                    file_name,
                    probe_spec,
                    probe_client,
                    probe_proxy,
                    interceptor,
                    task_ctx,
                    run_meta,
                );
            }
            return;
        }

        let queued = QueuedTask {
            task_id,
            url: db_url.clone(),
            save_dir,
            file_name,
            segments,
            is_resume: false,
            cookies,
            referrer,
            hint_file_size,
            torrent_file_bytes,
            proxy_url,
            user_agent,
            queue_id,
            checksum,
            ignore_tls_errors,
            extra_headers,
            selected_file_indices,
            method,
            body,
            audio_url,
            resolver_plugin_id,
            resolved: false,
            range_supported: false,
            resolver_item,
            source_url: db_url,
            resolver_patch: None,
        };
        self.enqueue_persisted_task(queued, has_resolver).await;
    }

    /// 排队分发：有容量（或 BT 恒直启）立即启动，否则入 `pending_queue` 并广播
    /// 位置。`create_task` 尾部与清单裂变（`apply_manifest_fission`）的兄弟任务
    /// 分发共用，避免复制粘贴（B5 契约）。`has_resolver` 为 true 时跳过
    /// meta-probe（探测原始页面 URL 无意义，二段解析会取得真实直链）。
    async fn enqueue_persisted_task(&mut self, queued: QueuedTask, has_resolver: bool) {
        // 与 create_task 建任务时的判定式一致（is_torrent_file_url 未涵盖——
        // 该分支历来只按 magnet/内嵌种子字节判定，保持原行为不变）。
        let is_bt = is_magnet(&queued.url) || !queued.torrent_file_bytes.is_empty();
        if is_bt || (self.has_capacity() && self.has_queue_capacity(&queued.queue_id)) {
            self.do_start_task(queued).await;
            // If do_start_task failed early (e.g. BT session init), the slot
            // was freed — drain the queue so pending tasks can proceed.
            self.drain_queue().await;
            self.sync_queue_occupancy();
        } else {
            log_info!(
                "[manager] queuing task {} (active={}, max={}, queue={})",
                queued.task_id,
                self.active_tasks.len(),
                self.max_concurrent,
                queued.queue_id
            );
            // 保存探测所需信息（queued 即将被 move 进队列）
            let probe_tid = queued.task_id.clone();
            let probe_url = queued.url.clone();
            let probe_name = queued.file_name.clone();
            let queued_hint = queued.hint_file_size;
            // 只探测队首窗口内的任务：批量导入的长队列不为每个任务发探测，
            // 轮到下载时自然得到名字与大小。
            let in_probe_window = self.pending_queue.len() < META_PROBE_WINDOW;
            let probe_context = (!has_resolver && queued_hint == 0 && in_probe_window).then(|| {
                let mut spec = downloader::RequestSpec::from_captured(
                    queued.method.as_deref(),
                    queued.cookies.clone(),
                    queued.referrer.clone(),
                    queued.extra_headers.clone(),
                    queued.body.clone(),
                );
                let egress = self.task_http_context(
                    &queued.url,
                    &queued.proxy_url,
                    &queued.user_agent,
                    &queued.queue_id,
                    queued.ignore_tls_errors,
                    None,
                );
                spec.egress = egress.router.clone();
                (spec, egress.client, egress.proxy)
            });
            let probe_extras = if probe_context.is_some() {
                Some(
                    self.meta_probe_extras(
                        &queued.task_id,
                        &queued.url,
                        &queued.file_name,
                        &queued.save_dir,
                        &queued.proxy_url,
                        queued.ignore_tls_errors,
                        &queued.user_agent,
                        &queued.queue_id,
                    )
                    .await,
                )
            } else {
                None
            };
            // 新增只会使此队列变为占用，不必重扫全部待启动项。
            self.mark_queue_occupied(&queued.queue_id);
            self.pending_queue.push_back(queued);
            if !self.suppress_bulk_broadcasts {
                self.queue_positions_dirty = true;
            }
            // 带 resolver 的任务跳过 probe（探测原始页面 URL 无意义）；hint 任务
            // （浏览器扩展给了大小）不发 HEAD——一次性签名 URL 会被任何请求消耗，
            // 只落 URL 推断的占位名，真名由实际下载的 GET 响应在完成期确定。
            if !has_resolver && queued_hint != 0 {
                self.fill_provisional_name(&probe_tid, &probe_url, &probe_name)
                    .await;
            } else if let Some((probe_spec, probe_client, probe_proxy)) = probe_context
                && let Some((interceptor, task_ctx, run_meta)) = probe_extras
            {
                self.spawn_meta_probe(
                    probe_tid,
                    probe_url,
                    probe_name,
                    probe_spec,
                    probe_client,
                    probe_proxy,
                    interceptor,
                    task_ctx,
                    run_meta,
                );
            }
        }
    }

    /// 任务有效 UA 的解析优先级：任务 > 队列 > 全局。任务 client 与多 CDN
    /// pinned client 共用此结果（两者 UA 必须逐字节一致）。
    fn resolved_task_ua<'a>(&'a self, user_agent: &'a str, queue_id: &str) -> &'a str {
        let queue_ua = self
            .queues
            .get(queue_id)
            .map(|q| q.default_user_agent.as_str())
            .unwrap_or("");
        if !user_agent.is_empty() {
            user_agent
        } else if !queue_ua.is_empty() {
            queue_ua
        } else {
            self.global_user_agent.as_str()
        }
    }

    /// 折算多 CDN 聚合的任务级输入（方案 §3.2 的 manager 侧条件）：
    /// 全局开关 && 未忽略 TLS 错误（§1.2 规则 1）&& 无有效代理（任务级或
    /// 全局级，含 System 解析结果；§11-5 代理路由优先，钉 IP 让位）。
    /// URL scheme/Range 验证/域名级条件由下载路径继续校验。
    fn cdn_task_input(
        &self,
        ignore_tls_errors: bool,
        task_proxy: &ProxyConfig,
        user_agent: &str,
    ) -> crate::cdn::CdnTaskInput {
        use crate::proxy_config::ProxyMode;
        crate::cdn::CdnTaskInput {
            enabled: self.cdn_multi_enabled
                && !ignore_tls_errors
                && task_proxy.mode == ProxyMode::None,
            max_nodes: self
                .cdn_max_nodes
                .clamp(0, crate::cdn::MAX_NODES_LIMIT as i32) as usize,
            user_agent: user_agent.to_string(),
        }
    }

    /// 本任务的系统 PAC 路由：任务级代理优先（不参与）；全局 System / Auto 时
    /// 由 spawned task 起跑前按链接修正出口（[`crate::pac_route`]）。
    fn pac_routing(&self, task_proxy_url: &str) -> crate::pac_route::PacRouting {
        use crate::pac_route::PacRouting;
        if !task_proxy_url.is_empty() {
            return PacRouting::Off;
        }
        match self.proxy_config.mode {
            ProxyMode::System => PacRouting::System,
            ProxyMode::Auto => PacRouting::Auto,
            ProxyMode::None | ProxyMode::Manual => PacRouting::Off,
        }
    }

    /// 折算多网卡聚合的任务级输入：全局开关关闭 → `None`（多段路径零行为
    /// 变化）。任务走代理（任务级/全局级，含 System 解析结果）或持有 Auto
    /// 多路径上下文时出口是代理，标记 `blocked_by_proxy`，coordinator 只上报
    /// `links_off/proxy` 不做规划。其余条件（地址族、VPN、同局域网）由
    /// [`crate::multi_nic::prepare_links`] 在下载路径上判定。
    fn multi_nic_input(
        &self,
        ignore_tls_errors: bool,
        task_proxy: &ProxyConfig,
        has_auto_paths: bool,
        user_agent: &str,
    ) -> Option<Arc<crate::multi_nic::MultiNicInput>> {
        use crate::proxy_config::ProxyMode;
        self.multi_nic_enabled.then(|| {
            Arc::new(crate::multi_nic::MultiNicInput {
                user_agent: user_agent.to_string(),
                ignore_tls_errors,
                blocked_by_proxy: task_proxy.mode != ProxyMode::None || has_auto_paths,
            })
        })
    }

    /// `ProxyMode::Auto` 的启动期路由决策（每任务一次）。
    ///
    /// 常规启动构造多路径上下文：直连与全部候选代理都是节点池里的路径，
    /// 按跨任务先验选起飞路径（[`pick_auto_takeoff`]：近期走不通的路径不
    /// 起飞；代理先验须领先直连 [`AUTO_START_PROXY_RATIO`] 倍才以代理起飞），
    /// 其余路径作为备选——探测阶段由 [`crate::auto_probe`] 错峰竞速，多段
    /// 下载中由 coordinator 同窗实测调度。局部续传同样适用——每个分段请求
    /// 都经 validator 校验，路径不一致在写盘前被拒绝。`forced_route` 来自
    /// 一次性备用链路或插件解析钉定的出口：它独立于通用自动重试配额、优先于先验，
    /// 且只走单一路径（仍带无备选的上下文，使该路径的实测速率回写跨任务先验）。
    fn auto_route_decision(
        &self,
        url: &str,
        user_agent: &str,
        ignore_tls_errors: bool,
        forced_route: Option<ForcedRoute>,
    ) -> Option<(
        ProxyConfig,
        &'static str,
        Option<Arc<crate::auto_proxy::AutoProxyCtx>>,
    )> {
        use crate::auto_proxy::{self, AutoProxyCtx, PathCandidate, RoutePath, route};
        if self.proxy_config.mode != ProxyMode::Auto {
            return None;
        }
        let host = crate::segment_coordinator::extract_host(url);
        // 单路径上下文：无备选，仅用于把该路径的实测速率回写先验。
        let single_path = |route: RoutePath, label: &'static str| {
            host.clone().filter(|_| !ignore_tls_errors).map(|host| {
                Arc::new(AutoProxyCtx {
                    start_prior_bps: crate::route_health::path_prior(&host, route).map(|p| p.bps),
                    host,
                    user_agent: user_agent.to_string(),
                    start_route: route,
                    start_label: label,
                    alternates: Vec::new(),
                })
            })
        };
        if let Some(ForcedRoute {
            target: AutoFailoverTarget::Direct,
            label,
        }) = forced_route
        {
            return Some((
                ProxyConfig::default(),
                label,
                single_path(RoutePath::Direct, label),
            ));
        }
        let candidates = auto_proxy::resolve_candidates(&self.proxy_config);
        if candidates.is_empty() {
            return Some((ProxyConfig::default(), route::DIRECT, None));
        }
        if let Some(ForcedRoute {
            target: AutoFailoverTarget::Proxy(source),
            label,
        }) = forced_route
        {
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.source == source)
            {
                return Some((
                    candidate.config.clone(),
                    label,
                    single_path(RoutePath::Proxy(source), label),
                ));
            }
            return Some((
                ProxyConfig::default(),
                route::DIRECT_FAILOVER,
                single_path(RoutePath::Direct, route::DIRECT_FAILOVER),
            ));
        }
        let Some(host) = host else {
            return Some((ProxyConfig::default(), route::DIRECT, None));
        };
        // validator 不一致（代理命中不同 CDN edge）的 host：只走直连。
        if crate::route_health::no_switch_active(&host) {
            return Some((ProxyConfig::default(), route::DIRECT, None));
        }
        let prior = |route: RoutePath| crate::route_health::path_prior(&host, route).map(|p| p.bps);
        let mut paths: Vec<PathCandidate> = Vec::with_capacity(candidates.len() + 1);
        paths.push(PathCandidate {
            route: RoutePath::Direct,
            config: ProxyConfig::default(),
            prior_bps: prior(RoutePath::Direct),
        });
        for candidate in candidates {
            let route = RoutePath::Proxy(candidate.source);
            paths.push(PathCandidate {
                route,
                config: candidate.config,
                prior_bps: prior(route),
            });
        }
        let blocked: Vec<bool> = paths
            .iter()
            .map(|p| crate::route_health::path_blocked(&host, p.route))
            .collect();
        let start_index = pick_auto_takeoff(&paths, &blocked);
        let start = paths.remove(start_index);
        let label = match start.route {
            // Auto 候选只含直连与代理；网卡链路只由多网卡聚合在 coordinator 挂入。
            RoutePath::Direct | RoutePath::Link(_) => route::DIRECT,
            RoutePath::Proxy(source) => route::with_source(route::PROXY_CACHED, source),
        };
        // 忽略 TLS 错误的任务不做多路径（备选路径 client 恒校验证书）。
        let ctx = (!ignore_tls_errors).then(|| {
            Arc::new(AutoProxyCtx {
                host,
                user_agent: user_agent.to_string(),
                start_route: start.route,
                start_prior_bps: start.prior_bps,
                start_label: label,
                alternates: paths,
            })
        });
        Some((start.config, label, ctx))
    }

    /// 全局 client。System 代理模式下按系统代理设置的指纹缓存：系统代理开关/地址
    /// 变化时才重建（构建一次约 70ms，不能每任务重建），使新任务的出口与
    /// `resolve()` 得到的代理保持一致；其余模式直接复用 `self.client`。
    fn global_client(&self, resolved: &ProxyConfig) -> Client {
        if self.proxy_config.mode != ProxyMode::System {
            return self.client.clone();
        }
        let fingerprint = crate::client_cache::proxy_fingerprint(resolved);
        let mut slot = self
            .system_client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((cached, client)) = slot.as_ref()
            && *cached == fingerprint
        {
            return client.clone();
        }
        match downloader::build_client(&self.proxy_config, &self.global_user_agent) {
            Ok(client) => {
                *slot = Some((fingerprint, client.clone()));
                client
            }
            Err(e) => {
                log_info!("[manager] failed to rebuild system-proxy client: {}", e);
                self.client.clone()
            }
        }
    }

    /// 为当前任务解析代理/UA/TLS 策略并构建一致的 HTTP 上下文。
    ///
    /// 站点代理规则只对「跟随全局」（`proxy_url` 为空）的任务生效：链接命中规则时任务
    /// 出口取该规则；未命中时取全局模式（含 Auto 决策）。插件解析过的任务传**源链接**
    /// （页面 URL）：解析产物视同一次重定向，最终链接由 `rule_route::apply` 重匹配——命中
    /// 他条规则才改道，未命中沿用源链接的出口，使解析与下载同出口。`forced_route` 为
    /// 已排程的一次性备用链路或解析钉定的出口（仅任务启动路径消费），client 与标签同源于它。
    fn task_http_context(
        &self,
        url: &str,
        proxy_url: &str,
        user_agent: &str,
        queue_id: &str,
        ignore_tls_errors: bool,
        forced_route: Option<ForcedRoute>,
    ) -> TaskEgress {
        let queue_ua = self
            .queues
            .get(queue_id)
            .map(|q| q.default_user_agent.as_str())
            .unwrap_or("");
        let resolved_ua = self.resolved_task_ua(user_agent, queue_id);
        let rules = if proxy_url.is_empty() {
            self.proxy_config.rules.clone()
        } else {
            None
        };
        if let Some(rules) = &rules
            && let Some(rule) = rules.first_match(url)
        {
            let proxy = rule.egress().resolve();
            let route = rule.route_label(&proxy);
            let index = rule.index();
            match crate::client_cache::get(&proxy, resolved_ua, ignore_tls_errors) {
                Ok(client) => {
                    return TaskEgress {
                        client,
                        proxy,
                        route,
                        auto_ctx: None,
                        router: Some(Arc::new(crate::rule_route::EgressRouter::new(
                            Arc::clone(rules),
                            Some(index),
                            resolved_ua,
                            ignore_tls_errors,
                        ))),
                        pac: crate::pac_route::PacRouting::Off,
                    };
                }
                Err(e) => {
                    crate::logger::report_error("download-manager", "build proxy-rule client", &e);
                    // base 必须为 None：命中该规则的每个请求都会进入 `client_for`，
                    // 重建失败时报错让任务失败，而不是悄悄走全局 client 泄漏出去。
                    return TaskEgress {
                        client: self.client.clone(),
                        proxy: self.proxy_config.resolve(),
                        route: "",
                        auto_ctx: None,
                        router: Some(Arc::new(crate::rule_route::EgressRouter::new(
                            Arc::clone(rules),
                            None,
                            resolved_ua,
                            ignore_tls_errors,
                        ))),
                        pac: crate::pac_route::PacRouting::Off,
                    };
                }
            }
        }
        let router = rules.map(|r| {
            Arc::new(crate::rule_route::EgressRouter::new(
                r,
                None,
                resolved_ua,
                ignore_tls_errors,
            ))
        });
        let pac = self.pac_routing(proxy_url);
        // ProxyMode::Auto 启动期决策（per-task 代理优先级更高，非空时跳过）。
        let auto = if proxy_url.is_empty() {
            self.auto_route_decision(url, resolved_ua, ignore_tls_errors, forced_route)
        } else {
            None
        };
        let (auto_override, (route, auto_ctx)) = match auto {
            Some((proxy, route, ctx)) => (Some(proxy), (route, ctx)),
            None => (None, ("", None)),
        };
        // Auto 判走代理时必须构建专属 client（全局 client 在 Auto 下恒直连）。
        let auto_needs_proxy_client =
            matches!(&auto_override, Some(p) if p.mode != ProxyMode::None);
        let needs_dedicated_client = !proxy_url.is_empty()
            || !user_agent.is_empty()
            || !queue_ua.is_empty()
            || ignore_tls_errors
            || auto_needs_proxy_client;
        if !needs_dedicated_client {
            let proxy = auto_override.unwrap_or_else(|| self.proxy_config.resolve());
            return TaskEgress {
                client: self.global_client(&proxy),
                proxy,
                route,
                auto_ctx,
                router,
                pac,
            };
        }

        let proxy = if !proxy_url.is_empty() {
            // `.resolve()`：把 system:// 哨兵（System 模式）现场具体化为
            // Manual/直连，使 FTP 直读 host/port 与 CDN 门槛判定一致生效。
            ProxyConfig::from_proxy_url(proxy_url).resolve()
        } else if let Some(p) = auto_override {
            p
        } else {
            self.proxy_config.resolve()
        };
        match crate::client_cache::get(&proxy, resolved_ua, ignore_tls_errors) {
            Ok(client) => TaskEgress {
                client,
                proxy,
                route,
                auto_ctx,
                router,
                pac,
            },
            Err(e) => {
                log_info!("[manager] failed to build per-task client: {}", e);
                // 构建失败降级为全局直连 client——不再对路由做任何声明
                // （空标签），避免「标签说代理、实际走直连」的可追溯性谎言。
                TaskEgress {
                    client: self.client.clone(),
                    proxy: self.proxy_config.resolve(),
                    route: "",
                    auto_ctx: None,
                    router,
                    pac,
                }
            }
        }
    }

    /// hint 任务（不得发探测请求）的占位名：名字为空时按 URL（含预签名 URL 的
    /// `response-content-disposition`）推断并以推断名落库，供排队期间 UI 显示；HTTP
    /// 完成期按实际 GET 响应精修。无网络 I/O。
    async fn fill_provisional_name(&self, task_id: &str, url: &str, file_name: &str) {
        if !file_name.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        let Some(resolved) = naming::name_from_url(url) else {
            return;
        };
        let inferred = resolved.source != naming::NameSource::QueryDisposition;
        if let Err(error) = self
            .db
            .update_task_file_name(task_id, &resolved.name, inferred)
            .await
        {
            crate::logger::report_error(
                "download-manager",
                "persist provisional task name",
                &error,
            );
            return;
        }
        self.sink.emit(EngineEvent::TaskMetaProbed {
            task_id: task_id.to_owned(),
            file_name: resolved.name,
            total_bytes: 0,
        });
    }

    /// 元数据探测的拦截输入：拦截器、任务信息、只读网络信息。
    #[allow(clippy::too_many_arguments)]
    async fn meta_probe_extras(
        &self,
        task_id: &str,
        url: &str,
        file_name: &str,
        save_dir: &str,
        proxy_url: &str,
        ignore_tls_errors: bool,
        user_agent: &str,
        queue_id: &str,
    ) -> (
        Arc<dyn crate::intercept::TaskInterceptor>,
        crate::intercept::TaskCtx,
        crate::intercept::RunMeta,
    ) {
        let interceptor = self.task_interceptor(task_id, url, url).await;
        let task_ctx = crate::intercept::TaskCtx {
            id: task_id.to_string(),
            url: url.to_string(),
            source_url: url.to_string(),
            file_name: file_name.to_string(),
            save_dir: save_dir.to_string(),
            protocol: crate::intercept::protocol_kind(url, false).as_str(),
            track_pair: false,
            is_resume: false,
        };
        let run_meta = crate::intercept::RunMeta {
            proxy_url: proxy_url.to_string(),
            ignore_tls_errors,
            user_agent: self.resolved_task_ua(user_agent, queue_id).to_string(),
        };
        (interceptor, task_ctx, run_meta)
    }

    /// 构造任务的拦截器：插件启用且有匹配项时为插件实现，否则为空实现。
    async fn task_interceptor(
        &self,
        task_id: &str,
        source_url: &str,
        current_url: &str,
    ) -> Arc<dyn crate::intercept::TaskInterceptor> {
        #[cfg(feature = "plugins")]
        if let Some(pm) = &self.plugin_manager {
            return pm.task_interceptor(task_id, source_url, current_url).await;
        }
        #[cfg(not(feature = "plugins"))]
        let _ = (task_id, source_url, current_url);
        crate::intercept::noop()
    }

    /// 后台元数据探测（HEAD → GET Range:0-0，非阻塞）：探得文件名/大小后
    /// 更新 DB 并广播 [`EngineEvent::TaskMetaProbed`]；失败静默。
    ///
    /// F020：用任务的鉴权上下文（cookies/referrer/extra_headers）构造 probe
    /// 的 `RequestSpec`，使背景 HEAD probe 与真正下载请求一致，避免鉴权站点
    /// 把缺鉴权的裸 HEAD 重定向到登录页污染 DB 文件名。带 resolver 的任务
    /// 不应调用（探测原始页面 URL 无意义）。
    #[allow(clippy::too_many_arguments)]
    fn spawn_meta_probe(
        &self,
        task_id: String,
        probe_url: String,
        current_name: String,
        probe_spec: downloader::RequestSpec,
        probe_client: Client,
        probe_proxy: ProxyConfig,
        interceptor: Arc<dyn crate::intercept::TaskInterceptor>,
        task_ctx: crate::intercept::TaskCtx,
        run_meta: crate::intercept::RunMeta,
    ) {
        let probe_db = self.db.clone();
        let probe_sink = self.sink.clone();
        #[cfg(feature = "plugins")]
        let probe_pm = self.plugin_manager.clone();
        let probe_gate = Arc::clone(&self.probe_gate);
        let slot = ProbeSlot::acquire(&self.probes_outstanding);
        tokio::spawn(async move {
            // 窗口名额随探测结束（含提前返回）释放。
            let _slot = slot;
            // 并发闸门：许可在整个探测期间持有，批量导入的探测按 8 路排队。
            let Ok(_permit) = probe_gate.acquire_owned().await else {
                tracing::debug!("meta probe gate closed before probe started");
                return;
            };
            let mut probe_url = probe_url;
            let mut probe_spec = probe_spec;
            if crate::intercept::wants(
                interceptor.as_ref(),
                crate::intercept::InterceptStage::Prepare,
            ) {
                let notes = crate::intercept::ActivityNotes::new(
                    probe_db.clone(),
                    probe_sink.clone(),
                    task_id.clone(),
                );
                let never = CancellationToken::new();
                let kind = crate::intercept::protocol_kind(&probe_url, false);
                let mut target = crate::intercept::RequestTarget {
                    url: &mut probe_url,
                    spec: &mut probe_spec,
                    file_name: None,
                    segment_count: None,
                    connection_cap: None,
                    hint_file_size: None,
                    range_verified: None,
                    is_resume: false,
                    kind,
                    has_audio_track: false,
                };
                match crate::intercept::drive::run_prepare(
                    interceptor.as_ref(),
                    &notes,
                    &never,
                    &task_ctx,
                    &run_meta,
                    0,
                    &mut target,
                )
                .await
                {
                    Err(abort) => {
                        log_warn!("[intercept] task {task_id} meta probe aborted: {abort}");
                        return;
                    }
                    // 一次性凭据：探测会作废它，元数据留待真正下载时获得。
                    Ok(outcome) if outcome.ephemeral => {
                        log_info!(
                            "[intercept] task {task_id} meta probe skipped: plugin declared ephemeral"
                        );
                        return;
                    }
                    Ok(_) => {}
                }
            }
            let probed = crate::meta_prober::probe_task_meta(
                &probe_url,
                &current_name,
                &probe_client,
                &probe_proxy,
                &probe_spec,
            )
            .await;
            let (name, size) = (probed.file_name, probed.total_bytes);
            if !name.is_empty() || size > 0 {
                if !name.is_empty()
                    && let Err(error) = probe_db
                        .update_task_file_name(&task_id, &name, probed.name_inferred)
                        .await
                {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                    return;
                }
                probe_sink.emit(EngineEvent::TaskMetaProbed {
                    task_id: task_id.clone(),
                    file_name: name.clone(),
                    total_bytes: size,
                });
                #[cfg(feature = "plugins")]
                if let Some(pm) = &probe_pm {
                    pm.notify(crate::plugin::PluginEvent::MetaProbed {
                        task_id,
                        url: probe_url,
                        file_name: name,
                        total_bytes: size,
                    })
                    .await;
                }
            }
        });
    }

    /// Internal: actually spawn the download task (no concurrency check).
    async fn do_start_task(&mut self, queued: QueuedTask) {
        // 插件惰性解析守卫（体首）：命中 resolver 且未解析 → off-actor resolve 后再入。
        #[cfg(feature = "plugins")]
        let mut queued = queued;
        // 解析交给起跑的出口只属于紧随其后的这次再入（已解析且仍绑定插件）；其余起跑一律作废，
        // 防止插件被卸载 / 忽略插件重试后的直连起跑沿用旧钉定。
        #[cfg(feature = "plugins")]
        if !queued.resolved || queued.resolver_plugin_id.is_empty() {
            self.resolved_egress.remove(&queued.task_id);
        }
        #[cfg(feature = "plugins")]
        if !queued.resolved
            && (!queued.resolver_plugin_id.is_empty() || !queued.resolver_item.is_empty())
        {
            // 绑定以 DB 为准（与 do_resume_task 对称）：排队期间插件被卸载会清掉 DB 绑定，
            // 内存里的 QueuedTask 仍带着失效的 resolver。读库失败按「仍绑定」处理。
            let second_stage = !queued.resolver_item.is_empty();
            match self.db.get_task_resolver(&queued.task_id).await {
                Ok(bound) if bound.is_empty() => {
                    queued.resolver_plugin_id.clear();
                    if second_stage {
                        // 二段解析任务的链接是分享页，绝不按原始链接下载：重新绑定，
                        // 仍无可用插件则 fail-closed。
                        match self.rebind_second_stage(&queued.task_id).await {
                            Some(identity) => {
                                queued.resolver_plugin_id = identity;
                                self.begin_resolve_start(queued).await;
                            }
                            None => self.fail_second_stage_unavailable(&queued.task_id).await,
                        }
                        return;
                    }
                }
                Ok(bound) => {
                    if queued.resolver_plugin_id.is_empty() {
                        queued.resolver_plugin_id = bound;
                    }
                    self.begin_resolve_start(queued).await;
                    return;
                }
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "load task resolver for start guard",
                        &error,
                    );
                    if queued.resolver_plugin_id.is_empty() {
                        self.fail_second_stage_unavailable(&queued.task_id).await;
                    } else {
                        self.begin_resolve_start(queued).await;
                    }
                    return;
                }
            }
        }
        let QueuedTask {
            task_id,
            url,
            save_dir,
            file_name,
            segments,
            is_resume: _,
            cookies,
            referrer,
            hint_file_size,
            torrent_file_bytes,
            proxy_url,
            user_agent,
            queue_id,
            checksum,
            ignore_tls_errors,
            extra_headers,
            #[cfg(feature = "bt")]
            selected_file_indices,
            #[cfg(not(feature = "bt"))]
                selected_file_indices: _,
            method,
            body,
            audio_url,
            resolver_plugin_id: _,
            resolved: _,
            range_supported,
            resolver_item: _,
            source_url,
            resolver_patch,
        } = queued;

        // Four-tier segment count priority:
        //   1. Task-level explicit choice (segments > 0) — highest priority
        //   2. Queue default_segments (> 0) — inherits from queue when task is auto
        //   3. Global default_segments (> 0) — global setting from config
        //   4. Segment advisor (segments == 0) — dynamic calculation at runtime
        let queue_default = self
            .queues
            .get(&queue_id)
            .map(|q| q.default_segments)
            .filter(|&s| s > 0)
            .unwrap_or(0);
        let segments = if segments > 0 {
            segments
        } else if queue_default > 0 {
            queue_default
        } else if self.global_default_segments > 0 {
            self.global_default_segments
        } else {
            0 // 0 → segment_advisor will calculate
        };

        // 第 5 层：域名单连接策略缓存覆盖。
        // 如果此域名曾因多连接被服务器拒绝（403/429），自动降级为单线程，
        // 避免重蹈覆辙。缓存带 24h TTL，过期后重新尝试多线程。
        let segments = if segments != 1 && is_single_conn_domain(&url) {
            log_info!(
                "[manager] task {} 域名命中单连接缓存，强制 segments=1",
                task_id
            );
            1
        } else {
            segments
        };

        // 通知平面：onStart（fire-and-forget，用解析后的实际 url）。
        #[cfg(feature = "plugins")]
        if let Some(pm) = &self.plugin_manager {
            pm.notify(crate::plugin::PluginEvent::Start {
                task_id: task_id.clone(),
                url: url.clone(),
            })
            .await;
        }

        // 队列 `queue.started` 必须先于该队列的首个 `task.started`：这里先登记占用
        // （空闲 → 占用的边沿在此触发），随后 `sync_queue_occupancy` 看到的就是
        // 已登记状态，不会重复通知。
        self.mark_queue_occupied(&queue_id);

        // Webhook：task.started（与 onStart 同点位，`do_start_task` 只走首次
        // 启动，resume 由 `do_resume_task` 承接，因此语义就是「首次进入
        // downloading」）。
        let started_event = self.webhook_task_event(
            crate::webhook::WebhookEventKind::TaskStarted,
            crate::webhook::WebhookTask {
                id: task_id.clone(),
                file_name: file_name.clone(),
                url: url.clone(),
                save_dir: save_dir.clone(),
                total_bytes: hint_file_size,
                status: 1,
                error_message: String::new(),
            },
            &queue_id,
        );
        self.emit_task_event(started_event);

        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();

        let use_ftp = is_ftp_url(&url);
        let use_hls = hls_downloader::is_hls_url(&url);
        // 轨对任务（audio_url 非空）复用 DASH 下载器的下载+mux 能力，与 .mpd 后缀正交。
        let use_dash = dash_downloader::is_dash_url(&url) || audio_url.is_some();
        let use_bt = is_magnet(&url) || !torrent_file_bytes.is_empty() || is_torrent_file_url(&url);
        let use_ed2k = is_ed2k_url(&url);

        // Insert a placeholder entry now so capacity/queue checks are correct
        // for any reentrant calls that may occur during BT session init below.
        // The `handle` field is filled in after tokio::spawn.
        self.active_tasks.insert(
            task_id.clone(),
            ActiveTaskEntry {
                token: cancel_token.clone(),
                generation: spawn_gen,
                handle: None,
                is_bt: use_bt,
                queue_id: queue_id.clone(),
            },
        );
        // Select speed limiter: queue-specific if the queue has a limit, global otherwise.
        let speed_limiter = self.queue_limiter_for(&queue_id);

        let done_tx = self.done_tx.clone();
        #[cfg(feature = "bt")]
        let panic_progress_tx = self.progress_tx.clone();
        #[cfg(feature = "bt")]
        let panic_task_id = task_id.clone();
        #[cfg(feature = "bt")]
        let panic_db = self.db.clone();
        let task_span = tracing::error_span!("download_task", task_id = %task_id);

        let handle = if use_bt {
            // BT 不走 HTTP 出口决策：解析留下的出口一并作废。
            #[cfg(feature = "plugins")]
            self.resolved_egress.remove(&task_id);
            // Lazily initialise the shared BT session.
            if let Err(e) = self.ensure_bt_session().await {
                crate::log_error!("[manager] failed to init BT session: {}", e);
                if let Err(db_error) = self
                    .db
                    .update_task_status(&task_id, 4, &e.to_string())
                    .await
                {
                    crate::logger::report_error(
                        "download-manager",
                        "persist BT session initialization error",
                        &db_error,
                    );
                }
                if self
                    .progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.clone(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: 4,
                        error_message: e.to_string(),
                        file_name: String::new(),
                        segment_details: None,
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                    tracing::debug!("download-manager notification receiver closed");
                }
                self.active_tasks.remove(&task_id);
                return;
            }
            #[cfg(feature = "bt")]
            {
                // bt_session is guaranteed to be Some after ensure_bt_session().
                let Some(bt_ref) = self.bt_session.as_ref() else {
                    crate::log_error!(
                        "[manager] BUG: bt_session is None after ensure_bt_session succeeded"
                    );
                    self.active_tasks.remove(&task_id);
                    return;
                };

                // Build the torrent source: prefer torrent file bytes if available,
                // otherwise use the URL as a magnet link.
                // Capture whether this is a .torrent-file task BEFORE the bytes
                // are moved into TorrentSource below.
                let is_torrent_file_task = !torrent_file_bytes.is_empty();
                let torrent_source = if is_torrent_file_task {
                    TorrentSource::TorrentFileBytes(torrent_file_bytes)
                } else {
                    TorrentSource::Magnet(url)
                };

                // Validate and persist user-specified custom name for BT rename.
                //
                // Only treat file_name as a custom rename target when the task
                // comes from a magnet URL and the user explicitly typed a name.
                // For .torrent-file tasks the file_name is auto-derived from the
                // .torrent filename (without the ".torrent" extension) by the client
                // layer — it has no extension and does not represent the user's
                // intent to rename the download.  Using it as custom_name would
                // cause the completed file to be saved without its real extension
                // (e.g. "cachyos-desktop-linux-260308" instead of
                // "cachyos-desktop-linux-260308.iso").
                //
                // The same trap exists for magnet tasks: the magnet `dn` display
                // name (usually extension-less, e.g. "Sintel") flows back as
                // file_name from every entry point that pre-fills it (browser
                // extension NMH `filename=`, the new-task dialog prefill), and it
                // is NOT a rename request — qBittorrent treats `dn` purely as a
                // cosmetic placeholder until metadata arrives, never as the disk
                // name.  So a file_name that merely echoes `dn` is ignored; only
                // a name that differs from `dn` proves user intent.
                //
                // Rule: custom_name is only honoured for magnet-URL tasks where
                // file_name is non-empty, safe, and differs from the magnet `dn`.
                // Torrent-file tasks always discover their real name from
                // metadata and never rename.
                let dn_echo = torrent_source.display_name().unwrap_or_default();
                let custom_name = if is_torrent_file_task {
                    // Task created from a .torrent file — ignore file_name.
                    String::new()
                } else if is_safe_file_name(&file_name) && file_name != dn_echo {
                    // Magnet task with a user-supplied name.
                    file_name.clone()
                } else {
                    String::new()
                };
                if !custom_name.is_empty()
                    && let Err(error) = self.db.save_bt_custom_name(&task_id, &custom_name).await
                {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                    persist_task_failure(
                        &task_id,
                        format!("persist BT custom name: {error}"),
                        &self.db,
                        &self.progress_tx,
                    )
                    .await;
                    self.active_tasks.remove(&task_id);
                    return;
                }

                // 首启也要尊重**已持久化**的文件选择，而不是无脑弹框：无人值守
                // 入口（RSS）在 `create_task` 里已经把「全部文件」写进 DB，读到
                // `Some([])` 就该直接跳过对话框。此前这里硬编码 `false`，与
                // `do_resume_task` 的三态读取行为分叉，导致 RSS 每建一个 BT 任务
                // 都弹一次选择框（且用户点「取消」后条目仍被标记「已下载」）。
                let (bt_pre_selected, bt_skip_selection) = match self
                    .db
                    .load_bt_selected_files(&task_id)
                    .await
                    .unwrap_or(None)
                {
                    None => (Vec::new(), false),
                    Some(indices) if indices.is_empty() => (Vec::new(), true),
                    Some(indices) => (indices, false),
                };

                let bt_params = BtDownloadParams {
                    task_id: task_id.clone(),
                    torrent_source,
                    save_dir,
                    db: self.db.clone(),
                    progress_tx: self.progress_tx.clone(),
                    cancel_token,
                    session: bt_ref.session(),
                    bt_runtime: bt_ref.runtime_handle(),
                    shared_bt: bt_ref.clone(),
                    existing_handle: None,
                    pre_selected_indices: if bt_pre_selected.is_empty() {
                        selected_file_indices
                    } else {
                        bt_pre_selected
                    },
                    skip_file_selection: bt_skip_selection,
                    custom_name,
                    selector: self.selector.clone(),
                    sink: self.sink.clone(),
                    // 任务级/队列级上传限速只能在 add 时烘焙（librqbit 无
                    // per-torrent 热更 API）；行刚插入时任务级通常为 0，恢复/
                    // 续种路径会带上用户后来设置的值。
                    upload_limit_bps: match self.db.load_task_by_id(&task_id).await {
                        Ok(Some(t)) => self.effective_task_upload_bps(&t),
                        _ => 0,
                    },
                };

                tokio::spawn(
                    async move {
                        let result =
                            std::panic::AssertUnwindSafe(bt_downloader::run_bt_download(bt_params))
                                .catch_unwind()
                                .await;

                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(downloader::DownloadError::Cancelled)) => {
                                tracing::debug!("BT download worker cancelled");
                            }
                            Ok(Err(error)) => {
                                handle_bt_worker_error(
                                    &panic_task_id,
                                    &error,
                                    &panic_db,
                                    &panic_progress_tx,
                                )
                                .await;
                            }
                            Err(panic_info) => {
                                let msg = panic_message(&panic_info);
                                handle_task_panic(
                                    &panic_task_id,
                                    msg,
                                    &panic_db,
                                    &panic_progress_tx,
                                )
                                .await;
                            }
                        }

                        if done_tx
                            .send(TaskDone {
                                task_id: panic_task_id,
                                generation: spawn_gen,
                                reserved_temp_path: None, // BT 任务不使用文件名预订机制
                                phase: TaskDonePhase::Finished,
                            })
                            .await
                            .is_err()
                        {
                            // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                            tracing::debug!("download-manager notification receiver closed");
                        }
                    }
                    .instrument(task_span),
                )
            }
            // 无 BT 后端：`ensure_bt_session` 恒返回错误并已在上面处理完毕。
            #[cfg(not(feature = "bt"))]
            {
                self.active_tasks.remove(&task_id);
                return;
            }
        } else {
            let (route_subject, forced_route) = self.take_start_route(&task_id);
            let TaskEgress {
                client: task_client,
                proxy: task_proxy,
                route: auto_route,
                auto_ctx,
                router: egress_router,
                pac: pac_routing,
            } = self.task_http_context(
                route_subject.as_deref().unwrap_or(&url),
                &proxy_url,
                &user_agent,
                &queue_id,
                ignore_tls_errors,
                forced_route,
            );
            // ProxyMode::Auto / 站点规则 可追溯性：启动基线路由落库（非 Auto 写空串清除
            // 旧标签），非空时广播给客户端原位刷新详情面板「链路」行。
            if let Err(e) = self.db.set_task_auto_route(&task_id, auto_route).await {
                log_info!("[manager] task {} auto_route 落库失败: {}", task_id, e);
            }
            if !auto_route.is_empty() {
                self.sink.emit(EngineEvent::TaskRouteChanged {
                    task_id: task_id.clone(),
                    route: auto_route.to_string(),
                });
            }
            // ---------------------------------------------------------------
            // 文件名最终决策在 spawned task 序幕执行（finalize_start_file_name）：
            // probe（网络 IO）→ HLS/DASH 归一 → dedup → 预订 → 落库全程
            // off-actor，本函数 spawn 后立即返回——创建任务后的 AllTasks
            // 快照不再被 probe 的网络往返压后，probe 期间其余信号照常处理。
            // dedup+预订经共享互斥锁串行化，原子性等价于旧 actor 同步段；
            // 预订仍经 TaskDone.reserved_temp_path 在 on_task_done 释放。
            // ---------------------------------------------------------------

            // 构造完整 HTTP 请求事务规格——method/body 来自浏览器扩展，
            // 用于在 form-POST 等非 GET 触发的下载场景中一比一重建原始请求。
            // 参见 downloader.rs 中 RequestSpec / build_request 的设计动机。
            let mut spec = downloader::RequestSpec::from_captured(
                method.as_deref(),
                cookies.clone(),
                referrer.clone(),
                extra_headers.clone(),
                body.clone(),
            );
            spec.egress = egress_router;

            // 多 CDN 聚合输入：manager 侧条件（开关/TLS/代理）在此折算；
            // pinned client 的 UA 与任务 client 同源（resolved_task_ua）。
            let cdn = self.cdn_task_input(
                ignore_tls_errors,
                &task_proxy,
                self.resolved_task_ua(&user_agent, &queue_id),
            );
            // 插件解析出的直链常绑定解析时的出口 IP：多网卡聚合会换公网 IP，不启用。
            let multi_nic = if route_subject.is_some() {
                None
            } else {
                self.multi_nic_input(
                    ignore_tls_errors,
                    &task_proxy,
                    auto_ctx
                        .as_ref()
                        .is_some_and(|ctx| !ctx.alternates.is_empty()),
                    self.resolved_task_ua(&user_agent, &queue_id),
                )
            };
            let task_unattended = self.db.is_task_unattended(&task_id).await.unwrap_or(false);
            let support = ask_support(
                &url,
                audio_url.is_some(),
                spec.method != reqwest::Method::GET || spec.body.is_some(),
            );
            let interceptor = self.task_interceptor(&task_id, &source_url, &url).await;
            let run_meta = crate::intercept::RunMeta {
                proxy_url: proxy_url.clone(),
                ignore_tls_errors,
                user_agent: self.resolved_task_ua(&user_agent, &queue_id).to_string(),
            };
            let params = DownloadParams {
                task_id: task_id.clone(),
                url,
                save_dir,
                file_name,
                segment_count: segments,
                is_resume: false,
                // hint 任务（插件 ephemeral / 浏览器扩展 fileSize）默认 Range 未
                // 验证 → 保守单流启动；resolver 插件显式担保（rangeSupported）
                // 时按已验证多段起飞。非 hint 任务照常 probe，此值不参与判定。
                range_verified: hint_file_size == 0 || range_supported,
                db: self.db.clone(),
                client: task_client,
                progress_tx: self.progress_tx.clone(),
                cancel_token,
                speed_limiter,
                cookies,
                referrer,
                hint_file_size,
                proxy_config: task_proxy,
                sink: self.sink.clone(),
                selector: self.selector.clone(),
                checksum,
                extra_headers,
                spec,
                audio_url,
                auto_max_connections: self.auto_max_connections,
                use_server_time: self.use_server_time,
                overwrite: self.global_overwrite_policy(),
                // 段行布局属主令牌：本次 spawn 的 generation。多段路径起飞时
                // 落 tasks.segments_epoch，旧 spawn 迟到的段进度写全类失效。
                spawn_gen: spawn_gen as i64,
                // 只有 HLS / DASH(含轨对)的 mux 用得到 ffmpeg，其余协议不必在 actor 上
                // 为每个任务读一次配置、stat 并扫描 PATH。
                ffmpeg_path: if use_hls || use_dash {
                    crate::components::resolve_ffmpeg(&self.db, &self.data_dir).await
                } else {
                    None
                },
                cdn,
                auto_proxy: auto_ctx,
                multi_nic,
                unattended: task_unattended,
                preprobed: None,
                interceptor,
                connection_cap: 0,
                run_meta,
            };

            let reserved_set = Arc::clone(&self.reserved_temp_paths);
            tokio::spawn(
                run_http_family_task(HttpFamilyRun {
                    params,
                    use_ftp,
                    use_hls,
                    use_dash,
                    use_ed2k,
                    prelude: Some(PreludeCtx {
                        global: self.file_exists_behavior,
                        support,
                    }),
                    reserved_set,
                    done_tx,
                    spawn_gen,
                    resolver_patch,
                    pac_routing,
                })
                .instrument(task_span),
            )
        };
        if let Some(entry) = self.active_tasks.get_mut(&task_id) {
            entry.handle = Some(handle);
        }
    }

    /// 清理某任务的 resolve 等待态（pause/cancel/delete 感知，即时生效 + 与
    /// on_resolve_ready 的 DB 复查形成双保险）。feature 关时为空操作。
    #[cfg(feature = "plugins")]
    fn clear_pending_resolve(&mut self, task_id: &str) {
        self.pending_resolve.remove(task_id);
        self.resume_applied.remove(task_id);
        self.resolved_egress.remove(task_id);
    }
    #[cfg(not(feature = "plugins"))]
    fn clear_pending_resolve(&mut self, _task_id: &str) {}
    /// 任务是否有进行中的 off-actor resolve（占位在 active_tasks、DB 仍是 paused/error）。
    #[cfg(feature = "plugins")]
    fn has_pending_resolve(&self, task_id: &str) -> bool {
        self.pending_resolve.contains_key(task_id)
    }
    #[cfg(not(feature = "plugins"))]
    fn has_pending_resolve(&self, _task_id: &str) -> bool {
        false
    }
    /// Emit a `TaskProgress` event for `task_id` using the latest DB row.
    /// `speed` is always reported as 0 because this helper is used for
    /// paused / completed / seeding transitions where no download speed exists.
    async fn emit_progress_from_db(
        &self,
        task_id: &str,
        status: i32,
        seeding_status: i32,
        seeding_message: &str,
        upload_speed_bps: i64,
    ) {
        if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
            self.sink.emit(EngineEvent::TaskProgress {
                task_id: task_id.to_string(),
                status,
                downloaded_bytes: t.downloaded_bytes,
                total_bytes: t.total_bytes,
                speed: 0,
                file_name: t.file_name.clone(),
                save_dir: t.save_dir.clone(),
                url: t.url.clone(),
                error_message: String::new(),
                upload_speed_bps,
                uploaded_bytes: t.uploaded_bytes,
                seeding_status,
                seeding_message: seeding_message.to_string(),
                seeding_time_secs: t.seeding_time_secs,
            });
        }
    }
    /// Publish the final paused frame after the matching downloader generation
    /// has stopped and persisted its flushed byte count.
    ///
    /// Returns `true` when a resume request arrived during cancellation and may
    /// now safely start a new generation. DB status and the active generation
    /// are checked again so stale TaskDone messages cannot overwrite a newer
    /// running task with `paused`.
    async fn finish_pending_pause(&mut self, task_id: &str, generation: u64) -> bool {
        let generation_matches = self
            .pending_pauses
            .get(task_id)
            .is_some_and(|pending| pending.generation == generation);
        if !generation_matches {
            return false;
        }
        let Some(pending) = self.pending_pauses.remove(task_id) else {
            return false;
        };

        if self
            .active_tasks
            .get(task_id)
            .is_some_and(|entry| entry.generation != generation)
        {
            return false;
        }

        let task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(task)) => task,
            Ok(None) => return false,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load task after pause flush",
                    &error,
                );
                return false;
            }
        };
        if task.status == 2 {
            if self
                .progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.to_string(),
                    downloaded_bytes: task.downloaded_bytes,
                    total_bytes: task.total_bytes,
                    status: 2,
                    error_message: String::new(),
                    file_name: task.file_name.clone(),
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                tracing::debug!("download-manager notification receiver closed");
            }
            self.send_segments_from_db(task_id, task.total_bytes).await;
            if pending.notify {
                let event = self
                    .webhook_event_from_task(crate::webhook::WebhookEventKind::TaskPaused, &task);
                self.emit_task_event(event);
            }
        }

        // 暂停写库失败可能仍显示 downloading；writer 已由匹配 TaskDone 确认退出，
        // 此时才允许此前挂起的显式恢复。完成/错误态仍不自动重启。
        pending.resume_requested && matches!(task.status, 0..=2)
    }

    /// 用户显式暂停**单个**任务。会发 `task.paused` webhook。
    ///
    /// 批量路径（`batch_pause` / 队列停用 / Boost 让位 / 改线程数的暂停-恢复）
    /// 必须走 [`Self::pause_task_silent`]——设计明确要求全局暂停不触发通知，
    /// 否则千级批量任务会给用户连发一屏推送。
    pub async fn pause_task(&mut self, task_id: &str) {
        self.pause_task_inner(task_id, true, true).await;
    }

    /// 内部/批量暂停：行为与 [`Self::pause_task`] 完全一致，只是不发 webhook。
    async fn pause_task_silent(&mut self, task_id: &str) {
        self.pause_task_inner(task_id, false, true).await;
    }

    /// 「暂停后立刻恢复」的内部流程（改分段数 / 重新下载）专用：静默暂停，
    /// 但**不**把让出的槽位 drain 给排队头部，否则随后的 resume 只能挂起
    /// `resume_requested`，待旧 spawn 收尾时已无容量而被挂到队尾。
    /// 调用方必须随后调用 resume（其自身会 drain），或在放弃恢复时自行 drain。
    async fn pause_task_keep_slot(&mut self, task_id: &str) {
        self.pause_task_inner(task_id, false, false).await;
    }

    async fn pause_task_inner(&mut self, task_id: &str, notify: bool, drain_freed_slot: bool) {
        self.clear_pending_resolve(task_id);
        self.retry_unschedule(task_id);
        // A repeated pause while the previous generation is still flushing is
        // idempotent. Preserve an explicit notification request, but do not
        // publish the stale DB snapshot as a terminal paused frame.
        if let Some(pending) = self.pending_pauses.get_mut(task_id) {
            // 最后一次用户意图是暂停：撤销 flush 窗口内挂起的恢复请求。
            pending.resume_requested = false;
            pending.notify |= notify;
            self.sync_queue_occupancy();
            return;
        }

        // Remove from pending queue if queued (not yet started).
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            if let Err(error) = self.db.update_task_status(task_id, 2, "").await {
                crate::logger::report_error("download-manager", "persist task state", &error);
                self.sync_queue_occupancy();
                return;
            }
            self.pending_queue.remove(pos);
            // 广播更新后的队列位置
            self.broadcast_queue_positions();
            self.emit_progress_from_db(task_id, 2, 0, "", 0).await;
            if notify {
                self.emit_paused_webhook(task_id).await;
            }
            self.sync_queue_occupancy();
            return;
        }

        // 下载器已把库写成完成态、TaskDone 尚在队列中：保留 active 条目，
        // 让 TaskDone 以匹配世代走完成收尾，不得把已完成任务改写成暂停。
        if self
            .active_tasks
            .get(task_id)
            .is_some_and(|e| e.handle.is_some())
            && self
                .db
                .load_task_by_id(task_id)
                .await
                .is_ok_and(|t| t.is_some_and(|t| t.status == 3))
        {
            return;
        }

        if let Some(entry) = self.active_tasks.remove(task_id) {
            let generation = entry.generation;
            let has_spawned_downloader = entry.handle.is_some();
            entry.token.cancel();

            // A real downloader must finish flushing before its paused progress
            // becomes authoritative. Placeholder entries used by off-actor
            // resolve have no writer and can still report immediately.
            if has_spawned_downloader {
                self.pending_pauses.insert(
                    task_id.to_string(),
                    PendingPause {
                        generation,
                        notify,
                        resume_requested: false,
                    },
                );
            }

            // For BT tasks, explicitly pause the torrent in the session so
            // that the handle stays cached for fast resume.  This is a
            // no-op if the download loop already called session.pause on
            // cancellation detection, but covers edge cases (e.g. pause
            // during metadata resolution).
            #[cfg(feature = "bt")]
            if let Some(ref bt) = self.bt_session
                && let Err(error) = bt.pause_task(task_id).await
            {
                crate::logger::report_error("download-manager", "pause seeding task", &error);
                // 旧 writer 仍可能 flush；匹配 TaskDone 前必须保留世代屏障。
                self.drain_queue().await;
                return;
            }

            if let Err(error) = self.db.update_task_status(task_id, 2, "").await {
                crate::logger::report_error("download-manager", "persist task state", &error);
                // 落库失败不代表 writer 已退出，不能让立即恢复跨过屏障。
                self.drain_queue().await;
                self.sync_queue_occupancy();
                return;
            }
            if !has_spawned_downloader {
                self.emit_progress_from_db(task_id, 2, 0, "", 0).await;
                if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
                    self.send_segments_from_db(task_id, t.total_bytes).await;
                }
                if notify {
                    self.emit_paused_webhook(task_id).await;
                }
            }

            // A slot freed up — try to start queued tasks (skipped when the caller
            // is about to resume this very task and wants to keep the slot).
            if drain_freed_slot {
                self.drain_queue().await;
            }

            // Boost 守卫：若用户手动暂停了当前优先任务，取消 Boost 并恢复其他任务
            if self.priority_task_id.as_deref() == Some(task_id) {
                self.clear_priority().await;
            }
            // NOTE: do NOT call maybe_release_bt_session() here.
            //
            // The spawned task may still be running while it flushes progress.
            // Its TaskDone finalizes pending_pauses and only then allows a
            // requested resume or BT session release.
            self.sync_queue_occupancy();
            return;
        }

        // 失败任务（可能正等待自动重试）：落 paused 才能让已排程的重试在到点时
        // 被 `is_task_in_error` 拦下（定时器本身不可取消），并结束本轮自动恢复周期。
        if self.pause_failed_task(task_id).await {
            if notify {
                self.emit_paused_webhook(task_id).await;
            }
            self.sync_queue_occupancy();
            return;
        }

        // Third branch: the task is a completed BT torrent that is seeding or
        // queued for a seeding slot. Pausing it must stop/dequeue the seeder,
        // settle its cumulative seeding time and persist the user-stopped
        // state without changing the overall completed status.
        if let Ok(Some(task)) = self.db.load_task_by_id(task_id).await
            && task.status == 3
        {
            match task.seeding_status {
                s if s == SEEDING_STATUS_ACTIVE || s == SEEDING_STATUS_QUEUED => {
                    #[cfg(feature = "bt")]
                    if let Some(ref bt) = self.bt_session {
                        if let Err(error) = bt.pause_task(task_id).await {
                            crate::logger::report_error(
                                "download-manager",
                                "pause seeding task",
                                &error,
                            );
                            return;
                        }
                        if let Some(seed) = bt.unregister_seeder(task_id).await
                            && let Err(error) = self
                                .db
                                .set_task_seeding_time(task_id, seed.seed_time_secs)
                                .await
                        {
                            crate::logger::report_error(
                                "download-manager",
                                "persist task state",
                                &error,
                            );
                        }
                        // 让出的槽位立即给排队中的下一个做种者。
                        self.reconcile_seeding_slots().await;
                    }
                    if let Err(error) = self
                        .db
                        .update_task_seeding_status(
                            task_id,
                            SeedingStopReason::UserStopped.as_i32(),
                            SeedingStopReason::UserStopped.message(),
                        )
                        .await
                    {
                        crate::logger::report_error(
                            "download-manager",
                            "persist task state",
                            &error,
                        );
                        return;
                    }
                    // 用户再次停止做种：撤销手动恢复时的限制豁免，下次普通恢复重新受限制约束。
                    if let Err(error) = self.db.set_task_seed_limits_exempt(task_id, false).await {
                        crate::logger::report_error(
                            "download-manager",
                            "clear seed limits exemption",
                            &error,
                        );
                    }
                    self.emit_progress_from_db(
                        task_id,
                        3,
                        SeedingStopReason::UserStopped.as_i32(),
                        SeedingStopReason::UserStopped.message(),
                        0,
                    )
                    .await;
                }
                s if s == SeedingStopReason::UserStopped.as_i32() => {
                    // Already paused by the user — idempotent no-op.
                }
                _ => {}
            }
        }

        if notify {
            self.emit_paused_webhook(task_id).await;
        }
        self.sync_queue_occupancy();
    }

    /// 读回任务行并发一条 `task.paused`。
    async fn emit_paused_webhook(&self, task_id: &str) {
        if let Ok(Some(task)) = self.db.load_task_by_id(task_id).await {
            let event =
                self.webhook_event_from_task(crate::webhook::WebhookEventKind::TaskPaused, &task);
            self.emit_task_event(event);
        }
    }

    /// 把失败任务（status 4）落为暂停并发进度帧；非失败任务返回 `false` 不做任何事。
    ///
    /// 自动重试的 sleep 定时器不可取消，到点时由 `is_task_in_error` 判定是否仍需重启，
    /// 所以撤销重试 = 让任务离开失败态。同时结束本轮备用链路排程。
    async fn pause_failed_task(&mut self, task_id: &str) -> bool {
        match self.db.load_task_by_id(task_id).await {
            Ok(Some(task)) if task.status == 4 => {}
            Ok(_) => return false,
            Err(error) => {
                crate::logger::report_error("download-manager", "load task state", &error);
                return false;
            }
        }
        self.retry_unschedule(task_id);
        self.auto_failover_pending.remove(task_id);
        if let Err(error) = self.db.update_task_status(task_id, 2, "").await {
            crate::logger::report_error("download-manager", "persist task state", &error);
            return false;
        }
        self.emit_progress_from_db(task_id, 2, 0, "", 0).await;
        true
    }

    pub async fn resume_task(&mut self, task_id: &str) {
        // 用户手动恢复开启新一轮重试预算。若备用链路已排程但尚未回流，
        // 保留其目标与单次守卫；否则允许新的手动周期再换路一次。
        self.auto_retry_counts.remove(task_id);
        #[cfg(feature = "plugins")]
        self.intercept_retry_counts.remove(task_id);
        if !self.auto_failover_pending.contains_key(task_id) {
            self.auto_failover_attempts.remove(task_id);
        }
        self.resume_task_inner(task_id).await;
    }

    /// 自动重试路径专用：恢复任务但**不**重置自动重试计数。
    /// 与 resume_task 的区别仅在于跳过 auto_retry_counts.remove，
    /// 使累积计数得以持久到下次失败，从而正确触发重试上限与递增退避。
    pub async fn resume_task_auto(&mut self, task_id: &str) {
        if self.is_task_in_error(task_id).await {
            let attempt = self.auto_retry_counts.get(task_id).copied();
            let message = match attempt {
                Some(attempt) => format!("自动重试下载（第 {attempt} 次）"),
                None => "备用链路重试下载".to_owned(),
            };
            if let Err(e) = crate::task_activity::record(
                &self.db,
                self.sink.as_ref(),
                task_id,
                "retry",
                message,
                Some(4),
            )
            .await
            {
                tracing::error!(task_id, error = %e, "failed to persist task auto retry");
            }
        }
        self.resume_task_inner(task_id).await;
    }

    /// 「重新下载」：把任务彻底退回未开始状态后重新启动——不管磁盘上是否
    /// 已有产物，一律丢弃重下。
    ///
    /// 与 [`Self::resume_task`]（续传）的区别在于起飞前的复位：先取消在途
    /// spawn，再删掉最终文件与 `.fdownloading` 临时文件、清空段行与
    /// 进度/总大小/完成时间/错误信息，因此新一轮下载必定从 0 字节开始。
    /// 复位全部同步完成于 resume 之前，UI 收到的全量快照即复位后状态。
    ///
    /// BT 任务不走这条路（librqbit 有自己的重校验语义），直接返回；
    /// 任务不存在同样直接返回。
    pub async fn restart_task(&mut self, task_id: &str) {
        let Ok(Some(task)) = self.db.load_task_by_id(task_id).await else {
            log_info!("[manager] restart_task {}: task not found", task_id);
            return;
        };
        if is_bt_url(&task.url) {
            log_info!(
                "[manager] restart_task {}: BT task — restart not supported",
                task_id
            );
            return;
        }

        // 1. 先停掉在途 spawn（含尚未起飞的排队项）。静默暂停路径已处理
        //    token cancel / active_tasks 摘除 / pending_queue 摘除 / 落 paused。
        //    注意它**不**等待 spawned task 退出：下面的磁盘清理因此要为
        //    「写入方还没松手」留出重试窗口（Windows 上占用中的文件
        //    `remove_file` 直接失败）。
        let was_active =
            self.active_tasks.contains_key(task_id) || matches!(task.status, 0 | 1 | 5);
        if was_active {
            log_info!(
                "[manager] restart_task {}: cancelling in-flight spawn first",
                task_id
            );
            self.pause_task_keep_slot(task_id).await;
        }

        // 2. 磁盘清理（best-effort，NotFound 静默）。暂停之后重新读库，拿到
        //    spawned task 可能已 dedup 落库的最新 file_name。
        // 暂停前的原始状态：重载后的 t.status 会被 pause_task_silent 改写。
        let orig_status = task.status;
        // 「跳过」采纳的已有文件不归本任务所有：重新下载不删它，并清除该决定使
        // 新一轮重新走命名/询问流程。
        let adopted_existing = self.skip_adopted(task_id).await;
        if let Err(error) = self.db.set_exists_decision(task_id, "").await {
            crate::logger::report_error("download-manager", "clear file-exists decision", &error);
        }
        let t = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            _ => task,
        };
        if is_safe_file_name(&t.file_name) {
            let path = PathBuf::from(&t.save_dir).join(&t.file_name);
            let temp_path = PathBuf::from(format!("{}{}", path.display(), downloader::TEMP_EXT));
            // 最终产物认领判定（详见 task_owns_final_file）：从未启动的任务
            // `file_name` 未经启动期 dedup，可能与早前同名任务留下的成品相
            // 撞——重下靠启动期 dedup 另起新名即可，绝不能删别人的文件。
            // 用暂停前的原始 status 判定（pause_task_silent 会把活跃任务改
            // 成 2，而活跃任务不可能是 3，语义一致）。
            if task_owns_final_file(orig_status) && !adopted_existing {
                remove_file_retrying(task_id, &path, was_active).await;
            }
            remove_file_retrying(task_id, &temp_path, was_active).await;
            // DASH 音轨 sidecar（轨对任务的视频轨 URL 非 .mpd，需查库确认）
            // 与其临时文件一并清理，否则重下会复用上一轮的旧音轨。
            let has_audio_sidecar = dash_downloader::is_dash_url(&t.url)
                || hls_downloader::is_hls_url(&t.url)
                || self
                    .db
                    .load_audio_url(task_id)
                    .await
                    .unwrap_or_default()
                    .is_some();
            if has_audio_sidecar {
                let audio_path = dash_downloader::build_audio_path(&path);
                let audio_temp =
                    PathBuf::from(format!("{}{}", audio_path.display(), downloader::TEMP_EXT));
                remove_file_retrying(task_id, &audio_temp, was_active).await;
                if task_has_started(orig_status, t.downloaded_bytes) {
                    remove_file_retrying(task_id, &audio_path, was_active).await;
                }
            }
        } else {
            log_info!(
                "[manager] restart_task {}: unsafe file name, skipping disk cleanup",
                task_id
            );
        }

        // 3. DB 复位。`update_task_file_missing` 只作用于 status=3 的行，
        //    因此必须排在把状态改回 0 之前。`update_task_status(_, 0, "")`
        //    同时清空 error_message 与 completed_at（见 Db::update_task_status）。
        if let Err(e) = self.db.delete_segments(task_id).await {
            log_info!(
                "[manager] restart_task {}: delete_segments error: {}",
                task_id,
                e
            );
        }
        if let Err(error) = self.db.reset_task_speed_history(task_id).await {
            crate::logger::report_error("download-manager", "reset speed history", &error);
        }
        if let Err(e) = self.db.update_task_progress(task_id, 0).await {
            log_info!(
                "[manager] restart_task {}: reset progress error: {}",
                task_id,
                e
            );
            return;
        }
        if let Err(e) = self.db.update_task_total_bytes(task_id, 0).await {
            log_info!(
                "[manager] restart_task {}: reset total_bytes error: {}",
                task_id,
                e
            );
        }
        if let Err(e) = self.db.update_task_file_missing(task_id, false).await {
            log_info!(
                "[manager] restart_task {}: clear file_missing error: {}",
                task_id,
                e
            );
        }
        if let Err(e) = self.db.update_task_status(task_id, 0, "").await {
            log_info!(
                "[manager] restart_task {}: reset status error: {}",
                task_id,
                e
            );
        }

        // 4. 内存态复位：重试配额/failover 标记/退避占用全部作废。
        self.auto_retry_counts.remove(task_id);
        #[cfg(feature = "plugins")]
        self.intercept_retry_counts.remove(task_id);
        self.auto_failover_pending.remove(task_id);
        self.auto_failover_attempts.remove(task_id);
        self.retry_unschedule(task_id);

        // Route the explicit reset through the same FIFO as transfer progress.
        // A plain snapshot cannot distinguish this from an old DB checkpoint.
        if self
            .progress_tx
            .send(ProgressUpdate {
                task_id: task_id.to_owned(),
                downloaded_bytes: 0,
                download_baseline: Some(0),
                status: 0,
                ..Default::default()
            })
            .await
            .is_err()
        {
            tracing::debug!("restart progress receiver closed");
            return;
        }

        // 5. 重新起飞，并下发全量快照——TaskProgress 字段不全，进度归零必须
        //    靠全量快照才能让 UI 看到。
        log_info!("[manager] restart_task {}: reset done, resuming", task_id);
        self.resume_task(task_id).await;
        self.send_tasks_snapshot().await;
    }

    /// 仅删除已完成任务的磁盘产物，保留任务记录——与
    /// [`Self::delete_task`]`(delete_files=true)` 相反（那个连任务本身一起删）。
    /// 删除后把 `file_missing` 落库为 `true` 并广播
    /// [`crate::events::EngineEvent::FileMissingChanged`]，复用文件跟踪已有的
    /// 「文件已丢失」展示；用户之后可用 [`Self::restart_task`] 在同一条任务
    /// 记录上重新下载，无需重新粘贴 URL/重设保存目录等参数。
    ///
    /// 只对 `status == 3`（completed）生效：未完成任务没有已落地的最终产物，
    /// 「只删文件」在语义上就是 [`Self::restart_task`]。BT 任务的产物由
    /// librqbit session 管理且常是多文件目录，这条路径不支持。
    ///
    /// 返回 `true` 表示文件已被清除（或原本就不存在）且 DB 已同步；`false`
    /// 表示任务不存在、不是已完成的非 BT 任务、或文件名不安全无法定位磁盘
    /// 路径。
    pub async fn clear_task_file(&mut self, task_id: &str) -> bool {
        let Ok(Some(t)) = self.db.load_task_by_id(task_id).await else {
            log_info!("[manager] clear_task_file {}: task not found", task_id);
            return false;
        };
        if t.status != 3 {
            log_info!(
                "[manager] clear_task_file {}: not completed (status={})",
                task_id,
                t.status
            );
            return false;
        }
        if is_bt_url(&t.url) {
            log_info!(
                "[manager] clear_task_file {}: BT task — not supported",
                task_id
            );
            return false;
        }
        if !is_safe_file_name(&t.file_name) {
            log_info!(
                "[manager] clear_task_file {}: unsafe file name, refusing",
                task_id
            );
            return false;
        }
        let path = PathBuf::from(&t.save_dir).join(&t.file_name);
        if let Err(e) = tokio::fs::remove_file(&path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log_info!(
                "[manager] clear_task_file {}: remove {} failed: {}",
                task_id,
                path.display(),
                e
            );
            return false;
        }
        // DASH 音轨 sidecar（视频轨对任务 URL 非 .mpd，需查库确认）随主文件
        // 一并清理，避免残留孤儿音轨文件。
        let has_audio_sidecar = dash_downloader::is_dash_url(&t.url)
            || hls_downloader::is_hls_url(&t.url)
            || self
                .db
                .load_audio_url(task_id)
                .await
                .unwrap_or_default()
                .is_some();
        if has_audio_sidecar {
            let audio_path = dash_downloader::build_audio_path(&path);
            if let Err(error) = tokio::fs::remove_file(&audio_path).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logger::report_warning("download-manager", "clear audio file", &error);
                return false;
            }
        }
        match self.db.update_task_file_missing(task_id, true).await {
            Ok(true) => {
                self.sink.emit(EngineEvent::FileMissingChanged(vec![(
                    task_id.to_string(),
                    true,
                )]));
                true
            }
            Ok(false) => false,
            Err(e) => {
                log_info!(
                    "[manager] clear_task_file {}: DB update error: {}",
                    task_id,
                    e
                );
                false
            }
        }
    }

    async fn resume_task_inner(&mut self, task_id: &str) {
        // 排程中的自动重试已落地（或被手动恢复抢先），解除队列占用标记。
        self.retry_unschedule(task_id);
        if let Some(pending) = self.pending_pauses.get_mut(task_id) {
            pending.resume_requested = true;
            return;
        }

        if self.active_tasks.contains_key(task_id) {
            // A task can be in active_tokens but already failed in the DB:
            // this happens when the download task has finished (status=4
            // written to DB) but the done_tx hasn't been consumed by the
            // actor loop yet.  If we silently return here, the user's retry
            // request is dropped and the task stays stuck in error state.
            //
            // Detect this race: if DB status is error (4), force-remove the
            // stale entry so the resume proceeds. The stale done_tx will be
            // harmlessly ignored because the new spawn increments the
            // generation counter, making the old generation mismatch in
            // on_task_done.
            // 完成态（3）同样是终态残留，但完成收尾（插件后处理 / webhook）还在
            // done 通道里，且完成任务不可再恢复（见 `resume_completed`）：保留条目、忽略。
            // resolve 占位期间 DB 仍是 error(4)/paused：不是终态残留，重复恢复直接忽略，
            // 否则会移除占位并再起一个并发 resolve。
            if self.has_pending_resolve(task_id) {
                return;
            }
            let is_failed = match self.db.load_task_by_id(task_id).await {
                Ok(task) => task.is_some_and(|t| t.status == 4),
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "check active task before resume",
                        &error,
                    );
                    return;
                }
            };
            if !is_failed {
                return; // still active, or completion pending — do not interrupt
            }
            log_info!(
                "[manager] resume_task {}: stale active_tasks entry (failed in DB) — force-removing",
                task_id
            );
            self.active_tasks.remove(task_id);
            // Do NOT drain_queue here — we are about to occupy the freed slot.
        }

        // Also check if already in the pending queue.
        if self.pending_queue.iter().any(|q| q.task_id == task_id) {
            return;
        }

        // Load task once and reuse for both the is_bt check and the queue entry.
        let task_row = match self.db.load_task_by_id(task_id).await {
            Ok(task) => task,
            Err(error) => {
                crate::logger::report_error("download-manager", "load task before resume", &error);
                return;
            }
        };

        if let Some(task) = &task_row
            && self.resume_completed(task_id, task).await
        {
            return;
        }

        let is_bt = task_row
            .as_ref()
            .map(|t| is_bt_url(&t.url))
            .unwrap_or(false);
        let queue_id = task_row
            .as_ref()
            .map(|t| t.queue_id.clone())
            .unwrap_or_default();

        if is_bt || (self.has_capacity() && self.has_queue_capacity(&queue_id)) {
            self.do_resume_task(task_id).await;
            // If do_resume_task failed early (e.g. BT session init), drain
            // the queue so pending tasks can proceed.
            self.drain_queue().await;
        } else {
            log_info!(
                "[manager] queuing resume for task {} (active={}, max={}, queue={})",
                task_id,
                self.active_tasks.len(),
                self.max_concurrent,
                queue_id
            );
            if let Some(t) = task_row {
                // 排队即 pending：持久化 status=0，让批量操作尾部的
                // TasksSnapshot（按 DB 生成）不会把排队任务回显成 paused。
                if let Err(error) = self.db.update_task_status(task_id, 0, "").await {
                    crate::logger::report_error("download-manager", "persist task state", &error);
                    return;
                }
                // Notify the host: task is now queued (pending), not actively resuming.
                // Without this signal, the UI keeps all tasks stuck in "resuming" status
                // even though only max_concurrent are actually downloading.
                self.sink.emit(EngineEvent::TaskProgress {
                    task_id: task_id.to_string(),
                    status: 0, // pending/queued
                    downloaded_bytes: t.downloaded_bytes,
                    total_bytes: t.total_bytes,
                    speed: 0,
                    file_name: t.file_name.clone(),
                    save_dir: t.save_dir.clone(),
                    url: t.url.clone(),
                    error_message: String::new(),
                    upload_speed_bps: 0,
                    uploaded_bytes: t.uploaded_bytes,
                    seeding_status: t.seeding_status,
                    seeding_message: t.seeding_message.clone(),
                    seeding_time_secs: t.seeding_time_secs,
                });
                self.pending_queue.push_back(QueuedTask::for_resume(t));
                // 单任务恢复立即发布位置；连续创建走宿主节拍合并。
                self.broadcast_queue_positions();
            }
        }
        // 恢复即重新占用队列——不登记的话下次清空不会触发 `queue.drained`。
        self.sync_queue_occupancy();
    }

    /// 浏览器 POST 接管任务无法恢复：以 [`downloader::POST_ONLY_RESUME_MESSAGE`]
    /// 落为失败并通知宿主（进度不清，用户回浏览器重新发起即可）。
    async fn fail_post_only_resume(&mut self, task_id: &str, task: &TaskInfo) {
        let msg = downloader::POST_ONLY_RESUME_MESSAGE;
        if let Err(error) = self.db.update_task_status(task_id, 4, msg).await {
            crate::logger::report_error("download-manager", "persist task state", &error);
        }
        self.sink.emit(EngineEvent::TaskProgress {
            task_id: task_id.to_string(),
            status: 4,
            downloaded_bytes: task.downloaded_bytes,
            total_bytes: task.total_bytes,
            speed: 0,
            file_name: task.file_name.clone(),
            save_dir: task.save_dir.clone(),
            url: task.url.clone(),
            error_message: msg.to_string(),
            upload_speed_bps: 0,
            uploaded_bytes: task.uploaded_bytes,
            seeding_status: task.seeding_status,
            seeding_message: task.seeding_message.clone(),
            seeding_time_secs: task.seeding_time_secs,
        });
    }

    /// Internal: actually spawn the resume (no concurrency check).
    async fn do_resume_task(&mut self, task_id: &str) {
        // 插件惰性解析守卫（体首，协议判定前）：命中 resolver 且非再入 → off-actor
        // resolve 后经 resume_applied 再入；对称占位防 resumeAll 并发双 resolve。
        // `resume_applied` 条目无条件在此取走：resolve 窗口内插件被卸载会清空 DB 绑定，
        // 此时丢弃已失效的解析结果、按原始链接继续（卸载语义），不让条目滞留。
        #[cfg(feature = "plugins")]
        let mut resolver_identity = String::new();
        #[cfg(feature = "plugins")]
        let plugin_applied: Option<crate::plugin::ResolveResult> = {
            let mut resolver = self.db.get_task_resolver(task_id).await.unwrap_or_default();
            let reentered = self.resume_applied.remove(task_id);
            if resolver.is_empty() {
                // 二段解析任务（任务组成员）的链接是分享页，绝不能按原始链接下载：绑定为空
                // （旧版 fail-closed 建档 / 卸载清绑定 / 忽略插件重试）时按创建规则重新绑定，
                // 仍无可用插件则 fail-closed。再入结果属于已失效的绑定，一并丢弃。
                let second_stage = match self.db.get_task_resolver_item(task_id).await {
                    Ok(item) => !item.is_empty(),
                    Err(error) => {
                        crate::logger::report_error(
                            "download-manager",
                            "load resolver item for resume guard",
                            &error,
                        );
                        true
                    }
                };
                if second_stage {
                    match self.rebind_second_stage(task_id).await {
                        Some(identity) => {
                            self.begin_resolve_resume(task_id, identity).await;
                        }
                        None => self.fail_second_stage_unavailable(task_id).await,
                    }
                    return;
                }
                self.resolved_egress.remove(task_id);
                None
            } else if let Some(res) = reentered {
                resolver_identity = std::mem::take(&mut resolver);
                Some(res) // 再入
            } else {
                self.begin_resolve_resume(task_id, resolver).await;
                return;
            }
        };
        #[cfg_attr(not(feature = "plugins"), allow(unused_mut))]
        let mut task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                log_info!("[manager] do_resume_task: task {} not found in DB", task_id);
                return;
            }
            Err(e) => {
                log_info!(
                    "[manager] do_resume_task: DB error for task {}: {}",
                    task_id,
                    e
                );
                if self
                    .progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.to_string(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: 4,
                        error_message: format!("database error: {e}"),
                        file_name: String::new(),
                        segment_details: None,
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                    tracing::debug!("download-manager notification receiver closed");
                }
                return;
            }
        };
        // 任务持久化的原始 URL（拦截上下文 `sourceUrl`），早于 resolver 改写。
        let source_url = task.url.clone();

        // 应用 resolve 改写；空 url = 放行（用原 url）。后续协议判定与下载均以
        // task.url 为准，因而自动重算 use_bt/hls/dash/ftp/ed2k。extra_headers/
        // audio_url/ephemeral 同样取出，在下方各自的落点应用——与 start 路径的
        // apply_resolve_to_queued 对称（缺一即 DASH 轨对丢音轨/鉴权直链丢头/
        // 一次性直链被 probe 作废）。
        #[cfg(feature = "plugins")]
        let plugin_resolved = plugin_applied.is_some();
        #[cfg(feature = "plugins")]
        let (
            plugin_patch,
            plugin_audio_url,
            plugin_ephemeral,
            plugin_total_bytes,
            plugin_range_supported,
        ) = if let Some(res) = plugin_applied {
            if !res.url.is_empty() {
                task.url = res.url;
            }
            if let Some(name) = res.file_name
                && !name.is_empty()
            {
                task.file_name = name;
            }
            (
                (res.extra_headers.is_some() || res.request.is_some()).then(|| {
                    (
                        resolver_identity.clone(),
                        crate::intercept::RequestPatch {
                            replace_headers: res.extra_headers,
                            ..res.request.unwrap_or_default()
                        },
                    )
                }),
                res.audio_url.filter(|a| !a.is_empty()),
                res.ephemeral,
                res.total_bytes.unwrap_or(0),
                res.range_supported,
            )
        } else {
            (None, None, false, 0, false)
        };

        // 浏览器 POST 接管的任务：请求体（常含一次性令牌）不落库，无法重放；改发
        // GET（HEAD + Range 探测）对只收 POST 的端点必然 405，对表单端点则会下到
        // 错误内容。恢复 / 重试一律明确失败，提示回浏览器重新发起。插件 resolve
        // 已给出新的请求（`plugin_resolved`）时不受此限。
        #[cfg(feature = "plugins")]
        let request_replaced = plugin_resolved;
        #[cfg(not(feature = "plugins"))]
        let request_replaced = false;
        if !request_replaced {
            match self.db.load_task_request_method(task_id).await {
                Ok(method) if !method.is_empty() => {
                    log_info!(
                        "[manager] resume_task {}: started by a {} request whose body is not \
                         persisted — refusing to resume",
                        task_id,
                        method
                    );
                    self.fail_post_only_resume(task_id, &task).await;
                    return;
                }
                Ok(_) => {}
                Err(error) => crate::logger::report_error(
                    "download-manager",
                    "load task request method",
                    &error,
                ),
            }
        }

        // 恢复持久化的浏览器请求上下文：鉴权站点（cookie+token 双因子的 fnOS、
        // 带 Authorization 的私有服务）缺它们 resume 必然 4xx。
        // 旧任务（升级前创建）三者皆空 → 行为与既往完全一致。
        let (resume_cookies, resume_referrer, resume_extra_headers) =
            match self.db.load_task_request_context(task_id).await {
                Ok(Some((c, r, h))) => {
                    let headers: std::collections::HashMap<String, String> = if h.is_empty() {
                        std::collections::HashMap::new()
                    } else {
                        serde_json::from_str(&h).unwrap_or_else(|e| {
                            log_info!(
                                "[manager] task {} extra_headers JSON 解析失败: {}",
                                task_id,
                                e
                            );
                            std::collections::HashMap::new()
                        })
                    };
                    if !c.is_empty() || !r.is_empty() || !headers.is_empty() {
                        log_info!(
                            "[manager] task {} resume 恢复请求上下文: cookies_len={}, \
                             referrer_len={}, extra_headers={}",
                            task_id,
                            c.len(),
                            r.len(),
                            headers.len()
                        );
                    }
                    (c, r, headers)
                }
                Ok(None) => Default::default(),
                Err(e) => {
                    log_info!(
                        "[manager] task {} load_task_request_context 失败: {}（按空上下文继续）",
                        task_id,
                        e
                    );
                    Default::default()
                }
            };

        // resolver 的 extraHeaders / request 补丁在 run_http_family_task 起跑前应用；
        // 这里的 resume_extra_headers 只来自 DB 中的基础请求上下文。
        // Read actual segment count from DB.  0 means "auto" — the downloader
        // will dynamically calculate the optimal count.
        let seg_count: i32 = self.db.get_task_segments(task_id).await.unwrap_or_default();

        // 域名单连接策略缓存覆盖（同 do_start_task）。
        let seg_count = if seg_count != 1 && is_single_conn_domain(&task.url) {
            log_info!(
                "[manager] resume task {} 域名命中单连接缓存，强制 segments=1",
                task_id
            );
            1
        } else {
            seg_count
        };

        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();

        let use_ftp = is_ftp_url(&task.url);
        let use_hls = hls_downloader::is_hls_url(&task.url);
        // 轨对任务：从 DB 读回音频轨 URL，重建轨对下载（与 .mpd 后缀正交）。
        let audio_url = self.db.load_audio_url(task_id).await.unwrap_or_default();
        // 插件任务：本次 resolve 的输出是权威值（含"无音轨"）。DB 里的 audio_url
        // 对插件任务只是 sidecar 删除标记（run_track_pair_inner 兜底落库），其
        // ephemeral 直链早已过期，且插件设置可能已改为无音轨模式——绝不回退。
        // 非插件任务（浏览器轨对/重启恢复）照旧读 DB 重建。
        #[cfg(feature = "plugins")]
        let audio_url = if plugin_resolved {
            plugin_audio_url
        } else {
            audio_url
        };
        let use_dash = dash_downloader::is_dash_url(&task.url) || audio_url.is_some();
        let use_bt = is_bt_url(&task.url);
        if use_bt && !self.bt_config.enabled {
            crate::log_warn!(
                "[manager] cannot resume BT task {}: {}",
                task_id,
                BT_DISABLED_MESSAGE
            );
            // 任务若仍停在「排队/下载中/准备中」假象上，落回暂停；已暂停/失败/
            // 完成的任务保持原状。
            self.park_bt_disabled_task(task_id).await;
            return;
        }
        let use_ed2k = is_ed2k_url(&task.url);

        // Insert placeholder entry (handle filled in after tokio::spawn).
        self.active_tasks.insert(
            task_id.to_string(),
            ActiveTaskEntry {
                token: cancel_token.clone(),
                generation: spawn_gen,
                handle: None,
                is_bt: use_bt,
                queue_id: task.queue_id.clone(),
            },
        );
        // Track queue membership and select the appropriate speed limiter.
        let speed_limiter = self.queue_limiter_for(&task.queue_id);

        let tid = task_id.to_string();
        let done_tx = self.done_tx.clone();
        #[cfg(feature = "bt")]
        let panic_progress_tx = self.progress_tx.clone();
        #[cfg(feature = "bt")]
        let panic_task_id = tid.clone();
        #[cfg(feature = "bt")]
        let panic_db = self.db.clone();
        let task_span = tracing::error_span!("download_task", task_id = %tid);

        let handle = if use_bt {
            // BT 不走 HTTP 出口决策：解析留下的出口一并作废。
            #[cfg(feature = "plugins")]
            self.resolved_egress.remove(task_id);
            // Lazily initialise the shared BT session.
            if let Err(e) = self.ensure_bt_session().await {
                crate::log_error!("[manager] failed to init BT session for resume: {}", e);
                if let Err(db_error) = self.db.update_task_status(task_id, 4, &e.to_string()).await
                {
                    crate::logger::report_error(
                        "download-manager",
                        "persist resumed BT session initialization error",
                        &db_error,
                    );
                }
                if self
                    .progress_tx
                    .send(ProgressUpdate {
                        task_id: tid.clone(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: 4,
                        error_message: e.to_string(),
                        file_name: String::new(),
                        segment_details: None,
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                    tracing::debug!("download-manager notification receiver closed");
                }
                self.active_tasks.remove(task_id);
                return;
            }
            #[cfg(feature = "bt")]
            {
                // bt_session is guaranteed to be Some after ensure_bt_session().
                let Some(bt_ref) = self.bt_session.as_ref() else {
                    crate::log_error!(
                        "[manager] BUG: bt_session is None after ensure_bt_session succeeded"
                    );
                    self.active_tasks.remove(task_id);
                    return;
                };

                // Try to resume from a cached handle (pause→resume within the
                // same app session).  If the handle is found, unpause it and
                // pass it to the download loop so it skips add_torrent entirely.
                let mut existing = match bt_ref.resume_task(task_id).await {
                    Ok(h) => h,
                    Err(e) => {
                        crate::log_warn!("[manager] BT resume_task error (will re-add): {}", e);
                        None
                    }
                };

                // Guard: if the user deleted the download files while the task
                // was paused (within the same app session), the cached handle's
                // in-memory piece bitfield is stale.  Reusing it would produce a
                // corrupt file because librqbit thinks pieces are present when
                // the underlying data is gone.  Detect this by checking whether
                // the output path still exists on disk.  If not, discard the
                // cached handle so that add_torrent runs full re-verification.
                if existing.is_some() && !task.file_name.is_empty() {
                    let output_path = PathBuf::from(&task.save_dir).join(&task.file_name);
                    // Also check the task-scoped staging directory: a paused
                    // download that hasn't finished yet will have its data in
                    // save_dir/.bt_stage_<task_id>/ rather than at the final path.
                    //
                    // The staging check requires actual data, not mere existence:
                    // if the user (or an external tool) deleted the staged FILE
                    // while the empty directory survived, the cached handle's
                    // in-memory piece bitfield would claim pieces that are gone
                    // from disk — librqbit would re-create the file sparse, never
                    // re-download those pieces, and "complete" a file with
                    // zero-filled holes (BUG-BT-PHANTOM-PIECES).
                    let stage_path = bt_downloader::bt_stage_dir(&task.save_dir, task_id);
                    let output_present =
                        output_path.exists() || bt_downloader::stage_dir_has_real_data(&stage_path);
                    if !output_present {
                        log_info!(
                            "[manager] BT task {} output missing/empty ({} and {}), discarding cached handle for re-verify",
                            task_id,
                            output_path.display(),
                            stage_path.display(),
                        );
                        // Delete the stale torrent from the session so add_torrent
                        // can re-add it fresh with proper piece verification.
                        // session.delete also drops the {hash}.bitv fastresume
                        // file, so the re-add cannot restore phantom pieces.
                        if let Err(error) = bt_ref.delete_task(task_id, false).await {
                            crate::logger::report_error(
                                "download-manager",
                                "discard stale BT handle",
                                &error,
                            );
                            self.active_tasks.remove(task_id);
                            return;
                        }
                        existing = None;
                    }
                }

                // Build the torrent source for resume: if the task was created
                // from a .torrent file, load the persisted bytes from DB.
                let torrent_source = if is_torrent_file_url(&task.url) {
                    let bytes = self
                        .db
                        .load_torrent_file_bytes(task_id)
                        .await
                        .unwrap_or_default()
                        .unwrap_or_default();
                    if bytes.is_empty() {
                        log_info!(
                            "[manager] BT task {} has torrent-file:// URL but no persisted bytes!",
                            task_id
                        );
                        let msg = "torrent file bytes lost — cannot resume";
                        if let Err(error) = self.db.update_task_status(task_id, 4, msg).await {
                            crate::logger::report_error(
                                "download-manager",
                                "persist task state",
                                &error,
                            );
                        }
                        self.active_tasks.remove(task_id);
                        return;
                    }
                    TorrentSource::TorrentFileBytes(bytes)
                } else {
                    TorrentSource::Magnet(task.url.clone())
                };

                // Load the persisted file selection from DB so that resumes
                // (including across app restarts where the in-memory handle is
                // gone) skip the file-selection dialog entirely.
                //
                // load_bt_selected_files returns:
                //   None        — user never confirmed a selection → show dialog
                //   Some([])    — user confirmed "all files" → skip dialog, no update_only_files
                //   Some([…])   — user confirmed a subset → skip dialog, apply update_only_files
                //
                // When existing_handle is Some (same-session resume), librqbit
                // already has the correct state; had_existing_handle=true in
                // bt_download_inner skips Phase 3.5 regardless of what we pass here.
                let (pre_selected_indices, skip_file_selection) = if existing.is_none() {
                    match self
                        .db
                        .load_bt_selected_files(task_id)
                        .await
                        .unwrap_or(None)
                    {
                        None => {
                            // Never confirmed — let Phase 3.5 show the dialog.
                            (Vec::new(), false)
                        }
                        Some(indices) if indices.is_empty() => {
                            // Confirmed "all files" — skip dialog, librqbit default is all.
                            (Vec::new(), true)
                        }
                        Some(indices) => {
                            // Confirmed subset — skip dialog, apply update_only_files.
                            (indices, false)
                        }
                    }
                } else {
                    // Existing handle: had_existing_handle handles everything.
                    (Vec::new(), false)
                };

                // Load user-specified custom name from DB for BT rename on completion.
                // 存量防护:修复前创建的任务可能把 magnet `dn` 回显误存成了
                // custom_name(见 do_start_task 的 dn_echo 规则)——恢复时同样按
                // 「等于 dn 即非重命名」豁免,避免完成落盘丢真实扩展名。
                let custom_name = {
                    let loaded = self
                        .db
                        .load_bt_custom_name(task_id)
                        .await
                        .unwrap_or_default();
                    let dn_echo = torrent_source.display_name().unwrap_or_default();
                    if !loaded.is_empty() && loaded == dn_echo {
                        String::new()
                    } else {
                        loaded
                    }
                };

                let upload_limit_bps = self.effective_task_upload_bps(&task);
                let bt_params = BtDownloadParams {
                    task_id: tid.clone(),
                    torrent_source,
                    save_dir: task.save_dir,
                    db: self.db.clone(),
                    progress_tx: self.progress_tx.clone(),
                    cancel_token,
                    session: bt_ref.session(),
                    bt_runtime: bt_ref.runtime_handle(),
                    shared_bt: bt_ref.clone(),
                    existing_handle: existing,
                    pre_selected_indices,
                    skip_file_selection,
                    custom_name,
                    selector: self.selector.clone(),
                    sink: self.sink.clone(),
                    upload_limit_bps,
                };

                tokio::spawn(
                    async move {
                        let result =
                            std::panic::AssertUnwindSafe(bt_downloader::run_bt_download(bt_params))
                                .catch_unwind()
                                .await;

                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(downloader::DownloadError::Cancelled)) => {
                                tracing::debug!("BT download worker cancelled");
                            }
                            Ok(Err(error)) => {
                                handle_bt_worker_error(
                                    &panic_task_id,
                                    &error,
                                    &panic_db,
                                    &panic_progress_tx,
                                )
                                .await;
                            }
                            Err(panic_info) => {
                                let msg = panic_message(&panic_info);
                                handle_task_panic(
                                    &panic_task_id,
                                    msg,
                                    &panic_db,
                                    &panic_progress_tx,
                                )
                                .await;
                            }
                        }

                        if done_tx
                            .send(TaskDone {
                                task_id: panic_task_id,
                                generation: spawn_gen,
                                reserved_temp_path: None, // BT 任务不使用文件名预订机制
                                phase: TaskDonePhase::Finished,
                            })
                            .await
                            .is_err()
                        {
                            // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
                            tracing::debug!("download-manager notification receiver closed");
                        }
                    }
                    .instrument(task_span),
                )
            }
            // 无 BT 后端：`ensure_bt_session` 恒返回错误并已在上面处理完毕。
            #[cfg(not(feature = "bt"))]
            {
                self.active_tasks.remove(task_id);
                return;
            }
        } else {
            // 恢复时优先沿用任务快照里的显式 User-Agent；否则重新解析队列/全局
            // 默认值。cookies/referrer/extra_headers 已在上方从 DB 恢复。
            let persisted_user_agent = resume_extra_headers.iter().find_map(|(name, value)| {
                name.eq_ignore_ascii_case("user-agent")
                    .then(|| value.clone())
            });
            let persisted_ua = persisted_user_agent.unwrap_or_default();
            // 与 start 路径共用 `task_http_context`：一次性备用链路（Auto）与解析钉定的出口
            // 在此消费，站点规则 / Auto 决策 / 专属 client 的取舍只有这一份实现。
            let (route_subject, forced_route) = self.take_start_route(&tid);
            let TaskEgress {
                client: task_client,
                proxy: task_proxy,
                route: auto_route,
                auto_ctx,
                router: egress_router,
                pac: pac_routing,
            } = self.task_http_context(
                route_subject.as_deref().unwrap_or(&task.url),
                &task.proxy_url,
                &persisted_ua,
                &task.queue_id,
                task.ignore_tls_errors,
                forced_route,
            );
            let resume_user_agent = self
                .resolved_task_ua(&persisted_ua, &task.queue_id)
                .to_string();
            if let Err(e) = self.db.set_task_auto_route(&tid, auto_route).await {
                log_info!("[manager] task {} auto_route 落库失败: {}", tid, e);
            }
            if !auto_route.is_empty() {
                self.sink.emit(EngineEvent::TaskRouteChanged {
                    task_id: tid.clone(),
                    route: auto_route.to_string(),
                });
            }
            let range_verified = self.db.get_task_range_verified(&tid).await.unwrap_or(true);
            #[cfg(feature = "plugins")]
            let range_verified = range_verified || plugin_range_supported;

            // 普通 HTTP 续传已持久化文件大小、分段进度与原始 validator。
            // 已知大小时直接从真实缺口发 Range 请求，避免 HEAD / Range 0-0 /
            // plain GET 重新消耗一次性签名 URL。下游直接发送 Range，并用持久化的
            // ETag / Last-Modified 对 206 响应做版本后验校验。
            //
            // 未知大小仅沿用未验证 hint 任务的免 probe 语义；FTP/HLS/DASH/ED2K
            // 使用各自下载器，不把 HTTP hint 契约扩散到协议专用路径。
            let is_plain_http = !use_ftp && !use_hls && !use_dash && !use_ed2k;
            // 换源后首次续传必须真实探测新链接：库里的大小属于旧链接，沿用它作 hint
            // 会跳过探测，`settle_source_change` 拿旧值与旧值比较而永远判「未换文件」，
            // 新文件更小时分段越过 EOF 持续 416。读标记失败按已换源处理（多一次探测，
            // 绝不拼接）。
            let source_changed = is_plain_http
                && match self.db.task_source_changed(&tid).await {
                    Ok(flag) => flag,
                    Err(e) => {
                        log_info!(
                            "[manager] task {} 读取换源标记失败，按已换源处理: {}",
                            tid,
                            e
                        );
                        true
                    }
                };
            let resume_hint = if source_changed {
                0
            } else if is_plain_http && task.total_bytes > 0 {
                task.total_bytes
            } else if is_plain_http && !range_verified {
                -1
            } else {
                0
            };
            // ephemeral 直链（一次性/防探测签名 URL）：resolve 刚给出新鲜直链，
            // probe 会作废它 → 跳过 probe（与 start 路径 hint 语义对称）。大小
            // 优先取 resolve 的 total_bytes，其次 DB 已知值，再退 -1（未知但可下）。
            #[cfg(feature = "plugins")]
            let resume_hint = if plugin_ephemeral {
                if plugin_total_bytes > 0 {
                    plugin_total_bytes
                } else if task.total_bytes > 0 {
                    task.total_bytes
                } else {
                    -1
                }
            } else {
                resume_hint
            };

            // 多 CDN 聚合输入与主请求使用同一份恢复 UA。
            let cdn = self.cdn_task_input(task.ignore_tls_errors, &task_proxy, &resume_user_agent);
            // 插件解析出的直链常绑定解析时的出口 IP：多网卡聚合会换公网 IP，不启用。
            let multi_nic = if route_subject.is_some() {
                None
            } else {
                self.multi_nic_input(
                    task.ignore_tls_errors,
                    &task_proxy,
                    auto_ctx
                        .as_ref()
                        .is_some_and(|ctx| !ctx.alternates.is_empty()),
                    &resume_user_agent,
                )
            };
            let task_unattended = self.db.is_task_unattended(&tid).await.unwrap_or(false);
            // 从未有过任何落盘进度的任务（稍后下载 / 排队中暂停后恢复 / 重启自动
            // 恢复 / 重新下载 / 答复后重入）等同全新起跑：经启动序幕统一处理命名、
            // 跳过、dedup 与「文件已存在」询问。
            let fresh = self.never_started(&task, audio_url.is_some()).await;
            let decision = match self.db.get_exists_decision(&tid).await {
                Ok(value) => crate::file_exists::ExistsDecision::from_db(&value),
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "load file-exists decision",
                        &error,
                    );
                    crate::file_exists::ExistsDecision::None
                }
            };
            let overwrite = if matches!(self.file_exists_behavior, FileExistsBehavior::Overwrite) {
                crate::file_exists::OverwritePolicy::Any
            } else if !fresh && decision == crate::file_exists::ExistsDecision::Overwrite {
                crate::file_exists::OverwritePolicy::Only(task.file_name.clone())
            } else {
                crate::file_exists::OverwritePolicy::Never
            };
            let support = ask_support(&task.url, audio_url.is_some(), false);
            let interceptor = self.task_interceptor(&tid, &source_url, &task.url).await;
            let run_meta = crate::intercept::RunMeta {
                proxy_url: task.proxy_url.clone(),
                ignore_tls_errors: task.ignore_tls_errors,
                user_agent: resume_user_agent.clone(),
            };
            #[cfg(feature = "plugins")]
            let resolver_patch = plugin_patch;
            #[cfg(not(feature = "plugins"))]
            let resolver_patch: Option<(String, crate::intercept::RequestPatch)> = None;
            let params = DownloadParams {
                task_id: tid.clone(),
                url: task.url,
                save_dir: task.save_dir,
                file_name: task.file_name,
                segment_count: seg_count,
                is_resume: !fresh,
                db: self.db.clone(),
                client: task_client,
                progress_tx: self.progress_tx.clone(),
                cancel_token,
                speed_limiter,
                cookies: resume_cookies.clone(),
                referrer: resume_referrer.clone(),
                hint_file_size: resume_hint,
                range_verified,
                proxy_config: task_proxy,
                sink: self.sink.clone(),
                selector: self.selector.clone(),
                checksum: task.checksum,
                extra_headers: resume_extra_headers.clone(),
                // 请求体不落库，走到这里的必是 GET 任务（非 GET 任务已在体首以
                // POST_ONLY_RESUME_MESSAGE 失败）。cookies/referrer/extra_headers
                // 从 DB 恢复，保住鉴权站点的 resume。
                spec: {
                    let mut spec = downloader::RequestSpec::from_captured(
                        None,
                        resume_cookies,
                        resume_referrer,
                        resume_extra_headers,
                        None,
                    );
                    spec.egress = egress_router;
                    spec
                },
                audio_url,
                auto_max_connections: self.auto_max_connections,
                use_server_time: self.use_server_time,
                overwrite,
                spawn_gen: spawn_gen as i64,
                // 只有 HLS / DASH(含轨对)的 mux 用得到 ffmpeg，其余协议不必在 actor 上
                // 为每个任务读一次配置、stat 并扫描 PATH。
                ffmpeg_path: if use_hls || use_dash {
                    crate::components::resolve_ffmpeg(&self.db, &self.data_dir).await
                } else {
                    None
                },
                cdn,
                auto_proxy: auto_ctx,
                multi_nic,
                unattended: task_unattended,
                preprobed: None,
                interceptor,
                connection_cap: 0,
                run_meta,
            };

            let reserved_set = Arc::clone(&self.reserved_temp_paths);
            tokio::spawn(
                run_http_family_task(HttpFamilyRun {
                    params,
                    use_ftp,
                    use_hls,
                    use_dash,
                    use_ed2k,
                    prelude: fresh.then_some(PreludeCtx {
                        global: self.file_exists_behavior,
                        support,
                    }),
                    reserved_set,
                    done_tx,
                    spawn_gen,
                    resolver_patch,
                    pac_routing,
                })
                .instrument(task_span),
            )
        };
        if let Some(entry) = self.active_tasks.get_mut(task_id) {
            entry.handle = Some(handle);
        }
    }

    pub async fn cancel_task(&mut self, task_id: &str) {
        // 清除自动重试计数，与 delete_task / resume_task 对齐。取消是用户的
        // 明确意图，必须从自动重试状态中移除，使后续 create/resume 干净起步。
        self.auto_retry_counts.remove(task_id);
        #[cfg(feature = "plugins")]
        self.intercept_retry_counts.remove(task_id);
        self.auto_failover_pending.remove(task_id);
        self.auto_failover_attempts.remove(task_id);
        self.clear_pending_resolve(task_id);

        // Remove from pending queue if queued.
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
            // 移除排队任务后立即广播,使其余排队任务位置实时前移(与 pause_task/
            // delete_tasks_batch 一致;否则要等后续 drain_queue 期间 UI 显示过时位置)。
            self.broadcast_queue_positions();
        }

        // 库已是完成态而 TaskDone 尚未消费：取消不得把成品改写成 cancelled，
        // 交给 TaskDone 走完成收尾。
        if self
            .active_tasks
            .get(task_id)
            .is_some_and(|e| e.handle.is_some())
            && self
                .db
                .load_task_by_id(task_id)
                .await
                .is_ok_and(|t| t.is_some_and(|t| t.status == 3))
        {
            return;
        }

        if let Some(entry) = self.active_tasks.remove(task_id) {
            entry.token.cancel();
            // For BT tasks, explicitly pause the torrent in the session so
            // that fast-resume data is preserved and the user can resume later.
            // This mirrors what pause_task does for BT tasks.
            #[cfg(feature = "bt")]
            if entry.is_bt
                && let Some(ref bt) = self.bt_session
                && let Err(error) = bt.pause_task(task_id).await
            {
                crate::logger::report_error("download-manager", "pause BT task", &error);
            }
            // Clean up the JoinHandle so it doesn't linger after cancellation.
            if let Some(handle) = entry.handle {
                drop(handle);
            }
        }

        if let Err(error) = self
            .db
            .update_task_status(task_id, 4, CANCELLED_ERROR_MESSAGE)
            .await
        {
            crate::logger::report_error("download-manager", "persist task state", &error);
            self.drain_queue().await;
            #[cfg(feature = "bt")]
            self.maybe_release_bt_session().await;
            return;
        }

        // Send update with actual task info if available
        let task_info = match self.db.load_task_by_id(task_id).await {
            Ok(task) => task,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load cancelled task information",
                    &error,
                );
                self.drain_queue().await;
                #[cfg(feature = "bt")]
                self.maybe_release_bt_session().await;
                return;
            }
        };

        self.sink.emit(EngineEvent::TaskProgress {
            task_id: task_id.to_string(),
            status: 4,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            file_name: task_info
                .as_ref()
                .map(|t| t.file_name.clone())
                .unwrap_or_default(),
            save_dir: task_info
                .as_ref()
                .map(|t| t.save_dir.clone())
                .unwrap_or_default(),
            url: task_info
                .as_ref()
                .map(|t| t.url.clone())
                .unwrap_or_default(),
            error_message: CANCELLED_ERROR_MESSAGE.to_string(),
            upload_speed_bps: 0,
            uploaded_bytes: task_info
                .as_ref()
                .map(|t| t.uploaded_bytes)
                .unwrap_or_default(),
            seeding_status: task_info
                .as_ref()
                .map(|t| t.seeding_status)
                .unwrap_or_default(),
            seeding_message: task_info
                .as_ref()
                .map(|t| t.seeding_message.clone())
                .unwrap_or_default(),
            seeding_time_secs: task_info
                .as_ref()
                .map(|t| t.seeding_time_secs)
                .unwrap_or_default(),
        });

        // 通知平面：onCancel（fire-and-forget，与 onDone/onError 互斥——只有
        // 未被 on_task_done 抢先处理终态的取消才会走到这里；593#1）。
        #[cfg(feature = "plugins")]
        if let Some(pm) = &self.plugin_manager {
            let url = task_info
                .as_ref()
                .map(|t| t.url.clone())
                .unwrap_or_default();
            pm.notify(crate::plugin::PluginEvent::Cancel {
                task_id: task_id.to_string(),
                url,
                reason: crate::plugin::CancelReason::UserCancelled,
            })
            .await;
        }

        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
        #[cfg(feature = "bt")]
        self.maybe_release_bt_session().await;
    }

    /// 删任务事务提交后，只广播实际修改过回链的源和一次 badge 快照。
    async fn broadcast_deleted_rss_sources(&mut self, sources: Vec<String>) {
        if sources.is_empty() {
            return;
        }
        for source_id in sources {
            self.rss.broadcast_items(&source_id, Vec::new()).await;
        }
        self.rss.broadcast_sources().await;
    }

    /// Delete task record and optionally its files on disk.
    ///
    /// If the task is actively downloading, the cancellation token is triggered
    /// first and we **await** the spawned task's JoinHandle so that all
    /// network connections and file handles are released before deletion.
    pub async fn delete_task(&mut self, task_id: &str, delete_files: bool) {
        self.auto_retry_counts.remove(task_id);
        #[cfg(feature = "plugins")]
        self.intercept_retry_counts.remove(task_id);
        self.auto_failover_pending.remove(task_id);
        self.auto_failover_attempts.remove(task_id);
        self.retry_unschedule(task_id);
        self.clear_pending_resolve(task_id);

        #[cfg(feature = "bt")]
        self.integrity_checks.cancel(task_id);
        // Remove from pending queue if queued.
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
            // 移除排队任务后立即广播剩余排队任务位置(与 delete_tasks_batch 一致)。
            self.broadcast_queue_positions();
        }

        // Cancel the active download (if any) and wait for the spawned task
        // to exit, ensuring all network sockets and file handles are closed.
        // 先声明"删除":HLS 直播在任务令牌取消(= 停止并保存)时会把已录内容定稿,
        // 删除不能走这条路径。登记在等待下载器退出之后释放。
        hls_downloader::request_discard(task_id);
        let maybe_handle = if let Some(entry) = self.active_tasks.remove(task_id) {
            entry.token.cancel();
            entry.handle
        } else {
            None
        };
        let handle_timed_out = if let Some(mut handle) = maybe_handle {
            // Timeout guard: don't block forever if the task misbehaves.
            // 取 `&mut handle` 使超时后仍能 abort：纯 async 的 HTTP/coordinator
            // 任务会在下一个 await 点立即取消，比单纯 drop(detach) 更快释放
            // 连接/文件句柄，避免被删任务在我们清理文件后又写回孤立文件。
            // 对 BT/FTP 的 spawn_blocking 内层阻塞线程，abort 外层 future 不影响
            // 阻塞线程本身，仍依赖 cancel_token + 下方 deferred_cleanup 兜底。
            wait_for_download_exit(
                &mut handle,
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                "wait for deleted download",
            )
            .await
        } else {
            false
        };
        hls_downloader::clear_discard(task_id);
        if handle_timed_out {
            log_info!(
                "[manager] delete_task {}: handle wait timed out, spawned task may still be running",
                task_id
            );
        }

        // 记录文件信息，供 handle 超时后延迟二次清理使用
        let mut deferred_cleanup: Option<(String, String, String, bool)> = None;

        // 在 handle 等待之后加载 DB，确保获取到 spawned task 可能更新的最新 file_name。
        let mut cleanup_error: Option<downloader::DownloadError> = None;
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(task) => task,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load task before deletion",
                    &error,
                );
                self.drain_queue().await;
                return;
            }
        };
        if let Some(t) = task {
            // 最终产物认领判定（详见 task_owns_final_file）：未完成任务的
            // save_dir/file_name 可能是未 dedup 的原始名，指向早前同名任务
            // 留下的成品，删除文件时必须跳过。
            let claimed_by_other = delete_files
                && file_name_claimed_by_others(
                    &self.db,
                    &t.save_dir,
                    &t.file_name,
                    &[task_id.to_string()],
                )
                .await;
            let adopted = self.skip_adopted(task_id).await;
            let owns_final = task_owns_final_file(t.status) && !claimed_by_other && !adopted;
            let has_started = task_has_started(t.status, t.downloaded_bytes) && !adopted;
            // 若 handle 超时且文件名已知，记录信息以便后续延迟清理
            if handle_timed_out && !t.file_name.is_empty() {
                // BT 的最终路径同样只归完成任务所有（dedup 在完成期），
                // 延迟清理沿用同一守卫；非 BT 走到这里必然启动过（有
                // handle），file_name 已 dedup，最终路径归本任务命名空间。
                let deferred_delete_files = if is_bt_url(&t.url) {
                    delete_files && owns_final
                } else {
                    delete_files && !claimed_by_other
                };
                deferred_cleanup = Some((
                    t.save_dir.clone(),
                    t.file_name.clone(),
                    t.url.clone(),
                    deferred_delete_files,
                ));
                // handle 被 abort 时 spawned task 的 TaskDone 不会发出,on_task_done
                // 无法释放 reserved_temp_paths 预订。此处按 DB 中(已 dedup 落库的)
                // file_name 重建预订路径并主动移除,避免残留到进程重启(否则后续同名
                // 下载会被误判为占用而 dedup 改名)。HashSet::remove 幂等无副作用。
                let reserved = PathBuf::from(&t.save_dir).join(format!(
                    "{}{}",
                    t.file_name,
                    downloader::TEMP_EXT
                ));
                lock_reserved(&self.reserved_temp_paths).remove(&reserved);
            }
            let path = PathBuf::from(&t.save_dir).join(&t.file_name);

            if is_bt_url(&t.url) {
                #[cfg(feature = "bt")]
                {
                    // Permanently remove from librqbit session (clears
                    // persistence data and optionally deletes files via
                    // librqbit's own cleanup).
                    if let Some(ref bt) = self.bt_session {
                        let handle_found = match bt.delete_task(task_id, delete_files).await {
                            Ok(found) => found,
                            Err(error) => {
                                crate::logger::report_error(
                                    "download-manager",
                                    "delete BT task",
                                    &error,
                                );
                                self.drain_queue().await;
                                return;
                            }
                        };
                        if !handle_found {
                            // Handle not in map: the task is still in the
                            // add_torrent phase (e.g. magnet DHT resolution)
                            // or a reseed's local-data check is in flight.
                            // Register a pending delete so the detached
                            // add_torrent closure / reseed closure cleans up the
                            // librqbit session entry (and files) once it resolves.
                            bt.register_pending_delete(task_id, delete_files).await;
                            // 二次检查：上面 miss 与 pending 写入之间存在 await
                            // 窗口，句柄可能恰好在此期间落位（且对方闭包的
                            // pending 检查已经跑完拿了个空）——此时 pending 将
                            // 永远无人消费，幽灵条目留在会话里继续做种/占位。
                            // 消费掉自己刚写的 pending，走正常删除。
                            if bt.cached_handle(task_id).await.is_some()
                                && let Some(df) = bt.take_pending_delete(task_id).await
                                && let Err(error) = bt.delete_task(task_id, df).await
                            {
                                crate::logger::report_error(
                                    "download-manager",
                                    "delete late BT handle",
                                    &error,
                                );
                                self.drain_queue().await;
                                return;
                            }
                        }
                    } else {
                        // BT 会话未创建（本次启动未跑过 BT 任务）：按路径直接
                        // 删 parts 边车（有会话时由 bt.delete_task 一并清理）。
                        crate::bt_partfile::remove_sidecar(&crate::bt_partfile::sidecar_path(
                            &self.app_data_dir,
                            task_id,
                        ));
                    }
                }
                // Fallback filesystem cleanup: covers the cross-session case
                // where the app restarted after completion (handle not in
                // SharedBtSession.handles) and session.delete could not be
                // called above.  We skip the outer path.exists() guard and
                // let each operation fail silently if the path is absent.
                // owns_final 守卫：BT 的 dedup 在完成期，未完成任务的
                // file_name 可能撞上早前同名任务的成品目录/文件。
                if delete_files && owns_final && is_safe_file_name(&t.file_name) {
                    if path.is_dir() {
                        if let Err(error) = tokio::fs::remove_dir_all(&path).await
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            crate::logger::report_warning(
                                "download-manager",
                                "remove task files",
                                &error,
                            );
                            if cleanup_error.is_none() {
                                cleanup_error = Some(error.into());
                            }
                        }
                    } else {
                        if let Err(error) = tokio::fs::remove_file(&path).await
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            crate::logger::report_warning(
                                "download-manager",
                                "remove task files",
                                &error,
                            );
                            if cleanup_error.is_none() {
                                cleanup_error = Some(error.into());
                            }
                        }
                    }
                }

                #[cfg(feature = "bt")]
                {
                    // Always remove the task-scoped staging directory regardless
                    // of delete_files: if the download never finished, the staging
                    // dir contains partial data that should be cleaned up when the
                    // task is deleted.  If it already finished and was moved to the
                    // final path, the staging dir should be empty (or already gone).
                    let stage_dir = bt_downloader::bt_stage_dir(&t.save_dir, task_id);
                    if stage_dir.exists() {
                        log_info!(
                            "[manager] delete_task {}: removing staging dir {}",
                            task_id,
                            stage_dir.display()
                        );
                        if let Err(error) = tokio::fs::remove_dir_all(&stage_dir).await
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            crate::logger::report_warning(
                                "download-manager",
                                "remove task files",
                                &error,
                            );
                            if cleanup_error.is_none() {
                                cleanup_error = Some(error.into());
                            }
                        }
                    }
                }
            } else {
                // HTTP / FTP / HLS / DASH: always clean up the in-progress temp file
                let temp_path =
                    PathBuf::from(format!("{}{}", path.display(), downloader::TEMP_EXT));
                if let Err(e) = tokio::fs::remove_file(&temp_path).await
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    log_info!(
                        "[manager] delete_task {}: remove temp {} failed: {}",
                        task_id,
                        temp_path.display(),
                        e
                    );
                    if cleanup_error.is_none() {
                        cleanup_error = Some(e.into());
                    }
                }

                // DASH audio sidecar: clean up .audio.m4a and its .part temp
                // 轨对任务（视频轨 URL 非 .mpd）也持有 sidecar，需一并清理。
                let has_audio_sidecar = dash_downloader::is_dash_url(&t.url)
                    || hls_downloader::is_hls_url(&t.url)
                    || self
                        .db
                        .load_audio_url(&t.task_id)
                        .await
                        .unwrap_or_default()
                        .is_some();
                if has_audio_sidecar {
                    let audio_path = dash_downloader::build_audio_path(&path);
                    let audio_temp =
                        PathBuf::from(format!("{}{}", audio_path.display(), downloader::TEMP_EXT));
                    if let Err(error) = tokio::fs::remove_file(&audio_temp).await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        crate::logger::report_warning(
                            "download-manager",
                            "remove task files",
                            &error,
                        );
                        if cleanup_error.is_none() {
                            cleanup_error = Some(error.into());
                        }
                    }
                    if delete_files
                        && has_started
                        && let Err(error) = tokio::fs::remove_file(&audio_path).await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        crate::logger::report_warning(
                            "download-manager",
                            "remove task files",
                            &error,
                        );
                        if cleanup_error.is_none() {
                            cleanup_error = Some(error.into());
                        }
                    }
                }

                if delete_files
                    && owns_final
                    && is_safe_file_name(&t.file_name)
                    && let Err(e) = tokio::fs::remove_file(&path).await
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    log_info!(
                        "[manager] delete_task {}: remove file {} failed: {}",
                        task_id,
                        path.display(),
                        e
                    );
                    if cleanup_error.is_none() {
                        cleanup_error = Some(e.into());
                    }
                }
            }
        }

        // 插件登记的衍生产物（如转码 mp4）随任务文件一并删除；须在 DB 行
        // 删除前读取登记表。
        if delete_files
            && let Ok(Some(t)) = self.db.load_task_by_id(task_id).await
            && let Err(error) =
                delete_task_artifact_files(&self.db, task_id, &t.save_dir, &[task_id.to_string()])
                    .await
        {
            crate::logger::report_error("download-manager", "delete task artifacts", &error);
            if cleanup_error.is_none() {
                cleanup_error = Some(error);
            }
        }

        // 竞争修复：若 handle 等待超时（spawned task 可能仍在运行），它可能在首次
        // 清理之后才创建临时文件。延迟二次清理以捕获这类孤立文件。
        // 下载器中新增的早期 cancel 检查已大幅缩小竞争窗口，此处为兜底保护。
        if let Some((save_dir, file_name, url, del_files)) = deferred_cleanup {
            let tid = task_id.to_string();
            tokio::spawn(deferred_file_cleanup(
                save_dir, file_name, url, del_files, tid,
            ));
        }

        if let Some(error) = cleanup_error {
            crate::logger::report_error("download-manager", "delete task files", &error);
            self.drain_queue().await;
            self.sync_queue_occupancy();
            return;
        }
        // 任务行删除后就查不到组归属：先登记组成员目录，删除成功后清理留空的组目录。
        let group_dirs = if delete_files {
            self.group_dir_targets(&[task_id.to_string()]).await
        } else {
            Vec::new()
        };
        match self.db.delete_task(task_id).await {
            Ok(sources) => self.broadcast_deleted_rss_sources(sources).await,
            Err(error) => {
                crate::logger::report_error("download-manager", "delete task record", &error);
                self.drain_queue().await;
                self.sync_queue_occupancy();
                return;
            }
        }
        self.prune_group_dirs(group_dirs, &[task_id.to_string()])
            .await;

        // Notify progress_reporter so it can remove its per-task HashMap
        // entries (states, last_dart_send, last_db_save).  Without this the
        // reporter leaks ~300-1400 bytes per deleted task indefinitely.
        if self
            .progress_tx
            .send(ProgressUpdate {
                task_id: task_id.to_string(),
                downloaded_bytes: 0,
                total_bytes: 0,
                status: 4, // triggers cleanup at progress_reporter
                error_message: "deleted".to_string(),
                file_name: String::new(),
                segment_details: None,
                ..Default::default()
            })
            .await
            .is_err()
        {
            // 接收方随 actor/预览生命周期退出，不再需要此一次性通知。
            tracing::debug!("download-manager notification receiver closed");
        }

        // Bug 4 修复：被删除的任务从 auto_paused_ids 中移除，
        // 避免 clear_priority 之后徒劳地对已删除任务调用 resume_task，
        // 产生无意义的 DB 查询或错误日志。
        self.auto_paused_ids.retain(|id| id != task_id);

        // Boost 守卫：若优先任务被删除，取消 Boost 并恢复其他任务
        if self.priority_task_id.as_deref() == Some(task_id) {
            self.clear_priority().await;
        }

        // 组 GC 钩子：成员删除后清理无成员的孤儿组行（D8 生命周期）。
        let gc_count = self.db.gc_empty_groups().await.unwrap_or(0);
        if gc_count > 0 {
            self.send_all_groups().await;
        }
        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
        self.sync_queue_occupancy();
        self.maybe_wal_checkpoint().await;
        #[cfg(feature = "bt")]
        self.maybe_release_bt_session().await;
    }

    // -----------------------------------------------------------------------
    // Batch operations — single IPC for N tasks
    // -----------------------------------------------------------------------

    /// 批量删除任务：取消在途下载、并发清理文件，再在一个事务里删掉清理成功的任务记录，
    /// 最后经 progress reporter 发一条 [`EngineEvent::TasksDeleted`]（排在这些任务已入队的
    /// 进度帧之后）。
    ///
    /// - 删除所需的库内事实（「文件已存在」决定、独立音轨、衍生产物、同名产物归属）整批
    ///   预读，不逐任务查询；
    /// - 单个任务清理失败（文件删不掉、超时、清理任务 panic）只保留该任务，其余照常删除，
    ///   失败数见 [`BatchDeleteOutcome::failed`]；
    /// - 预读失败时不动任何任务；提交失败时不删任何记录；两者都返回错误。
    pub async fn delete_tasks_batch(
        &mut self,
        task_ids: &[String],
        delete_files: bool,
    ) -> Result<BatchDeleteOutcome, crate::db::DbError> {
        if task_ids.is_empty() {
            return Ok(BatchDeleteOutcome::default());
        }
        log_info!(
            "[manager] delete_tasks_batch: {} tasks, delete_files={}",
            task_ids.len(),
            delete_files
        );
        // 预读成功后才动运行态：读库失败不会留下「已取消却未删除」的任务。
        let mut plans = self.plan_batch_deletion(task_ids, delete_files).await?;
        // 任务行删除后就查不到组归属：先登记组成员目录，删除成功后清理留空的组目录。
        let group_dirs = if delete_files {
            self.group_dir_targets(task_ids).await
        } else {
            Vec::new()
        };

        let id_set: HashSet<&str> = task_ids.iter().map(String::as_str).collect();
        self.pending_queue
            .retain(|q| !id_set.contains(q.task_id.as_str()));
        // 队列变更后立即广播剩余排队任务的位置，不等清理结束后的 drain_queue。
        self.broadcast_queue_positions();

        let file_permits = Arc::new(Semaphore::new(DELETE_FILE_CONCURRENCY));
        #[cfg(feature = "bt")]
        let app_data_dir: Arc<str> = Arc::from(self.app_data_dir.as_str());
        let mut workers = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            hls_downloader::request_discard(task_id);
            #[cfg(feature = "bt")]
            self.integrity_checks.cancel(task_id);
            let handle = self
                .active_tasks
                .remove(task_id.as_str())
                .and_then(|entry| {
                    entry.token.cancel();
                    entry.handle
                });
            let files = plans.remove(task_id.as_str());
            if let Some(files) = &files
                && !files.is_bt
            {
                // abort 超时时 on_task_done 不会执行，预订与重试计数在此（&mut self 上下文）
                // 主动释放；HashSet::remove 幂等。
                let reserved = PathBuf::from(&files.save_dir).join(format!(
                    "{}{}",
                    files.file_name,
                    downloader::TEMP_EXT
                ));
                lock_reserved(&self.reserved_temp_paths).remove(&reserved);
                self.auto_retry_counts.remove(task_id.as_str());
            }
            let job = DeleteCleanup {
                task_id: task_id.clone(),
                handle,
                files,
                #[cfg(feature = "bt")]
                bt_session: self.bt_session.clone(),
                #[cfg(feature = "bt")]
                app_data_dir: Arc::clone(&app_data_dir),
                file_permits: Arc::clone(&file_permits),
            };
            workers.push((task_id, tokio::spawn(run_delete_cleanup(job))));
        }

        let mut deleted = Vec::with_capacity(workers.len());
        let mut failed = 0;
        for (task_id, worker) in workers {
            let joined = worker.await;
            hls_downloader::clear_discard(task_id);
            match joined {
                Ok(Ok(())) => deleted.push(task_id.clone()),
                Ok(Err(error)) => {
                    crate::logger::report_error(
                        "download-manager",
                        "clean up task before batch deletion",
                        &error,
                    );
                    failed += 1;
                }
                Err(error) => {
                    if error.is_cancelled() {
                        tracing::debug!("task cleanup worker cancelled");
                    } else {
                        crate::logger::report_error(
                            "download-manager",
                            "join task cleanup worker",
                            &error,
                        );
                    }
                    failed += 1;
                }
            }
        }

        if !deleted.is_empty() {
            match self.db.delete_tasks_batch(&deleted).await {
                Ok(sources) => {
                    self.broadcast_deleted_rss_sources(sources).await;
                    self.prune_group_dirs(group_dirs, &deleted).await;
                }
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "delete batch task records",
                        &error,
                    );
                    self.drain_queue().await;
                    self.sync_queue_occupancy();
                    return Err(error);
                }
            }
            let deleted_set: HashSet<&str> = deleted.iter().map(String::as_str).collect();
            self.auto_paused_ids
                .retain(|id| !deleted_set.contains(id.as_str()));
            for task_id in &deleted {
                self.retry_unschedule(task_id);
            }
            if self
                .priority_task_id
                .as_deref()
                .is_some_and(|id| deleted_set.contains(id))
            {
                self.clear_priority().await;
            }
            let outcome = BatchDeleteOutcome {
                deleted: deleted.len(),
                failed,
            };
            // 经 reporter 发出：排在这些任务已入队的进度帧之后，并顺带清理其内部状态表。
            if self
                .progress_tx
                .send(ProgressUpdate {
                    deleted_tasks: deleted,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("download-manager deletion receiver closed");
            }
            // 组 GC 钩子：批量删除后清理无成员的孤儿组行（D8 生命周期）。
            let gc_count = self.db.gc_empty_groups().await.unwrap_or(0);
            if gc_count > 0 {
                self.send_all_groups().await;
            }
            self.finish_batch_deletion().await;
            return Ok(outcome);
        }
        self.finish_batch_deletion().await;
        Ok(BatchDeleteOutcome { deleted: 0, failed })
    }

    /// 批量删除收尾：腾出的并发槽只在最后统一调度一次。
    async fn finish_batch_deletion(&mut self) {
        self.drain_queue().await;
        self.sync_queue_occupancy();
        self.maybe_wal_checkpoint().await;
        #[cfg(feature = "bt")]
        self.maybe_release_bt_session().await;
    }

    /// 整批预读删除所需的库内事实，逐任务算出要清理的文件（库里已无记录的任务不在结果中）。
    async fn plan_batch_deletion(
        &self,
        task_ids: &[String],
        delete_files: bool,
    ) -> Result<HashMap<String, DeleteFiles>, crate::db::DbError> {
        let tasks = self.db.load_tasks_by_ids(task_ids).await?;
        let mut save_dirs: Vec<String> = tasks.iter().map(|task| task.save_dir.clone()).collect();
        save_dirs.sort_unstable();
        save_dirs.dedup();
        let facts = self.db.load_deletion_facts(task_ids, &save_dirs).await?;
        let batch: HashSet<&str> = task_ids.iter().map(String::as_str).collect();
        // 同 `file_name_claimed_by_others`：名字同时登记在批外任务名下时不得删除。
        let claimed_by_other = |save_dir: &str, file_name: &str| {
            !file_name.is_empty()
                && facts
                    .file_owners
                    .get(&(save_dir.to_owned(), file_name.to_owned()))
                    .is_some_and(|owners| owners.iter().any(|id| !batch.contains(id.as_str())))
        };
        let mut plans = HashMap::with_capacity(tasks.len());
        for task in tasks {
            // 采纳了既有文件（逐任务决定为跳过）的任务不拥有该文件。
            let adopted = facts
                .exists_decisions
                .get(&task.task_id)
                .is_some_and(|decision| {
                    crate::file_exists::ExistsDecision::from_db(decision)
                        == crate::file_exists::ExistsDecision::Skip
                });
            // 最终产物认领判定（详见 task_owns_final_file）：未完成任务的 file_name
            // 可能是未 dedup 的原始名，指向早前同名任务留下的成品。
            let claimed = delete_files && claimed_by_other(&task.save_dir, &task.file_name);
            let artifacts = if delete_files {
                facts
                    .artifacts
                    .get(&task.task_id)
                    .into_iter()
                    .flatten()
                    .filter(|name| {
                        is_safe_file_name(name) && !claimed_by_other(&task.save_dir, name)
                    })
                    .map(|name| PathBuf::from(&task.save_dir).join(name))
                    .collect()
            } else {
                Vec::new()
            };
            let files = DeleteFiles {
                is_bt: is_bt_url(&task.url),
                delete_files,
                owns_final: task_owns_final_file(task.status) && !claimed && !adopted,
                has_started: task_has_started(task.status, task.downloaded_bytes) && !adopted,
                claimed_by_other: claimed,
                has_audio_sidecar: dash_downloader::is_dash_url(&task.url)
                    || hls_downloader::is_hls_url(&task.url)
                    || facts.has_audio_url.contains(&task.task_id),
                artifacts,
                save_dir: task.save_dir,
                file_name: task.file_name,
                url: task.url,
            };
            plans.insert(task.task_id, files);
        }
        Ok(plans)
    }

    /// Batch resume multiple tasks.  Pre-loads all task info in one DB query
    /// to avoid N+1 queries, then processes each with the cached data.
    ///
    /// 批量事件契约：循环内不逐任务广播——排队任务统一批量落库 pending 后，
    /// 尾部一次 [`EngineEvent::QueuePositionsChanged`] + 一次
    /// [`EngineEvent::TasksSnapshot`]，N 任务恒定常数条消息（此前为 2N 条）。
    pub async fn batch_resume(&mut self, task_ids: &[String]) {
        if task_ids.is_empty() {
            return;
        }

        // Batch-load all task info to avoid N separate DB queries.
        let task_map: HashMap<String, TaskInfo> = match self.db.load_tasks_by_ids(task_ids).await {
            Ok(tasks) => tasks.into_iter().map(|t| (t.task_id.clone(), t)).collect(),
            Err(e) => {
                log_info!("[manager] batch_resume: load_tasks_by_ids error: {}", e);
                // Fallback to per-task queries.
                for tid in task_ids {
                    self.resume_task(tid).await;
                }
                return;
            }
        };

        // 收集进入 pending_queue 排队的任务，循环后统一落库/广播。
        let mut queued: Vec<String> = Vec::new();
        for tid in task_ids {
            if let Some(task_row) = task_map.get(tid.as_str())
                && self.resume_task_with_row(tid, task_row.clone()).await
            {
                queued.push(tid.clone());
            }
        }
        if !queued.is_empty() {
            // 快照按 DB 生成：排队任务必须持久化为 pending(0)，否则尾部
            // 快照会把它们回显成 paused。
            if let Err(e) = self.db.update_tasks_status_batch(&queued, 0).await {
                crate::logger::report_error(
                    "download-manager",
                    "persist resumed pending tasks",
                    &e,
                );
                let failed_ids: HashSet<&str> = queued.iter().map(String::as_str).collect();
                self.pending_queue
                    .retain(|queued| !failed_ids.contains(queued.task_id.as_str()));
                self.broadcast_queue_positions();
                self.send_tasks_snapshot().await;
                return;
            }
            self.broadcast_queue_positions();
        }
        self.send_tasks_snapshot().await;
    }

    /// 完成态任务的「继续」只有一种含义：BT 停止做种后恢复做种。其余完成态一律
    /// 忽略（不改库、不发事件）——普通恢复流水线会把已完成的文件从头重下（#935），
    /// 任何宿主 / 客户端直接调 resume 都不得触发；要重下只走 [`Self::restart_task`]。
    /// 返回 `true` 表示任务是完成态且已处理完毕，调用方不得再走普通恢复路径。
    async fn resume_completed(&mut self, task_id: &str, task: &TaskInfo) -> bool {
        if task.status != 3 {
            return false;
        }
        #[cfg(feature = "bt")]
        if self.try_resume_seeding(task_id, task).await {
            return true;
        }
        log_info!(
            "[manager] resume_task {}: completed task — ignored (redownload goes through restart_task)",
            task_id
        );
        true
    }

    /// 已完成任务的做种恢复：停止态（2..=7，用户暂停或限制达标）重新注册
    /// 为做种者，或在活动做种数达上限时进入做种队列。返回 `true` 表示该
    /// 任务按做种语义处理完毕（含失败提示），调用方不得再走普通恢复路径；
    /// `false` 表示任务不属于做种恢复场景。
    ///
    /// 因分享率 / 时长 / 不活跃限制（2 / 3 / 7）停止的任务，手动恢复即「强制做种」
    /// （对齐 qBittorrent 强制继续）：置 `seed_limits_exempt`，求值 tick 不再按
    /// 全局 / 任务级限制停它，直到用户再次停止做种（[`Self::pause_task_inner`]）；
    /// 否则同一条件下一个 tick 就会把刚恢复的任务停回去。标记落库，重启后的自动
    /// 续种同样生效。
    #[cfg(feature = "bt")]
    async fn try_resume_seeding(&mut self, task_id: &str, task: &TaskInfo) -> bool {
        if task.status != 3 || !(2..=7).contains(&task.seeding_status) {
            return false;
        }
        let stopped_status = task.seeding_status;
        let stopped_message = task.seeding_message.as_str();
        if [
            SeedingStopReason::RatioReached,
            SeedingStopReason::TimeReached,
            SeedingStopReason::InactiveTimeReached,
        ]
        .iter()
        .any(|reason| reason.as_i32() == stopped_status)
            && let Err(error) = self.db.set_task_seed_limits_exempt(task_id, true).await
        {
            crate::logger::report_error(
                "download-manager",
                "persist seed limits exemption",
                &error,
            );
        }
        // 重启后 BT 会话是惰性创建的——恢复做种前先把它拉起来。
        if self.bt_session.is_none()
            && let Err(e) = self.ensure_bt_session().await
        {
            log_info!(
                "[manager] resume_task {}: BT session init failed: {}",
                task_id,
                e
            );
            self.emit_progress_from_db(task_id, 3, stopped_status, stopped_message, 0)
                .await;
            return true;
        }
        let Some(bt) = self.bt_session.clone() else {
            // ensure_bt_session 成功后不可能为 None；防御性兜底。
            self.emit_progress_from_db(task_id, 3, stopped_status, stopped_message, 0)
                .await;
            return true;
        };
        // 恢复做种也是 BT 活动：结束「仅暂停任务保活」的空闲期。
        self.bt_paused_idle_since = None;
        let Some(handle) = bt.cached_handle(task_id).await else {
            // 重启后句柄丢失：从磁盘已有数据重新挂载（初检可能耗时数分钟，
            // 不能阻塞 actor），期间以排队做种态提示校验中。
            self.spawn_reseed_from_disk(bt, task).await;
            return true;
        };
        let seed_time_base = self.db.get_task_seeding_time(task_id).await.unwrap_or(0);
        let registration = bt
            .register_seeder(
                task_id,
                handle,
                task.uploaded_at_completion,
                0,
                seed_time_base,
            )
            .await;
        match registration {
            SeedingRegistration::Activated | SeedingRegistration::AlreadyPresent => {
                if let Err(e) = bt.resume_task(task_id).await {
                    // unpause 失败不得谎报做种中：回滚注册并保持停止态。
                    log_info!("[manager] resume_task {}: BT resume failed: {}", task_id, e);
                    if let Some(seed) = bt.unregister_seeder(task_id).await
                        && let Err(error) = self
                            .db
                            .set_task_seeding_time(task_id, seed.seed_time_secs)
                            .await
                    {
                        crate::logger::report_error(
                            "download-manager",
                            "persist task state",
                            &error,
                        );
                    }
                    self.emit_progress_from_db(task_id, 3, stopped_status, stopped_message, 0)
                        .await;
                    return true;
                }
                if let Err(error) = self
                    .db
                    .set_task_seeding_active(task_id, chrono::Local::now().timestamp())
                    .await
                {
                    crate::logger::report_error(
                        "download-manager",
                        "persist seeding state",
                        &error,
                    );
                    if let Err(rollback_error) =
                        rollback_seeding_registration(&bt, &self.db, task_id).await
                    {
                        crate::logger::report_error(
                            "download-manager",
                            "roll back seeding activation",
                            &rollback_error,
                        );
                    }
                    return true;
                }
                self.emit_progress_from_db(task_id, 3, SEEDING_STATUS_ACTIVE, "", 0)
                    .await;
            }
            SeedingRegistration::Queued => {
                if let Err(error) = self.db.set_task_seeding_queued(task_id).await {
                    crate::logger::report_error(
                        "download-manager",
                        "persist seeding state",
                        &error,
                    );
                    if let Err(rollback_error) =
                        rollback_seeding_registration(&bt, &self.db, task_id).await
                    {
                        crate::logger::report_error(
                            "download-manager",
                            "roll back seeding activation",
                            &rollback_error,
                        );
                    }
                    return true;
                }
                self.emit_progress_from_db(
                    task_id,
                    3,
                    SEEDING_STATUS_QUEUED,
                    SEEDING_QUEUED_MESSAGE,
                    0,
                )
                .await;
            }
        }
        true
    }

    /// BT 任务实况：已连接 peer（含速度）、peer 数量汇总与逐 tracker 汇报状态。
    /// 任务未在跑 / 暂停时 peer 为空，tracker 行仍列出（状态「未联系」）。
    ///
    /// # Errors
    /// 任务不存在、不是 BT 任务或读库失败。
    #[cfg(feature = "bt")]
    pub async fn bt_status(
        &self,
        task_id: &str,
    ) -> Result<crate::bt_status::BtStatusView, crate::bt_local_seed::LocalSeedError> {
        use crate::bt_local_seed::LocalSeedError;
        use crate::bt_status::{BtStatusView, PeerCounts, PeerRow};
        let db_error = |error: crate::db::DbError| LocalSeedError::Operation(format!("{error:#}"));
        let task = self
            .db
            .load_task_by_id(task_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| LocalSeedError::InvalidArgument("task not found".to_owned()))?;
        let bytes = self
            .db
            .load_torrent_file_bytes(task_id)
            .await
            .map_err(db_error)?
            .filter(|bytes| !bytes.is_empty());
        let (info_hash, own): (String, Vec<String>) = if let Some(bytes) = &bytes {
            let parsed = librqbit::torrent_from_bytes(bytes).map_err(|error| {
                LocalSeedError::InvalidArgument(format!("torrent parse error: {error}"))
            })?;
            let own = parsed
                .iter_announce()
                .map(|tracker| String::from_utf8_lossy(tracker.as_ref()).into_owned())
                .collect();
            (parsed.info_hash.as_string(), own)
        } else if is_magnet(&task.url) {
            let info_hash = TorrentSource::Magnet(task.url.clone())
                .info_hash_hex()
                .ok_or_else(|| LocalSeedError::InvalidArgument("invalid magnet link".to_owned()))?;
            let own = librqbit::Magnet::parse(&crate::bt_downloader::normalize_magnet(&task.url))
                .map(|magnet| magnet.trackers)
                .unwrap_or_default();
            (info_hash, own)
        } else {
            return Err(LocalSeedError::InvalidArgument(
                "not a BitTorrent task".to_owned(),
            ));
        };
        let session = crate::bt_downloader::extra_trackers_for(&self.bt_config);
        let trackers = crate::bt_status::merge_trackers(
            &own,
            &session,
            &crate::bt_status::snapshot(&info_hash),
        );

        let guard_hash: Option<[u8; 20]> = hex::decode(&info_hash)
            .ok()
            .and_then(|bytes| bytes.try_into().ok());
        let guard = self.bt_guard.guard();
        let mut peers: Vec<PeerRow> = Vec::new();
        let mut counts = PeerCounts::default();
        let handle = match &self.bt_session {
            Some(bt) => bt.cached_handle(task_id).await,
            None => None,
        };
        if let Some(handle) = handle {
            if let Some(live) = handle.live() {
                // 国旗：库未就绪时后台下载（只在有人查看 peer 列表时触发）。
                crate::geoip::ensure_background(self.data_dir.clone(), self.proxy_config.clone());
                let filter = serde_json::from_value(serde_json::json!({ "state": "live" }))
                    .map_err(|error| LocalSeedError::Operation(error.to_string()))?;
                peers = live
                    .per_peer_stats_snapshot(filter)
                    .peers
                    .into_iter()
                    .map(|(addr, stats)| {
                        let country = crate::geoip::lookup_addr(&addr);
                        let insight = guard_hash.and_then(|hash| {
                            addr.parse::<std::net::SocketAddr>()
                                .ok()
                                .and_then(|socket| guard.peer_insight(&hash, socket))
                        });
                        PeerRow {
                            addr,
                            client: stats.client_name.unwrap_or_default(),
                            transport: stats
                                .conn_kind
                                .map(|kind| format!("{kind:?}").to_lowercase())
                                .unwrap_or_default(),
                            downloaded: stats.counters.fetched_bytes,
                            uploaded: stats.counters.uploaded_bytes,
                            down_bps: 0,
                            up_bps: 0,
                            country,
                            insight,
                        }
                    })
                    .collect();
            }
            if let Some(live) = handle.stats().live {
                let ps = &live.snapshot.peer_stats;
                counts = PeerCounts {
                    live: ps.live,
                    connecting: ps.connecting,
                    queued: ps.queued,
                    seen: ps.seen,
                    dead: ps.dead,
                };
            }
        }
        crate::bt_status::apply_speeds(task_id, &mut peers);
        peers.sort_by(|a, b| {
            (b.down_bps + b.up_bps)
                .cmp(&(a.down_bps + a.up_bps))
                .then_with(|| (b.downloaded + b.uploaded).cmp(&(a.downloaded + a.uploaded)))
                .then_with(|| a.addr.cmp(&b.addr))
        });
        Ok(BtStatusView {
            dht_enabled: self.bt_config.enable_dht,
            counts,
            trackers,
            peers,
        })
    }

    /// BT 任务的分享材料：磁力链接（含种子自带与会话追加的 tracker）与可选 `.torrent` 字节。
    /// 优先用库里持久化的种子字节；纯磁力任务只有磁力链接，不返回种子字节。
    ///
    /// # Errors
    /// 任务不存在、不是 BT 任务或读库失败。
    #[cfg(feature = "bt")]
    pub async fn torrent_share(
        &self,
        task_id: &str,
        include_torrent: bool,
    ) -> Result<
        (crate::bt_local_seed::TorrentShare, Option<Vec<u8>>),
        crate::bt_local_seed::LocalSeedError,
    > {
        use crate::bt_local_seed::{LocalSeedError, TorrentShare, torrent_share};
        let db_error = |error: crate::db::DbError| LocalSeedError::Operation(format!("{error:#}"));
        let task = self
            .db
            .load_task_by_id(task_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| LocalSeedError::InvalidArgument("task not found".to_owned()))?;
        let extras = crate::bt_downloader::extra_trackers_for(&self.bt_config);
        let bytes = self
            .db
            .load_torrent_file_bytes(task_id)
            .await
            .map_err(db_error)?
            .filter(|bytes| !bytes.is_empty());
        if let Some(bytes) = bytes {
            let share = torrent_share(&bytes, &extras)?;
            return Ok((share, include_torrent.then_some(bytes)));
        }
        if is_magnet(&task.url) {
            let info_hash = TorrentSource::Magnet(task.url.clone())
                .info_hash_hex()
                .ok_or_else(|| LocalSeedError::InvalidArgument("invalid magnet link".to_owned()))?;
            let name = crate::bt_downloader::magnet_display_name(&task.url).unwrap_or_default();
            let magnet = crate::bt_downloader::magnet_with_trackers(&task.url, &extras);
            return Ok((
                TorrentShare {
                    info_hash,
                    name,
                    magnet,
                },
                None,
            ));
        }
        Err(LocalSeedError::InvalidArgument(
            "not a BitTorrent task".to_owned(),
        ))
    }

    /// 把磁盘上已有的内容登记为「已完成 + 做种」的 BT 任务并立即开始做种（本机做种）。
    ///
    /// 数据校验与挂载走续种路径（[`Self::spawn_reseed_from_disk`]，BT runtime 上异步），
    /// 期间显示为排队做种并附校验说明；校验失败回退「用户停止」并附原因。`unlimited` 为真时
    /// 任务级分享率 / 时长 / 不活跃限制全部写成「不限制」（永不因限制停止）。
    /// 用户显式发起，不受「任务完成后自动做种」全局开关约束。
    ///
    /// # Errors
    /// BT 被禁用、会话不可用或落库失败。
    #[cfg(feature = "bt")]
    pub async fn create_local_seed(
        &mut self,
        plan: crate::bt_local_seed::LocalSeedPlan,
        queue_id: String,
        unlimited: bool,
    ) -> Result<String, crate::bt_local_seed::LocalSeedError> {
        use crate::bt_local_seed::LocalSeedError;
        if !self.bt_config.enabled {
            return Err(LocalSeedError::InvalidArgument(
                BT_DISABLED_MESSAGE.to_owned(),
            ));
        }
        let db_error = |error: crate::db::DbError| LocalSeedError::Operation(format!("{error:#}"));
        // 同一 info hash 已被别的任务持有（下载 / 做种，含纯磁力任务）：不重复创建。
        let new_hash = TorrentSource::TorrentFileBytes(plan.torrent_bytes.clone()).info_hash_hex();
        if let Some(new_hash) = new_hash {
            let (torrents, magnets) = self.db.list_bt_task_refs().await.map_err(db_error)?;
            for (id, name) in torrents {
                let Some(bytes) = self
                    .db
                    .load_torrent_file_bytes(&id)
                    .await
                    .map_err(db_error)?
                else {
                    continue;
                };
                if TorrentSource::TorrentFileBytes(bytes)
                    .info_hash_hex()
                    .as_deref()
                    == Some(new_hash.as_str())
                {
                    return Err(LocalSeedError::Duplicate { task_id: id, name });
                }
            }
            for (id, url, name) in magnets {
                if TorrentSource::Magnet(url).info_hash_hex().as_deref() == Some(new_hash.as_str())
                {
                    return Err(LocalSeedError::Duplicate { task_id: id, name });
                }
            }
        }
        let task_id = Uuid::now_v7().to_string();
        self.db
            .insert_task(
                &task_id,
                "torrent-file://local",
                &plan.file_name,
                &plan.save_dir,
                1,
                plan.total_bytes,
                "",
                &queue_id,
                "",
                3,
            )
            .await
            .map_err(db_error)?;
        self.db
            .save_torrent_file_bytes(&task_id, &plan.torrent_bytes)
            .await
            .map_err(db_error)?;
        self.db
            .update_task_progress(&task_id, plan.total_bytes)
            .await
            .map_err(db_error)?;
        self.db
            .update_task_status(&task_id, 3, "")
            .await
            .map_err(db_error)?;
        if unlimited {
            self.db
                .set_task_seed_limits(&task_id, -1, -1, -1, -1, 0)
                .await
                .map_err(db_error)?;
        }
        let stopped = SeedingStopReason::UserStopped;
        self.db
            .update_task_seeding_status(&task_id, stopped.as_i32(), stopped.message())
            .await
            .map_err(db_error)?;
        let task = self
            .db
            .load_task_by_id(&task_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| LocalSeedError::Operation("local seed task vanished".to_owned()))?;
        self.sink
            .emit(EngineEvent::TaskChanged(Box::new(task.clone())));
        if !self.try_resume_seeding(&task_id, &task).await {
            return Err(LocalSeedError::Operation(
                "failed to start seeding".to_owned(),
            ));
        }
        Ok(task_id)
    }

    /// 应用重启后的做种恢复：句柄已丢失，把磁盘上的完成数据重新挂载进
    /// librqbit（paused 添加 + 完整性校验，见
    /// [`SharedBtSession::readd_for_seeding`]），校验通过后按活动做种上限
    /// 注册为做种者或排队。初检在 BT runtime 上异步执行，不阻塞 actor；
    /// 期间任务显示为排队做种（附校验说明），失败回退停止态并附原因。
    /// 中途崩溃残留的排队态由下次启动的 `reset_stale_seeding` 兜底。
    #[cfg(feature = "bt")]
    async fn spawn_reseed_from_disk(&self, bt: Arc<SharedBtSession>, task: &TaskInfo) {
        let stopped_status = task.seeding_status;
        let stopped_message = task.seeding_message.clone();
        let task_id = task.task_id.clone();

        // 数据源重建：.torrent 任务从 DB 取回种子字节，其余按 magnet 处理。
        let source = if is_magnet(&task.url) {
            TorrentSource::Magnet(task.url.clone())
        } else {
            let bytes = self
                .db
                .load_torrent_file_bytes(&task_id)
                .await
                .unwrap_or_default()
                .unwrap_or_default();
            if bytes.is_empty() {
                log_info!(
                    "[manager] reseed {}: no torrent bytes persisted, cannot re-add",
                    task_id
                );
                self.emit_progress_from_db(&task_id, 3, stopped_status, &stopped_message, 0)
                    .await;
                return;
            }
            TorrentSource::TorrentFileBytes(bytes)
        };
        // 文件选择：completed 任务必然确认过；子集烘焙进 add options。
        let only_files = match self
            .db
            .load_bt_selected_files(&task_id)
            .await
            .unwrap_or(None)
        {
            Some(list) if !list.is_empty() => Some(
                list.into_iter()
                    .map(|i| i.max(0) as usize)
                    .collect::<Vec<usize>>(),
            ),
            _ => None,
        };
        // parts 边车（完成时写下）：优先用它做种——选中文件按记录的最终
        // 路径打开、未选文件路由到边车 blob，不在 save_dir 重建任何占位
        // 文件，扁平化/重命名布局也能通过校验。缺失/损坏时回退默认
        // storage（下面的 output_folder 推断仅在回退路径生效）。
        let parts_factory = match crate::bt_partfile::load_seed_factory(
            &crate::bt_partfile::sidecar_path(&self.app_data_dir, &task_id),
            std::path::Path::new(&task.save_dir),
        ) {
            Ok(f) => f,
            Err(e) => {
                log_info!(
                    "[manager] reseed {}: parts sidecar unusable ({}) — falling back to default storage",
                    task_id,
                    e
                );
                None
            }
        };
        // 回退路径（无边车）：完成时数据已从 staging 搬到 save_dir——多
        // 文件种子在 save_dir/<file_name>/ 下保留 torrent 相对布局，以该
        // 目录为 output_folder；单文件直接落在 save_dir。重命名过的单文件
        // 与 torrent 内部名不匹配，会在完整性校验处失败并附原因提示。
        let root = std::path::Path::new(&task.save_dir).join(&task.file_name);
        let output_folder = if root.is_dir() {
            root.to_string_lossy().into_owned()
        } else {
            task.save_dir.clone()
        };

        // 校验中提示：复用排队做种态（附说明），结束后被真实状态覆盖。
        if let Err(error) = self.db.set_task_seeding_queued(&task_id).await {
            crate::logger::report_error("download-manager", "persist task state", &error);
            return;
        }
        self.emit_progress_from_db(
            &task_id,
            3,
            SEEDING_STATUS_QUEUED,
            "verifying local data",
            0,
        )
        .await;

        let db = self.db.clone();
        let sink = self.sink.clone();
        let uploaded_at_completion = task.uploaded_at_completion;
        let upload_limit_bps = self.effective_task_upload_bps(task);
        // 在途登记与关停信号：会话重建（改监听类设置 / 禁用 BT）时，本任务不再向
        // 旧会话登记，由 `invalidate_bt_session` 在新会话上统一重新挂载。
        let reseed_guard = bt.begin_reseed(&task_id);
        let closing = bt.closing_token();
        bt.clone().runtime_handle().spawn(async move {
            let _reseed_guard = reseed_guard;
            let readd = tokio::select! {
                () = closing.cancelled() => return,
                result = bt.readd_for_seeding(
                    &task_id,
                    source,
                    output_folder,
                    only_files,
                    upload_limit_bps,
                    parts_factory,
                ) => result,
            };
            if closing.is_cancelled() {
                return;
            }
            match readd {
                Ok(handle) => {
                    // 校验窗口期删除的消费点：readd 期间（本地数据校验可长达
                    // 分钟级）用户删除任务时句柄尚未入 map，manager 只能写
                    // pending delete——此处必须消费，否则任务行已删、种子却
                    // 注册做种成幽灵条目（占做种位、阻止 BT 会话释放）。
                    // 句柄已在 readd 内 store：pending 若写得更晚，由 manager
                    // 删除路径的「二次检查」兜底消费，两侧合并闭合竞态窗口。
                    if let Some(del_files) = bt.take_pending_delete(&task_id).await {
                        log_info!(
                            "[manager] reseed {}: pending delete applied after local-data check (delete_files={})",
                            task_id,
                            del_files
                        );
                        if let Err(error) = bt.delete_task(&task_id, del_files).await {
                            crate::logger::report_error("download-manager", "delete reseed handle", &error);
                        }
                        return;
                    }
                    let seed_time_base = db.get_task_seeding_time(&task_id).await.unwrap_or(0);
                    let registration = bt
                        .register_seeder(
                            &task_id,
                            handle,
                            uploaded_at_completion,
                            0,
                            seed_time_base,
                        )
                        .await;
                    // 登记期间会话进入关停：做种表已被重建方读走或即将读到本任务
                    // （守卫仍在），不再写库，免得盖掉重建方的状态。
                    if closing.is_cancelled() {
                        return;
                    }
                    match registration {
                        SeedingRegistration::Activated | SeedingRegistration::AlreadyPresent => {
                            if let Err(e) = bt.resume_task(&task_id).await {
                                log_info!("[manager] reseed {}: unpause failed: {}", task_id, e);
                                if let Some(seed) = bt.unregister_seeder(&task_id).await
                                    && let Err(error) = db
                                        .set_task_seeding_time(&task_id, seed.seed_time_secs)
                                        .await {
                                        crate::logger::report_error("download-manager", "persist task state", &error);
                                    }
                                if let Err(error) = db
                                    .update_task_seeding_status(
                                        &task_id,
                                        stopped_status,
                                        &stopped_message,
                                    )
                                    .await {
                                    crate::logger::report_error("download-manager", "persist task state", &error);
                                }
                                emit_seeding_progress(
                                    &db,
                                    &sink,
                                    &task_id,
                                    stopped_status,
                                    &stopped_message,
                                )
                                .await;
                                return;
                            }
                            if let Err(error) = db
                                .set_task_seeding_active(&task_id, chrono::Local::now().timestamp())
                                .await {
                                crate::logger::report_error("download-manager", "persist seeding state", &error);
                                if let Err(rollback_error) = rollback_seeding_registration(&bt, &db, &task_id).await {
                                    crate::logger::report_error("download-manager", "roll back seeding activation", &rollback_error);
                                }
                                return;
                            }
                            emit_seeding_progress(&db, &sink, &task_id, SEEDING_STATUS_ACTIVE, "")
                                .await;
                        }
                        SeedingRegistration::Queued => {
                            if let Err(error) = db.set_task_seeding_queued(&task_id).await {
                                crate::logger::report_error("download-manager", "persist seeding state", &error);
                                if let Err(rollback_error) = rollback_seeding_registration(&bt, &db, &task_id).await {
                                    crate::logger::report_error("download-manager", "roll back seeding activation", &rollback_error);
                                }
                                return;
                            }
                            emit_seeding_progress(
                                &db,
                                &sink,
                                &task_id,
                                SEEDING_STATUS_QUEUED,
                                SEEDING_QUEUED_MESSAGE,
                            )
                            .await;
                        }
                    }
                }
                Err(msg) => {
                    log_info!("[manager] reseed {}: {}", task_id, msg);
                    // 校验窗口期若有删除请求，torrent 已在 readd 失败路径中
                    // 移出会话——只需清掉挂起的 pending 条目（防残留）。
                    if let Some(delete_files) = bt.take_pending_delete(&task_id).await {
                        tracing::debug!(delete_files, "cleared pending deletion after reseed failed");
                    }
                    // 回退停止态：状态码保留原停止原因，说明换成失败原因。
                    if let Err(error) = db
                        .update_task_seeding_status(&task_id, stopped_status, &msg)
                        .await {
                        crate::logger::report_error("download-manager", "persist task state", &error);
                    }
                    emit_seeding_progress(&db, &sink, &task_id, stopped_status, &msg).await;
                }
            }
        });
    }

    /// Resume a task using a pre-loaded TaskInfo row (avoids redundant DB query).
    ///
    /// 返回 `true` = 任务已进入 `pending_queue` 排队。本函数不发任何事件：
    /// 状态持久化与广播由调用方 [`Self::batch_resume`] 在循环后统一完成。
    async fn resume_task_with_row(&mut self, task_id: &str, task_row: TaskInfo) -> bool {
        // 批量手动 resume 与单任务 resume_task 语义对齐：用户手动恢复应重置
        // 自动重试计数，给一个全新的重试配额。否则一个已耗尽配额的任务被批量
        // 恢复后，下次可重试错误会立刻命中"已耗尽"分支、停在 error，与单任务
        // 手动恢复行为不一致（BUG-BATCH-RESUME-NO-RETRY-RESET）。
        self.auto_retry_counts.remove(task_id);
        #[cfg(feature = "plugins")]
        self.intercept_retry_counts.remove(task_id);
        self.retry_unschedule(task_id);
        if let Some(pending) = self.pending_pauses.get_mut(task_id) {
            pending.resume_requested = true;
            return false;
        }

        if self.active_tasks.contains_key(task_id) {
            if self.has_pending_resolve(task_id) {
                return false;
            }
            // 与 resume_task_inner 一致：只摘除失败态（4）残留；完成态（3）的
            // 完成收尾仍在 done 通道里，保留条目并忽略。
            if task_row.status != 4 {
                return false; // still active, or completion pending — do not interrupt
            }
            log_info!(
                "[manager] resume_task {}: stale active_tasks entry (failed in DB) — force-removing",
                task_id
            );
            self.active_tasks.remove(task_id);
        }

        if self.pending_queue.iter().any(|q| q.task_id == task_id) {
            return false;
        }

        if self.resume_completed(task_id, &task_row).await {
            return false;
        }

        let is_bt = is_bt_url(&task_row.url);
        let queue_id = task_row.queue_id.clone();

        if is_bt || (self.has_capacity() && self.has_queue_capacity(&queue_id)) {
            self.do_resume_task(task_id).await;
            self.drain_queue().await;
            false
        } else {
            log_info!(
                "[manager] queuing resume for task {} (active={}, max={}, queue={})",
                task_id,
                self.active_tasks.len(),
                self.max_concurrent,
                queue_id
            );
            self.pending_queue
                .push_back(QueuedTask::for_resume(task_row));
            true
        }
    }

    /// Batch pause multiple tasks.
    ///
    /// 与逐任务 [`Self::pause_task`] 循环语义等价，但为大批量（任务组/停止
    /// 队列/全局暂停）消除逐任务事件风暴：
    /// - 排队中的任务：批量摘除 + 单次批量落库 paused，不逐任务发事件；
    ///   先摘排队再暂停活跃——顺序反了会让 pause 触发的 drain_queue 把
    ///   仍在排队的任务顶进刚释放的槽位；
    /// - 活跃任务：仍逐个走 [`Self::pause_task_silent`]（取消令牌/BT 会话/Boost
    ///   守卫/分段快照），数量受并发上限约束；批量语义不发 `task.paused`
    ///   webhook——那是通知风暴的定义；
    /// - 尾部一次 [`EngineEvent::QueuePositionsChanged`] + 一次
    ///   [`EngineEvent::TasksSnapshot`] 取代逐任务广播。
    pub async fn batch_pause(&mut self, task_ids: &[String]) {
        self.batch_pause_impl(task_ids, true, FailedPause::RetryPending)
            .await;
    }

    /// 用户显式选中任务的批量暂停：与 [`Self::batch_pause`] 相同，但选中的**任何**
    /// 失败任务都落为暂停（与单任务 [`Self::pause_task`] 一致），而不只是等待自动
    /// 重试的那些。
    pub async fn pause_selected(&mut self, task_ids: &[String]) {
        self.batch_pause_impl(task_ids, true, FailedPause::Any)
            .await;
    }

    /// 「全部暂停」语义：停下一切。与 [`Self::batch_pause`] 的区别仅在 Boost：
    /// 优先任务被暂停时，Boost 让位暂停的任务保持暂停（丢弃登记），而不是被
    /// 恢复——否则用户点全部暂停反而会拉起一批任务。
    pub async fn batch_pause_all(&mut self, task_ids: &[String]) {
        self.batch_pause_impl(task_ids, false, FailedPause::RetryPending)
            .await;
    }

    /// 正在等待自动重试（或备用链路重试）的失败任务。「全部暂停」/ 停止队列
    /// 必须把它们纳入作用对象，否则暂停后重试到点又把任务拉起来。
    pub fn retry_pending_task_ids(&self) -> Vec<String> {
        self.retry_scheduled.keys().cloned().collect()
    }

    async fn batch_pause_impl(
        &mut self,
        task_ids: &[String],
        restore_boost_paused: bool,
        failed_scope: FailedPause,
    ) {
        if task_ids.is_empty() {
            return;
        }
        let idset: HashSet<&str> = task_ids.iter().map(|s| s.as_str()).collect();
        // 优先任务在本批内：先摘掉 Boost 登记，避免 pause_task_inner 的 Boost
        // 守卫在循环中途无条件恢复 auto-paused 任务（其中可能正属于刚停止的队列）。
        let boost_released: Option<Vec<String>> = if self
            .priority_task_id
            .as_deref()
            .is_some_and(|p| idset.contains(p))
        {
            Some(self.release_priority_for_batch().await)
        } else {
            None
        };
        let queued: Vec<String> = self
            .pending_queue
            .iter()
            .filter(|e| idset.contains(e.task_id.as_str()))
            .map(|e| e.task_id.clone())
            .collect();
        if !queued.is_empty() {
            self.pending_queue
                .retain(|e| !idset.contains(e.task_id.as_str()));
            for tid in &queued {
                self.clear_pending_resolve(tid);
            }
            if let Err(e) = self.db.update_tasks_status_batch(&queued, 2).await {
                log_info!("[manager] batch_pause: persist paused error: {}", e);
            }
        }
        let active: Vec<String> = self
            .active_tasks
            .keys()
            .filter(|id| idset.contains(id.as_str()))
            .cloned()
            .collect();
        for tid in &active {
            self.pause_task_silent(tid).await;
        }
        // 做种/排队做种的任务（status=3）既不在 pending_queue 也不在
        // active_tasks，单独收集，逐个走 pause_task_silent 的做种分支
        // （停止做种 → UserStopped）。
        #[cfg(feature = "bt")]
        let seeding: Vec<String> = if let Some(ref bt) = self.bt_session {
            bt.seeding_manager()
                .all_task_ids()
                .await
                .into_iter()
                .filter(|id| idset.contains(id.as_str()))
                .collect()
        } else {
            Vec::new()
        };
        #[cfg(not(feature = "bt"))]
        let seeding: Vec<String> = Vec::new();
        for tid in &seeding {
            self.pause_task_silent(tid).await;
        }
        let handled: HashSet<&str> = queued
            .iter()
            .chain(&active)
            .chain(&seeding)
            .map(String::as_str)
            .collect();
        let candidates: Vec<&str> = idset
            .iter()
            .copied()
            .filter(|id| !handled.contains(id))
            .collect();
        let failed_paused = self.pause_failed_batch(&candidates, failed_scope).await;
        if let Some(paused) = boost_released {
            if restore_boost_paused {
                self.restore_boost_paused_after_batch(paused, &idset).await;
            } else {
                log_info!(
                    "[manager] batch_pause_all: boost cancelled, {} auto-paused task(s) stay paused",
                    paused.len()
                );
            }
            self.broadcast_queue_positions();
            self.sink.emit(EngineEvent::PriorityTaskChanged {
                priority_task_id: String::new(),
                auto_paused_count: 0,
            });
        } else if queued.is_empty() && active.is_empty() && seeding.is_empty() && !failed_paused {
            return; // 全员本就非活跃非排队非做种非失败：保持既往完全无操作、无广播。
        }
        if !queued.is_empty() {
            self.broadcast_queue_positions();
        }
        self.send_tasks_snapshot().await;
    }

    /// 批量暂停中的失败任务：落 paused（单次批量落库、不逐任务发事件，尾部快照覆盖），
    /// 让已排程的自动重试到点时被 `is_task_in_error` 拦下。返回是否有任务被暂停。
    async fn pause_failed_batch(&mut self, candidates: &[&str], scope: FailedPause) -> bool {
        let mut failed: Vec<String> = Vec::new();
        for &id in candidates {
            if scope == FailedPause::RetryPending && !self.retry_scheduled.contains_key(id) {
                continue;
            }
            match self.db.load_task_by_id(id).await {
                Ok(Some(task)) if task.status == 4 => failed.push(task.task_id),
                Ok(_) => {}
                Err(error) => {
                    crate::logger::report_error("download-manager", "load task state", &error);
                }
            }
        }
        if failed.is_empty() {
            return false;
        }
        if let Err(error) = self.db.update_tasks_status_batch(&failed, 2).await {
            crate::logger::report_error("download-manager", "persist task state", &error);
            return false;
        }
        for id in &failed {
            self.retry_unschedule(id);
            self.auto_failover_pending.remove(id);
        }
        true
    }

    /// 批量暂停取消 Boost 后恢复让位任务：跳过本批内的任务、已完成的任务和
    /// 所在队列已停止的任务（与 `resume_all_eligible` 的准入一致）。
    async fn restore_boost_paused_after_batch(
        &mut self,
        paused: Vec<String>,
        idset: &HashSet<&str>,
    ) {
        let candidates: Vec<String> = paused
            .into_iter()
            .filter(|id| !idset.contains(id.as_str()))
            .collect();
        if candidates.is_empty() {
            return;
        }
        let rows = match self.db.load_tasks_by_ids(&candidates).await {
            Ok(r) => r,
            Err(e) => {
                log_info!("[manager] batch_pause: boost restore load error: {}", e);
                return;
            }
        };
        let info: HashMap<&str, &TaskInfo> = rows.iter().map(|t| (t.task_id.as_str(), t)).collect();
        for id in &candidates {
            let Some(t) = info.get(id.as_str()) else {
                continue;
            };
            if t.status == 3 {
                continue;
            }
            let running = self
                .queues
                .get(t.queue_id.as_str())
                .map(|q| q.is_running)
                .unwrap_or(true);
            if running {
                self.resume_task(id).await;
            }
        }
    }

    /// 取消所有在途任务并等待下载 task 与 BT/DHT 持久化退出。
    pub async fn shutdown(&mut self) {
        // 做种时长平时只在状态迁移/顺带写/30 分钟兜底时落库，正常关机在此结算。
        #[cfg(feature = "bt")]
        self.flush_seed_times().await;
        #[cfg(feature = "bt")]
        self.integrity_checks.cancel_all();
        let mut handles = Vec::new();
        for (_task_id, entry) in self.active_tasks.drain() {
            entry.token.cancel();
            if let Some(handle) = entry.handle {
                handles.push(handle);
            }
        }
        self.pending_queue.clear();
        // 整体等待设上限（小于 daemon 的 10s 关停超时）：个别 spawn 卡住时不能拖死
        // 关停流程，使 actor 被整体 abort 而来不及落盘活动日志。
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        for mut handle in handles {
            if wait_for_download_exit(&mut handle, deadline, "wait for download shutdown").await {
                tracing::debug!("download shutdown deadline reached; worker aborted");
            }
        }
        #[cfg(feature = "bt")]
        if let Some(bt) = self.bt_session.take()
            && let Err(error) = tokio::task::spawn_blocking(move || match Arc::try_unwrap(bt) {
                Ok(owned) => owned.shutdown(),
                Err(shared) => shared.shutdown(),
            })
            .await
        {
            if error.is_cancelled() {
                tracing::debug!("BT shutdown worker cancelled");
            } else {
                crate::logger::report_error("download-manager", "join BT shutdown worker", &error);
            }
        }
        // BT 会话已关停（含其 Disconnected 回调）：停泵并做防吸血最终落库。
        #[cfg(feature = "bt")]
        self.bt_guard.shutdown().await;
    }
}

impl Drop for DownloadManager {
    fn drop(&mut self) {
        // Cancel all active downloads (non-blocking, just sets atomic flags).
        for (_tid, entry) in self.active_tasks.drain() {
            entry.token.cancel();
        }
        self.pending_queue.clear();
        #[cfg(feature = "bt")]
        self.integrity_checks.cancel_all();

        // Shut down the BT session on a dedicated thread to avoid deadlock.
        // `SharedBtSession::shutdown()` calls `runtime.block_on()`, which
        // panics if called from within a tokio runtime context.  Spawning a
        // std thread guarantees we are outside any runtime.
        #[cfg(feature = "bt")]
        if let Some(bt) = self.bt_session.take() {
            std::thread::spawn(move || match Arc::try_unwrap(bt) {
                Ok(owned) => owned.shutdown(),
                Err(shared) => shared.shutdown(),
            });
            // Note: we intentionally don't join the thread — the BT runtime
            // shutdown is best-effort on app exit.  The OS will reclaim
            // resources if it doesn't finish in time.
        }
    }
}

impl DownloadManager {
    // -----------------------------------------------------------------------
    // Named queue management
    // -----------------------------------------------------------------------

    /// Broadcast the current list of named queues to the host.
    pub async fn send_all_queues(&self) {
        match self.db.load_all_queues().await {
            Ok(queues) => self.sink.emit(EngineEvent::QueuesChanged(queues)),
            Err(e) => log_info!("[manager] load_all_queues error: {}", e),
        }
    }

    /// Create a new named queue and broadcast the updated list.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_queue(
        &mut self,
        name: String,
        speed_limit_kbps: i64,
        upload_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: String,
        default_segments: i32,
        default_user_agent: String,
    ) {
        let id = Uuid::now_v7().to_string();
        let position = match self.db.queue_count().await {
            Ok(n) => n,
            Err(e) => {
                log_info!("[manager] queue_count error: {}", e);
                0
            }
        };
        if let Err(e) = self
            .db
            .insert_queue(
                &id,
                &name,
                speed_limit_kbps,
                upload_limit_kbps,
                max_concurrent,
                &default_save_dir,
                position,
                default_segments,
                &default_user_agent,
            )
            .await
        {
            log_info!("[manager] insert_queue error: {}", e);
            return;
        }
        // Sync in-memory cache.
        self.queues.insert(
            id.clone(),
            QueueInfo {
                queue_id: id.clone(),
                name: name.clone(),
                speed_limit_kbps,
                upload_limit_kbps,
                max_concurrent,
                default_save_dir,
                position,
                default_segments,
                default_user_agent,
                is_running: true,
                schedule_enabled: false,
                schedule_start: String::new(),
                schedule_stop: String::new(),
                schedule_days: 127,
            },
        );
        log_info!("[manager] created queue: id={}, name={}", id, name);
        self.send_all_queues().await;
    }

    /// Update an existing queue and broadcast the updated list.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_queue(
        &mut self,
        queue_id: String,
        name: String,
        speed_limit_kbps: i64,
        upload_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: String,
        default_segments: i32,
        default_user_agent: String,
    ) {
        // 内置队列不可重命名（UI 按固定 ID 显示本地化名称）；其余设置照常。
        let name = if is_builtin_queue(&queue_id) {
            self.queues
                .get(&queue_id)
                .map(|q| q.name.clone())
                .unwrap_or(name)
        } else {
            name
        };
        if let Err(e) = self
            .db
            .update_queue(
                &queue_id,
                &name,
                speed_limit_kbps,
                upload_limit_kbps,
                max_concurrent,
                &default_save_dir,
                default_segments,
                &default_user_agent,
            )
            .await
        {
            log_info!("[manager] update_queue error: {}", e);
            return;
        }
        // Sync in-memory cache.
        if let Some(q) = self.queues.get_mut(&queue_id) {
            q.name = name;
            q.speed_limit_kbps = speed_limit_kbps;
            q.upload_limit_kbps = upload_limit_kbps;
            q.max_concurrent = max_concurrent;
            q.default_save_dir = default_save_dir;
            q.default_segments = default_segments;
            q.default_user_agent = default_user_agent;
        }
        // If a per-queue limiter already exists, update its limit in place.
        if let Some(limiter) = self.queue_limiters.get(&queue_id) {
            limiter.set_limit(self.queue_limit_bps(speed_limit_kbps));
        }
        log_info!("[manager] updated queue: {}", queue_id);
        self.send_all_queues().await;
        // 并发上限可能被调大：让被旧上限卡住的排队任务立即起跑。
        self.drain_queue().await;
    }

    /// Delete a named queue (tasks move to the builtin main queue) and
    /// broadcast.  Builtin queues (`main`/`later`) are protected.
    pub async fn delete_queue(&mut self, queue_id: String) {
        if is_builtin_queue(&queue_id) {
            log_info!(
                "[manager] delete_queue: builtin '{}' is protected",
                queue_id
            );
            return;
        }
        let moved_ids = match self.db.delete_queue(&queue_id).await {
            Ok(ids) => ids,
            Err(e) => {
                log_info!("[manager] delete_queue error: {}", e);
                return;
            }
        };
        // DB 已把任务迁到主队列；调度缓存和事件流必须在同一操作内跟进。
        for entry in self.active_tasks.values_mut() {
            if entry.queue_id == queue_id {
                entry.queue_id = MAIN_QUEUE_ID.to_owned();
            }
        }
        self.pending_queue.reassign_queue(&queue_id, MAIN_QUEUE_ID);
        self.queues.remove(&queue_id);
        self.queue_limiters.remove(&queue_id);
        self.schedule_fired.retain(|(qid, _), _| qid != &queue_id);
        for task_id in moved_ids {
            self.sink.emit(EngineEvent::TaskQueueChanged {
                task_id,
                queue_id: MAIN_QUEUE_ID.to_owned(),
            });
        }
        // 位置事件更新待排任务的 queuePosition；全量任务快照是 queue_order
        // 已归零的权威来源，TaskQueueChanged 本身只包含归属 ID。
        // 队列迁移属于显式操作，位置必须在任务/队列快照之前同步收敛。
        self.queue_positions_dirty = true;
        self.drain_queue().await;
        self.flush_queue_positions();
        self.send_tasks_snapshot().await;
        log_info!("[manager] deleted queue: {}", queue_id);
        self.send_all_queues().await;
    }

    // -----------------------------------------------------------------------
    // Task group management（多文件任务组，B3/B4 契约）
    // -----------------------------------------------------------------------

    /// 广播全部任务组快照（组建/删除/改名/回收后调用，照 [`Self::send_all_queues`]
    /// 模式）。
    pub async fn send_all_groups(&self) {
        match self.db.load_all_groups().await {
            Ok(groups) => self.sink.emit(EngineEvent::GroupsChanged(groups)),
            Err(e) => log_info!("[manager] load_all_groups error: {}", e),
        }
    }

    /// 建组：`items` 为空直接返回 `None`（不建空组；即便单条目也走本方法而非
    /// 平任务——设计文档 §7.3「选中 1 项也建组」，平任务无法携带
    /// `resolver_item` 规格选择）。落盘目标 = `base_save_dir/sanitize(group_name)
    /// /item.rel_path`（`group_name` 为空则直接用 `base_save_dir`）。逐成员经
    /// [`Self::create_task`] 创建，复用其全部并发/队列/resolver 落库/
    /// fail-closed 逻辑；结束后广播任务快照与组列表。
    pub async fn create_task_group(&mut self, spec: CreateGroupSpec) -> Option<String> {
        if spec.items.is_empty() {
            return None;
        }
        let group_save_dir = if spec.group_name.trim().is_empty() {
            spec.base_save_dir.clone()
        } else {
            PathBuf::from(&spec.base_save_dir)
                .join(naming::sanitize_filename(&spec.group_name))
                .to_string_lossy()
                .into_owned()
        };
        let group_id = Uuid::new_v4().to_string();
        if let Err(e) = self
            .db
            .insert_group(
                &group_id,
                &spec.group_name,
                &spec.source_url,
                &group_save_dir,
            )
            .await
        {
            log_info!("[manager] insert_group error: {}", e);
            return None;
        }
        // 抑制逐成员广播：N 成员的 create_task 各发一条 TaskProgress + 每次
        // 排队一条全量队列位置（O(N²) 载荷）；尾部快照 + 单次位置广播覆盖。
        self.suppress_bulk_broadcasts = true;
        for item in &spec.items {
            let save_dir = if item.rel_path.is_empty() {
                group_save_dir.clone()
            } else {
                PathBuf::from(&group_save_dir)
                    .join(&item.rel_path)
                    .to_string_lossy()
                    .into_owned()
            };
            self.create_task(NewTaskSpec {
                url: spec.source_url.clone(),
                save_dir,
                file_name: item.file_name.clone(),
                segments: spec.segments,
                cookies: spec.cookies.clone(),
                referrer: spec.referrer.clone(),
                hint_file_size: item.size,
                proxy_url: spec.proxy_url.clone(),
                user_agent: spec.user_agent.clone(),
                queue_id: spec.queue_id.clone(),
                ignore_tls_errors: spec.ignore_tls_errors,
                extra_headers: spec.extra_headers.clone(),
                start_paused: spec.start_paused,
                group_id: group_id.clone(),
                resolver_item: item.resolver_item.clone(),
                resolver_plugin_id: spec.resolver_plugin_id.clone(),
                ..Default::default()
            })
            .await;
        }
        self.suppress_bulk_broadcasts = false;
        // 逐成员抑制后统一广播一次队列位置（此前每入队一个成员就全量广播一次）。
        self.broadcast_queue_positions();
        self.send_tasks_snapshot().await;
        self.send_all_groups().await;
        Some(group_id)
    }

    /// 暂停组内成员（批量路径：排队成员批量摘除落库、活跃成员逐个取消，
    /// 尾部一次快照广播，见 [`Self::batch_pause`]；对非活跃成员是安全的空操作）。
    pub async fn pause_group(&mut self, group_id: &str) {
        let ids = self.db.group_member_ids(group_id).await.unwrap_or_default();
        self.batch_pause(&ids).await;
    }

    /// 恢复组内成员（暂停/出错/排队状态的成员会被重新启动；已完成成员跳过，
    /// 不会被重新下载）。批量路径：一次载入成员行 + 尾部一次快照广播。
    pub async fn resume_group(&mut self, group_id: &str) {
        let ids = self.db.group_member_ids(group_id).await.unwrap_or_default();
        let rows = match self.db.load_tasks_by_ids(&ids).await {
            Ok(rows) => rows,
            Err(e) => {
                log_info!("[manager] resume_group: load members error: {}", e);
                return;
            }
        };
        let status: HashMap<&str, i32> = rows
            .iter()
            .map(|t| (t.task_id.as_str(), t.status))
            .collect();
        // 按 group_member_ids 的启动顺序过滤，保持队列顺序稳定。
        let resumable: Vec<String> = ids
            .into_iter()
            .filter(|id| status.get(id.as_str()).is_some_and(|s| *s != 3))
            .collect();
        self.batch_resume(&resumable).await;
    }

    /// 仅重试组内失败（status=4）成员，跳过其余状态的成员。批量路径：
    /// 一次载入成员行取代逐成员 N+1 查询。
    pub async fn retry_group_failed(&mut self, group_id: &str) {
        let ids = self.db.group_member_ids(group_id).await.unwrap_or_default();
        let rows = match self.db.load_tasks_by_ids(&ids).await {
            Ok(rows) => rows,
            Err(e) => {
                log_info!("[manager] retry_group_failed: load members error: {}", e);
                return;
            }
        };
        let failed: HashSet<&str> = rows
            .iter()
            .filter(|t| t.status == 4)
            .map(|t| t.task_id.as_str())
            .collect();
        let to_retry: Vec<String> = ids
            .into_iter()
            .filter(|id| failed.contains(id.as_str()))
            .collect();
        self.batch_resume(&to_retry).await;
    }

    /// 删除整组：批量删除全部成员（结果语义同 [`Self::delete_tasks_batch`]）；组行由其尾部的
    /// GC 钩子（末个成员删除后 [`crate::db::Db::gc_empty_groups`]）自动回收。
    pub async fn delete_group(
        &mut self,
        group_id: &str,
        delete_files: bool,
    ) -> Result<BatchDeleteOutcome, crate::db::DbError> {
        let ids = self.db.group_member_ids(group_id).await?;
        self.delete_tasks_batch(&ids, delete_files).await
    }

    /// 重命名任务组。`name` trim 后为空则忽略（不清空组名）。
    pub async fn rename_group(&mut self, group_id: &str, name: &str) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Err(e) = self.db.rename_group(group_id, trimmed).await {
            log_info!("[manager] rename_group error: {}", e);
            return;
        }
        self.send_all_groups().await;
    }

    /// 用户显式重命名任务文件。返回稳定错误码（供前端映射 i18n）：
    /// `invalid-name` / `task-active` / `bt-unsupported` / `not-found` /
    /// `target-exists`，磁盘 rename 失败返回原始 IO 错误文本。
    ///
    /// 约束与行为：
    /// - 仅接受非活跃任务（活跃任务的写入路径持有旧名句柄，重命名会与
    ///   finalize/分段写竞争）；actor 单线程串行化保证本方法执行期间不会
    ///   有新的 start/resume 插入。
    /// - BT 任务不支持：其落盘名由 `bt_custom_name`/metadata 驱动，且可能是
    ///   目录（librqbit 持句柄期间目录 rename 必败），语义与单文件重命名不同。
    /// - 磁盘上同时迁移：最终文件、`.fdownloading` 临时文件（暂停任务续传
    ///   路径按 `save_dir/file_name + TEMP_EXT` 重建，改名后无缝续传）、
    ///   DASH 音轨 sidecar 及其临时文件。均不存在时仅改 DB（未开始/文件丢失）。
    /// - 目标名已被磁盘占用即拒绝，绝不覆盖既有文件。
    /// - 新名必须是 [`naming::sanitize_filename`] 的不动点：各下载器（HTTP / FTP /
    ///   HLS / DASH / eD2K）续传时都按清洗后的名字重建 `.fdownloading` 路径，
    ///   清洗会改写的名字（Windows 非法字符、保留设备名、首尾点 / 空格、超长）
    ///   改名后续传找不到临时文件，进度归零并遗留孤儿临时文件。
    pub async fn rename_task(&mut self, task_id: &str, new_name: &str) -> Result<(), String> {
        let new_name = new_name.trim();
        if !is_safe_file_name(new_name) {
            return Err("invalid-name".to_string());
        }
        if self.active_tasks.contains_key(task_id) {
            return Err("task-active".to_string());
        }
        let t = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return Err("not-found".to_string()),
            Err(e) => return Err(format!("db: {e}")),
        };
        // status 1/5 而不在 active_tasks 属异常残留，同样拒绝。
        if t.status == 1 || t.status == 5 {
            return Err("task-active".to_string());
        }
        if is_bt_url(&t.url) {
            return Err("bt-unsupported".to_string());
        }
        if t.file_name == new_name {
            return Ok(());
        }
        // 名字未变时上面已直接放行：`out=` 等入口落库的未清洗原名（起跑前尚未被下载器改写）
        // 原样确认不应报错。
        if naming::sanitize_filename(new_name) != new_name {
            return Err("invalid-name".to_string());
        }
        async fn rollback_rename(
            paths: &[Option<(&Path, &Path)>; 4],
            moved: &[bool; 4],
            mut primary: String,
        ) -> String {
            for (index, pair) in paths.iter().enumerate().rev() {
                if moved[index]
                    && let Some((old, new)) = pair
                    && let Err(error) = tokio::fs::rename(new, old).await
                {
                    crate::logger::report_error(
                        "download-manager",
                        "roll back task rename",
                        &error,
                    );
                    primary.push_str(&format!(
                        "; rollback {} -> {}: {error}",
                        new.display(),
                        old.display()
                    ));
                }
            }
            primary
        }

        let dir = PathBuf::from(&t.save_dir);
        let old_path = dir.join(&t.file_name);
        let new_path = dir.join(new_name);
        let old_temp = PathBuf::from(format!("{}{}", old_path.display(), downloader::TEMP_EXT));
        let new_temp = PathBuf::from(format!("{}{}", new_path.display(), downloader::TEMP_EXT));
        let case_only = t.file_name.to_lowercase() == new_name.to_lowercase();
        if !case_only
            && (tokio::fs::try_exists(&new_path)
                .await
                .map_err(|error| format!("rename: {error}"))?
                || tokio::fs::try_exists(&new_temp)
                    .await
                    .map_err(|error| format!("rename: {error}"))?)
        {
            return Err("target-exists".to_string());
        }
        let has_audio_sidecar = dash_downloader::is_dash_url(&t.url)
            || hls_downloader::is_hls_url(&t.url)
            || self
                .db
                .load_audio_url(task_id)
                .await
                .map_err(|error| format!("db: {error}"))?
                .is_some();
        let audio_paths = has_audio_sidecar.then(|| {
            let old = dash_downloader::build_audio_path(&old_path);
            let new = dash_downloader::build_audio_path(&new_path);
            let old_temp = PathBuf::from(format!("{}{}", old.display(), downloader::TEMP_EXT));
            let new_temp = PathBuf::from(format!("{}{}", new.display(), downloader::TEMP_EXT));
            (old, new, old_temp, new_temp)
        });
        let paths: [Option<(&Path, &Path)>; 4] = [
            Some((&old_path, &new_path)),
            Some((&old_temp, &new_temp)),
            audio_paths
                .as_ref()
                .map(|(old, new, _, _)| (old.as_path(), new.as_path())),
            audio_paths
                .as_ref()
                .map(|(_, _, old, new)| (old.as_path(), new.as_path())),
        ];
        let mut present = [false; 4];
        let mut moved = [false; 4];
        if is_safe_file_name(&t.file_name) {
            // 先校对全部源/目标，任何 sidecar 冲突都不能先迁移主文件。
            for (index, pair) in paths.iter().enumerate() {
                if let Some((old, new)) = pair {
                    present[index] = tokio::fs::try_exists(old)
                        .await
                        .map_err(|error| format!("rename: {error}"))?;
                    if present[index]
                        && !case_only
                        && tokio::fs::try_exists(new)
                            .await
                            .map_err(|error| format!("rename: {error}"))?
                    {
                        return Err("target-exists".to_string());
                    }
                }
            }
            for (index, pair) in paths.iter().enumerate() {
                if present[index]
                    && let Some((old, new)) = pair
                {
                    if let Err(error) = tokio::fs::rename(old, new).await {
                        return Err(
                            rollback_rename(&paths, &moved, format!("rename: {error}")).await
                        );
                    }
                    moved[index] = true;
                }
            }
        }
        if let Err(error) = self
            .db
            .set_task_file_name_with_source(task_id, new_name, false)
            .await
        {
            return Err(rollback_rename(&paths, &moved, format!("db: {error}")).await);
        }
        // 名字由用户改定：此前的逐任务「文件已存在」决定绑定的是旧名，作废。
        if let Err(error) = self.db.set_exists_decision(task_id, "").await {
            crate::logger::report_error("download-manager", "clear file-exists decision", &error);
        }
        log_info!(
            "[manager] rename_task {}: '{}' -> '{}'",
            task_id,
            t.file_name,
            new_name
        );
        self.send_tasks_snapshot().await;
        Ok(())
    }

    /// 更换任务下载源地址（旧链接失效但内容不变的场景），保留已下载分段与
    /// 进度：`task_segments` 按 `task_id` 索引，与 `url` 无关，续传直接沿用
    /// 既有 `downloaded_bytes` 从新地址继续。返回稳定错误码（供前端映射
    /// i18n）：`invalid-url` / `task-active` / `task-completed` /
    /// `bt-unsupported` / `not-found` / `protocol-unsupported` /
    /// `protocol-mismatch`，thunder 链接解码失败透传其错误文本。
    ///
    /// 约束与行为：
    /// - 仅接受非活跃、非完成任务：downloading(1)/preparing(5) 与
    ///   `rename_task` 同款活跃判定一并拒绝；completed(3) 拒绝；
    ///   pending(0)/paused(2)/error(4) 允许。
    /// - BT 任务（magnet / `torrent-file://` 哨兵）不支持：其"地址"是
    ///   info-hash/本地种子文件，与单链接换源语义不同。
    /// - 新地址若是 `thunder://` 链接先解出真实地址；解出/原始地址都必须
    ///   是 http(s) 或 ftp，且协议分类须与旧地址一致（HTTP↔HTTP、
    ///   FTP↔FTP）——跨协议换源等价于建新任务，不在本方法范围内。
    /// - 写入新地址后清空历史 `error_message`（状态不变）并重置自动重试/
    ///   failover 配额，给新地址一次干净的重试预算；同时清空首次 probe 记录的
    ///   ETag/Last-Modified 基线——新源几乎必然给出不同 validator，不清会在
    ///   续传首个 206 上触发 `VersionChanged` 清盘重下，与「换链续传」目的
    ///   相悖（内容一致性此后由 Content-Range 总大小与可选 checksum 兜底）；
    ///   同时置 `source_changed`：下次续传探测后零容差比对新旧大小，不一致（换成了
    ///   另一个文件，含只差几百 KB 的新版本）即清空进度从头下载，见
    ///   `Db::settle_source_change`；大小完全一致时沿用进度（内容是否相同无从判定）。
    ///   不主动切换任务状态——续传或重试由调用方另行触发。
    pub async fn change_task_url(&mut self, task_id: &str, new_url: &str) -> Result<(), String> {
        let new_url = new_url.trim();
        if new_url.is_empty() {
            return Err("invalid-url".to_string());
        }
        if self.active_tasks.contains_key(task_id) {
            return Err("task-active".to_string());
        }
        let t = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return Err("not-found".to_string()),
            Err(e) => return Err(format!("db: {e}")),
        };
        // status 1/5 而不在 active_tasks 属异常残留，与 rename_task 同款守卫。
        if t.status == 1 || t.status == 5 {
            return Err("task-active".to_string());
        }
        if t.status == 3 {
            return Err("task-completed".to_string());
        }
        if is_bt_url(&t.url) {
            return Err("bt-unsupported".to_string());
        }
        let decoded_new = if crate::thunder::is_thunder_url(new_url) {
            crate::thunder::decode_thunder_url(new_url)?
        } else {
            new_url.to_string()
        };
        let old_protocol =
            task_url_protocol(&t.url).ok_or_else(|| "protocol-unsupported".to_string())?;
        let new_protocol =
            task_url_protocol(&decoded_new).ok_or_else(|| "protocol-unsupported".to_string())?;
        if old_protocol != new_protocol {
            return Err("protocol-mismatch".to_string());
        }
        if t.url == decoded_new {
            return Ok(());
        }
        if let Err(e) = self.db.update_task_url(task_id, &decoded_new).await {
            log_info!("[manager] change_task_url {} db error: {}", task_id, e);
            return Err(format!("db: {e}"));
        }
        if !t.error_message.is_empty()
            && let Err(e) = self.db.update_task_status(task_id, t.status, "").await
        {
            log_info!(
                "[manager] change_task_url {} clear error_message error: {}",
                task_id,
                e
            );
        }
        if let Err(e) = self.db.set_task_validator(task_id, "", "").await {
            log_info!(
                "[manager] change_task_url {} clear validator error: {}",
                task_id,
                e
            );
        }
        if let Err(e) = self.db.clear_task_response_naming(task_id).await {
            log_info!(
                "[manager] change_task_url {} clear response naming evidence error: {}",
                task_id,
                e
            );
        }
        self.auto_retry_counts.remove(task_id);
        #[cfg(feature = "plugins")]
        self.intercept_retry_counts.remove(task_id);
        self.auto_failover_pending.remove(task_id);
        self.auto_failover_attempts.remove(task_id);
        log_info!(
            "[manager] change_task_url {}: '{}' -> '{}'",
            task_id,
            t.url,
            decoded_new
        );
        self.send_tasks_snapshot().await;
        Ok(())
    }

    /// Move a task to a different queue and broadcast the updated queue list.
    pub async fn move_task_to_queue(&mut self, task_id: String, queue_id: String) {
        // '' 已不是有效归属：兜底重映射到主队列（兼容旧客户端信号）。
        let queue_id = if queue_id.is_empty() {
            MAIN_QUEUE_ID.to_string()
        } else {
            queue_id
        };
        if let Err(e) = self.db.move_task_to_queue(&task_id, &queue_id).await {
            log_info!("[manager] move_task_to_queue error: {}", e);
            return;
        }
        // If the task is currently active, update its tracked queue.
        // Note: the existing speed limiter runs to completion; the new
        // queue limiter takes effect on next resume.
        if let Some(entry) = self.active_tasks.get_mut(&task_id) {
            entry.queue_id = queue_id.clone();
        }
        // 若任务仍在 pending_queue 等待中,同步更新其 queue_id;否则 drain_queue 会用
        // 陈旧 queue_id 做 has_queue_capacity 门控,do_start_task 又据其选定限速器与
        // 写入 active 条目,导致任务实际跑在旧队列下、并发/限速归错队列且与 DB/UI 不一致。
        // task_id 在 pending_queue 中唯一(入队前有去重守卫),命中即可 break。
        self.pending_queue.set_task_queue(&task_id, &queue_id);
        log_info!("[manager] moved task {} to queue '{}'", task_id, queue_id);
        // 定向广播归属变化：AllQueues 只带队列元数据，不带任务归属，
        // 客户端任务表若不更新会导致「移动到队列」看似无效。
        self.sink
            .emit(EngineEvent::TaskQueueChanged { task_id, queue_id });
        self.send_all_queues().await;
        // 目标队列若有空位，排队中的任务（含刚移入的）应立即起跑。
        self.drain_queue().await;
    }

    /// 启动队列：置运行态并按队列内顺序（`queue_order` → `created_at`）
    /// 恢复其中所有 pending/paused 任务，经全局/队列并发门控依次开跑或
    /// 排队等待。幂等：已在运行时仅补启动未跑的任务。
    pub async fn start_queue(&mut self, queue_id: String) {
        if !self.queues.contains_key(&queue_id) {
            log_info!("[manager] start_queue: unknown queue '{}'", queue_id);
            return;
        }
        if let Err(e) = self.db.set_queue_running(&queue_id, true).await {
            log_info!("[manager] start_queue: persist error: {}", e);
            return;
        }
        if let Some(q) = self.queues.get_mut(&queue_id) {
            q.is_running = true;
        }
        let ids = match self.db.queue_startable_task_ids(&queue_id).await {
            Ok(ids) => ids,
            Err(e) => {
                log_info!("[manager] start_queue: load tasks error: {}", e);
                Vec::new()
            }
        };
        log_info!(
            "[manager] start_queue '{}': resuming {} task(s)",
            queue_id,
            ids.len()
        );
        self.batch_resume(&ids).await;
        self.send_all_queues().await;
    }

    /// 停止队列：置停止态并暂停其中所有排队中与活跃的任务。任务保持
    /// paused，等待下次「启动队列」按序恢复；其他队列不受影响（释放的
    /// 并发槽位立即让给它们）。恒运行队列（主队列）拒绝停止。
    pub async fn stop_queue(&mut self, queue_id: String) {
        if !self.queues.contains_key(&queue_id) {
            log_info!("[manager] stop_queue: unknown queue '{}'", queue_id);
            return;
        }
        if is_always_running_queue(&queue_id) {
            log_info!("[manager] stop_queue: '{}' always runs; ignored", queue_id);
            return;
        }
        if let Err(e) = self.db.set_queue_running(&queue_id, false).await {
            log_info!("[manager] stop_queue: persist error: {}", e);
            return;
        }
        if let Some(q) = self.queues.get_mut(&queue_id) {
            q.is_running = false;
        }
        // 先摘排队中的、再暂停活跃的——顺序由 batch_pause 内部保证（反了会
        // 让 pause 触发的 drain_queue 把本队列仍在排队的任务顶进刚释放的槽位）。
        let mut to_pause: Vec<String> = self
            .pending_queue
            .iter()
            .filter(|entry| entry.queue_id == queue_id)
            .map(|entry| entry.task_id.clone())
            .collect();
        to_pause.extend(
            self.active_tasks
                .iter()
                .filter(|(_, entry)| entry.queue_id == queue_id)
                .map(|(id, _)| id.clone()),
        );
        // 等待自动重试的失败任务：不暂停的话重试到点会在已停止的队列里重新跑起来。
        to_pause.extend(
            self.retry_scheduled
                .iter()
                .filter(|(_, queue)| **queue == queue_id)
                .map(|(id, _)| id.clone()),
        );
        log_info!(
            "[manager] stop_queue '{}': pausing {} task(s)",
            queue_id,
            to_pause.len()
        );
        self.batch_pause(&to_pause).await;
        self.send_all_queues().await;
    }

    /// 更新队列的每日定时计划并广播。`start`/`stop` 为 `HH:MM`（空 = 该
    /// 边沿不定时，两者彼此独立：只停不启/只启不停均合法）；`days` 为
    /// 星期位掩码（bit0=周一 … bit6=周日），0 视作每天。非法时间格式
    /// 忽略本次更新；恒运行队列（主队列）不可定时，同样忽略。
    pub async fn set_queue_schedule(
        &mut self,
        queue_id: String,
        enabled: bool,
        start: String,
        stop: String,
        days: i32,
    ) {
        if is_always_running_queue(&queue_id) {
            log_info!(
                "[manager] set_queue_schedule: '{}' always runs and cannot be scheduled",
                queue_id
            );
            return;
        }
        if (!start.is_empty() && parse_hhmm(&start).is_none())
            || (!stop.is_empty() && parse_hhmm(&stop).is_none())
        {
            log_info!(
                "[manager] set_queue_schedule: invalid time '{}'/'{}' for '{}'",
                start,
                stop,
                queue_id
            );
            return;
        }
        let days = if days & 0x7f == 0 { 0x7f } else { days & 0x7f };
        // 两个时刻都为空的「启用」没有任何可执行的动作——规范化为未启用，
        // 杜绝「已启用但永不触发」的僵尸状态（侧栏定时图标等 UI 均以
        // schedule_enabled 判定，必须真实）。
        let enabled = enabled && !(start.is_empty() && stop.is_empty());
        if let Err(e) = self
            .db
            .set_queue_schedule(&queue_id, enabled, &start, &stop, days)
            .await
        {
            log_info!("[manager] set_queue_schedule error: {}", e);
            return;
        }
        if let Some(q) = self.queues.get_mut(&queue_id) {
            q.schedule_enabled = enabled;
            q.schedule_start = start;
            q.schedule_stop = stop;
            q.schedule_days = days;
        }
        // 计划变更后清空该队列的边沿账本，让新时刻当天仍可触发。
        self.schedule_fired.retain(|(qid, _), _| qid != &queue_id);
        log_info!("[manager] updated schedule for queue '{}'", queue_id);
        self.send_all_queues().await;
    }

    /// 持久化队列内任务顺序（写为 1..N 的 `queue_order`），「启动队列」
    /// 按此顺序恢复。调用方传入该队列任务的完整新顺序。
    pub async fn reorder_queue_tasks(&mut self, queue_id: String, ordered_ids: Vec<String>) {
        if ordered_ids.is_empty() {
            return;
        }
        if let Err(e) = self.db.reorder_queue_tasks(&queue_id, &ordered_ids).await {
            log_info!("[manager] reorder_queue_tasks error: {}", e);
        }
    }

    /// 队列定时调度 tick（宿主每 20~30s 调用一次）。
    ///
    /// 边沿触发 + 当日补触发：对每个启用定时且当天生效的队列，找出「今天
    /// 时刻已过且尚未处理」的启动/停止边沿——同一分钟内多次 tick、手动
    /// 启停后同一天不会重复触发；休眠/重启错过的边沿在当天恢复运行后补
    /// 触发一次。同天两个边沿都新近越过时只执行时间靠后的那个。
    pub async fn tick_queue_schedules(&mut self) {
        use chrono::{Datelike, Timelike};
        let now = chrono::Local::now();
        let today = now.date_naive();
        let day_bit = 1i32 << now.weekday().num_days_from_monday();
        let now_min = now.hour() * 60 + now.minute();
        let (passed_edges, actions) = due_schedule_actions(
            self.queues.values(),
            &self.schedule_fired,
            today,
            day_bit,
            now_min,
        );
        for (key, date) in passed_edges {
            self.schedule_fired.insert(key, date);
        }
        for (queue_id, is_start) in actions {
            log_info!(
                "[manager] queue schedule fired: '{}' → {}",
                queue_id,
                if is_start { "start" } else { "stop" }
            );
            if is_start {
                self.start_queue(queue_id).await;
            } else {
                self.stop_queue(queue_id).await;
            }
        }
    }

    /// RSS 轮询节拍：派发全部到期订阅的 off-actor 抓取。
    ///
    /// 与 [`Self::tick_queue_schedules`] 同款——宿主只提供节拍，判定与派发
    /// 全在引擎内；抓取本身不在 actor 上跑，本方法立即返回。
    pub fn tick_rss_sources(&mut self) {
        let now = crate::rss::unix_now();
        let (proxy, ua) = (self.proxy_config.clone(), self.global_user_agent.clone());
        self.rss.tick(now, &proxy, &ua);
    }

    /// 新建订阅并**立刻抓一次**，返回新订阅 ID（`url` 为空时 `None`）。
    ///
    /// 光靠 `last_fetch_at = 0` 等下一次 tick 不够：节拍是分钟级的，用户点完
    /// 「订阅」看到的是一条空列表，只能自己再按一次「立即抓取」。订阅这个动作
    /// 本身就是「我要这个源的内容」，抓取必须同步跟上。
    ///
    /// 所有入口（daemon RPC / REST / MCP / CLI）都应走这里而不是
    /// `rss.create_source`——后者只落库，代理与 UA 也不在 `RssManager` 手上。
    pub async fn create_rss_source(
        &mut self,
        source: crate::rss::model::RssSourceInfo,
    ) -> Option<String> {
        let id = self.rss.create_source(source).await?;
        let (proxy, ua) = (self.proxy_config.clone(), self.global_user_agent.clone());
        self.rss.refresh_now(&id, &proxy, &ua);
        Some(id)
    }

    /// 立即抓取一个订阅（「立即刷新」）。已在抓取中视为成功（幂等，
    /// [`crate::rss::RssRefreshOutcome::AlreadyRunning`]），只有订阅不存在才是
    /// [`crate::rss::RssRefreshOutcome::NotFound`]。
    pub fn refresh_rss_source(&mut self, source_id: &str) -> crate::rss::RssRefreshOutcome {
        let (proxy, ua) = (self.proxy_config.clone(), self.global_user_agent.clone());
        self.rss.refresh_now(source_id, &proxy, &ua)
    }

    /// 只读验证一个订阅地址（新建订阅向导）。结果经 RSS 回流通道返回。
    /// `provider_id` 为空按内置 RSS 处理，其余走插件订阅 provider。
    #[allow(clippy::too_many_arguments)]
    pub fn validate_rss_feed(
        &self,
        request_id: String,
        provider_id: String,
        provider_config: String,
        url: String,
        cookies: String,
        user_agent: String,
        proxy_url: String,
    ) {
        let (proxy, ua) = (self.proxy_config.clone(), self.global_user_agent.clone());
        self.rss.validate(
            request_id,
            provider_id,
            provider_config,
            url,
            cookies,
            user_agent,
            proxy_url,
            &proxy,
            &ua,
        );
    }

    /// 只读验证的**请求-应答**变体（REST `POST /api/v1/rss/validate`、CLI）。
    ///
    /// 返回一个可在 actor 之外 `await` 的 future——调用方拿到它后立刻
    /// [`tokio::spawn`] 并等 oneshot，actor 本身不会被网络 IO 阻塞
    /// （与 `spawn_resolve_preview` 的先例同款）。
    pub fn rss_validate_future(
        &self,
        provider_id: String,
        provider_config: String,
        url: String,
        cookies: String,
        user_agent: String,
        proxy_url: String,
    ) -> impl Future<Output = crate::rss::RssValidateOutcome> + Send + use<> {
        let (proxy, ua) = (self.proxy_config.clone(), self.global_user_agent.clone());
        self.rss.validate_future(
            String::new(),
            provider_id,
            provider_config,
            url,
            cookies,
            user_agent,
            proxy_url,
            &proxy,
            &ua,
        )
    }

    /// 剪贴板识别用的只读链接探测（`daemon.link.probe`）：只发一次 HEAD。
    ///
    /// client 与 meta-probe 同源（全局代理 / UA / Auto 路由）；返回的 future 可在 actor 之外
    /// `await`。非 http(s) 链接不联网，直接 `Unknown`。
    pub fn link_probe_future(
        &self,
        url: String,
    ) -> impl Future<Output = crate::meta_prober::LinkProbe> + Send + use<> {
        let egress = self.task_http_context(&url, "", "", "", false, None);
        let client = egress.client;
        let router = egress.router;
        async move {
            let lower = url.get(..8).unwrap_or(&url).to_ascii_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                return crate::meta_prober::LinkProbe {
                    verdict: crate::meta_prober::LinkVerdict::Unknown,
                    final_url: url,
                    file_name: String::new(),
                    mime: String::new(),
                    total_bytes: 0,
                };
            }
            let mut spec = crate::downloader::RequestSpec::empty_get();
            spec.egress = router;
            crate::meta_prober::probe_link_kind(&url, &client, &spec).await
        }
    }

    /// 已完成 BT 任务的完整性校验（`daemon.task.verify`）：按种子 piece 表全量重哈希
    /// 落盘数据，只读、不改任务状态。前置校验与元数据收集在 actor 内完成，返回的
    /// future 在阻塞线程上哈希、可在 actor 之外 `await`；同一任务同时只跑一个，
    /// 删除任务或引擎关停时取消。
    #[cfg(feature = "bt")]
    pub async fn bt_integrity_check_future(
        &self,
        task_id: &str,
    ) -> Result<
        impl Future<
            Output = Result<
                crate::bt_verify::BtIntegrityReport,
                crate::bt_verify::BtIntegrityError,
            >,
        > + Send
        + use<>,
        crate::bt_verify::BtIntegrityError,
    > {
        use crate::bt_verify::{BtIntegrityError, LandedCheck, MetadataSources};

        let db_error = |error: DbError| BtIntegrityError::Internal(format!("{error:#}"));
        let task = self
            .db
            .load_task_by_id(task_id)
            .await
            .map_err(db_error)?
            .ok_or(BtIntegrityError::NotFound)?;
        if task.status != 3 || !is_bt_url(&task.url) {
            return Err(BtIntegrityError::NotEligible);
        }
        let guard = self
            .integrity_checks
            .begin(task_id)
            .ok_or(BtIntegrityError::Busy)?;

        let live = match &self.bt_session {
            Some(bt) => match bt.cached_handle(task_id).await {
                Some(handle) => match handle.with_metadata(|meta| meta.torrent_bytes.clone()) {
                    Ok(bytes) => Some(bytes),
                    Err(error) => {
                        log_info!(
                            "[manager] integrity check {}: live metadata unavailable: {:#}",
                            task_id,
                            error
                        );
                        None
                    }
                },
                None => None,
            },
            None => None,
        };
        let stored = if live.is_none() {
            self.db
                .load_torrent_file_bytes(task_id)
                .await
                .map_err(db_error)?
                .map(bytes::Bytes::from)
        } else {
            None
        };
        let cached = if is_magnet(&task.url) {
            TorrentSource::Magnet(task.url.clone())
                .info_hash_hex()
                .map(|hash| {
                    (
                        crate::bt_partfile::persistence_dir(&self.app_data_dir)
                            .join(format!("{hash}.torrent")),
                        hash,
                    )
                })
        } else {
            None
        };
        let selection = self
            .db
            .load_bt_selected_files(task_id)
            .await
            .map_err(db_error)?
            .filter(|list| !list.is_empty() && list.iter().all(|&index| index >= 0))
            .map(|list| list.into_iter().map(|index| index as usize).collect());

        Ok(crate::bt_verify::run(
            LandedCheck {
                task_id: task_id.to_owned(),
                save_dir: PathBuf::from(&task.save_dir),
                file_name: task.file_name.clone(),
                selection,
                sidecar_path: crate::bt_partfile::sidecar_path(&self.app_data_dir, task_id),
                metadata: MetadataSources {
                    live,
                    stored,
                    cached,
                },
            },
            guard,
        ))
    }

    /// RSS off-actor 回流的唯一入口（宿主 actor 的 `rss_rx` 分支调用）。
    ///
    /// 抓取结果先经 `RssManager` 完成去重/过滤/落库，再把「应下载」的条目
    /// 逐条走 [`Self::create_task`]——任务创建的唯一收敛点不因 RSS 而分叉。
    pub async fn on_rss_event(&mut self, event: crate::rss::RssEvent) {
        let outcome = match event {
            crate::rss::RssEvent::Validated(v) => {
                self.rss.emit_validated(*v);
                return;
            }
            crate::rss::RssEvent::TorrentReady(t) => {
                self.on_rss_torrent_ready(*t).await;
                return;
            }
            crate::rss::RssEvent::Fetched(f) => *f,
        };
        let source_id = outcome.source_id.clone();
        let plans = self.rss.apply_fetch(outcome).await;
        if plans.is_empty() {
            return;
        }
        let notified = self.create_rss_tasks(plans).await;
        if !notified.is_empty() {
            // 条目状态刚被改写为「已下载」，重播一次条目流让 UI 立即反映，
            // 并把合批通知标题挂在同一条事件上（宿主弹一条通知，不是 N 条）。
            self.rss.broadcast_items(&source_id, notified).await;
        }
        // 不论是否通知都重读一次未读计数：无变化的后续拉取只推内存镜像，
        // 这里不刷新的话 badge 会一直计入已转为下载的条目。
        self.rss.broadcast_sources().await;
    }

    /// `.torrent` 字节到手 → 建**真正的 BT 任务**（见
    /// [`crate::rss::RssDownloadPlan::is_torrent_file`]）。
    ///
    /// 抓取失败时**不**退化成「把 .torrent 当普通文件下下来」——那正是要修的
    /// 老毛病；条目留在 `New` 并进入指数退避，到期后自动重试（临时网络抖动自愈）。
    async fn on_rss_torrent_ready(&mut self, outcome: crate::rss::RssTorrentOutcome) {
        let plan = *outcome.plan;
        if !outcome.error.is_empty() {
            log_info!(
                "[rss] torrent fetch failed, item backs off before the next auto attempt: {} ({})",
                plan.title,
                outcome.error
            );
            // 记录失败码与失败次数而非静默：条目保持 New，指数退避期内不参与自动派发，
            // 到期后排在没失败过的条目之后，失效种子不会长期占住 max_per_fetch 的名额。
            self.rss
                .record_torrent_failure(&plan.source_id, &plan.guid)
                .await;
            return;
        }
        let spec = NewTaskSpec {
            // BT 任务的 url 由 create_task 换成 `torrent-file://local` 哨兵，
            // 真正的内容走 torrent_file_bytes 持久化。
            url: plan.url.clone(),
            save_dir: self.resolve_rss_save_dir(&plan.save_dir, &plan.queue_id),
            torrent_file_bytes: outcome.bytes,
            proxy_url: plan.proxy_url.clone(),
            user_agent: plan.user_agent.clone(),
            queue_id: plan.queue_id.clone(),
            start_paused: plan.start_paused,
            unattended_selection: true,
            ..Default::default()
        };
        let notify = plan.notify;
        let title = plan.title.clone();
        let source_id = plan.source_id.clone();
        if self.finish_rss_task(&plan, spec).await.is_none() {
            return;
        }
        self.rss
            .broadcast_items(&source_id, if notify { vec![title] } else { Vec::new() })
            .await;
        self.rss.broadcast_sources().await;
    }

    /// 手动下载一个 RSS 条目（「仍要下载」/「补下」，绕过规则与剧集去重）。
    /// 手动下载不受种子抓取退避限制，并清零该条目累积的失败计数。
    pub async fn download_rss_item(&mut self, source_id: &str, guid: &str) {
        let Some(plan) = self.rss.manual_download(source_id, guid).await else {
            return;
        };
        self.rss.clear_item_backoff(source_id, guid).await;
        self.create_rss_tasks(vec![*plan]).await;
        self.rss.broadcast_items(source_id, Vec::new()).await;
        self.rss.broadcast_sources().await;
    }

    /// 按建任务指令批量创建任务，返回需要进通知的条目标题。
    ///
    /// `.torrent` 条目在这里**不直接建任务**：先 off-actor 抓种子字节，等
    /// `TorrentReady` 回流再建，因此不计入本次返回的通知标题（通知随第二段
    /// 单独发出）。
    async fn create_rss_tasks(&mut self, plans: Vec<crate::rss::RssDownloadPlan>) -> Vec<String> {
        let mut notify_titles = Vec::new();
        let (proxy, ua) = (self.proxy_config.clone(), self.global_user_agent.clone());
        for plan in plans {
            if plan.is_torrent_file() {
                self.rss.spawn_torrent_fetch(Box::new(plan), &proxy, &ua);
                continue;
            }
            let spec = NewTaskSpec {
                url: plan.url.clone(),
                save_dir: self.resolve_rss_save_dir(&plan.save_dir, &plan.queue_id),
                cookies: plan.cookies.clone(),
                referrer: plan.referrer.clone(),
                resolver_item: plan.resolver_item.clone(),
                proxy_url: plan.proxy_url.clone(),
                user_agent: plan.user_agent.clone(),
                queue_id: plan.queue_id.clone(),
                start_paused: plan.start_paused,
                unattended_selection: true,
                ..Default::default()
            };
            let notify = plan.notify;
            let title = plan.title.clone();
            if self.finish_rss_task(&plan, spec).await.is_some() && notify {
                notify_titles.push(title);
            }
        }
        notify_titles
    }

    /// 建任务 + 打溯源指针 + 回写条目状态（两条建任务路径的共同收尾）。
    async fn finish_rss_task(
        &mut self,
        plan: &crate::rss::RssDownloadPlan,
        spec: NewTaskSpec,
    ) -> Option<String> {
        let task_id = self.create_task(spec).await?;
        if let Err(e) = self.db.set_task_rss_source(&task_id, &plan.source_id).await {
            log_info!("[rss] set_task_rss_source error: {}", e);
        }
        self.rss
            .mark_downloaded(&plan.source_id, &plan.guid, &task_id)
            .await;
        // 补一次全量任务快照：`create_task` 只发单条 `TaskProgress`，而该事件
        // **不带 queue_id**——客户端「按进度新建任务」只能拿到一个 queue_id 为
        // 空的条目，于是新任务不属于任何队列、队列视图里根本看不见，得手动
        // 停/启队列触发全量刷新才归位。客户端自己发起的创建不受影响（它本来
        // 就知道队列），RSS 是引擎自发的，必须由引擎把归属补上。
        self.send_tasks_snapshot().await;
        Some(task_id)
    }

    /// 保存目录兜底链：订阅目录 → 队列默认目录 → 全局默认目录（§3.1）。
    fn resolve_rss_save_dir(&self, source_dir: &str, queue_id: &str) -> String {
        if !source_dir.is_empty() {
            return source_dir.to_string();
        }
        let queue_dir = self
            .queues
            .get(if queue_id.is_empty() {
                MAIN_QUEUE_ID
            } else {
                queue_id
            })
            .map(|q| q.default_save_dir.as_str())
            .unwrap_or("");
        if queue_dir.is_empty() {
            self.default_save_dir.clone()
        } else {
            queue_dir.to_string()
        }
    }

    /// 全局恢复：恢复所有「运行中队列」内的 paused 任务；停止队列内的任务
    /// 不动（由「启动队列」显式恢复）。对齐 qBittorrent Start all，另外恢复
    /// 用户手动停止做种的已完成 BT 任务（见 [`Self::resume_user_stopped_seeds`]）。
    /// 返回尝试恢复的任务数。
    pub async fn resume_all_eligible(&mut self) -> usize {
        let resumed = match self.db.eligible_resume_task_ids().await {
            Ok(ids) => {
                let n = ids.len();
                self.batch_resume(&ids).await;
                n
            }
            Err(e) => {
                log_info!("[manager] resume_all_eligible error: {}", e);
                0
            }
        };
        #[cfg(feature = "bt")]
        let resumed = resumed + self.resume_user_stopped_seeds().await;
        resumed
    }

    /// 「全部开始」的做种部分：只恢复 `UserStopped` 且文件仍在的已完成任务。
    /// 达分享率 / 时长 / 不活跃限制而停止的不纳入——恢复后下一次求值 tick
    /// 会立即再停，结果与不恢复等价，只多一轮状态抖动。全局「完成后自动做种」
    /// 关闭时（完成即落 UserStopped）整段跳过，免得全部开始把历史任务全拉起做种；
    /// BT 全局禁用时同样跳过（单任务恢复由 daemon 回报 BT 已禁用）。
    #[cfg(feature = "bt")]
    async fn resume_user_stopped_seeds(&mut self) -> usize {
        if !self.bt_config.enabled {
            return 0;
        }
        let seed_enabled = match self.db.get_config("bt_seed_enabled").await {
            Ok(value) => value.is_none_or(|v| v != "0"),
            Err(error) => {
                crate::logger::report_warning(
                    "download-manager",
                    "read seeding setting for resume all",
                    &error,
                );
                return 0;
            }
        };
        if !seed_enabled {
            return 0;
        }
        let stopped = match self
            .db
            .load_tasks_with_seeding_status(SeedingStopReason::UserStopped.as_i32())
            .await
        {
            Ok(tasks) => tasks,
            Err(error) => {
                crate::logger::report_error(
                    "download-manager",
                    "load user-stopped seeds for resume all",
                    &error,
                );
                return 0;
            }
        };
        let mut resumed = 0;
        for task in stopped
            .into_iter()
            .filter(|task| task.status == 3 && !task.file_missing)
        {
            if self.try_resume_seeding(&task.task_id, &task).await {
                resumed += 1;
            }
        }
        resumed
    }
}

/// EMA smoothing factor.  α = 0.4 gives a good balance between
/// responsiveness and smoothness when combined with the 1-second fixed
/// sampling window below.  With one sample per second the speed converges
/// to ~90 % of a step change within 3–4 samples.
const EMA_ALPHA: f64 = 0.4;

/// Fixed speed sampling window (ms).  Instead of computing instant speed on
/// every incoming `ProgressUpdate` (which can arrive every few ms when
/// multiple segment workers interleave), we accumulate downloaded bytes and
/// compute `delta_bytes / delta_time` only once per window.  This eliminates
/// the noise caused by uneven update spacing in multi-segment downloads.
const SPEED_SAMPLE_INTERVAL_MS: u128 = 1_000;

/// 种子采样窗（ms）：尚无任何速度估计（任务刚起步/刚恢复，ema==0）时的
/// 短窗，让第一个速度值在首字节后数百毫秒内出现（IDM/aria2 式即时反馈）；
/// 得到首个估计后切回 [`SPEED_SAMPLE_INTERVAL_MS`] 稳态窗口。300ms 至少
/// 覆盖一个 coordinator 上报周期（200ms），种子样本仍是窗口均值而非单点
/// 瞬时值。
const SPEED_SEED_INTERVAL_MS: u128 = 300;

/// Decay factor applied to EMA when no new bytes arrive during a full
/// sampling window.  0.5 per window means speed halves every second during
/// a stall, reaching <1 KB/s in ~10 windows (~10 s) for a 1 MB/s baseline.
const SPEED_DECAY_FACTOR: f64 = 0.5;

/// Minimum interval between forwarding progress to the host (per task) to avoid
/// flooding the signal channel when many segments report simultaneously.
const MIN_DART_INTERVAL_MS: u128 = 500;

/// 供 BT runtime 上的做种恢复任务发进度事件（读最新行，status 恒 3）。
#[cfg(feature = "bt")]
async fn emit_seeding_progress(
    db: &Db,
    sink: &Arc<dyn EventSink>,
    task_id: &str,
    seeding_status: i32,
    seeding_message: &str,
) {
    // 读库 + 发出同样要与在途整表快照互斥，见 `Db::lock_event_order`。
    let _order = db.lock_event_order().await;
    match db.load_task_by_id(task_id).await {
        Ok(Some(t)) => {
            sink.emit(EngineEvent::TaskProgress {
                task_id: task_id.to_string(),
                status: 3,
                downloaded_bytes: t.downloaded_bytes,
                total_bytes: t.total_bytes,
                speed: 0,
                file_name: t.file_name.clone(),
                save_dir: t.save_dir.clone(),
                url: t.url.clone(),
                error_message: String::new(),
                upload_speed_bps: 0,
                uploaded_bytes: t.uploaded_bytes,
                seeding_status,
                seeding_message: seeding_message.to_string(),
                seeding_time_secs: t.seeding_time_secs,
            });
        }
        Ok(None) => {}
        Err(error) => {
            let span = tracing::error_span!("seeding_progress", task_id);
            let _guard = span.enter();
            crate::logger::report_error("download-manager", "load seeding progress", &error);
        }
    }
}

pub async fn progress_reporter(
    mut rx: mpsc::Receiver<ProgressUpdate>,
    db: Db,
    sink: Arc<dyn EventSink>,
) {
    let mut states: HashMap<String, TaskSpeedState> = HashMap::new();
    // Track last time we forwarded progress to the host per task (rate limiting).
    let mut last_dart_send: HashMap<String, std::time::Instant> = HashMap::new();
    // Track last DB persistence per task (independent of host updates).
    let mut last_db_save: HashMap<String, std::time::Instant> = HashMap::new();
    // BT 数据完成通知去重:每个 task_id 只发一次 `EngineEvent::BtDataFinished`
    // (完成搬移失败后的重试路径可能再次进入 finished 分支)。
    let mut bt_finish_notified: HashSet<String> = HashSet::new();

    while let Some(update) = rx.recv().await {
        if !update.deleted_tasks.is_empty() {
            let deleted = update.deleted_tasks;
            for task_id in &deleted {
                states.remove(task_id);
                last_dart_send.remove(task_id);
                last_db_save.remove(task_id);
                bt_finish_notified.remove(task_id);
            }
            crate::speed_history::forget(&db, &deleted).await;
            // 与终态帧同理：必须排在任何「删除前读库」的快照之后，否则快照会让已删任务复活。
            let _order = db.lock_event_order().await;
            sink.emit(EngineEvent::TasksDeleted(deleted));
            continue;
        }
        let now = std::time::Instant::now();
        let previous_downloaded_bytes = update
            .download_baseline
            .and_then(|_| states.get(&update.task_id).map(|state| state.latest_bytes));

        // 测速计数器：下载器上报了传输层接收字节时用它，否则用落盘字节。
        let speed_counter = update.transfer_bytes.unwrap_or(update.downloaded_bytes);
        let speed_from_transfer = update.transfer_bytes.is_some();

        // Latch file_name: once we get a non-empty name, remember it.
        let state = states.entry(update.task_id.clone()).or_insert_with(|| {
            TaskSpeedState {
                ema_speed: 0.0,
                sample_bytes: speed_counter,
                sample_time: now,
                latest_speed_bytes: speed_counter,
                speed_from_transfer,
                last_transfer_bytes: None,
                unsent_transfer_bytes: 0,
                latest_bytes: update.downloaded_bytes,
                file_name: String::new(),
                cached_segments: None,
                cached_runtime: None,
                last_sent_status: -1, // never sent yet
                last_raw_status: update.status,
                awaiting_first_growth: update.status == 1,
                sent_nonzero_speed: false,
                logged_missing_segments: false,
                upload_bps: 0,
                last_uploaded_snapshot: 0,
                cumulative_uploaded: 0,
            }
        });

        // BT upload accounting: a newly created state has no baseline. Seed it
        // from the persisted DB value so pause/resume cycles (or reporter
        // restarts) don't briefly flash zero in the UI before the next delta.
        if state.cumulative_uploaded == 0
            && let Ok(db_total) = db.get_task_uploaded_bytes(&update.task_id).await
        {
            state.cumulative_uploaded = db_total;
        }

        if !update.file_name.is_empty() {
            state.file_name = update.file_name.clone();
        }

        // Always cache the latest segment snapshot, regardless of rate-limiting.
        if update.segment_details.is_some() {
            state.cached_segments = update.segment_details.clone();
        }
        if let Some(runtime) = &update.runtime {
            state.cached_runtime = Some(runtime.clone());
        }

        // 接收字节在同一次运行内单调；比上次小 = 新一轮运行的计数器（从 0 起）。
        // 未转发的部分攒着，随下一次转发的进度帧交给宿主统计流量。
        if let Some(transfer) = update.transfer_bytes {
            let fresh = match state.last_transfer_bytes {
                Some(previous) if transfer >= previous => transfer - previous,
                _ => transfer,
            };
            state.unsent_transfer_bytes = state.unsent_transfer_bytes.saturating_add(fresh.max(0));
            state.last_transfer_bytes = Some(transfer);
        }

        // -----------------------------------------------------------------
        // Fixed-window speed calculation
        //
        // Instead of computing instant speed on every incoming update
        // (which is noisy for multi-segment downloads where dt can be as
        // short as 5 ms due to interleaved worker reports), we accumulate
        // bytes and compute speed once per SPEED_SAMPLE_INTERVAL_MS.
        //
        // Resume / status-transition handling:
        // - Entering downloading (5/2 -> 1) may carry baseline jumps.
        // - Some sources send an initial status=1 with downloaded=0, then
        //   quickly jump to resumed bytes on the next update.
        // - 首个「有增长」的更新被吸收为测量基线（awaiting_first_growth），
        //   之后的增量才计入速度——跳变永远进不了速度计算。
        // -----------------------------------------------------------------
        let entered_downloading = update.status == 1 && state.last_raw_status != 1;
        if entered_downloading {
            state.ema_speed = 0.0;
            state.sample_bytes = speed_counter;
            state.sample_time = now;
            state.awaiting_first_growth = true;
            state.sent_nonzero_speed = false;
        }

        if update.status == 1 {
            // Non-monotonic check (e.g. server reset, re-probe); switching
            // between written and received counters is a new baseline too.
            if speed_counter < state.latest_speed_bytes
                || speed_from_transfer != state.speed_from_transfer
            {
                state.ema_speed = 0.0;
                state.sample_bytes = speed_counter;
                state.sample_time = now;
                state.awaiting_first_growth = true;
            }

            state.latest_speed_bytes = speed_counter;
            state.speed_from_transfer = speed_from_transfer;

            // 首增长基线吸收：进入下载态后第一个「有增长」的更新可能携带
            // resume 基线跳变，其 delta 不代表真实传输。把它仅用作测量基线
            // （代价：丢弃至多一条更新 ≈ 200ms 的真实增量），后续增量即纯
            // 传输字节。
            if state.awaiting_first_growth && speed_counter > state.sample_bytes {
                state.awaiting_first_growth = false;
                state.sample_bytes = speed_counter;
                state.sample_time = now;
            }

            // Only compute speed when the sampling window expires.
            // 尚无速度估计（ema==0）时用种子短窗尽快出第一个值，
            // 有估计后回到 1s 稳态窗 + EMA 平滑。
            let window_ms = if state.ema_speed == 0.0 {
                SPEED_SEED_INTERVAL_MS
            } else {
                SPEED_SAMPLE_INTERVAL_MS
            };
            let window_elapsed_ms = now.duration_since(state.sample_time).as_millis();
            if window_elapsed_ms >= window_ms {
                let dt = now.duration_since(state.sample_time).as_secs_f64();
                let delta = speed_counter - state.sample_bytes;

                if delta > 0 && dt > 0.01 {
                    let window_speed = delta as f64 / dt;
                    if state.ema_speed == 0.0 {
                        // First valid sample — adopt directly for instant feedback.
                        state.ema_speed = window_speed;
                    } else {
                        state.ema_speed =
                            EMA_ALPHA * window_speed + (1.0 - EMA_ALPHA) * state.ema_speed;
                    }
                } else {
                    // No new bytes in this window — connection may be stalling.
                    // Decay aggressively so the UI reflects actual throughput.
                    state.ema_speed *= SPEED_DECAY_FACTOR;
                    if state.ema_speed < 1024.0 {
                        state.ema_speed = 0.0;
                    }
                }

                // Advance sampling window baseline.
                state.sample_bytes = speed_counter;
                state.sample_time = now;
            }
            // Within the window: just accumulate bytes, no speed recalc.
        } else {
            // Non-downloading state: reset everything.
            state.ema_speed = 0.0;
            state.awaiting_first_growth = false;
            state.sent_nonzero_speed = false;
            state.sample_bytes = speed_counter;
            state.sample_time = now;
            state.latest_speed_bytes = speed_counter;
            state.speed_from_transfer = speed_from_transfer;
        }
        state.latest_bytes = update.downloaded_bytes;
        state.last_raw_status = update.status;
        state.upload_bps = update.upload_speed_bps;

        // -----------------------------------------------------------------
        // BT upload cumulative accounting (download + seeding phases).
        //
        // librqbit's `stats.live.snapshot.uploaded_bytes` is a per-session
        // counter that resets to zero whenever the torrent is paused and
        // resumed (or the whole BT session is rebuilt). To keep the UI's
        // "uploaded bytes / ratio" correct across these resets, we delta-
        // accumulate against the DB column `tasks.uploaded_bytes`.
        //
        // A task is treated as a BT uploader once it has ever reported a
        // non-zero upload snapshot or upload speed. On the first such
        // observation (or after a state reset on resume) we load the DB
        // baseline so we can continue from the previous cumulative value.
        // -----------------------------------------------------------------
        let mut cumulative_uploaded = update.uploaded_bytes;
        let is_bt_upload = update.uploaded_bytes > 0
            || update.upload_speed_bps > 0
            || state.last_uploaded_snapshot > 0;
        if is_bt_upload {
            let snapshot = update.uploaded_bytes;
            let delta = if snapshot >= state.last_uploaded_snapshot {
                snapshot - state.last_uploaded_snapshot
            } else {
                // Counter reset (pause/resume or session rebuild). Do not
                // subtract a negative delta; the new session's counter starts
                // from zero and will be accumulated going forward.
                0
            };
            state.last_uploaded_snapshot = snapshot;

            if delta > 0 {
                state.cumulative_uploaded =
                    match db.add_task_uploaded_bytes(&update.task_id, delta).await {
                        Ok(total) => total,
                        Err(e) => {
                            log_info!(
                                "[progress-reporter] add_task_uploaded_bytes error for {}: {}",
                                &update.task_id,
                                e
                            );
                            state.cumulative_uploaded.saturating_add(delta)
                        }
                    };
            }

            cumulative_uploaded = state.cumulative_uploaded;
        }

        // BT 数据下载完成标记:绕过节流立即上报(一次性事件,节流可能吞掉),
        // 按 task_id 去重。
        if update.bt_data_finished && bt_finish_notified.insert(update.task_id.clone()) {
            sink.emit(EngineEvent::BtDataFinished {
                task_id: update.task_id.clone(),
            });
        }

        let smoothed_speed = state.ema_speed as i64;
        // 任务速度曲线：下载态逐秒累计平滑速度（与界面显示同源），离开下载态落库。
        crate::speed_history::observe(
            &db,
            &update.task_id,
            update.status == 1,
            smoothed_speed,
            now,
        )
        .await;
        let resolved_name = state.file_name.clone();

        // For terminal states (completed / error / paused) always send immediately.
        // For downloading (status=1) and preparing (status=5), rate-limit to avoid flooding the host.
        // BT tasks that are actively seeding (status=3, seeding_status=1) are also
        // treated as live so the UI keeps receiving upload speed updates.
        let is_seeding = update.seeding_status == SEEDING_STATUS_ACTIVE;
        let is_terminal = update.status != 1 && update.status != 5 && !is_seeding;
        // Status transitions (e.g. preparing→downloading) must also be sent
        // immediately so the UI never skips an intermediate state.
        let is_status_change = update.status != state.last_sent_status;
        // 速度从 0 → 非零的首次转变（起步/恢复后的第一个速度估计）视同
        // 状态变更立即推送：不被 500ms 节流吞掉，速度与 ETA 即刻可见。
        let speed_now_visible =
            update.status == 1 && smoothed_speed > 0 && !state.sent_nonzero_speed;
        let should_send = is_terminal || is_status_change || speed_now_visible || {
            let last = last_dart_send.get(&update.task_id);
            last.is_none()
                || now.duration_since(*last.unwrap_or(&now)).as_millis() >= MIN_DART_INTERVAL_MS
        };

        // Always send if this update carries a newly resolved file_name.
        let has_new_name = !update.file_name.is_empty();

        if should_send || has_new_name || update.download_baseline.is_some() {
            // Terminal states (completed / error / paused) should report zero
            // speed so the UI doesn't show a stale EMA value. Active seeders keep
            // their upload speed so the list/detail panels remain accurate.
            let report_speed = if is_terminal { 0 } else { smoothed_speed };
            if report_speed > 0 {
                state.sent_nonzero_speed = true;
            }
            {
                // Terminal and authoritative baseline frames must follow any
                // snapshot whose DB read preceded this transfer boundary.
                let _order = if (update.status != 1 && update.status != 5)
                    || update.download_baseline.is_some()
                {
                    Some(db.lock_event_order().await)
                } else {
                    None
                };
                if let Some(downloaded_bytes) = update.download_baseline {
                    sink.emit(EngineEvent::TaskTransferBaseline {
                        task_id: update.task_id.clone(),
                        downloaded_bytes,
                        previous_downloaded_bytes,
                    });
                }
                if state.unsent_transfer_bytes > 0 {
                    sink.emit(EngineEvent::TaskTransferred {
                        task_id: update.task_id.clone(),
                        bytes: std::mem::take(&mut state.unsent_transfer_bytes),
                    });
                }
                sink.emit(EngineEvent::TaskProgress {
                    task_id: update.task_id.clone(),
                    status: update.status,
                    downloaded_bytes: update.downloaded_bytes,
                    total_bytes: update.total_bytes,
                    speed: report_speed,
                    file_name: resolved_name,
                    save_dir: String::new(),
                    url: String::new(),
                    error_message: update.error_message.clone(),
                    upload_speed_bps: if is_terminal { 0 } else { state.upload_bps },
                    uploaded_bytes: cumulative_uploaded,
                    seeding_status: update.seeding_status,
                    seeding_message: update.seeding_message.clone(),
                    seeding_time_secs: update.seeding_time_secs,
                });
            }
            if let Some(mut runtime) = state.cached_runtime.clone() {
                if is_terminal {
                    runtime.sampled_at_ms = chrono::Utc::now().timestamp_millis();
                    runtime.sample_sequence = crate::transfer_activity::next_sample_sequence();
                    if update.total_bytes > 0 {
                        runtime.total_bytes = update.total_bytes;
                    }
                    runtime.active_transfers = runtime.active_transfers.map(|_| 0);
                    for segment in &mut runtime.segments {
                        segment.active = segment.active.map(|_| false);
                        if update.status == 3 {
                            segment.downloaded_bytes = segment.end_byte - segment.start_byte + 1;
                        }
                    }
                }
                sink.emit(EngineEvent::TaskRuntimeChanged(runtime));
            }

            // Send segment-level progress for IDM-style visualization.
            // Use the cached snapshot (updated on every incoming update)
            // instead of the current update's segment_details, because
            // rate-limiting may cause the current update to lack details.
            if let Some(ref segs) = state.cached_segments {
                // When task is completed (status==3), fix up each segment's
                // downloaded_bytes to its full size so the detail panel
                // displays 100% even if the last segment update was stale
                // (e.g. download finished too fast for an intermediate update).
                let final_segs: Vec<SegmentDetail> = if update.status == 3 {
                    segs.iter()
                        .map(|s| {
                            let full_size = s.end_byte - s.start_byte + 1;
                            SegmentDetail {
                                index: s.index,
                                start_byte: s.start_byte,
                                end_byte: s.end_byte,
                                downloaded_bytes: full_size,
                            }
                        })
                        .collect()
                } else {
                    segs.iter()
                        .map(|s| SegmentDetail {
                            index: s.index,
                            start_byte: s.start_byte,
                            end_byte: s.end_byte,
                            downloaded_bytes: s.downloaded_bytes,
                        })
                        .collect()
                };

                // Routine per-send logging is intentionally omitted here:
                // this branch fires up to twice per second per task and the
                // resulting "sending SegmentProgress" lines carry no
                // diagnostic value while dominating the log volume.
                sink.emit(EngineEvent::SegmentProgress {
                    task_id: update.task_id.clone(),
                    total_bytes: update.total_bytes,
                    segment_count: segs.len() as i32,
                    segments: final_segs,
                });
                state.logged_missing_segments = false;
            } else if !state.logged_missing_segments {
                // Genuine anomaly (segment panel will stay empty), but it
                // repeats on every rate-limited send — log once per task
                // until segments appear again.
                log_info!(
                    "[seg-vis] NO cached segments for task {}, segment_details in update: {}",
                    update.task_id,
                    update.segment_details.is_some()
                );
                state.logged_missing_segments = true;
            }

            state.last_sent_status = update.status;
            last_dart_send.insert(update.task_id.clone(), now);
        }

        // Persist progress to DB periodically (per-task timer, matches
        // segment persistence interval for crash-recovery consistency).
        //
        // DB writes are fire-and-forget (spawned, not awaited) so they don't
        // block the progress consumption loop.  Under high throughput (many
        // HTTP segments + BT) the channel would back-pressure and stall BT
        // progress reporting if we awaited each DB write synchronously.
        if update.status == 1 {
            let task_last_save = last_db_save.entry(update.task_id.clone()).or_insert(now);
            if task_last_save.elapsed().as_secs() >= downloader::DB_SAVE_INTERVAL_SECS {
                let db_clone = db.clone();
                let tid = update.task_id.clone();
                let dl = update.downloaded_bytes;
                tokio::spawn(async move {
                    // F009：单调写入。fire-and-forget 的 status=1 进度写入与下方
                    // awaited 的 status=3 完成写入竞争同一把 DB Connection 锁，
                    // 落库顺序不确定。一个先发起、携带中途较小 downloaded_bytes
                    // 的后台写入可能在完成写入之后才抢到锁，把 100% 覆盖回中途值。
                    // 用 MAX 语义的单调写入彻底消除该顺序依赖（进度只前进不回退）。
                    if let Err(error) = db_clone.update_task_progress_monotonic(&tid, dl).await {
                        let span = tracing::error_span!("progress_persistence", task_id = %tid);
                        let _guard = span.enter();
                        crate::logger::report_error(
                            "download-manager",
                            "persist active progress",
                            &error,
                        );
                    }
                });
                *task_last_save = now;
            }
        }

        // When a task completes, persist final downloaded_bytes *and*
        // total_bytes to DB so that subsequent app restarts load correct
        // 100% progress.  For unknown-size downloads the total_bytes was 0
        // during transfer but gets resolved to the actual file size upon
        // completion — we must persist that final value too.
        // Completion writes are awaited (not fire-and-forget) to guarantee
        // the final values are persisted before we clean up state.
        if update.status == 3 {
            if update.downloaded_bytes > 0 {
                // F009：同样走单调写入。完成写入是该任务进度的最终权威值
                // （= 文件总大小），用 MAX 语义后，任何在其之后才落库的陈旧
                // status=1 后台写入（携带更小的中途值）都会被钳制为 no-op，
                // 不会把已显示的 100% 覆盖回中途进度。
                if let Err(error) = db
                    .update_task_progress_monotonic(&update.task_id, update.downloaded_bytes)
                    .await
                {
                    let span = tracing::error_span!(
                        "progress_persistence",
                        task_id = %update.task_id
                    );
                    let _guard = span.enter();
                    crate::logger::report_error(
                        "download-manager",
                        "persist completed progress",
                        &error,
                    );
                }
            }
            // Use total_bytes when available; fall back to downloaded_bytes
            // for unknown-size downloads where total_bytes may still be 0.
            let final_total = if update.total_bytes > 0 {
                update.total_bytes
            } else {
                update.downloaded_bytes
            };
            if final_total > 0
                && let Err(error) = db
                    .update_task_total_bytes(&update.task_id, final_total)
                    .await
            {
                let span = tracing::error_span!(
                    "progress_persistence",
                    task_id = %update.task_id
                );
                let _guard = span.enter();
                crate::logger::report_error(
                    "download-manager",
                    "persist completed total bytes",
                    &error,
                );
            }
        }

        // Clean up tasks that are no longer actively downloading.
        // Status 2 (paused): speed state is stale; a fresh one will be
        //   created via `or_insert_with` when the task resumes.
        // Status 3 (completed) / 4 (error/cancelled/deleted): terminal.
        // 做种期的实时上传统计不经 reporter（由 manager 直接 sink.emit），
        // 完成帧之后不会再有本任务的 ProgressUpdate，保留状态只会泄漏。
        let should_remove_state = update.status == 2 || update.status == 3 || update.status == 4;
        if should_remove_state {
            states.remove(&update.task_id);
            last_dart_send.remove(&update.task_id);
            last_db_save.remove(&update.task_id);
        }
    }
}

#[cfg(test)]
mod file_exists_tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::file_exists::OverwritePolicy;

    #[cfg(feature = "bt")]
    #[test]
    fn webhook_provisional_file_name_follows_source_protocol() {
        assert_eq!(
            webhook_provisional_file_name("https://cdn.example.com/a/My%20File.iso?token=x"),
            "My File.iso"
        );
        assert_eq!(
            webhook_provisional_file_name("ftp://mirror.example.com/pub/f.tar.gz"),
            "f.tar.gz"
        );
        assert_eq!(
            webhook_provisional_file_name("magnet:?xt=urn:btih:abc&dn=Ubuntu%2024.04"),
            "Ubuntu 24.04"
        );
        // 无 dn 的磁力 / 本地哨兵给不出可靠名字，宁可留空也不编造。
        assert_eq!(webhook_provisional_file_name("magnet:?xt=urn:btih:abc"), "");
        assert_eq!(webhook_provisional_file_name("torrent-file://local"), "");
    }

    // -----------------------------------------------------------------------
    // dedup_filename_sync — allow_overwrite（config `file_exists_behavior`
    // == "overwrite"）:仅磁盘最终文件存在时保留原名;temp / reserved 命中
    // 仍照旧编号改名(与 `downloader::dedup_filename` 语义严格对称)。
    // -----------------------------------------------------------------------

    fn unique_dedup_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown_test_ddsync_{}_{}",
            tag,
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn dedup_filename_sync_overwrite_keeps_name_when_only_final_exists() {
        let dir = unique_dedup_dir("final");
        std::fs::create_dir_all(&dir).expect("prepare or clean test files");
        std::fs::write(dir.join("test.txt"), b"old").expect("prepare or clean test files");

        let result = dedup_filename_sync(
            &dir,
            "test.txt",
            &HashSet::new(),
            &OverwritePolicy::Any,
            false,
        );
        assert_eq!(
            result, "test.txt",
            "overwrite 模式下仅最终文件存在必须保留原名（finalize 时覆盖）"
        );
        // rename 模式(默认)对同一状态照旧编号改名。
        let result = dedup_filename_sync(
            &dir,
            "test.txt",
            &HashSet::new(),
            &OverwritePolicy::Never,
            false,
        );
        assert_eq!(result, "test (1).txt");
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    #[test]
    fn dedup_filename_sync_overwrite_temp_file_still_conflicts() {
        let dir = unique_dedup_dir("temp");
        std::fs::create_dir_all(&dir).expect("prepare or clean test files");
        // 在途下载的临时文件是硬冲突——绝不覆盖其他任务的在途产物。
        std::fs::write(
            dir.join(format!("test.txt{}", downloader::TEMP_EXT)),
            b"partial",
        )
        .expect("prepare or clean test files");

        let result = dedup_filename_sync(
            &dir,
            "test.txt",
            &HashSet::new(),
            &OverwritePolicy::Any,
            false,
        );
        assert_eq!(result, "test (1).txt");
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    #[test]
    fn dedup_filename_sync_overwrite_reserved_hit_still_conflicts() {
        let dir = unique_dedup_dir("reserved");
        std::fs::create_dir_all(&dir).expect("prepare or clean test files");
        // 磁盘干净,但兄弟任务已预订同名 temp 路径。
        let mut reserved = HashSet::new();
        reserved.insert(dir.join(format!("video.mp4{}", downloader::TEMP_EXT)));

        let result =
            dedup_filename_sync(&dir, "video.mp4", &reserved, &OverwritePolicy::Any, false);
        assert_eq!(result, "video (1).mp4");

        // 同名目录也不覆盖(文件不能盖到目录上)。
        std::fs::create_dir_all(dir.join("data.bin")).expect("prepare or clean test files");
        let result = dedup_filename_sync(
            &dir,
            "data.bin",
            &HashSet::new(),
            &OverwritePolicy::Any,
            false,
        );
        assert_eq!(result, "data (1).bin");
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    #[test]
    fn file_exists_behavior_from_config_str_parses_known_values_and_falls_back_to_rename() {
        assert_eq!(
            FileExistsBehavior::from_config_str("overwrite"),
            FileExistsBehavior::Overwrite
        );
        assert_eq!(
            FileExistsBehavior::from_config_str("skip"),
            FileExistsBehavior::Skip
        );
        assert_eq!(
            FileExistsBehavior::from_config_str("rename"),
            FileExistsBehavior::Rename
        );
        // 未识别 / 缺省值一律回退到 Rename（现状默认行为）。
        assert_eq!(
            FileExistsBehavior::from_config_str(""),
            FileExistsBehavior::Rename
        );
        assert_eq!(
            FileExistsBehavior::from_config_str("bogus"),
            FileExistsBehavior::Rename
        );
    }

    /// resolver 插件 resolve 结果 → QueuedTask 的应用语义（hint / Range 担保）。
    #[cfg(feature = "plugins")]
    mod resolve_apply {
        use super::*;
        use crate::plugin::ResolveResult;

        fn queued() -> QueuedTask {
            QueuedTask {
                task_id: "t1".into(),
                url: "https://example.com/source".into(),
                save_dir: "/tmp".into(),
                file_name: String::new(),
                segments: 0,
                is_resume: false,
                cookies: String::new(),
                referrer: String::new(),
                hint_file_size: 0,
                torrent_file_bytes: Vec::new(),
                proxy_url: String::new(),
                user_agent: String::new(),
                queue_id: String::new(),
                checksum: String::new(),
                ignore_tls_errors: false,
                extra_headers: std::collections::HashMap::new(),
                selected_file_indices: Vec::new(),
                method: None,
                body: None,
                audio_url: None,
                resolver_plugin_id: "yt@flux".into(),
                resolved: false,
                range_supported: false,
                resolver_item: String::new(),
                source_url: String::new(),
                resolver_patch: None,
            }
        }

        /// ephemeral + 已知大小 → hint = totalBytes（跳过 probe，大小可信）。
        #[test]
        fn ephemeral_with_size_uses_total_as_hint() {
            let mut q = queued();
            apply_resolve_to_queued(
                &mut q,
                ResolveResult {
                    url: "https://cdn.example.com/direct".into(),
                    total_bytes: Some(42_000_000),
                    ephemeral: true,
                    ..Default::default()
                },
            );
            assert_eq!(q.hint_file_size, 42_000_000);
            assert_eq!(q.url, "https://cdn.example.com/direct");
            assert!(!q.range_supported);
        }

        /// 回归：ephemeral 但大小未知 → hint = -1（跳过 probe、大小未知）。
        /// 旧实现落 0 会照常 probe，把一次性签名直链打废。
        #[test]
        fn ephemeral_without_size_skips_probe_with_minus_one() {
            let mut q = queued();
            apply_resolve_to_queued(
                &mut q,
                ResolveResult {
                    url: "https://cdn.example.com/direct".into(),
                    total_bytes: None,
                    ephemeral: true,
                    ..Default::default()
                },
            );
            assert_eq!(q.hint_file_size, -1);
        }

        /// 非 ephemeral → hint 归零（正常 probe 取 ETag，保 resume 一致性），
        /// 即使浏览器扩展曾给过 hint。
        #[test]
        fn non_ephemeral_resets_hint_for_probe() {
            let mut q = queued();
            q.hint_file_size = 123;
            apply_resolve_to_queued(
                &mut q,
                ResolveResult {
                    url: "https://cdn.example.com/direct".into(),
                    ..Default::default()
                },
            );
            assert_eq!(q.hint_file_size, 0);
        }

        /// rangeSupported 担保透传：与 ephemeral hint 组合时，do_start_task 据此
        /// 传 range_verified=true → 跳 probe 且按已验证 Range 多段规划。
        #[test]
        fn range_supported_flag_is_plumbed() {
            let mut q = queued();
            apply_resolve_to_queued(
                &mut q,
                ResolveResult {
                    url: "https://cdn.example.com/direct".into(),
                    total_bytes: Some(1_000_000_000),
                    ephemeral: true,
                    range_supported: true,
                    ..Default::default()
                },
            );
            assert!(q.range_supported);
            assert_eq!(q.hint_file_size, 1_000_000_000);
        }
    }
    /// 多变体收敛语义（选择/回退/字段覆盖）。用 NoopSelection（headless）驱动，
    /// 覆盖「无选择器 → default_variant_index 回退」与字段覆盖规则。
    #[cfg(feature = "plugins")]
    mod variant_collapse {
        use super::*;
        use crate::NoopSelection;
        use crate::plugin::{ResolveResult, ResolveVariant};

        fn variant(label: &str, url: &str) -> ResolveVariant {
            ResolveVariant {
                label: label.into(),
                url: url.into(),
                ..Default::default()
            }
        }

        /// headless（NoopSelection）→ 回退 default_variant_index，选中变体
        /// 覆盖顶层 url，variants 清空。
        #[tokio::test]
        async fn headless_falls_back_to_default_index() {
            let mut res = ResolveResult {
                variants: vec![
                    variant("1080p", "https://v.example.com/hi"),
                    variant("720p", "https://v.example.com/mid"),
                ],
                default_variant_index: 1,
                ..Default::default()
            };
            collapse_resolve_variants("t1", &mut res, &NoopSelection).await;
            assert_eq!(res.url, "https://v.example.com/mid");
            assert!(res.variants.is_empty());
        }

        /// default_variant_index 越界 → 按 0 处理。
        #[tokio::test]
        async fn out_of_range_default_clamps_to_zero() {
            let mut res = ResolveResult {
                variants: vec![variant("only", "https://v.example.com/a")],
                default_variant_index: 9,
                ..Default::default()
            };
            collapse_resolve_variants("t1", &mut res, &NoopSelection).await;
            assert_eq!(res.url, "https://v.example.com/a");
        }

        /// 选中变体的 Some 字段覆盖顶层；None 字段保留顶层原值。
        #[tokio::test]
        async fn variant_fields_override_only_when_present() {
            let mut res = ResolveResult {
                url: "https://old.example.com".into(),
                file_name: Some("base.mp4".into()),
                total_bytes: Some(1),
                variants: vec![ResolveVariant {
                    label: "audio".into(),
                    url: "https://v.example.com/audio".into(),
                    audio_url: None,
                    file_name: None,
                    total_bytes: Some(2_000),
                    ..Default::default()
                }],
                ..Default::default()
            };
            collapse_resolve_variants("t1", &mut res, &NoopSelection).await;
            assert_eq!(res.url, "https://v.example.com/audio");
            assert_eq!(res.file_name.as_deref(), Some("base.mp4"));
            assert_eq!(res.total_bytes, Some(2_000));
        }

        /// 用户点关闭/取消（选择器返回 -1）→ collapse 返回 true，且不收敛
        /// variants（顶层 url 不被改写，交由 actor 取消任务）。
        #[tokio::test]
        async fn user_cancel_returns_true_without_collapsing() {
            use crate::selection::{HostSelection, SelectionOutcome};
            struct CancelSelection;
            #[async_trait::async_trait]
            impl HostSelection for CancelSelection {
                async fn select_hls_quality(
                    &self,
                    _: &str,
                    _: &[crate::model::HlsQualityOption],
                    _: std::time::Duration,
                ) -> SelectionOutcome<i32> {
                    SelectionOutcome::UserChose(0)
                }
                async fn select_bt_files(
                    &self,
                    _: &str,
                    _: &[crate::model::BtFileEntry],
                    _: Option<std::time::Duration>,
                ) -> SelectionOutcome<Vec<i32>> {
                    SelectionOutcome::UserChose(vec![])
                }
                async fn select_resolve_variant(
                    &self,
                    _: &str,
                    _: &[crate::model::ResolveVariantOption],
                    _: i32,
                    _: std::time::Duration,
                ) -> SelectionOutcome<i32> {
                    SelectionOutcome::UserChose(-1)
                }
                fn provide_hls_selection(&self, _: &str, _: i32) {}
                fn provide_bt_selection(&self, _: &str, _: Vec<i32>) {}
                fn provide_variant_selection(&self, _: &str, _: i32) {}
            }
            let mut res = ResolveResult {
                url: "https://old.example.com".into(),
                variants: vec![
                    variant("1080p", "https://v.example.com/hi"),
                    variant("720p", "https://v.example.com/mid"),
                ],
                ..Default::default()
            };
            let cancelled = collapse_resolve_variants("t1", &mut res, &CancelSelection).await;
            assert!(cancelled);
            assert_eq!(res.url, "https://old.example.com");
        }
    }

    /// F042 回归：`is_safe_file_name` 必须拒绝所有会使
    /// `save_dir.join(name)` 退化为 `save_dir` 本身或逃逸出 `save_dir` 的输入。
    /// 尤其是 `"."`（CurDir），历史上被漏判为安全，可导致 BT 删除路径
    /// `remove_dir_all` 整个保存目录。
    #[test]
    fn is_safe_file_name_rejects_dangerous_names() {
        // 危险输入：必须返回 false。
        assert!(!is_safe_file_name(""), "empty string must be rejected");
        assert!(!is_safe_file_name("."), "CurDir must be rejected (F042)");
        assert!(!is_safe_file_name(".."), "ParentDir must be rejected");
        assert!(
            !is_safe_file_name("../escape.txt"),
            "leading parent traversal must be rejected"
        );
        assert!(
            !is_safe_file_name("foo/../bar"),
            "embedded parent traversal must be rejected"
        );
        assert!(
            !is_safe_file_name("./file.txt"),
            "leading CurDir must be rejected"
        );
        #[cfg(unix)]
        assert!(
            !is_safe_file_name("/etc/passwd"),
            "absolute path must be rejected"
        );
    }

    /// 合法的单段文件名（含中文、空格、点号扩展名）必须仍判为安全，
    /// 确保 F042 的收紧没有误伤正常下载文件名。
    #[test]
    fn is_safe_file_name_accepts_normal_names() {
        assert!(is_safe_file_name("movie.mp4"));
        assert!(is_safe_file_name("我的文件 (1).zip"));
        assert!(is_safe_file_name("archive.tar.gz"));
        assert!(is_safe_file_name("name_without_ext"));
        // BT 单顶层目录名（无分隔符）仍是合法的直接子项。
        assert!(is_safe_file_name("My Torrent Folder"));
    }

    #[test]
    fn split_relative_file_name_moves_subdirs_into_save_dir() {
        let base = std::path::Path::new("dl");
        let joined = |parts: &[&str]| {
            parts
                .iter()
                .fold(base.to_path_buf(), |dir, part| dir.join(part))
                .to_string_lossy()
                .into_owned()
        };
        // #716：aria2 `out=folder/file.zip` 落到 `dl/folder/file.zip`，不是 `folder_file.zip`。
        assert_eq!(
            split_relative_file_name("dl", "folder/sub\\file.zip"),
            (joined(&["folder", "sub"]), "file.zip".to_string())
        );
        // 穿越与根前缀不能逃出 save_dir。
        assert_eq!(
            split_relative_file_name("dl", "../../etc/passwd"),
            (joined(&["etc"]), "passwd".to_string())
        );
        assert_eq!(
            split_relative_file_name("dl", "C:\\x\\a.bin"),
            (joined(&["C_", "x"]), "a.bin".to_string())
        );
        // 无分隔符、或 save_dir 未定时原样返回。
        assert_eq!(
            split_relative_file_name("dl", "a b.zip"),
            ("dl".to_string(), "a b.zip".to_string())
        );
        assert_eq!(
            split_relative_file_name("", "x/a.zip"),
            (String::new(), "x/a.zip".to_string())
        );
    }

    /// F041 守卫前提：取消标记不能被 `is_retriable_error` 误判为可重试。
    /// 否则 `on_task_done` 会为取消任务自发 spawn 重试，绕过
    /// `is_task_in_error` 守卫。此测试锁定该不变量。
    #[test]
    fn cancelled_marker_is_not_retriable() {
        assert!(
            !is_retriable_error(CANCELLED_ERROR_MESSAGE),
            "cancelled tasks must never be treated as retriable network errors"
        );
    }

    #[test]
    fn auto_failover_tries_manual_system_and_direct_at_most_once() {
        use crate::auto_proxy::CandidateSource::{ManualFields, System};

        let sources = [ManualFields, System];
        let mut attempts = AutoFailoverAttempts::default();
        assert_eq!(
            auto_failover_target(
                "direct",
                "download stalled for 5s",
                &sources,
                true,
                &mut attempts,
            ),
            Some(AutoFailoverTarget::Proxy(ManualFields)),
            "直连中途断流应先尝试显式手动代理"
        );
        assert_eq!(
            auto_failover_target(
                "proxy:failover:manual",
                "error decoding response body",
                &sources,
                true,
                &mut attempts,
            ),
            Some(AutoFailoverTarget::Proxy(System)),
            "手动代理失败后应继续尝试系统代理"
        );
        assert_eq!(
            auto_failover_target(
                "proxy:failover:system",
                "operation timed out",
                &sources,
                true,
                &mut attempts,
            ),
            None,
            "三条已尝试链路不得再次循环"
        );

        let mut proxy_first = AutoFailoverAttempts::default();
        assert_eq!(
            auto_failover_target(
                "proxy:cached:system",
                "connection reset",
                &sources,
                true,
                &mut proxy_first,
            ),
            Some(AutoFailoverTarget::Proxy(ManualFields)),
            "系统代理先失败时应尝试尚未使用的手动代理"
        );
        assert_eq!(
            auto_failover_target(
                "proxy:failover:manual",
                "connection reset",
                &sources,
                true,
                &mut proxy_first,
            ),
            Some(AutoFailoverTarget::Direct),
            "两个代理均失败后应回退尚未尝试的本地直连"
        );

        let mut offline = AutoFailoverAttempts::default();
        assert_eq!(
            auto_failover_target(
                "proxy:cached:system",
                "operation timed out",
                &[System],
                false,
                &mut offline,
            ),
            None,
            "整机离线且无其他代理时不得误判系统代理失效"
        );
        let mut permanent = AutoFailoverAttempts::default();
        assert_eq!(
            auto_failover_target(
                "direct",
                "HTTP 404 Not Found",
                &sources,
                true,
                &mut permanent,
            ),
            None,
            "永久 HTTP 错误不得靠换路重试"
        );
        let mut no_proxy = AutoFailoverAttempts::default();
        assert_eq!(
            auto_failover_target("direct", "connection reset", &[], true, &mut no_proxy,),
            None,
            "没有候选代理且直连已失败时保持原错误"
        );
    }

    #[tokio::test]
    async fn auto_failover_runs_when_general_auto_retry_is_disabled() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.insert_task(
            "t-auto-failover",
            "https://example.com/release.bin",
            "release.bin",
            "",
            4,
            32 * 1024 * 1024,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        db.set_task_auto_route("t-auto-failover", crate::auto_proxy::route::DIRECT)
            .await
            .expect("set direct route");
        db.update_task_status("t-auto-failover", 4, "download stalled for 5s")
            .await
            .expect("set transport failure");

        let proxy_config = ProxyConfig {
            mode: ProxyMode::Auto,
            proxy_type: crate::proxy_config::ProxyType::Http,
            host: "127.0.0.1".to_string(),
            port: 7890,
            username: String::new(),
            password: String::new(),
            no_proxy_list: String::new(),
            rules: None,
        };
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: String::new(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config,
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        mgr.set_max_auto_retries(0);
        let mut retry_rx = mgr.take_retry_rx().expect("retry receiver");
        mgr.active_tasks.insert(
            "t-auto-failover".to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 7,
                handle: None,
                is_bt: false,
                queue_id: String::new(),
            },
        );

        mgr.on_task_done(&TaskDone {
            task_id: "t-auto-failover".to_string(),
            generation: 7,
            reserved_temp_path: None,
            phase: TaskDonePhase::Finished,
        })
        .await;

        let scheduled = tokio::time::timeout(std::time::Duration::from_secs(1), retry_rx.recv())
            .await
            .expect("failover should schedule immediately");
        assert_eq!(scheduled.as_deref(), Some("t-auto-failover"));
        assert_eq!(
            mgr.auto_failover_pending.get("t-auto-failover"),
            Some(&AutoFailoverTarget::Proxy(
                crate::auto_proxy::CandidateSource::ManualFields,
            ))
        );
        assert!(
            !mgr.auto_retry_counts.contains_key("t-auto-failover"),
            "备用链路不得消耗用户配置的通用自动重试配额"
        );
        let (selected_proxy, route, ctx) = mgr
            .auto_route_decision(
                "https://example.com/release.bin",
                "",
                false,
                Some(ForcedRoute::failover(AutoFailoverTarget::Proxy(
                    crate::auto_proxy::CandidateSource::ManualFields,
                ))),
            )
            .expect("auto mode should return a route decision");
        assert_eq!(selected_proxy.port, 7890);
        assert_eq!(route, crate::auto_proxy::route::PROXY_FAILOVER_MANUAL);
        let ctx = ctx.expect("forced route keeps a single-path context for prior learning");
        assert!(
            ctx.alternates.is_empty(),
            "forced failover must stay on the one path (no probe race, no multipath)"
        );
        assert_eq!(
            ctx.start_route,
            crate::auto_proxy::RoutePath::Proxy(crate::auto_proxy::CandidateSource::ManualFields)
        );
    }

    #[test]
    fn auto_takeoff_avoids_recently_failed_paths() {
        use crate::auto_proxy::{CandidateSource, PathCandidate, RoutePath};
        let path = |route, prior_bps| PathCandidate {
            route,
            config: ProxyConfig::default(),
            prior_bps,
        };
        let manual = RoutePath::Proxy(CandidateSource::ManualFields);
        let system = RoutePath::Proxy(CandidateSource::System);

        // 直连可用：代理先验须领先 1.5× 才以代理起飞。
        let paths = [
            path(RoutePath::Direct, Some(1e6)),
            path(manual, Some(1.4e6)),
            path(system, Some(1.6e6)),
        ];
        assert_eq!(pick_auto_takeoff(&paths, &[false; 3]), 2);
        let slow_proxies = [
            path(RoutePath::Direct, Some(1e6)),
            path(manual, Some(1.4e6)),
            path(system, None),
        ];
        assert_eq!(pick_auto_takeoff(&slow_proxies, &[false; 3]), 0);
        // 领先的代理近期走不通：不以它起飞。
        assert_eq!(pick_auto_takeoff(&paths, &[false, false, true]), 0);

        // 直连近期走不通（被墙）：无先验的代理也优先于再撞一次直连，同分取
        // 候选顺序靠前者；有先验的更快代理优先。
        let cold = [
            path(RoutePath::Direct, Some(5e6)),
            path(manual, None),
            path(system, None),
        ];
        assert_eq!(pick_auto_takeoff(&cold, &[true, false, false]), 1);
        assert_eq!(pick_auto_takeoff(&cold, &[true, true, false]), 2);
        let warm = [
            path(RoutePath::Direct, None),
            path(manual, Some(1e5)),
            path(system, Some(2e6)),
        ];
        assert_eq!(pick_auto_takeoff(&warm, &[true, false, false]), 2);

        // 全部走不通：失败记录无区分度，回到先验规则。
        assert_eq!(pick_auto_takeoff(&paths, &[true; 3]), 2);
        assert_eq!(pick_auto_takeoff(&cold, &[true; 3]), 0);
    }

    /// 构造一个刚以可重试错误失败、已排程自动重试的任务（#947 场景）。
    async fn manager_with_retry_pending(ids: &[&str]) -> (DownloadManager, mpsc::Receiver<String>) {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        for id in ids {
            db.insert_task(
                id,
                "https://example.com/a.bin",
                "a.bin",
                "",
                4,
                1024,
                "",
                "",
                "",
                0,
            )
            .await
            .expect("insert task");
            db.update_task_status(id, 4, "connection reset")
                .await
                .expect("set transport failure");
        }
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: String::new(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        mgr.set_max_auto_retries(3);
        mgr.set_auto_retry_delay_secs(0);
        let retry_rx = mgr.take_retry_rx().expect("retry receiver");
        for (generation, id) in (1u64..).zip(ids) {
            mgr.active_tasks.insert(
                id.to_string(),
                ActiveTaskEntry {
                    token: CancellationToken::new(),
                    generation,
                    handle: None,
                    is_bt: false,
                    queue_id: String::new(),
                },
            );
            mgr.on_task_done(&TaskDone {
                task_id: id.to_string(),
                generation,
                reserved_temp_path: None,
                phase: TaskDonePhase::Finished,
            })
            .await;
            assert!(
                mgr.retry_scheduled.contains_key(*id),
                "前置：{id} 已排程自动重试"
            );
        }
        (mgr, retry_rx)
    }

    #[tokio::test]
    async fn pausing_retry_pending_failed_task_stops_the_scheduled_retry() {
        let (mut mgr, mut retry_rx) = manager_with_retry_pending(&["t"]).await;
        mgr.pause_task("t").await;

        let row = mgr
            .db
            .load_task_by_id("t")
            .await
            .expect("load")
            .expect("row");
        assert_eq!(row.status, 2, "失败任务暂停后落为已暂停");
        assert!(mgr.retry_pending_task_ids().is_empty());
        // 定时器仍会到点：宿主的守卫必须拒绝重启。
        let fired = tokio::time::timeout(std::time::Duration::from_secs(1), retry_rx.recv())
            .await
            .expect("retry timer fires");
        assert_eq!(fired.as_deref(), Some("t"));
        assert!(
            !mgr.is_task_in_error("t").await,
            "到点的重试不得再拉起已暂停任务"
        );
    }

    #[tokio::test]
    async fn pause_all_covers_retry_pending_but_keeps_terminal_failures() {
        let (mut mgr, _retry_rx) = manager_with_retry_pending(&["waiting"]).await;
        mgr.db
            .insert_task(
                "done-failing",
                "https://example.com/b.bin",
                "b.bin",
                "",
                4,
                1,
                "",
                "",
                "",
                0,
            )
            .await
            .expect("insert terminal failure");
        mgr.db
            .update_task_status("done-failing", 4, "HTTP 404")
            .await
            .expect("set terminal failure");

        let mut ids = mgr.retry_pending_task_ids();
        ids.push("done-failing".to_string());
        mgr.batch_pause_all(&ids).await;

        let status = |row: Option<TaskInfo>| row.expect("row").status;
        assert_eq!(
            status(mgr.db.load_task_by_id("waiting").await.expect("load")),
            2
        );
        assert_eq!(
            status(mgr.db.load_task_by_id("done-failing").await.expect("load")),
            4,
            "全部暂停不改写不会再重试的失败任务"
        );

        // 用户显式选中则任何失败任务都可暂停。
        mgr.pause_selected(&["done-failing".to_string()]).await;
        assert_eq!(
            status(mgr.db.load_task_by_id("done-failing").await.expect("load")),
            2
        );
    }

    /// BUG-BT-PHANTOM-PIECES：完成前 piece 校验失败必须可自动重试——重试
    /// 路径会重新 add_torrent 并触发 librqbit 全量校验,只补齐损坏 piece。
    #[test]
    fn bt_piece_verification_failure_is_retriable() {
        assert!(is_retriable_error(
            "BT piece verification failed: 36 bad piece(s) — data will be re-checked and re-downloaded"
        ));
    }

    /// R2-1 回归：轨对 resume 时轨长探测失败（dash 的 fail-loud 保留段行
    /// 路径）必须可自动重试——重试会重新 resolve 拿新直链自愈；不命中
    /// 白名单则任务卡 error 态等人工恢复，违背该路径的设计注释。
    #[test]
    fn track_probe_failure_is_retriable() {
        assert!(is_retriable_error(
            "track probe failed with 4 resumable segment row(s) retained; \
             refusing single-stream fallback that would destroy resume state"
        ));
    }

    /// #379 回归：磁力元数据解析超时的错误消息不能命中
    /// `is_retriable_error` 关键词（如 "timeout"/"timed out"）。否则
    /// 死磁力会被自动重试，每轮再烧 5 分钟并在意外时机弹出文件选择框。
    #[test]
    fn magnet_metadata_timeout_error_is_not_retriable() {
        let msg = "magnet metadata resolution took too long (300s) — no peers/DHT response; check trackers or network";
        assert!(
            !is_retriable_error(msg),
            "magnet metadata timeout must not trigger auto-retry"
        );
    }

    /// #636 回归辅助：构造指定 HTTP 状态码的 reqwest 错误消息（与
    /// downloader.rs / segment_coordinator.rs 的同名辅助手法一致）：从合成
    /// 响应调用 `error_for_status()` 得到真实 reqwest::Error，再经
    /// `DownloadError::Request` 的 Display 得到落库的 `error_message` 原文。
    fn make_status_error_message(status: u16) -> String {
        let http_resp = ::reqwest::Response::from(
            ::http::Response::builder()
                .status(status)
                .body("")
                .unwrap_or_else(|_| panic!("failed to build http::Response with status {status}")),
        );
        let err = http_resp.error_for_status().unwrap_err();
        downloader::DownloadError::Request(err).to_string()
    }

    /// #636：5xx（含 503 Service Unavailable）在 reqwest 错误消息里必须被
    /// 识别为可重试——`error_message` 落库的正是 `DownloadError::Request`
    /// 的 Display，即 reqwest 固定措辞 `HTTP status server error (503 ...)`。
    #[test]
    fn http_503_error_message_is_retriable() {
        let msg = make_status_error_message(503);
        assert!(
            is_retriable_error(&msg),
            "503 message '{msg}' should be retriable"
        );
    }

    /// #636：404（资源永久不存在）与 403（服务端拒绝，另由
    /// `is_server_rejection` 以降速/换链路语义处理）必须仍不可重试，避免
    /// 对不存在资源或被拉黑的链接无限重试空转。
    #[test]
    fn http_404_and_403_error_messages_are_not_retriable() {
        let msg_404 = make_status_error_message(404);
        let msg_403 = make_status_error_message(403);
        assert!(
            !is_retriable_error(&msg_404),
            "404 message '{msg_404}' must not be retriable"
        );
        assert!(
            !is_retriable_error(&msg_403),
            "403 message '{msg_403}' must not be retriable"
        );
    }

    /// #636：既有关键词判定（如 "timed out"）与新增的状态码判定并存，互不
    /// 挤占——回归锁定关键词分支在状态码判定接入后仍生效。
    #[test]
    fn existing_keyword_case_still_retriable_after_status_wiring() {
        assert!(is_retriable_error("operation timed out after 30s"));
    }

    /// URL 里的 eof / timeout / stalled 字样不得改变判定：永久 HTTP 错误带完整 URL，
    /// 网络层错误的关键词仍要命中。
    #[test]
    fn url_substrings_do_not_affect_retriable_verdict() {
        assert!(!is_retriable_error(
            "request failed: HTTP status client error (404 Not Found) for url \
             (https://download.example.com/eof/timeout/stalled.bin)"
        ));
        assert!(!is_retriable_error(
            "request failed: HTTP status client error (403 Forbidden) for url \
             (https://cdn.example.com/connection-reset/file.zip)"
        ));
        assert!(is_retriable_error(
            "request failed: error sending request for url \
             (https://cdn.example.com/a.bin): connection reset by peer"
        ));
        assert!(is_retriable_error(
            "request failed: HTTP status server error (503 Service Unavailable) for url \
             (https://cdn.example.com/a.bin)"
        ));
    }

    // -------------------------------------------------------------------------
    // 文件跟踪（FluxDown #11）：task_target_path / probe_missing / scan_missing_files
    // -------------------------------------------------------------------------

    /// FluxDown #11：空名与路径穿越/绝对路径必须解析为 `None`——无法安全判定
    /// 存在性时跳过该任务，而不是把 `save_dir` 本身或盘外路径当成目标文件。
    #[test]
    fn task_target_path_rejects_unsafe_or_empty_names() {
        assert_eq!(
            task_target_path("save/dir", ""),
            None,
            "empty name must be rejected"
        );
        assert_eq!(
            task_target_path("save/dir", "."),
            None,
            "CurDir must be rejected"
        );
        assert_eq!(
            task_target_path("save/dir", ".."),
            None,
            "ParentDir must be rejected"
        );
        #[cfg(unix)]
        assert_eq!(
            task_target_path("save/dir", "/etc/passwd"),
            None,
            "absolute path must be rejected"
        );
        #[cfg(windows)]
        assert_eq!(
            task_target_path("C:\\save\\dir", "C:\\Windows\\System32"),
            None,
            "absolute path must be rejected"
        );
    }

    /// 正常文件名必须解析为 `save_dir` 下的直接子路径。
    #[test]
    fn task_target_path_joins_safe_name_onto_save_dir() {
        assert_eq!(
            task_target_path("save/dir", "movie.mp4"),
            Some(PathBuf::from("save/dir").join("movie.mp4"))
        );
    }

    /// 文件跟踪测试专用的唯一临时目录（防并行测试互相干扰，测后自行清理）。
    fn unique_filetrack_test_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown_filetrack_test_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[tokio::test]
    async fn probe_missing_reports_existing_file_as_present() {
        let dir = unique_filetrack_test_dir("probe_file");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let file = dir.join("movie.mp4");
        std::fs::write(&file, b"data").expect("write test file");

        assert_eq!(probe_missing(&file).await, Some(false));

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    #[tokio::test]
    async fn probe_missing_reports_deleted_file_as_missing() {
        let dir = unique_filetrack_test_dir("probe_deleted");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let file = dir.join("movie.mp4");
        std::fs::write(&file, b"data").expect("write test file");
        std::fs::remove_file(&file).expect("delete test file");

        assert_eq!(probe_missing(&file).await, Some(true));

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// BT 单顶层目录任务的目标路径是目录而非文件；目录存在也必须判定为
    /// "未丢失"。
    #[tokio::test]
    async fn probe_missing_treats_existing_directory_as_present() {
        let dir = unique_filetrack_test_dir("probe_dir");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let target = dir.join("Torrent Folder");
        std::fs::create_dir_all(&target).expect("create target dir");

        assert_eq!(probe_missing(&target).await, Some(false));

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// 文件跟踪 e2e 测试用的记录型 sink：原样收集每个 `emit` 的事件，供测试
    /// 断言 `scan_missing_files` 触发的 `FileMissingChanged` 的内容与次数。
    struct RecordingSink {
        events: std::sync::Mutex<Vec<EngineEvent>>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<EngineEvent> {
            self.events.lock().expect("sink mutex poisoned").clone()
        }
    }

    impl EventSink for RecordingSink {
        fn emit(&self, event: EngineEvent) {
            self.events.lock().expect("sink mutex poisoned").push(event);
        }
    }

    /// 文件跟踪测试用的空清理通道发送端：`auto_delete = false` 的用例不会
    /// 触碰它，只是为了填满 `scan_missing_files` 的形参。
    fn noop_cleanup_tx() -> mpsc::Sender<Vec<String>> {
        mpsc::channel(8).0
    }

    /// 插入一个任务并把状态推进到 `status`：`Db::insert_task` 固定以
    /// status=0 落库，文件跟踪测试需要 completed(3)/downloading(1) 等具体
    /// 状态。
    async fn insert_task_at_status(
        db: &Db,
        id: &str,
        save_dir: &str,
        file_name: &str,
        status: i32,
    ) {
        db.insert_task(
            id,
            "http://example.com/file.bin",
            file_name,
            save_dir,
            1,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        if status != 0 {
            db.update_task_status(id, status, "")
                .await
                .expect("advance task status");
        }
    }

    /// 已完成记录保留期契约：保留期 0 什么都不做；开启后只删超期的已完成记录，
    /// 文件原样保留，较新 / 未完成 / 无时间戳的任务不动；去抖窗口内被动触发不重复清理，
    /// `force`（设置变更 / 启动）绕过去抖。
    #[tokio::test]
    async fn purge_expired_completed_records_removes_only_old_records_and_keeps_files() {
        let dir = unique_filetrack_test_dir("retention_purge");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        let kept_file = dir.join("old.bin");
        std::fs::write(&kept_file, b"payload").expect("write file");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "old", &save_dir, "old.bin", 3).await;
        insert_task_at_status(&db, "fresh", &save_dir, "fresh.bin", 3).await;
        insert_task_at_status(&db, "paused", &save_dir, "paused.bin", 2).await;
        db.set_completed_at_for_test("old", "1000")
            .await
            .expect("backdate");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink,
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        // 保留期 0（永不）：什么都不删。
        assert_eq!(mgr.purge_expired_completed_records(true).await, 0);
        assert!(db.load_task_by_id("old").await.expect("load").is_some());

        mgr.set_completed_record_retention_days(7);
        assert_eq!(mgr.purge_expired_completed_records(false).await, 1);
        assert!(db.load_task_by_id("old").await.expect("load").is_none());
        assert!(db.load_task_by_id("fresh").await.expect("load").is_some());
        assert!(db.load_task_by_id("paused").await.expect("load").is_some());
        assert!(kept_file.exists(), "保留期清理只删记录，绝不删文件");

        // 去抖窗口内：被动触发不再查询；force 绕过。
        db.set_completed_at_for_test("fresh", "1000")
            .await
            .expect("backdate");
        assert_eq!(mgr.purge_expired_completed_records(false).await, 0);
        assert!(db.load_task_by_id("fresh").await.expect("load").is_some());
        assert_eq!(mgr.purge_expired_completed_records(true).await, 1);
        assert!(db.load_task_by_id("fresh").await.expect("load").is_none());

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_disabled_setting_rejects_task_creation_and_session() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let temp_dir = std::env::temp_dir();
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: temp_dir.to_string_lossy().into_owned(),
                app_data_dir: temp_dir.to_string_lossy().into_owned(),
                data_dir: temp_dir.clone(),
                bt_config: BtConfig {
                    enabled: false,
                    ..Default::default()
                },
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        // 1. ensure_bt_session must fail with clear message
        let err = mgr.ensure_bt_session().await.expect_err("must fail");
        assert!(
            err.to_string()
                .contains("BitTorrent is disabled in settings"),
            "expected disabled message, got {err:?}"
        );

        // 2. create_task for magnet URL must be rejected
        let magnet_spec = NewTaskSpec {
            url: "magnet:?xt=urn:btih:da39a3ee5e6b4b0d3255bfef95601890afd80709&dn=test".to_string(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        assert!(
            mgr.create_task(magnet_spec).await.is_none(),
            "magnet task creation must be rejected when BT is disabled"
        );

        // 3. create_task with raw torrent bytes must also be rejected
        let torrent_bytes_spec = NewTaskSpec {
            url: String::new(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            torrent_file_bytes: vec![1, 2, 3],
            ..Default::default()
        };
        assert!(
            mgr.create_task(torrent_bytes_spec).await.is_none(),
            "torrent file task creation must be rejected when BT is disabled"
        );

        // 4. HTTP task must still be accepted even when BT is disabled
        let http_spec = NewTaskSpec {
            url: "https://example.com/file.zip".to_string(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            start_paused: true,
            ..Default::default()
        };
        assert!(
            mgr.create_task(http_spec).await.is_some(),
            "HTTP task creation must proceed normally when BT is disabled"
        );

        // 5. Enabling BT allows session creation and task creation
        mgr.set_bt_config(BtConfig {
            enabled: true,
            ..Default::default()
        });
        let magnet_spec_enabled = NewTaskSpec {
            url: "magnet:?xt=urn:btih:da39a3ee5e6b4b0d3255bfef95601890afd80709&dn=test".to_string(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            start_paused: true,
            ..Default::default()
        };
        let bt_task_id = mgr
            .create_task(magnet_spec_enabled)
            .await
            .expect("magnet task creation must be accepted when BT is enabled");
        let initial_task = mgr
            .db
            .load_task_by_id(&bt_task_id)
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(initial_task.status, 2, "task is created paused");

        // 6. When BT is disabled again, resume_task must leave the task paused rather than failing it
        mgr.set_bt_config(BtConfig {
            enabled: false,
            ..Default::default()
        });

        mgr.resume_task(&bt_task_id).await;
        assert!(
            !mgr.active_tasks.contains_key(&bt_task_id),
            "BT task must not be spawned in active_tasks when BT is disabled"
        );
        let loaded = mgr
            .db
            .load_task_by_id(&bt_task_id)
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(
            loaded.status, 2,
            "BT task must remain paused (status 2) instead of mutating to failed (status 4)"
        );

        // 7. auto_resume_on_start must skip BT tasks when BT is disabled
        mgr.db
            .set_config("auto_resume_on_start", "true")
            .await
            .expect("set config");
        mgr.auto_resume_on_start(vec![bt_task_id.clone()]).await;
        assert!(
            !mgr.active_tasks.contains_key(&bt_task_id),
            "auto_resume_on_start must skip BT task when BT is disabled"
        );
    }

    /// 编译期没有 BT 后端（`--no-default-features`，如 iOS 商店构建）：即使配置显式
    /// `enabled: true`，magnet / 种子字节任务也必须被明确拒绝且不落库，会话初始化返回
    /// 同一通用文案；HTTP 任务不受影响。
    #[cfg(not(feature = "bt"))]
    #[tokio::test]
    async fn without_bt_backend_rejects_magnet_and_torrent_tasks_even_if_enabled() {
        const MAGNET: &str = "magnet:?xt=urn:btih:da39a3ee5e6b4b0d3255bfef95601890afd80709&dn=t";
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let temp_dir = std::env::temp_dir();
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: temp_dir.to_string_lossy().into_owned(),
                app_data_dir: temp_dir.to_string_lossy().into_owned(),
                data_dir: temp_dir.clone(),
                bt_config: BtConfig {
                    enabled: true,
                    ..Default::default()
                },
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        // 配置声称启用也不能让一个不存在的后端“生效”（构造与热更新两条入口）。
        assert!(!mgr.bt_config.enabled, "构造入口必须归一化为禁用");
        mgr.set_bt_config(BtConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(!mgr.bt_config.enabled, "热更新入口必须归一化为禁用");

        let err = mgr
            .ensure_bt_session()
            .await
            .expect_err("no BT backend: session init must fail");
        assert_eq!(err.to_string(), UNSUPPORTED_PROTOCOL_MESSAGE);

        assert!(mgr.rejects_new_bt_task(MAGNET, &[]));
        assert!(mgr.rejects_new_bt_task("", &[1, 2, 3]));
        assert!(!mgr.rejects_new_bt_task("https://example.com/f.zip", &[]));
        assert_eq!(
            CreateRejection::BtDisabled.to_string(),
            UNSUPPORTED_PROTOCOL_MESSAGE
        );

        let magnet = NewTaskSpec {
            url: MAGNET.to_string(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            start_paused: true,
            ..Default::default()
        };
        assert!(
            mgr.create_task(magnet).await.is_none(),
            "magnet 任务必须被拒绝"
        );
        let torrent_bytes = NewTaskSpec {
            url: String::new(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            torrent_file_bytes: vec![1, 2, 3],
            ..Default::default()
        };
        assert!(
            mgr.create_task(torrent_bytes).await.is_none(),
            "种子字节任务必须被拒绝"
        );
        assert!(
            mgr.db
                .load_all_tasks()
                .await
                .expect("load tasks")
                .is_empty(),
            "被拒绝的 BT 任务不得落库"
        );

        let http = NewTaskSpec {
            url: "https://example.com/file.zip".to_string(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            start_paused: true,
            ..Default::default()
        };
        assert!(
            mgr.create_task(http).await.is_some(),
            "HTTP 任务不受 BT 后端缺失影响"
        );
    }

    /// 编译期没有 eD2K 后端：ed2k:// 链接（含 thunder 封装）预检命中、建任务被明确
    /// 拒绝且不落库，其余协议不受影响。
    #[cfg(not(feature = "ed2k"))]
    #[tokio::test]
    async fn without_ed2k_backend_rejects_ed2k_tasks() {
        const ED2K: &str = "ed2k://|file|a.bin|1024|0123456789ABCDEF0123456789ABCDEF|/";
        assert!(rejects_unsupported_protocol(ED2K));
        assert!(rejects_unsupported_protocol("  ED2K://|file|a|1|0|/"));
        assert!(!rejects_unsupported_protocol("https://example.com/a.bin"));
        assert!(!rejects_unsupported_protocol(
            "magnet:?xt=urn:btih:da39a3ee5e6b4b0d3255bfef95601890afd80709"
        ));
        assert_eq!(
            CreateRejection::UnsupportedProtocol.to_string(),
            UNSUPPORTED_PROTOCOL_MESSAGE
        );

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let temp_dir = std::env::temp_dir();
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: temp_dir.to_string_lossy().into_owned(),
                app_data_dir: temp_dir.to_string_lossy().into_owned(),
                data_dir: temp_dir.clone(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        let ed2k = NewTaskSpec {
            url: ED2K.to_string(),
            save_dir: temp_dir.to_string_lossy().into_owned(),
            start_paused: true,
            ..Default::default()
        };
        assert!(mgr.create_task(ed2k).await.is_none(), "ed2k 任务必须被拒绝");
        assert!(
            mgr.db
                .load_all_tasks()
                .await
                .expect("load tasks")
                .is_empty(),
            "被拒绝的 ed2k 任务不得落库"
        );
    }

    /// BT 禁用后排队 / 恢复路径：BT 任务不得停在「等待/下载中」假象，也不得被置失败；
    /// 预检接口须能识别被拒的 BT 任务，供 daemon 向客户端回报原因。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_disabled_parks_queued_and_resumed_bt_tasks_and_reports_rejections() {
        const MAGNET: &str = "magnet:?xt=urn:btih:da39a3ee5e6b4b0d3255bfef95601890afd80709&dn=t";
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let temp_dir = std::env::temp_dir();
        let save_dir = temp_dir.to_string_lossy().into_owned();
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: save_dir.clone(),
                data_dir: temp_dir.clone(),
                bt_config: BtConfig {
                    enabled: false,
                    ..Default::default()
                },
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        for id in [
            "bt-queued",
            "bt-swept",
            "bt-stuck",
            "bt-paused",
            "bt-failed",
        ] {
            mgr.db
                .insert_task(id, MAGNET, "t", &save_dir, 1, 0, "", "", "", 0)
                .await
                .expect("insert BT task");
        }
        insert_task_at_status(&mgr.db, "http-paused", &save_dir, "f.bin", 2).await;
        mgr.db
            .update_task_status("bt-stuck", 1, "")
            .await
            .expect("mark stuck");
        mgr.db
            .update_task_status("bt-paused", 2, "")
            .await
            .expect("mark paused");
        mgr.db
            .update_task_status("bt-failed", 4, "boom")
            .await
            .expect("mark failed");

        let queued_bt = |task_id: &str| QueuedTask {
            task_id: task_id.to_string(),
            url: MAGNET.to_string(),
            save_dir: save_dir.clone(),
            file_name: String::new(),
            segments: 0,
            is_resume: false,
            cookies: String::new(),
            referrer: String::new(),
            hint_file_size: 0,
            torrent_file_bytes: Vec::new(),
            proxy_url: String::new(),
            user_agent: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            extra_headers: std::collections::HashMap::new(),
            selected_file_indices: Vec::new(),
            method: None,
            body: None,
            audio_url: None,
            resolver_plugin_id: String::new(),
            resolved: false,
            range_supported: false,
            resolver_item: String::new(),
            source_url: String::new(),
            resolver_patch: None,
        };
        let status_of = |db: &Db, id: &str| {
            let db = db.clone();
            let id = id.to_string();
            async move {
                db.load_task_by_id(&id)
                    .await
                    .expect("load task")
                    .expect("task exists")
                    .status
            }
        };

        // 预检：新建 BT 任务与恢复 BT 任务的拒绝判定。
        assert!(mgr.rejects_new_bt_task(MAGNET, &[]));
        assert!(mgr.rejects_new_bt_task("", &[1, 2, 3]));
        assert!(!mgr.rejects_new_bt_task("https://example.com/f.zip", &[]));
        let ids: Vec<String> = ["bt-stuck", "http-paused", "bt-paused"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let mut rejected = mgr
            .bt_disabled_resume_rejections(&ids)
            .await
            .expect("load rejections");
        rejected.sort();
        assert_eq!(rejected, ["bt-paused", "bt-stuck"]);

        // 排队中的 BT 项：drain 时落回暂停，而不是启动后被置失败(4)。
        mgr.pending_queue.push_back(queued_bt("bt-queued"));
        mgr.drain_queue().await;
        assert!(mgr.pending_queue.is_empty(), "BT item must leave the queue");
        assert!(!mgr.active_tasks.contains_key("bt-queued"));
        assert_eq!(status_of(&mgr.db, "bt-queued").await, 2);
        assert!(
            sink.events().iter().any(|event| matches!(
                event,
                EngineEvent::TaskProgress { task_id, status: 2, .. } if task_id == "bt-queued"
            )),
            "parking must broadcast the paused state"
        );

        // 禁用生效时的即时清扫（无 BT 会话也要执行）。
        mgr.pending_queue.push_back(queued_bt("bt-swept"));
        mgr.invalidate_bt_session().await;
        assert!(mgr.pending_queue.is_empty());
        assert_eq!(status_of(&mgr.db, "bt-swept").await, 2);

        // 恢复：停在「下载中」假象的任务落回暂停；暂停 / 失败任务保持原状。
        for id in ["bt-stuck", "bt-paused", "bt-failed"] {
            mgr.resume_task(id).await;
            assert!(!mgr.active_tasks.contains_key(id), "{id} must not spawn");
        }
        assert_eq!(status_of(&mgr.db, "bt-stuck").await, 2);
        assert_eq!(status_of(&mgr.db, "bt-paused").await, 2);
        assert_eq!(status_of(&mgr.db, "bt-failed").await, 4);

        // 重新启用后预检不再拒绝。
        mgr.set_bt_config(BtConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(!mgr.rejects_new_bt_task(MAGNET, &[]));
        assert!(
            mgr.bt_disabled_resume_rejections(&ids)
                .await
                .expect("load rejections")
                .is_empty()
        );
    }

    /// 禁用 BT 必须释放已建立的真实会话（关闭监听、停止做种），而不只是拒绝新任务；
    /// 重新创建需等再次启用。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn disabling_bt_releases_live_session_and_blocks_recreation() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let work = std::env::temp_dir().join(format!(
            "fluxdown_bt_disable_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&work).expect("create work dir");
        let port = std::net::TcpListener::bind(("0.0.0.0", 0))
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local addr")
            .port();
        let enabled_config = BtConfig {
            enabled: true,
            enable_dht: false,
            enable_upnp: false,
            port_start: port,
            port_end: port,
            ..Default::default()
        };
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: work.to_string_lossy().into_owned(),
                app_data_dir: work.to_string_lossy().into_owned(),
                data_dir: work.clone(),
                bt_config: enabled_config.clone(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        mgr.ensure_bt_session()
            .await
            .expect("enabled BT must create a session");
        assert!(mgr.bt_session.is_some(), "live session must exist");

        mgr.set_bt_config(BtConfig {
            enabled: false,
            ..enabled_config
        });
        mgr.invalidate_bt_session().await;
        assert!(
            mgr.bt_session.is_none(),
            "disabling BT must release the live session"
        );
        let err = mgr
            .ensure_bt_session()
            .await
            .expect_err("disabled BT must not recreate the session");
        assert!(
            err.to_string()
                .contains("BitTorrent is disabled in settings"),
            "expected disabled message, got {err:?}"
        );
        assert!(mgr.bt_session.is_none(), "no session after refused ensure");

        if let Err(error) = std::fs::remove_dir_all(&work) {
            tracing::debug!(%error, "BT disable test directory cleanup skipped");
        }
    }

    #[cfg(feature = "bt")]
    fn bt_test_config(port: u16) -> BtConfig {
        BtConfig {
            enabled: true,
            enable_dht: false,
            enable_upnp: false,
            enable_utp: false,
            port_start: port,
            port_end: port,
            ..Default::default()
        }
    }

    #[cfg(feature = "bt")]
    fn bt_test_manager(db: Db, work: &std::path::Path, config: BtConfig) -> DownloadManager {
        DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: work.to_string_lossy().into_owned(),
                app_data_dir: work.to_string_lossy().into_owned(),
                data_dir: work.to_path_buf(),
                bt_config: config,
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager")
    }

    #[cfg(feature = "bt")]
    fn bt_test_dir(tag: &str) -> (std::path::PathBuf, u16) {
        let work = std::env::temp_dir().join(format!(
            "fluxdown_bt_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&work).expect("create work dir");
        let port = std::net::TcpListener::bind(("0.0.0.0", 0))
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local addr")
            .port();
        (work, port)
    }

    /// #939 现场：启动续种的本地数据校验尚在途时，会话因监听类设置变更重建。
    /// 任务必须在新会话上继续做种（对齐 qBittorrent：改设置不打断做种），
    /// 旧会话连同其 runtime 必须真正释放（不 panic、不留继续上传的僵尸会话），
    /// 新会话绑回同一端口。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn session_rebuild_carries_in_flight_reseed_onto_new_session() {
        let (work, port) = bt_test_dir("rebuild_reseed");
        let save = work.join("save");
        std::fs::create_dir_all(&save).expect("save dir");
        let payload: Vec<u8> = (0..8 * 1024 * 1024_u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        std::fs::write(save.join("payload.bin"), &payload).expect("payload");
        let torrent = librqbit::create_torrent(
            &save.join("payload.bin"),
            librqbit::CreateTorrentOptions {
                piece_length: Some(64 * 1024),
                ..Default::default()
            },
            &librqbit::spawn_utils::BlockingSpawner::new(1),
        )
        .await
        .expect("create torrent")
        .as_bytes()
        .expect("torrent bytes");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.insert_task(
            "seed",
            "torrent-file://local",
            "payload.bin",
            &save.to_string_lossy(),
            1,
            payload.len() as i64,
            "",
            "",
            "",
            3,
        )
        .await
        .expect("insert task");
        db.save_torrent_file_bytes("seed", &torrent)
            .await
            .expect("save torrent bytes");
        db.set_task_seeding_active("seed", 0)
            .await
            .expect("mark seeding");
        let config = bt_test_config(port);
        let mut mgr = bt_test_manager(db, &work, config.clone());
        mgr.ensure_bt_session().await.expect("BT session");
        let old = mgr.bt_session.clone().expect("live session");
        let task = mgr
            .db
            .load_task_by_id("seed")
            .await
            .expect("load task")
            .expect("task row");
        // 启动续种：挂载在 BT runtime 上进行，校验未完成就触发重建。
        mgr.spawn_reseed_from_disk(old.clone(), &task).await;
        let old_session = Arc::downgrade(&old);
        let old_port = old.session().listen_addr().map(|addr| addr.port());
        drop(old);

        mgr.set_bt_config(BtConfig {
            mse_mode: crate::bt_config::BtMseMode::Forced,
            ..config
        });
        mgr.invalidate_bt_session().await;

        let new = mgr.bt_session.clone().expect("session rebuilt");
        assert_ne!(Arc::as_ptr(&new), old_session.as_ptr(), "a new session");
        assert_eq!(
            new.session().listen_addr().map(|addr| addr.port()),
            old_port,
            "rebuilt session binds the same port"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let row = mgr
                .db
                .load_task_by_id("seed")
                .await
                .expect("load task")
                .expect("task row");
            let registered = new
                .seeding_manager()
                .active_task_ids()
                .await
                .contains(&"seed".to_string());
            if row.seeding_status == SEEDING_STATUS_ACTIVE
                && registered
                && old_session.upgrade().is_none()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "seeding must resume on the new session and the old one must be released \
                 (status={}, registered={registered}, old alive={})",
                row.seeding_status,
                old_session.upgrade().is_some()
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        mgr.shutdown().await;
        drop(new);
        if let Err(error) = std::fs::remove_dir_all(&work) {
            tracing::debug!(%error, "BT rebuild test directory cleanup skipped");
        }
    }

    /// 因分享率达标停止做种的任务被用户手动恢复后，求值 tick 不得再按同一限制把它
    /// 停回去（对齐 qBittorrent 强制继续）；用户再次停止做种即撤销豁免。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn manual_resume_after_ratio_stop_is_exempt_from_limits_until_stopped_again() {
        let (work, port) = bt_test_dir("resume_exempt");
        let save = work.join("save");
        std::fs::create_dir_all(&save).expect("save dir");
        let payload: Vec<u8> = (0..1024 * 1024_u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        std::fs::write(save.join("payload.bin"), &payload).expect("payload");
        let torrent = librqbit::create_torrent(
            &save.join("payload.bin"),
            librqbit::CreateTorrentOptions {
                piece_length: Some(64 * 1024),
                ..Default::default()
            },
            &librqbit::spawn_utils::BlockingSpawner::new(1),
        )
        .await
        .expect("create torrent")
        .as_bytes()
        .expect("torrent bytes");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.insert_task(
            "seed",
            "torrent-file://local",
            "payload.bin",
            &save.to_string_lossy(),
            1,
            payload.len() as i64,
            "",
            "",
            "",
            3,
        )
        .await
        .expect("insert task");
        db.save_torrent_file_bytes("seed", &torrent)
            .await
            .expect("save torrent bytes");
        // 已上传 = 一倍体积，全局分享率上限 0.5：达标停止。
        db.add_task_uploaded_bytes("seed", payload.len() as i64)
            .await
            .expect("uploaded");
        let stopped = SeedingStopReason::RatioReached;
        db.update_task_seeding_status("seed", stopped.as_i32(), stopped.message())
            .await
            .expect("mark ratio stop");

        let config = BtConfig {
            seed_ratio_limit: 0.5,
            ..bt_test_config(port)
        };
        let mut mgr = bt_test_manager(db, &work, config);
        mgr.resume_task("seed").await;
        assert!(
            mgr.db
                .list_seed_limits_exempt_ids()
                .await
                .expect("exempt ids")
                .contains("seed"),
            "manual resume of a limit-stopped seed must be exempted"
        );

        let seeding_status = |mgr: &DownloadManager| {
            let db = mgr.db.clone();
            async move {
                db.load_task_by_id("seed")
                    .await
                    .expect("load task")
                    .expect("task row")
                    .seeding_status
            }
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while seeding_status(&mgr).await != SEEDING_STATUS_ACTIVE {
            assert!(
                std::time::Instant::now() < deadline,
                "resumed seed must become active"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        mgr.tick_seeding_evaluation().await;
        assert_eq!(
            seeding_status(&mgr).await,
            SEEDING_STATUS_ACTIVE,
            "an exempt seed must survive the limit evaluation tick"
        );

        mgr.pause_task("seed").await;
        assert_eq!(
            seeding_status(&mgr).await,
            SeedingStopReason::UserStopped.as_i32()
        );
        assert!(
            mgr.db
                .list_seed_limits_exempt_ids()
                .await
                .expect("exempt ids")
                .is_empty(),
            "stopping seeding again must clear the exemption"
        );

        mgr.shutdown().await;
        if let Err(error) = std::fs::remove_dir_all(&work) {
            tracing::debug!(%error, "BT resume exemption test directory cleanup skipped");
        }
    }

    /// 本机做种：为磁盘上已有的文件生成种子，登记为已完成任务并开始做种；
    /// `unlimited` 把任务级限制全写成「不限制」，即便全局分享率上限已被超过也不会停。
    #[cfg(feature = "bt")]
    #[tokio::test(flavor = "multi_thread")]
    async fn local_seed_registers_completed_task_and_never_stops_when_unlimited() {
        let (work, port) = bt_test_dir("local_seed");
        let content = work.join("content");
        std::fs::create_dir_all(&content).expect("content dir");
        let file = content.join("payload.bin");
        let payload: Vec<u8> = (0..1024 * 1024_u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        std::fs::write(&file, &payload).expect("payload");
        let bytes = crate::bt_local_seed::build_torrent(&file, Vec::new())
            .await
            .expect("build torrent");
        let plan = crate::bt_local_seed::plan_local_seed(&file, bytes).expect("plan");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let config = BtConfig {
            seed_ratio_limit: 0.0001,
            ..bt_test_config(port)
        };
        let mut mgr = bt_test_manager(db, &work, config);
        let task_id = mgr
            .create_local_seed(plan, "main".to_owned(), true)
            .await
            .expect("create local seed");

        let load = |mgr: &DownloadManager| {
            let db = mgr.db.clone();
            let task_id = task_id.clone();
            async move {
                db.load_task_by_id(&task_id)
                    .await
                    .expect("load task")
                    .expect("task row")
            }
        };
        let row = load(&mgr).await;
        assert_eq!(row.status, 3);
        assert_eq!(row.file_name, "payload.bin");
        assert_eq!(row.save_dir, content.to_string_lossy());
        assert_eq!(row.total_bytes, payload.len() as i64);
        assert_eq!(row.seed_ratio_limit_milli, -1);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while load(&mgr).await.seeding_status != SEEDING_STATUS_ACTIVE {
            assert!(
                std::time::Instant::now() < deadline,
                "local seed must become active"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // 即便已有上传量、全局分享率上限极低，不限制的任务也不会被停。
        mgr.db
            .add_task_uploaded_bytes(&task_id, payload.len() as i64)
            .await
            .expect("uploaded");
        mgr.tick_seeding_evaluation().await;
        assert_eq!(load(&mgr).await.seeding_status, SEEDING_STATUS_ACTIVE);

        // 同一份数据再做种一次：按 info hash 识别为重复，不创建第二个任务。
        let again_bytes = crate::bt_local_seed::build_torrent(&file, Vec::new())
            .await
            .expect("rebuild torrent");
        let again = crate::bt_local_seed::plan_local_seed(&file, again_bytes).expect("plan again");
        match mgr.create_local_seed(again, "main".to_owned(), false).await {
            Err(crate::bt_local_seed::LocalSeedError::Duplicate {
                task_id: owner,
                name,
            }) => {
                assert_eq!(owner, task_id);
                assert_eq!(name, "payload.bin");
            }
            other => panic!("expected duplicate, got {other:?}"),
        }

        // 实况：本机做种生成的种子无 tracker，列表来自会话追加（内置公共 tracker）；无人连接时无 peer。
        let status = mgr.bt_status(&task_id).await.expect("bt status");
        assert!(!status.trackers.is_empty());
        assert!(
            status
                .trackers
                .iter()
                .all(|row| row.source == crate::bt_status::TrackerSource::Session)
        );
        assert!(status.peers.is_empty());

        mgr.shutdown().await;
        if let Err(error) = std::fs::remove_dir_all(&work) {
            tracing::debug!(%error, "BT local seed test directory cleanup skipped");
        }
    }

    /// 端到端：真实 librqbit 会话向本地 HTTP tracker announce，状态采集层把它折叠成「正常」。
    /// 验证 `bt_status::layer` 确实能从 `tracker_comms` 的真实 span / 事件里读到汇报结果
    /// （单元测试只用手写事件）。需全局 subscriber，所以单独成测（nextest 每测一进程）。
    #[cfg(feature = "bt")]
    #[tokio::test(flavor = "multi_thread")]
    async fn tracker_status_layer_sees_real_announces() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tracing_subscriber::layer::SubscriberExt as _;
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(crate::bt_status::layer()),
        )
        .expect("global subscriber");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("tracker listener");
        let tracker_port = listener.local_addr().expect("tracker addr").port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits_in = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                hits_in.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    if socket.read(&mut buf).await.is_err() {
                        return;
                    }
                    let body = b"d8:intervali1800e5:peers0:e";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    if socket.write_all(body).await.is_err() {
                        tracing::debug!("test tracker client went away");
                    }
                });
            }
        });

        let (work, port) = bt_test_dir("tracker_layer");
        let content = work.join("content");
        std::fs::create_dir_all(&content).expect("content dir");
        let file = content.join("payload.bin");
        std::fs::write(&file, vec![5u8; 256 * 1024]).expect("payload");
        let tracker_url = format!("http://127.0.0.1:{tracker_port}/announce");
        let bytes = crate::bt_local_seed::build_torrent(&file, Vec::new())
            .await
            .expect("torrent");
        let plan = crate::bt_local_seed::plan_local_seed(&file, bytes).expect("plan");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let config = BtConfig {
            custom_trackers: tracker_url.clone(),
            ..bt_test_config(port)
        };
        let mut mgr = bt_test_manager(db, &work, config);
        let task_id = mgr
            .create_local_seed(plan, "main".to_owned(), false)
            .await
            .expect("local seed");

        assert!(crate::bt_status::installed());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let status = mgr.bt_status(&task_id).await.expect("bt status");
            let working = status.trackers.iter().find(|row| {
                row.url == tracker_url
                    && row
                        .entry
                        .as_ref()
                        .is_some_and(|entry| entry.state == crate::bt_status::TrackerState::Working)
            });
            if let Some(row) = working {
                let entry = row.entry.clone().expect("entry");
                assert_eq!(entry.peers, Some(0));
                assert!(entry.last_ok_at > 0);
                assert!(entry.next_announce_at > entry.last_ok_at);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tracker never reported working (hits={}, seeding_status={:?}): {:?}",
                hits.load(std::sync::atomic::Ordering::SeqCst),
                mgr.db
                    .load_task_by_id(&task_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|t| t.seeding_status),
                status.trackers
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        mgr.shutdown().await;
        if let Err(error) = std::fs::remove_dir_all(&work) {
            tracing::debug!(%error, "BT tracker layer test directory cleanup skipped");
        }
    }

    /// tracker 列表变更（设置 / 订阅刷新）只经 `set_bt_config` 热生效于新加入
    /// 的种子：会话不重建（做种与下载不被打断），新 add 带上新列表。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn tracker_list_change_applies_to_new_torrents_without_rebuild() {
        let (work, port) = bt_test_dir("tracker_hot");
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let config = bt_test_config(port);
        let mut mgr = bt_test_manager(db, &work, config.clone());
        mgr.ensure_bt_session().await.expect("BT session");
        let before = mgr.bt_session.clone().expect("live session");

        mgr.set_bt_config(BtConfig {
            custom_trackers: "udp://fresh.example:6969/announce".into(),
            subscription_trackers: "https://sub.example/announce".into(),
            ..config
        });
        let after = mgr.bt_session.clone().expect("session kept");
        assert!(
            Arc::ptr_eq(&before, &after),
            "tracker change must not rebuild"
        );

        let source = crate::bt_downloader::TorrentSource::Magnet(
            "magnet:?xt=urn:btih:def7006974f228ca39c44c806a0fb839a0469c97".into(),
        );
        let mut opts = librqbit::AddTorrentOptions::default();
        let librqbit::AddTorrent::Url(url) = after.add_input(&source, &mut opts) else {
            panic!("magnet stays a URL add");
        };
        let parsed = librqbit::Magnet::parse(&url).expect("valid magnet");
        assert_eq!(
            parsed.trackers,
            [
                "udp://fresh.example:6969/announce",
                "https://sub.example/announce"
            ]
        );

        mgr.shutdown().await;
        drop((before, after));
        if let Err(error) = std::fs::remove_dir_all(&work) {
            tracing::debug!(%error, "BT tracker test directory cleanup skipped");
        }
    }

    /// 暂停终态必须等下载器 flush + 最终进度落库后再进入 progress_reporter。
    /// 否则暂停帧会携带 3 秒周期内的旧 DB 快照，恢复时表现为百分比前跳。
    #[tokio::test]
    async fn pause_publishes_flushed_progress_after_matching_task_done() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "pause-flush", "/tmp", "file.bin", 1).await;
        db.update_task_total_bytes("pause-flush", 215_020_021)
            .await
            .expect("set total");
        db.update_task_progress("pause-flush", 30_000_000)
            .await
            .expect("set stale persisted progress");

        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_string(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        let mut progress_rx = mgr.take_progress_rx().expect("progress receiver");
        let token = CancellationToken::new();
        let waiter = token.clone();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
        });
        mgr.active_tasks.insert(
            "pause-flush".to_string(),
            ActiveTaskEntry {
                token,
                generation: 41,
                handle: Some(handle),
                is_bt: false,
                queue_id: String::new(),
            },
        );

        mgr.pause_task("pause-flush").await;
        assert!(
            !sink.events().iter().any(|event| matches!(
                event,
                EngineEvent::TaskProgress {
                    task_id,
                    status: 2,
                    ..
                } if task_id == "pause-flush"
            )),
            "active pause must not publish the stale DB snapshot directly"
        );
        assert!(
            progress_rx.try_recv().is_err(),
            "progress_reporter must wait for the downloader's final flush"
        );

        db.update_task_progress("pause-flush", 31_513_021)
            .await
            .expect("persist flushed progress");
        mgr.on_task_done(&TaskDone {
            task_id: "pause-flush".to_string(),
            generation: 41,
            reserved_temp_path: None,
            phase: TaskDonePhase::Finished,
        })
        .await;

        let paused = tokio::time::timeout(std::time::Duration::from_secs(1), progress_rx.recv())
            .await
            .expect("paused progress should be published")
            .expect("progress channel should stay open");
        assert_eq!(paused.status, 2);
        assert_eq!(paused.downloaded_bytes, 31_513_021);
        assert_eq!(paused.total_bytes, 215_020_021);
    }

    /// 快速暂停→恢复不得在旧下载器尚未退出时启动新一代；旧世代迟到的
    /// TaskDone 也不得把已经运行的新世代覆盖成 paused。
    #[tokio::test]
    async fn resume_waits_for_pause_flush_and_stale_done_cannot_pause_new_generation() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "pause-race", "/tmp", "file.bin", 1).await;
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_string(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        let mut progress_rx = mgr.take_progress_rx().expect("progress receiver");
        let old_token = CancellationToken::new();
        let old_waiter = old_token.clone();
        let old_handle = tokio::spawn(async move {
            old_waiter.cancelled().await;
        });
        mgr.active_tasks.insert(
            "pause-race".to_string(),
            ActiveTaskEntry {
                token: old_token,
                generation: 51,
                handle: Some(old_handle),
                is_bt: false,
                queue_id: String::new(),
            },
        );

        mgr.pause_task("pause-race").await;
        mgr.resume_task("pause-race").await;
        assert!(
            !mgr.active_tasks.contains_key("pause-race"),
            "resume must be deferred until the cancelled writer exits"
        );
        assert!(
            mgr.pending_pauses
                .get("pause-race")
                .is_some_and(|pending| pending.resume_requested),
            "resume intent must be retained while cancellation finishes"
        );

        // Model a newer generation installed by another actor operation before
        // the old TaskDone is consumed. The stale completion must be harmless.
        let new_token = CancellationToken::new();
        let new_waiter = new_token.clone();
        let new_handle = tokio::spawn(async move {
            new_waiter.cancelled().await;
        });
        mgr.active_tasks.insert(
            "pause-race".to_string(),
            ActiveTaskEntry {
                token: new_token,
                generation: 52,
                handle: Some(new_handle),
                is_bt: false,
                queue_id: String::new(),
            },
        );
        db.update_task_status("pause-race", 1, "")
            .await
            .expect("mark newer generation active");

        mgr.on_task_done(&TaskDone {
            task_id: "pause-race".to_string(),
            generation: 51,
            reserved_temp_path: None,
            phase: TaskDonePhase::Finished,
        })
        .await;

        assert_eq!(
            mgr.active_tasks
                .get("pause-race")
                .map(|entry| entry.generation),
            Some(52),
            "stale TaskDone must not remove the newer generation"
        );
        assert!(
            progress_rx.try_recv().is_err(),
            "stale pause completion must not publish a terminal paused frame"
        );
        assert!(
            !sink.events().iter().any(|event| matches!(
                event,
                EngineEvent::TaskProgress {
                    task_id,
                    status: 2,
                    ..
                } if task_id == "pause-race"
            )),
            "newer running generation must stay visible"
        );
    }

    /// 调度测试用管理器：全局并发 `max_concurrent`，其余默认。
    fn sched_manager(db: &Db, max_concurrent: usize) -> DownloadManager {
        sched_manager_with_sink(db, max_concurrent, Arc::new(RecordingSink::new()))
    }

    fn sched_manager_with_sink(
        db: &Db,
        max_concurrent: usize,
        sink: Arc<dyn EventSink>,
    ) -> DownloadManager {
        DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_string(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink,
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager")
    }

    async fn sqlite_fault_fixture(tag: &str) -> (PathBuf, Db, sqlx::SqlitePool) {
        let dir = unique_filetrack_test_dir(tag);
        std::fs::create_dir_all(&dir).expect("create fault test directory");
        let url = format!("sqlite://{}?mode=rwc", dir.join("tasks.db").display());
        let db = Db::connect(&url).await.expect("connect fault database");
        let injector = sqlx::SqlitePool::connect(&url)
            .await
            .expect("connect fault injector");
        (dir, db, injector)
    }

    #[tokio::test]
    async fn pause_database_failure_keeps_barrier_until_matching_done() {
        let (dir, db, injector) = sqlite_fault_fixture("pause_barrier_failure").await;
        insert_task_at_status(&db, "pause-db", &dir.to_string_lossy(), "file.bin", 1).await;
        sqlx::query("CREATE TRIGGER reject_pause BEFORE UPDATE OF status ON tasks WHEN NEW.status = 2 BEGIN SELECT RAISE(FAIL, 'pause write rejected'); END")
            .execute(&injector).await.expect("install pause write failure");
        let mut manager = sched_manager(&db, 1);
        let mut progress = manager.take_progress_rx().expect("progress receiver");
        occupy_slot(&mut manager, "pause-db", 17, "", false);

        manager.pause_task("pause-db").await;
        manager.resume_task("pause-db").await;
        assert!(!manager.active_tasks.contains_key("pause-db"));
        assert!(
            manager
                .pending_pauses
                .get("pause-db")
                .is_some_and(|pending| { pending.generation == 17 && pending.resume_requested })
        );
        assert_eq!(
            db.load_task_by_id("pause-db")
                .await
                .expect("load task")
                .expect("task exists")
                .status,
            1
        );
        assert!(matches!(
            progress.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        manager
            .on_task_done(&TaskDone {
                task_id: "pause-db".to_string(),
                generation: 16,
                reserved_temp_path: None,
                phase: TaskDonePhase::Finished,
            })
            .await;
        assert!(
            manager.pending_pauses.contains_key("pause-db"),
            "a stale done must not release the cancelled writer's barrier"
        );
        assert!(
            manager.finish_pending_pause("pause-db", 17).await,
            "only the matching completion may release the explicit resume intent"
        );
        assert!(!manager.pending_pauses.contains_key("pause-db"));
        assert!(
            matches!(progress.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "a failed pause must not publish a false paused frame"
        );

        drop(manager);
        injector.close().await;
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning(
                "download-manager-test",
                "remove pause failure fixture",
                &error,
            );
        }
    }

    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_progress_database_error_publishes_failed_task() {
        let (dir, db, injector) = sqlite_fault_fixture("bt_progress_failure").await;
        insert_task_at_status(&db, "bt-db", &dir.to_string_lossy(), "file.bin", 1).await;
        sqlx::query("CREATE TRIGGER reject_bt_progress BEFORE UPDATE OF downloaded_bytes ON tasks BEGIN SELECT RAISE(FAIL, 'BT progress write rejected'); END")
            .execute(&injector).await.expect("install BT progress failure");
        let failure = db
            .update_task_progress("bt-db", 2048)
            .await
            .expect_err("periodic progress must fail");
        let (tx, mut rx) = mpsc::channel(1);
        handle_bt_worker_error("bt-db", &downloader::DownloadError::Db(failure), &db, &tx).await;
        assert_eq!(
            db.load_task_by_id("bt-db")
                .await
                .expect("load task")
                .expect("task exists")
                .status,
            4
        );
        assert_eq!(rx.recv().await.expect("failure frame").status, 4);

        injector.close().await;
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning(
                "download-manager-test",
                "remove BT progress failure fixture",
                &error,
            );
        }
    }

    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_outer_error_preserves_existing_terminal_and_duplicate_state() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "bt-completed", "/tmp", "complete.bin", 3).await;
        insert_task_at_status(&db, "bt-duplicate", "/tmp", "duplicate.bin", 4).await;
        let sentinel = format!("{}owner-task", bt_downloader::DUPLICATE_TORRENT_MSG_PREFIX);
        db.update_task_status("bt-duplicate", 4, &sentinel)
            .await
            .expect("mark duplicate");
        let error = downloader::DownloadError::Other("late BT worker failure".to_string());
        let (tx, mut rx) = mpsc::channel(1);

        handle_bt_worker_error("bt-completed", &error, &db, &tx).await;
        handle_bt_worker_error("bt-duplicate", &error, &db, &tx).await;
        assert_eq!(
            db.load_task_by_id("bt-completed")
                .await
                .expect("load completed task")
                .expect("task exists")
                .status,
            3
        );
        let duplicate = db
            .load_task_by_id("bt-duplicate")
            .await
            .expect("load duplicate task")
            .expect("task exists");
        assert_eq!(duplicate.status, 4);
        assert_eq!(
            duplicate
                .error_message
                .strip_prefix(bt_downloader::DUPLICATE_TORRENT_MSG_PREFIX),
            Some("owner-task")
        );
        assert!(
            matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "outer failure must not emit a competing terminal frame"
        );
    }

    #[tokio::test]
    async fn task_snapshot_read_failure_does_not_publish_false_empty_snapshot() {
        let (dir, db, injector) = sqlite_fault_fixture("snapshot_read_failure").await;
        insert_task_at_status(&db, "preserved-task", &dir.to_string_lossy(), "file.bin", 2).await;
        let sink = Arc::new(RecordingSink::new());
        let mut manager = sched_manager_with_sink(&db, 1, sink.clone());
        manager.startup_reset_done = true;
        sqlx::query("ALTER TABLE tasks RENAME TO unavailable_tasks")
            .execute(&injector)
            .await
            .expect("install snapshot read failure");

        manager.load_and_send_all_tasks().await;
        assert!(
            !sink
                .events()
                .iter()
                .any(|event| matches!(event, EngineEvent::TasksSnapshot(_))),
            "database failure must not clear the consumer's existing task list"
        );
        sqlx::query("ALTER TABLE unavailable_tasks RENAME TO tasks")
            .execute(&injector)
            .await
            .expect("restore task table");
        manager.load_and_send_all_tasks().await;
        assert!(sink.events().iter().any(|event| {
            match event {
                EngineEvent::TasksSnapshot(tasks) => tasks
                    .iter()
                    .any(|task| task.task_id == "preserved-task" && task.status == 2),
                _ => false,
            }
        }));

        drop(manager);
        injector.close().await;
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning(
                "download-manager-test",
                "remove snapshot failure fixture",
                &error,
            );
        }
    }

    #[tokio::test]
    async fn create_persist_failure_rolls_back_whole_batch_and_publishes_nothing() {
        let (dir, db, injector) = sqlite_fault_fixture("create_failure").await;
        sqlx::query("CREATE TRIGGER reject_context BEFORE INSERT ON tasks WHEN NEW.cookies = 'session=secret' BEGIN SELECT RAISE(FAIL, 'context write rejected'); END")
            .execute(&injector).await.expect("install context failure");
        let sink = Arc::new(RecordingSink::new());
        let mut manager = sched_manager_with_sink(&db, 1, sink.clone());
        let spec = |name: &str, cookies: &str| NewTaskSpec {
            url: format!("http://127.0.0.1:1/{name}"),
            save_dir: dir.to_string_lossy().into_owned(),
            file_name: name.to_string(),
            cookies: cookies.to_string(),
            start_paused: true,
            ..Default::default()
        };
        let created = manager
            .create_task(spec("file.bin", "session=secret"))
            .await;
        assert!(
            created.is_none(),
            "failed persistence must not create a task"
        );
        let batch = manager
            .create_tasks_batch(
                "failing-batch",
                vec![spec("ok.bin", ""), spec("bad.bin", "session=secret")],
            )
            .await;
        assert!(batch.is_err(), "one failing row fails the whole batch");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
            .fetch_one(&injector)
            .await
            .expect("count tasks");
        assert_eq!(rows, 0, "the batch transaction must roll back every row");
        let records: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_create_batches")
            .fetch_one(&injector)
            .await
            .expect("count batch records");
        assert_eq!(records, 0, "a failed batch must stay retryable");
        assert!(manager.pending_queue.is_empty());
        assert!(manager.active_tasks.is_empty());
        assert!(!sink.events().iter().any(|event| matches!(
            event,
            EngineEvent::TaskChanged(_)
                | EngineEvent::TasksChanged(_)
                | EngineEvent::TaskProgress { .. }
                | EngineEvent::TaskQueueChanged { .. }
        )));
        drop(manager);
        injector.close().await;
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning(
                "download-manager-test",
                "remove creation failure fixture",
                &error,
            );
        }
    }

    /// 取出 sink 里收到的全部 `TaskNotice`（按到达顺序）。
    fn collected_notices(sink: &RecordingSink) -> Vec<crate::webhook::TaskNotice> {
        sink.events()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::TaskNotice(notice) => Some(notice),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn task_event_helper_emits_notice_with_unique_ids_and_keeps_webhook_return() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mgr = sched_manager_with_sink(&db, 1, sink.clone());
        insert_task_at_status(&db, "t-done", "/tmp", "done.bin", 3).await;
        insert_task_at_status(&db, "t-fail", "/tmp", "fail.bin", 4).await;
        let done = db
            .load_task_by_id("t-done")
            .await
            .expect("load")
            .expect("task");
        let failed = db
            .load_task_by_id("t-fail")
            .await
            .expect("load")
            .expect("task");
        use crate::webhook::WebhookEventKind as Kind;
        let before = chrono::Utc::now().timestamp_millis();
        // 没有任何端点订阅时 webhook.emit 返回 0（simulate 依赖该语义），helper 原样透传。
        assert_eq!(
            mgr.emit_task_event(mgr.webhook_event_from_task(Kind::TaskCompleted, &done)),
            0
        );
        assert_eq!(
            mgr.emit_task_event(mgr.webhook_event_from_task(Kind::TaskFailed, &failed)),
            0
        );
        let notices = collected_notices(&sink);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].event, "task.completed");
        assert_eq!(notices[1].event, "task.failed");
        assert_ne!(notices[0].delivery_id, notices[1].delivery_id);
        assert!(uuid::Uuid::parse_str(&notices[0].delivery_id).is_ok());
        assert!(notices[0].timestamp_ms >= before);
        let task = notices[0].task.as_ref().expect("task snapshot");
        assert_eq!(task.id, "t-done");
        assert_eq!(task.file_name, "done.bin");
        assert_eq!(task.status, 3);
    }

    #[tokio::test]
    async fn lifecycle_trigger_points_emit_task_notice() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = sched_manager_with_sink(&db, 1, sink.clone());

        // task.created：建任务即发。
        let id = mgr
            .create_task(NewTaskSpec {
                url: "http://127.0.0.1:1/file.bin".to_string(),
                save_dir: std::env::temp_dir().to_string_lossy().into_owned(),
                file_name: "file.bin".to_string(),
                start_paused: true,
                ..Default::default()
            })
            .await
            .expect("create paused task");
        let notices = collected_notices(&sink);
        assert_eq!(notices.len(), 1, "created 应恰好一条");
        assert_eq!(notices[0].event, "task.created");
        assert_eq!(
            notices[0].task.as_ref().map(|t| t.id.as_str()),
            Some(id.as_str())
        );

        // task.paused：读回任务行后发。
        mgr.emit_paused_webhook(&id).await;
        let notices = collected_notices(&sink);
        assert_eq!(
            notices.last().map(|n| n.event.as_str()),
            Some("task.paused")
        );

        // queue.drained：占用 → 空的边沿触发，无任务快照。
        mgr.occupied_queues.insert("default".to_string());
        mgr.sync_queue_occupancy();
        let notices = collected_notices(&sink);
        let drained = notices.last().expect("drained notice");
        assert_eq!(drained.event, "queue.drained");
        assert_eq!(drained.queue_id, "default");
        assert!(drained.task.is_none());
    }

    /// `task.started`（真实建任务即启动）与 `task.completed` / `task.failed`（`on_task_done`
    /// 终态点）都经同一个 helper 发出 `TaskNotice`。
    #[tokio::test]
    async fn started_completed_and_failed_trigger_points_emit_task_notice() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = sched_manager_with_sink(&db, 1, sink.clone());
        mgr.set_max_auto_retries(0);

        // task.started：未暂停的新任务在派发时发出（紧随 task.created）。
        let id = mgr
            .create_task(NewTaskSpec {
                url: "http://127.0.0.1:1/never.bin".to_string(),
                save_dir: std::env::temp_dir().to_string_lossy().into_owned(),
                file_name: "never.bin".to_string(),
                ..Default::default()
            })
            .await
            .expect("create task");
        let kinds: Vec<String> = collected_notices(&sink)
            .into_iter()
            .filter(|n| n.task.as_ref().is_some_and(|t| t.id == id))
            .map(|n| n.event)
            .collect();
        assert_eq!(kinds, ["task.created", "task.started"]);

        // task.completed / task.failed：on_task_done 的终态点（失败仅在放弃重试后发出）。
        for (task_id, status, expected) in [
            ("done-ok", 3, "task.completed"),
            ("done-bad", 4, "task.failed"),
        ] {
            insert_task_at_status(&db, task_id, "/tmp", "x.bin", status).await;
            mgr.active_tasks.insert(
                task_id.to_string(),
                ActiveTaskEntry {
                    token: CancellationToken::new(),
                    generation: 3,
                    handle: None,
                    is_bt: false,
                    queue_id: String::new(),
                },
            );
            mgr.on_task_done(&TaskDone {
                task_id: task_id.to_string(),
                generation: 3,
                reserved_temp_path: None,
                phase: TaskDonePhase::Finished,
            })
            .await;
            let notice = collected_notices(&sink)
                .into_iter()
                .find(|n| n.event == expected && n.task.as_ref().is_some_and(|t| t.id == task_id))
                .unwrap_or_else(|| panic!("{expected} notice for {task_id}"));
            assert_eq!(notice.task.as_ref().map(|t| t.status), Some(status));
        }
    }

    /// 在 `queue_id` 队列上插一条占位活跃任务（只用于驱动占用集，不起真实下载）。
    fn insert_placeholder_active(mgr: &mut DownloadManager, task_id: &str, queue_id: &str) {
        mgr.active_tasks.insert(
            task_id.to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 1,
                handle: None,
                is_bt: false,
                queue_id: queue_id.to_string(),
            },
        );
    }

    /// `queue.started` / `queue.drained` 是同一队列上的一对边沿：每次空闲 → 占用
    /// 只发一次 started，占用 → 空闲只发一次 drained，二者严格交替。
    #[tokio::test]
    async fn queue_started_and_drained_alternate_once_per_edge() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = sched_manager_with_sink(&db, 4, sink.clone());
        let edges = |sink: &RecordingSink| -> Vec<String> {
            collected_notices(sink)
                .into_iter()
                .filter(|n| n.event.starts_with("queue."))
                .map(|n| format!("{}:{}", n.event, n.queue_id))
                .collect()
        };

        // 空闲时重算不产生任何事件。
        mgr.sync_queue_occupancy();
        assert!(edges(&sink).is_empty());

        // 空闲 → 占用：恰好一条 started，且无任务快照。
        insert_placeholder_active(&mut mgr, "a", "q1");
        mgr.sync_queue_occupancy();
        assert_eq!(edges(&sink), ["queue.started:q1"]);
        let started = collected_notices(&sink).pop().expect("started notice");
        assert!(started.task.is_none());

        // 已占用期间再加任务 / 重复重算：不重复通知。
        insert_placeholder_active(&mut mgr, "b", "q1");
        mgr.sync_queue_occupancy();
        mgr.sync_queue_occupancy();
        assert_eq!(edges(&sink), ["queue.started:q1"]);

        // 只有最后一个任务离开才 drained；其间占用不变。
        mgr.active_tasks.remove("a");
        mgr.sync_queue_occupancy();
        assert_eq!(edges(&sink), ["queue.started:q1"]);
        mgr.active_tasks.remove("b");
        mgr.sync_queue_occupancy();
        assert_eq!(edges(&sink), ["queue.started:q1", "queue.drained:q1"]);

        // 再次空闲 → 占用：重新发 started；另一个队列独立计边沿。
        insert_placeholder_active(&mut mgr, "c", "q1");
        insert_placeholder_active(&mut mgr, "d", "q2");
        mgr.sync_queue_occupancy();
        let all = edges(&sink);
        assert_eq!(all.len(), 4);
        assert_eq!(all[..2], ["queue.started:q1", "queue.drained:q1"]);
        assert!(all.contains(&"queue.started:q1".to_string()));
        assert!(all.contains(&"queue.started:q2".to_string()));
    }

    /// 已排程自动重试的任务视为仍占用：重试间隙既不发 drained，也不会在回流后
    /// 再补一条 started。
    #[tokio::test]
    async fn queue_started_not_repeated_across_retry_gap() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = sched_manager_with_sink(&db, 4, sink.clone());

        insert_placeholder_active(&mut mgr, "r", "q1");
        mgr.sync_queue_occupancy();
        // 任务失败离场、排程重试：占用由 retry_scheduled 接力。
        mgr.active_tasks.remove("r");
        mgr.retry_scheduled
            .insert("r".to_string(), "q1".to_string());
        mgr.sync_queue_occupancy();
        // 重试回流：重新进入活跃集合。
        mgr.retry_scheduled.remove("r");
        insert_placeholder_active(&mut mgr, "r", "q1");
        mgr.sync_queue_occupancy();

        let events: Vec<String> = collected_notices(&sink)
            .into_iter()
            .map(|n| n.event)
            .collect();
        assert_eq!(events, ["queue.started"]);
    }

    /// 进程启动恢复只是把上次的占用接回来：不发 `queue.started`，但占用已登记，
    /// 之后清空仍能触发 `queue.drained`、再次忙碌仍能触发 `queue.started`。
    #[tokio::test]
    async fn queue_started_is_silent_on_boot_restore() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "was_active", "").await;
        db.update_task_status("was_active", 1, "")
            .await
            .expect("status");
        db.set_config("auto_resume_on_start", "true")
            .await
            .expect("config");

        let sink = Arc::new(RecordingSink::new());
        let mut mgr = sched_manager_with_sink(&db, 4, sink.clone());
        mgr.load_queues().await;
        mgr.load_and_send_all_tasks().await;

        assert!(
            mgr.active_tasks.contains_key("was_active"),
            "auto_resume_on_start 应已拉起被中断的任务"
        );
        assert!(!mgr.restoring_on_start, "恢复结束后必须放开静默开关");
        assert!(
            collected_notices(&sink)
                .iter()
                .all(|n| n.event != "queue.started"),
            "启动恢复不得发 queue.started"
        );
        let restored_queue = mgr
            .active_tasks
            .get("was_active")
            .map(|e| e.queue_id.clone())
            .expect("restored entry");
        assert!(mgr.occupied_queues.contains(&restored_queue));

        // 恢复出的任务离场 → drained；之后再忙碌 → started。
        cancel_all_active(&mgr);
        mgr.active_tasks.remove("was_active");
        mgr.sync_queue_occupancy();
        insert_placeholder_active(&mut mgr, "fresh", &restored_queue);
        mgr.sync_queue_occupancy();
        let events: Vec<String> = collected_notices(&sink)
            .into_iter()
            .map(|n| n.event)
            .filter(|e| e.starts_with("queue."))
            .collect();
        assert_eq!(events, ["queue.drained", "queue.started"]);
    }

    /// 真实建任务：队列的 `queue.started` 必须先于该队列的首个 `task.started`。
    #[tokio::test]
    async fn queue_started_precedes_first_task_started() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = sched_manager_with_sink(&db, 1, sink.clone());
        mgr.set_max_auto_retries(0);

        mgr.create_task(NewTaskSpec {
            url: "http://127.0.0.1:1/never.bin".to_string(),
            save_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            file_name: "never.bin".to_string(),
            ..Default::default()
        })
        .await
        .expect("create task");
        let events: Vec<String> = collected_notices(&sink)
            .into_iter()
            .map(|n| n.event)
            .collect();
        let position = |name: &str| events.iter().position(|e| e == name);
        let queue_started = position("queue.started").expect("queue.started notice");
        let task_started = position("task.started").expect("task.started notice");
        assert!(queue_started < task_started, "{events:?}");
        assert_eq!(
            events.iter().filter(|e| *e == "queue.started").count(),
            1,
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn rename_database_failure_rolls_back_main_temp_and_audio_files() {
        let (dir, db, injector) = sqlite_fault_fixture("rename_db_failure").await;
        insert_task_at_status(&db, "rename-db", &dir.to_string_lossy(), "old.bin", 2).await;
        db.save_audio_url("rename-db", "https://example.com/audio.m4a")
            .await
            .expect("save audio URL");
        let files = [
            ("old.bin", b"final".as_slice()),
            ("old.bin.fdownloading", b"partial".as_slice()),
            ("old.audio.m4a", b"audio".as_slice()),
            ("old.audio.m4a.fdownloading", b"audio partial".as_slice()),
        ];
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).expect("write rename fixture");
        }
        sqlx::query("CREATE TRIGGER reject_name BEFORE UPDATE OF file_name ON tasks BEGIN SELECT RAISE(FAIL, 'name write rejected'); END")
            .execute(&injector).await.expect("install name failure");
        let mut manager = sched_manager(&db, 1);
        let error = manager
            .rename_task("rename-db", "new.bin")
            .await
            .expect_err("DB failure must fail rename");
        assert!(error.starts_with("db:"));
        for (name, bytes) in files {
            assert_eq!(
                std::fs::read(dir.join(name)).expect("old file restored"),
                bytes
            );
            assert!(
                !dir.join(name.replacen("old", "new", 1)).exists(),
                "new path must be rolled back"
            );
        }
        assert_eq!(
            db.load_task_by_id("rename-db")
                .await
                .expect("load")
                .expect("task")
                .file_name,
            "old.bin"
        );
        drop(manager);
        injector.close().await;
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning(
                "download-manager-test",
                "remove rename failure fixture",
                &error,
            );
        }
    }

    #[tokio::test]
    async fn delete_failure_keeps_record_and_never_reports_deleted() {
        let dir = unique_filetrack_test_dir("delete_failure");
        std::fs::create_dir_all(&dir).expect("create deletion fixture");
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect database");
        let mut manager = sched_manager(&db, 1);
        let mut progress = manager.take_progress_rx().expect("progress receiver");
        std::fs::create_dir_all(dir.join("single")).expect("create undeletable file target");
        std::fs::write(dir.join("single").join("payload"), b"keep").expect("write payload");
        insert_task_at_status(&db, "single", &dir.to_string_lossy(), "single", 3).await;
        manager.delete_task("single", true).await;
        assert!(
            db.load_task_by_id("single").await.expect("load").is_some(),
            "failed cleanup must retain the task for retry"
        );
        assert_eq!(
            std::fs::read(dir.join("single").join("payload")).expect("payload preserved"),
            b"keep"
        );
        assert!(
            matches!(progress.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "failed deletion must not emit a deleted frame"
        );

        // 批量：清理失败的任务单独保留，其余照常删除，删除通知只含真正删掉的任务。
        std::fs::create_dir_all(dir.join("batch-bad")).expect("create undeletable file target");
        std::fs::write(dir.join("batch-bad").join("payload"), b"keep").expect("write payload");
        std::fs::write(dir.join("batch-ok"), b"product").expect("write product");
        for id in ["batch-bad", "batch-ok"] {
            insert_task_at_status(&db, id, &dir.to_string_lossy(), id, 3).await;
        }
        let outcome = manager
            .delete_tasks_batch(&["batch-bad".to_owned(), "batch-ok".to_owned()], true)
            .await
            .expect("batch delete");
        assert_eq!(
            outcome,
            BatchDeleteOutcome {
                deleted: 1,
                failed: 1
            }
        );
        assert!(
            db.load_task_by_id("batch-bad")
                .await
                .expect("load")
                .is_some()
        );
        assert!(
            db.load_task_by_id("batch-ok")
                .await
                .expect("load")
                .is_none()
        );
        assert!(!dir.join("batch-ok").exists(), "completed product removed");
        assert_eq!(
            std::fs::read(dir.join("batch-bad").join("payload")).expect("payload preserved"),
            b"keep"
        );
        let notice = progress.try_recv().expect("one batch deletion notice");
        assert_eq!(notice.deleted_tasks, ["batch-ok"]);
        assert!(matches!(
            progress.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(manager);
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning(
                "download-manager-test",
                "remove deletion failure fixture",
                &error,
            );
        }
    }

    #[tokio::test]
    async fn start_name_failure_releases_reservation_and_reports_error() {
        let (dir, db, injector) = sqlite_fault_fixture("start_name_failure").await;
        std::fs::write(dir.join("old.bin"), b"existing data")
            .expect("write conflicting final file");
        sqlx::query("CREATE TRIGGER reject_start_name BEFORE UPDATE OF file_name ON tasks BEGIN SELECT RAISE(FAIL, 'start name write rejected'); END")
            .execute(&injector).await.expect("install start failure");
        let mut manager = sched_manager(&db, 1);
        let mut done = manager.take_done_rx().expect("done receiver");
        let mut progress = manager.take_progress_rx().expect("progress receiver");
        let id = manager
            .create_task(NewTaskSpec {
                url: "http://127.0.0.1:1/old.bin".to_string(),
                save_dir: dir.to_string_lossy().into_owned(),
                file_name: "old.bin".to_string(),
                hint_file_size: -1,
                ..Default::default()
            })
            .await
            .expect("task row creation succeeds");
        let finished = tokio::time::timeout(std::time::Duration::from_secs(2), done.recv())
            .await
            .expect("failed prelude must terminate")
            .expect("done frame");
        assert_eq!(finished.task_id, id);
        assert!(finished.reserved_temp_path.is_none());
        assert!(
            lock_reserved(&manager.reserved_temp_paths).is_empty(),
            "failed name persistence must release its reservation"
        );
        let frame = progress.recv().await.expect("failure progress");
        assert_eq!(frame.status, 4);
        assert!(frame.error_message.contains("start name write rejected"));
        assert_eq!(
            std::fs::read(dir.join("old.bin")).expect("read existing final"),
            b"existing data"
        );
        let task = db.load_task_by_id(&id).await.expect("load").expect("task");
        assert_eq!(task.status, 4);
        assert_eq!(task.file_name, "old.bin");
        manager.on_task_done(&finished).await;
        manager.shutdown().await;
        drop(manager);
        injector.close().await;
        drop(db);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning(
                "download-manager-test",
                "remove start failure fixture",
                &error,
            );
        }
    }

    /// 在 `queue_id` 队列里插入一个 paused 任务（指向必然拒绝连接的本地端口）。
    async fn insert_paused_task_in_queue(db: &Db, id: &str, queue_id: &str) {
        db.insert_task(
            id,
            "http://127.0.0.1:1/file.bin",
            "file.bin",
            "/tmp",
            1,
            0,
            "",
            queue_id,
            "",
            0,
        )
        .await
        .expect("insert task");
        db.update_task_status(id, 2, "").await.expect("pause task");
    }

    /// 占一个并发槽的假活跃条目（带一个等待取消的 handle，模拟真实 spawn）。
    fn occupy_slot(
        mgr: &mut DownloadManager,
        id: &str,
        generation: u64,
        queue_id: &str,
        is_bt: bool,
    ) {
        let token = CancellationToken::new();
        let waiter = token.clone();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
        });
        mgr.active_tasks.insert(
            id.to_string(),
            ActiveTaskEntry {
                token,
                generation,
                handle: Some(handle),
                is_bt,
                queue_id: queue_id.to_string(),
            },
        );
    }

    fn cancel_all_active(mgr: &DownloadManager) {
        for entry in mgr.active_tasks.values() {
            entry.token.cancel();
        }
    }

    /// flush 窗口内「暂停→恢复→暂停」：最后一次意图是暂停，挂起的恢复请求必须撤销，
    /// 否则旧 spawn 的 TaskDone 到达后任务会自己重新开始下载。
    #[tokio::test]
    async fn repeated_pause_cancels_pending_resume_request() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        db.update_task_status("a", 1, "").await.expect("active");
        let mut mgr = sched_manager(&db, 2);
        occupy_slot(&mut mgr, "a", 3, "", false);

        mgr.pause_task("a").await;
        mgr.resume_task("a").await;
        assert!(
            mgr.pending_pauses
                .get("a")
                .is_some_and(|p| p.resume_requested),
            "flush 窗口内的恢复应被挂起"
        );
        mgr.pause_task("a").await;
        assert!(
            mgr.pending_pauses
                .get("a")
                .is_some_and(|p| !p.resume_requested),
            "再次暂停必须撤销挂起的恢复请求"
        );
    }

    /// 下载器已写入完成态、TaskDone 尚未消费时，暂停/取消不得把成品改写成
    /// paused/cancelled，也不得摘掉 active 条目（完成收尾要靠它的世代匹配）。
    #[tokio::test]
    async fn pause_and_cancel_do_not_clobber_completed_task() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "done", "").await;
        db.update_task_status("done", 3, "")
            .await
            .expect("complete");
        let mut mgr = sched_manager(&db, 2);
        occupy_slot(&mut mgr, "done", 5, "", false);

        mgr.pause_task("done").await;
        assert!(mgr.active_tasks.contains_key("done"));
        assert!(mgr.pending_pauses.is_empty());
        let row = db
            .load_task_by_id("done")
            .await
            .expect("load")
            .expect("row");
        assert_eq!(row.status, 3);

        mgr.cancel_task("done").await;
        assert!(mgr.active_tasks.contains_key("done"));
        let row = db
            .load_task_by_id("done")
            .await
            .expect("load")
            .expect("row");
        assert_eq!(row.status, 3, "cancel 不得覆盖完成态");
        cancel_all_active(&mgr);
    }

    /// Boost 期间的批量暂停：全局暂停（batch_pause_all）不得恢复让位任务；
    /// 停止队列式批量暂停只恢复所在队列仍在运行的让位任务。
    #[tokio::test]
    async fn batch_pause_with_boost_does_not_resurrect_auto_paused_tasks() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        for id in ["boosted", "p_main"] {
            insert_paused_task_in_queue(&db, id, "").await;
        }
        insert_paused_task_in_queue(&db, "p_later", "later").await;
        db.update_task_status("boosted", 1, "")
            .await
            .expect("active");

        // 全局暂停：让位任务保持暂停。
        let mut mgr = sched_manager(&db, 4);
        mgr.load_queues().await;
        occupy_slot(&mut mgr, "boosted", 1, "", false);
        mgr.priority_task_id = Some("boosted".into());
        mgr.record_auto_paused("p_main");
        mgr.record_auto_paused("p_later");
        mgr.batch_pause_all(&["boosted".to_string()]).await;
        assert!(mgr.priority_task_id.is_none());
        assert!(mgr.auto_paused_ids.is_empty());
        assert!(mgr.active_tasks.is_empty(), "全部暂停后不应有任务被拉起");
        assert!(mgr.pending_queue.is_empty());

        // 停止队列式暂停：运行中队列的让位任务恢复，已停止队列的不恢复。
        mgr.pending_pauses.clear();
        db.update_task_status("boosted", 1, "")
            .await
            .expect("active");
        occupy_slot(&mut mgr, "boosted", 2, "", false);
        mgr.priority_task_id = Some("boosted".into());
        mgr.record_auto_paused("p_main");
        mgr.record_auto_paused("p_later");
        if let Some(q) = mgr.queues.get_mut("later") {
            q.is_running = false;
        }
        mgr.batch_pause(&["boosted".to_string()]).await;
        assert!(mgr.priority_task_id.is_none());
        assert!(
            mgr.active_tasks.contains_key("p_main")
                || mgr.pending_queue.iter().any(|q| q.task_id == "p_main"),
            "运行中队列的让位任务应恢复"
        );
        assert!(!mgr.active_tasks.contains_key("p_later"));
        assert!(!mgr.pending_queue.iter().any(|q| q.task_id == "p_later"));
        cancel_all_active(&mgr);
    }

    /// 已完成 / 失败 / 不存在的任务不能设为优先：直接拒绝，绝不为它们暂停其他任务。
    #[tokio::test]
    async fn boost_rejects_ended_tasks_without_pausing_others() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        for id in ["done", "failed", "running"] {
            insert_paused_task_in_queue(&db, id, "").await;
        }
        db.update_task_status("done", 3, "").await.expect("done");
        db.update_task_status("failed", 4, "boom")
            .await
            .expect("failed");
        db.update_task_status("running", 1, "")
            .await
            .expect("running");
        let mut mgr = sched_manager(&db, 4);
        mgr.load_queues().await;
        occupy_slot(&mut mgr, "running", 1, "", false);

        assert!(matches!(
            mgr.set_priority_task("done".into()).await,
            Err(BoostError::Completed)
        ));
        assert!(matches!(
            mgr.set_priority_task("failed".into()).await,
            Err(BoostError::Ended)
        ));
        assert!(matches!(
            mgr.set_priority_task("missing".into()).await,
            Err(BoostError::NotFound)
        ));
        assert!(mgr.priority_task_id.is_none());
        assert!(mgr.auto_paused_ids.is_empty());
        assert!(
            mgr.active_tasks.contains_key("running"),
            "被拒时不得暂停其他任务"
        );
        cancel_all_active(&mgr);
    }

    /// 优先任务经非常规路径离场（「文件已存在」询问被取消 → 暂停）时 Boost 也必须结束，
    /// 让位任务恢复，不能残留「取消优先」。
    #[tokio::test]
    async fn boost_ends_when_priority_task_leaves_through_decision_cancel() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        for id in ["boosted", "yielded"] {
            insert_paused_task_in_queue(&db, id, "").await;
        }
        let mut mgr = sched_manager(&db, 4);
        mgr.load_queues().await;
        occupy_slot(&mut mgr, "boosted", 3, "", false);
        mgr.awaiting_decision.insert("boosted".into(), 3);
        mgr.priority_task_id = Some("boosted".into());
        mgr.record_auto_paused("yielded");

        mgr.on_task_done(&TaskDone {
            task_id: "boosted".into(),
            generation: 3,
            reserved_temp_path: None,
            phase: TaskDonePhase::DecisionReady(crate::selection::FileExistsChoice::Cancel),
        })
        .await;

        assert!(mgr.priority_task_id.is_none());
        assert!(mgr.auto_paused_ids.is_empty());
        assert!(
            mgr.active_tasks.contains_key("yielded")
                || mgr.pending_queue.iter().any(|q| q.task_id == "yielded"),
            "让位任务应在 Boost 结束时恢复"
        );
        cancel_all_active(&mgr);
    }

    /// daemon 重启：开启启动恢复时 Boost 连同让位登记一并恢复；关闭时 Boost 结束，
    /// 让位任务与被中断任务同等保持暂停。持久化状态读取后即清除。
    #[tokio::test]
    async fn boost_survives_restart_per_auto_resume_setting() {
        for auto_resume in [true, false] {
            let db = Db::connect("sqlite::memory:")
                .await
                .expect("connect mem db");
            db.seed_builtin_queues().await.expect("seed");
            for id in ["boosted", "yielded"] {
                insert_paused_task_in_queue(&db, id, "").await;
                db.update_task_status(id, 1, "").await.expect("running");
            }
            let mut before = sched_manager(&db, 4);
            before.load_queues().await;
            occupy_slot(&mut before, "boosted", 1, "", false);
            occupy_slot(&mut before, "yielded", 2, "", false);
            before
                .set_priority_task("boosted".into())
                .await
                .expect("boost");
            assert_eq!(before.auto_paused_ids, vec!["yielded".to_string()]);
            cancel_all_active(&before);
            drop(before);

            db.set_config(
                "auto_resume_on_start",
                if auto_resume { "true" } else { "false" },
            )
            .await
            .expect("config");
            let reset = db.reset_incomplete_tasks_to_paused().await.expect("reset");
            assert_eq!(reset, vec!["boosted".to_string()]);
            let mut after = sched_manager(&db, 4);
            after.load_queues().await;
            after.resume_on_start(reset).await;

            if auto_resume {
                assert_eq!(after.priority_task_id.as_deref(), Some("boosted"));
                assert_eq!(after.auto_paused_ids, vec!["yielded".to_string()]);
                assert!(after.active_tasks.contains_key("boosted"));
                assert!(!after.active_tasks.contains_key("yielded"));
                assert!(
                    db.get_config("boost_state").await.expect("read").is_some(),
                    "恢复后的 Boost 重新持久化"
                );
            } else {
                assert!(after.priority_task_id.is_none());
                assert!(after.active_tasks.is_empty());
                assert!(db.get_config("boost_state").await.expect("read").is_none());
            }
            cancel_all_active(&after);
        }
    }

    /// 批量恢复也要清掉已排程的自动重试占位，否则队列永远被算作占用。
    #[tokio::test]
    async fn batch_resume_clears_retry_scheduled() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        let mut mgr = sched_manager(&db, 1);
        mgr.retry_scheduled.insert("a".into(), String::new());
        mgr.batch_resume(&["a".to_string()]).await;
        assert!(mgr.retry_scheduled.is_empty());
        cancel_all_active(&mgr);
    }

    /// #935：完成态任务的「继续」一律忽略——单任务、批量（与可恢复任务混选）都不得
    /// 把它送进下载流水线（会从头重下），状态与进度保持不变；同批的暂停任务照常恢复。
    #[tokio::test]
    async fn resume_ignores_completed_tasks() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "done", "").await;
        db.update_task_progress("done", 1024)
            .await
            .expect("progress");
        db.update_task_status("done", 3, "")
            .await
            .expect("complete");
        insert_paused_task_in_queue(&db, "paused", "").await;
        let mut mgr = sched_manager(&db, 4);

        mgr.resume_task("done").await;
        mgr.batch_resume(&["done".to_string(), "paused".to_string()])
            .await;

        assert!(!mgr.active_tasks.contains_key("done"));
        assert!(mgr.pending_queue.iter().all(|q| q.task_id != "done"));
        let done = db
            .load_task_by_id("done")
            .await
            .expect("load")
            .expect("done row");
        assert_eq!((done.status, done.downloaded_bytes), (3, 1024));
        assert!(mgr.active_tasks.contains_key("paused"));
        cancel_all_active(&mgr);
    }

    /// 完成帧尚未被 actor 消费（活跃条目仍在、DB 已是 3）时恢复：保留条目让完成收尾
    /// 照常进行，不得摘除后重新起飞；失败态（4）的同类残留仍按原语义摘除并重试。
    #[tokio::test]
    async fn resume_keeps_pending_completion_entry() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "done", "").await;
        db.update_task_status("done", 3, "")
            .await
            .expect("complete");
        insert_paused_task_in_queue(&db, "failed", "").await;
        db.update_task_status("failed", 4, "boom")
            .await
            .expect("fail");
        let mut mgr = sched_manager(&db, 4);
        occupy_slot(&mut mgr, "done", 7, "", false);
        occupy_slot(&mut mgr, "failed", 7, "", false);

        mgr.resume_task("done").await;
        mgr.batch_resume(&["done".to_string()]).await;
        mgr.resume_task("failed").await;

        assert_eq!(mgr.active_tasks.get("done").map(|e| e.generation), Some(7));
        let failed_generation = mgr.active_tasks.get("failed").map(|e| e.generation);
        assert!(failed_generation.is_some_and(|generation| generation != 7));
        cancel_all_active(&mgr);
    }

    /// 「等待重试」集合大小变化时上报 `RetryPendingChanged`，数值不变不重复发。
    #[tokio::test]
    async fn retry_pending_count_is_reported_only_on_change() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = sched_manager_with_sink(&db, 1, sink.clone());

        mgr.retry_schedule("a", String::new());
        mgr.retry_schedule("a", String::new());
        mgr.retry_schedule("b", String::new());
        mgr.retry_unschedule("a");
        mgr.retry_unschedule("a");
        mgr.retry_unschedule("b");

        let counts: Vec<u32> = sink
            .events()
            .iter()
            .filter_map(|event| match event {
                EngineEvent::RetryPendingChanged { count } => Some(*count),
                _ => None,
            })
            .collect();
        assert_eq!(counts, vec![1, 2, 1, 0]);
    }

    /// 排队期间插件被卸载（DB 绑定已清）：出队启动时按原始链接直接下载，
    /// 不再带着失效 resolver 进入 resolve 而失败。
    #[cfg(feature = "plugins")]
    #[tokio::test]
    async fn queued_task_with_uninstalled_resolver_starts_direct_download() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        let mut mgr = sched_manager(&db, 1);

        mgr.do_start_task(QueuedTask {
            task_id: "a".to_string(),
            url: "http://127.0.0.1:1/file.bin".to_string(),
            save_dir: "/tmp".to_string(),
            file_name: "file.bin".to_string(),
            segments: 0,
            is_resume: false,
            cookies: String::new(),
            referrer: String::new(),
            hint_file_size: 0,
            torrent_file_bytes: Vec::new(),
            proxy_url: String::new(),
            user_agent: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            extra_headers: std::collections::HashMap::new(),
            selected_file_indices: Vec::new(),
            method: None,
            body: None,
            audio_url: None,
            resolver_plugin_id: "gone@plugin".to_string(),
            resolved: false,
            range_supported: false,
            resolver_item: String::new(),
            source_url: String::new(),
            resolver_patch: None,
        })
        .await;

        assert!(
            !mgr.has_pending_resolve("a"),
            "an unbound resolver must not enter resolve"
        );
        assert!(mgr.active_tasks.contains_key("a"));
        cancel_all_active(&mgr);
    }

    /// resolve 窗口内插件被卸载（DB 绑定已清）：Resume 再入留下的 `resume_applied` 条目
    /// 必须被 `do_resume_task` 取走（按原始链接继续），不能滞留到下一次 resume 被误用。
    #[cfg(feature = "plugins")]
    #[tokio::test]
    async fn resume_applied_is_consumed_when_binding_was_cleared() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        let mut mgr = sched_manager(&db, 1);
        mgr.resume_applied.insert(
            "a".to_string(),
            crate::plugin::ResolveResult {
                url: "http://127.0.0.1:1/stale-resolved.bin".to_string(),
                ..Default::default()
            },
        );

        mgr.do_resume_task("a").await;

        assert!(
            !mgr.resume_applied.contains_key("a"),
            "a re-entry result must never outlive the resume that consumed it"
        );
        cancel_all_active(&mgr);
    }

    /// 队列限速改回 0：在跑任务持有的队列 limiter 必须回落到全局限速，
    /// 且之后全局限速变化也要同步过来。
    #[tokio::test]
    async fn zeroed_queue_limiter_follows_global_limit() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let mut mgr = sched_manager(&db, 1);
        mgr.set_speed_limit(5_000);
        assert_eq!(mgr.queue_limit_bps(2), 2 * 1024);
        assert_eq!(mgr.queue_limit_bps(0), 5_000);

        let mut q = sched_queue("q", true, "", "", 0x7f);
        q.speed_limit_kbps = 10;
        mgr.queues.insert("q".into(), q);
        let limiter = mgr.queue_limiter_for("q");
        assert_eq!(limiter.limit(), 10 * 1024);

        if let Some(q) = mgr.queues.get_mut("q") {
            q.speed_limit_kbps = 0;
        }
        mgr.set_speed_limit(7_000);
        assert_eq!(limiter.limit(), 7_000);
    }

    /// 启动恢复只拉起启动矫正时被复位的任务：用户手动暂停的不动，
    /// 开关关闭时完全不恢复。
    #[tokio::test]
    async fn auto_resume_on_start_resumes_only_reset_tasks() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "was_active", "").await;
        db.update_task_status("was_active", 1, "")
            .await
            .expect("status");
        insert_paused_task_in_queue(&db, "manual_paused", "").await;

        let ids = db.reset_incomplete_tasks_to_paused().await.expect("reset");
        assert_eq!(ids, vec!["was_active".to_string()]);

        let mut mgr = sched_manager(&db, 4);
        mgr.load_queues().await;
        mgr.auto_resume_on_start(ids.clone()).await;
        assert!(mgr.active_tasks.is_empty(), "开关默认关闭：不恢复");

        db.set_config("auto_resume_on_start", "true")
            .await
            .expect("config");
        mgr.auto_resume_on_start(ids).await;
        assert!(mgr.active_tasks.contains_key("was_active"));
        assert!(!mgr.active_tasks.contains_key("manual_paused"));
        cancel_all_active(&mgr);
    }

    /// 插件 requestRetry 不得在引擎已排程自动重试时再扣一次配额、再排一次恢复。
    #[cfg(feature = "plugins")]
    #[tokio::test]
    async fn plugin_request_retry_defers_to_scheduled_engine_retry() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        db.update_task_status("a", 4, "connection reset")
            .await
            .expect("error");
        let mut mgr = sched_manager(&db, 1);

        mgr.retry_scheduled.insert("a".into(), String::new());
        mgr.plugin_request_retry("a", 10_000).await;
        assert!(!mgr.auto_retry_counts.contains_key("a"));

        mgr.retry_scheduled.clear();
        mgr.plugin_request_retry("a", 10_000).await;
        assert_eq!(mgr.auto_retry_counts.get("a"), Some(&1));
        assert!(mgr.retry_scheduled.contains_key("a"));
    }

    /// failed 阶段的插件重试不受 `max_auto_retries == 0` 门控，且走独立额度。
    #[cfg(feature = "plugins")]
    #[tokio::test]
    async fn interceptor_retry_uses_independent_quota() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        db.update_task_status("a", 4, "HTTP 403")
            .await
            .expect("error");
        let mut mgr = sched_manager(&db, 1);
        mgr.set_max_auto_retries(0);

        // 无标记（onError requestRetry）在关闭自动重试时不排程。
        mgr.plugin_request_retry("a", 10_000).await;
        assert!(!mgr.retry_scheduled.contains_key("a"));

        for round in 1..=INTERCEPT_RETRY_LIMIT {
            mgr.intercept_retry_marks
                .lock()
                .expect("marks")
                .insert("a".into());
            mgr.plugin_request_retry("a", 10_000).await;
            assert!(mgr.retry_scheduled.contains_key("a"), "第 {round} 次应排程");
            assert!(mgr.intercept_retry_marks.lock().expect("marks").is_empty());
            mgr.retry_scheduled.clear();
        }
        mgr.intercept_retry_marks
            .lock()
            .expect("marks")
            .insert("a".into());
        mgr.plugin_request_retry("a", 10_000).await;
        assert!(!mgr.retry_scheduled.contains_key("a"), "额度用尽后不再排程");
        assert!(!mgr.auto_retry_counts.contains_key("a"));
        assert_eq!(
            mgr.intercept_retry_counts.get("a"),
            Some(&INTERCEPT_RETRY_LIMIT)
        );
    }

    /// 调大队列并发上限必须立即放行被旧上限卡住的排队任务，
    /// 而不是等到别的任务结束。
    #[tokio::test]
    async fn raising_queue_limit_starts_blocked_pending_task() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        db.insert_queue("work", "Work", 0, 0, 1, "", 2, 0, "")
            .await
            .expect("insert queue");
        insert_paused_task_in_queue(&db, "p1", "work").await;
        let mut mgr = sched_manager(&db, 8);
        mgr.load_queues().await;
        occupy_slot(&mut mgr, "occupier", 1, "work", false);

        mgr.resume_task("p1").await;
        assert_eq!(mgr.pending_queue.len(), 1, "队列已满，任务必须排队");

        mgr.update_queue(
            "work".into(),
            "Work".into(),
            0,
            0,
            2,
            String::new(),
            0,
            String::new(),
        )
        .await;
        assert!(mgr.pending_queue.is_empty(), "上限调大后应立即出队");
        assert!(mgr.active_tasks.contains_key("p1"));
        cancel_all_active(&mgr);
    }

    /// 把排队任务移入有空位的队列必须立即起跑。
    #[tokio::test]
    async fn moving_pending_task_into_free_queue_starts_it() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        for id in ["full", "free"] {
            db.insert_queue(id, id, 0, 0, 1, "", 2, 0, "")
                .await
                .expect("insert queue");
        }
        insert_paused_task_in_queue(&db, "p1", "full").await;
        let mut mgr = sched_manager(&db, 8);
        mgr.load_queues().await;
        occupy_slot(&mut mgr, "occupier", 1, "full", false);
        mgr.resume_task("p1").await;
        assert_eq!(mgr.pending_queue.len(), 1);

        mgr.move_task_to_queue("p1".into(), "free".into()).await;
        assert!(mgr.pending_queue.is_empty());
        assert!(mgr.active_tasks.contains_key("p1"));
        cancel_all_active(&mgr);
    }

    /// 主队列恒运行：停止与定时（含定时 tick）都不能让它停下或暂停其中的活跃任务；
    /// 其他队列照常可停。
    #[tokio::test]
    async fn main_queue_rejects_stop_and_schedule() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        let mut mgr = sched_manager(&db, 8);
        mgr.load_queues().await;
        occupy_slot(&mut mgr, "running", 1, MAIN_QUEUE_ID, false);

        mgr.stop_queue(MAIN_QUEUE_ID.into()).await;
        mgr.set_queue_schedule(
            MAIN_QUEUE_ID.into(),
            true,
            String::new(),
            "00:00".into(),
            0x7f,
        )
        .await;
        mgr.tick_queue_schedules().await;

        let main = &mgr.queues[MAIN_QUEUE_ID];
        assert!(main.is_running && !main.schedule_enabled && main.schedule_stop.is_empty());
        assert!(mgr.active_tasks.contains_key("running"));
        let stored = db.load_all_queues().await.expect("load");
        let stored = stored
            .iter()
            .find(|queue| queue.queue_id == MAIN_QUEUE_ID)
            .expect("main row");
        assert!(stored.is_running && !stored.schedule_enabled);

        mgr.stop_queue(crate::model::LATER_QUEUE_ID.into()).await;
        mgr.start_queue(crate::model::LATER_QUEUE_ID.into()).await;
        mgr.stop_queue(crate::model::LATER_QUEUE_ID.into()).await;
        assert!(!mgr.queues[crate::model::LATER_QUEUE_ID].is_running);
        cancel_all_active(&mgr);
    }

    /// 删除队列后，迁回主队列的排队任务按主队列容量立即重新调度。
    #[tokio::test]
    async fn deleting_queue_reschedules_migrated_pending_task() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        db.insert_queue("work", "Work", 0, 0, 1, "", 2, 0, "")
            .await
            .expect("insert queue");
        insert_paused_task_in_queue(&db, "p1", "work").await;
        let mut mgr = sched_manager(&db, 8);
        mgr.load_queues().await;
        occupy_slot(&mut mgr, "occupier", 1, "work", false);
        mgr.resume_task("p1").await;
        assert_eq!(mgr.pending_queue.len(), 1);

        mgr.delete_queue("work".into()).await;
        assert!(
            mgr.pending_queue.is_empty(),
            "迁回无限制的主队列后应立即起跑"
        );
        assert!(mgr.active_tasks.contains_key("p1"));
        cancel_all_active(&mgr);
    }

    /// `max_concurrent <= 0` 一律视为不限；BT 既不占用也不受队列槽位约束。
    #[tokio::test]
    async fn queue_capacity_ignores_bt_and_treats_non_positive_as_unlimited() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        db.insert_queue("neg", "neg", 0, 0, -1, "", 2, 0, "")
            .await
            .expect("insert queue");
        db.insert_queue("one", "one", 0, 0, 1, "", 3, 0, "")
            .await
            .expect("insert queue");
        let mut mgr = sched_manager(&db, 8);
        mgr.load_queues().await;

        occupy_slot(&mut mgr, "a", 1, "neg", false);
        occupy_slot(&mut mgr, "b", 1, "neg", false);
        assert!(mgr.has_queue_capacity("neg"), "负数上限 = 不限");

        occupy_slot(&mut mgr, "bt", 1, "one", true);
        assert!(mgr.has_queue_capacity("one"), "BT 不占用队列槽位");
        occupy_slot(&mut mgr, "http", 1, "one", false);
        assert!(!mgr.has_queue_capacity("one"), "HTTP 任务仍占用队列槽位");
        cancel_all_active(&mgr);
    }

    /// 内部「暂停 + 立即恢复」（改分段数）必须保住槽位：旧下载器收尾时，
    /// 任务自己恢复，而不是槽位被排队头部抢走、自己被挂到队尾。
    #[tokio::test]
    async fn changing_segments_keeps_slot_against_pending_head() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        db.update_task_status("a", 1, "").await.expect("active");
        insert_paused_task_in_queue(&db, "b", "").await;
        let mut mgr = sched_manager(&db, 1);
        let _progress_rx = mgr.take_progress_rx().expect("progress receiver");
        occupy_slot(&mut mgr, "a", 7, "", false);
        mgr.resume_task("b").await;
        assert_eq!(mgr.pending_queue.len(), 1, "b 在队列里等待唯一槽位");

        let updated = mgr.set_task_segments("a", 4).await.expect("set segments");
        assert!(updated);
        assert_eq!(
            mgr.pending_queue
                .iter()
                .map(|q| q.task_id.as_str())
                .collect::<Vec<_>>(),
            ["b"],
            "暂停-恢复期间槽位不得被排队头部提前拿走"
        );

        mgr.on_task_done(&TaskDone {
            task_id: "a".to_string(),
            generation: 7,
            reserved_temp_path: None,
            phase: TaskDonePhase::Finished,
        })
        .await;
        assert!(
            mgr.active_tasks.contains_key("a"),
            "旧下载器退出后 a 必须自己恢复"
        );
        assert_eq!(
            mgr.pending_queue
                .iter()
                .map(|q| q.task_id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        cancel_all_active(&mgr);
    }

    /// 用户暂停仍然立即把槽位让给排队头部。
    #[tokio::test]
    async fn user_pause_still_frees_slot_immediately() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        insert_paused_task_in_queue(&db, "a", "").await;
        db.update_task_status("a", 1, "").await.expect("active");
        insert_paused_task_in_queue(&db, "b", "").await;
        let mut mgr = sched_manager(&db, 1);
        occupy_slot(&mut mgr, "a", 7, "", false);
        mgr.resume_task("b").await;
        assert_eq!(mgr.pending_queue.len(), 1);

        mgr.pause_task("a").await;
        assert!(mgr.pending_queue.is_empty());
        assert!(mgr.active_tasks.contains_key("b"));
        cancel_all_active(&mgr);
    }

    /// 取消 Boost 必须按暂停顺序恢复：先前活跃的先拿回槽位，
    /// 其余按原顺序排队，而不是 HashSet 的随机顺序。
    #[tokio::test]
    async fn clear_priority_resumes_in_pause_order() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        let ids = ["t0", "t1", "t2", "t3", "t4", "t5"];
        for id in ids {
            insert_paused_task_in_queue(&db, id, "").await;
        }
        let mut mgr = sched_manager(&db, 1);
        mgr.priority_task_id = Some("boosted".into());
        for id in ids {
            mgr.record_auto_paused(id);
        }
        mgr.record_auto_paused("t3"); // 重复登记不改变顺序、不重复

        mgr.clear_priority().await;
        assert!(
            mgr.active_tasks.contains_key("t0"),
            "最先暂停的任务先拿回槽位"
        );
        assert_eq!(
            mgr.pending_queue
                .iter()
                .map(|q| q.task_id.as_str())
                .collect::<Vec<_>>(),
            ["t1", "t2", "t3", "t4", "t5"]
        );
        assert!(mgr.auto_paused_ids.is_empty());
        cancel_all_active(&mgr);
    }

    /// 批量事件契约：批量恢复/暂停 N 个排队任务 = 常数条事件
    /// （1 × QueuePositionsChanged + 1 × TasksSnapshot），零逐任务
    /// TaskProgress；状态经批量 SQL 落库（恢复排队 → 0，暂停 → 2）。
    #[tokio::test]
    async fn batch_resume_and_pause_emit_constant_event_count() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        for id in ["q1", "q2", "q3"] {
            insert_task_at_status(&db, id, "/tmp", "f.bin", 2).await;
        }
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_string(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        // 占满唯一并发槽：三个任务全部走排队分支，不触发真实下载。
        mgr.active_tasks.insert(
            "occupier".to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 0,
                handle: None,
                is_bt: false,
                queue_id: String::new(),
            },
        );

        let count = |events: &[EngineEvent]| {
            let progress = events
                .iter()
                .filter(|e| matches!(e, EngineEvent::TaskProgress { .. }))
                .count();
            let positions = events
                .iter()
                .filter(|e| matches!(e, EngineEvent::QueuePositionsChanged(_)))
                .count();
            let snapshots = events
                .iter()
                .filter(|e| matches!(e, EngineEvent::TasksSnapshot(_)))
                .count();
            (progress, positions, snapshots)
        };

        let ids: Vec<String> = ["q1", "q2", "q3"].iter().map(|s| s.to_string()).collect();
        mgr.batch_resume(&ids).await;

        let (progress, positions, snapshots) = count(&sink.events());
        assert_eq!(progress, 0, "批量恢复不得逐任务发 TaskProgress");
        assert_eq!(positions, 1, "批量恢复只广播一次队列位置");
        assert_eq!(snapshots, 1, "批量恢复只广播一次任务快照");
        assert_eq!(mgr.pending_queue.len(), 3);
        for id in ["q1", "q2", "q3"] {
            let t = db.load_task_by_id(id).await.expect("load").expect("task");
            assert_eq!(t.status, 0, "排队任务必须批量持久化为 pending");
        }

        // 批量暂停：同样常数条事件，排队任务全部摘除并落库 paused。
        mgr.batch_pause(&ids).await;

        let (progress, positions, snapshots) = count(&sink.events());
        assert_eq!(progress, 0, "批量暂停排队任务不得逐任务发 TaskProgress");
        assert_eq!(positions, 2, "批量暂停只追加一次队列位置广播");
        assert_eq!(snapshots, 2, "批量暂停只追加一次任务快照");
        assert!(mgr.pending_queue.is_empty());
        for id in ["q1", "q2", "q3"] {
            let t = db.load_task_by_id(id).await.expect("load").expect("task");
            assert_eq!(t.status, 2, "排队任务必须批量持久化为 paused");
        }
    }

    #[tokio::test]
    async fn deleting_tasks_broadcasts_only_affected_rss_sources_after_commit() {
        use crate::rss::model::{RssItemInfo, RssItemStatus, RssSourceInfo};

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let save_dir = unique_dedup_dir("rss-deletion-events");
        let save_dir = save_dir.to_str().expect("temporary path is UTF-8");
        for (source_id, task_id) in [("first", "t1"), ("second", "t2"), ("unaffected", "other")] {
            db.insert_rss_source(&RssSourceInfo {
                source_id: source_id.into(),
                ..Default::default()
            })
            .await
            .expect("insert source");
            db.insert_rss_items(&[RssItemInfo {
                source_id: source_id.into(),
                guid: "episode".into(),
                status: RssItemStatus::Downloaded,
                task_id: task_id.into(),
                ..Default::default()
            }])
            .await
            .expect("insert item");
        }
        for task_id in ["t1", "t2"] {
            db.insert_task(
                task_id,
                "https://feed.test/file",
                "file",
                save_dir,
                3,
                0,
                "",
                "",
                "",
                0,
            )
            .await
            .expect("insert task");
        }
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.into(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        mgr.delete_task("t1", false).await;
        let first = sink.events();
        let items: Vec<_> = first
            .iter()
            .filter_map(|event| match event {
                EngineEvent::RssItemsChanged {
                    source_id,
                    items,
                    notify_titles,
                } => {
                    assert!(notify_titles.is_empty());
                    assert_eq!(items[0].status, RssItemStatus::Ignored);
                    assert!(items[0].task_id.is_empty());
                    Some(source_id.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(items, ["first"]);
        assert_eq!(
            first
                .iter()
                .filter(|event| matches!(event, EngineEvent::RssSourcesChanged(_)))
                .count(),
            1
        );

        let outcome = mgr
            .delete_tasks_batch(&["t2".into()], false)
            .await
            .expect("batch delete");
        assert_eq!(outcome.deleted, 1);
        let events = sink.events();
        let second = &events[first.len()..];
        let items: Vec<_> = second
            .iter()
            .filter_map(|event| match event {
                EngineEvent::RssItemsChanged {
                    source_id, items, ..
                } => {
                    assert_eq!(items[0].status, RssItemStatus::Ignored);
                    Some(source_id.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(items, ["second"]);
        assert_eq!(
            second
                .iter()
                .filter(|event| matches!(event, EngineEvent::RssSourcesChanged(_)))
                .count(),
            1
        );
    }

    /// 删除队列时 DB 重归属任务，事件流必须把受影响的任务定向迁回主队列。
    #[tokio::test]
    async fn delete_queue_emits_task_migrations_before_queue_list() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.seed_builtin_queues().await.expect("seed");
        db.insert_queue("work", "Work", 0, 0, 0, "", 2, 0, "")
            .await
            .expect("insert queue");
        for id in ["a", "b"] {
            db.insert_task(
                id,
                "http://example.com/file",
                "file",
                "/tmp",
                0,
                0,
                "",
                "work",
                "",
                0,
            )
            .await
            .expect("insert task");
        }
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_owned(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        mgr.delete_queue("work".to_owned()).await;
        let events = sink.events();
        let mut moved: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::TaskQueueChanged { task_id, queue_id }
                    if queue_id == MAIN_QUEUE_ID =>
                {
                    Some(task_id.as_str())
                }
                _ => None,
            })
            .collect();
        moved.sort_unstable();
        assert_eq!(moved, ["a", "b"]);
        let migration_end = events
            .iter()
            .rposition(|event| matches!(event, EngineEvent::TaskQueueChanged { .. }))
            .expect("migration event");
        // 队列顺序无变化（此处队列始终为空）时不再发布位置事件；其余事件的相对顺序不变。
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, EngineEvent::QueuePositionsChanged(_)))
        );
        let snapshot_index = events
            .iter()
            .position(|event| matches!(event, EngineEvent::TasksSnapshot(_)))
            .expect("task snapshot");
        let queues_index = events
            .iter()
            .position(|event| matches!(event, EngineEvent::QueuesChanged(_)))
            .expect("queue list");
        assert!(migration_end < snapshot_index && snapshot_index < queues_index);
        let EngineEvent::TasksSnapshot(tasks) = &events[snapshot_index] else {
            panic!("task snapshot missing")
        };
        for id in ["a", "b"] {
            let task = tasks
                .iter()
                .find(|task| task.task_id == id)
                .expect("migrated task");
            assert_eq!(task.queue_id, MAIN_QUEUE_ID);
            assert_eq!(task.queue_order, 0, "snapshot must carry DB-reset order");
        }
        for id in ["a", "b"] {
            assert_eq!(
                db.load_task_by_id(id)
                    .await
                    .expect("load")
                    .expect("task")
                    .queue_id,
                MAIN_QUEUE_ID,
            );
        }
    }

    /// 新任务第一条事实携带完整持久化字段，队列筛选不再等待全表补帧。
    #[tokio::test]
    async fn create_task_emits_complete_fact_before_progress() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_string(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        // 占满唯一并发槽：新任务走排队分支，不触发真实下载。
        mgr.active_tasks.insert(
            "occupier".to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 0,
                handle: None,
                is_bt: false,
                queue_id: String::new(),
            },
        );

        let id = mgr
            .create_task(NewTaskSpec {
                url: "http://example.com/file.bin".to_string(),
                save_dir: "/tmp".to_string(),
                file_name: "file.bin".to_string(),
                hint_file_size: 1,
                ..Default::default()
            })
            .await
            .expect("create task");

        let events = sink.events();
        let EngineEvent::TaskChanged(task) = events.first().expect("created event") else {
            panic!("first creation event must be the complete task fact");
        };
        assert_eq!(task.task_id, id);
        assert_eq!(task.queue_id, MAIN_QUEUE_ID);
        assert_eq!(
            task.as_ref(),
            &mgr.db
                .load_task_by_id(&id)
                .await
                .expect("read task")
                .expect("task exists")
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            EngineEvent::TasksSnapshot(_) | EngineEvent::TaskQueueChanged { .. }
        )));
    }

    /// dedup_same_url 开启时，活跃任务的重复 URL 直接返回既有任务 id，不建
    /// 新任务；关闭（默认）时行为不变，允许重复。
    #[tokio::test]
    async fn create_task_dedup_same_url_returns_existing_active_task() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.set_config("dedup_same_url", "true")
            .await
            .expect("set config");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_string(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        // 占满唯一并发槽：两次 create_task 都走排队分支，不触发真实下载。
        mgr.active_tasks.insert(
            "occupier".to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 0,
                handle: None,
                is_bt: false,
                queue_id: String::new(),
            },
        );

        let first_id = mgr
            .create_task(NewTaskSpec {
                url: "http://example.com/dup.bin#ignored-fragment".to_string(),
                save_dir: "/tmp".to_string(),
                file_name: "dup.bin".to_string(),
                ..Default::default()
            })
            .await
            .expect("create first task");

        // 同一 URL（fragment 不同）第二次创建：应返回同一个任务 id，DB 里
        // 仍然只有一条任务记录。
        let second_id = mgr
            .create_task(NewTaskSpec {
                url: "http://example.com/dup.bin#other-fragment".to_string(),
                save_dir: "/tmp".to_string(),
                file_name: "dup.bin".to_string(),
                ..Default::default()
            })
            .await
            .expect("dedup should return existing task id");
        assert_eq!(first_id, second_id);

        let all_tasks = mgr.db.load_all_tasks().await.expect("load tasks");
        assert_eq!(
            all_tasks.len(),
            1,
            "dedup_same_url 必须阻止建第二条重复任务记录"
        );
    }

    #[tokio::test]
    async fn create_task_without_dedup_config_allows_duplicate_urls() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        // dedup_same_url 未设置 → 默认关闭，保持历史行为。
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db,
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: "/tmp".to_string(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");
        mgr.active_tasks.insert(
            "occupier".to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 0,
                handle: None,
                is_bt: false,
                queue_id: String::new(),
            },
        );

        let first_id = mgr
            .create_task(NewTaskSpec {
                url: "http://example.com/dup2.bin".to_string(),
                save_dir: "/tmp".to_string(),
                file_name: "dup2.bin".to_string(),
                ..Default::default()
            })
            .await
            .expect("create first task");
        let second_id = mgr
            .create_task(NewTaskSpec {
                url: "http://example.com/dup2.bin".to_string(),
                save_dir: "/tmp".to_string(),
                file_name: "dup2.bin".to_string(),
                ..Default::default()
            })
            .await
            .expect("create second task");
        assert_ne!(first_id, second_id);
        let all_tasks = mgr.db.load_all_tasks().await.expect("load tasks");
        assert_eq!(all_tasks.len(), 2);
    }

    /// 重命名核心契约：最终文件与 `.fdownloading` 临时文件随 DB `file_name`
    /// 一并迁移；成功后广播一次任务快照。
    #[tokio::test]
    async fn rename_task_moves_files_and_updates_db() {
        let dir = unique_filetrack_test_dir("rename_ok");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        std::fs::write(dir.join("old.bin"), b"data").expect("write final");
        std::fs::write(
            dir.join(format!("old2.bin{}", downloader::TEMP_EXT)),
            b"partial",
        )
        .expect("write temp");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "r-done", &save_dir, "old.bin", 3).await;
        insert_task_at_status(&db, "r-paused", &save_dir, "old2.bin", 2).await;
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        // 完成任务：最终文件迁移。
        mgr.rename_task("r-done", "new.bin").await.expect("rename");
        assert!(!dir.join("old.bin").exists(), "old final must be gone");
        assert!(dir.join("new.bin").exists(), "new final must exist");
        let t = db
            .load_task_by_id("r-done")
            .await
            .expect("load")
            .expect("task");
        assert_eq!(t.file_name, "new.bin");
        assert!(
            sink.events()
                .iter()
                .any(|e| matches!(e, EngineEvent::TasksSnapshot(_))),
            "successful rename must broadcast a tasks snapshot"
        );

        // 暂停任务：`.fdownloading` 临时文件迁移，续传路径按新名重建。
        mgr.rename_task("r-paused", "new2.bin")
            .await
            .expect("rename paused");
        assert!(
            dir.join(format!("new2.bin{}", downloader::TEMP_EXT))
                .exists(),
            "temp file must follow the rename"
        );
        let t = db
            .load_task_by_id("r-paused")
            .await
            .expect("load")
            .expect("task");
        assert_eq!(t.file_name, "new2.bin");

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// 重命名拒绝面：目标名被占、活跃任务、BT 任务、非法名——全部返回
    /// 稳定错误码且不落任何 DB/磁盘变更。
    #[tokio::test]
    async fn rename_task_rejects_conflict_active_bt_and_invalid() {
        let dir = unique_filetrack_test_dir("rename_reject");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        std::fs::write(dir.join("a.bin"), b"a").expect("write a");
        std::fs::write(dir.join("taken.bin"), b"x").expect("write taken");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "r1", &save_dir, "a.bin", 3).await;
        db.insert_task(
            "r-bt",
            "magnet:?xt=urn:btih:0000000000000000000000000000000000000000",
            "bt-name",
            &save_dir,
            1,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert bt task");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        assert_eq!(
            mgr.rename_task("r1", "taken.bin").await,
            Err("target-exists".to_string())
        );
        assert_eq!(
            mgr.rename_task("r1", "../escape.bin").await,
            Err("invalid-name".to_string())
        );
        // 下载器续传按 `sanitize_filename` 重建临时文件路径：清洗会改写的名字一律拒绝，
        // 否则改名后续传找不到 `.fdownloading`，进度归零并留下孤儿临时文件。
        let too_long = format!("{}.bin", "a".repeat(200));
        for name in [
            "a:b.bin",
            "what?.bin",
            "CON.txt",
            "name.",
            "a/b.bin",
            "a\u{202E}nib.exe",
            too_long.as_str(),
        ] {
            assert_eq!(
                mgr.rename_task("r1", name).await,
                Err("invalid-name".to_string()),
                "{name:?}"
            );
        }
        assert!(
            dir.join("a.bin").exists(),
            "rejected names leave the file alone"
        );
        assert_eq!(
            mgr.rename_task("r-bt", "renamed").await,
            Err("bt-unsupported".to_string())
        );
        assert_eq!(
            mgr.rename_task("ghost", "x.bin").await,
            Err("not-found".to_string())
        );
        db.save_audio_url("r1", "https://example.com/audio.m4a")
            .await
            .expect("save audio URL");
        std::fs::write(dir.join("a.audio.m4a"), b"audio").expect("write audio");
        std::fs::create_dir_all(dir.join("sidecar.audio.m4a")).expect("block sidecar target");
        assert!(
            mgr.rename_task("r1", "sidecar.bin").await.is_err(),
            "sidecar conflict must fail before changing the main file"
        );
        assert!(
            dir.join("a.bin").exists(),
            "main file must retain its old name on failure"
        );
        assert!(!dir.join("sidecar.bin").exists());
        assert_eq!(
            db.load_task_by_id("r1")
                .await
                .expect("load")
                .expect("task")
                .file_name,
            "a.bin"
        );
        mgr.active_tasks.insert(
            "r1".to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 0,
                handle: None,
                is_bt: false,
                queue_id: String::new(),
            },
        );
        assert_eq!(
            mgr.rename_task("r1", "b.bin").await,
            Err("task-active".to_string())
        );
        // 全部被拒：原文件原名保持不变。
        assert!(dir.join("a.bin").exists());
        let t = db.load_task_by_id("r1").await.expect("load").expect("task");
        assert_eq!(t.file_name, "a.bin");

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// 换源成功面：暂停/错误任务可换同协议新地址，进度/分段不受影响；
    /// error 任务的历史 `error_message` 被清空但 status 不变（留给调用方
    /// 另行触发续传/重试）。thunder:// 新地址先解出真实地址再落库。
    #[tokio::test]
    async fn change_task_url_updates_url_and_clears_error_message() {
        let dir = unique_filetrack_test_dir("change_url_ok");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "u-paused", &save_dir, "a.bin", 2).await;
        insert_task_at_status(&db, "u-error", &save_dir, "b.bin", 4).await;
        db.update_task_status("u-error", 4, "boom")
            .await
            .expect("set error message");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink,
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        mgr.change_task_url("u-paused", "http://example.com/new.bin")
            .await
            .expect("change paused task url");
        let t = db
            .load_task_by_id("u-paused")
            .await
            .expect("load")
            .expect("task");
        assert_eq!(t.url, "http://example.com/new.bin");
        assert_eq!(t.status, 2, "paused status must be left untouched");

        // thunder:// 新地址：落库的是解出的真实地址，且历史错误信息被清空。
        let thunder_url = "thunder://QUFodHRwOi8vZXhhbXBsZS5jb20vZmlsZS56aXBaWg==";
        mgr.change_task_url("u-error", thunder_url)
            .await
            .expect("change error task url via thunder link");
        let t = db
            .load_task_by_id("u-error")
            .await
            .expect("load")
            .expect("task");
        assert_eq!(t.url, "http://example.com/file.zip");
        assert_eq!(t.status, 4, "error status must be left untouched");
        assert!(
            t.error_message.is_empty(),
            "historical error_message must be cleared"
        );

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// 换源拒绝面：活跃/完成/BT 任务、跨协议换源、空地址——全部返回稳定
    /// 错误码且不落库。
    #[tokio::test]
    async fn change_task_url_rejects_active_completed_bt_and_protocol_mismatch() {
        let dir = unique_filetrack_test_dir("change_url_reject");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "c1", &save_dir, "a.bin", 2).await;
        insert_task_at_status(&db, "c-done", &save_dir, "b.bin", 3).await;
        db.insert_task(
            "c-bt",
            "magnet:?xt=urn:btih:0000000000000000000000000000000000000000",
            "bt-name",
            &save_dir,
            1,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert bt task");
        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink,
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        assert_eq!(
            mgr.change_task_url("c-done", "http://example.com/new.bin")
                .await,
            Err("task-completed".to_string())
        );
        assert_eq!(
            mgr.change_task_url("c-bt", "http://example.com/new.bin")
                .await,
            Err("bt-unsupported".to_string())
        );
        assert_eq!(
            mgr.change_task_url("c1", "ftp://example.com/new.bin").await,
            Err("protocol-mismatch".to_string())
        );
        assert_eq!(
            mgr.change_task_url("c1", "  ").await,
            Err("invalid-url".to_string())
        );
        assert_eq!(
            mgr.change_task_url("ghost", "http://example.com/new.bin")
                .await,
            Err("not-found".to_string())
        );
        mgr.active_tasks.insert(
            "c1".to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 0,
                handle: None,
                is_bt: false,
                queue_id: String::new(),
            },
        );
        assert_eq!(
            mgr.change_task_url("c1", "http://example.com/new.bin")
                .await,
            Err("task-active".to_string())
        );
        // 全部被拒：原地址保持不变。
        let t = db.load_task_by_id("c1").await.expect("load").expect("task");
        assert_eq!(t.url, "http://example.com/file.bin");

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// FluxDown #11 核心契约：completed 任务的目标文件消失后 `file_missing`
    /// 落库为 true 并定向上报 `FileMissingChanged`；文件移回后无棘轮地翻回
    /// false 并再次上报（双向自愈）。文件仍存在时不落库变化、不发事件。
    #[tokio::test]
    async fn scan_missing_files_round_trip_self_heals_when_file_returns() {
        let dir = unique_filetrack_test_dir("scan_roundtrip");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let file_name = "movie.mp4";
        let file_path = dir.join(file_name);
        std::fs::write(&file_path, b"data").expect("write test file");
        let save_dir = dir.to_string_lossy().to_string();

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "t-roundtrip", &save_dir, file_name, 3).await;

        let sink = Arc::new(RecordingSink::new());

        // (a) 文件仍在：不落库变化、不发事件。
        scan_missing_files(
            db.clone(),
            sink.clone(),
            Arc::new(AtomicBool::new(false)),
            false,
            noop_cleanup_tx(),
        )
        .await;
        let task = db
            .load_task_by_id("t-roundtrip")
            .await
            .expect("load")
            .expect("task present");
        assert!(
            !task.file_missing,
            "file_missing must stay false while the file exists"
        );
        assert!(
            sink.events().is_empty(),
            "no-change scan must not emit FileMissingChanged"
        );

        // (b) 文件被删：翻为 true，发一次事件。
        std::fs::remove_file(&file_path).expect("delete test file");
        scan_missing_files(
            db.clone(),
            sink.clone(),
            Arc::new(AtomicBool::new(false)),
            false,
            noop_cleanup_tx(),
        )
        .await;
        let task = db
            .load_task_by_id("t-roundtrip")
            .await
            .expect("load")
            .expect("task present");
        assert!(
            task.file_missing,
            "file_missing must flip true once the file disappears"
        );
        let events = sink.events();
        assert_eq!(
            events.len(),
            1,
            "exactly one FileMissingChanged expected after deletion"
        );
        match &events[0] {
            EngineEvent::FileMissingChanged(changes) => {
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0], ("t-roundtrip".to_string(), true));
            }
            other => panic!("expected FileMissingChanged(true), got {other:?}"),
        }

        // (c) 文件移回：翻回 false，再发一次事件（双向自愈，无棘轮）。
        std::fs::write(&file_path, b"data").expect("recreate test file");
        scan_missing_files(
            db.clone(),
            sink.clone(),
            Arc::new(AtomicBool::new(false)),
            false,
            noop_cleanup_tx(),
        )
        .await;
        let task = db
            .load_task_by_id("t-roundtrip")
            .await
            .expect("load")
            .expect("task present");
        assert!(
            !task.file_missing,
            "file_missing must self-heal back to false once the file returns"
        );
        let events = sink.events();
        assert_eq!(
            events.len(),
            2,
            "second FileMissingChanged expected after the file returns"
        );
        match &events[1] {
            EngineEvent::FileMissingChanged(changes) => {
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0], ("t-roundtrip".to_string(), false));
            }
            other => panic!("expected FileMissingChanged(false), got {other:?}"),
        }

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// R7 回归：非 completed 任务（status=1，下载中）即便目标文件不存在也
    /// 绝不能被文件跟踪标记——下载中的文件本就还没落地，误标会在 UI 上产生
    /// 假的"文件已丢失"提示。
    #[tokio::test]
    async fn scan_missing_files_never_marks_downloading_task_with_missing_file() {
        let dir = unique_filetrack_test_dir("scan_downloading");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "t-downloading", &save_dir, "movie.mp4", 1).await;

        let sink = Arc::new(RecordingSink::new());
        scan_missing_files(
            db.clone(),
            sink.clone(),
            Arc::new(AtomicBool::new(false)),
            false,
            noop_cleanup_tx(),
        )
        .await;

        let task = db
            .load_task_by_id("t-downloading")
            .await
            .expect("load")
            .expect("task present");
        assert!(
            !task.file_missing,
            "status != 3 tasks must never be scanned or marked missing"
        );
        assert!(sink.events().is_empty());

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// 同名竞态回归：一个 completed 任务与一个 active(downloading) 任务共享
    /// 同一 `(save_dir, file_name)`（例如用户删除文件后用同名重新发起下
    /// 载）。目标文件在磁盘上不存在时，completed 任务必须被跳过而不是被误
    /// 标为丢失——它的"丢失"只是因为 active 任务尚未把文件写回原处。
    #[tokio::test]
    async fn scan_missing_files_skips_completed_task_when_active_task_shares_path() {
        let dir = unique_filetrack_test_dir("scan_race");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        let file_name = "movie.mp4"; // 磁盘上不存在

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "t-completed-stale", &save_dir, file_name, 3).await;
        insert_task_at_status(&db, "t-active-redownload", &save_dir, file_name, 1).await;

        let sink = Arc::new(RecordingSink::new());
        scan_missing_files(
            db.clone(),
            sink.clone(),
            Arc::new(AtomicBool::new(false)),
            false,
            noop_cleanup_tx(),
        )
        .await;

        let completed = db
            .load_task_by_id("t-completed-stale")
            .await
            .expect("load")
            .expect("task present");
        assert!(
            !completed.file_missing,
            "completed task sharing a target path with an active task must be skipped"
        );
        assert!(sink.events().is_empty());

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// `file_missing_action` 回流契约：`auto_delete = true` 时，本轮**新**判定
    /// 为丢失的任务 id（且仅这些）经清理通道回流一次；`"keep"`（false）时
    /// 通道保持空——UI 只看到标记，不删记录。
    #[tokio::test]
    async fn scan_missing_files_auto_delete_reports_ids() {
        let dir = unique_filetrack_test_dir("scan_autodelete");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        let gone_path = dir.join("gone.bin");
        std::fs::write(&gone_path, b"data").expect("write test file");
        std::fs::write(dir.join("kept.bin"), b"data").expect("write kept file");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "t-autodel", &save_dir, "gone.bin", 3).await;
        insert_task_at_status(&db, "t-kept", &save_dir, "kept.bin", 3).await;
        std::fs::remove_file(&gone_path).expect("delete test file");

        let sink = Arc::new(RecordingSink::new());

        // keep 模式：照常标记 + 发事件，但一个 id 都不回流。
        let (keep_tx, mut keep_rx) = mpsc::channel(8);
        scan_missing_files(
            db.clone(),
            sink.clone(),
            Arc::new(AtomicBool::new(false)),
            false,
            keep_tx,
        )
        .await;
        assert!(keep_rx.try_recv().is_err(), "keep 模式不得回流任何 task_id");

        // 复位标记，让下一轮扫描重新把它判定为「新丢失」。
        db.update_task_file_missing("t-autodel", false)
            .await
            .expect("reset file_missing");

        // delete 模式：恰好回流那一个丢失的任务，文件仍在的任务不受牵连。
        let (del_tx, mut del_rx) = mpsc::channel(8);
        scan_missing_files(
            db.clone(),
            sink.clone(),
            Arc::new(AtomicBool::new(false)),
            true,
            del_tx,
        )
        .await;
        let ids = del_rx
            .try_recv()
            .expect("delete 模式必须回流丢失的 task_id");
        assert_eq!(ids, vec!["t-autodel".to_string()]);
        assert!(del_rx.try_recv().is_err(), "只应回流一批");

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// 「重新下载」契约：磁盘产物被删、DB 进度/总大小/丢失标记/完成时间全部
    /// 复位、段行清空。这些复位在 `restart_task` 内同步完成于 resume 之前，
    /// 因此断言不与后续（必定失败的）下载 spawn 竞态。
    #[tokio::test]
    async fn restart_task_resets_progress_and_files() {
        let dir = unique_filetrack_test_dir("restart_reset");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        let file_path = dir.join("done.bin");
        std::fs::write(&file_path, b"stale payload").expect("write test file");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        // 端口 1 必定拒绝连接：重启后的下载会立刻失败，零真实网络流量。
        db.insert_task(
            "t-restart",
            "http://127.0.0.1:1/x.bin",
            "done.bin",
            &save_dir,
            1,
            4096,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        db.update_task_progress("t-restart", 4096)
            .await
            .expect("seed progress");
        db.update_task_status("t-restart", 3, "")
            .await
            .expect("mark completed");
        db.update_task_file_missing("t-restart", true)
            .await
            .expect("flag missing");
        db.insert_segments("t-restart", &[(0, 0, 4095)])
            .await
            .expect("insert segments");

        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        mgr.restart_task("t-restart").await;

        assert!(
            !file_path.exists(),
            "restart 必须丢弃上一轮的磁盘产物，即使它还在"
        );
        let task = db
            .load_task_by_id("t-restart")
            .await
            .expect("load")
            .expect("task present");
        assert_eq!(task.downloaded_bytes, 0, "进度必须归零");
        assert_eq!(task.total_bytes, 0, "总大小必须归零，重下时重新探测");
        assert!(!task.file_missing, "file_missing 标记必须清除");
        assert_eq!(task.completed_at, "", "completed_at 必须清空");
        assert!(
            db.load_segments("t-restart")
                .await
                .expect("load segments")
                .is_empty(),
            "段行必须清空，否则重下会按旧布局续传"
        );
        // 重新下载必须真正起飞：完成态「继续」被忽略（#935），但 restart 先复位为 0 再恢复。
        assert_ne!(task.status, 3, "重新下载后任务不得仍停在完成态");
        assert!(
            mgr.active_tasks.contains_key("t-restart")
                || mgr.pending_queue.iter().any(|q| q.task_id == "t-restart"),
            "已完成任务的重新下载必须重新开始下载"
        );
        cancel_all_active(&mgr);

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// 仅删文件、保留任务记录：与 `delete_task(delete_files=true)`
    /// 相反。文件被删、`file_missing` 落库为 true、任务行本身原样保留
    /// （status 仍是 3，可用 `restart_task` 重新下载同一条记录）。
    #[tokio::test]
    async fn clear_task_file_deletes_disk_file_but_keeps_task_record() {
        let dir = unique_filetrack_test_dir("clear_file");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        let file_path = dir.join("done.bin");
        std::fs::write(&file_path, b"finished payload").expect("write test file");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.insert_task(
            "t-clear",
            "http://example.com/done.bin",
            "done.bin",
            &save_dir,
            1,
            4096,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        db.update_task_progress("t-clear", 4096)
            .await
            .expect("seed progress");
        db.update_task_status("t-clear", 3, "")
            .await
            .expect("mark completed");

        let sink = Arc::new(RecordingSink::new());
        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            sink.clone(),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        let ok = mgr.clear_task_file("t-clear").await;
        assert!(ok, "clear_task_file must succeed for a completed task");
        assert!(!file_path.exists(), "磁盘文件必须被删除");

        let task = db
            .load_task_by_id("t-clear")
            .await
            .expect("load")
            .expect("task record must still exist");
        assert_eq!(task.status, 3, "任务记录（含 status）必须原样保留");
        assert!(task.file_missing, "file_missing 必须落库为 true");

        assert!(
            sink.events().iter().any(|e| matches!(
                e,
                EngineEvent::FileMissingChanged(changes)
                    if changes == &[("t-clear".to_string(), true)]
            )),
            "must broadcast FileMissingChanged(true) for the cleared task"
        );

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    #[tokio::test]
    async fn clear_task_file_refuses_non_completed_task() {
        let dir = unique_filetrack_test_dir("clear_file_active");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let save_dir = dir.to_string_lossy().to_string();
        let file_path = dir.join("partial.bin");
        std::fs::write(&file_path, b"still downloading").expect("write test file");

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.insert_task(
            "t-active",
            "http://example.com/partial.bin",
            "partial.bin",
            &save_dir,
            1,
            4096,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("insert task");
        // status stays 0 (pending) — not completed.

        let mut mgr = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: save_dir.clone(),
                app_data_dir: String::new(),
                data_dir: std::env::temp_dir(),
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
            },
            Arc::new(RecordingSink::new()),
            Arc::new(crate::NoopSelection),
        )
        .expect("construct manager");

        let ok = mgr.clear_task_file("t-active").await;
        assert!(!ok, "non-completed task must be refused");
        assert!(
            file_path.exists(),
            "must not touch the file of a non-completed task"
        );

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // Windows 上 DB 句柄可能仍持有目录；清理不改变行为断言结果。
            crate::logger::report_warning("download-manager-test", "remove test directory", &error);
        }
    }

    /// BT 数据下载完成标记的一次性契约(`bt_finish_notified` 去重集,见
    /// `progress_reporter` 文档):同一 task_id 第二次标记
    /// `bt_data_finished=true`(对应完成搬移失败后重试路径重新进入 finished
    /// 分支)不得再次触发 `EngineEvent::BtDataFinished`,否则宿主会
    /// 对同一 GID 重复广播 `aria2.onBtDownloadComplete`。
    #[tokio::test]
    async fn progress_reporter_emits_bt_data_finished_once_and_dedupes_repeat() {
        let (tx, rx) = mpsc::channel(8);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let handle = tokio::spawn(progress_reporter(rx, db, sink.clone()));

        for _ in 0..2 {
            tx.send(ProgressUpdate {
                task_id: "bt1".to_string(),
                status: 1,
                downloaded_bytes: 100,
                total_bytes: 100,
                bt_data_finished: true,
                ..Default::default()
            })
            .await
            .expect("send update");
        }
        drop(tx);
        handle
            .await
            .expect("reporter task must finish once channel closes");

        let bt_finished_count = sink
            .events()
            .into_iter()
            .filter(|e| matches!(e, EngineEvent::BtDataFinished { task_id } if task_id == "bt1"))
            .count();
        assert_eq!(
            bt_finished_count, 1,
            "second bt_data_finished=true mark on the same task must not refire"
        );
    }

    fn progress_statuses(sink: &RecordingSink) -> Vec<i32> {
        sink.events()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::TaskProgress { status, .. } => Some(status),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn transfer_baselines_bypass_throttling_and_precede_matching_progress() {
        let (tx, rx) = mpsc::channel(8);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        for baseline in [8_000_000, 0, 64 * 1024] {
            tx.send(ProgressUpdate {
                task_id: "reset".into(),
                status: 1,
                downloaded_bytes: baseline,
                download_baseline: Some(baseline),
                ..Default::default()
            })
            .await
            .expect("enqueue authoritative baseline");
        }
        drop(tx);
        progress_reporter(rx, db, sink.clone()).await;
        let relevant: Vec<_> = sink
            .events()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::TaskTransferBaseline {
                    downloaded_bytes, ..
                } => Some((true, downloaded_bytes)),
                EngineEvent::TaskProgress {
                    downloaded_bytes, ..
                } => Some((false, downloaded_bytes)),
                _ => None,
            })
            .collect();
        assert_eq!(
            relevant,
            [
                (true, 8_000_000),
                (false, 8_000_000),
                (true, 0),
                (false, 0),
                (true, 64 * 1024),
                (false, 64 * 1024),
            ]
        );
    }

    #[tokio::test]
    async fn next_transfer_baseline_carries_the_previous_raw_window_end() {
        const MIB: i64 = 1024 * 1024;
        let (tx, rx) = mpsc::channel(8);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        for (downloaded_bytes, baseline) in [
            (MIB, true),
            (17 * MIB, false),
            (17 * MIB, true),
            (33 * MIB, false),
            (33 * MIB, true),
        ] {
            tx.send(ProgressUpdate {
                task_id: "range-windows".into(),
                status: 1,
                downloaded_bytes,
                download_baseline: baseline.then_some(downloaded_bytes),
                ..Default::default()
            })
            .await
            .expect("enqueue range boundary");
        }
        drop(tx);
        progress_reporter(rx, db, sink.clone()).await;
        let baselines: Vec<_> = sink
            .events()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::TaskTransferBaseline {
                    downloaded_bytes,
                    previous_downloaded_bytes,
                    ..
                } => Some((downloaded_bytes, previous_downloaded_bytes)),
                _ => None,
            })
            .collect();
        assert_eq!(
            baselines,
            [
                (MIB, None),
                (17 * MIB, Some(17 * MIB)),
                (33 * MIB, Some(33 * MIB)),
            ]
        );
    }

    /// 落盘字节按段跳变(HLS 整段缓冲后写盘)时,速度与宿主流量都跟随接收字节:
    /// 落盘计数不动,速度照样出现;接收增量全部在对应进度帧之前交给宿主,总量守恒,
    /// 整段落盘的跳变不再计一次。
    #[tokio::test]
    async fn received_bytes_drive_speed_and_host_traffic() {
        let (tx, rx) = mpsc::channel(16);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let handle = tokio::spawn(progress_reporter(rx, db, sink.clone()));
        let received = |downloaded_bytes, transfer| ProgressUpdate {
            task_id: "hls".into(),
            status: 1,
            downloaded_bytes,
            transfer_bytes: Some(transfer),
            ..Default::default()
        };
        for (step, transfer) in [0_i64, 200_000, 400_000, 600_000].into_iter().enumerate() {
            if step > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            tx.send(received(0, transfer))
                .await
                .expect("enqueue received sample");
        }
        tx.send(received(600_000, 600_000))
            .await
            .expect("enqueue segment write");
        tx.send(ProgressUpdate {
            task_id: "hls".into(),
            status: 3,
            downloaded_bytes: 600_000,
            total_bytes: 600_000,
            ..Default::default()
        })
        .await
        .expect("enqueue completion");
        drop(tx);
        handle.await.expect("reporter task");

        let events = sink.events();
        let transferred: i64 = events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::TaskTransferred { bytes, .. } => Some(*bytes),
                _ => None,
            })
            .sum();
        assert_eq!(transferred, 600_000);
        for pair in events.windows(2) {
            if let EngineEvent::TaskTransferred { task_id, .. } = &pair[0] {
                assert!(
                    matches!(&pair[1], EngineEvent::TaskProgress { task_id: id, .. } if id == task_id),
                    "received bytes must precede their progress frame"
                );
            }
        }
        // 200 KB / 400 ms = 500 KB/s,而落盘计数在测速期间一直是 0。
        let peak = events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::TaskProgress {
                    status: 1,
                    downloaded_bytes: 0,
                    speed,
                    ..
                } => Some(*speed),
                _ => None,
            })
            .max()
            .unwrap_or_default();
        assert!((250_000..=1_000_000).contains(&peak), "peak speed {peak}");
    }

    /// 在途整表快照期间，终态进度帧必须等快照发出后再发：读库早于落库的旧快照
    /// 不得排在该任务完成帧之后，否则已完成的行会被回退成「下载中」。
    #[tokio::test]
    async fn terminal_progress_waits_for_in_flight_snapshot() {
        let (tx, rx) = mpsc::channel(8);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let handle = tokio::spawn(progress_reporter(rx, db.clone(), sink.clone()));

        let in_flight_snapshot = db.lock_event_order().await;
        tx.send(ProgressUpdate {
            task_id: "late".to_owned(),
            status: 3,
            downloaded_bytes: 10,
            total_bytes: 10,
            ..Default::default()
        })
        .await
        .expect("send terminal update");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            progress_statuses(&sink).is_empty(),
            "terminal frame must not overtake an in-flight snapshot"
        );

        drop(in_flight_snapshot);
        drop(tx);
        handle
            .await
            .expect("reporter task must finish once channel closes");
        assert_eq!(progress_statuses(&sink), [3]);
    }

    /// 活跃态进度帧不取顺序门：快照持门期间下载进度不得被阻塞。
    #[tokio::test]
    async fn active_progress_does_not_wait_for_snapshot() {
        let (tx, rx) = mpsc::channel(8);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let handle = tokio::spawn(progress_reporter(rx, db.clone(), sink.clone()));

        let in_flight_snapshot = db.lock_event_order().await;
        tx.send(ProgressUpdate {
            task_id: "live".to_owned(),
            status: 1,
            downloaded_bytes: 5,
            total_bytes: 10,
            ..Default::default()
        })
        .await
        .expect("send active update");
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("active frame must not wait for the snapshot gate")
            .expect("reporter task must finish once channel closes");
        assert_eq!(progress_statuses(&sink), [1]);
        drop(in_flight_snapshot);
    }

    /// 整表快照「读库 + 发出」必须在持门期间进行，与终态帧互斥。
    #[tokio::test]
    async fn tasks_snapshot_holds_event_order_gate() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let manager = sched_manager_with_sink(&db, 1, sink.clone());

        let terminal_frame = db.lock_event_order().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            manager.send_tasks_snapshot(),
        )
        .await;
        assert!(blocked.is_err(), "snapshot must wait for the gate holder");
        assert!(sink.events().is_empty());

        drop(terminal_frame);
        manager.send_tasks_snapshot().await;
        assert!(
            sink.events()
                .iter()
                .any(|event| matches!(event, EngineEvent::TasksSnapshot(_)))
        );
    }

    /// 做种状态帧在 actor 之外先落库、再读库发出，同样不得抢在在途整表快照之前。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn seeding_progress_waits_for_in_flight_snapshot() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        insert_task_at_status(&db, "seed", "/tmp", "seed.bin", 3).await;
        let sink: Arc<RecordingSink> = Arc::new(RecordingSink::new());
        let dyn_sink: Arc<dyn EventSink> = sink.clone();

        let in_flight_snapshot = db.lock_event_order().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            emit_seeding_progress(&db, &dyn_sink, "seed", SEEDING_STATUS_ACTIVE, ""),
        )
        .await;
        assert!(blocked.is_err(), "seeding frame must wait for the snapshot");
        assert!(progress_statuses(&sink).is_empty());

        drop(in_flight_snapshot);
        emit_seeding_progress(&db, &dyn_sink, "seed", SEEDING_STATUS_ACTIVE, "").await;
        assert_eq!(progress_statuses(&sink), [3]);
    }

    /// BT 上传速率透传契约:活跃状态原样透传 `upload_speed_bps`(latch 进
    /// `TaskSpeedState.upload_bps`),到达终态时强制归零,避免 UI 在任务
    /// 完成/出错后仍显示陈旧的上传速率。
    #[tokio::test]
    async fn progress_reporter_forwards_upload_speed_then_zeroes_on_terminal() {
        let (tx, rx) = mpsc::channel(8);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let handle = tokio::spawn(progress_reporter(rx, db, sink.clone()));

        tx.send(ProgressUpdate {
            task_id: "bt2".to_string(),
            status: 1,
            downloaded_bytes: 10,
            total_bytes: 1000,
            upload_speed_bps: 8192,
            ..Default::default()
        })
        .await
        .expect("send active update");

        tx.send(ProgressUpdate {
            task_id: "bt2".to_string(),
            status: 3,
            downloaded_bytes: 1000,
            total_bytes: 1000,
            upload_speed_bps: 8192,
            ..Default::default()
        })
        .await
        .expect("send terminal update");

        drop(tx);
        handle
            .await
            .expect("reporter task must finish once channel closes");

        let progress_events: Vec<(i32, i64)> = sink
            .events()
            .into_iter()
            .filter_map(|e| match e {
                EngineEvent::TaskProgress {
                    status,
                    upload_speed_bps,
                    ..
                } => Some((status, upload_speed_bps)),
                _ => None,
            })
            .collect();

        assert_eq!(
            progress_events.first(),
            Some(&(1, 8192)),
            "active update must forward upload_speed_bps as-is"
        );
        assert_eq!(
            progress_events.last(),
            Some(&(3, 0)),
            "terminal update must zero upload_speed_bps regardless of the raw value"
        );
    }

    #[tokio::test]
    async fn progress_reporter_terminal_clears_only_observed_activity_and_keeps_ranges() {
        let (tx, rx) = mpsc::channel(8);
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let sink = Arc::new(RecordingSink::new());
        let handle = tokio::spawn(progress_reporter(rx, db, sink.clone()));
        let segment = crate::transfer_activity::TaskSegment {
            index: 0,
            start_byte: 0,
            end_byte: 99,
            downloaded_bytes: 20,
            active: Some(true),
        };
        tx.send(ProgressUpdate {
            task_id: "range".to_owned(),
            status: 1,
            downloaded_bytes: 20,
            total_bytes: 100,
            runtime: Some(TaskRuntime {
                task_id: "range".to_owned(),
                sampled_at_ms: 1,
                sample_sequence: crate::transfer_activity::next_sample_sequence(),
                active_transfers: Some(1),
                connected_peers: None,
                parallelism_limit: Some(8),
                total_bytes: 100,
                segments: vec![segment],
                source_bytes: None,
            }),
            ..Default::default()
        })
        .await
        .expect("send live sample");
        tx.send(ProgressUpdate {
            task_id: "range".to_owned(),
            status: 2,
            downloaded_bytes: 20,
            total_bytes: 100,
            ..Default::default()
        })
        .await
        .expect("send pause");
        drop(tx);
        handle.await.expect("reporter finished");
        let samples: Vec<_> = sink
            .events()
            .into_iter()
            .filter_map(|e| match e {
                EngineEvent::TaskRuntimeChanged(runtime) => Some(runtime),
                _ => None,
            })
            .collect();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].active_transfers, Some(1));
        assert_eq!(samples[1].active_transfers, Some(0));
        assert!(samples[1].sample_sequence > samples[0].sample_sequence);
        assert_eq!(samples[1].segments[0].active, Some(false));
        assert_eq!(samples[1].segments[0].downloaded_bytes, 20);
        assert_eq!(
            (
                samples[1].segments[0].start_byte,
                samples[1].segments[0].end_byte
            ),
            (0, 99)
        );
    }

    // -----------------------------------------------------------------------
    // 队列定时调度：HH:MM 解析与边沿判定
    // -----------------------------------------------------------------------

    fn sched_queue(id: &str, running: bool, start: &str, stop: &str, days: i32) -> QueueInfo {
        QueueInfo {
            queue_id: id.to_string(),
            name: id.to_string(),
            speed_limit_kbps: 0,
            upload_limit_kbps: 0,
            max_concurrent: 0,
            default_save_dir: String::new(),
            position: 0,
            default_segments: 0,
            default_user_agent: String::new(),
            is_running: running,
            schedule_enabled: true,
            schedule_start: start.to_string(),
            schedule_stop: stop.to_string(),
            schedule_days: days,
        }
    }

    #[cfg(feature = "bt")]
    #[test]
    fn effective_upload_bps_prefers_task_then_queue_then_unlimited() {
        // 任务级 > 0 直接生效，无视队列级。
        assert_eq!(effective_upload_bps(256_000, Some(512)), 256_000);
        // 任务级未设 → 队列级 KB/s ×1024。
        assert_eq!(effective_upload_bps(0, Some(512)), 512 * 1024);
        // 两级都未设 / 队列未知 → 0（不设 torrent 级限制）。
        assert_eq!(effective_upload_bps(0, Some(0)), 0);
        assert_eq!(effective_upload_bps(0, None), 0);
        // 防御：负值一律视为未设。
        assert_eq!(effective_upload_bps(-5, Some(-3)), 0);
    }

    #[test]
    fn parse_hhmm_accepts_valid_and_rejects_garbage() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("8:05"), Some(485));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm(""), None);
        assert_eq!(parse_hhmm("abc"), None);
        assert_eq!(parse_hhmm("12-30"), None);
    }

    #[test]
    fn schedule_edge_fires_once_per_day() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let queues = [sched_queue("q", false, "10:00", "", 0x7f)];
        let mut fired = HashMap::new();

        // 到点：start 边沿触发。
        let (passed, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 600);
        assert_eq!(actions, vec![("q".to_string(), true)]);
        for (k, d) in passed {
            fired.insert(k, d);
        }

        // 同日再 tick（含用户手动停止后）：同一边沿不再触发。
        let (_, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 601);
        assert!(actions.is_empty(), "an edge fires at most once per day");

        // 次日：重新触发。
        let tomorrow = today.succ_opt().unwrap();
        let (_, actions) = due_schedule_actions(queues.iter(), &fired, tomorrow, 1, 600);
        assert_eq!(actions.len(), 1, "a new day re-arms the edge");
    }

    #[test]
    fn schedule_catchup_prefers_latest_edge() {
        // start=10:00 与 stop=11:00 都已越过（休眠唤醒补触发场景）：
        // 两个边沿都记账，但只执行时间靠后的 stop。
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let queues = [sched_queue("q", true, "10:00", "11:00", 0x7f)];
        let fired = HashMap::new();
        let (passed, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 720);
        assert_eq!(passed.len(), 2, "both passed edges must be recorded");
        assert_eq!(actions, vec![("q".to_string(), false)]);
    }

    /// 跨午夜窗口（23:00-07:00）：凌晨 01:00 重启/唤醒必须补触发昨日启动边沿，
    /// 且记账为昨天——今晚 23:00 的正常启动不能被占掉。
    #[test]
    fn overnight_window_catches_up_yesterdays_start_after_midnight() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap_or_default();
        let yesterday = today.pred_opt().unwrap_or_default();
        let queues = [sched_queue("q", true, "23:00", "07:00", 0x7f)];
        let mut fired = HashMap::new();

        let (passed, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 60);
        assert_eq!(actions, vec![("q".to_string(), true)]);
        for (k, d) in passed {
            assert_eq!(d, yesterday);
            fired.insert(k, d);
        }

        // 同一凌晨再 tick：不重复触发。
        let (_, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 90);
        assert!(actions.is_empty());

        // 当晚 23:00：今天的启动边沿仍正常触发。
        let (_, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 23 * 60);
        assert_eq!(actions, vec![("q".to_string(), true)]);

        // 昨天不在生效日掩码内（仅 bit1 生效，今天是 bit1）→ 不补触发。
        let only_today = [sched_queue("q", true, "23:00", "07:00", 0b000_0010)];
        let (_, actions) =
            due_schedule_actions(only_today.iter(), &HashMap::new(), today, 1 << 1, 60);
        assert!(actions.is_empty());
    }

    #[test]
    fn schedule_respects_day_mask_disabled_and_future_times() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let fired = HashMap::new();

        // 仅周一（bit0）生效；tick 处于周四（bit3）→ 不触发。
        let queues = [sched_queue("q", false, "10:00", "", 0b000_0001)];
        let (_, actions) = due_schedule_actions(queues.iter(), &fired, today, 1 << 3, 600);
        assert!(actions.is_empty());

        // 定时未启用 → 不触发。
        let mut disabled = sched_queue("q", false, "10:00", "", 0x7f);
        disabled.schedule_enabled = false;
        let (_, actions) = due_schedule_actions([disabled].iter(), &fired, today, 1, 600);
        assert!(actions.is_empty());

        // 尚未到点 → 不触发。
        let queues = [sched_queue("q", false, "10:00", "", 0x7f)];
        let (_, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 599);
        assert!(actions.is_empty());
    }

    #[test]
    fn schedule_tie_prefers_stop() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let queues = [sched_queue("q", true, "10:00", "10:00", 0x7f)];
        let fired = HashMap::new();
        let (_, actions) = due_schedule_actions(queues.iter(), &fired, today, 1, 600);
        assert_eq!(
            actions,
            vec![("q".to_string(), false)],
            "start == stop resolves to stop"
        );
    }

    // -----------------------------------------------------------------------
    // decide_bt_session_release — 暂停任务保活宽限
    // -----------------------------------------------------------------------

    #[cfg(feature = "bt")]
    const GRACE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

    #[cfg(feature = "bt")]
    fn paused_only() -> BtSessionHolds {
        BtSessionHolds {
            paused_incomplete: true,
            ..BtSessionHolds::default()
        }
    }

    #[cfg(feature = "bt")]
    #[test]
    fn paused_only_session_is_kept_for_grace_then_released() {
        let t0 = std::time::Instant::now();
        let mut since = None;

        // 首次观察起算宽限，不释放。
        let first = decide_bt_session_release(paused_only(), &mut since, t0, GRACE);
        assert!(matches!(
            first,
            BtSessionVerdict::KeepPausedGrace {
                just_armed: true,
                ..
            }
        ));
        assert_eq!(since, Some(t0));

        // 宽限内：仍保留，起点不动，剩余时间递减。
        let mid = t0 + GRACE - std::time::Duration::from_secs(1);
        match decide_bt_session_release(paused_only(), &mut since, mid, GRACE) {
            BtSessionVerdict::KeepPausedGrace {
                just_armed,
                remaining,
            } => {
                assert!(!just_armed);
                assert_eq!(remaining, std::time::Duration::from_secs(1));
            }
            other => panic!("expected grace keep, got {other:?}"),
        }
        assert_eq!(since, Some(t0));

        // 到期：释放，并清掉起点。
        assert_eq!(
            decide_bt_session_release(paused_only(), &mut since, t0 + GRACE, GRACE),
            BtSessionVerdict::Release {
                after_paused_grace: true
            }
        );
        assert_eq!(since, None);
    }

    #[cfg(feature = "bt")]
    #[test]
    fn bt_activity_keeps_session_and_resets_grace() {
        let t0 = std::time::Instant::now();
        let busy_cases = [
            (
                BtSessionHolds {
                    active_download: true,
                    ..paused_only()
                },
                BtSessionKeep::ActiveDownload,
            ),
            (
                BtSessionHolds {
                    seeders: true,
                    ..paused_only()
                },
                BtSessionKeep::Seeders,
            ),
            (
                BtSessionHolds {
                    queued: true,
                    ..paused_only()
                },
                BtSessionKeep::QueuedTask,
            ),
            (
                BtSessionHolds {
                    inflight_add: true,
                    ..paused_only()
                },
                BtSessionKeep::InflightAdd,
            ),
        ];
        for (holds, reason) in busy_cases {
            // 已经空闲了很久（宽限早已超时）时出现 BT 活动：不能释放，且起点被清。
            let mut since = Some(t0);
            let verdict = decide_bt_session_release(holds, &mut since, t0 + GRACE * 4, GRACE);
            assert_eq!(verdict, BtSessionVerdict::Keep(reason));
            assert_eq!(since, None, "{reason:?} must clear the idle start");
        }

        // 活动结束后重新暂停：宽限从头起算，而不是沿用旧起点立即释放。
        let mut since = Some(t0);
        let active_verdict = decide_bt_session_release(
            BtSessionHolds {
                active_download: true,
                ..BtSessionHolds::default()
            },
            &mut since,
            t0 + GRACE * 2,
            GRACE,
        );
        assert_eq!(
            active_verdict,
            BtSessionVerdict::Keep(BtSessionKeep::ActiveDownload)
        );
        let repaused = t0 + GRACE * 2 + std::time::Duration::from_secs(10);
        assert!(matches!(
            decide_bt_session_release(paused_only(), &mut since, repaused, GRACE),
            BtSessionVerdict::KeepPausedGrace {
                just_armed: true,
                ..
            }
        ));
        assert_eq!(since, Some(repaused));
    }

    #[cfg(feature = "bt")]
    #[test]
    fn session_without_any_hold_is_released_immediately() {
        let t0 = std::time::Instant::now();
        let mut since = Some(t0);
        assert_eq!(
            decide_bt_session_release(BtSessionHolds::default(), &mut since, t0, GRACE),
            BtSessionVerdict::Release {
                after_paused_grace: false
            }
        );
        assert_eq!(since, None);
    }
}
