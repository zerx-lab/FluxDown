use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::FutureExt;
use reqwest::Client;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bt_downloader::{self, BtConfig, BtDownloadParams, SharedBtSession, TorrentSource};
use crate::bt_seeding::{
    SEEDING_STATUS_ACTIVE, SeedingLimitConfig, SeedingStopReason, SeedingUploadSnapshot,
};
use crate::dash_downloader;
use crate::db::Db;
use crate::downloader::{self, DownloadParams, ProgressUpdate, SegmentProgressInfo};
use crate::events::{EngineEvent, EventSink};
use crate::ftp_downloader;
use crate::hls_downloader;
use crate::logger::log_info;
use crate::model::{
    MAIN_QUEUE_ID, QueueInfo, QueuePosition, SegmentDetail, TaskInfo, is_builtin_queue,
};
use crate::proxy_config::ProxyConfig;
use crate::segment_coordinator::is_single_conn_domain;
use crate::selection::HostSelection;
use crate::speed_limiter::SpeedLimiter;

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

/// Handle a panicked download task: log the error, persist error status to DB,
/// and send an error progress update to Dart.
///
/// This is the common panic-recovery logic shared by all download task spawns
/// (HTTP, FTP, BT — both create and resume paths).
async fn handle_task_panic(
    task_id: &str,
    msg: &str,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
) {
    log_info!("[download] PANIC in task {}: {}", task_id, msg);
    let _ = db.update_task_status(task_id, 4, msg).await;
    let _ = progress_tx
        .send(ProgressUpdate {
            task_id: task_id.to_string(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: 4,
            error_message: msg.to_string(),
            file_name: String::new(),
            segment_details: None,
            ..Default::default()
        })
        .await;
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

/// Auto 模式最大连接数上限的默认值（config `auto_max_connections`，经
/// [`DownloadManager::set_auto_max_connections`] 注入）。
///
/// 语义：advisor 推荐值经此裁剪——`effective = min(advisor, cap)`。
/// 默认 16 而非 advisor 的绝对上限 64：避免对连接敏感的服务器/CDN 上来就
/// 32/64 并发触发风控；需要更高并发的用户可在设置中显式调大。
const DEFAULT_AUTO_MAX_CONNECTIONS: i32 = 16;

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

/// `invalidate_bt_session` 在关停前等待 inflight `add_torrent` 任务归零的
/// 总上限。BT 监听端口由这些 detached 任务持有的 `Arc<Session>` 绑定，
/// 超时后即便仍有 inflight 也强行继续关停（避免无限等待挂死配置变更）。
/// 5s 取自 magnet DHT 元数据解析的典型耗时上界。
const INVALIDATE_INFLIGHT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `invalidate_bt_session` 轮询 inflight `add_torrent` 状态的间隔。
/// 200ms 足够细以快速响应归零，又不会空耗 CPU。
const INVALIDATE_INFLIGHT_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(200);

/// 判断错误信息是否属于可自动重试的瞬时网络错误。
/// 排除永久性错误（404、403、checksum 等），仅重试网络层问题。
fn is_retriable_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
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

/// Determine if a URL uses the FTP protocol (case-insensitive).
fn is_ftp_url(url: &str) -> bool {
    url.get(..6)
        .map(|prefix| prefix.eq_ignore_ascii_case("ftp://"))
        .unwrap_or(false)
}

/// Determine if a URL is a magnet link.
fn is_magnet(url: &str) -> bool {
    bt_downloader::is_magnet_url(url)
}

/// Determine if a URL is a torrent-file sentinel (task created from .torrent file).
fn is_torrent_file_url(url: &str) -> bool {
    url.starts_with("torrent-file://")
}

/// Determine if a URL represents any kind of BT download (magnet or .torrent file).
fn is_bt_url(url: &str) -> bool {
    is_magnet(url) || is_torrent_file_url(url)
}

/// 文件跟踪扫描的并发上限。`try_exists` 内部走 tokio blocking 线程池，限流以
/// bound 该共享池占用，防慢盘/网络盘扫描饿死并发下载 IO。
const FILE_SCAN_CONCURRENCY: usize = 64;

/// 单次文件存在性探测的超时（秒），防失联网络盘把整批扫描拖住到 OS 默认
/// 重试时长。
const FILE_SCAN_STAT_TIMEOUT_SECS: u64 = 5;

/// 文件跟踪：构造 completed 任务的目标磁盘路径。`file_name` 为空或不安全
/// （未命名 magnet、路径穿越等）时返回 `None`——无法可靠判定存在性，跳过。
fn task_target_path(save_dir: &str, file_name: &str) -> Option<PathBuf> {
    if file_name.is_empty() || !is_safe_file_name(file_name) {
        return None;
    }
    Some(PathBuf::from(save_dir).join(file_name))
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
async fn scan_missing_files(db: Db, sink: Arc<dyn EventSink>, scanning: Arc<AtomicBool>) {
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

    let tasks = match db.load_all_tasks().await {
        Ok(t) => t,
        Err(e) => {
            log_info!("[file-scan] load_all_tasks error: {}", e);
            return;
        }
    };

    // 活跃任务（pending/downloading/preparing）占用的目标路径：避免正在重下
    // 同名文件时把旧的 completed 任务误判为丢失。
    let active_paths: HashSet<(&str, &str)> = tasks
        .iter()
        .filter(|t| matches!(t.status, 0 | 1 | 5))
        .map(|t| (t.save_dir.as_str(), t.file_name.as_str()))
        .collect();

    let sem = Arc::new(Semaphore::new(FILE_SCAN_CONCURRENCY));
    let mut futs = Vec::new();
    for t in tasks.iter().filter(|t| t.status == 3) {
        if active_paths.contains(&(t.save_dir.as_str(), t.file_name.as_str())) {
            continue;
        }
        let Some(path) = task_target_path(&t.save_dir, &t.file_name) else {
            continue;
        };
        let sem = sem.clone();
        let id = t.task_id.clone();
        let was_missing = t.file_missing;
        futs.push(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            let missing = probe_missing(&path).await?;
            (missing != was_missing).then_some((id, missing))
        });
    }

    let mut changes: Vec<(String, bool)> = Vec::new();
    for (id, missing) in futures_util::future::join_all(futs)
        .await
        .into_iter()
        .flatten()
    {
        match db.update_task_file_missing(&id, missing).await {
            Ok(true) => changes.push((id, missing)),
            Ok(false) => {} // 任务已离开 status=3（被删/状态变化）→ 良性空操作
            Err(e) => log_info!("[file-scan] update {} error: {}", id, e),
        }
    }

    if !changes.is_empty() {
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
                let _ = tokio::fs::remove_dir_all(&path).await;
            } else {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
        let stage_dir = bt_downloader::bt_stage_dir(&save_dir, &task_id);
        if stage_dir.exists() {
            log_info!(
                "[manager] delete {} deferred: removing staging dir {}",
                task_id,
                stage_dir.display()
            );
            let _ = tokio::fs::remove_dir_all(&stage_dir).await;
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
        let _ = tokio::fs::remove_file(&audio_temp).await;
        if delete_files && is_safe_file_name(&file_name) {
            let _ = tokio::fs::remove_file(&audio_path).await;
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

/// 删除任务已登记的衍生产物文件（插件 onDone 经 `flux.task.recordArtifact`
/// 登记，如转码 mp4）。仅在「删除任务并删除文件」时调用；文件名经
/// `is_safe_file_name` 二次校验后限定在 `save_dir` 内 best-effort 删除。
async fn delete_task_artifact_files(db: &crate::db::Db, task_id: &str, save_dir: &str) {
    let Ok(names) = db.load_task_artifacts(task_id).await else {
        return;
    };
    for name in names {
        if is_safe_file_name(&name) {
            let _ = tokio::fs::remove_file(PathBuf::from(save_dir).join(&name)).await;
        }
    }
}

/// Synchronous version of `dedup_filename` for use in the manager's
/// synchronous section (before `tokio::spawn`).
///
/// Checks both the on-disk state and the `reserved` in-flight set so that
/// the chosen name does not collide with files already being downloaded by
/// sibling tasks in the same batch.
///
/// Unlike the async version, this uses `std::path::Path::exists()` for the
/// fast-path disk check — acceptable here because we are on the
/// `current_thread` runtime in a synchronous (non-`.await`) section and the
/// result only needs to be "good enough" at the moment of reservation.
fn dedup_filename_sync(
    dir: &std::path::Path,
    name: &str,
    reserved: &HashSet<std::path::PathBuf>,
) -> String {
    let temp_ext = downloader::TEMP_EXT;

    // Phase 1: fast probe.
    let candidate = dir.join(name);
    let temp_candidate = PathBuf::from(format!("{}{}", candidate.display(), temp_ext));
    if !reserved.contains(&temp_candidate) && !candidate.exists() && !temp_candidate.exists() {
        return name.to_string();
    }

    // Phase 2: conflict — scan directory once into a set.
    // 条目名小写折叠:Windows/APFS 大小写不敏感,精确比较会漏判仅大小写
    // 不同的编号变体,finalize rename 的 REPLACE 语义会静默覆盖真实文件
    // (同 `downloader::dedup_filename`)。
    let existing: HashSet<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| {
                e.ok()
                    .map(|e| e.file_name().to_string_lossy().to_lowercase())
            })
            .collect()
        })
        .unwrap_or_default();

    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str());

    for i in 1..=9999 {
        let new_name = if let Some(ext) = ext {
            format!("{} ({}).{}", stem, i, ext)
        } else {
            format!("{} ({})", stem, i)
        };
        let temp_name = format!("{}{}", new_name, temp_ext);
        let temp_path = dir.join(&temp_name);
        if !reserved.contains(&temp_path)
            && !existing.contains(&new_name.to_lowercase())
            && !existing.contains(&temp_name.to_lowercase())
        {
            return new_name;
        }
    }
    // 极端兜底:编号变体全被占用时返回原名会导致落盘覆盖,用 UUID 后缀
    // 保证唯一(对齐 `downloader::dedup_filename` / BT `dedup_name_in_dir`)。
    let uniq = uuid::Uuid::new_v4();
    match ext {
        Some(e) => format!("{} ({}).{}", stem, uniq, e),
        None => format!("{} ({})", stem, uniq),
    }
}

/// Notification sent from a spawned download task when it finishes.
pub struct TaskDone {
    pub task_id: String,
    /// Generation counter — must match `active_tokens` entry to allow cleanup.
    /// Prevents a stale TaskDone from an old spawn removing a newer token.
    pub generation: u64,
    /// 本次任务在 `do_start_task` 中预订的临时文件路径（`.fdownloading`）。
    /// `on_task_done` 收到后从 `reserved_temp_paths` 中移除，释放预订。
    /// BT 任务、file_name 为空（probe 后确定）的任务此字段为 `None`。
    pub reserved_temp_path: Option<std::path::PathBuf>,
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
    /// downloaded_bytes at the start of the current sampling window.
    sample_bytes: i64,
    /// Timestamp of the current sampling window start.
    sample_time: std::time::Instant,
    /// Latest downloaded_bytes seen (for non-monotonic detection).
    latest_bytes: i64,
    /// Resolved file_name (latched from the first non-empty update).
    file_name: String,
    /// Cached segment snapshot — updated on every incoming update that
    /// carries segment_details, regardless of rate-limiting.  This ensures
    /// the next send always has the latest segment data available.
    cached_segments: Option<Vec<SegmentProgressInfo>>,
    /// Last status sent to Dart.  Used to detect status transitions so that
    /// they are always forwarded immediately (not rate-limited).
    last_sent_status: i32,
    /// Last raw status observed from downloader updates.
    last_raw_status: i32,
    /// Number of sampling windows to skip speed calculation for.
    /// Used as warmup after prepare/resume to avoid artificial speed spikes
    /// caused by baseline jumps (e.g. resume from non-zero downloaded bytes).
    speed_warmup_remaining: u8,
    /// Whether the "no cached segments" anomaly has already been logged for
    /// this task — it indicates a real problem (segment visualization will
    /// be empty) but repeats on every update, so log it only once.
    logged_missing_segments: bool,
    /// Latest upload speed (bytes/sec) reported by the downloader.
    /// Non-zero only for BT tasks (librqbit stats); latched from every
    /// incoming update so throttled emits carry the freshest value.
    upload_bps: i64,
}

/// 解析 `HH:MM` 为当日分钟数（0..1440）。非法输入返回 `None`。
fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// 定时边沿标识：`(queue_id, 是否启动边沿)`。启动/停止各是独立边沿，
/// 分别按天记账（见 `DownloadManager::schedule_fired`）。
type ScheduleEdge = (String, bool);

/// 定时调度的边沿判定核心（纯函数）。
///
/// 对每个启用定时且 `day_bit` 命中的队列，找出「今天时刻已过（`now_min`）
/// 且 `fired` 账本里今天尚未处理」的启动/停止边沿：
/// - 返回值 `.0` = 本次新越过的全部边沿（调用方应记账为 `today`，保证每
///   边沿每天至多处理一次——含手动启停后的不重复触发）；
/// - 返回值 `.1` = 应执行的动作 `(queue_id, 是否启动)`；同队列同天两个
///   边沿都新近越过时只保留时间靠后的那个，平局（start == stop）取停止。
fn due_schedule_actions<'a>(
    queues: impl Iterator<Item = &'a QueueInfo>,
    fired: &HashMap<ScheduleEdge, chrono::NaiveDate>,
    today: chrono::NaiveDate,
    day_bit: i32,
    now_min: u32,
) -> (Vec<ScheduleEdge>, Vec<ScheduleEdge>) {
    let mut passed_edges: Vec<ScheduleEdge> = Vec::new();
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
            passed_edges.push((q.queue_id.clone(), is_start));
            // 平局取停止边沿：迭代顺序 start 在前，`>=` 让后者覆盖——宁停不启。
            if newest.is_none_or(|(m, _)| minute >= m) {
                newest = Some((minute, is_start));
            }
        }
        if let Some((_, is_start)) = newest {
            actions.push((q.queue_id.clone(), is_start));
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
    /// 自定义 HTTP method（None = GET）。
    pub method: Option<String>,
    /// 捕获的请求体（POST 接管）。
    pub body: Option<downloader::CapturedRequestBody>,
    /// 音频轨 URL（「视频轨+音频轨」离散下载对）。
    pub audio_url: Option<String>,
    /// 稍后下载：true = 建任务后不启动（paused 入库），待「启动队列」
    /// 按序恢复或用户手动恢复。
    pub start_paused: bool,
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

/// off-actor resolve 的种类（start 或 resume 侧再入）。
#[cfg(feature = "plugins")]
#[derive(Debug, Clone, Copy)]
pub enum ResolveKind {
    Start,
    Resume,
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
    /// 用户在变体选择弹窗点关闭/取消 → 取消该任务（而非回退默认变体）。
    pub cancelled: bool,
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
    if let Some(v) = variants.into_iter().nth(idx as usize) {
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
    false
}

/// 把 resolve 结果应用到 QueuedTask（再入前）。非 ephemeral 保持 hint_file_size=0
/// 以正常 probe 取 ETag（保 resume If-Range 一致性）；ephemeral 走 skip-probe
/// hint 路径：知大小用 total_bytes，不知大小用 -1（跳 probe + 大小未知，与
/// resume 路径语义对称——绝不为 ephemeral 直链发出 probe 请求）。
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
    if let Some(headers) = res.extra_headers {
        queued.extra_headers = headers;
    }
    if res.audio_url.is_some() {
        queued.audio_url = res.audio_url;
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
    pending_queue: VecDeque<QueuedTask>,
    /// Global speed limiter shared with all HTTP/FTP download tasks.
    speed_limiter: SpeedLimiter,
    /// Shared BT session — lazily initialised on first BT download.
    /// All BT tasks share a single `librqbit::Session` (DHT, trackers,
    /// listening port, speed limits) to avoid per-task resource waste.
    /// Wrapped in `Arc` so spawned download tasks can cache handles.
    bt_session: Option<Arc<SharedBtSession>>,
    /// Default save directory used to initialise the BT session.
    default_save_dir: String,
    /// Application data directory (exe dir) for BT persistence files.
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
    /// 下载完成后是否把文件修改时间设为服务器提供的 `Last-Modified` 时间
    /// （config `use_server_time`，默认关闭）。
    use_server_time: bool,
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
    /// 文件跟踪扫描是否正在进行（防重叠）。内存级；`Arc` 以便 detached 扫描
    /// task 与调用方共享同一标志。
    scanning: Arc<AtomicBool>,
    /// Boost 模式当前优先任务 ID（内存级，重启清空）。None = 无优先任务。
    priority_task_id: Option<String>,
    /// 因 Boost 模式自动暂停的任务 ID 集合（内存级，重启清空）。
    /// 取消 Boost 时这些任务会自动恢复。
    auto_paused_ids: HashSet<String>,
    /// 任务级自动重试：网络 stall / 瞬时错误导致任务失败后，延迟自动恢复。
    /// key = task_id，value = 已自动重试次数。
    /// 超过 `max_auto_retries` 后不再重试，保持 error 状态等用户手动恢复。
    auto_retry_counts: HashMap<String, u32>,
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
    /// 当前正在下载（或排队准备启动）的任务已预订的临时文件路径集合。
    ///
    /// 用于解决 `dedup_filename` 的 TOCTOU 竞态：多个并发任务同时调用
    /// `dedup_filename` 时，都可能看到磁盘上同名文件不存在，进而选出相同
    /// 文件名并相互覆盖对方的 `.fdownloading` 临时文件，导致文件内容丢失。
    ///
    /// 修复策略：在 `do_start_task` 的同步段（`spawn` 之前）将该任务的
    /// 临时文件路径（`save_dir/file_name.fdownloading`）原子性地插入此集合，
    /// 并在 `on_task_done` / `cancel_task` / `delete_task` 时移除。
    /// `dedup_filename` 接收此集合的快照，在检查文件名冲突时同时排除
    /// 已被其他 in-flight 任务预订的路径，彻底消除批量下载中的文件名竞态。
    ///
    /// 由于整个 manager 运行在 `tokio::current_thread` 上，此字段无需加锁。
    reserved_temp_paths: HashSet<std::path::PathBuf>,
    /// 引擎事件接收端(进度/队列变化/分段拆分等)——由宿主注入。
    sink: Arc<dyn EventSink>,
    /// 需要宿主介入决策的选择接口(HLS 画质/BT 文件选择)——由宿主注入。
    selector: Arc<dyn HostSelection>,
    /// 插件管理器（Arc 共享）。`None` 直到 `install_plugin_manager` 注入。
    /// 仅 `plugins` feature 下存在；feature 关时无此字段、下载主链路零变化。
    #[cfg(feature = "plugins")]
    plugin_manager: Option<Arc<crate::plugin::PluginManager>>,
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
}

/// Configuration parameters for [`DownloadManager::new`].
/// Grouping avoids the `clippy::too_many_arguments` limit and makes
/// call sites self-documenting.
pub struct DownloadManagerConfig {
    pub max_concurrent: usize,
    pub speed_limit_bps: u64,
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
            default_save_dir,
            app_data_dir,
            data_dir,
            bt_config,
            proxy_config,
            user_agent,
        } = config;
        let client = downloader::build_client(&proxy_config, &user_agent)?;
        let (tx, rx) = mpsc::channel(8192);
        let (done_tx, done_rx) = mpsc::channel(64);
        let (retry_tx, retry_rx) = mpsc::channel(32);
        #[cfg(feature = "plugins")]
        let (resolve_tx, resolve_rx) = mpsc::unbounded_channel();
        #[cfg(feature = "plugins")]
        let (plugin_retry_tx, plugin_retry_rx) = mpsc::unbounded_channel();
        let limiter = SpeedLimiter::new(speed_limit_bps);
        limiter.spawn_refill_task();
        Ok(Self {
            db,
            client,
            proxy_config,
            active_tasks: HashMap::new(),
            generation: 0,
            progress_tx: tx,
            progress_rx: Some(rx),
            done_tx,
            done_rx: Some(done_rx),
            max_concurrent,
            pending_queue: VecDeque::new(),
            speed_limiter: limiter,
            bt_session: None,
            default_save_dir,
            app_data_dir,
            data_dir,
            bt_config,
            global_user_agent: user_agent,
            global_default_segments: 0,
            auto_max_connections: DEFAULT_AUTO_MAX_CONNECTIONS,
            use_server_time: false,
            queues: HashMap::new(),
            queue_limiters: HashMap::new(),
            schedule_fired: HashMap::new(),
            startup_reset_done: false,
            scanning: Arc::new(AtomicBool::new(false)),
            priority_task_id: None,
            auto_paused_ids: HashSet::new(),
            auto_retry_counts: HashMap::new(),
            max_auto_retries: DEFAULT_MAX_TASK_AUTO_RETRIES,
            auto_retry_delay_secs: DEFAULT_AUTO_RETRY_BASE_DELAY_SECS,
            retry_tx,
            retry_rx: Some(retry_rx),
            reserved_temp_paths: HashSet::new(),
            sink,
            selector,
            #[cfg(feature = "plugins")]
            plugin_manager: None,
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

    // ===================================================================
    // 插件系统（off-actor 惰性 resolve / 通知 / 命令式重试）
    // 仅 `plugins` feature 下编译；feature 关时下载主链路零变化。
    // ===================================================================

    /// 注入插件管理器（Engine::new 构造后调用）。
    #[cfg(feature = "plugins")]
    pub fn install_plugin_manager(&mut self, pm: Arc<crate::plugin::PluginManager>) {
        self.plugin_manager = Some(pm);
    }

    /// 获取插件管理器（供 hub/server ApiHost 实现读操作 + 集成测试）。
    #[cfg(feature = "plugins")]
    pub fn plugin_manager(&self) -> Option<Arc<crate::plugin::PluginManager>> {
        self.plugin_manager.clone()
    }

    /// 构造一个市场客户端（读 config `market_index_sources` 作为自定义索引源，
    /// 空则用内置候选源）。供 hub/server ApiHost 的市场浏览/安装方法调用。
    #[cfg(feature = "plugins")]
    pub async fn market_client(&self) -> Option<crate::plugin::MarketClient> {
        let pm = self.plugin_manager.clone()?;
        let all = self.db.get_all_config().await.unwrap_or_default();
        let sources = crate::plugin::MarketClient::source_config(&all);
        Some(crate::plugin::MarketClient::new(
            pm,
            self.db.clone(),
            sources,
        ))
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

    /// 纯 Rust glob 首匹配（同步逻辑，async 仅因读 RwLock）。feature 关时恒空。
    #[cfg(feature = "plugins")]
    async fn plugin_match_resolver(&self, url: &str) -> String {
        match &self.plugin_manager {
            Some(pm) => pm.match_resolver(url).await.unwrap_or_default(),
            None => String::new(),
        }
    }
    #[cfg(not(feature = "plugins"))]
    async fn plugin_match_resolver(&self, _url: &str) -> String {
        String::new()
    }

    /// off-actor resolve worker：在插件专用 runtime 上 spawn（禁裸 tokio::spawn），
    /// panic 隔离，无条件回流（交 on_resolve_ready 兜底）。
    #[cfg(feature = "plugins")]
    fn spawn_resolve_worker(
        &self,
        task_id: String,
        identity: String,
        req: crate::plugin::ResolveRequest,
        kind: ResolveKind,
        generation: u64,
    ) {
        use futures_util::FutureExt;
        let Some(pm) = self.plugin_manager.clone() else {
            return;
        };
        let tx = self.resolve_tx.clone();
        let handle = pm.runtime_handle();
        let id_for_worker = identity.clone();
        let selector = self.selector.clone();
        handle.spawn(async move {
            let fut = std::panic::AssertUnwindSafe(pm.resolve(&id_for_worker, req));
            let mut result = match fut.catch_unwind().await {
                Ok(r) => r,
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "resolver panicked".to_string());
                    Err(crate::plugin::PluginError::Runtime(msg))
                }
            };
            // 多变体收敛：在 off-actor worker 内 await 用户选择（不阻塞 actor），
            // 收敛为单一直链后再回流。stale 场景由 on_resolve_ready 世代守卫兜底。
            // 返回 true = 用户点关闭/取消 → 回流标记 cancelled，actor 取消任务。
            let mut cancelled = false;
            if let Ok(Some(res)) = &mut result
                && !res.variants.is_empty()
            {
                cancelled = collapse_resolve_variants(&task_id, res, selector.as_ref()).await;
            }
            let _ = tx.send(ResolveOutcome {
                task_id,
                identity,
                kind,
                generation,
                result,
                cancelled,
            });
        });
    }

    /// resolve 回流再入（actor 上下文）。先复查 DB 生命周期，再按结果分派。
    #[cfg(feature = "plugins")]
    pub async fn on_resolve_ready(&mut self, out: ResolveOutcome) {
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
        // 用户在变体选择弹窗点关闭/取消 → 取消该任务（cancel_task 会清 pending_resolve /
        // active_tasks 占位、置 status=4、drain_queue）。放在世代守卫之后：确认本 outcome
        // 拥有当前 pending 条目才动手。
        if out.cancelled {
            log_info!(
                "[plugin-resolve] task {} variant selection cancelled by user, cancelling task",
                task_id
            );
            self.cancel_task(&task_id).await;
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
                // Ok(Some) = 改写；Ok(None) = 放行（用原 url）。
                match out.kind {
                    ResolveKind::Start => {
                        if let Some(PendingResolve::Start { mut queued, .. }) =
                            self.pending_resolve.remove(&task_id)
                        {
                            if let Some(res) = applied {
                                apply_resolve_to_queued(&mut queued, res);
                            }
                            queued.resolved = true;
                            self.do_start_task(*queued).await;
                            self.drain_queue().await;
                        }
                    }
                    ResolveKind::Resume => {
                        self.pending_resolve.remove(&task_id);
                        self.active_tasks.remove(&task_id);
                        // Some(res) 改写；None → 空 ResolveResult 表示放行（用原 url）。
                        // 经 resume_applied 字段传给 do_resume_task（避免改其签名/所有调用点）。
                        self.resume_applied
                            .insert(task_id.clone(), applied.unwrap_or_default());
                        self.do_resume_task(&task_id).await;
                        self.drain_queue().await;
                    }
                }
            }
            Err(e) => {
                let msg = format!("[插件] {}: {}", out.identity, e);
                let _ = self.db.update_task_status(&task_id, 4, &msg).await;
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
                });
                self.pending_resolve.remove(&task_id);
                self.active_tasks.remove(&task_id);
                self.drain_queue().await;
            }
        }
    }

    /// 插件 onError 命令式重试（actor 上下文，复用 auto_retry 账本限流）。
    #[cfg(feature = "plugins")]
    pub async fn plugin_request_retry(&mut self, task_id: &str, delay_ms: u64) {
        let max_retries = self.max_auto_retries;
        if max_retries == 0 {
            return;
        }
        let terminal_error = self
            .db
            .load_task_by_id(task_id)
            .await
            .ok()
            .flatten()
            .map(|t| t.status == 4)
            .unwrap_or(false);
        if !terminal_error {
            return;
        }
        let count = self
            .auto_retry_counts
            .entry(task_id.to_string())
            .or_insert(0);
        if max_retries != -1 && (*count as i32) >= max_retries {
            log_info!("[plugin] task {} 重试已达上限，忽略 requestRetry", task_id);
            return;
        }
        *count += 1;
        let tx = self.retry_tx.clone();
        let tid = task_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let _ = tx.send(tid).await;
        });
    }

    /// do_start_task 体首守卫调用：占位 + 存 pending_resolve + spawn off-actor worker。
    #[cfg(feature = "plugins")]
    async fn begin_resolve_start(&mut self, queued: QueuedTask) {
        let task_id = queued.task_id.clone();
        let identity = queued.resolver_plugin_id.clone();
        // 占位插入 active_tasks（用原始 url 算 is_bt，真实 generation/cancel_token）：
        // resolve 期间任务可被 pause/cancel 触达、正确占并发计数；再入时被覆盖。
        self.generation += 1;
        let spawn_gen = self.generation;
        let is_bt = is_magnet(&queued.url)
            || !queued.torrent_file_bytes.is_empty()
            || is_torrent_file_url(&queued.url);
        self.active_tasks.insert(
            task_id.clone(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: spawn_gen,
                handle: None,
                is_bt,
                queue_id: queued.queue_id.clone(),
            },
        );
        let req = crate::plugin::ResolveRequest {
            task_id: task_id.clone(),
            url: queued.url.clone(),
            cookies: queued.cookies.clone(),
            referrer: queued.referrer.clone(),
            user_agent: queued.user_agent.clone(),
            extra_headers: queued.extra_headers.clone(),
        };
        self.pending_resolve.insert(
            task_id.clone(),
            PendingResolve::Start {
                queued: Box::new(queued),
                generation: spawn_gen,
            },
        );
        self.spawn_resolve_worker(task_id, identity, req, ResolveKind::Start, spawn_gen);
    }

    /// do_resume_task 体首守卫调用：对称占位（防 resumeAll 并发双 resolve）+ spawn。
    #[cfg(feature = "plugins")]
    async fn begin_resolve_resume(&mut self, task_id: &str, identity: String) {
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            _ => return,
        };
        // 对称占位：resolve-wait 期间 task 须在 active_tasks，否则 resume_task_inner
        // 重入检查恒 false，resumeAll/双击/自动重试会并发 spawn 第二个 resolve。
        self.generation += 1;
        let spawn_gen = self.generation;
        let is_bt = is_bt_url(&task.url);
        self.active_tasks.insert(
            task_id.to_string(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: spawn_gen,
                handle: None,
                is_bt,
                queue_id: task.queue_id.clone(),
            },
        );
        let (cookies, referrer, extra_headers) = self
            .db
            .load_task_request_context(task_id)
            .await
            .ok()
            .flatten()
            .map(|(c, r, h)| {
                let headers: std::collections::HashMap<String, String> =
                    serde_json::from_str(&h).unwrap_or_default();
                (c, r, headers)
            })
            .unwrap_or_default();
        let req = crate::plugin::ResolveRequest {
            task_id: task_id.to_string(),
            url: task.url.clone(),
            cookies,
            referrer,
            user_agent: String::new(),
            extra_headers,
        };
        self.pending_resolve.insert(
            task_id.to_string(),
            PendingResolve::Resume {
                generation: spawn_gen,
            },
        );
        self.spawn_resolve_worker(
            task_id.to_string(),
            identity,
            req,
            ResolveKind::Resume,
            spawn_gen,
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
        self.db
            .load_task_by_id(task_id)
            .await
            .ok()
            .flatten()
            .map(|t| t.status == 4 && t.error_message != CANCELLED_ERROR_MESSAGE)
            .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // Configuration update methods (called from actor when SaveConfig arrives)
    // -----------------------------------------------------------------------

    /// Update max concurrent tasks limit.  Immediately drains the queue
    /// if the new limit allows more active tasks.
    pub async fn set_max_concurrent(&mut self, max: usize) {
        self.max_concurrent = max;
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

    /// Update whether completed downloads adopt the server-provided
    /// `Last-Modified` timestamp as the file's modification time
    /// (config `use_server_time`). Takes effect for downloads started
    /// after the change; already-running downloads keep the value they
    /// were spawned with.
    pub fn set_use_server_time(&mut self, v: bool) {
        self.use_server_time = v;
    }

    /// Update global speed limit (bytes/sec).  Takes effect immediately on
    /// all active and future HTTP/FTP/BT downloads and BT uploads.  0 = unlimited.
    pub fn set_speed_limit(&mut self, bps: u64) {
        self.speed_limiter.set_limit(bps);
        // Synchronise speed limit to the shared BT session (if initialised).
        if let Some(ref bt) = self.bt_session {
            bt.set_speed_limit(bps);
            bt.set_upload_speed_limit(bps);
        }
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
        self.proxy_config = config;
        // 网络出口变化：域名连接上限是对【旧出口】的服务器策略观察，
        // 换代理后不再可信，清空重学（内存 + 持久化）。
        crate::segment_coordinator::clear_domain_conn_caps(&self.db);
        Ok(())
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
        if was_active {
            self.pause_task(task_id).await;
        }
        self.db.update_task_segments(task_id, seg).await?;
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
    #[allow(dead_code)]
    pub fn proxy_config(&self) -> &ProxyConfig {
        &self.proxy_config
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
        self.global_user_agent = ua;
        Ok(())
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
        if self.bt_session.is_none() {
            let speed_limit = self.speed_limiter.limit();
            let save_dir = self.default_save_dir.clone();
            let data_dir = self.app_data_dir.clone();
            let config = self.bt_config.clone();
            let session = tokio::task::spawn_blocking(move || {
                SharedBtSession::new(&save_dir, &data_dir, speed_limit, &config)
            })
            .await
            .map_err(|e| {
                downloader::DownloadError::Other(format!("BT session init thread panicked: {e}"))
            })??;
            self.bt_session = Some(Arc::new(session));
        }
        Ok(())
    }

    /// Update BT configuration.  The new config will take effect when the
    /// next BT session is created (either on first BT download or after
    /// `invalidate_bt_session` is called).
    pub fn set_bt_config(&mut self, config: BtConfig) {
        self.bt_config = config;
    }

    /// Periodically account for seeding uploads and evaluate ratio/time limits.
    ///
    /// This is a cheap no-op when no BT session exists or no seeders are active.
    /// Upload bytes are persisted and emitted so the UI can show live upload
    /// speed/ratio; seeders that exceed configured limits are stopped (or
    /// removed, depending on the configured `seed_then_action`).
    pub async fn tick_seeding_evaluation(&mut self) {
        self.account_seeding_uploads().await;
        let to_stop = self.evaluate_seeding_limits().await;
        let then_action =
            crate::bt_seeding::SeedingThenAction::parse(&self.bt_config.seed_then_action);
        for (task_id, reason) in to_stop {
            let short = &task_id[..task_id.len().min(8)];
            log_info!("[manager] stopping seeder {}: {}", short, reason.message());

            let bt = self.bt_session.clone();
            if let Some(bt) = bt {
                let _ = bt.unregister_seeder(&task_id).await;
                let _ = bt.pause_task(&task_id).await;
            }

            if let Ok(Some(t)) = self.db.load_task_by_id(&task_id).await {
                match then_action {
                    crate::bt_seeding::SeedingThenAction::DeleteTask => {
                        let _ = self
                            .db
                            .update_task_seeding_status(
                                &task_id,
                                reason.as_i32(),
                                reason.message(),
                            )
                            .await;
                        self.delete_task(&task_id, false).await;
                        continue;
                    }
                    crate::bt_seeding::SeedingThenAction::DeleteTaskAndFiles => {
                        let _ = self
                            .db
                            .update_task_seeding_status(
                                &task_id,
                                reason.as_i32(),
                                reason.message(),
                            )
                            .await;
                        self.delete_task(&task_id, true).await;
                        continue;
                    }
                    crate::bt_seeding::SeedingThenAction::Stop => {
                        let _ = self
                            .db
                            .update_task_seeding_status(&task_id, reason.as_i32(), reason.message())
                            .await;
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
                        });
                    }
                }
            }
        }
    }

    /// Persist and emit upload stats for every active seeder.
    ///
    /// Uses delta accumulation so `tasks.uploaded_bytes` stays correct across
    /// librqbit counter resets (pause/resume or session rebuild).
    async fn account_seeding_uploads(&self) {
        let Some(ref bt) = self.bt_session else {
            return;
        };
        let seeding_mgr = bt.seeding_manager();
        let task_ids = seeding_mgr.all_task_ids().await;
        for task_id in task_ids {
            let Some(handle) = seeding_mgr.get_handle(&task_id).await else {
                continue;
            };
            let stats = handle.stats();
            let Some(live) = stats.live.as_ref() else {
                // No live snapshot while paused — do not overwrite with zero.
                continue;
            };
            let snapshot_uploaded = live.snapshot.uploaded_bytes as i64;
            let upload_speed_bps = (live.upload_speed.mbps * 1024.0 * 1024.0) as i64;

            let Some(delta) = seeding_mgr
                .apply_upload_snapshot(&task_id, snapshot_uploaded, upload_speed_bps)
                .await
            else {
                continue;
            };

            let new_total = match self.db.add_task_uploaded_bytes(&task_id, delta).await {
                Ok(n) => n,
                Err(e) => {
                    log_info!("[manager] add_task_uploaded_bytes error: {}", e);
                    continue;
                }
            };

            if let Ok(Some(t)) = self.db.load_task_by_id(&task_id).await {
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
                    upload_speed_bps,
                    uploaded_bytes: new_total,
                    seeding_status: 1,
                    seeding_message: String::new(),
                });
            }
        }
    }

    /// Evaluate configured seeding limits for every active seeder.
    ///
    /// Uses the persisted cumulative `uploaded_bytes` and `downloaded_bytes`
    /// from the DB so ratio limits are not under-counted across librqbit
    /// session resets.
    async fn evaluate_seeding_limits(&self) -> Vec<(String, SeedingStopReason)> {
        let Some(ref bt) = self.bt_session else {
            return Vec::new();
        };

        let config = SeedingLimitConfig {
            ratio_limit: self.bt_config.seed_ratio_limit,
            post_ratio_limit: self.bt_config.seed_post_ratio_limit,
            seed_time_limit_minutes: self.bt_config.seed_time_limit_minutes,
            inactive_time_limit_minutes: self.bt_config.seed_inactive_time_limit_minutes,
            operator: self.bt_config.seed_limit_operator,
            then_action: crate::bt_seeding::SeedingThenAction::parse(
                &self.bt_config.seed_then_action,
            ),
        };
        if !config.has_enabled_conditions() {
            return Vec::new();
        }

        let seeding_mgr = bt.seeding_manager();
        let task_ids = seeding_mgr.all_task_ids().await;
        if task_ids.is_empty() {
            return Vec::new();
        }

        let now_unix = chrono::Local::now().timestamp();

        // Build snapshots for limit evaluation from live stats and DB totals.
        let mut snapshots: HashMap<String, SeedingUploadSnapshot> = HashMap::new();
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

            let total_uploaded = self.db.get_task_uploaded_bytes(task_id).await.unwrap_or(0);
            let total_downloaded = self
                .db
                .get_task_downloaded_bytes(task_id)
                .await
                .unwrap_or(1);

            snapshots.insert(
                task_id.clone(),
                SeedingUploadSnapshot {
                    total_uploaded,
                    total_downloaded,
                    upload_speed_bps,
                },
            );
        }

        seeding_mgr
            .evaluate_limits(&config, now_unix, |id| {
                snapshots.get(id).copied().unwrap_or_default()
            })
            .await
    }

    /// Invalidate (destroy) the current BT session so it will be re-created
    /// with the latest `bt_config` on the next BT download.  Active BT
    /// downloads are gracefully paused first so their progress is preserved
    /// and they appear as "paused" (status 2) in the UI.
    pub async fn invalidate_bt_session(&mut self) {
        if self.bt_session.is_none() {
            return;
        }

        // 1. Collect all active BT task IDs.
        let bt_task_ids: Vec<String> = self
            .active_tasks
            .iter()
            .filter(|(_, e)| e.is_bt)
            .map(|(id, _)| id.clone())
            .collect();

        // 1b. Mark any active seeders as stopped because the whole BT session is
        // about to be released. This prevents stale "seeding" UI state.
        if let Some(ref bt) = self.bt_session {
            let seeder_ids = bt.seeding_manager().all_task_ids().await;
            for tid in &seeder_ids {
                let _ = bt.unregister_seeder(tid).await;
                let _ = self
                    .db
                    .update_task_seeding_status(
                        tid,
                        crate::bt_seeding::SeedingStopReason::SessionReleased.as_i32(),
                        crate::bt_seeding::SeedingStopReason::SessionReleased.message(),
                    )
                    .await;
                if let Ok(Some(t)) = self.db.load_task_by_id(tid).await {
                    self.sink.emit(EngineEvent::TaskProgress {
                        task_id: tid.clone(),
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
                        seeding_status: crate::bt_seeding::SeedingStopReason::SessionReleased
                            .as_i32(),
                        seeding_message: crate::bt_seeding::SeedingStopReason::SessionReleased
                            .message()
                            .to_string(),
                    });
                }
            }
        }

        // 2. Gracefully pause each active BT task (cancel token, persist
        //    progress, update DB status to paused, notify Dart).
        if !bt_task_ids.is_empty() {
            log_info!(
                "[manager] pausing {} active BT task(s) before session invalidation",
                bt_task_ids.len()
            );
            for tid in &bt_task_ids {
                if let Some(entry) = self.active_tasks.remove(tid) {
                    entry.token.cancel();

                    // Pause the torrent handle in the session so librqbit
                    // flushes its piece-level state to disk.
                    if let Some(ref bt) = self.bt_session {
                        let _ = bt.pause_task(tid).await;
                    }

                    let _ = self.db.update_task_status(tid, 2, "").await;

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
                        });

                        self.send_segments_from_db(tid, t.total_bytes).await;
                    }

                    // Boost guard: if the paused task was the current priority
                    // (Boost) target, cancel Boost and resume other tasks.
                    if self.priority_task_id.as_deref() == Some(tid.as_str()) {
                        self.clear_priority().await;
                    }
                }
            }

            // Give in-flight BT download loops a moment to detect
            // cancellation and exit cleanly before we tear down the runtime.
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        }

        // 2b. 等待仍在进行的 detached `add_torrent` 任务（如 magnet 的 DHT
        //     元数据解析）结束后再关停。这些任务持有 `Arc<Session>`，绑定着
        //     BT 监听端口；若在它们结束前关停并重建 session，下一次 BT 下载
        //     会因端口仍被占用而立即失败。与 `maybe_release_bt_session` 的
        //     inflight 检查对齐——固定 600ms 是经验值，无法保证 add_torrent
        //     已完成，故在此显式轮询直至归零或超时。
        if let Some(ref bt) = self.bt_session {
            let deadline = tokio::time::Instant::now() + INVALIDATE_INFLIGHT_TIMEOUT;
            while bt.has_inflight_adds() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(INVALIDATE_INFLIGHT_POLL_INTERVAL).await;
            }
            if bt.has_inflight_adds() {
                log_info!(
                    "[manager] invalidate: inflight add_torrent still pending after timeout, forcing shutdown"
                );
            }
        }

        // 3. Destroy the session on a background thread (block_on inside).
        if let Some(bt) = self.bt_session.take() {
            log_info!("[manager] invalidating BT session for config change");
            std::thread::spawn(move || match Arc::try_unwrap(bt) {
                Ok(owned) => owned.shutdown(),
                Err(shared) => shared.shutdown(),
            });
        }
    }

    /// 广播当前 pending_queue 中所有任务的队列位置（每次队列变化后调用）
    fn broadcast_queue_positions(&self) {
        let positions: Vec<QueuePosition> = self
            .pending_queue
            .iter()
            .enumerate()
            .map(|(i, q)| QueuePosition {
                task_id: q.task_id.clone(),
                position: (i + 1) as i32,
            })
            .collect();
        self.sink
            .emit(EngineEvent::QueuePositionsChanged(positions));
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
                        limiter.set_limit((q.speed_limit_kbps.max(0) as u64) * 1024);
                    }
                    self.queues.insert(q.queue_id.clone(), q);
                }
            }
            Err(e) => log_info!("[manager] load_queues error: {}", e),
        }
    }

    /// Whether we have a free slot for a new download.
    ///
    /// BT tasks and active seeders now count toward the same global
    /// `max_concurrent` ceiling as HTTP/FTP downloads. `0` means unlimited.
    async fn has_capacity(&self) -> bool {
        if self.max_concurrent == 0 {
            return true;
        }
        let active = self.active_tasks.len();
        let seeding = if let Some(bt) = self.bt_session.as_ref() {
            bt.seeding_manager().active_count().await as usize
        } else {
            0
        };
        active + seeding < self.max_concurrent
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
        let queue_max = self
            .queues
            .get(queue_id)
            .map(|q| q.max_concurrent as usize)
            .unwrap_or(0);
        if queue_max == 0 {
            return true;
        }
        let active_in_queue = self
            .active_tasks
            .values()
            .filter(|e| e.queue_id.as_str() == queue_id)
            .count();
        active_in_queue < queue_max
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

    /// Try to start tasks from the pending queue until we run out of capacity.
    ///
    /// Queue-aware: tasks blocked only by their queue's concurrent limit are
    /// skipped so that tasks from other queues (or the default queue) can
    /// proceed, rather than blocking the entire pending queue.
    async fn drain_queue(&mut self) {
        // Drain into a Vec up-front so every removal is O(1) via iteration
        // instead of O(n) per `VecDeque::remove(i)`.  Total cost: O(n).
        let pending: Vec<_> = self.pending_queue.drain(..).collect();
        let mut kept = VecDeque::with_capacity(pending.len());
        let mut global_full = false;

        for queued in pending {
            // Once global capacity is exhausted, keep all remaining items
            // without further checks (matches the original early-break).
            if global_full {
                kept.push_back(queued);
                continue;
            }
            // Global concurrency ceiling reached — keep this and the rest.
            if !self.has_capacity().await {
                kept.push_back(queued);
                global_full = true;
                continue;
            }
            // Edge case: task was resumed/cancelled while queued — drop it.
            if self.active_tasks.contains_key(&queued.task_id) {
                continue;
            }
            // Queue-level concurrency check: keep (don't start) if the
            // target queue is full; it may be drained on a future call.
            if !self.has_queue_capacity(&queued.queue_id) {
                kept.push_back(queued);
                continue;
            }
            // Start the task.
            if queued.is_resume {
                self.do_resume_task(&queued.task_id).await;
            } else {
                self.do_start_task(queued).await;
            }
        }

        self.pending_queue = kept;
        // 队列变化后广播最新位置
        self.broadcast_queue_positions();
    }

    // -----------------------------------------------------------------------
    // Public task operations
    // -----------------------------------------------------------------------

    /// Remove a finished task from active_tokens (called by actor loop).
    /// Only removes the entry if the generation matches, preventing a stale
    /// `TaskDone` from an old spawn from accidentally removing a newer token.
    pub async fn on_task_done(&mut self, done: &TaskDone) {
        let task_id = done.task_id.as_str();
        let generation = done.generation;

        let generation_matched = self
            .active_tasks
            .get(task_id)
            .map(|e| e.generation == generation)
            .unwrap_or(false);

        // Release the file-name reservation unconditionally (success, error,
        // or cancel) so the slot is freed for the next task that picks the
        // same filename.
        if let Some(ref path) = done.reserved_temp_path {
            self.reserved_temp_paths.remove(path);
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

        // A slot freed up — try to start queued tasks.
        // SAFETY (current_thread): `remove` + `drain_queue` have no `.await` between
        // them at this point, so no other task can observe the partially-updated state.
        // If this code is ever ported to a multi-threaded runtime, a lock around
        // `active_tokens` modifications would be required.
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
            // max == 0：用户关闭了自动重试，直接跳过（不分配计数）。
            if max_retries != 0 && task.status == 4 && is_retriable_error(&task.error_message) {
                let count = self
                    .auto_retry_counts
                    .entry(task_id.to_string())
                    .or_insert(0);
                // 无限（-1）时不设上限；否则按 *count < max 判定。
                if max_retries == -1 || (*count as i32) < max_retries {
                    *count += 1;
                    let attempt = *count;
                    // 延迟 = 基础值 × 已重试次数（递增），但封顶到 MAX_AUTO_RETRY_DELAY_SECS，
                    // 避免无限模式下退避无界增长。
                    //
                    // 防护：无限重试（-1）模式下强制 base 至少 1s。否则当用户同时选了
                    // "无限重试"+"延迟 0s"（两者皆为合法 UI 取值）时，delay 恒为 0、
                    // 计数无上限 → 对永久失活的主机（connection refused 在毫秒级失败且
                    // 被判定为可重试）形成零延迟热循环，疯狂重连、刷 DB、永不进入
                    // 稳定 error 态。有限次数模式仍允许 0 延迟（最多重试 n 次后自然停止）。
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
                        let _ = tx.send(tid).await;
                    });
                } else {
                    log_info!(
                        "[manager] auto-retry exhausted for task {} ({} attempts), staying in error",
                        task_id,
                        max_retries
                    );
                }
            } else if task.status == 3 {
                // 任务成功完成，清除重试计数
                self.auto_retry_counts.remove(task_id);
            }

            // 通知平面：onDone / onError（fire-and-forget）。onError 内脚本可经
            // flux.task.requestRetry 命令式重试（受 max_auto_retries 约束）。
            #[cfg(feature = "plugins")]
            if let Some(pm) = &self.plugin_manager {
                if task.status == 3 {
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
                }
            }
        }

        self.maybe_wal_checkpoint().await;
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
    async fn maybe_release_bt_session(&mut self) {
        if self.bt_session.is_none() {
            return;
        }
        // Keep the session alive if any BT tasks are actively downloading.
        if self.active_tasks.values().any(|e| e.is_bt) {
            return;
        }
        // Keep the session alive if any completed torrents are still seeding.
        if let Some(ref bt) = self.bt_session
            && bt.has_seeders().await
        {
            log_info!("[manager] deferring BT session release — seeders active");
            return;
        }
        // BT tasks bypass the pending queue, so this guard is purely
        // defensive in case the invariant changes in the future.
        if self.pending_queue.iter().any(|q| is_bt_url(&q.url)) {
            return;
        }
        // Keep the session alive while any detached `add_torrent` task is
        // still running.  Those tasks hold an `Arc<Session>` that keeps the
        // BT listening port bound; creating a new session while the old port
        // is in use causes the next BT download to fail immediately.
        if let Some(ref bt) = self.bt_session
            && bt.has_inflight_adds()
        {
            log_info!(
                "[manager] deferring BT session release — detached add_torrent still in flight"
            );
            return;
        }
        log_info!("[manager] all BT tasks finished/paused — releasing BT session");
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
    /// # use fluxdown_engine::bt_downloader::BtConfig;
    /// # use fluxdown_engine::proxy_config::ProxyConfig;
    /// # async fn run() -> Result<(), fluxdown_engine::EngineError> {
    /// # let config = EngineConfig { max_concurrent: 5, speed_limit_bps: 0, default_save_dir: "/tmp/downloads".to_string(), app_data_dir: "/tmp/fluxdown".to_string(), bt_config: BtConfig::default(), proxy_config: ProxyConfig::default(), user_agent: String::new(), data_dir_override: None, database_url: None };
    /// let engine = Engine::new(config, Arc::new(NoopSink), Arc::new(NoopSelection)).await?;
    /// engine.manager.spawn_file_scan();
    /// # Ok(())
    /// # }
    /// ```
    pub fn spawn_file_scan(&self) {
        let db = self.db.clone();
        let sink = self.sink.clone();
        let scanning = self.scanning.clone();
        tokio::spawn(async move {
            scan_missing_files(db, sink, scanning).await;
        });
    }

    /// Reset seeding state left over from a previous session.
    ///
    /// librqbit does not restore seeders across restarts (we intentionally clear
    /// its session.json), so any task that was seeding when the app exited will
    /// be stuck with `seeding_status = 1` but no actual peer connections.
    /// Normalize those rows to `UserStopped` and clear the persisted start time.
    pub async fn reset_stale_seeding(&self) {
        let stale = match self.db.load_tasks_with_seeding_status(1).await {
            Ok(t) => t,
            Err(e) => {
                log_info!("[manager] load_tasks_with_seeding_status error: {}", e);
                return;
            }
        };
        for t in stale {
            let short = &t.task_id[..t.task_id.len().min(8)];
            log_info!(
                "[manager] resetting stale seeding state for task {} to user-stopped",
                short
            );
            let _ = self
                .db
                .update_task_seeding_status(
                    &t.task_id,
                    crate::bt_seeding::SeedingStopReason::UserStopped.as_i32(),
                    crate::bt_seeding::SeedingStopReason::UserStopped.message(),
                )
                .await;
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
            });
        }
    }

    pub async fn load_and_send_all_tasks(&mut self) {
        // 启动时将残留的 downloading/pending 状态矫正为 paused（仅首次执行）
        // 后续由 create_task / batch_create 触发时不重复重置，避免将刚插入的
        // pending 任务误改为 paused 导致前端显示"已暂停"
        let is_first_run = !self.startup_reset_done;
        if is_first_run {
            self.startup_reset_done = true;
            if let Err(e) = self.db.reset_incomplete_tasks_to_paused().await {
                log_info!("reset_incomplete_tasks_to_paused error: {}", e);
            }
        }

        let tasks = match self.db.load_all_tasks().await {
            Ok(t) => t,
            Err(e) => {
                log_info!("load_all_tasks error: {}", e);
                Vec::new()
            }
        };

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
        if is_first_run {
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
            let mut save_dirs: std::collections::HashSet<&str> = std::collections::HashSet::new();
            save_dirs.insert(self.default_save_dir.as_str());
            for t in &tasks {
                save_dirs.insert(t.save_dir.as_str());
            }

            // Identify completed BT tasks whose staging dir still exists so
            // we can attempt a rescue move before unconditional cleanup.
            // Owned tuples:rescue 内含 move_path(最坏 2s 瞬时锁重试退避),
            // 必须经 spawn_blocking 跑,不能在 current_thread runtime 上同步
            // 阻塞(会冻结进度上报/FFI 响应)。
            let rescue_input: Vec<(String, String, String)> = task_map
                .iter()
                .filter_map(|(&id, (status, save_dir, file_name, _))| {
                    if *status != 3 {
                        return None;
                    }
                    let stage = bt_downloader::bt_stage_dir(save_dir, id);
                    if stage.exists() {
                        Some((id.to_string(), save_dir.to_string(), file_name.to_string()))
                    } else {
                        None
                    }
                })
                .collect();

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
                if let Ok(rows) = self.db.list_config_with_prefix("bt_completion_top_").await {
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
                }
                let rescued = tokio::task::spawn_blocking(move || {
                    bt_downloader::rescue_stranded_staging_files(&rescue_input, &rescue_claims)
                })
                .await
                .unwrap_or_default();
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
            for save_dir in &save_dirs {
                let dir = std::path::Path::new(save_dir);
                let entries = match std::fs::read_dir(dir) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for entry in entries.filter_map(|e| e.ok()) {
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
                            if let Err(e) = std::fs::remove_dir_all(&path) {
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
                            let has_real_data = std::fs::read_dir(&path)
                                .map(|rd| {
                                    rd.filter_map(|e| e.ok())
                                        .any(|e| e.metadata().map(|m| m.len() > 0).unwrap_or(false))
                                })
                                .unwrap_or(false);
                            if has_real_data {
                                log_info!(
                                    "[manager] startup cleanup: keeping completed-task staging dir {} (still has real data; rescue likely partially failed)",
                                    path.display()
                                );
                            } else {
                                log_info!(
                                    "[manager] startup cleanup: removing completed-task staging dir {}",
                                    path.display()
                                );
                                if let Err(e) = std::fs::remove_dir_all(&path) {
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
                            let has_real_data = std::fs::read_dir(&path)
                                .map(|rd| {
                                    rd.filter_map(|e| e.ok())
                                        .any(|e| e.metadata().map(|m| m.len() > 0).unwrap_or(false))
                                })
                                .unwrap_or(false);
                            if has_real_data {
                                log_info!(
                                    "[manager] startup cleanup: keeping staging dir {} (task active/paused, has data)",
                                    path.display()
                                );
                            } else {
                                log_info!(
                                    "[manager] startup cleanup: removing empty staging dir {} (task active/paused but no real data)",
                                    path.display()
                                );
                                if let Err(e) = std::fs::remove_dir_all(&path) {
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

        // Snapshot task info before sending AllTasks (which consumes `tasks`).
        let task_snapshots: Vec<(String, i64)> = tasks
            .iter()
            .map(|t| (t.task_id.clone(), t.total_bytes))
            .collect();

        self.sink.emit(EngineEvent::TasksSnapshot(tasks));

        // Send persisted segment data for each task so the UI can display
        // download distribution immediately after app restart.
        for (task_id, total_bytes) in &task_snapshots {
            self.send_segments_from_db(task_id, *total_bytes).await;
        }
        if is_first_run {
            // 文件跟踪：仅进程启动时扫一次；运行期检测交给 RescanFiles（桌面/
            // 移动聚焦）与 headless 定时器两条专属触发路径。
            self.spawn_file_scan();
            // librqbit 不跨重启恢复做种，把残留做种态重置为 UserStopped。
            self.reset_stale_seeding().await;
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
        if let Ok(db_segs) = self.db.load_segments(task_id).await
            && !db_segs.is_empty()
        {
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

    /// 创建下载任务，返回新任务 ID（插入失败时 `None`）。
    ///
    /// `spec.start_paused` = 稍后下载：任务以 paused(2) 落库，不占并发、
    /// 不进等待队列，由「启动队列」按序恢复或用户手动恢复；后台元数据
    /// 探测照常进行，UI 能立即显示文件名/大小。
    pub async fn create_task(&mut self, spec: NewTaskSpec) -> Option<String> {
        let NewTaskSpec {
            url,
            save_dir,
            file_name,
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
            extra_headers,
            selected_file_indices,
            method,
            body,
            audio_url,
            start_paused,
        } = spec;
        // 任务必属队列：未指定时归入内置主队列（'' 不再是有效归属，统一
        // 覆盖旧客户端信号 / aria2 / REST / CLI 等所有创建入口）。
        let queue_id = if queue_id.is_empty() {
            MAIN_QUEUE_ID.to_string()
        } else {
            queue_id
        };
        let task_id = Uuid::new_v4().to_string();
        let created_id = task_id.clone();
        // ED2K 链接自带文件名/大小/root hash：调用方未显式给名时从链接回填，
        // 并把 hint_file_size 设为链接声明的大小（run_ed2k_download 以链接为准）。
        let (file_name, hint_file_size) = if crate::ed2k::link::is_ed2k_url(&url) {
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
        // When segments <= 0 ("auto"), store 0 in DB and let the downloader
        // dynamically calculate the optimal count after probing file size,
        // CPU cores, and bandwidth.
        let seg = if segments <= 0 { 0 } else { segments };

        // Determine the URL to store in DB.  For .torrent file tasks, use a
        // sentinel URL since the actual content is in torrent_file_bytes.
        let db_url = if !torrent_file_bytes.is_empty() {
            "torrent-file://local".to_string()
        } else {
            url.clone()
        };

        // 稍后下载以 paused(2) 落库；正常创建 pending(0)。
        let initial_status = if start_paused { 2 } else { 0 };
        if let Err(e) = self
            .db
            .insert_task_with_tls_policy(
                &task_id,
                &db_url,
                &file_name,
                &save_dir,
                seg,
                0,
                &proxy_url,
                &queue_id,
                &checksum,
                ignore_tls_errors,
                initial_status,
            )
            .await
        {
            log_info!("insert_task error: {}", e);
            return None;
        }

        // 持久化浏览器请求上下文（cookies/referrer/extra_headers），resume 时
        // 恢复鉴权。全空则跳过（多数直链任务），省一次写。
        if !cookies.is_empty() || !referrer.is_empty() || !extra_headers.is_empty() {
            let headers_json = if extra_headers.is_empty() {
                String::new()
            } else {
                serde_json::to_string(&extra_headers).unwrap_or_default()
            };
            if let Err(e) = self
                .db
                .set_task_request_context(&task_id, &cookies, &referrer, &headers_json)
                .await
            {
                log_info!("set_task_request_context error: {}", e);
            }
        }

        // Persist .torrent file bytes to DB for resume after restart.
        if !torrent_file_bytes.is_empty()
            && let Err(e) = self
                .db
                .save_torrent_file_bytes(&task_id, &torrent_file_bytes)
                .await
        {
            log_info!("save_torrent_file_bytes error: {}", e);
        }
        // 轨对任务：持久化音频轨 URL，供重启恢复时重建轨对下载。
        if let Some(ref au) = audio_url
            && !au.is_empty()
            && let Err(e) = self.db.save_audio_url(&task_id, au).await
        {
            log_info!("save_audio_url error: {}", e);
        }

        self.sink.emit(EngineEvent::TaskProgress {
            task_id: task_id.clone(),
            status: initial_status,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            file_name: file_name.clone(),
            save_dir: save_dir.clone(),
            url: db_url.clone(),
            error_message: String::new(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });

        // 插件惰性解析：命中 resolver 则打标（仅存 ID）；协议判定/probe 推迟到实际
        // 下载前的 off-actor resolve，此处不跑 JS。原始 url 参与匹配（非 db_url）。
        let resolver_plugin_id = self.plugin_match_resolver(&url).await;
        let has_resolver = !resolver_plugin_id.is_empty();
        if has_resolver {
            let _ = self
                .db
                .set_task_resolver(&task_id, &resolver_plugin_id)
                .await;
        }
        // BT tasks bypass the HTTP/FTP concurrency queue — they are managed
        // by the shared librqbit session with its own concurrency controls.
        let is_bt = is_magnet(&url) || !torrent_file_bytes.is_empty();

        if start_paused {
            // 稍后下载：不启动、不排队。后台 probe 让 UI 尽快拿到文件名/
            // 大小；带 resolver（探测原始页面 URL 无意义）或 BT（无 HTTP
            // 元数据可探）任务跳过，语义与排队/直启分支一致。
            if !has_resolver && !is_bt {
                let probe_spec = downloader::RequestSpec::from_captured(
                    method.as_deref(),
                    cookies.clone(),
                    referrer.clone(),
                    extra_headers.clone(),
                    body.clone(),
                );
                let (probe_client, probe_proxy) =
                    self.task_http_context(&proxy_url, &user_agent, &queue_id, ignore_tls_errors);
                self.spawn_meta_probe(
                    task_id,
                    db_url,
                    file_name,
                    probe_spec,
                    probe_client,
                    probe_proxy,
                );
            }
            return Some(created_id);
        }

        let queued = QueuedTask {
            task_id,
            url: db_url,
            save_dir,
            file_name,
            segments: seg,
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
        };
        if is_bt || (self.has_capacity().await && self.has_queue_capacity(&queued.queue_id)) {
            self.do_start_task(queued).await;
            // If do_start_task failed early (e.g. BT session init), the slot
            // was freed — drain the queue so pending tasks can proceed.
            self.drain_queue().await;
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
            let probe_spec = downloader::RequestSpec::from_captured(
                queued.method.as_deref(),
                queued.cookies.clone(),
                queued.referrer.clone(),
                queued.extra_headers.clone(),
                queued.body.clone(),
            );
            let (probe_client, probe_proxy) = self.task_http_context(
                &queued.proxy_url,
                &queued.user_agent,
                &queued.queue_id,
                queued.ignore_tls_errors,
            );
            self.pending_queue.push_back(queued);
            // 广播最新队列位置
            self.broadcast_queue_positions();
            // 带 resolver 的任务跳过 probe（探测原始页面 URL 无意义）。
            if !has_resolver {
                self.spawn_meta_probe(
                    probe_tid,
                    probe_url,
                    probe_name,
                    probe_spec,
                    probe_client,
                    probe_proxy,
                );
            }
        }
        Some(created_id)
    }

    /// 为当前任务解析代理/UA/TLS 策略并构建一致的 HTTP 上下文。
    fn task_http_context(
        &self,
        proxy_url: &str,
        user_agent: &str,
        queue_id: &str,
        ignore_tls_errors: bool,
    ) -> (Client, ProxyConfig) {
        let queue_ua = self
            .queues
            .get(queue_id)
            .map(|q| q.default_user_agent.as_str())
            .unwrap_or("");
        let resolved_ua = if !user_agent.is_empty() {
            user_agent
        } else if !queue_ua.is_empty() {
            queue_ua
        } else {
            self.global_user_agent.as_str()
        };
        let needs_dedicated_client = !proxy_url.is_empty()
            || !user_agent.is_empty()
            || !queue_ua.is_empty()
            || ignore_tls_errors;
        if !needs_dedicated_client {
            return (self.client.clone(), self.proxy_config.resolve());
        }

        let proxy = if proxy_url.is_empty() {
            self.proxy_config.resolve()
        } else {
            ProxyConfig::from_proxy_url(proxy_url)
        };
        match downloader::build_client_with_tls_policy(&proxy, resolved_ua, ignore_tls_errors) {
            Ok(client) => (client, proxy),
            Err(e) => {
                log_info!("[manager] failed to build per-task client: {}", e);
                (self.client.clone(), self.proxy_config.resolve())
            }
        }
    }

    /// 后台元数据探测（HEAD → GET Range:0-0，非阻塞）：探得文件名/大小后
    /// 更新 DB 并广播 [`EngineEvent::TaskMetaProbed`]；失败静默。
    ///
    /// F020：用任务的鉴权上下文（cookies/referrer/extra_headers）构造 probe
    /// 的 `RequestSpec`，使背景 HEAD probe 与真正下载请求一致，避免鉴权站点
    /// 把缺鉴权的裸 HEAD 重定向到登录页污染 DB 文件名。带 resolver 的任务
    /// 不应调用（探测原始页面 URL 无意义）。
    fn spawn_meta_probe(
        &self,
        task_id: String,
        probe_url: String,
        current_name: String,
        probe_spec: downloader::RequestSpec,
        probe_client: Client,
        probe_proxy: ProxyConfig,
    ) {
        let probe_db = self.db.clone();
        let probe_sink = self.sink.clone();
        #[cfg(feature = "plugins")]
        let probe_pm = self.plugin_manager.clone();
        tokio::spawn(async move {
            let (name, size) = crate::meta_prober::probe_task_meta(
                &probe_url,
                &current_name,
                &probe_client,
                &probe_proxy,
                &probe_spec,
            )
            .await;
            if !name.is_empty() || size > 0 {
                if !name.is_empty() {
                    let _ = probe_db.update_task_file_name(&task_id, &name).await;
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
        if !queued.resolver_plugin_id.is_empty() && !queued.resolved {
            self.begin_resolve_start(queued).await;
            return;
        }
        let QueuedTask {
            task_id,
            url,
            save_dir,
            mut file_name,
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
            selected_file_indices,
            method,
            body,
            audio_url,
            resolver_plugin_id: _,
            resolved: _,
            range_supported,
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

        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();

        let use_ftp = is_ftp_url(&url);
        let use_hls = hls_downloader::is_hls_url(&url);
        // 轨对任务（audio_url 非空）复用 DASH 下载器的下载+mux 能力，与 .mpd 后缀正交。
        let use_dash = dash_downloader::is_dash_url(&url) || audio_url.is_some();
        let use_bt = is_magnet(&url) || !torrent_file_bytes.is_empty() || is_torrent_file_url(&url);
        let use_ed2k = crate::ed2k::link::is_ed2k_url(&url);

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
        let panic_progress_tx = self.progress_tx.clone();
        let panic_task_id = task_id.clone();
        let panic_db = self.db.clone();

        let handle = if use_bt {
            // Lazily initialise the shared BT session.
            if let Err(e) = self.ensure_bt_session().await {
                log_info!("[manager] failed to init BT session: {}", e);
                let _ = self
                    .db
                    .update_task_status(&task_id, 4, &e.to_string())
                    .await;
                let _ = self
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
                    .await;
                self.active_tasks.remove(&task_id);
                return;
            }
            // bt_session is guaranteed to be Some after ensure_bt_session().
            let Some(bt_ref) = self.bt_session.as_ref() else {
                log_info!("[manager] BUG: bt_session is None after ensure_bt_session succeeded");
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
            // .torrent filename (without the ".torrent" extension) by the Dart
            // layer — it has no extension and does not represent the user's
            // intent to rename the download.  Using it as custom_name would
            // cause the completed file to be saved without its real extension
            // (e.g. "cachyos-desktop-linux-260308" instead of
            // "cachyos-desktop-linux-260308.iso").
            //
            // Rule: custom_name is only honoured for magnet-URL tasks where
            // file_name is non-empty and safe.  Torrent-file tasks always
            // discover their real name from metadata and never rename.
            let custom_name = if is_torrent_file_task {
                // Task created from a .torrent file — ignore file_name.
                String::new()
            } else if is_safe_file_name(&file_name) {
                // Magnet task with a user-supplied name.
                file_name.clone()
            } else {
                String::new()
            };
            if !custom_name.is_empty() {
                let _ = self.db.save_bt_custom_name(&task_id, &custom_name).await;
            }

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
                pre_selected_indices: selected_file_indices,
                skip_file_selection: false,
                custom_name,
                selector: self.selector.clone(),
            };

            tokio::spawn(async move {
                let result =
                    std::panic::AssertUnwindSafe(bt_downloader::run_bt_download(bt_params))
                        .catch_unwind()
                        .await;

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx
                    .send(TaskDone {
                        task_id: panic_task_id,
                        generation: spawn_gen,
                        reserved_temp_path: None, // BT 任务不使用文件名预订机制
                    })
                    .await;
            })
        } else {
            let (task_client, task_proxy) =
                self.task_http_context(&proxy_url, &user_agent, &queue_id, ignore_tls_errors);
            // ---------------------------------------------------------------
            // 文件名最终决策：manager 是文件名的唯一决策者
            //
            // 流程:
            //   1. 若 file_name 为空 → await probe 拿 Content-Disposition / URL 文件名
            //      （probe 是 async，但发生在"同步预订段"之前；同时只允许 file_name
            //       为空的任务 await，已知 file_name 的任务直接进同步段，互不干扰）
            //   2. 同步段（无 .await）：
            //      - dedup_filename_sync(磁盘 + 兄弟任务的 reserved_temp_paths)
            //      - reserved_temp_paths.insert(自己的 temp 路径)
            //      - 持久化最终 file_name 到 DB
            //
            // 与旧设计的本质区别：
            //   - 旧设计：manager 同步段做一次 dedup 并插入 reserved，spawned task
            //     再做一次 dedup 并把 reserved 快照（含自己）传进去。后者会把"自己
            //     已预订"误判为"已被占用"，触发回归 bug（PR #296 自我冲突）。
            //   - 新设计：spawned task 不再 dedup，DownloadParams 不再携带
            //     reserved_filenames_snapshot；下载器内部不再变更文件名。
            //
            // Reservation 在 `on_task_done` 中通过 TaskDone.reserved_temp_path 释放。
            // ---------------------------------------------------------------
            let save_path = std::path::PathBuf::from(&save_dir);

            // Step 1: 若名称未知，先 probe（async；不在同步预订段内）。
            // BT 不走此分支，FTP/HLS/DASH/HTTP 共用此 probe 接口。
            // probe 失败则保持 file_name 为空——下载器内部仍有兜底（URL 解析），
            // 但此时无法做 manager 级 dedup，dedup_filename_sync 会返回原名。
            if file_name.is_empty() {
                // Step 1a: 先从 DB 读一次——任务在 pending_queue 等待期间，
                // create_task 中 spawn 的背景 probe 可能已经把文件名写进 DB。
                // 直接复用，避免对一次性 CDN URL 重复 probe 消耗 token。
                if let Ok(Some(t)) = self.db.load_task_by_id(&task_id).await
                    && !t.file_name.is_empty()
                {
                    file_name = t.file_name;
                }
            }

            if file_name.is_empty() {
                // Step 1b: DB 中也没有名字（未排队，或背景 probe 未完成/失败）
                // → 在此 await 一次 probe。注意此 .await 在同步预订段之前，
                // 不会破坏 dedup+insert 的原子性。
                //
                // F020：probe 携带任务的鉴权上下文（cookies/referrer/extra_headers），
                // 与下方真正下载用的 `spec` 同源，避免鉴权站点把缺鉴权的裸 HEAD
                // 重定向到登录页、用错误页的 Content-Disposition 污染文件名。
                let probe_spec = downloader::RequestSpec::from_captured(
                    method.as_deref(),
                    cookies.clone(),
                    referrer.clone(),
                    extra_headers.clone(),
                    body.clone(),
                );
                let (probed_name, _probed_size) = crate::meta_prober::probe_task_meta(
                    &url,
                    &file_name,
                    &task_client,
                    &task_proxy,
                    &probe_spec,
                )
                .await;
                if !probed_name.is_empty() {
                    file_name = probed_name;
                    let _ = self.db.update_task_file_name(&task_id, &file_name).await;
                    self.sink.emit(EngineEvent::TaskMetaProbed {
                        task_id: task_id.clone(),
                        file_name: file_name.clone(),
                        total_bytes: 0,
                    });
                }
            }

            // HLS：在 dedup + 预订之前把名称归一化为 .ts，使 manager 级 dedup 和
            // reserved_temp_paths 预订都基于 HLS 下载器最终的落盘名。否则不同前缀名
            // （clip.m3u8 / clip.mp4）会在 HLS 下载器内 force_ts 后塌缩为同一 clip.ts，
            // 绕过 manager dedup，导致两个任务静默覆盖同一文件。force_ts_extension
            // 幂等；HLS 下载器内仍保留幂等的 force_ts 作为兜底/续传安全网。
            //
            // 即使 probe 后 file_name 仍为空，也用 URL 末段兜底出与 HLS 下载器空名
            // 分支一致的名称（extract_from_url + force_ts），使空名 HLS 任务也纳入
            // dedup + 预订协调——否则两个同源、均探测不到名的并发 HLS 任务会各自
            // 塌缩为同一 .ts 并互相 truncate/交错写入而损坏内容。
            if hls_downloader::is_hls_url(&url) {
                let base = if file_name.is_empty() {
                    downloader::extract_from_url(&url).unwrap_or_else(|| "download.ts".to_string())
                } else {
                    file_name.clone()
                };
                let ts_name = hls_downloader::force_ts_extension(&base);
                if ts_name != file_name {
                    file_name = ts_name;
                    let _ = self.db.update_task_file_name(&task_id, &file_name).await;
                }
            }

            // DASH：probe 后仍空名时,用 URL 末段兜底为 .mp4(与 DASH 下载器空名分支
            // 一致),使空名 DASH 任务也纳入 dedup + 预订协调,避免两个同源、均探测不到
            // 名的并发 DASH 任务塌缩到同一 .mp4 路径互相覆盖。非空名 DASH 下载器原样
            // 使用 p.file_name(不强制扩展名),故此处仅处理空名,不改非空名。
            if file_name.is_empty() && dash_downloader::is_dash_url(&url) {
                let url_name = downloader::extract_from_url(&url)
                    .unwrap_or_else(|| "download.mpd".to_string());
                file_name = match url_name.rfind('.') {
                    Some(pos) => format!("{}.mp4", &url_name[..pos]),
                    None => format!("{}.mp4", url_name),
                };
                let _ = self.db.update_task_file_name(&task_id, &file_name).await;
            }

            // Step 2: dedup + insert reserved。
            // dedup_filename_sync 本身是同步的；仅当 dedup 改名时有一次
            // update_task_file_name 落库 .await（须在 insert 前完成，否则 spawned
            // task 可能用到旧名）。do_start_task 持有 &mut self 且运行于
            // current_thread runtime，同一时刻只有一个实例执行，故
            // dedup→落库→insert 之间不会与兄弟任务的预订交错，无竞态。
            // 此时 self.reserved_temp_paths 中只有兄弟任务的预订，不包含自己，
            // 因此不会出现"自我冲突"。
            let reserved_temp_path: Option<std::path::PathBuf> = if !file_name.is_empty() {
                let deduped =
                    dedup_filename_sync(&save_path, &file_name, &self.reserved_temp_paths);
                if deduped != file_name {
                    file_name = deduped.clone();
                    // dedup 改名后立即落库（spawned task 不再修改文件名）
                    let _ = self.db.update_task_file_name(&task_id, &file_name).await;
                }
                let temp = save_path.join(format!("{}{}", deduped, downloader::TEMP_EXT));
                self.reserved_temp_paths.insert(temp.clone());
                Some(temp)
            } else {
                // 兜底：file_name 仍为空（probe 失败）。下载器内部会从响应头
                // / URL 兜底解析名称，但不做 dedup（无法与 reserved 协调）。
                // 这是极端情况，正常路径不会到此。
                None
            };

            // 构造完整 HTTP 请求事务规格——method/body 来自浏览器扩展，
            // 用于在 form-POST 等非 GET 触发的下载场景中一比一重建原始请求。
            // 参见 downloader.rs 中 RequestSpec / build_request 的设计动机。
            let spec = downloader::RequestSpec::from_captured(
                method.as_deref(),
                cookies.clone(),
                referrer.clone(),
                extra_headers.clone(),
                body.clone(),
            );

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
                // 段行布局属主令牌：本次 spawn 的 generation。多段路径起飞时
                // 落 tasks.segments_epoch，旧 spawn 迟到的段进度写全类失效。
                spawn_gen: spawn_gen as i64,
                ffmpeg_path: crate::components::resolve_ffmpeg(&self.db, &self.data_dir).await,
            };

            tokio::spawn(async move {
                let result = if use_ftp {
                    std::panic::AssertUnwindSafe(ftp_downloader::run_ftp_download(params))
                        .catch_unwind()
                        .await
                } else if use_hls {
                    std::panic::AssertUnwindSafe(hls_downloader::run_hls_download(params))
                        .catch_unwind()
                        .await
                } else if use_dash {
                    std::panic::AssertUnwindSafe(dash_downloader::run_dash_download(params))
                        .catch_unwind()
                        .await
                } else if use_ed2k {
                    std::panic::AssertUnwindSafe(crate::ed2k::run_ed2k_download(params))
                        .catch_unwind()
                        .await
                } else {
                    std::panic::AssertUnwindSafe(downloader::run_download(params))
                        .catch_unwind()
                        .await
                };

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx
                    .send(TaskDone {
                        task_id: panic_task_id,
                        generation: spawn_gen,
                        reserved_temp_path,
                    })
                    .await;
            })
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
    }
    #[cfg(not(feature = "plugins"))]
    fn clear_pending_resolve(&mut self, _task_id: &str) {}
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
            });
        }
    }

    pub async fn pause_task(&mut self, task_id: &str) {
        self.clear_pending_resolve(task_id);
        // Remove from pending queue if queued (not yet started).
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
            // 广播更新后的队列位置
            self.broadcast_queue_positions();
            let _ = self.db.update_task_status(task_id, 2, "").await;
            self.emit_progress_from_db(task_id, 2, 0, "", 0).await;
            return;
        }

        if let Some(entry) = self.active_tasks.remove(task_id) {
            entry.token.cancel();

            // For BT tasks, explicitly pause the torrent in the session so
            // that the handle stays cached for fast resume.  This is a
            // no-op if the download loop already called session.pause on
            // cancellation detection, but covers edge cases (e.g. pause
            // during metadata resolution).
            if let Some(ref bt) = self.bt_session {
                let _ = bt.pause_task(task_id).await;
            }

            let _ = self.db.update_task_status(task_id, 2, "").await;
            self.emit_progress_from_db(task_id, 2, 0, "", 0).await;

            if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
                // Send persisted segment data so the UI retains the download
                // distribution visualization after pausing.
                self.send_segments_from_db(task_id, t.total_bytes).await;
            }

            // A slot freed up — try to start queued tasks.
            self.drain_queue().await;

            // Boost 守卫：若用户手动暂停了当前优先任务，取消 Boost 并恢复其他任务
            if self.priority_task_id.as_deref() == Some(task_id) {
                self.clear_priority().await;
            }
            // NOTE: do NOT call maybe_release_bt_session() here.
            //
            // pause_task() removes the task from active_tasks and cancels the
            // CancellationToken, but the spawned BT task (bt_download_inner)
            // may still be running on the shared BT runtime for up to ~500 ms
            // (one poll-sleep interval) before it detects cancellation and exits.
            //
            // If we call maybe_release_bt_session() now, it sees no BT tasks in
            // active_tasks and shuts down the runtime immediately — which aborts
            // bt_download_inner mid-flight and causes run_bt_download to return a
            // JoinError.  That JoinError propagates as DownloadError::Other (or
            // Cancelled if our guard fires), and the spawned wrapper still sends
            // done_tx → on_task_done → maybe_release_bt_session, so the session
            // is released safely once the task has actually stopped.
        }

        // Third branch: the task is a completed BT torrent that is currently
        // seeding. Pausing it must stop the seeder and persist the user-stopped
        // state without changing the overall completed status.
        if let Ok(Some(task)) = self.db.load_task_by_id(task_id).await
            && task.status == 3
        {
            match task.seeding_status {
                s if s == SEEDING_STATUS_ACTIVE => {
                    if let Some(ref bt) = self.bt_session {
                        let _ = bt.pause_task(task_id).await;
                        let _ = bt.unregister_seeder(task_id).await;
                    }
                    let _ = self
                        .db
                        .update_task_seeding_status(
                            task_id,
                            SeedingStopReason::UserStopped.as_i32(),
                            SeedingStopReason::UserStopped.message(),
                        )
                        .await;
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
    }

    pub async fn resume_task(&mut self, task_id: &str) {
        // 用户手动恢复时重置自动重试计数，让下次失败重新获得完整重试配额。
        self.auto_retry_counts.remove(task_id);
        self.resume_task_inner(task_id).await;
    }

    /// 自动重试路径专用：恢复任务但**不**重置自动重试计数。
    /// 与 resume_task 的区别仅在于跳过 auto_retry_counts.remove，
    /// 使累积计数得以持久到下次失败，从而正确触发重试上限与递增退避。
    pub async fn resume_task_auto(&mut self, task_id: &str) {
        self.resume_task_inner(task_id).await;
    }

    async fn resume_task_inner(&mut self, task_id: &str) {
        if self.active_tasks.contains_key(task_id) {
            // A task can be in active_tokens but already terminal in the DB:
            // this happens when the download task has finished (status=3/4
            // written to DB) but the done_tx hasn't been consumed by the
            // actor loop yet.  If we silently return here, the user's retry
            // request is dropped and the task stays stuck in error state.
            //
            // Detect this race: if DB status is terminal (completed=3 or
            // error=4), force-remove the stale entry so the resume proceeds.
            // The stale done_tx will be harmlessly ignored because the new
            // spawn increments the generation counter, making the old
            // generation mismatch in on_task_done.
            let is_terminal = self
                .db
                .load_task_by_id(task_id)
                .await
                .ok()
                .flatten()
                .map(|t| t.status == 3 || t.status == 4)
                .unwrap_or(false);
            if !is_terminal {
                return; // truly still active — do not interrupt
            }
            log_info!(
                "[manager] resume_task {}: stale active_tasks entry (terminal in DB) — force-removing",
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
        let task_row = self.db.load_task_by_id(task_id).await.ok().flatten();

        // Handle paused BT seeders before treating the task as an ordinary resume.
        // A completed task with seeding_status == 4 is still a finished download;
        // resuming it only reactivates seeding.
        if let Some(ref task) = task_row
            && task.status == 3
            && task.seeding_status == SeedingStopReason::UserStopped.as_i32()
        {
            let stopped_status = SeedingStopReason::UserStopped.as_i32();
            let stopped_message = SeedingStopReason::UserStopped.message();
            if let Some(ref bt) = self.bt_session {
                match bt.resume_task(task_id).await {
                    Ok(Some(handle)) => {
                        let started_at_unix = chrono::Local::now().timestamp();
                        bt.register_seeder(
                            task_id,
                            handle,
                            task.uploaded_at_completion,
                            started_at_unix,
                            0,
                        )
                        .await;
                        let _ = self
                            .db
                            .set_task_seeding_active(task_id, started_at_unix)
                            .await;
                        self.emit_progress_from_db(task_id, 3, 1, "", 0).await;
                    }
                    Ok(None) => {
                        log_info!(
                            "[manager] resume_task {}: no cached BT handle, cannot resume seeding",
                            task_id
                        );
                        self.emit_progress_from_db(task_id, 3, stopped_status, stopped_message, 0)
                            .await;
                    }
                    Err(e) => {
                        log_info!("[manager] resume_task {}: BT resume failed: {}", task_id, e);
                        self.emit_progress_from_db(task_id, 3, stopped_status, stopped_message, 0)
                            .await;
                    }
                }
            } else {
                log_info!(
                    "[manager] resume_task {}: no BT session, cannot resume seeding",
                    task_id
                );
                self.emit_progress_from_db(task_id, 3, stopped_status, stopped_message, 0)
                    .await;
            }
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

        if is_bt || (self.has_capacity().await && self.has_queue_capacity(&queue_id)) {
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
                // Notify Dart: task is now queued (pending), not actively resuming.
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
                });
                self.pending_queue.push_back(QueuedTask {
                    task_id: task_id.to_string(),
                    url: t.url,
                    save_dir: t.save_dir,
                    file_name: t.file_name,
                    segments: 0, // not used for resume
                    is_resume: true,
                    cookies: String::new(), // resume 上下文由 do_resume_task 从 DB 恢复
                    referrer: String::new(),
                    hint_file_size: 0, // no hint on resume; use probe to get current size
                    torrent_file_bytes: Vec::new(), // loaded from DB in do_resume_task
                    proxy_url: t.proxy_url,
                    user_agent: String::new(), // use global UA on resume
                    queue_id: t.queue_id,
                    checksum: t.checksum, // loaded from DB for integrity verification
                    ignore_tls_errors: false, // resume path reloads the persisted value from DB
                    extra_headers: std::collections::HashMap::new(), // 恢复任务无额外请求头
                    selected_file_indices: Vec::new(), // resume tasks have no pre-selection
                    method: None,         // 不持久化 method/body，恢复时按 GET 重发
                    body: None,
                    // resume 路径下 do_resume_task 会从 DB 重新读 audio_url，此处 None 即可。
                    audio_url: None,
                    resolver_plugin_id: String::new(),
                    resolved: false,
                    range_supported: false,
                });
                // 入队后立即广播最新队列位置(与 create_task 一致),否则要等后续
                // drain_queue 才广播,期间 UI 显示过时的排队位置。
                self.broadcast_queue_positions();
            }
        }
    }

    /// Internal: actually spawn the resume (no concurrency check).
    async fn do_resume_task(&mut self, task_id: &str) {
        // 插件惰性解析守卫（体首，协议判定前）：命中 resolver 且非再入 → off-actor
        // resolve 后经 resume_applied 再入；对称占位防 resumeAll 并发双 resolve。
        #[cfg(feature = "plugins")]
        let plugin_applied: Option<crate::plugin::ResolveResult> = {
            let resolver = self.db.get_task_resolver(task_id).await.unwrap_or_default();
            if resolver.is_empty() {
                None
            } else if let Some(res) = self.resume_applied.remove(task_id) {
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
                let _ = self
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
                    .await;
                return;
            }
        };

        // 应用 resolve 改写；空 url = 放行（用原 url）。后续协议判定与下载均以
        // task.url 为准，因而自动重算 use_bt/hls/dash/ftp/ed2k。extra_headers/
        // audio_url/ephemeral 同样取出，在下方各自的落点应用——与 start 路径的
        // apply_resolve_to_queued 对称（缺一即 DASH 轨对丢音轨/鉴权直链丢头/
        // 一次性直链被 probe 作废）。
        #[cfg(feature = "plugins")]
        let plugin_resolved = plugin_applied.is_some();
        #[cfg(feature = "plugins")]
        let (
            plugin_extra_headers,
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
                res.extra_headers,
                res.audio_url.filter(|a| !a.is_empty()),
                res.ephemeral,
                res.total_bytes.unwrap_or(0),
                res.range_supported,
            )
        } else {
            (None, None, false, 0, false)
        };

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

        // resolve 的新鲜 extra_headers 优先于 DB 快照（轮换签名头场景）。
        #[cfg(feature = "plugins")]
        let resume_extra_headers = plugin_extra_headers.unwrap_or(resume_extra_headers);

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
        let use_ed2k = crate::ed2k::link::is_ed2k_url(&task.url);

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
        let panic_progress_tx = self.progress_tx.clone();
        let panic_task_id = tid.clone();
        let panic_db = self.db.clone();

        let handle = if use_bt {
            // Lazily initialise the shared BT session.
            if let Err(e) = self.ensure_bt_session().await {
                log_info!("[manager] failed to init BT session for resume: {}", e);
                let _ = self.db.update_task_status(task_id, 4, &e.to_string()).await;
                let _ = self
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
                    .await;
                self.active_tasks.remove(task_id);
                return;
            }
            // bt_session is guaranteed to be Some after ensure_bt_session().
            let Some(bt_ref) = self.bt_session.as_ref() else {
                log_info!("[manager] BUG: bt_session is None after ensure_bt_session succeeded");
                self.active_tasks.remove(task_id);
                return;
            };

            // Try to resume from a cached handle (pause→resume within the
            // same app session).  If the handle is found, unpause it and
            // pass it to the download loop so it skips add_torrent entirely.
            let mut existing = match bt_ref.resume_task(task_id).await {
                Ok(h) => h,
                Err(e) => {
                    log_info!("[manager] BT resume_task error (will re-add): {}", e);
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
                    bt_ref.delete_task(task_id, false).await;
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
                    let _ = self.db.update_task_status(task_id, 4, msg).await;
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
            let custom_name = self
                .db
                .load_bt_custom_name(task_id)
                .await
                .unwrap_or_default();

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
            };

            tokio::spawn(async move {
                let result =
                    std::panic::AssertUnwindSafe(bt_downloader::run_bt_download(bt_params))
                        .catch_unwind()
                        .await;

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx
                    .send(TaskDone {
                        task_id: panic_task_id,
                        generation: spawn_gen,
                        reserved_temp_path: None, // BT 任务不使用文件名预订机制
                    })
                    .await;
            })
        } else {
            // Resolve proxy and UA for resume: use global UA (cookies not
            // persisted in DB, so only proxy_url is available from task row).
            let needs_rebuild = !task.proxy_url.is_empty()
                || !self.global_user_agent.is_empty()
                || task.ignore_tls_errors;
            let (task_client, task_proxy) = if needs_rebuild {
                let pc = if task.proxy_url.is_empty() {
                    self.proxy_config.resolve()
                } else {
                    ProxyConfig::from_proxy_url(&task.proxy_url)
                };
                match downloader::build_client_with_tls_policy(
                    &pc,
                    &self.global_user_agent,
                    task.ignore_tls_errors,
                ) {
                    Ok(c) => (c, pc),
                    Err(e) => {
                        log_info!("[manager] failed to build per-task client on resume: {}", e);
                        (self.client.clone(), self.proxy_config.resolve())
                    }
                }
            } else {
                (self.client.clone(), self.proxy_config.resolve())
            };
            // Range 未验证的 hint 任务（tasks.range_verified==0：源自浏览器扩展
            // hint、从未拿到过 206/Accept-Ranges 证据）：resume 以 DB 里的
            // total_bytes 作 hint 延续「跳过 probe + 首连接 plain GET」保守启动
            // ——落回默认 probe 会对配额型端点（fnOS multiple-download）重新
            // 发 HEAD/Range 并作废 token。已验证任务/旧库任务：照常 probe。
            let range_verified = self.db.get_task_range_verified(&tid).await.unwrap_or(true);
            // resolver 插件本次 resolve 担保 Range 支持 → 视为已验证：配合下方
            // ephemeral hint，跳过 probe 且按多段起飞（不落保守单流启动）。
            #[cfg(feature = "plugins")]
            let range_verified = range_verified || plugin_range_supported;
            let resume_hint = if range_verified {
                0 // no hint on resume; use probe to get current size
            } else if task.total_bytes > 0 {
                task.total_bytes
            } else {
                -1 // 大小未知但确认可下载（沿用扩展 webRequest 嗅探语义）
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

            let params = DownloadParams {
                task_id: tid.clone(),
                url: task.url,
                save_dir: task.save_dir,
                file_name: task.file_name,
                segment_count: seg_count,
                is_resume: true,
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
                // method/body 仍不持久化：恢复一律按 GET 重发（重放 POST 体有
                // 副作用风险，成本远高于收益）。cookies/referrer/extra_headers
                // 则从 DB 恢复，保住鉴权站点的 resume。
                spec: downloader::RequestSpec::from_captured(
                    None,
                    resume_cookies,
                    resume_referrer,
                    resume_extra_headers,
                    None,
                ),
                audio_url,
                auto_max_connections: self.auto_max_connections,
                use_server_time: self.use_server_time,
                spawn_gen: spawn_gen as i64,
                ffmpeg_path: crate::components::resolve_ffmpeg(&self.db, &self.data_dir).await,
            };

            tokio::spawn(async move {
                let result = if use_ftp {
                    std::panic::AssertUnwindSafe(ftp_downloader::run_ftp_download(params))
                        .catch_unwind()
                        .await
                } else if use_hls {
                    std::panic::AssertUnwindSafe(hls_downloader::run_hls_download(params))
                        .catch_unwind()
                        .await
                } else if use_dash {
                    std::panic::AssertUnwindSafe(dash_downloader::run_dash_download(params))
                        .catch_unwind()
                        .await
                } else if use_ed2k {
                    std::panic::AssertUnwindSafe(crate::ed2k::run_ed2k_download(params))
                        .catch_unwind()
                        .await
                } else {
                    std::panic::AssertUnwindSafe(downloader::run_download(params))
                        .catch_unwind()
                        .await
                };

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx
                    .send(TaskDone {
                        task_id: panic_task_id,
                        generation: spawn_gen,
                        reserved_temp_path: None, // resume 任务不预订文件名
                    })
                    .await;
            })
        };
        if let Some(entry) = self.active_tasks.get_mut(task_id) {
            entry.handle = Some(handle);
        }
    }

    pub async fn cancel_task(&mut self, task_id: &str) {
        // 清除自动重试计数，与 delete_task / resume_task 对齐。取消是用户的
        // 明确意图，必须从自动重试状态中移除，使后续 create/resume 干净起步。
        self.auto_retry_counts.remove(task_id);
        self.clear_pending_resolve(task_id);

        // Remove from pending queue if queued.
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
            // 移除排队任务后立即广播,使其余排队任务位置实时前移(与 pause_task/
            // delete_tasks_batch 一致;否则要等后续 drain_queue 期间 UI 显示过时位置)。
            self.broadcast_queue_positions();
        }

        if let Some(entry) = self.active_tasks.remove(task_id) {
            entry.token.cancel();
            // For BT tasks, explicitly pause the torrent in the session so
            // that fast-resume data is preserved and the user can resume later.
            // This mirrors what pause_task does for BT tasks.
            if entry.is_bt
                && let Some(ref bt) = self.bt_session
            {
                let _ = bt.pause_task(task_id).await;
            }
            // Clean up the JoinHandle so it doesn't linger after cancellation.
            if let Some(handle) = entry.handle {
                drop(handle);
            }
        }

        let _ = self
            .db
            .update_task_status(task_id, 4, CANCELLED_ERROR_MESSAGE)
            .await;

        // Send update with actual task info if available
        let task_info = self.db.load_task_by_id(task_id).await.ok().flatten();

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
        });

        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
        self.maybe_release_bt_session().await;
    }

    /// Delete task record and optionally its files on disk.
    ///
    /// If the task is actively downloading, the cancellation token is triggered
    /// first and we **await** the spawned task's `JoinHandle` so that all
    /// network connections and file handles are fully released before we
    /// attempt to remove files.  A 5-second timeout prevents indefinite hangs.
    pub async fn delete_task(&mut self, task_id: &str, delete_files: bool) {
        self.auto_retry_counts.remove(task_id);
        self.clear_pending_resolve(task_id);

        // Remove from pending queue if queued.
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
            // 移除排队任务后立即广播剩余排队任务位置(与 delete_tasks_batch 一致)。
            self.broadcast_queue_positions();
        }

        // Cancel the active download (if any) and wait for the spawned task
        // to exit, ensuring all network sockets and file handles are closed.
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
            match tokio::time::timeout(std::time::Duration::from_secs(5), &mut handle).await {
                Ok(_) => false,
                Err(_) => {
                    handle.abort();
                    true
                }
            }
        } else {
            false
        };
        if handle_timed_out {
            log_info!(
                "[manager] delete_task {}: handle wait timed out, spawned task may still be running",
                task_id
            );
        }

        // 记录文件信息，供 handle 超时后延迟二次清理使用
        let mut deferred_cleanup: Option<(String, String, String, bool)> = None;

        // 在 handle 等待之后加载 DB，确保获取到 spawned task 可能更新的最新 file_name。
        if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
            // 若 handle 超时且文件名已知，记录信息以便后续延迟清理
            if handle_timed_out && !t.file_name.is_empty() {
                deferred_cleanup = Some((
                    t.save_dir.clone(),
                    t.file_name.clone(),
                    t.url.clone(),
                    delete_files,
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
                self.reserved_temp_paths.remove(&reserved);
            }
            let path = PathBuf::from(&t.save_dir).join(&t.file_name);

            if is_bt_url(&t.url) {
                // Permanently remove from librqbit session (clears
                // persistence data and optionally deletes files via
                // librqbit's own cleanup).
                if let Some(ref bt) = self.bt_session {
                    let handle_found = bt.delete_task(task_id, delete_files).await;
                    if !handle_found {
                        // Handle not in map: the task is still in the
                        // add_torrent phase (e.g. magnet DHT resolution).
                        // Register a pending delete so the detached
                        // add_torrent closure cleans up the librqbit session
                        // entry (and files) once metadata resolves.
                        bt.register_pending_delete(task_id, delete_files).await;
                    }
                }
                // Fallback filesystem cleanup: covers the cross-session case
                // where the app restarted after completion (handle not in
                // SharedBtSession.handles) and session.delete could not be
                // called above.  We skip the outer path.exists() guard and
                // let each operation fail silently if the path is absent.
                if delete_files && is_safe_file_name(&t.file_name) {
                    if path.is_dir() {
                        let _ = tokio::fs::remove_dir_all(&path).await;
                    } else {
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }

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
                    let _ = tokio::fs::remove_dir_all(&stage_dir).await;
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
                }

                // DASH audio sidecar: clean up .audio.m4a and its .part temp
                // 轨对任务（视频轨 URL 非 .mpd）也持有 sidecar，需一并清理。
                let has_audio_sidecar = dash_downloader::is_dash_url(&t.url)
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
                    let _ = tokio::fs::remove_file(&audio_temp).await;
                    if delete_files {
                        let _ = tokio::fs::remove_file(&audio_path).await;
                    }
                }

                if delete_files
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
                }
            }
        }

        // Notify progress_reporter so it can remove its per-task HashMap
        // entries (states, last_dart_send, last_db_save).  Without this the
        // reporter leaks ~300-1400 bytes per deleted task indefinitely.
        let _ = self
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
            .await;

        // 插件登记的衍生产物（如转码 mp4）随任务文件一并删除；须在 DB 行
        // 删除前读取登记表。
        if delete_files && let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
            delete_task_artifact_files(&self.db, task_id, &t.save_dir).await;
        }

        // If the task was still marked as seeding, record the deletion reason
        // before the row disappears.
        if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await
            && t.seeding_status == crate::bt_seeding::SEEDING_STATUS_ACTIVE
        {
            let _ = self
                .db
                .update_task_seeding_status(
                    task_id,
                    crate::bt_seeding::SeedingStopReason::TaskDeleted.as_i32(),
                    crate::bt_seeding::SeedingStopReason::TaskDeleted.message(),
                )
                .await;
        }

        if let Err(e) = self.db.delete_task(task_id).await {
            log_info!("[manager] delete_task {}: DB delete error: {}", task_id, e);
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

        // Bug 4 修复：被删除的任务从 auto_paused_ids 中移除，
        // 避免 clear_priority 之后徒劳地对已删除任务调用 resume_task，
        // 产生无意义的 DB 查询或错误日志。
        self.auto_paused_ids.remove(task_id);

        // Boost 守卫：若优先任务被删除，取消 Boost 并恢复其他任务
        if self.priority_task_id.as_deref() == Some(task_id) {
            self.clear_priority().await;
        }

        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
        self.maybe_wal_checkpoint().await;
        self.maybe_release_bt_session().await;
    }

    // -----------------------------------------------------------------------
    // Batch operations — single IPC for N tasks
    // -----------------------------------------------------------------------

    /// Batch-delete multiple tasks.  Cancels active downloads, cleans files,
    /// then removes all DB records in a single transaction.
    pub async fn delete_tasks_batch(&mut self, task_ids: &[String], delete_files: bool) {
        if task_ids.is_empty() {
            return;
        }
        let id_set: HashSet<&str> = task_ids.iter().map(|s| s.as_str()).collect();
        log_info!(
            "[manager] delete_tasks_batch: {} tasks, delete_files={}",
            task_ids.len(),
            delete_files
        );

        // 1. Remove from pending queue in one pass.
        self.pending_queue
            .retain(|q| !id_set.contains(q.task_id.as_str()));
        // 队列变更后立即广播剩余排队任务的最新位置(与 pause_task 一致),否则要等到
        // 后续 drain_queue 才广播,中间历经 handle 取消+文件清理(最长 15s)期间 UI
        // 会显示过时的队列位置。broadcast_queue_positions 是只读广播,无副作用。
        self.broadcast_queue_positions();

        // 2. Cancel all active downloads + collect (task_id, JoinHandle) pairs.
        //    We pair each handle with its task ID so we can send per-task
        //    "deleted" confirmation as soon as that handle completes, rather
        //    than waiting for ALL handles before starting any cleanup.
        let mut handle_map: HashMap<String, JoinHandle<()>> = HashMap::new();
        for tid in task_ids {
            if let Some(entry) = self.active_tasks.remove(tid.as_str()) {
                entry.token.cancel();
                if let Some(h) = entry.handle {
                    handle_map.insert(tid.clone(), h);
                }
            }
        }

        // 3. Batch-load all task info from DB in one query (non-blocking, no
        //    need to wait for handles first).
        let task_infos = self
            .db
            .load_tasks_by_ids(task_ids)
            .await
            .unwrap_or_default();
        let info_map: HashMap<&str, &TaskInfo> =
            task_infos.iter().map(|t| (t.task_id.as_str(), t)).collect();

        // 4. Spawn per-task cleanup futures.  Each future:
        //    a) waits for its own JoinHandle (if any) — only blocks THIS task
        //    b) does file cleanup
        //    c) sends its own "deleted" confirmation signal to Dart
        //    This gives Dart incremental progress as each task finishes
        //    independently, instead of all-at-once after a global barrier.
        let file_sem = Arc::new(Semaphore::new(64));
        let mut cleanup_futs: Vec<JoinHandle<()>> = Vec::new();

        for tid in task_ids {
            let ptx = self.progress_tx.clone();
            let tid_owned = tid.clone();
            let maybe_handle = handle_map.remove(tid.as_str());
            let sem = file_sem.clone();

            if let Some(t) = info_map.get(tid.as_str()) {
                // Task has DB info → needs file cleanup.
                let path = PathBuf::from(&t.save_dir).join(&t.file_name);

                if is_bt_url(&t.url) {
                    let bt_session = self.bt_session.clone();
                    let safe = is_safe_file_name(&t.file_name);
                    // Capture save_dir directly so the staging-dir path is
                    // always correct even when file_name is empty (in which
                    // case path == save_dir and path.parent() would be the
                    // *parent* of save_dir — wrong).
                    let save_dir_owned = t.save_dir.clone();
                    // 供 handle 超时后的延迟二次清理使用（F010）。
                    let file_name_owned = t.file_name.clone();
                    let url_owned = t.url.clone();
                    cleanup_futs.push(tokio::spawn(async move {
                        // Wait for this task's download handle (10s per-task timeout).
                        // 超时后 abort 外层 future，加速纯 async 任务释放连接/句柄，
                        // 与 delete_task 单任务路径一致（F011）。
                        let handle_timed_out = if let Some(mut h) = maybe_handle {
                            if tokio::time::timeout(std::time::Duration::from_secs(10), &mut h)
                                .await
                                .is_err()
                            {
                                h.abort();
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        // BT session delete
                        if let Some(ref bt) = bt_session {
                            let found = bt.delete_task(&tid_owned, delete_files).await;
                            if !found {
                                bt.register_pending_delete(&tid_owned, delete_files).await;
                            }
                        }
                        // BT file cleanup (final path, i.e. save_dir/file_name).
                        // Only attempted when file_name is non-empty and safe;
                        // covers the cross-session case where librqbit already
                        // moved the file out of the staging directory.
                        if delete_files && safe {
                            let Ok(_permit) = sem.acquire().await else {
                                return;
                            };
                            if path.is_dir() {
                                let _ = tokio::fs::remove_dir_all(&path).await;
                            } else {
                                let _ = tokio::fs::remove_file(&path).await;
                            }
                        }
                        // Always clean up the task-scoped staging directory.
                        // Use save_dir_owned (the original DB value) rather than
                        // path.parent() to avoid the empty-file_name edge case
                        // where path == save_dir and path.parent() would be the
                        // grandparent directory.
                        let stage_dir = bt_downloader::bt_stage_dir(&save_dir_owned, &tid_owned);
                        if stage_dir.exists() {
                            log_info!(
                                "[manager] delete_tasks_batch {}: removing staging dir {}",
                                tid_owned,
                                stage_dir.display()
                            );
                            let _ = tokio::fs::remove_dir_all(&stage_dir).await;
                        }
                        // Signal completion
                        let _ = ptx
                            .send(ProgressUpdate {
                                task_id: tid_owned.clone(),
                                downloaded_bytes: 0,
                                total_bytes: 0,
                                status: 4,
                                error_message: "deleted".to_string(),
                                file_name: String::new(),
                                segment_details: None,
                                ..Default::default()
                            })
                            .await;
                        // F010：handle 超时时下载任务可能仍在写盘，延迟二次清理
                        // 兜底孤立的最终文件/staging 目录，与单任务路径一致。
                        if handle_timed_out {
                            tokio::spawn(deferred_file_cleanup(
                                save_dir_owned,
                                file_name_owned,
                                url_owned,
                                delete_files,
                                tid_owned,
                            ));
                        }
                    }));
                } else {
                    let url = t.url.clone();
                    let file_name = t.file_name.clone();
                    // 供 handle 超时后的延迟二次清理使用（F010）。
                    let save_dir_owned = t.save_dir.clone();
                    // BUG-MGR-BATCH-DELETE-RESERVATION-LEAK 修复：
                    // 批量删除在 tokio::spawn 内无法访问 &mut self，故 abort 超时时
                    // on_task_done 永不执行，预订永远不会被释放。在进入 spawn 之前的
                    // &mut self 上下文中主动移除预订（HashSet::remove 幂等，无副作用）。
                    let reserved = PathBuf::from(&t.save_dir).join(format!(
                        "{}{}",
                        t.file_name,
                        downloader::TEMP_EXT
                    ));
                    self.reserved_temp_paths.remove(&reserved);
                    // 与单任务 delete_task 一致：移除自动重试计数（同样因 abort 超时
                    // 时 on_task_done 不执行而需在 &mut self 上下文主动清理）。task_id
                    // 是一次性 UUID 不会复用，故仅为内存一致性，无功能影响。
                    self.auto_retry_counts.remove(tid.as_str());
                    // 轨对任务的 sidecar（.audio.m4a）清理：spawn 内无 &mut self，
                    // 在此 &mut self 上下文预读，move 进闭包。
                    let has_audio_sidecar = dash_downloader::is_dash_url(&t.url)
                        || self
                            .db
                            .load_audio_url(&t.task_id)
                            .await
                            .unwrap_or_default()
                            .is_some();
                    cleanup_futs.push(tokio::spawn(async move {
                        // Wait for this task's download handle (10s per-task timeout).
                        // 超时后 abort 外层 future，加速纯 async 任务释放连接/句柄，
                        // 与 delete_task 单任务路径一致（F011）。
                        let handle_timed_out = if let Some(mut h) = maybe_handle {
                            if tokio::time::timeout(std::time::Duration::from_secs(10), &mut h)
                                .await
                                .is_err()
                            {
                                h.abort();
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        let Ok(_permit) = sem.acquire().await else {
                            return;
                        };
                        // Remove temp file
                        let temp_path = PathBuf::from(format!(
                            "{}{}",
                            path.display(),
                            crate::downloader::TEMP_EXT
                        ));
                        if let Err(e) = tokio::fs::remove_file(&temp_path).await
                            && e.kind() != std::io::ErrorKind::NotFound
                        {
                            log_info!(
                                "[manager] delete_tasks_batch {}: remove temp {} failed: {}",
                                tid_owned,
                                temp_path.display(),
                                e
                            );
                        }

                        // DASH / 轨对 audio sidecar cleanup
                        if has_audio_sidecar {
                            let audio_path = dash_downloader::build_audio_path(&path);
                            let audio_temp = PathBuf::from(format!(
                                "{}{}",
                                audio_path.display(),
                                crate::downloader::TEMP_EXT
                            ));
                            let _ = tokio::fs::remove_file(&audio_temp).await;
                            if delete_files {
                                let _ = tokio::fs::remove_file(&audio_path).await;
                            }
                        }

                        if delete_files
                            && is_safe_file_name(&file_name)
                            && let Err(e) = tokio::fs::remove_file(&path).await
                            && e.kind() != std::io::ErrorKind::NotFound
                        {
                            log_info!(
                                "[manager] delete_tasks_batch {}: remove file {} failed: {}",
                                tid_owned,
                                path.display(),
                                e
                            );
                        }

                        // Signal completion
                        let _ = ptx
                            .send(ProgressUpdate {
                                task_id: tid_owned.clone(),
                                downloaded_bytes: 0,
                                total_bytes: 0,
                                status: 4,
                                error_message: "deleted".to_string(),
                                file_name: String::new(),
                                segment_details: None,
                                ..Default::default()
                            })
                            .await;
                        // F010：handle 超时时下载任务可能仍在写临时文件，延迟
                        // 二次清理兜底，与单任务路径一致。
                        if handle_timed_out {
                            tokio::spawn(deferred_file_cleanup(
                                save_dir_owned,
                                file_name,
                                url,
                                delete_files,
                                tid_owned,
                            ));
                        }
                    }));
                }
            } else {
                // Task NOT in DB (already cleaned / no record) — just wait
                // for handle (if any) then signal immediately.
                cleanup_futs.push(tokio::spawn(async move {
                    // 超时后 abort，与其它清理路径一致（F011）。
                    if let Some(mut h) = maybe_handle
                        && tokio::time::timeout(std::time::Duration::from_secs(10), &mut h)
                            .await
                            .is_err()
                    {
                        h.abort();
                    }
                    let _ = ptx
                        .send(ProgressUpdate {
                            task_id: tid_owned,
                            downloaded_bytes: 0,
                            total_bytes: 0,
                            status: 4,
                            error_message: "deleted".to_string(),
                            file_name: String::new(),
                            segment_details: None,
                            ..Default::default()
                        })
                        .await;
                }));
            }
        }

        // 5. Wait for all per-task cleanup futures (15s global timeout).
        //    Progress signals arrive incrementally as each task completes.
        if !cleanup_futs.is_empty() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                futures_util::future::join_all(cleanup_futs),
            )
            .await;
        }

        // 5.5 插件登记的衍生产物随任务文件一并删除（须在 DB 批量删除前读表）。
        if delete_files {
            for tid in task_ids {
                if let Some(t) = info_map.get(tid.as_str()) {
                    delete_task_artifact_files(&self.db, tid, &t.save_dir).await;
                }
            }
        }

        // 6. Single-transaction batch DB delete.
        if let Err(e) = self.db.delete_tasks_batch(task_ids).await {
            log_info!("[manager] delete_tasks_batch DB error: {}", e);
        }

        // 7. Cleanup boost state.
        for tid in task_ids {
            self.auto_paused_ids.remove(tid.as_str());
            if self.priority_task_id.as_deref() == Some(tid.as_str()) {
                self.clear_priority().await;
            }
        }

        // 8. drain_queue + wal_checkpoint only once at the end.
        self.drain_queue().await;
        self.maybe_wal_checkpoint().await;
        self.maybe_release_bt_session().await;
    }

    /// Batch resume multiple tasks.  Pre-loads all task info in one DB query
    /// to avoid N+1 queries, then processes each with the cached data.
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

        for tid in task_ids {
            if let Some(task_row) = task_map.get(tid.as_str()) {
                self.resume_task_with_row(tid, task_row.clone()).await;
            }
        }
    }

    /// Resume a task using a pre-loaded TaskInfo row (avoids redundant DB query).
    async fn resume_task_with_row(&mut self, task_id: &str, task_row: TaskInfo) {
        // 批量手动 resume 与单任务 resume_task 语义对齐：用户手动恢复应重置
        // 自动重试计数，给一个全新的重试配额。否则一个已耗尽配额的任务被批量
        // 恢复后，下次可重试错误会立刻命中"已耗尽"分支、停在 error，与单任务
        // 手动恢复行为不一致（BUG-BATCH-RESUME-NO-RETRY-RESET）。
        self.auto_retry_counts.remove(task_id);
        if self.active_tasks.contains_key(task_id) {
            let is_terminal = task_row.status == 3 || task_row.status == 4;
            if !is_terminal {
                return; // truly still active — do not interrupt
            }
            log_info!(
                "[manager] resume_task {}: stale active_tasks entry (terminal in DB) — force-removing",
                task_id
            );
            self.active_tasks.remove(task_id);
        }

        if self.pending_queue.iter().any(|q| q.task_id == task_id) {
            return;
        }

        let is_bt = is_bt_url(&task_row.url);
        let queue_id = task_row.queue_id.clone();

        if is_bt || (self.has_capacity().await && self.has_queue_capacity(&queue_id)) {
            self.do_resume_task(task_id).await;
            self.drain_queue().await;
        } else {
            log_info!(
                "[manager] queuing resume for task {} (active={}, max={}, queue={})",
                task_id,
                self.active_tasks.len(),
                self.max_concurrent,
                queue_id
            );
            self.sink.emit(EngineEvent::TaskProgress {
                task_id: task_id.to_string(),
                status: 0,
                downloaded_bytes: task_row.downloaded_bytes,
                total_bytes: task_row.total_bytes,
                speed: 0,
                file_name: task_row.file_name.clone(),
                save_dir: task_row.save_dir.clone(),
                url: task_row.url.clone(),
                error_message: String::new(),
                upload_speed_bps: 0,
                uploaded_bytes: task_row.uploaded_bytes,
                seeding_status: task_row.seeding_status,
                seeding_message: task_row.seeding_message.clone(),
            });
            self.pending_queue.push_back(QueuedTask {
                task_id: task_id.to_string(),
                url: task_row.url,
                save_dir: task_row.save_dir,
                file_name: task_row.file_name,
                segments: 0,
                is_resume: true,
                cookies: String::new(), // resume 上下文由 do_resume_task 从 DB 恢复
                referrer: String::new(),
                hint_file_size: 0,
                torrent_file_bytes: Vec::new(),
                proxy_url: task_row.proxy_url,
                user_agent: String::new(),
                queue_id: task_row.queue_id,
                checksum: task_row.checksum,
                ignore_tls_errors: false, // resume path reloads the persisted value from DB
                extra_headers: std::collections::HashMap::new(), // 恢复任务无额外请求头
                selected_file_indices: Vec::new(), // resume tasks have no pre-selection
                method: None,
                body: None,
                // resume 路径 do_resume_task 从 DB 重读 audio_url，此处 None。
                audio_url: None,
                resolver_plugin_id: String::new(),
                resolved: false,
                range_supported: false,
            });
            // 入队后立即广播最新队列位置(覆盖单个 resume 与 batch_resume 批量入队;
            // broadcast_queue_positions 为只读信号,多次调用无副作用)。
            self.broadcast_queue_positions();
        }
    }

    /// Batch pause multiple tasks.
    pub async fn batch_pause(&mut self, task_ids: &[String]) {
        for tid in task_ids {
            self.pause_task(tid).await;
        }
    }
}

impl Drop for DownloadManager {
    fn drop(&mut self) {
        // Cancel all active downloads (non-blocking, just sets atomic flags).
        for (_tid, entry) in self.active_tasks.drain() {
            entry.token.cancel();
        }
        self.pending_queue.clear();

        // Shut down the BT session on a dedicated thread to avoid deadlock.
        // `SharedBtSession::shutdown()` calls `runtime.block_on()`, which
        // panics if called from within a tokio runtime context.  Spawning a
        // std thread guarantees we are outside any runtime.
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

    /// Broadcast the current list of named queues to Dart.
    pub async fn send_all_queues(&self) {
        match self.db.load_all_queues().await {
            Ok(queues) => self.sink.emit(EngineEvent::QueuesChanged(queues)),
            Err(e) => log_info!("[manager] load_all_queues error: {}", e),
        }
    }

    /// Create a new named queue and broadcast the updated list.
    pub async fn create_queue(
        &mut self,
        name: String,
        speed_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: String,
        default_segments: i32,
        default_user_agent: String,
    ) {
        let id = Uuid::new_v4().to_string();
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
            q.max_concurrent = max_concurrent;
            q.default_save_dir = default_save_dir;
            q.default_segments = default_segments;
            q.default_user_agent = default_user_agent;
        }
        // If a per-queue limiter already exists, update its limit in place.
        if let Some(limiter) = self.queue_limiters.get(&queue_id) {
            limiter.set_limit((speed_limit_kbps.max(0) as u64) * 1024);
        }
        log_info!("[manager] updated queue: {}", queue_id);
        self.send_all_queues().await;
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
        if let Err(e) = self.db.delete_queue(&queue_id).await {
            log_info!("[manager] delete_queue error: {}", e);
            return;
        }
        // Sync in-memory cache.
        self.queues.remove(&queue_id);
        self.queue_limiters.remove(&queue_id);
        self.schedule_fired.retain(|(qid, _), _| qid != &queue_id);
        log_info!("[manager] deleted queue: {}", queue_id);
        self.send_all_queues().await;
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
        for entry in self.pending_queue.iter_mut() {
            if entry.task_id == task_id {
                entry.queue_id = queue_id.clone();
                break;
            }
        }
        log_info!("[manager] moved task {} to queue '{}'", task_id, queue_id);
        // 定向广播归属变化：AllQueues 只带队列元数据，不带任务归属，
        // 客户端任务表若不更新会导致「移动到队列」看似无效。
        self.sink
            .emit(EngineEvent::TaskQueueChanged { task_id, queue_id });
        self.send_all_queues().await;
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
    /// 并发槽位立即让给它们）。
    pub async fn stop_queue(&mut self, queue_id: String) {
        if !self.queues.contains_key(&queue_id) {
            log_info!("[manager] stop_queue: unknown queue '{}'", queue_id);
            return;
        }
        if let Err(e) = self.db.set_queue_running(&queue_id, false).await {
            log_info!("[manager] stop_queue: persist error: {}", e);
            return;
        }
        if let Some(q) = self.queues.get_mut(&queue_id) {
            q.is_running = false;
        }
        // 先暂停排队中的（直接从 pending_queue 摘除，不触发 drain），再暂停
        // 活跃的——顺序反了会让 pause 触发的 drain_queue 把本队列仍在排队的
        // 任务顶进刚释放的槽位。
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
        log_info!(
            "[manager] stop_queue '{}': pausing {} task(s)",
            queue_id,
            to_pause.len()
        );
        for tid in &to_pause {
            self.pause_task(tid).await;
        }
        self.send_all_queues().await;
    }

    /// 更新队列的每日定时计划并广播。`start`/`stop` 为 `HH:MM`（空 = 该
    /// 边沿不定时，两者彼此独立：只停不启/只启不停均合法）；`days` 为
    /// 星期位掩码（bit0=周一 … bit6=周日），0 视作每天。非法时间格式
    /// 忽略本次更新。
    pub async fn set_queue_schedule(
        &mut self,
        queue_id: String,
        enabled: bool,
        start: String,
        stop: String,
        days: i32,
    ) {
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
        for key in passed_edges {
            self.schedule_fired.insert(key, today);
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

    /// 全局恢复：恢复所有「运行中队列」内的 paused 任务；停止队列内的任务
    /// 不动（由「启动队列」显式恢复）。返回尝试恢复的任务数。
    pub async fn resume_all_eligible(&mut self) -> usize {
        match self.db.eligible_resume_task_ids().await {
            Ok(ids) => {
                let n = ids.len();
                self.batch_resume(&ids).await;
                n
            }
            Err(e) => {
                log_info!("[manager] resume_all_eligible error: {}", e);
                0
            }
        }
    }

    // -----------------------------------------------------------------------
    // Boost / Priority download
    // -----------------------------------------------------------------------

    /// Set or toggle the priority (Boost) download task.
    ///
    /// - If `task_id` is empty, or equals the current priority task → cancel boost.
    /// - Otherwise: auto-pause all other active/queued tasks, ensure the target
    ///   task is downloading, and broadcast the new state to Dart.
    pub async fn set_priority_task(&mut self, task_id: String) {
        // Toggle off if same task or empty
        if task_id.is_empty() || self.priority_task_id.as_deref() == Some(task_id.as_str()) {
            self.clear_priority().await;
            return;
        }

        // 切换 boost 目标时，保留上一轮 boost 自动暂停的任务 ID，
        // 使它们在新 boost 结束时也能一并被恢复，避免永久卡在暂停状态。
        // 将新目标从集合中移除（它将被启动，不需要在结束时当作"恢复对象"）。
        self.auto_paused_ids.remove(&task_id);
        self.priority_task_id = None;

        // Step 1: If the target task is currently waiting in pending_queue, extract it
        // before we start pausing others.  This is critical: without this, two problems occur:
        //   a) resume_task() has an early-return guard for tasks already in pending_queue,
        //      so the target would never actually start.
        //   b) drain_queue() called inside each pause_task() call below could promote
        //      a different queued task to active, causing it to immediately get paused again.
        // By removing the target first we guarantee it won't be touched by drain_queue.
        let target_was_queued = self
            .pending_queue
            .iter()
            .position(|q| q.task_id == task_id)
            .map(|pos| {
                self.pending_queue.remove(pos);
                true
            })
            .unwrap_or(false);

        // Step 2: Auto-pause all currently active tasks (except the target itself,
        // which may already be downloading).
        // Note: each pause_task() call invokes drain_queue(), which could promote a
        // queued task to active.  We collect active IDs first, then pause them.
        let active_ids: Vec<String> = self
            .active_tasks
            .keys()
            .filter(|id| id.as_str() != task_id.as_str())
            .cloned()
            .collect();
        for id in active_ids {
            self.auto_paused_ids.insert(id.clone());
            self.pause_task(&id).await;
        }

        // Step 3: Pause all remaining tasks in the pending queue (excluding the target).
        let queued_ids: Vec<String> = self
            .pending_queue
            .iter()
            .filter(|t| t.task_id != task_id.as_str())
            .map(|t| t.task_id.clone())
            .collect();
        for id in queued_ids {
            self.auto_paused_ids.insert(id.clone());
            self.pause_task(&id).await;
        }

        // Step 4: Mop up — drain_queue() calls in step 2/3 may have promoted additional
        // tasks to active.  Pause anything that slipped through.
        let stray_active: Vec<String> = self
            .active_tasks
            .keys()
            .filter(|id| id.as_str() != task_id.as_str() && !self.auto_paused_ids.contains(*id))
            .cloned()
            .collect();
        for id in stray_active {
            self.auto_paused_ids.insert(id.clone());
            self.pause_task(&id).await;
        }

        self.priority_task_id = Some(task_id.clone());

        // Step 5: Ensure the target task is downloading.
        // For a previously-queued target: it was removed from pending_queue in step 1
        // so resume_task() will proceed normally (no early-return guard).
        // For an already-active target: nothing to do.
        if !self.active_tasks.contains_key(&task_id) {
            // Remove from auto_paused_ids so clear_priority won't try to resume
            // the task that's already running as priority.
            self.auto_paused_ids.remove(&task_id);
            if target_was_queued {
                // Task was queued but never actually started (pending_queue slot) —
                // call do_resume_task directly since we already verified capacity
                // by pausing all other tasks above.
                self.do_resume_task(&task_id).await;
            } else {
                // Task was paused/error — use the full resume path.
                self.resume_task(&task_id).await;
            }
        }

        // 验证目标任务是否真的启动成功。
        // 若 do_resume_task / resume_task 内部出错（DB 读取失败、BT 初始化失败等），
        // 任务不会出现在 active_tokens 中。此时必须取消 boost 并恢复已暂停的任务，
        // 否则 Dart 侧会显示 boost 激活但实际无任务下载，产生莫名其妙的结果。
        if !self.active_tasks.contains_key(&task_id) {
            log_info!(
                "[manager] boost: target task {} failed to start — cancelling boost mode",
                task_id
            );
            self.clear_priority().await;
            return;
        }

        log_info!(
            "[manager] boost mode: priority={}, auto_paused={}",
            task_id,
            self.auto_paused_ids.len()
        );

        self.sink.emit(EngineEvent::PriorityTaskChanged {
            priority_task_id: task_id,
            auto_paused_count: self.auto_paused_ids.len() as i32,
        });
    }

    /// Cancel boost mode and resume all auto-paused tasks.
    async fn clear_priority(&mut self) {
        self.priority_task_id = None;
        let to_resume: Vec<String> = self.auto_paused_ids.drain().collect();
        log_info!(
            "[manager] boost cancelled, resuming {} tasks",
            to_resume.len()
        );
        for id in &to_resume {
            // Bug 5 修复：跳过已完成的任务，避免 clear_priority 误重启已完成下载。
            // 场景：boost 激活期间某任务恰好完成，clear_priority 时不应再 resume 它。
            let is_completed = self
                .db
                .load_task_by_id(id)
                .await
                .ok()
                .flatten()
                .map(|t| t.status == 3)
                .unwrap_or(false);
            if is_completed {
                log_info!("[manager] clear_priority: skipping completed task {}", id);
                continue;
            }
            self.resume_task(id).await;
        }
        // 在发出 PriorityTaskChanged 之前广播最新队列位置。
        // resume_task 对于无空余槽的任务只是将其入队，不会主动广播。
        // 此次广播确保 Dart 在收到 PriorityTaskChanged 时已知道哪些任务在队列中
        // （queuePosition > 0），使 pauseAll 能正确识别并暂停它们。
        self.broadcast_queue_positions();
        self.sink.emit(EngineEvent::PriorityTaskChanged {
            priority_task_id: String::new(),
            auto_paused_count: 0,
        });
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

/// Decay factor applied to EMA when no new bytes arrive during a full
/// sampling window.  0.5 per window means speed halves every second during
/// a stall, reaching <1 KB/s in ~10 windows (~10 s) for a 1 MB/s baseline.
const SPEED_DECAY_FACTOR: f64 = 0.5;

/// Minimum interval between forwarding progress to Dart (per task) to avoid
/// flooding the signal channel when many segments report simultaneously.
const MIN_DART_INTERVAL_MS: u128 = 500;

pub async fn progress_reporter(
    mut rx: mpsc::Receiver<ProgressUpdate>,
    db: Db,
    sink: Arc<dyn EventSink>,
) {
    let mut states: HashMap<String, TaskSpeedState> = HashMap::new();
    // Track last time we sent a signal to Dart per task (rate limiting).
    let mut last_dart_send: HashMap<String, std::time::Instant> = HashMap::new();
    // Track last DB persistence per task (independent of Dart updates).
    let mut last_db_save: HashMap<String, std::time::Instant> = HashMap::new();
    // BT 数据完成通知去重:每个 task_id 只发一次 `EngineEvent::BtDataFinished`
    // (完成搬移失败后的重试路径可能再次进入 finished 分支)。
    let mut bt_finish_notified: HashSet<String> = HashSet::new();

    while let Some(update) = rx.recv().await {
        let now = std::time::Instant::now();

        // Latch file_name: once we get a non-empty name, remember it.
        let state = states.entry(update.task_id.clone()).or_insert_with(|| {
            TaskSpeedState {
                ema_speed: 0.0,
                sample_bytes: update.downloaded_bytes,
                sample_time: now,
                latest_bytes: update.downloaded_bytes,
                file_name: String::new(),
                cached_segments: None,
                last_sent_status: -1, // never sent yet
                last_raw_status: update.status,
                speed_warmup_remaining: if update.status == 1 { 1 } else { 0 },
                logged_missing_segments: false,
                upload_bps: 0,
            }
        });

        if !update.file_name.is_empty() {
            state.file_name = update.file_name.clone();
        }

        // Always cache the latest segment snapshot, regardless of rate-limiting.
        if update.segment_details.is_some() {
            state.cached_segments = update.segment_details.clone();
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
        // - A warmup window skips the first sample to prevent spikes.
        // -----------------------------------------------------------------
        let entered_downloading = update.status == 1 && state.last_raw_status != 1;
        if entered_downloading {
            state.ema_speed = 0.0;
            state.sample_bytes = update.downloaded_bytes;
            state.sample_time = now;
            state.speed_warmup_remaining = 1;
        }

        if update.status == 1 {
            // Non-monotonic check (e.g. server reset, re-probe).
            if update.downloaded_bytes < state.latest_bytes {
                state.ema_speed = 0.0;
                state.sample_bytes = update.downloaded_bytes;
                state.sample_time = now;
                state.speed_warmup_remaining = 1;
            }

            state.latest_bytes = update.downloaded_bytes;

            // Only compute speed when the sampling window expires.
            let window_elapsed_ms = now.duration_since(state.sample_time).as_millis();
            if window_elapsed_ms >= SPEED_SAMPLE_INTERVAL_MS {
                let dt = now.duration_since(state.sample_time).as_secs_f64();
                let delta = update.downloaded_bytes - state.sample_bytes;

                if state.speed_warmup_remaining > 0 {
                    // Warmup: just advance baseline, skip speed calc.
                    state.speed_warmup_remaining -= 1;
                } else if delta > 0 && dt > 0.01 {
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
                state.sample_bytes = update.downloaded_bytes;
                state.sample_time = now;
            }
            // Within the window: just accumulate bytes, no speed recalc.
        } else {
            // Non-downloading state: reset everything.
            state.ema_speed = 0.0;
            state.speed_warmup_remaining = 0;
            state.sample_bytes = update.downloaded_bytes;
            state.sample_time = now;
            state.latest_bytes = update.downloaded_bytes;
        }
        state.last_raw_status = update.status;
        state.upload_bps = update.upload_speed_bps;

        // BT 数据下载完成标记:绕过节流立即上报(一次性事件,节流可能吞掉),
        // 按 task_id 去重。
        if update.bt_data_finished && bt_finish_notified.insert(update.task_id.clone()) {
            sink.emit(EngineEvent::BtDataFinished {
                task_id: update.task_id.clone(),
            });
        }

        let smoothed_speed = state.ema_speed as i64;
        let resolved_name = state.file_name.clone();

        // For terminal states (completed / error / paused) always send immediately.
        // For downloading (status=1) and preparing (status=5), rate-limit to avoid flooding Dart.
        // BT tasks that are actively seeding (status=3, seeding_status=1) are also
        // treated as live so the UI keeps receiving upload speed updates.
        let is_seeding = update.seeding_status == SEEDING_STATUS_ACTIVE;
        let is_terminal = update.status != 1 && update.status != 5 && !is_seeding;
        // Status transitions (e.g. preparing→downloading) must also be sent
        // immediately so the UI never skips an intermediate state.
        let is_status_change = update.status != state.last_sent_status;
        let should_send = is_terminal || is_status_change || {
            let last = last_dart_send.get(&update.task_id);
            last.is_none()
                || now.duration_since(*last.unwrap_or(&now)).as_millis() >= MIN_DART_INTERVAL_MS
        };

        // Always send if this update carries a newly resolved file_name.
        let has_new_name = !update.file_name.is_empty();

        if should_send || has_new_name {
            // Terminal states (completed / error / paused) should report zero
            // speed so the UI doesn't show a stale EMA value. Active seeders keep
            // their upload speed so the list/detail panels remain accurate.
            let report_speed = if is_terminal { 0 } else { smoothed_speed };
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
                uploaded_bytes: update.uploaded_bytes,
                seeding_status: update.seeding_status,
                seeding_message: update.seeding_message.clone(),
            });

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
                    let _ = db_clone.update_task_progress_monotonic(&tid, dl).await;
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
                let _ = db
                    .update_task_progress_monotonic(&update.task_id, update.downloaded_bytes)
                    .await;
            }
            // Use total_bytes when available; fall back to downloaded_bytes
            // for unknown-size downloads where total_bytes may still be 0.
            let final_total = if update.total_bytes > 0 {
                update.total_bytes
            } else {
                update.downloaded_bytes
            };
            if final_total > 0 {
                let _ = db
                    .update_task_total_bytes(&update.task_id, final_total)
                    .await;
            }
        }

        // Clean up tasks that are no longer actively downloading.
        // Status 2 (paused): speed state is stale; a fresh one will be
        //   created via `or_insert_with` when the task resumes.
        // Status 3 (completed) / 4 (error/cancelled/deleted): terminal.
        // BT seeders (status=3 with seeding_status=1) stay alive so the UI
        // continues to receive live upload speed/ratio updates.
        let should_remove_state =
            update.status == 2 || update.status == 4 || (update.status == 3 && !is_seeding);
        if should_remove_state {
            states.remove(&update.task_id);
            last_dart_send.remove(&update.task_id);
            last_db_save.remove(&update.task_id);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn probe_missing_reports_deleted_file_as_missing() {
        let dir = unique_filetrack_test_dir("probe_deleted");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let file = dir.join("movie.mp4");
        std::fs::write(&file, b"data").expect("write test file");
        std::fs::remove_file(&file).expect("delete test file");

        assert_eq!(probe_missing(&file).await, Some(true));

        let _ = std::fs::remove_dir_all(&dir);
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

        let _ = std::fs::remove_dir_all(&dir);
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
        scan_missing_files(db.clone(), sink.clone(), Arc::new(AtomicBool::new(false))).await;
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
        scan_missing_files(db.clone(), sink.clone(), Arc::new(AtomicBool::new(false))).await;
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
        scan_missing_files(db.clone(), sink.clone(), Arc::new(AtomicBool::new(false))).await;
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

        let _ = std::fs::remove_dir_all(&dir);
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
        scan_missing_files(db.clone(), sink.clone(), Arc::new(AtomicBool::new(false))).await;

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

        let _ = std::fs::remove_dir_all(&dir);
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
        scan_missing_files(db.clone(), sink.clone(), Arc::new(AtomicBool::new(false))).await;

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

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// BT 数据下载完成标记的一次性契约(`bt_finish_notified` 去重集,见
    /// `progress_reporter` 文档):同一 task_id 第二次标记
    /// `bt_data_finished=true`(对应完成搬移失败后重试路径重新进入 finished
    /// 分支)不得再次触发 `EngineEvent::BtDataFinished`,否则 hub/server 会
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

    // -----------------------------------------------------------------------
    // 队列定时调度：HH:MM 解析与边沿判定
    // -----------------------------------------------------------------------

    fn sched_queue(id: &str, running: bool, start: &str, stop: &str, days: i32) -> QueueInfo {
        QueueInfo {
            queue_id: id.to_string(),
            name: id.to_string(),
            speed_limit_kbps: 0,
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
        for k in passed {
            fired.insert(k, today);
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
}
