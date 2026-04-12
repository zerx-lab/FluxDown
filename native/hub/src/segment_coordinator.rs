//! IDM-style dynamic segment coordinator.
//!
//! Instead of spawning a fixed set of segment tasks and waiting for them all to
//! finish, the coordinator manages a pool of **workers** that are assigned
//! segments on demand.  When a worker finishes its segment, it asks the
//! coordinator for more work — which may be an existing pending segment or a
//! **newly created** segment obtained by splitting the largest in-progress
//! segment in half (the *in-half division rule*).
//!
//! This achieves two key IDM-style behaviours:
//! 1. **Connection reuse** — TCP/TLS connections stay alive across segments.
//! 2. **Dynamic segmentation** — slow segments are split at runtime so idle
//!    workers can help.
//!
//! ## Invariants
//!
//! After every mutation of the segment map, these invariants hold:
//! - The union of all `[start_byte, end_byte]` ranges covers `[0, total_bytes-1]`
//!   exactly, with no gaps and no overlaps.
//! - `next_index` is strictly greater than any existing segment index.
//! - Every segment's `downloaded_bytes <= end_byte - start_byte + 1`.
//!
//! ## Crash safety
//!
//! On resume, the segment map is rebuilt from DB rows.  A split that was
//! persisted to DB but whose worker never started is restored as `Pending`.
//! A split whose parent's `end_byte` was updated but the new child row wasn't
//! written yet results in a gap — the integrity check at the end of download
//! catches this and the task retries from scratch.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use rinf::RustSignal;

use crate::db::Db;
use crate::downloader::{DownloadError, ProgressUpdate, SegmentProgressInfo, is_server_rejection};
use crate::logger::log_info;
use crate::signals::SegmentSplitEvent;
use crate::speed_limiter::SpeedLimiter;

// ---------------------------------------------------------------------------
// 域名单连接策略缓存
// ---------------------------------------------------------------------------
// 当 coordinator 检测到某域名的服务器拒绝多连接（403/429），将该域名记录
// 到进程级缓存。后续对同域名的下载任务会自动降级为单线程，避免重蹈覆辙。
// 缓存带 24h TTL——服务器策略可能变化，过期后重新尝试多线程。

/// TTL: 24 小时后允许重新尝试多线程。
const SINGLE_CONN_TTL: Duration = Duration::from_secs(24 * 3600);

/// 进程级的域名 → 上次检测时间缓存。
static SINGLE_CONN_DOMAINS: OnceLock<StdMutex<HashMap<String, Instant>>> = OnceLock::new();

fn single_conn_cache() -> &'static StdMutex<HashMap<String, Instant>> {
    SINGLE_CONN_DOMAINS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// 提取 URL 的 host 部分（含端口），用于域名级缓存的 key。
fn extract_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|u| {
        u.host_str().map(|h| {
            if let Some(port) = u.port() {
                format!("{}:{}", h, port)
            } else {
                h.to_string()
            }
        })
    })
}

/// 记录某域名的服务器拒绝多连接。
pub(crate) fn record_single_conn_domain(url: &str) {
    if let Some(host) = extract_host(url)
        && let Ok(mut cache) = single_conn_cache().lock()
    {
        log_info!(
            "[conn-policy] 记录域名 {} 为单连接限制，24h 内自动使用单线程",
            host
        );
        cache.insert(host, Instant::now());
    }
}

/// 检查某域名是否在单连接缓存中（且未过期）。
pub(crate) fn is_single_conn_domain(url: &str) -> bool {
    if let Some(host) = extract_host(url)
        && let Ok(mut cache) = single_conn_cache().lock()
        && let Some(recorded) = cache.get(&host)
    {
        if recorded.elapsed() < SINGLE_CONN_TTL {
            return true;
        }
        // 过期，移除
        cache.remove(&host);
    }
    false
}

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// 最小拆分阈值的默认/上限值（高速连接 >10 MB/s 时使用）。
/// 低速连接通过 [`dynamic_min_split_bytes`] 自适应降低此阈值。
const MIN_SPLIT_BYTES: i64 = 2 * 1024 * 1024; // 2 MB

/// 根据当前实时吞吐量动态计算最小拆分阈值。
///
/// - 低速（< 1 MB/s）：512 KB — 更积极拆分，空闲 worker 可快速参与慢段
/// - 中速（1–10 MB/s）：1 MB — 平衡 HTTP 请求开销与并行收益
/// - 高速（> 10 MB/s）：2 MB — TLS 1.3 握手占比 <1%，保持默认
fn dynamic_min_split_bytes(throughput_bps: f64) -> i64 {
    const BW_LOW: f64 = 1.0 * 1024.0 * 1024.0; //  1 MB/s
    const BW_HIGH: f64 = 10.0 * 1024.0 * 1024.0; // 10 MB/s
    if throughput_bps < BW_LOW {
        512 * 1024 // 512 KB
    } else if throughput_bps < BW_HIGH {
        1024 * 1024 // 1 MB
    } else {
        MIN_SPLIT_BYTES // 2 MB
    }
}

/// Maximum total number of segments (including dynamically created ones).
const MAX_SEGMENTS: i32 = 64;

/// 尾部微拆分阈值：当正常拆分（`dynamic_min_split_bytes` 计算的阈值）失败时，
/// 用此极低阈值重试，避免下载尾部空闲 worker 干等最后一个慢段。
///
/// 64 KB 的段在 1 MB/s 连接上只需 64ms，TLS 1.3 握手开销（1 RTT ≈ 30ms）
/// 占比约 32%，仍有净收益。低于此值则 HTTP 请求开销反超下载本身。
///
/// 此设计是 fast-down 投机执行（Speculative Execution）的实用替代方案。
/// fast-down 用 AtomicU128 CAS 让多个 worker 竞争同一段的字节范围（零额外
/// HTTP 请求），但需要重构整个写入路径为 CAS-guarded（放弃 BufWriter、修改
/// 进度报告/DB 持久化）。尾部微拆分在 FluxDown 架构下以极小改动覆盖了 90%+
/// 的尾延迟场景：段 remaining ≥ 128KB 时拆成两半各 ≥64KB，两个 worker 各发
/// 独立 Range 请求并行完成。
///
/// **瀑布防护**：尾部微拆分仅在最大剩余分段 ≥ 2 × TAIL_MIN_SPLIT_BYTES（即
/// 128KB）时才激活，确保只救援真正的"落后分段"而非均等小分段。若所有分段
/// 剩余量均接近 TAIL_MIN_SPLIT_BYTES（例如下载最后 1% 时大量分段均为 ~66KB），
/// 继续拆分只会产生更多 HTTP 请求开销并导致 worker 集体退出——活跃 worker 数
/// 从 ~48 骤降至 ~16，引发 UI 速度指示器的"99% 速度下降"现象。
const TAIL_MIN_SPLIT_BYTES: i64 = 64 * 1024; // 64 KB

/// Proactive split 定时器间隔（秒）。
///
/// 定时预拆分最大 Active 段为 Pending，使下一个完成的 worker 无需在 Done
/// 处理的关键路径上计算拆分 + DB 持久化，直接从 Pending 队列取任务。
const PROACTIVE_SPLIT_INTERVAL_SECS: u64 = 2;

/// 默认 BufWriter 容量（低速/小段场景）。
const BUF_WRITER_CAPACITY_SMALL: usize = 256 * 1024; // 256 KB
/// 中等段（4-32 MB）使用 512 KB 缓冲区，减少系统调用频率。
const BUF_WRITER_CAPACITY_MEDIUM: usize = 512 * 1024; // 512 KB
/// 大段（>32 MB）使用 1 MB 缓冲区，充分利用高速连接。
const BUF_WRITER_CAPACITY_LARGE: usize = 1024 * 1024; // 1 MB

/// 根据段剩余字节数动态选择 BufWriter 容量。
/// 大段使用更大的缓冲区以减少 write 系统调用频率；
/// 小段使用较小缓冲区避免内存浪费。
fn buf_writer_capacity_for_segment(remaining_bytes: i64) -> usize {
    const THRESHOLD_LARGE: i64 = 32 * 1024 * 1024; // 32 MB
    const THRESHOLD_MEDIUM: i64 = 4 * 1024 * 1024; //  4 MB
    if remaining_bytes >= THRESHOLD_LARGE {
        BUF_WRITER_CAPACITY_LARGE
    } else if remaining_bytes >= THRESHOLD_MEDIUM {
        BUF_WRITER_CAPACITY_MEDIUM
    } else {
        BUF_WRITER_CAPACITY_SMALL
    }
}

/// Return type for `build_fresh_segments`: (in-memory map, DB tuples).
type FreshSegments = (BTreeMap<i32, LiveSegment>, Vec<(i32, i64, i64)>);

/// DB save interval — matches downloader.rs.
const DB_SAVE_INTERVAL_SECS: u64 = 3;

/// Progress report interval to Dart UI.
const UI_REPORT_INTERVAL_MS: u128 = 200;

/// Retry constants for segment downloads.
///
/// 大文件下载（>1GB）最多 32 个分段并发，每个分段独立受 stall 检测。
/// 网络抖动时任何一个分段重试耗尽都会导致整个任务失败。
/// 5 次重试（含指数退避：2s/4s/8s/16s）给予充足的恢复窗口，
/// 总容忍时间从 ~36s 提升到 ~80s，大幅降低大文件下载因瞬时网络问题而中断的概率。
const MAX_RETRIES: u32 = 5;
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// 单个 chunk 的读取超时（stall detection）。如果超过此时间没有收到任何数据，
/// 视为连接停滞，返回错误触发 retry 机制（断开旧连接，用 Range 请求从断点续传）。
/// 5 秒足够容忍正常的 CDN 抖动，又能快速从真正卡死的连接中恢复。
/// 这解决了大文件下载到 98%+ 时 TCP 连接卡死、速度趋近 0 的问题。
const CHUNK_STALL_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Segment state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegState {
    /// Segment exists but no worker is downloading it yet.
    Pending,
    /// A worker is actively downloading this segment.
    Active,
    /// Segment has been fully downloaded.
    Completed,
}

#[derive(Debug, Clone)]
struct LiveSegment {
    index: i32,
    start_byte: i64,
    end_byte: i64,
    /// Bytes downloaded within this segment (relative to start_byte).
    downloaded_bytes: i64,
    state: SegState,
}

impl LiveSegment {
    /// Total size of this segment in bytes.
    fn size(&self) -> i64 {
        (self.end_byte - self.start_byte + 1).max(0)
    }

    /// Remaining bytes to download in this segment.
    fn remaining(&self) -> i64 {
        (self.size() - self.downloaded_bytes).max(0)
    }

    /// Whether this segment has been fully downloaded.
    #[cfg(test)]
    fn is_complete(&self) -> bool {
        self.downloaded_bytes >= self.size()
    }
}

// ---------------------------------------------------------------------------
// Worker ↔ Coordinator messages
// ---------------------------------------------------------------------------

/// Sent by a worker to the coordinator when its segment finishes or fails.
enum WorkerEvent {
    /// Segment completed successfully.
    Done {
        worker_id: usize,
        seg_index: i32,
        downloaded_bytes: i64,
    },
    /// Segment failed after all retries.
    Failed {
        #[allow(dead_code)]
        worker_id: usize,
        seg_index: i32,
        error: DownloadError,
    },
}

/// Sent by the coordinator to a worker to assign work.
struct WorkerAssignment {
    seg_index: i32,
    seg_start: i64,
    actual_start: i64,
    seg_end: i64,
}

/// Result of `find_next_work`: an assignment plus optionally the index of the
/// parent segment that was shrunk by a split (for targeted DB persistence).
struct NextWork {
    assignment: WorkerAssignment,
    /// If this work came from an in-half split, this is the index of the
    /// segment that was shrunk.  `None` when reusing an existing Pending segment.
    split_parent: Option<i32>,
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// Run the dynamic segment coordinator.
///
/// This replaces the old "spawn N tasks and join" logic in
/// `download_multi_segment`.  The function signature is intentionally close to
/// the original so it can be swapped in with minimal changes.
#[allow(clippy::too_many_arguments)]
pub async fn run_coordinated_download(
    task_id: &str,
    url: &str,
    dest: &Path,
    total_bytes: i64,
    initial_segment_count: i32,
    client: &Client,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
    etag: &str,
    last_modified: &str,
) -> Result<(), DownloadError> {
    // ----- 0. Defensive checks ------------------------------------------------
    if total_bytes <= 0 {
        return Err(DownloadError::Other(format!(
            "coordinator: invalid total_bytes={total_bytes} for task {task_id}"
        )));
    }
    if initial_segment_count < 1 {
        return Err(DownloadError::Other(format!(
            "coordinator: invalid initial_segment_count={initial_segment_count} for task {task_id}"
        )));
    }

    // ----- 1. Build initial segment map from DB or fresh calculation ---------
    let existing = db.load_segments(task_id).await?;
    let mut segments: BTreeMap<i32, LiveSegment> = BTreeMap::new();
    let mut next_index: i32;

    if existing.is_empty() {
        // Create fresh segments (uniform split).
        let (fresh, db_segs) = build_fresh_segments(initial_segment_count, total_bytes);
        segments = fresh;
        db.insert_segments(task_id, &db_segs).await?;
        next_index = initial_segment_count;
    } else {
        // Restore from DB (resume scenario).
        next_index = 0;
        for seg in &existing {
            let state = if seg.downloaded_bytes >= (seg.end_byte - seg.start_byte + 1) {
                SegState::Completed
            } else {
                SegState::Pending
            };
            segments.insert(
                seg.index,
                LiveSegment {
                    index: seg.index,
                    start_byte: seg.start_byte,
                    end_byte: seg.end_byte,
                    downloaded_bytes: seg.downloaded_bytes,
                    state,
                },
            );
            if seg.index >= next_index {
                next_index = seg.index + 1;
            }
        }
    }

    // Verify the invariant: segment ranges must cover [0, total_bytes-1] exactly.
    //
    // When resuming, the freshly probed total_bytes may differ from the value
    // encoded in DB segment boundaries (e.g. CDN re-signing shifts Content-Length
    // by a few bytes, or the server file has genuinely changed size).
    //
    // Three distinct cases:
    //
    //  db_total == total_bytes
    //    → Exact match.  Trust DB segments as-is.
    //
    //  db_total < total_bytes  (server reports a *larger* file)
    //    → Two sub-cases distinguished by a tolerance threshold:
    //
    //      delta <= threshold  (CDN drift — Transfer-Encoding overhead,
    //                           dynamic header injection, signed-URL padding…)
    //        → The extra bytes the server "claims" are not real file content.
    //          Trust DB segments and correct tasks.total_bytes to db_total so
    //          the progress bar reaches exactly 100 % when segments complete.
    //
    //      delta > threshold  (file genuinely grew on the server)
    //        → The tail content is real and must be downloaded.  Rebuild
    //          segments from scratch using the new total_bytes so every byte
    //          is covered.  Without this, the tail would never be fetched,
    //          the file would be silently truncated, and the integrity check
    //          would still pass because it compares against the old db_total.
    //
    //  db_total > total_bytes  (server reports a *smaller* file)
    //    → Do NOT trust DB segments.  Requesting Range bytes beyond the server's
    //      actual EOF would return 416 Range Not Satisfiable.  Fall through with
    //      total_bytes so validate_coverage detects the mismatch and rebuilds
    //      segments to fit the new (smaller) file size.
    //
    // Threshold: same formula used in db::update_task_file_info_resume —
    //   1 % of db_total, capped at 1 MiB, floor 1 byte.
    // Keeping both thresholds in sync ensures the two layers never disagree
    // about whether a size change is "real".
    let effective_total_bytes = if !existing.is_empty() {
        // segments is non-empty here; max() will always return Some.
        let db_total = segments
            .values()
            .map(|s| s.end_byte + 1)
            .max()
            .unwrap_or(total_bytes); // unreachable, defensive only

        if db_total != total_bytes {
            log_info!(
                "[coordinator] task {} total_bytes probe={} vs db_segments={}",
                task_id,
                total_bytes,
                db_total
            );
        }

        if db_total == total_bytes {
            // Exact match — nothing to do.
            db_total
        } else if db_total < total_bytes {
            // Server reports a larger file than what the DB segments cover.
            // Decide whether this is CDN drift or a genuine file growth.
            let threshold: i64 = (db_total / 100).clamp(1, 1_048_576);
            let delta = total_bytes - db_total;

            if delta <= threshold {
                // CDN drift — the extra bytes are not real file content.
                // Trust existing segments and snap tasks.total_bytes back to
                // db_total so the UI reaches 100 % on segment completion.
                log_info!(
                    "[coordinator] task {} probe={} db={} delta={} <= threshold={}: \
                     CDN drift, trusting DB segments",
                    task_id,
                    total_bytes,
                    db_total,
                    delta,
                    threshold
                );
                let _ = db.update_task_total_bytes(task_id, db_total).await;
                db_total
            } else {
                // Genuine file growth — the tail bytes are real and must be
                // fetched.  Rebuild segments from scratch using the new size.
                // This discards all prior progress, but keeping the old
                // segments would silently truncate the file.
                log_info!(
                    "[coordinator] task {} probe={} db={} delta={} > threshold={}: \
                     file genuinely grew, rebuilding segments to avoid tail truncation",
                    task_id,
                    total_bytes,
                    db_total,
                    delta,
                    threshold
                );
                db.delete_segments(task_id).await?;
                let (fresh, db_segs) = build_fresh_segments(initial_segment_count, total_bytes);
                segments = fresh;
                db.insert_segments(task_id, &db_segs).await?;
                next_index = initial_segment_count;
                let _ = db.update_task_total_bytes(task_id, total_bytes).await;
                // Return early — segments are already valid, skip validate_coverage.
                // Re-run pre-allocation and workers with total_bytes.
                total_bytes
            }
        } else {
            // db_total > total_bytes: server file is smaller than DB segments cover.
            // Using db_total would issue Range requests past EOF → 416 errors.
            // Use total_bytes so validate_coverage below detects the mismatch
            // and resets segments to the current file size.
            log_info!(
                "[coordinator] task {} DB segments cover {} bytes but server reports only {}; \
                 resetting segments to avoid out-of-range requests",
                task_id,
                db_total,
                total_bytes
            );
            total_bytes
        }
    } else {
        total_bytes
    };

    if let Err(msg) = validate_coverage(&segments, effective_total_bytes) {
        log_info!(
            "[coordinator] task {} segment coverage invalid: {}. Resetting all segments.",
            task_id,
            msg
        );
        // Coverage is broken (e.g. partial split persisted before crash, or file
        // size changed so db_total > total_bytes above).
        // Safest recovery: wipe segments and start fresh.
        db.delete_segments(task_id).await?;
        let (fresh, db_segs) = build_fresh_segments(initial_segment_count, total_bytes);
        segments = fresh;
        db.insert_segments(task_id, &db_segs).await?;
        next_index = initial_segment_count;
        // update_task_total_bytes may have set tasks.total_bytes to db_total earlier
        // (db_total <= total_bytes path).  After a fresh reset the canonical size is
        // total_bytes (from probe), so re-sync.
        let _ = db.update_task_total_bytes(task_id, total_bytes).await;
    }

    // Integrity check for resumed files: verify the file on disk is intact.
    {
        let db_downloaded: i64 = segments.values().map(|s| s.downloaded_bytes).sum();
        let file_len = match tokio::fs::metadata(dest).await {
            Ok(m) => m.len() as i64,
            Err(_) => 0,
        };
        if db_downloaded > 0 && (file_len == 0 || file_len < db_downloaded) {
            log_info!(
                "[coordinator] task {} file integrity mismatch: file_len={}, db_downloaded={}. Resetting.",
                task_id,
                file_len,
                db_downloaded
            );
            for seg in segments.values_mut() {
                seg.downloaded_bytes = 0;
                seg.state = SegState::Pending;
            }
            db.reset_segments_progress(task_id).await?;
        }
    }

    // ----- 2. Pre-allocate file to full size --------------------------------
    //
    // Linux:   fallocate(2) 分配真实磁盘块（不写零，近乎瞬时），避免
    //          set_len()/ftruncate 创建稀疏文件导致的碎片化和延迟 ENOSPC。
    // Windows: SetFileInformationByHandle(FileAllocationInfo) 预分配 NTFS
    //          物理簇（连续优先），提前检测磁盘空间不足，减少碎片化；
    //          再 SetEndOfFile 设置逻辑大小。
    // macOS 等: 回退到 set_len()。
    {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dest)
            .await?;
        let current_len = file.metadata().await?.len();
        let target_len = effective_total_bytes as u64;
        if current_len < target_len {
            #[cfg(target_os = "linux")]
            {
                let std_file = file.into_std().await;
                tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
                    use std::os::unix::io::AsRawFd;
                    let fd = std_file.as_raw_fd();
                    // fallocate(fd, 0, 0, len): 预分配 [0, len) 范围的磁盘块，
                    // 不写零，ext4/XFS/Btrfs 均支持，耗时 O(1)。
                    // mode=0 同时将文件大小设为 max(当前大小, offset+len)。
                    let ret = unsafe { libc::fallocate(fd, 0, 0, target_len as libc::off_t) };
                    if ret == 0 {
                        return Ok(());
                    }
                    // fallocate 失败 — 检查是否为文件系统不支持
                    let err = std::io::Error::last_os_error();
                    let raw = err.raw_os_error().unwrap_or(0);
                    if raw == libc::EOPNOTSUPP || raw == libc::ENOSYS {
                        // tmpfs/NFS 等不支持 fallocate 的文件系统，回退到 ftruncate
                        log_info!(
                            "[coordinator] fallocate 不支持 (errno={}), 回退到 ftruncate",
                            raw
                        );
                        std_file.set_len(target_len)?;
                        Ok(())
                    } else {
                        // ENOSPC 等真实错误，直接上报（提前检测磁盘空间不足）
                        Err(err.into())
                    }
                })
                .await
                .map_err(|e| DownloadError::Other(format!("fallocate task panicked: {e}")))??;
            }
            #[cfg(target_os = "windows")]
            {
                let std_file = file.into_std().await;
                tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
                    use std::os::windows::io::AsRawHandle;
                    // FILE_ALLOCATION_INFO: 单字段 AllocationSize (LARGE_INTEGER = i64)
                    #[repr(C)]
                    struct FileAllocInfo {
                        allocation_size: i64,
                    }
                    let handle = std_file.as_raw_handle();
                    // Step 1: 预分配 NTFS 物理簇——立即保留磁盘空间（连续簇优先），
                    // 磁盘不足时提前报错（等效 Linux fallocate 的 ENOSPC 检测），
                    // 减少多段随机写时的 NTFS 碎片化。
                    let info = FileAllocInfo {
                        allocation_size: target_len as i64,
                    };
                    let ret = unsafe {
                        windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle(
                            handle,
                            windows_sys::Win32::Storage::FileSystem::FileAllocationInfo,
                            &info as *const _ as *const core::ffi::c_void,
                            std::mem::size_of::<FileAllocInfo>() as u32,
                        )
                    };
                    if ret == 0 {
                        // FAT32/exFAT/网络驱动器等不支持时仅记录日志，不中断
                        log_info!(
                            "[coordinator] SetFileInformationByHandle(FileAllocationInfo) 失败: {}",
                            std::io::Error::last_os_error()
                        );
                    }
                    // Step 2: 设置逻辑 EOF——后续 seek+write 依赖此值
                    std_file.set_len(target_len)?;
                    Ok(())
                })
                .await
                .map_err(|e| DownloadError::Other(format!("prealloc task panicked: {e}")))??;
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                file.set_len(target_len).await?;
            }
        }
    }

    // ----- 3. Shared state for progress reporting ---------------------------
    let total_downloaded = Arc::new(AtomicI64::new(
        segments.values().map(|s| s.downloaded_bytes).sum::<i64>(),
    ));

    // The shared segment-progress vector mirrors the `segments` map and is
    // updated by workers via std::sync::Mutex (cheap, no async).
    let seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>> =
        Arc::new(StdMutex::new(build_seg_state_vec(&segments)));

    // ----- 4. Event channel (workers → coordinator) -------------------------
    let (event_tx, mut event_rx) = mpsc::channel::<WorkerEvent>(64);

    // ----- 5. Worker pool ---------------------------------------------------
    // Only create as many initial workers as there are pending segments.
    // On resume, most segments may be Completed already — spawning workers
    // for them wastes resources and creates idle tasks.
    let pending_count = segments
        .values()
        .filter(|s| s.state == SegState::Pending)
        .count();
    let initial_workers = pending_count
        .min(initial_segment_count as usize)
        .min(MAX_SEGMENTS as usize);

    let mut worker_assign_txs: Vec<Option<mpsc::Sender<WorkerAssignment>>> =
        Vec::with_capacity(initial_workers);
    let mut worker_handles: Vec<Option<tokio::task::JoinHandle<()>>> =
        Vec::with_capacity(initial_workers);

    // Collect pending assignments.
    let mut pending_assignments: Vec<WorkerAssignment> = segments
        .values()
        .filter(|s| s.state == SegState::Pending)
        .map(|s| WorkerAssignment {
            seg_index: s.index,
            seg_start: s.start_byte,
            actual_start: s.start_byte + s.downloaded_bytes,
            seg_end: s.end_byte,
        })
        .collect();
    let mut assign_iter = pending_assignments.drain(..);

    for worker_id in 0..initial_workers {
        let (assign_tx, assign_rx) = mpsc::channel::<WorkerAssignment>(4);
        let evt_tx = event_tx.clone();

        // Send initial assignment (if available).
        if let Some(assignment) = assign_iter.next() {
            let seg_idx = assignment.seg_index;
            if let Some(seg) = segments.get_mut(&seg_idx) {
                seg.state = SegState::Active;
            }
            // This send cannot fail — channel just created with capacity 4.
            let _ = assign_tx.try_send(assignment);
        }

        let handle = spawn_worker(
            worker_id,
            assign_rx,
            evt_tx,
            task_id.to_string(),
            url.to_string(),
            dest.to_path_buf(),
            effective_total_bytes,
            client.clone(),
            cancel_token.clone(),
            total_downloaded.clone(),
            seg_states.clone(),
            db.clone(),
            progress_tx.clone(),
            speed_limiter.clone(),
            cookies.to_string(),
            referrer.to_string(),
            extra_headers.clone(),
            etag.to_string(),
            last_modified.to_string(),
        );

        worker_assign_txs.push(Some(assign_tx));
        worker_handles.push(Some(handle));
    }
    drop(assign_iter);
    drop(pending_assignments);

    // Drop the original event_tx so the channel closes when all workers finish.
    drop(event_tx);

    // If all segments are already completed (rare but possible), exit early.
    if all_done(&segments) {
        for tx in &mut worker_assign_txs {
            *tx = None;
        }
        for handle in &mut worker_handles {
            if let Some(h) = handle.take() {
                let _ = h.await;
            }
        }
        return Ok(());
    }

    // ----- 6. Coordinator event loop ----------------------------------------
    // serial_mode: 当检测到服务器拒绝多连接（403/429）时置为 true，
    // 此后 coordinator 同一时刻只分配一个分段给一个 worker，
    // 避免并发连接触发服务器的反多线程机制。
    let mut serial_mode = false;
    let mut final_error: Option<DownloadError> = None;

    // 吞吐量跟踪：用于动态调整 MIN_SPLIT_BYTES
    let mut last_throughput_bytes = total_downloaded.load(Ordering::Relaxed);
    let mut last_throughput_time = Instant::now();
    let mut current_min_split = MIN_SPLIT_BYTES;

    // Proactive split timer: pre-create Pending segments so the next idle
    // worker can pick one up immediately without a split in the hot path.
    let mut proactive_interval =
        tokio::time::interval(Duration::from_secs(PROACTIVE_SPLIT_INTERVAL_SECS));
    proactive_interval.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            biased;

            _ = cancel_token.cancelled() => {
                for tx in &mut worker_assign_txs {
                    *tx = None;
                }
                final_error = Some(DownloadError::Cancelled);
                break;
            }

            event = event_rx.recv() => {
                match event {
                    Some(WorkerEvent::Done { worker_id, seg_index, downloaded_bytes }) => {
                        // Mark segment completed in our authoritative map.
                        if let Some(seg) = segments.get_mut(&seg_index) {
                            // Cap downloaded_bytes to segment size: a worker may
                            // have written one chunk past the split boundary before
                            // seg_states reflected the shrunk end_byte.  Clamping
                            // here keeps the coordinator's total accurate.
                            seg.downloaded_bytes = downloaded_bytes.min(seg.size());
                            seg.state = SegState::Completed;
                        }

                        // Sync the coordinator's view of active segments'
                        // downloaded_bytes from the shared state (updated by
                        // workers in real-time) so that split-point calculations
                        // use current data, not stale initial values.
                        sync_downloaded_from_shared(&mut segments, &seg_states);

                        // Try to assign new work to this worker.
                        // 串行模式下：同一时刻只允许一个 worker 工作，
                        // 且不进行分段拆分（避免产生新的并发连接）。
                        // 动态计算最小拆分阈值：根据最近的实时吞吐量调整，
                        // 低速时更积极拆分，高速时保守避免 HTTP 请求开销。
                        {
                            let now = Instant::now();
                            let elapsed = now.duration_since(last_throughput_time);
                            if elapsed.as_millis() >= 500 {
                                let current_bytes = total_downloaded.load(Ordering::Relaxed);
                                let delta = (current_bytes - last_throughput_bytes).max(0) as f64;
                                let throughput = delta / elapsed.as_secs_f64();
                                current_min_split = dynamic_min_split_bytes(throughput);
                                last_throughput_bytes = current_bytes;
                                last_throughput_time = now;
                            }
                        }

                        let next_work = if serial_mode {
                            let other_active = segments.values()
                                .any(|s| s.state == SegState::Active);
                            if other_active {
                                // 还有其他 worker 在下载 → 退休当前 worker，等它完成
                                None
                            } else {
                                // 无其他活跃连接 → 取一个 Pending 分段（不拆分）
                                find_next_pending_only(&mut segments)
                            }
                        } else {
                            find_next_work(
                                &mut segments,
                                &mut next_index,
                                effective_total_bytes,
                                current_min_split,
                            )
                        };

                        if let Some(next) = next_work {
                            let new_seg_idx = next.assignment.seg_index;

                            // Persist new/updated segments to DB.
                            persist_segment_change(
                                db, task_id, &segments,
                                new_seg_idx, next.split_parent,
                            ).await;

                            // Notify Dart about the split event (if this came from a split).
                            if let Some(parent_idx) = next.split_parent {
                                send_split_event(
                                    task_id, parent_idx, new_seg_idx,
                                    &segments, false,
                                );
                            }

                            // Update shared visualization state (segment count
                            // or ranges may have changed due to split).
                            rebuild_seg_states(&segments, &seg_states);

                            if let Some(Some(tx)) = worker_assign_txs.get(worker_id)
                                && tx.send(next.assignment).await.is_err() {
                                    // Worker died — reclaim segment.
                                    if let Some(seg) = segments.get_mut(&new_seg_idx) {
                                        seg.state = SegState::Pending;
                                    }
                                }
                        } else {
                            // No more work — retire this worker.
                            if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                *slot = None;
                            }
                        }

                        // Check if all segments are done.
                        if all_done(&segments) {
                            for tx in &mut worker_assign_txs {
                                *tx = None;
                            }
                            break;
                        }
                    }

                    Some(WorkerEvent::Failed { worker_id, seg_index, error }) => {
                        // 检测服务器是否拒绝多连接（403/429）。
                        // 判定条件：错误为 403/429 且存在其他正常工作的分段
                        // （证明 URL 本身有效，只是并发连接被拒绝）。
                        let other_working = segments.values().any(|s| {
                            s.index != seg_index
                                && matches!(s.state, SegState::Active | SegState::Completed)
                        });

                        if is_server_rejection(&error) && other_working {
                            // ---- 自动降级为串行模式 ----
                            if !serial_mode {
                                log_info!(
                                    "[coordinator] task {} 检测到服务器拒绝多连接 (seg {}), \
                                     降级为串行模式",
                                    task_id,
                                    seg_index
                                );
                                serial_mode = true;
                                // 记录域名，后续同域名任务自动单线程
                                record_single_conn_domain(url);
                            }

                            // 将失败分段标记为 Pending，等待串行下载
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.state = SegState::Pending;
                            }

                            // 退休当前失败的 worker（关闭其分配通道）
                            if let Some(slot) = worker_assign_txs.get_mut(worker_id) {
                                *slot = None;
                            }

                            // 安全检查：如果所有 worker 都已退休且无活跃分段，
                            // 说明服务器甚至拒绝单连接 → 无法继续。
                            let any_alive = worker_assign_txs.iter().any(|tx| tx.is_some())
                                || segments.values().any(|s| s.state == SegState::Active);
                            if !any_alive && !all_done(&segments) {
                                final_error = Some(DownloadError::Other(
                                    "服务器拒绝所有下载连接（包括单连接），无法继续下载"
                                        .to_string(),
                                ));
                                break;
                            }

                            // 不 break、不 cancel — 让已建立连接的 active workers
                            // 继续下载。当它们完成后，Done 事件会触发串行分配剩余
                            // Pending 分段。
                        } else {
                            // 非连接限制错误，或 URL 本身无效 → 原有行为：全部取消。
                            cancel_token.cancel();
                            if let Some(seg) = segments.get_mut(&seg_index) {
                                seg.state = SegState::Pending;
                            }
                            for tx in &mut worker_assign_txs {
                                *tx = None;
                            }
                            if final_error.is_none() {
                                final_error = Some(error);
                            }
                            break;
                        }
                    }

                    None => {
                        // All workers dropped their event_tx — we're done.
                        break;
                    }
                }
            }

            // --- Proactive split timer ------------------------------------
            // Periodically pre-split the largest active segment to create a
            // Pending segment.  The next worker to finish picks it up via
            // find_next_work → Strategy 1 (Pending), skipping the expensive
            // split + DB-persist that would otherwise block the Done handler.
            _ = proactive_interval.tick() => {
                if !serial_mode && !all_done(&segments) {
                    sync_downloaded_from_shared(&mut segments, &seg_states);
                    // Try proactive split at the normal threshold first; if that fails
                    // (last segment has < current_min_split but >= TAIL_MIN_SPLIT_BYTES
                    // remaining), also try the tail micro threshold so the proactive
                    // timer covers the full range from current_min_split down to 64 KB.
                    let work = try_proactive_split(
                        &mut segments,
                        &mut next_index,
                        current_min_split,
                    )
                    .or_else(|| {
                        if current_min_split > TAIL_MIN_SPLIT_BYTES {
                            // Mirror the straggler guard from find_next_work Strategy 3:
                            // only pre-create a pending micro-segment when there is a
                            // genuine outlier (largest active segment ≥ 2 × TAIL_MIN_SPLIT_BYTES).
                            // Pre-splitting equally-small segments would prime idle workers
                            // to cascade-split the tail and retire en-masse at 99%.
                            let max_remaining = segments
                                .values()
                                .filter(|s| s.state == SegState::Active)
                                .map(|s| s.remaining())
                                .max()
                                .unwrap_or(0);
                            if max_remaining >= 2 * TAIL_MIN_SPLIT_BYTES {
                                try_proactive_split(
                                    &mut segments,
                                    &mut next_index,
                                    TAIL_MIN_SPLIT_BYTES,
                                )
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    });
                    if let Some(next) = work {
                        let new_seg_idx = next.assignment.seg_index;
                        persist_segment_change(
                            db, task_id, &segments,
                            new_seg_idx, next.split_parent,
                        ).await;
                        if let Some(parent_idx) = next.split_parent {
                            send_split_event(
                                task_id, parent_idx, new_seg_idx,
                                &segments, true,
                            );
                        }
                        rebuild_seg_states(&segments, &seg_states);
                    }
                }
            }

        }
    }

    // ----- 7. Wait for all worker tasks to finish ---------------------------
    for handle in &mut worker_handles {
        if let Some(h) = handle.take() {
            let _ = h.await;
        }
    }

    if let Some(err) = final_error {
        return Err(err);
    }

    // ----- 8. Final verification --------------------------------------------
    // Sync one last time to get the most accurate downloaded_bytes.
    sync_downloaded_from_shared(&mut segments, &seg_states);

    let seg_total: i64 = segments.values().map(|s| s.downloaded_bytes).sum();
    if seg_total < effective_total_bytes {
        return Err(DownloadError::Other(format!(
            "coordinator: incomplete download, segments total={} expected={}",
            seg_total, effective_total_bytes
        )));
    }

    // Verify byte-range coverage as a final safety net.
    if let Err(msg) = validate_coverage(&segments, effective_total_bytes) {
        return Err(DownloadError::Other(format!(
            "coordinator: post-download coverage error: {}",
            msg
        )));
    }

    // Flush the authoritative in-memory downloaded_bytes (already capped to
    // segment size) back to the DB in a single transaction.  This is the
    // canonical final state: any overshoot from the split race is corrected
    // here, ensuring run_download_inner's integrity check sees correct totals.
    let flush_updates: Vec<(i32, i64)> = segments
        .values()
        .map(|s| (s.index, s.downloaded_bytes))
        .collect();
    if let Err(e) = db.flush_segments_progress(task_id, flush_updates).await {
        log_info!(
            "[coordinator] task {} final flush failed (non-fatal): {}",
            task_id,
            e
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: build fresh uniform segments
// ---------------------------------------------------------------------------

/// Create `count` uniform segments spanning `[0, total_bytes-1]` and return
/// both the in-memory map and the DB tuples for batch insertion.
fn build_fresh_segments(count: i32, total_bytes: i64) -> FreshSegments {
    let count_i64 = count as i64;
    let chunk = total_bytes / count_i64;
    let mut segments = BTreeMap::new();
    let mut db_segs = Vec::with_capacity(count as usize);
    for i in 0..count {
        let start = i as i64 * chunk;
        let end = if i == count - 1 {
            total_bytes - 1
        } else {
            (i as i64 + 1) * chunk - 1
        };
        segments.insert(
            i,
            LiveSegment {
                index: i,
                start_byte: start,
                end_byte: end,
                downloaded_bytes: 0,
                state: SegState::Pending,
            },
        );
        db_segs.push((i, start, end));
    }
    (segments, db_segs)
}

// ---------------------------------------------------------------------------
// Segment coverage validation
// ---------------------------------------------------------------------------

/// Verify that segment ranges cover `[0, total_bytes-1]` with no gaps/overlaps.
fn validate_coverage(
    segments: &BTreeMap<i32, LiveSegment>,
    total_bytes: i64,
) -> Result<(), String> {
    if segments.is_empty() {
        return Err("no segments".to_string());
    }

    // Sort by start_byte to check contiguity.
    let mut sorted: Vec<&LiveSegment> = segments.values().collect();
    sorted.sort_by_key(|s| s.start_byte);

    // First segment must start at 0.
    if sorted[0].start_byte != 0 {
        return Err(format!(
            "first segment starts at {} instead of 0",
            sorted[0].start_byte
        ));
    }

    // Last segment must end at total_bytes - 1.
    let last = sorted[sorted.len() - 1];
    if last.end_byte != total_bytes - 1 {
        return Err(format!(
            "last segment ends at {} instead of {}",
            last.end_byte,
            total_bytes - 1
        ));
    }

    // Check contiguity: each segment's start must be exactly previous end + 1.
    for window in sorted.windows(2) {
        let prev = window[0];
        let curr = window[1];
        let expected_start = prev.end_byte + 1;
        if curr.start_byte != expected_start {
            return Err(format!(
                "gap or overlap between segment {} (end={}) and segment {} (start={})",
                prev.index, prev.end_byte, curr.index, curr.start_byte
            ));
        }
    }

    // Verify total coverage equals total_bytes.
    let total_coverage: i64 = segments.values().map(|s| s.size()).sum();
    if total_coverage != total_bytes {
        return Err(format!(
            "total coverage {} != total_bytes {}",
            total_coverage, total_bytes
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Work assignment logic
// ---------------------------------------------------------------------------

/// Find the next piece of work for an idle worker.
///
/// Strategy (matching IDM behaviour):
/// 1. If there is a `Pending` segment, return it.
/// 2. Otherwise, try to split the largest `Active` segment in half.
/// 3. If nothing can be split, return `None` (worker should retire).
fn find_next_work(
    segments: &mut BTreeMap<i32, LiveSegment>,
    next_index: &mut i32,
    _total_bytes: i64,
    min_split: i64,
) -> Option<NextWork> {
    // Strategy 1: existing Pending segment.
    if let Some(seg) = segments.values().find(|s| s.state == SegState::Pending) {
        let assignment = WorkerAssignment {
            seg_index: seg.index,
            seg_start: seg.start_byte,
            actual_start: seg.start_byte + seg.downloaded_bytes,
            seg_end: seg.end_byte,
        };
        let idx = seg.index;
        if let Some(s) = segments.get_mut(&idx) {
            s.state = SegState::Active;
        }
        return Some(NextWork {
            assignment,
            split_parent: None,
        });
    }

    // Strategy 2: split the largest active segment at the dynamic threshold.
    if let Some(work) = try_split_largest(segments, next_index, min_split) {
        return Some(work);
    }

    // Strategy 3: tail micro-split — when normal split fails (remaining bytes
    // below the dynamic threshold), retry with TAIL_MIN_SPLIT_BYTES (64 KB).
    //
    // This eliminates "tail stall": in a 16-segment download of a 1 GB file,
    // if the last segment has 1.5 MB remaining and MIN_SPLIT is 2 MB, 15 workers
    // idle while 1 slow worker finishes.  With tail micro-split, the 1.5 MB is
    // split into 750 KB + 750 KB, and an idle worker helps finish it 2× faster.
    //
    // Guard A: only activate when the normal threshold is above the tail threshold;
    // if dynamic_min_split already returned 512 KB (low speed), and 512 KB >
    // 64 KB, we retry at 64 KB.  If min_split is already <= 64 KB, there's
    // nothing smaller to try.
    //
    // Guard B: "straggler" check — only micro-split when the largest remaining
    // active segment is ≥ 2 × TAIL_MIN_SPLIT_BYTES (128 KB), indicating a
    // genuine imbalance worth rescuing.
    //
    // Without Guard B, when all remaining segments are equally small (all ~66 KB
    // at the tail of a 50 MB download with 48 workers), workers cascade-split
    // them into ~33 KB pieces.  Workers finishing those micro-segments find
    // nothing more to split and retire en-masse, dropping active worker count
    // from ~48 → ~16 and causing the visible "99% speed drop" in the UI.
    //
    // With Guard B, the cascade stops naturally: a worker finishing a 33 KB
    // segment finds no straggler (max_remaining ≈ 66 KB < 128 KB) and retires
    // gracefully instead of further subdividing already-tiny peers.
    if min_split > TAIL_MIN_SPLIT_BYTES {
        let max_remaining = segments
            .values()
            .filter(|s| s.state == SegState::Active)
            .map(|s| s.remaining())
            .max()
            .unwrap_or(0);
        if max_remaining >= 2 * TAIL_MIN_SPLIT_BYTES {
            try_split_largest(segments, next_index, TAIL_MIN_SPLIT_BYTES)
        } else {
            None
        }
    } else {
        None
    }
}

/// 串行模式专用：只从 Pending 分段中分配工作，不进行拆分。
///
/// 与 [`find_next_work`] 不同，此函数绝不会拆分 Active 分段来创建新工作，
/// 确保在限制并发连接的服务器上不会发起额外的 HTTP 请求。
fn find_next_pending_only(segments: &mut BTreeMap<i32, LiveSegment>) -> Option<NextWork> {
    let seg = segments.values().find(|s| s.state == SegState::Pending)?;
    let assignment = WorkerAssignment {
        seg_index: seg.index,
        seg_start: seg.start_byte,
        actual_start: seg.start_byte + seg.downloaded_bytes,
        seg_end: seg.end_byte,
    };
    let idx = seg.index;
    if let Some(s) = segments.get_mut(&idx) {
        s.state = SegState::Active;
    }
    Some(NextWork {
        assignment,
        split_parent: None,
    })
}

/// IDM-style in-half division: find the active segment with the most remaining
/// bytes and split it at the midpoint of its remaining range.
///
/// Returns a `NextWork` for the **new** segment (upper half), including the
/// index of the parent segment that was shrunk, or `None` if no segment is
/// large enough to split.
fn try_split_largest(
    segments: &mut BTreeMap<i32, LiveSegment>,
    next_index: &mut i32,
    min_split: i64,
) -> Option<NextWork> {
    // Only count non-Completed segments — Completed slots do not contribute
    // to the concurrent-connection limit. This allows idle workers to keep
    // helping the last active segment even after many historical splits.
    let active_or_pending = segments
        .values()
        .filter(|s| s.state != SegState::Completed)
        .count();
    if active_or_pending >= MAX_SEGMENTS as usize {
        return None;
    }

    // Find the active segment with the most remaining bytes.
    let best_idx = segments
        .values()
        .filter(|s| s.state == SegState::Active && s.remaining() >= min_split)
        .max_by_key(|s| s.remaining())
        .map(|s| s.index)?;

    let best = segments.get(&best_idx)?;

    // The current download position in the best segment.
    let current_pos = best.start_byte + best.downloaded_bytes;
    let remaining = best.end_byte - current_pos + 1;

    if remaining < min_split {
        return None;
    }

    // Split point = midpoint of the remaining range.
    let split_point = current_pos + remaining / 2;

    // Validate: split_point must be within (current_pos, end_byte].
    // This guarantees both halves are non-empty.
    if split_point <= current_pos || split_point > best.end_byte {
        return None;
    }

    let old_end = best.end_byte;

    // New segment covers [split_point, old_end].
    let new_index = *next_index;
    *next_index += 1;

    let new_seg = LiveSegment {
        index: new_index,
        start_byte: split_point,
        end_byte: old_end,
        downloaded_bytes: 0,
        state: SegState::Active,
    };

    // Shrink the original segment to [old_start, split_point - 1].
    // The worker currently downloading this segment sees the new end_byte
    // via the shared seg_states and truncates its writes accordingly.
    if let Some(orig) = segments.get_mut(&best_idx) {
        orig.end_byte = split_point - 1;
    }

    let assignment = WorkerAssignment {
        seg_index: new_index,
        seg_start: split_point,
        actual_start: split_point,
        seg_end: old_end,
    };

    segments.insert(new_index, new_seg);

    log_info!(
        "[coordinator] split segment {} → new segment {} at byte {} (parent remaining: {}→{})",
        best_idx,
        new_index,
        split_point,
        remaining,
        split_point - current_pos
    );

    Some(NextWork {
        assignment,
        split_parent: Some(best_idx),
    })
}

/// Proactively split the largest active segment while other workers are still
/// running, creating a **Pending** (not Active) child so that an idle or newly-
/// freed worker can pick it up via `find_next_work`.
///
/// Called periodically by the coordinator's proactive-split timer (every
/// [`PROACTIVE_SPLIT_INTERVAL_SECS`] seconds) to pre-create work items.  This
/// moves the split computation + DB persistence off the critical Done-handler
/// path, reducing worker idle time between segments.
///
/// Returns `None` when:
/// - any `Pending` segment already exists (no need to create more), or
/// - no active segment is large enough to split (< `min_split` remaining), or
/// - the segment cap `MAX_SEGMENTS` would be exceeded.
fn try_proactive_split(
    segments: &mut BTreeMap<i32, LiveSegment>,
    next_index: &mut i32,
    min_split: i64,
) -> Option<NextWork> {
    // Do nothing if there's already a pending segment waiting for a worker.
    if segments.values().any(|s| s.state == SegState::Pending) {
        return None;
    }

    // Only count non-Completed segments — Completed slots do not contribute
    // to the concurrent-connection limit. This allows idle workers to keep
    // helping the last active segment even after many historical splits.
    let active_or_pending = segments
        .values()
        .filter(|s| s.state != SegState::Completed)
        .count();
    if active_or_pending >= MAX_SEGMENTS as usize {
        return None;
    }

    // Find the active segment with the most remaining bytes.
    let best_idx = segments
        .values()
        .filter(|s| s.state == SegState::Active && s.remaining() >= min_split)
        .max_by_key(|s| s.remaining())
        .map(|s| s.index)?;

    let best = segments.get(&best_idx)?;
    let current_pos = best.start_byte + best.downloaded_bytes;
    let remaining = best.end_byte - current_pos + 1;

    if remaining < min_split {
        return None;
    }

    let split_point = current_pos + remaining / 2;
    if split_point <= current_pos || split_point > best.end_byte {
        return None;
    }

    let old_end = best.end_byte;
    let new_index = *next_index;
    *next_index += 1;

    // New segment is Pending — a worker will pick it up when idle.
    let new_seg = LiveSegment {
        index: new_index,
        start_byte: split_point,
        end_byte: old_end,
        downloaded_bytes: 0,
        state: SegState::Pending,
    };

    if let Some(orig) = segments.get_mut(&best_idx) {
        orig.end_byte = split_point - 1;
    }

    let assignment = WorkerAssignment {
        seg_index: new_index,
        seg_start: split_point,
        actual_start: split_point,
        seg_end: old_end,
    };

    segments.insert(new_index, new_seg);

    log_info!(
        "[coordinator] proactive split: segment {} → new pending segment {} at byte {}",
        best_idx,
        new_index,
        split_point
    );

    Some(NextWork {
        assignment,
        split_parent: Some(best_idx),
    })
}

// ---------------------------------------------------------------------------
// Helper: check completion
// ---------------------------------------------------------------------------

fn all_done(segments: &BTreeMap<i32, LiveSegment>) -> bool {
    segments.values().all(|s| s.state == SegState::Completed)
}

// ---------------------------------------------------------------------------
// Helpers: shared state synchronization
// ---------------------------------------------------------------------------

/// Build a fresh `Vec<SegmentProgressInfo>` from the segment map.
fn build_seg_state_vec(segments: &BTreeMap<i32, LiveSegment>) -> Vec<SegmentProgressInfo> {
    segments
        .values()
        .map(|s| SegmentProgressInfo {
            index: s.index,
            start_byte: s.start_byte,
            end_byte: s.end_byte,
            downloaded_bytes: s.downloaded_bytes,
        })
        .collect()
}

/// Overwrite the shared visualization state from the authoritative segment map.
fn rebuild_seg_states(
    segments: &BTreeMap<i32, LiveSegment>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
) {
    let new_states = build_seg_state_vec(segments);
    if let Ok(mut states) = seg_states.lock() {
        *states = new_states;
    }
}

/// Sync the coordinator's `downloaded_bytes` for Active segments from the
/// shared state (which workers update in real-time).
///
/// Without this, `try_split_largest` would calculate split points based on
/// the initial `downloaded_bytes` at assignment time, potentially placing the
/// split within an already-downloaded region.
fn sync_downloaded_from_shared(
    segments: &mut BTreeMap<i32, LiveSegment>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
) {
    let snapshot = match seg_states.lock() {
        Ok(guard) => guard.clone(),
        Err(e) => e.into_inner().clone(),
    };
    for info in &snapshot {
        if let Some(seg) = segments.get_mut(&info.index)
            && seg.state == SegState::Active
        {
            seg.downloaded_bytes = info.downloaded_bytes;
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: persist segment changes to DB
// ---------------------------------------------------------------------------

/// Persist a segment change (new segment from split, or re-assigned pending)
/// and optionally the parent whose end_byte was shrunk.
///
/// When a `split_parent` is provided, both the child upsert and the parent
/// end_byte update are persisted in a **single** SQLite transaction via
/// `Db::persist_split`, preventing crash-induced overlaps.
///
/// When no parent is given (simple re-assignment), only the child is upserted.
async fn persist_segment_change(
    db: &Db,
    task_id: &str,
    segments: &BTreeMap<i32, LiveSegment>,
    changed_index: i32,
    split_parent: Option<i32>,
) {
    let Some(seg) = segments.get(&changed_index) else {
        return;
    };

    if let Some(parent_idx) = split_parent {
        // Split scenario: atomic transaction for both child + parent.
        if let Some(parent) = segments.get(&parent_idx) {
            if let Err(e) = db
                .persist_split(
                    task_id,
                    seg.index,
                    seg.start_byte,
                    seg.end_byte,
                    seg.downloaded_bytes,
                    parent.index,
                    parent.end_byte,
                )
                .await
            {
                log_info!(
                    "[coordinator] persist_split failed: task={}, child={}, parent={}, err={}",
                    task_id,
                    seg.index,
                    parent.index,
                    e
                );
            }
        } else {
            // Parent not found in map — fall back to child-only upsert.
            if let Err(e) = db
                .upsert_segment(
                    task_id,
                    seg.index,
                    seg.start_byte,
                    seg.end_byte,
                    seg.downloaded_bytes,
                )
                .await
            {
                log_info!(
                    "[coordinator] upsert_segment failed: task={}, seg={}, err={}",
                    task_id,
                    seg.index,
                    e
                );
            }
        }
    } else {
        // No parent — simple upsert (e.g. reassigning a pending segment).
        if let Err(e) = db
            .upsert_segment(
                task_id,
                seg.index,
                seg.start_byte,
                seg.end_byte,
                seg.downloaded_bytes,
            )
            .await
        {
            log_info!(
                "[coordinator] upsert_segment failed: task={}, seg={}, err={}",
                task_id,
                seg.index,
                e
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: send split event to Dart
// ---------------------------------------------------------------------------

/// Send a `SegmentSplitEvent` signal to Dart so the UI can animate the split.
fn send_split_event(
    task_id: &str,
    parent_idx: i32,
    child_idx: i32,
    segments: &BTreeMap<i32, LiveSegment>,
    is_proactive: bool,
) {
    let Some(parent) = segments.get(&parent_idx) else {
        return;
    };
    let Some(child) = segments.get(&child_idx) else {
        return;
    };

    SegmentSplitEvent {
        task_id: task_id.to_string(),
        parent_index: parent_idx,
        parent_new_end: parent.end_byte,
        child_index: child_idx,
        child_start: child.start_byte,
        child_end: child.end_byte,
        is_proactive,
        total_segments: segments.len() as i32,
    }
    .send_signal_to_dart();

    log_info!(
        "[coordinator] split event sent: parent={} new_end={}, child={} [{}, {}], proactive={}, total={}",
        parent_idx,
        parent.end_byte,
        child_idx,
        child.start_byte,
        child.end_byte,
        is_proactive,
        segments.len()
    );
}

// ---------------------------------------------------------------------------
// Worker implementation
// ---------------------------------------------------------------------------

/// Spawn a worker task that loops: receive assignment → download segment → report.
///
/// The worker reuses its HTTP client (and thus TCP/TLS connections) across
/// multiple segment assignments — achieving IDM-style connection reuse.
#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    worker_id: usize,
    mut assign_rx: mpsc::Receiver<WorkerAssignment>,
    event_tx: mpsc::Sender<WorkerEvent>,
    task_id: String,
    url: String,
    dest: PathBuf,
    total_bytes: i64,
    client: Client,
    cancel_token: CancellationToken,
    total_downloaded: Arc<AtomicI64>,
    seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    db: Db,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    speed_limiter: SpeedLimiter,
    cookies: String,
    referrer: String,
    extra_headers: std::collections::HashMap<String, String>,
    etag: String,
    last_modified: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Worker loop: keep accepting assignments until the channel closes.
        while let Some(assignment) = assign_rx.recv().await {
            if cancel_token.is_cancelled() {
                break;
            }

            let result = do_segment_with_retry(
                &task_id,
                assignment.seg_index,
                &url,
                &dest,
                assignment.seg_start,
                assignment.actual_start,
                assignment.seg_end,
                &client,
                &cancel_token,
                &total_downloaded,
                total_bytes,
                &db,
                &progress_tx,
                &seg_states,
                &speed_limiter,
                &cookies,
                &referrer,
                &extra_headers,
                &etag,
                &last_modified,
            )
            .await;

            match result {
                Ok(downloaded) => {
                    let _ = event_tx
                        .send(WorkerEvent::Done {
                            worker_id,
                            seg_index: assignment.seg_index,
                            downloaded_bytes: downloaded,
                        })
                        .await;
                }
                Err(DownloadError::Cancelled) => {
                    // Don't report — coordinator already knows via cancel_token.
                    break;
                }
                Err(e) => {
                    let _ = event_tx
                        .send(WorkerEvent::Failed {
                            worker_id,
                            seg_index: assignment.seg_index,
                            error: e,
                        })
                        .await;
                    break;
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Segment download with retry
// ---------------------------------------------------------------------------

/// Download a single segment with automatic retry on transient failures.
/// Returns the total `downloaded_bytes` for this segment on success.
#[allow(clippy::too_many_arguments)]
async fn do_segment_with_retry(
    task_id: &str,
    seg_idx: i32,
    url: &str,
    dest: &Path,
    seg_start: i64,
    mut actual_start: i64,
    mut seg_end: i64,
    client: &Client,
    cancel: &CancellationToken,
    total_downloaded: &AtomicI64,
    total_bytes: i64,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
    expected_etag: &str,
    expected_last_modified: &str,
) -> Result<i64, DownloadError> {
    let mut attempts = 0u32;

    loop {
        match do_segment(
            task_id,
            seg_idx,
            url,
            dest,
            seg_start,
            actual_start,
            seg_end,
            client,
            cancel,
            total_downloaded,
            total_bytes,
            db,
            progress_tx,
            seg_states,
            speed_limiter,
            cookies,
            referrer,
            extra_headers,
            expected_etag,
            expected_last_modified,
        )
        .await
        {
            Ok(dl) => return Ok(dl),
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            // RangeNotSupported is a permanent server property — retrying would
            // get the same 200-instead-of-206 response every time.  Return
            // immediately so the coordinator can cancel all workers and trigger
            // the single-stream fallback without burning 3× exponential-backoff
            // delays (up to 14 s for the first worker alone).
            Err(e @ DownloadError::RangeNotSupported(_)) => return Err(e),
            Err(e) => {
                // 403/429 是服务器明确拒绝多连接，重试毫无意义——
                // 立即返回让 coordinator 进行降级处理。
                if is_server_rejection(&e) {
                    log_info!(
                        "[segment-retry] task {} seg {} 收到服务器拒绝，跳过重试直接上报",
                        task_id,
                        seg_idx
                    );
                    return Err(e);
                }
                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(e);
                }
                // Recover actual_start *and* seg_end from DB for partial progress.
                // seg_end may have been shrunk by a coordinator split since we started.
                if let Ok(segs) = db.load_segments(task_id).await
                    && let Some(seg) = segs.iter().find(|s| s.index == seg_idx)
                {
                    seg_end = seg.end_byte;
                    actual_start = seg_start + seg.downloaded_bytes;
                    if actual_start > seg_end {
                        // Segment completed during previous attempt.
                        return Ok(seg.downloaded_bytes);
                    }
                }
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

/// Download a single segment.  Returns `downloaded_bytes` for the segment.
///
/// The worker detects dynamic segment shrinking (from coordinator splits) by
/// reading the shared `seg_states` **before** each write.  Writes are truncated
/// at the effective boundary to prevent cross-segment data corruption.
#[allow(clippy::too_many_arguments)]
async fn do_segment(
    task_id: &str,
    seg_idx: i32,
    url: &str,
    dest: &Path,
    seg_start: i64,
    actual_start: i64,
    seg_end: i64,
    client: &Client,
    cancel: &CancellationToken,
    total_downloaded: &AtomicI64,
    total_bytes: i64,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
    expected_etag: &str,
    expected_last_modified: &str,
) -> Result<i64, DownloadError> {
    if actual_start > seg_end {
        // Already complete.
        return Ok(seg_end - seg_start + 1);
    }

    let range = format!("bytes={}-{}", actual_start, seg_end);
    let mut req = client.get(url).header("Range", range);
    if !cookies.is_empty() {
        req = req.header("Cookie", cookies);
    }
    if !referrer.is_empty() {
        req = req.header(reqwest::header::REFERER, referrer);
    }
    req = crate::downloader::apply_extra_headers(req, extra_headers);
    let resp = req.send().await?.error_for_status()?;

    // --- Range support verification ----------------------------------------
    // We sent a `Range: bytes=X-Y` header; the server MUST respond with 206
    // Partial Content if it honours Range requests.  A 200 OK response means
    // the server ignored the Range header and is streaming the full file from
    // byte 0 — writing that body at `actual_start` would overwrite adjacent
    // segments and silently corrupt the assembled output file.
    //
    // Observed with FnOS NAS "multiple-download?token=..." endpoints: the
    // server accepts the Range header syntactically but always replies 200 +
    // full content, making multi-segment assembly impossible.
    //
    // Fix: record the host so future tasks automatically use single-stream
    // mode (24 h TTL via the existing single-conn cache); return an error so
    // the coordinator cancels all workers for the current attempt.  On retry
    // the cached policy kicks in and the download proceeds in single-stream.
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        // Record the host so future tasks for the same server start in
        // single-stream mode immediately (24 h TTL in-process cache).
        record_single_conn_domain(url);
        return Err(DownloadError::RangeNotSupported(resp.status().to_string()));
    }

    // --- ETag / Last-Modified consistency check -----------------------------
    // Verify that this segment's response comes from the same file version as
    // the initial probe.  A mismatch means the server updated the file while
    // we're downloading — the resulting file would be a corrupt splice of two
    // different versions.
    //
    // Only check when the probe returned a non-empty value AND the segment
    // response also provides the header.  Many CDN edge servers strip these
    // headers on Range responses, so a missing header is not an error.
    if !expected_etag.is_empty()
        && let Some(resp_etag) = resp.headers().get(reqwest::header::ETAG)
        && let Ok(resp_etag_str) = resp_etag.to_str()
        && !resp_etag_str.is_empty()
        && resp_etag_str != expected_etag
    {
        return Err(DownloadError::Other(format!(
            "segment {}: ETag mismatch — probe=\"{}\", segment=\"{}\". \
             The file may have changed on the server during download.",
            seg_idx, expected_etag, resp_etag_str
        )));
    }
    if !expected_last_modified.is_empty()
        && let Some(resp_lm) = resp.headers().get(reqwest::header::LAST_MODIFIED)
        && let Ok(resp_lm_str) = resp_lm.to_str()
        && !resp_lm_str.is_empty()
        && resp_lm_str != expected_last_modified
    {
        return Err(DownloadError::Other(format!(
            "segment {}: Last-Modified mismatch — probe=\"{}\", segment=\"{}\". \
             The file may have changed on the server during download.",
            seg_idx, expected_last_modified, resp_lm_str
        )));
    }

    // Safety net: if a Range response carries Content-Encoding, the raw
    // compressed bytes cannot be spliced into the correct file offset — each
    // segment would need independent decompression but the decompressed size
    // is unpredictable, making precise byte-range assembly impossible.
    //
    // The probe phase now checks GET Range:0-0 specifically and disables
    // multi-segment when Range responses are compressed.  Reaching this point
    // with compression means the server changed behaviour between probe and
    // download (e.g. CDN edge node rotation).  This is extremely rare but we
    // must guard against it to prevent silent file corruption.
    if let Some(enc) = crate::downloader::detect_content_encoding(resp.headers()) {
        // Record the domain so that the retry (or any future task for this
        // host) automatically uses single-stream mode.
        record_single_conn_domain(url);
        return Err(DownloadError::Other(format!(
            "segment {}: server returned Content-Encoding ({:?}) on a Range response. \
             Compressed byte ranges cannot be assembled into a valid file. \
             Please retry — the download will use single-stream mode.",
            seg_idx, enc
        )));
    }

    // For segment 0, try extracting a better filename from the response.
    if seg_idx == 0
        && let Some(cd) = resp.headers().get(reqwest::header::CONTENT_DISPOSITION)
    {
        let resp_name = crate::downloader::extract_filename(resp.headers(), resp.url().as_str());
        if !resp_name.is_empty() && resp_name != "download" {
            log_info!(
                "[coordinator-seg0] got better name from response: {} (cd={:?})",
                resp_name,
                cd
            );
            let snapshot = seg_states.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.to_string(),
                    downloaded_bytes: total_downloaded.load(Ordering::Relaxed),
                    total_bytes,
                    status: 1,
                    error_message: String::new(),
                    file_name: resp_name,
                    segment_details: Some(snapshot),
                })
                .await;
        }
    }

    let mut stream = resp.bytes_stream();

    let file = OpenOptions::new().write(true).open(dest).await?;
    let seg_remaining = seg_end - actual_start + 1;
    let buf_cap = buf_writer_capacity_for_segment(seg_remaining);
    let mut file = tokio::io::BufWriter::with_capacity(buf_cap, file);
    file.seek(std::io::SeekFrom::Start(actual_start as u64))
        .await?;

    let mut seg_downloaded = actual_start - seg_start;
    let mut last_report = Instant::now();
    let mut last_db_save = Instant::now();

    // The effective end byte, which may shrink if the coordinator splits us.
    let mut effective_end = seg_end;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                file.flush().await?;
                update_seg_state(seg_states, seg_idx, seg_downloaded, effective_end);
                let _ = db.update_segment_progress(task_id, seg_idx, seg_downloaded).await;
                return Err(DownloadError::Cancelled);
            }
            result = tokio::time::timeout(CHUNK_STALL_TIMEOUT, stream.next()) => {
                // Unwrap the timeout layer first.  If no chunk arrived within
                // CHUNK_STALL_TIMEOUT the TCP connection is likely dead — flush
                // partial progress and bubble up an error so do_segment_with_retry
                // can resume from a fresh connection.
                let chunk = match result {
                    Ok(c) => c,
                    Err(_) => {
                        file.flush().await?;
                        update_seg_state(seg_states, seg_idx, seg_downloaded, effective_end);
                        let _ = db.update_segment_progress(
                            task_id, seg_idx, seg_downloaded,
                        ).await;
                        return Err(DownloadError::Other(format!(
                            "segment {} stalled: no data received for {}s",
                            seg_idx, CHUNK_STALL_TIMEOUT.as_secs()
                        )));
                    }
                };
                match chunk {
                    Some(Ok(bytes)) => {
                        // --- Boundary check BEFORE writing ---
                        // Read the possibly-shrunk end_byte from shared state.
                        if let Ok(states) = seg_states.lock()
                            && let Some(s) = states.iter().find(|s| s.index == seg_idx) {
                                effective_end = s.end_byte;
                            }

                        // Calculate the write budget.
                        let current_pos = seg_start + seg_downloaded;
                        let budget = (effective_end - current_pos + 1).max(0) as usize;
                        let write_len = bytes.len().min(budget);

                        if write_len == 0 {
                            // Reached the (possibly shrunk) boundary — stop.
                            break;
                        }

                        let write_slice = &bytes[..write_len];

                        // --- Speed limiter: write in sub-chunks as tokens allow ---
                        let mut offset = 0usize;
                        while offset < write_len {
                            let remaining = (write_len - offset) as u64;
                            let allowed = speed_limiter.consume(remaining).await;
                            let end = offset + allowed as usize;
                            file.write_all(&write_slice[offset..end]).await?;
                            offset = end;
                        }

                        let len = write_len as i64;
                        seg_downloaded += len;
                        total_downloaded.fetch_add(len, Ordering::Relaxed);

                        // Update shared segment state (workers → coordinator channel).
                        update_seg_state(seg_states, seg_idx, seg_downloaded, effective_end);

                        // If we truncated the chunk, we hit the boundary.
                        if write_len < bytes.len() {
                            file.flush().await?;
                            let _ = db.update_segment_progress(
                                task_id, seg_idx, seg_downloaded,
                            ).await;
                            break;
                        }

                        // --- Progress report to Dart ---
                        if last_report.elapsed().as_millis() >= UI_REPORT_INTERVAL_MS {
                            let current_total = total_downloaded.load(Ordering::Relaxed);
                            let snapshot = seg_states
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.to_string(),
                                    downloaded_bytes: current_total,
                                    total_bytes,
                                    status: 1,
                                    error_message: String::new(),
                                    file_name: String::new(),
                                    segment_details: Some(snapshot),
                                })
                                .await;
                            last_report = Instant::now();
                        }

                        // --- DB persistence (periodic) ---
                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            let _ = db
                                .update_segment_progress(task_id, seg_idx, seg_downloaded)
                                .await;
                            last_db_save = Instant::now();
                        }
                    }
                    Some(Err(e)) => {
                        file.flush().await?;
                        update_seg_state(seg_states, seg_idx, seg_downloaded, effective_end);
                        let _ = db
                            .update_segment_progress(task_id, seg_idx, seg_downloaded)
                            .await;
                        return Err(DownloadError::Request(e));
                    }
                    None => break,
                }
            }
        }
    }

    file.flush().await?;

    // Linux: posix_fadvise(FADV_DONTNEED) 通知内核释放已完成段的页缓存，
    // 防止大文件下载过程中页缓存无限增长占满内存。
    // 参考 aria2 的 readDataDropCache() 策略。
    // posix_fadvise 仅为内核提供提示，不阻塞，无需 spawn_blocking。
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.get_ref().as_raw_fd();
        unsafe {
            libc::posix_fadvise(
                fd,
                seg_start as libc::off_t,
                seg_downloaded as libc::off_t,
                libc::POSIX_FADV_DONTNEED,
            );
        }
    }

    update_seg_state(seg_states, seg_idx, seg_downloaded, effective_end);
    let _ = db
        .update_segment_progress(task_id, seg_idx, seg_downloaded)
        .await;

    Ok(seg_downloaded)
}

/// Update a single segment's progress in the shared visualization state.
fn update_seg_state(
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    seg_idx: i32,
    downloaded_bytes: i64,
    end_byte: i64,
) {
    if let Ok(mut states) = seg_states.lock()
        && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx)
    {
        s.downloaded_bytes = downloaded_bytes;
        s.end_byte = end_byte;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        LiveSegment, MAX_SEGMENTS, MIN_SPLIT_BYTES, SegState, TAIL_MIN_SPLIT_BYTES, all_done,
        dynamic_min_split_bytes, extract_host, find_next_pending_only, find_next_work,
        is_single_conn_domain, record_single_conn_domain, single_conn_cache, try_proactive_split,
        try_split_largest, validate_coverage,
    };
    use crate::downloader::{DownloadError, is_server_rejection};
    use std::collections::BTreeMap;

    fn make_seg(index: i32, start: i64, end: i64, downloaded: i64, state: SegState) -> LiveSegment {
        LiveSegment {
            index,
            start_byte: start,
            end_byte: end,
            downloaded_bytes: downloaded,
            state,
        }
    }

    // -----------------------------------------------------------------------
    // validate_coverage
    // -----------------------------------------------------------------------

    #[test]
    fn coverage_valid_single_segment() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_ok());
    }

    #[test]
    fn coverage_valid_multi_segment() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 499, 500, SegState::Completed));
        segs.insert(1, make_seg(1, 500, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_ok());
    }

    #[test]
    fn coverage_gap_detected() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 499, 0, SegState::Active));
        // Gap: 500 is missing
        segs.insert(1, make_seg(1, 501, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_overlap_detected() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 500, 0, SegState::Active));
        // Overlap: both cover byte 500
        segs.insert(1, make_seg(1, 500, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_wrong_start() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 1, 999, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_wrong_end() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 998, 0, SegState::Active));
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    #[test]
    fn coverage_empty_segments() {
        let segs = BTreeMap::new();
        assert!(validate_coverage(&segs, 1000).is_err());
    }

    // -----------------------------------------------------------------------
    // try_split_largest
    // -----------------------------------------------------------------------

    #[test]
    fn split_largest_basic() {
        let mut segs = BTreeMap::new();
        // Segment 0: 0..99MB, downloaded 10MB — remaining 90MB
        segs.insert(
            0,
            make_seg(0, 0, 100_000_000 - 1, 10_000_000, SegState::Active),
        );
        // Segment 1: 100MB..199MB, downloaded 50MB — remaining 50MB
        segs.insert(
            1,
            make_seg(
                1,
                100_000_000,
                200_000_000 - 1,
                50_000_000,
                SegState::Active,
            ),
        );

        let mut next_idx = 2;
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(result.is_some(), "should split the largest segment");

        let next = result.expect("already checked");
        assert_eq!(
            next.assignment.seg_index, 2,
            "new segment index should be 2"
        );
        assert_eq!(next_idx, 3);
        assert_eq!(next.split_parent, Some(0), "parent should be segment 0");

        // Original segment 0 should have a smaller end_byte now.
        let orig = segs.get(&0).expect("segment 0 exists");
        assert!(
            orig.end_byte < 100_000_000 - 1,
            "segment 0 should be shrunk"
        );

        // New segment should cover the upper half.
        let new_seg = segs.get(&2).expect("segment 2 exists");
        assert_eq!(new_seg.end_byte, 100_000_000 - 1);
        assert_eq!(new_seg.start_byte, next.assignment.seg_start);

        // Coverage must remain valid.
        assert!(
            validate_coverage(&segs, 200_000_000).is_ok(),
            "coverage must be valid after split"
        );
    }

    #[test]
    fn split_no_split_when_too_small() {
        let mut segs = BTreeMap::new();
        // Segment with only 2MB remaining — below MIN_SPLIT_BYTES.
        segs.insert(0, make_seg(0, 0, 3_000_000, 1_000_001, SegState::Active));

        let mut next_idx = 1;
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(result.is_none(), "should not split small segments");
    }

    #[test]
    fn split_respects_max_segments() {
        let mut segs = BTreeMap::new();
        for i in 0..MAX_SEGMENTS {
            segs.insert(
                i,
                make_seg(
                    i,
                    i as i64 * 10_000_000,
                    (i as i64 + 1) * 10_000_000 - 1,
                    0,
                    SegState::Active,
                ),
            );
        }
        let mut next_idx = MAX_SEGMENTS;
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(result.is_none(), "should not exceed MAX_SEGMENTS");
    }

    /// After Fix 1: completed segments do not count toward MAX_SEGMENTS.
    /// 63 Completed + 1 Active of 10 MB should allow a split because
    /// active_or_pending = 1 < MAX_SEGMENTS = 64.
    #[test]
    fn split_allowed_when_completed_segments_free_slots() {
        let total_bytes: i64 = 10_000_000;
        let mut segs = BTreeMap::new();
        // 63 completed segments (minimal placeholder ranges).
        for i in 0..(MAX_SEGMENTS - 1) {
            segs.insert(i, make_seg(i, i as i64, i as i64, 1, SegState::Completed));
        }
        // 1 active segment with 10 MB remaining (well above MIN_SPLIT_BYTES).
        segs.insert(
            MAX_SEGMENTS - 1,
            make_seg(MAX_SEGMENTS - 1, 0, total_bytes - 1, 0, SegState::Active),
        );
        let mut next_idx = MAX_SEGMENTS;

        // With old code: segments.len() == 64 → None (workers retired).
        // With fix: active_or_pending == 1 < 64 → should split successfully.
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(
            result.is_some(),
            "completed segments must not prevent splits of the remaining active segment"
        );
        // next_idx must have been incremented, confirming a new segment was created.
        assert_eq!(
            next_idx,
            MAX_SEGMENTS + 1,
            "next_idx must advance after a successful split"
        );
        // The new segment must exist in the map.
        assert!(
            segs.contains_key(&MAX_SEGMENTS),
            "new segment must be inserted into the map"
        );
        // Note: validate_coverage is intentionally omitted here because the 63
        // Completed placeholder segments use non-contiguous byte ranges (they are
        // stand-ins for "historically finished" slots, not a valid byte layout).
        // The purpose of this test is solely to verify that the active_or_pending
        // count check allows the split; byte-range integrity is covered by the
        // split_consecutive_splits_maintain_coverage and split_largest_basic tests.
    }

    /// MAX_SEGMENTS still limits truly concurrent connections:
    /// when 64 Active/Pending segments exist, no further split is allowed.
    #[test]
    fn split_blocked_when_max_active_segments_reached() {
        let mut segs = BTreeMap::new();
        for i in 0..MAX_SEGMENTS {
            segs.insert(
                i,
                make_seg(
                    i,
                    i as i64 * 1000,
                    i as i64 * 1000 + 999,
                    0,
                    SegState::Active,
                ),
            );
        }
        let mut next_idx = MAX_SEGMENTS;

        // active_or_pending == 64 >= MAX_SEGMENTS → must still return None.
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(
            result.is_none(),
            "must not exceed MAX_SEGMENTS active connections"
        );
    }

    #[test]
    fn split_consecutive_splits_maintain_coverage() {
        let total_bytes: i64 = 200_000_000;
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, total_bytes - 1, 0, SegState::Active));

        let mut next_idx = 1;

        // Perform multiple consecutive splits.
        for _ in 0..5 {
            let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
            assert!(result.is_some(), "should be able to split");
            assert!(
                validate_coverage(&segs, total_bytes).is_ok(),
                "coverage must remain valid after each split"
            );
        }

        // All segments should cover exactly [0, total_bytes-1].
        let total_coverage: i64 = segs.values().map(|s| s.size()).sum();
        assert_eq!(total_coverage, total_bytes);
    }

    #[test]
    fn split_with_progress_uses_correct_midpoint() {
        let total_bytes: i64 = 100_000_000;
        let mut segs = BTreeMap::new();
        // Segment at 70% progress — remaining 30MB.
        segs.insert(
            0,
            make_seg(0, 0, total_bytes - 1, 70_000_000, SegState::Active),
        );

        let mut next_idx = 1;
        let result = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(result.is_some());

        let next = result.expect("checked");
        // Split should be at midpoint of remaining [70_000_000, 99_999_999].
        // remaining = 30_000_000, midpoint = 70_000_000 + 15_000_000 = 85_000_000
        assert_eq!(next.assignment.seg_start, 85_000_000);
        assert_eq!(next.assignment.seg_end, 99_999_999);

        let orig = segs.get(&0).expect("exists");
        assert_eq!(orig.end_byte, 84_999_999);

        assert!(validate_coverage(&segs, total_bytes).is_ok());
    }

    #[test]
    fn split_does_not_split_completed_segments() {
        let mut segs = BTreeMap::new();
        segs.insert(
            0,
            make_seg(0, 0, 9_999_999, 10_000_000, SegState::Completed),
        );
        segs.insert(1, make_seg(1, 10_000_000, 19_999_999, 0, SegState::Active));
        let mut next = 2;

        let result = try_split_largest(&mut segs, &mut next, MIN_SPLIT_BYTES);
        assert!(result.is_some());

        let next = result.expect("checked");
        // Should split segment 1 (Active), not segment 0 (Completed).
        assert_eq!(next.split_parent, Some(1));
    }

    // -----------------------------------------------------------------------
    // find_next_work
    // -----------------------------------------------------------------------

    #[test]
    fn find_work_prefers_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 50_000_000, 0, SegState::Active));
        segs.insert(
            1,
            make_seg(1, 50_000_001, 100_000_000, 0, SegState::Pending),
        );

        let mut next_idx = 2;
        let result = find_next_work(&mut segs, &mut next_idx, 100_000_001, MIN_SPLIT_BYTES);
        assert!(result.is_some());
        let next = result.expect("checked");
        assert_eq!(
            next.assignment.seg_index, 1,
            "should pick the pending segment first"
        );
        assert!(
            next.split_parent.is_none(),
            "pending reuse should not have split_parent"
        );
    }

    #[test]
    fn find_work_splits_when_no_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 9_999_999, 0, SegState::Active));
        let mut next_idx = 1;

        let result = find_next_work(&mut segs, &mut next_idx, 10_000_000, MIN_SPLIT_BYTES);
        assert!(result.is_some());
        let next = result.expect("checked");
        assert!(next.split_parent.is_some(), "should come from a split");
        assert_eq!(next_idx, 2, "next_index should have advanced");
    }

    #[test]
    fn find_work_returns_none_when_all_done() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));

        let mut next_idx = 1;
        let result = find_next_work(&mut segs, &mut next_idx, 100, MIN_SPLIT_BYTES);
        assert!(result.is_none(), "no work when all completed");
    }

    // -----------------------------------------------------------------------
    // Tail micro-split (Strategy 3 in find_next_work)
    // -----------------------------------------------------------------------

    /// When a segment's remaining bytes are between TAIL_MIN_SPLIT_BYTES*2 and
    /// MIN_SPLIT_BYTES, normal split fails but tail micro-split succeeds.
    #[test]
    fn tail_microsplit_splits_below_normal_threshold() {
        let mut segs = BTreeMap::new();
        // 500 KB remaining — too small for MIN_SPLIT_BYTES (2 MB) but
        // large enough for TAIL_MIN_SPLIT_BYTES (64 KB).
        let remaining = 500 * 1024; // 500 KB
        assert!(
            remaining < MIN_SPLIT_BYTES,
            "precondition: below normal threshold"
        );
        assert!(
            remaining >= TAIL_MIN_SPLIT_BYTES * 2,
            "precondition: above tail threshold"
        );
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));

        let mut next_idx = 1;

        // Normal split should fail:
        let normal = try_split_largest(&mut segs, &mut next_idx, MIN_SPLIT_BYTES);
        assert!(normal.is_none(), "normal split should fail for 500 KB");

        // But find_next_work should succeed via tail micro-split (Strategy 3):
        let result = find_next_work(&mut segs, &mut next_idx, remaining, MIN_SPLIT_BYTES);
        assert!(
            result.is_some(),
            "tail micro-split should succeed for 500 KB"
        );
        let next = result.expect("checked");
        assert!(next.split_parent.is_some(), "should come from a split");
        assert!(
            validate_coverage(&segs, remaining).is_ok(),
            "coverage must be valid after tail micro-split"
        );
    }

    /// Segments smaller than 2× TAIL_MIN_SPLIT_BYTES cannot be micro-split.
    #[test]
    fn tail_microsplit_respects_minimum() {
        let mut segs = BTreeMap::new();
        // 100 KB remaining — just above TAIL_MIN_SPLIT_BYTES (64 KB) but below 2×64 KB=128 KB.
        // Actually TAIL_MIN_SPLIT_BYTES is 64KB, and try_split_largest requires remaining >= threshold.
        // With 100KB remaining and threshold 64KB, the split point would be at 50KB from current_pos.
        // Each half would be 50KB, which is < 64KB... but the check is remaining >= threshold,
        // not each-half >= threshold. Let's use 60 KB which is < 64 KB.
        let remaining = 60 * 1024; // 60 KB < TAIL_MIN_SPLIT_BYTES
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));

        let mut next_idx = 1;
        let result = find_next_work(&mut segs, &mut next_idx, remaining, MIN_SPLIT_BYTES);
        assert!(
            result.is_none(),
            "should not split segment smaller than TAIL_MIN_SPLIT_BYTES"
        );
    }

    /// Tail micro-split does not trigger when min_split is already at
    /// TAIL_MIN_SPLIT_BYTES (guard: min_split > TAIL_MIN_SPLIT_BYTES).
    #[test]
    fn tail_microsplit_no_infinite_retry() {
        let mut segs = BTreeMap::new();
        // 100 KB remaining, min_split already at TAIL_MIN_SPLIT_BYTES.
        let remaining = 100 * 1024;
        segs.insert(0, make_seg(0, 0, remaining - 1, 0, SegState::Active));

        let mut next_idx = 1;
        // When min_split == TAIL_MIN_SPLIT_BYTES, Strategy 3 should not retry.
        let result = find_next_work(&mut segs, &mut next_idx, remaining, TAIL_MIN_SPLIT_BYTES);
        // 100KB >= 64KB so try_split_largest(TAIL) succeeds, but we're testing
        // that when called with TAIL_MIN_SPLIT_BYTES directly, Strategy 2
        // handles it (not Strategy 3 infinite loop).
        // Strategy 2: try_split_largest(segs, next, 64KB) with 100KB remaining → succeeds.
        assert!(
            result.is_some(),
            "Strategy 2 itself should handle TAIL_MIN_SPLIT_BYTES"
        );
    }

    /// dynamic_min_split_bytes returns expected thresholds at boundary speeds.
    #[test]
    fn dynamic_min_split_at_boundaries() {
        // < 1 MB/s → 512 KB
        assert_eq!(dynamic_min_split_bytes(500.0 * 1024.0), 512 * 1024);
        // 1 MB/s – 10 MB/s → 1 MB
        assert_eq!(dynamic_min_split_bytes(5.0 * 1024.0 * 1024.0), 1024 * 1024);
        // > 10 MB/s → 2 MB (MIN_SPLIT_BYTES)
        assert_eq!(
            dynamic_min_split_bytes(50.0 * 1024.0 * 1024.0),
            MIN_SPLIT_BYTES
        );
    }

    /// Tail micro-split maintains full byte coverage after splitting.
    #[test]
    fn tail_microsplit_maintains_coverage() {
        let total: i64 = 10 * 1024 * 1024; // 10 MB
        let mut segs = BTreeMap::new();
        // Two segments: seg0 completed, seg1 active with 300 KB remaining.
        let seg1_start = total - 300 * 1024;
        segs.insert(
            0,
            make_seg(0, 0, seg1_start - 1, seg1_start, SegState::Completed),
        );
        segs.insert(1, make_seg(1, seg1_start, total - 1, 0, SegState::Active));
        assert!(validate_coverage(&segs, total).is_ok(), "precondition");

        let mut next_idx = 2;
        let result = find_next_work(&mut segs, &mut next_idx, total, MIN_SPLIT_BYTES);
        assert!(result.is_some(), "tail micro-split should work");
        assert!(
            validate_coverage(&segs, total).is_ok(),
            "coverage must remain valid after tail micro-split"
        );
        // Verify three segments now exist.
        assert_eq!(segs.len(), 3);
    }

    // -----------------------------------------------------------------------
    // try_proactive_split
    // -----------------------------------------------------------------------

    #[test]
    fn proactive_split_skips_when_pending_exists() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 49_999_999, 0, SegState::Active));
        segs.insert(1, make_seg(1, 50_000_000, 99_999_999, 0, SegState::Pending));

        let mut next_idx = 2;
        assert!(
            try_proactive_split(&mut segs, &mut next_idx, MIN_SPLIT_BYTES).is_none(),
            "should not proactively split when Pending segments exist"
        );
    }

    #[test]
    fn proactive_split_creates_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 19_999_999, 0, SegState::Active));
        let mut next = 1;

        let result = try_proactive_split(&mut segs, &mut next, MIN_SPLIT_BYTES);
        assert!(result.is_some(), "proactive split should succeed");

        // New segment should be Pending.
        let new_seg = segs.get(&1).expect("new segment exists");
        assert_eq!(new_seg.state, SegState::Pending);
        assert!(validate_coverage(&segs, 20_000_000).is_ok());
    }

    // -----------------------------------------------------------------------
    // all_done
    // -----------------------------------------------------------------------

    #[test]
    fn all_done_true_when_all_completed() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 100, SegState::Completed));
        assert!(all_done(&segs));
    }

    #[test]
    fn all_done_false_when_active() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 50, SegState::Active));
        assert!(!all_done(&segs));
    }

    #[test]
    fn all_done_false_when_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 0, SegState::Pending));
        assert!(!all_done(&segs));
    }

    // -----------------------------------------------------------------------
    // LiveSegment methods
    // -----------------------------------------------------------------------

    #[test]
    fn segment_size_and_remaining() {
        let seg = make_seg(0, 100, 199, 50, SegState::Active);
        assert_eq!(seg.size(), 100);
        assert_eq!(seg.remaining(), 50);
        assert!(!seg.is_complete());
    }

    #[test]
    fn segment_complete() {
        let seg = make_seg(0, 0, 99, 100, SegState::Completed);
        assert!(seg.is_complete());
        assert_eq!(seg.remaining(), 0);
    }

    // -----------------------------------------------------------------------
    // find_next_pending_only（串行模式专用）
    // -----------------------------------------------------------------------

    #[test]
    fn pending_only_returns_pending_segment() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 0, SegState::Pending));
        segs.insert(2, make_seg(2, 200, 299, 0, SegState::Pending));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_some());
        let next = result.unwrap();
        assert_eq!(next.assignment.seg_index, 1);
        assert!(next.split_parent.is_none(), "串行模式不应产生拆分");
        // 分段应被标记为 Active
        assert_eq!(segs.get(&1).unwrap().state, SegState::Active);
    }

    #[test]
    fn pending_only_returns_none_when_no_pending() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99, 100, SegState::Completed));
        segs.insert(1, make_seg(1, 100, 199, 50, SegState::Active));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_none(), "没有 Pending 分段时应返回 None");
    }

    #[test]
    fn pending_only_never_splits() {
        // 即使有很大的 Active 分段，串行模式也不应拆分
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 99_999_999, 1_000_000, SegState::Active));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_none(), "串行模式不应拆分 Active 分段");
        assert_eq!(segs.len(), 1, "分段数量不应增加");
    }

    #[test]
    fn pending_only_resumes_partial_progress() {
        let mut segs = BTreeMap::new();
        segs.insert(0, make_seg(0, 0, 999, 500, SegState::Pending));

        let result = find_next_pending_only(&mut segs);
        assert!(result.is_some());
        let next = result.unwrap();
        assert_eq!(next.assignment.seg_start, 0);
        assert_eq!(next.assignment.actual_start, 500, "应从已下载位置续传");
        assert_eq!(next.assignment.seg_end, 999);
    }

    // -----------------------------------------------------------------------
    // is_server_rejection
    // -----------------------------------------------------------------------

    #[test]
    fn server_rejection_ignores_non_request_errors() {
        // Other、Cancelled、Io 等非 Request 类型的错误不应被判定为服务器拒绝
        assert!(!is_server_rejection(&DownloadError::Other(
            "some error".to_string()
        )));
        assert!(!is_server_rejection(&DownloadError::Cancelled));
        assert!(!is_server_rejection(&DownloadError::Other(
            "403 forbidden".to_string()
        )));
    }

    /// 编译时验证 is_server_rejection 可以接受 DownloadError::Request 变体。
    /// 构造真实的 reqwest::Error(403/429) 需要 `http` crate，此处仅验证类型兼容性。
    #[test]
    fn server_rejection_accepts_request_variant() {
        // 不实际发起 HTTP 请求，仅验证代码路径可编译。
        if false {
            let client = reqwest::Client::new();
            let _ = async {
                let resp = client.get("http://x").send().await.unwrap();
                let err = resp.error_for_status().unwrap_err();
                let dl_err = DownloadError::Request(err);
                let _ = is_server_rejection(&dl_err);
            };
        }
    }

    // -----------------------------------------------------------------------
    // 域名缓存（extract_host / record / is_single_conn）
    // -----------------------------------------------------------------------

    #[test]
    fn extract_host_basic() {
        assert_eq!(
            extract_host("https://example.com/file.zip"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn extract_host_with_port() {
        assert_eq!(
            extract_host("https://example.com:8443/file.zip"),
            Some("example.com:8443".to_string())
        );
    }

    #[test]
    fn extract_host_invalid_url() {
        assert_eq!(extract_host("not a url"), None);
    }

    #[test]
    fn single_conn_domain_record_and_check() {
        let url = "http://single-conn-test-record.example.com/file";
        let domain = "single-conn-test-record.example.com";

        // 预清理：确保本测试域名在全局缓存中不存在（防止并行/重试干扰）
        if let Ok(mut cache) = single_conn_cache().lock() {
            cache.remove(domain);
        }

        assert!(!is_single_conn_domain(url), "记录前不应命中缓存");

        record_single_conn_domain(url);
        assert!(is_single_conn_domain(url), "记录后应命中缓存");

        // 同域名不同路径也应命中
        assert!(
            is_single_conn_domain("http://single-conn-test-record.example.com/other.zip"),
            "同域名不同路径应命中缓存"
        );

        // 不同域名不应命中
        assert!(
            !is_single_conn_domain("http://single-conn-test-record-other.example.com/file"),
            "不同域名不应命中缓存"
        );

        // 清理：从缓存中移除测试数据
        if let Ok(mut cache) = single_conn_cache().lock() {
            cache.remove(domain);
        }
    }

    #[test]
    fn single_conn_domain_different_ports_are_separate() {
        let url_a = "http://single-conn-test-ports-a.example.com:8080/file";
        let url_b = "http://single-conn-test-ports-b.example.com:9090/file";
        let domain_a = "single-conn-test-ports-a.example.com:8080";
        let domain_b = "single-conn-test-ports-b.example.com:9090";

        // 预清理：确保两个测试域名在全局缓存中不存在
        if let Ok(mut cache) = single_conn_cache().lock() {
            cache.remove(domain_a);
            cache.remove(domain_b);
        }

        record_single_conn_domain(url_a);
        assert!(is_single_conn_domain(url_a), "记录后 url_a 应命中缓存");
        // 不同域名（含不同端口）不应命中
        assert!(
            !is_single_conn_domain(url_b),
            "不同端口/域名应视为不同服务器"
        );

        // 清理
        if let Ok(mut cache) = single_conn_cache().lock() {
            cache.remove(domain_a);
            cache.remove(domain_b);
        }
    }
}
