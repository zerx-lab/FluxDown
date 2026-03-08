use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::FutureExt;
use reqwest::Client;
use rinf::RustSignal;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bt_downloader::{self, BtConfig, BtDownloadParams, SharedBtSession, TorrentSource};
use crate::db::Db;
use crate::downloader::{self, DownloadParams, ProgressUpdate, SegmentProgressInfo};
use crate::dash_downloader;
use crate::ftp_downloader;
use crate::hls_downloader;
use crate::proxy_config::ProxyConfig;
use crate::signals::{
    AllQueues, AllTasks, PriorityTaskChanged, QueueInfo, QueuePosition, QueuePositionsUpdate,
    SegmentDetail, SegmentProgress, TaskInfo, TaskMetaProbed, TaskProgress,
};
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
    rinf::debug_print!("[download] PANIC in task {}: {}", task_id, msg);
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
        })
        .await;
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

/// Returns true only when `name` is safe to join onto a base directory for
/// deletion purposes.  Rejects three classes of dangerous values:
///   1. empty string  → `save_dir.join("")` == `save_dir` itself
///   2. absolute path → `PathBuf::join` silently replaces `save_dir` entirely
///   3. `..` component → path traversal that escapes `save_dir`
fn is_safe_file_name(name: &str) -> bool {
    use std::path::Component;
    if name.is_empty() {
        return false;
    }
    let p = std::path::Path::new(name);
    !p.is_absolute()
        && !p
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::RootDir))
}

/// Notification sent from a spawned download task when it finishes.
pub struct TaskDone {
    pub task_id: String,
    /// Generation counter — must match `active_tokens` entry to allow cleanup.
    /// Prevents a stale TaskDone from an old spawn removing a newer token.
    pub generation: u64,
}

/// Per-task state tracked by the progress reporter for EMA speed smoothing.
struct TaskSpeedState {
    /// Smoothed speed in bytes/sec (EMA).
    ema_speed: f64,
    /// Last known downloaded_bytes — for computing delta.
    last_downloaded: i64,
    /// Timestamp of last update.
    last_time: std::time::Instant,
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
    /// Number of downloading updates to skip speed calculation for.
    /// Used as warmup after prepare/resume to avoid artificial speed spikes
    /// caused by baseline jumps (e.g. resume from non-zero downloaded bytes).
    speed_warmup_remaining: u8,
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
}

pub struct DownloadManager {
    db: Db,
    client: Client,
    /// Current proxy configuration — used to rebuild Client on config change.
    proxy_config: ProxyConfig,
    active_tokens: HashMap<String, (CancellationToken, u64)>,
    /// JoinHandles for spawned download tasks — used by `delete_task` to await
    /// task exit, ensuring file handles and network connections are released
    /// before we attempt to remove files from disk.
    active_handles: HashMap<String, JoinHandle<()>>,
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
    /// Set of task_ids that are BT (magnet) downloads.  Used to exclude BT
    /// tasks from the HTTP/FTP concurrency limit in `has_capacity()`.
    bt_task_ids: HashSet<String>,
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
    /// User-configurable BT settings (DHT, UPnP, ports, custom trackers).
    bt_config: BtConfig,
    /// Pending HLS quality selections: task_id → oneshot sender.
    /// Populated when an HLS download starts; consumed when Dart sends
    /// `SelectHlsQuality`.
    hls_quality_senders: HashMap<String, oneshot::Sender<i32>>,
    /// Globally configured user-agent string. Empty = use built-in Chrome UA.
    global_user_agent: String,
    /// Global default segment count from settings. 0 = defer to segment_advisor.
    global_default_segments: i32,
    /// In-memory cache of named queue settings (queue_id → QueueInfo).
    /// Kept in sync with the DB on every queue CRUD operation.
    queues: HashMap<String, QueueInfo>,
    /// Per-queue speed limiters (queue_id → SpeedLimiter).
    /// Created on demand for queues that have speed_limit_kbps > 0.
    queue_limiters: HashMap<String, SpeedLimiter>,
    /// Maps active task_id → queue_id for per-queue concurrency counting.
    active_task_queue: HashMap<String, String>,
    /// 是否已完成启动时的 reset_incomplete_tasks_to_paused 矫正。
    /// 该矫正仅需在第一次 load_and_send_all_tasks 时执行一次，
    /// 后续由 create_task / batch_create 触发时不得重复重置。
    startup_reset_done: bool,
    /// Boost 模式当前优先任务 ID（内存级，重启清空）。None = 无优先任务。
    priority_task_id: Option<String>,
    /// 因 Boost 模式自动暂停的任务 ID 集合（内存级，重启清空）。
    /// 取消 Boost 时这些任务会自动恢复。
    auto_paused_ids: HashSet<String>,
}

/// Configuration parameters for [`DownloadManager::new`].
/// Grouping avoids the `clippy::too_many_arguments` limit and makes
/// call sites self-documenting.
pub struct DownloadManagerConfig {
    pub max_concurrent: usize,
    pub speed_limit_bps: u64,
    pub default_save_dir: String,
    pub app_data_dir: String,
    pub bt_config: BtConfig,
    pub proxy_config: ProxyConfig,
    pub user_agent: String,
}
impl DownloadManager {
    pub fn new(
        db: Db,
        config: DownloadManagerConfig,
    ) -> Result<Self, downloader::DownloadError> {
        let DownloadManagerConfig {
            max_concurrent,
            speed_limit_bps,
            default_save_dir,
            app_data_dir,
            bt_config,
            proxy_config,
            user_agent,
        } = config;
        let client = downloader::build_client(&proxy_config, &user_agent)?;
        let (tx, rx) = mpsc::channel(8192);
        let (done_tx, done_rx) = mpsc::channel(64);
        let limiter = SpeedLimiter::new(speed_limit_bps);
        limiter.spawn_refill_task();
        Ok(Self {
            db,
            client,
            proxy_config,
            active_tokens: HashMap::new(),
            active_handles: HashMap::new(),
            generation: 0,
            progress_tx: tx,
            progress_rx: Some(rx),
            done_tx,
            done_rx: Some(done_rx),
            max_concurrent,
            pending_queue: VecDeque::new(),
            bt_task_ids: HashSet::new(),
            speed_limiter: limiter,
            bt_session: None,
            hls_quality_senders: HashMap::new(),
            default_save_dir,
            app_data_dir,
            bt_config,
            global_user_agent: user_agent,
            global_default_segments: 0,
            queues: HashMap::new(),
            queue_limiters: HashMap::new(),
            active_task_queue: HashMap::new(),
            startup_reset_done: false,
            priority_task_id: None,
            auto_paused_ids: HashSet::new(),
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

    /// Update global speed limit (bytes/sec).  Takes effect immediately on
    /// all active and future HTTP/FTP/BT downloads.  0 = unlimited.
    pub fn set_speed_limit(&mut self, bps: u64) {
        self.speed_limiter.set_limit(bps);
        // Synchronise speed limit to the shared BT session (if initialised).
        if let Some(ref bt) = self.bt_session {
            bt.set_speed_limit(bps);
        }
    }

    /// Update proxy configuration.  Rebuilds the shared HTTP client so that
    /// all **new** downloads use the updated proxy settings.  Already-running
    /// downloads keep their existing client and are unaffected.
    ///
    /// Returns `Err` if the new client cannot be built (e.g. invalid SOCKS URL).
    pub fn set_proxy_config(&mut self, config: ProxyConfig) -> Result<(), downloader::DownloadError> {
        rinf::debug_print!(
            "[manager] updating proxy config: mode={}, type={}, host={}, port={}",
            config.mode.as_str(),
            config.proxy_type.as_str(),
            config.host,
            config.port,
        );
        let new_client = downloader::build_client(&config, &self.global_user_agent)?;
        self.client = new_client;
        self.proxy_config = config;
        Ok(())
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
        rinf::debug_print!("[manager] updating global_user_agent: {}", ua);
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
            .map_err(|e| downloader::DownloadError::Other(
                format!("BT session init thread panicked: {e}"),
            ))??;
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

    /// Invalidate (destroy) the current BT session so it will be re-created
    /// with the latest `bt_config` on the next BT download.  Active BT
    /// downloads will be paused first.
    pub async fn invalidate_bt_session(&mut self) {
        if let Some(bt) = self.bt_session.take() {
            rinf::debug_print!("[manager] invalidating BT session for config change");
            let bt_clone = bt.clone();
            // Shutdown on a blocking thread since it calls block_on internally.
            let _ = tokio::task::spawn_blocking(move || {
                bt_clone.shutdown();
            })
            .await;
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
        QueuePositionsUpdate { positions }.send_signal_to_dart();
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
            Err(e) => rinf::debug_print!("[manager] load_queues error: {}", e),
        }
    }

    /// Whether we have a free slot for a new HTTP/FTP download.
    /// BT tasks are excluded from this count because they are managed by the
    /// shared librqbit session with its own concurrency controls.
    fn has_capacity(&self) -> bool {
        if self.max_concurrent == 0 {
            return true;
        }
        // Use saturating_sub to guard against underflow if bt_task_ids
        // temporarily contains an entry not yet cleaned from active_tokens
        // (e.g. generation mismatch in on_task_done).
        let http_ftp_active = self.active_tokens.len().saturating_sub(self.bt_task_ids.len());
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
        let queue_max = self
            .queues
            .get(queue_id)
            .map(|q| q.max_concurrent as usize)
            .unwrap_or(0);
        if queue_max == 0 {
            return true;
        }
        let active_in_queue = self
            .active_task_queue
            .values()
            .filter(|qid| qid.as_str() == queue_id)
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
        let mut i = 0;
        while i < self.pending_queue.len() {
            // Global concurrency ceiling reached — stop entirely.
            if !self.has_capacity() {
                break;
            }
            // Edge case: task was resumed/cancelled while queued.
            if self.active_tokens.contains_key(&self.pending_queue[i].task_id) {
                self.pending_queue.remove(i);
                continue;
            }
            // Queue-level concurrency check: skip (don't remove) if the
            // target queue is full; try the next pending task instead.
            if !self.has_queue_capacity(&self.pending_queue[i].queue_id.clone()) {
                i += 1;
                continue;
            }
            let Some(queued) = self.pending_queue.remove(i) else { break };
            if queued.is_resume {
                self.do_resume_task(&queued.task_id).await;
            } else {
                self.do_start_task(queued).await;
            }
            // Don't increment i — an element was removed.
        }
        // 队列变化后广播最新位置
        self.broadcast_queue_positions();
    }

    // -----------------------------------------------------------------------
    // Public task operations
    // -----------------------------------------------------------------------

    /// Remove a finished task from active_tokens (called by actor loop).
    /// Only removes the entry if the generation matches, preventing a stale
    /// `TaskDone` from an old spawn from accidentally removing a newer token.
    pub async fn on_task_done(&mut self, task_id: &str, generation: u64) {
        let generation_matched = if let Some((_, stored_gen)) = self.active_tokens.get(task_id) {
            *stored_gen == generation
        } else {
            false
        };

        if generation_matched {
            self.active_tokens.remove(task_id);
            self.active_handles.remove(task_id);
            self.bt_task_ids.remove(task_id);
            self.hls_quality_senders.remove(task_id);
            self.active_task_queue.remove(task_id);

            // Boost 模式：优先任务完成后自动恢复其他任务。
            // 仅在 generation 匹配时触发，防止旧 spawn 发来的 stale TaskDone
            // 误将仍在运行的新 spawn 的 Boost 状态清除。
            if self.priority_task_id.as_deref() == Some(task_id) {
                self.clear_priority().await;
            }
        }

        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
        self.maybe_wal_checkpoint().await;
        self.maybe_release_bt_session().await;
    }

    /// Run a WAL checkpoint when all tasks are idle (no active downloads and
    /// nothing queued) so the WAL file doesn't linger and cause sporadic disk
    /// I/O in the background.
    async fn maybe_wal_checkpoint(&self) {
        if self.active_tokens.is_empty()
            && self.pending_queue.is_empty()
            && let Err(e) = self.db.wal_checkpoint().await
        {
            rinf::debug_print!("[manager] wal_checkpoint error: {e}");
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
        if !self.bt_task_ids.is_empty() {
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
        if let Some(ref bt) = self.bt_session {
            if bt.has_inflight_adds() {
                rinf::debug_print!(
                    "[manager] deferring BT session release — detached add_torrent still in flight"
                );
                return;
            }
        }
        rinf::debug_print!("[manager] all BT tasks finished/paused — releasing BT session");
        // Shut down on a background thread (same pattern as Drop) to avoid
        // blocking the actor loop while the librqbit runtime winds down.
        if let Some(bt) = self.bt_session.take() {
            std::thread::spawn(move || {
                match Arc::try_unwrap(bt) {
                    Ok(owned) => owned.shutdown(),
                    Err(shared) => shared.shutdown(),
                }
            });
        }
    }

    /// Forward a quality selection from Dart to the waiting HLS download task.
    pub fn send_hls_quality_selection(&mut self, task_id: &str, selected_index: i32) {
        if let Some(tx) = self.hls_quality_senders.remove(task_id) {
            let _ = tx.send(selected_index);
        } else {
            rinf::debug_print!(
                "[manager] no pending HLS quality selection for task {}",
                task_id
            );
        }
    }

    pub async fn load_and_send_all_tasks(&mut self) {
        // 启动时将残留的 downloading/pending 状态矫正为 paused（仅首次执行）
        // 后续由 create_task / batch_create 触发时不重复重置，避免将刚插入的
        // pending 任务误改为 paused 导致前端显示"已暂停"
        if !self.startup_reset_done {
            self.startup_reset_done = true;
            if let Err(e) = self.db.reset_incomplete_tasks_to_paused().await {
                rinf::debug_print!("reset_incomplete_tasks_to_paused error: {}", e);
            }
        }

        let tasks = match self.db.load_all_tasks().await {
            Ok(t) => t,
            Err(e) => {
                rinf::debug_print!("load_all_tasks error: {}", e);
                Vec::new()
            }
        };

        // Snapshot task info before sending AllTasks (which consumes `tasks`).
        let task_snapshots: Vec<(String, i64)> = tasks
            .iter()
            .map(|t| (t.task_id.clone(), t.total_bytes))
            .collect();

        AllTasks { tasks }.send_signal_to_dart();

        // Send persisted segment data for each task so the UI can display
        // download distribution immediately after app restart.
        for (task_id, total_bytes) in &task_snapshots {
            self.send_segments_from_db(task_id, *total_bytes).await;
        }
    }

    /// Load segment records from DB and send a `SegmentProgress` signal to Dart.
    /// Used when pausing and on app startup to restore the download distribution
    /// visualization without requiring an active download.
    async fn send_segments_from_db(&self, task_id: &str, total_bytes: i64) {
        if let Ok(db_segs) = self.db.load_segments(task_id).await
            && !db_segs.is_empty()
        {
            SegmentProgress {
                task_id: task_id.to_string(),
                total_bytes,
                segment_count: db_segs.len() as i32,
                segments: db_segs
                    .iter()
                    .map(|s| SegmentDetail {
                        index: s.index,
                        start_byte: s.start_byte,
                        end_byte: s.end_byte,
                        downloaded_bytes: s.downloaded_bytes,
                    })
                    .collect(),
            }
            .send_signal_to_dart();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_task(
        &mut self,
        url: String,
        save_dir: String,
        file_name: String,
        segments: i32,
        cookies: String,
        referrer: String,
        hint_file_size: i64,
        torrent_file_bytes: Vec<u8>,
        proxy_url: String,
        user_agent: String,
        queue_id: String,
        checksum: String,
    ) {
        let task_id = Uuid::new_v4().to_string();
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

        if let Err(e) = self
            .db
            .insert_task(&task_id, &db_url, &file_name, &save_dir, seg, 0, &proxy_url, &queue_id, &checksum)
            .await
        {
            rinf::debug_print!("insert_task error: {}", e);
            return;
        }

        // Persist .torrent file bytes to DB for resume after restart.
        if !torrent_file_bytes.is_empty()
            && let Err(e) = self.db.save_torrent_file_bytes(&task_id, &torrent_file_bytes).await
        {
            rinf::debug_print!("save_torrent_file_bytes error: {}", e);
        }

        TaskProgress {
            task_id: task_id.clone(),
            status: 0,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            file_name: file_name.clone(),
            save_dir: save_dir.clone(),
            url: db_url.clone(),
            error_message: String::new(),
        }
        .send_signal_to_dart();

        // BT tasks bypass the HTTP/FTP concurrency queue — they are managed
        // by the shared librqbit session with its own concurrency controls.
        let is_bt = is_magnet(&url) || !torrent_file_bytes.is_empty();
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
        };
        if is_bt || (self.has_capacity() && self.has_queue_capacity(&queued.queue_id)) {
            self.do_start_task(queued).await;
            // If do_start_task failed early (e.g. BT session init), the slot
            // was freed — drain the queue so pending tasks can proceed.
            self.drain_queue().await;
        } else {
            rinf::debug_print!(
                "[manager] queuing task {} (active={}, max={}, queue={})",
                queued.task_id,
                self.active_tokens.len(),
                self.max_concurrent,
                queued.queue_id
            );
            // 保存探测所需信息（queued 即将被 move 进队列）
            let probe_tid = queued.task_id.clone();
            let probe_url = queued.url.clone();
            let probe_name = queued.file_name.clone();
            self.pending_queue.push_back(queued);
            // 广播最新队列位置
            self.broadcast_queue_positions();
            // Spawn 元数据探测（后台，非阻塞）
            let probe_client = self.client.clone();
            let probe_db = self.db.clone();
            tokio::spawn(async move {
                let (name, size) =
                    crate::meta_prober::probe_task_meta(&probe_url, &probe_name, &probe_client)
                        .await;
                if !name.is_empty() || size > 0 {
                    if !name.is_empty() {
                        let _ = probe_db.update_task_file_name(&probe_tid, &name).await;
                    }
                    TaskMetaProbed {
                        task_id: probe_tid,
                        file_name: name,
                        total_bytes: size,
                    }
                    .send_signal_to_dart();
                }
            });
        }
    }

    /// Internal: actually spawn the download task (no concurrency check).
    async fn do_start_task(&mut self, queued: QueuedTask) {
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
        } = queued;

        // Four-tier segment count priority:
        //   1. Task-level explicit choice (segments > 0) — highest priority
        //   2. Queue default_segments (> 0) — inherits from queue when task is auto
        //   3. Global default_segments (> 0) — global setting from config
        //   4. Segment advisor (segments == 0) — dynamic calculation at runtime
        let queue_default = self.queues
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

        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();
        self.active_tokens
            .insert(task_id.clone(), (cancel_token.clone(), spawn_gen));
        // Track which queue this task belongs to for per-queue concurrency counting.
        self.active_task_queue.insert(task_id.clone(), queue_id.clone());
        // Select speed limiter: queue-specific if the queue has a limit, global otherwise.
        let speed_limiter = self.queue_limiter_for(&queue_id);

        let use_ftp = is_ftp_url(&url);
        let use_hls = hls_downloader::is_hls_url(&url);
        let use_dash = dash_downloader::is_dash_url(&url);
        let use_bt = is_magnet(&url) || !torrent_file_bytes.is_empty() || is_torrent_file_url(&url);

        if use_bt {
            self.bt_task_ids.insert(task_id.clone());
        }

        let done_tx = self.done_tx.clone();
        let panic_progress_tx = self.progress_tx.clone();
        let panic_task_id = task_id.clone();
        let panic_db = self.db.clone();

        let handle = if use_bt {
            // Lazily initialise the shared BT session.
            if let Err(e) = self.ensure_bt_session().await {
                rinf::debug_print!("[manager] failed to init BT session: {}", e);
                let _ = self.db.update_task_status(&task_id, 4, &e.to_string()).await;
                let _ = self.progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.clone(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: 4,
                        error_message: e.to_string(),
                        file_name: String::new(),
                        segment_details: None,
                    })
                    .await;
                self.active_tokens.remove(&task_id);
                self.bt_task_ids.remove(&task_id);
                return;
            }
            // bt_session is guaranteed to be Some after ensure_bt_session().
            let Some(bt_ref) = self.bt_session.as_ref() else {
                rinf::debug_print!("[manager] BUG: bt_session is None after ensure_bt_session succeeded");
                self.active_tokens.remove(&task_id);
                return;
            };

            // Build the torrent source: prefer torrent file bytes if available,
            // otherwise use the URL as a magnet link.
            let torrent_source = if !torrent_file_bytes.is_empty() {
                TorrentSource::TorrentFileBytes(torrent_file_bytes)
            } else {
                TorrentSource::Magnet(url)
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
            };

            tokio::spawn(async move {
                let result = std::panic::AssertUnwindSafe(bt_downloader::run_bt_download(bt_params))
                    .catch_unwind()
                    .await;

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx.send(TaskDone { task_id: panic_task_id, generation: spawn_gen }).await;
            })
        } else {
            // Resolve proxy and UA: per-task values override global config.
            // `.resolve()` expands System mode into a concrete Manual config
            // so that FTP downloader (which reads host/port directly) works.
            //
            // Three-tier UA resolution (highest → lowest priority):
            //   1. Per-task explicit UA — set when creating/confirming the task
            //   2. Per-queue default UA — inherited from queue when task UA is empty
            //   3. Global UA — final fallback when both task and queue UA are empty
            let queue_ua = self
                .queues
                .get(&queue_id)
                .map(|q| q.default_user_agent.as_str())
                .unwrap_or("");
            let resolved_ua = if !user_agent.is_empty() {
                user_agent.as_str()
            } else if !queue_ua.is_empty() {
                queue_ua
            } else {
                self.global_user_agent.as_str()
            };
            let needs_rebuild = !proxy_url.is_empty() || !user_agent.is_empty() || !queue_ua.is_empty();
            let (task_client, task_proxy) = if needs_rebuild {
                let pc = if proxy_url.is_empty() {
                    self.proxy_config.resolve()
                } else {
                    ProxyConfig::from_proxy_url(&proxy_url)
                };
                match downloader::build_client(&pc, resolved_ua) {
                    Ok(c) => (c, pc),
                    Err(e) => {
                        rinf::debug_print!(
                            "[manager] failed to build per-task client: {}",
                            e
                        );
                        // Fallback to global
                        (self.client.clone(), self.proxy_config.resolve())
                    }
                }
            } else {
                (self.client.clone(), self.proxy_config.resolve())
            };

            let hls_quality_rx = if use_hls || use_dash {
                let (tx, rx) = oneshot::channel();
                self.hls_quality_senders.insert(task_id.clone(), tx);
                Some(rx)
            } else {
                None
            };

            let params = DownloadParams {
                task_id: task_id.clone(),
                url,
                save_dir,
                file_name,
                segment_count: segments,
                is_resume: false,
                db: self.db.clone(),
                client: task_client,
                progress_tx: self.progress_tx.clone(),
                cancel_token,
                speed_limiter,
                cookies,
                referrer,
                hint_file_size,
                proxy_config: task_proxy,
                hls_quality_rx,
                checksum,
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
                } else {
                    std::panic::AssertUnwindSafe(downloader::run_download(params))
                        .catch_unwind()
                        .await
                };

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx.send(TaskDone { task_id: panic_task_id, generation: spawn_gen }).await;
            })
        };
        self.active_handles.insert(task_id, handle);
    }

    pub async fn pause_task(&mut self, task_id: &str) {
        // Remove from pending queue if queued (not yet started).
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
            // 广播更新后的队列位置
            self.broadcast_queue_positions();
            let _ = self.db.update_task_status(task_id, 2, "").await;
            if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
                TaskProgress {
                    task_id: task_id.to_string(),
                    status: 2,
                    downloaded_bytes: t.downloaded_bytes,
                    total_bytes: t.total_bytes,
                    speed: 0,
                    file_name: t.file_name.clone(),
                    save_dir: t.save_dir.clone(),
                    url: t.url.clone(),
                    error_message: String::new(),
                }
                .send_signal_to_dart();
            }
            return;
        }

        if let Some((token, _gen)) = self.active_tokens.remove(task_id) {
            token.cancel();
            self.bt_task_ids.remove(task_id);
            self.hls_quality_senders.remove(task_id);
            self.active_task_queue.remove(task_id);

            // For BT tasks, explicitly pause the torrent in the session so
            // that the handle stays cached for fast resume.  This is a
            // no-op if the download loop already called session.pause on
            // cancellation detection, but covers edge cases (e.g. pause
            // during metadata resolution).
            if let Some(ref bt) = self.bt_session {
                let _ = bt.pause_task(task_id).await;
            }

            let _ = self.db.update_task_status(task_id, 2, "").await;

            if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
                TaskProgress {
                    task_id: task_id.to_string(),
                    status: 2,
                    downloaded_bytes: t.downloaded_bytes,
                    total_bytes: t.total_bytes,
                    speed: 0,
                    file_name: t.file_name.clone(),
                    save_dir: t.save_dir.clone(),
                    url: t.url.clone(),
                    error_message: String::new(),
                }
                .send_signal_to_dart();

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
            self.maybe_release_bt_session().await;
        }
    }

    pub async fn resume_task(&mut self, task_id: &str) {
        if self.active_tokens.contains_key(task_id) {
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
            rinf::debug_print!(
                "[manager] resume_task {}: stale active_tokens entry (terminal in DB) — force-removing",
                task_id
            );
            self.active_tokens.remove(task_id);
            self.active_handles.remove(task_id);
            self.bt_task_ids.remove(task_id);
            self.hls_quality_senders.remove(task_id);
            self.active_task_queue.remove(task_id);
            // Do NOT drain_queue here — we are about to occupy the freed slot.
        }

        // Also check if already in the pending queue.
        if self.pending_queue.iter().any(|q| q.task_id == task_id) {
            return;
        }

        // Load task once and reuse for both the is_bt check and the queue entry.
        let task_row = self.db.load_task_by_id(task_id).await.ok().flatten();
        let is_bt = task_row.as_ref().map(|t| is_bt_url(&t.url)).unwrap_or(false);
        let queue_id = task_row.as_ref().map(|t| t.queue_id.clone()).unwrap_or_default();

        if is_bt || (self.has_capacity() && self.has_queue_capacity(&queue_id)) {
            self.do_resume_task(task_id).await;
            // If do_resume_task failed early (e.g. BT session init), drain
            // the queue so pending tasks can proceed.
            self.drain_queue().await;
        } else {
            rinf::debug_print!(
                "[manager] queuing resume for task {} (active={}, max={}, queue={})",
                task_id,
                self.active_tokens.len(),
                self.max_concurrent,
                queue_id
            );
            if let Some(t) = task_row {
                // Notify Dart: task is now queued (pending), not actively resuming.
                // Without this signal, the UI keeps all tasks stuck in "resuming" status
                // even though only max_concurrent are actually downloading.
                TaskProgress {
                    task_id: task_id.to_string(),
                    status: 0, // pending/queued
                    downloaded_bytes: t.downloaded_bytes,
                    total_bytes: t.total_bytes,
                    speed: 0,
                    file_name: t.file_name.clone(),
                    save_dir: t.save_dir.clone(),
                    url: t.url.clone(),
                    error_message: String::new(),
                }
                .send_signal_to_dart();
                self.pending_queue.push_back(QueuedTask {
                    task_id: task_id.to_string(),
                    url: t.url,
                    save_dir: t.save_dir,
                    file_name: t.file_name,
                    segments: 0, // not used for resume
                    is_resume: true,
                    cookies: String::new(), // cookies not available for resume from DB
                    referrer: String::new(), // referrer not persisted; not needed for resume
                    hint_file_size: 0, // no hint on resume; use probe to get current size
                    torrent_file_bytes: Vec::new(), // loaded from DB in do_resume_task
                    proxy_url: t.proxy_url,
                    user_agent: String::new(), // use global UA on resume
                    queue_id: t.queue_id,
                    checksum: t.checksum, // loaded from DB for integrity verification
                });
            }
        }
    }

    /// Internal: actually spawn the resume (no concurrency check).
    async fn do_resume_task(&mut self, task_id: &str) {
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                rinf::debug_print!("[manager] do_resume_task: task {} not found in DB", task_id);
                return;
            }
            Err(e) => {
                rinf::debug_print!("[manager] do_resume_task: DB error for task {}: {}", task_id, e);
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
                    })
                    .await;
                return;
            }
        };

        // Read actual segment count from DB.  0 means "auto" — the downloader
        // will dynamically calculate the optimal count.
        let seg_count: i32 = self.db.get_task_segments(task_id).await.unwrap_or_default();

        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();
        self.active_tokens
            .insert(task_id.to_string(), (cancel_token.clone(), spawn_gen));
        // Track queue membership and select the appropriate speed limiter.
        self.active_task_queue.insert(task_id.to_string(), task.queue_id.clone());
        let speed_limiter = self.queue_limiter_for(&task.queue_id);

        let use_ftp = is_ftp_url(&task.url);
        let use_hls = hls_downloader::is_hls_url(&task.url);
        let use_dash = dash_downloader::is_dash_url(&task.url);
        let use_bt = is_bt_url(&task.url);

        if use_bt {
            self.bt_task_ids.insert(task_id.to_string());
        }

        let tid = task_id.to_string();
        let done_tx = self.done_tx.clone();
        let panic_progress_tx = self.progress_tx.clone();
        let panic_task_id = tid.clone();
        let panic_db = self.db.clone();

        let handle = if use_bt {
            // Lazily initialise the shared BT session.
            if let Err(e) = self.ensure_bt_session().await {
                rinf::debug_print!("[manager] failed to init BT session for resume: {}", e);
                let _ = self.db.update_task_status(task_id, 4, &e.to_string()).await;
                let _ = self.progress_tx
                    .send(ProgressUpdate {
                        task_id: tid.clone(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: 4,
                        error_message: e.to_string(),
                        file_name: String::new(),
                        segment_details: None,
                    })
                    .await;
                self.active_tokens.remove(task_id);
                self.bt_task_ids.remove(task_id);
                return;
            }
            // bt_session is guaranteed to be Some after ensure_bt_session().
            let Some(bt_ref) = self.bt_session.as_ref() else {
                rinf::debug_print!("[manager] BUG: bt_session is None after ensure_bt_session succeeded");
                self.active_tokens.remove(task_id);
                return;
            };

            // Try to resume from a cached handle (pause→resume within the
            // same app session).  If the handle is found, unpause it and
            // pass it to the download loop so it skips add_torrent entirely.
            let mut existing = match bt_ref.resume_task(task_id).await {
                Ok(h) => h,
                Err(e) => {
                    rinf::debug_print!("[manager] BT resume_task error (will re-add): {}", e);
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
                if !output_path.exists() {
                    rinf::debug_print!(
                        "[manager] BT task {} output missing ({}), discarding cached handle for re-verify",
                        task_id, output_path.display()
                    );
                    // Delete the stale torrent from the session so add_torrent
                    // can re-add it fresh with proper piece verification.
                    bt_ref.delete_task(task_id, false).await;
                    existing = None;
                }
            }

            // Build the torrent source for resume: if the task was created
            // from a .torrent file, load the persisted bytes from DB.
            let torrent_source = if is_torrent_file_url(&task.url) {
                let bytes = self.db.load_torrent_file_bytes(task_id).await
                    .unwrap_or_default()
                    .unwrap_or_default();
                if bytes.is_empty() {
                    rinf::debug_print!(
                        "[manager] BT task {} has torrent-file:// URL but no persisted bytes!",
                        task_id
                    );
                    let msg = "torrent file bytes lost — cannot resume";
                    let _ = self.db.update_task_status(task_id, 4, msg).await;
                    self.active_tokens.remove(task_id);
                    self.bt_task_ids.remove(task_id);
                    return;
                }
                TorrentSource::TorrentFileBytes(bytes)
            } else {
                TorrentSource::Magnet(task.url.clone())
            };

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
            };

            tokio::spawn(async move {
                let result = std::panic::AssertUnwindSafe(bt_downloader::run_bt_download(bt_params))
                    .catch_unwind()
                    .await;

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx.send(TaskDone { task_id: panic_task_id, generation: spawn_gen }).await;
            })
        } else {
            // Resolve proxy and UA for resume: use global UA (cookies not
            // persisted in DB, so only proxy_url is available from task row).
            let needs_rebuild = !task.proxy_url.is_empty() || !self.global_user_agent.is_empty();
            let (task_client, task_proxy) = if needs_rebuild {
                let pc = if task.proxy_url.is_empty() {
                    self.proxy_config.resolve()
                } else {
                    ProxyConfig::from_proxy_url(&task.proxy_url)
                };
                match downloader::build_client(&pc, &self.global_user_agent) {
                    Ok(c) => (c, pc),
                    Err(e) => {
                        rinf::debug_print!(
                            "[manager] failed to build per-task client on resume: {}",
                            e
                        );
                        (self.client.clone(), self.proxy_config.resolve())
                    }
                }
            } else {
                (self.client.clone(), self.proxy_config.resolve())
            };

            let hls_quality_rx = if use_hls || use_dash {
                let (tx, rx) = oneshot::channel();
                self.hls_quality_senders.insert(tid.clone(), tx);
                Some(rx)
            } else {
                None
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
                cookies: String::new(),
                referrer: String::new(), // referrer not persisted; not needed for resume
                hint_file_size: 0, // no hint on resume; use probe to get current size
                proxy_config: task_proxy,
                hls_quality_rx,
                checksum: task.checksum,
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
                } else {
                    std::panic::AssertUnwindSafe(downloader::run_download(params))
                        .catch_unwind()
                        .await
                };

                if let Err(panic_info) = result {
                    let msg = panic_message(&panic_info);
                    handle_task_panic(&panic_task_id, &msg, &panic_db, &panic_progress_tx).await;
                }

                let _ = done_tx.send(TaskDone { task_id: panic_task_id, generation: spawn_gen }).await;
            })
        };
        self.active_handles.insert(tid, handle);
    }

    pub async fn cancel_task(&mut self, task_id: &str) {
        // Remove from pending queue if queued.
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
        }

        if let Some((token, _gen)) = self.active_tokens.remove(task_id) {
            token.cancel();
            self.bt_task_ids.remove(task_id);
            self.hls_quality_senders.remove(task_id);
            self.active_task_queue.remove(task_id);

            // For BT tasks, explicitly pause the torrent in the session so
            // that fast-resume data is preserved and the user can resume later.
            // This mirrors what pause_task does for BT tasks.
            if let Some(ref bt) = self.bt_session {
                let _ = bt.pause_task(task_id).await;
            }
        }
        // Clean up the JoinHandle so it doesn't linger after cancellation.
        self.active_handles.remove(task_id);

        let _ = self.db.update_task_status(task_id, 4, "cancelled").await;

        // Send update with actual task info if available
        let (file_name, save_dir, url) = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => (t.file_name, t.save_dir, t.url),
            _ => Default::default(),
        };

        TaskProgress {
            task_id: task_id.to_string(),
            status: 4,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            file_name,
            save_dir,
            url,
            error_message: "cancelled".to_string(),
        }
        .send_signal_to_dart();

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
        // Remove from pending queue if queued.
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
        }

        // Cancel the active download (if any) and wait for the spawned task
        // to exit, ensuring all network sockets and file handles are closed.
        if let Some((token, _gen)) = self.active_tokens.remove(task_id) {
            token.cancel();
            self.bt_task_ids.remove(task_id);
            self.hls_quality_senders.remove(task_id);
            self.active_task_queue.remove(task_id);
        }
        if let Some(handle) = self.active_handles.remove(task_id) {
            // Timeout guard: don't block forever if the task misbehaves.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                handle,
            )
            .await;
        }

        if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
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
            } else {
                // HTTP / FTP / HLS / DASH: always clean up the in-progress temp file
                let temp_path =
                    PathBuf::from(format!("{}{}", path.display(), downloader::TEMP_EXT));
                let _ = tokio::fs::remove_file(&temp_path).await;

                // DASH audio sidecar: clean up .audio.m4a and its .part temp
                if dash_downloader::is_dash_url(&t.url) {
                    let audio_path = dash_downloader::build_audio_path(&path);
                    let audio_temp = PathBuf::from(format!(
                        "{}{}",
                        audio_path.display(),
                        downloader::TEMP_EXT
                    ));
                    let _ = tokio::fs::remove_file(&audio_temp).await;
                    if delete_files {
                        let _ = tokio::fs::remove_file(&audio_path).await;
                    }
                }

                if delete_files && is_safe_file_name(&t.file_name) {
                    let _ = tokio::fs::remove_file(&path).await;
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
            })
            .await;

        let _ = self.db.delete_task(task_id).await;

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
        rinf::debug_print!(
            "[manager] delete_tasks_batch: {} tasks, delete_files={}",
            task_ids.len(),
            delete_files
        );

        // 1. Remove from pending queue in one pass.
        self.pending_queue.retain(|q| !id_set.contains(q.task_id.as_str()));

        // 2. Cancel all active downloads + collect (task_id, JoinHandle) pairs.
        //    We pair each handle with its task ID so we can send per-task
        //    "deleted" confirmation as soon as that handle completes, rather
        //    than waiting for ALL handles before starting any cleanup.
        let mut handle_map: HashMap<String, JoinHandle<()>> = HashMap::new();
        for tid in task_ids {
            if let Some((token, _gen)) = self.active_tokens.remove(tid.as_str()) {
                token.cancel();
                self.bt_task_ids.remove(tid.as_str());
                self.hls_quality_senders.remove(tid.as_str());
                self.active_task_queue.remove(tid.as_str());
            }
            if let Some(h) = self.active_handles.remove(tid.as_str()) {
                handle_map.insert(tid.clone(), h);
            }
        }

        // 3. Batch-load all task info from DB in one query (non-blocking, no
        //    need to wait for handles first).
        let task_infos = self.db.load_tasks_by_ids(task_ids).await.unwrap_or_default();
        let info_map: HashMap<&str, &TaskInfo> = task_infos
            .iter()
            .map(|t| (t.task_id.as_str(), t))
            .collect();

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
                    cleanup_futs.push(tokio::spawn(async move {
                        // Wait for this task's download handle (10s per-task timeout).
                        if let Some(h) = maybe_handle {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(10),
                                h,
                            )
                            .await;
                        }
                        // BT session delete
                        if let Some(ref bt) = bt_session {
                            let found = bt.delete_task(&tid_owned, delete_files).await;
                            if !found {
                                bt.register_pending_delete(&tid_owned, delete_files).await;
                            }
                        }
                        // BT file cleanup
                        if delete_files && safe {
                            let _permit = sem.acquire().await.unwrap();
                            if path.is_dir() {
                                let _ = tokio::fs::remove_dir_all(&path).await;
                            } else {
                                let _ = tokio::fs::remove_file(&path).await;
                            }
                        }
                        // Signal completion
                        let _ = ptx
                            .send(ProgressUpdate {
                                task_id: tid_owned,
                                downloaded_bytes: 0,
                                total_bytes: 0,
                                status: 4,
                                error_message: "deleted".to_string(),
                                file_name: String::new(),
                                segment_details: None,
                            })
                            .await;
                    }));
                } else {
                    let url = t.url.clone();
                    let file_name = t.file_name.clone();
                    cleanup_futs.push(tokio::spawn(async move {
                        // Wait for this task's download handle (10s per-task timeout).
                        if let Some(h) = maybe_handle {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(10),
                                h,
                            )
                            .await;
                        }
                        let _permit = sem.acquire().await.unwrap();
                        // Remove temp file
                        let temp_path = PathBuf::from(format!(
                            "{}{}",
                            path.display(),
                            crate::downloader::TEMP_EXT
                        ));
                        let _ = tokio::fs::remove_file(&temp_path).await;

                        // DASH audio sidecar cleanup
                        if dash_downloader::is_dash_url(&url) {
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

                        if delete_files && is_safe_file_name(&file_name) {
                            let _ = tokio::fs::remove_file(&path).await;
                        }

                        // Signal completion
                        let _ = ptx
                            .send(ProgressUpdate {
                                task_id: tid_owned,
                                downloaded_bytes: 0,
                                total_bytes: 0,
                                status: 4,
                                error_message: "deleted".to_string(),
                                file_name: String::new(),
                                segment_details: None,
                            })
                            .await;
                    }));
                }
            } else {
                // Task NOT in DB (already cleaned / no record) — just wait
                // for handle (if any) then signal immediately.
                cleanup_futs.push(tokio::spawn(async move {
                    if let Some(h) = maybe_handle {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            h,
                        )
                        .await;
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

        // 6. Single-transaction batch DB delete.
        if let Err(e) = self.db.delete_tasks_batch(task_ids).await {
            rinf::debug_print!("[manager] delete_tasks_batch DB error: {}", e);
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
        let task_map: HashMap<String, TaskInfo> =
            match self.db.load_tasks_by_ids(task_ids).await {
                Ok(tasks) => tasks.into_iter().map(|t| (t.task_id.clone(), t)).collect(),
                Err(e) => {
                    rinf::debug_print!(
                        "[manager] batch_resume: load_tasks_by_ids error: {}",
                        e
                    );
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
        if self.active_tokens.contains_key(task_id) {
            let is_terminal = task_row.status == 3 || task_row.status == 4;
            if !is_terminal {
                return; // truly still active — do not interrupt
            }
            rinf::debug_print!(
                "[manager] resume_task {}: stale active_tokens entry (terminal in DB) — force-removing",
                task_id
            );
            self.active_tokens.remove(task_id);
            self.active_handles.remove(task_id);
            self.bt_task_ids.remove(task_id);
            self.hls_quality_senders.remove(task_id);
            self.active_task_queue.remove(task_id);
        }

        if self.pending_queue.iter().any(|q| q.task_id == task_id) {
            return;
        }

        let is_bt = is_bt_url(&task_row.url);
        let queue_id = task_row.queue_id.clone();

        if is_bt || (self.has_capacity() && self.has_queue_capacity(&queue_id)) {
            self.do_resume_task(task_id).await;
            self.drain_queue().await;
        } else {
            rinf::debug_print!(
                "[manager] queuing resume for task {} (active={}, max={}, queue={})",
                task_id,
                self.active_tokens.len(),
                self.max_concurrent,
                queue_id
            );
            TaskProgress {
                task_id: task_id.to_string(),
                status: 0,
                downloaded_bytes: task_row.downloaded_bytes,
                total_bytes: task_row.total_bytes,
                speed: 0,
                file_name: task_row.file_name.clone(),
                save_dir: task_row.save_dir.clone(),
                url: task_row.url.clone(),
                error_message: String::new(),
            }
            .send_signal_to_dart();
            self.pending_queue.push_back(QueuedTask {
                task_id: task_id.to_string(),
                url: task_row.url,
                save_dir: task_row.save_dir,
                file_name: task_row.file_name,
                segments: 0,
                is_resume: true,
                cookies: String::new(),
                referrer: String::new(),
                hint_file_size: 0,
                torrent_file_bytes: Vec::new(),
                proxy_url: task_row.proxy_url,
                user_agent: String::new(),
                queue_id: task_row.queue_id,
                checksum: task_row.checksum,
            });
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
        for (_tid, (token, _gen)) in self.active_tokens.drain() {
            token.cancel();
        }
        self.bt_task_ids.clear();
        self.pending_queue.clear();

        // Shut down the BT session on a dedicated thread to avoid deadlock.
        // `SharedBtSession::shutdown()` calls `runtime.block_on()`, which
        // panics if called from within a tokio runtime context.  Spawning a
        // std thread guarantees we are outside any runtime.
        if let Some(bt) = self.bt_session.take() {
            std::thread::spawn(move || {
                match Arc::try_unwrap(bt) {
                    Ok(owned) => owned.shutdown(),
                    Err(shared) => shared.shutdown(),
                }
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
            Ok(queues) => AllQueues { queues }.send_signal_to_dart(),
            Err(e) => rinf::debug_print!("[manager] load_all_queues error: {}", e),
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
                rinf::debug_print!("[manager] queue_count error: {}", e);
                0
            }
        };
        if let Err(e) = self
            .db
            .insert_queue(&id, &name, speed_limit_kbps, max_concurrent, &default_save_dir, position, default_segments, &default_user_agent)
            .await
        {
            rinf::debug_print!("[manager] insert_queue error: {}", e);
            return;
        }
        // Sync in-memory cache.
        self.queues.insert(id.clone(), QueueInfo {
            queue_id: id.clone(),
            name: name.clone(),
            speed_limit_kbps,
            max_concurrent,
            default_save_dir,
            position,
            default_segments,
            default_user_agent,
        });
        rinf::debug_print!("[manager] created queue: id={}, name={}", id, name);
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
        if let Err(e) = self
            .db
            .update_queue(&queue_id, &name, speed_limit_kbps, max_concurrent, &default_save_dir, default_segments, &default_user_agent)
            .await
        {
            rinf::debug_print!("[manager] update_queue error: {}", e);
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
        rinf::debug_print!("[manager] updated queue: {}", queue_id);
        self.send_all_queues().await;
    }

    /// Delete a named queue (tasks move to default queue) and broadcast.
    pub async fn delete_queue(&mut self, queue_id: String) {
        if let Err(e) = self.db.delete_queue(&queue_id).await {
            rinf::debug_print!("[manager] delete_queue error: {}", e);
            return;
        }
        // Sync in-memory cache.
        self.queues.remove(&queue_id);
        self.queue_limiters.remove(&queue_id);
        rinf::debug_print!("[manager] deleted queue: {}", queue_id);
        self.send_all_queues().await;
    }

    /// Move a task to a different queue and broadcast the updated queue list.
    pub async fn move_task_to_queue(&mut self, task_id: String, queue_id: String) {
        if let Err(e) = self.db.move_task_to_queue(&task_id, &queue_id).await {
            rinf::debug_print!("[manager] move_task_to_queue error: {}", e);
            return;
        }
        // If the task is currently active, update its tracked queue.
        // Note: the existing speed limiter runs to completion; the new
        // queue limiter takes effect on next resume.
        if self.active_task_queue.contains_key(&task_id) {
            self.active_task_queue.insert(task_id.clone(), queue_id.clone());
        }
        rinf::debug_print!("[manager] moved task {} to queue '{}'", task_id, queue_id);
        self.send_all_queues().await;
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
            .map(|pos| { self.pending_queue.remove(pos); true })
            .unwrap_or(false);

        // Step 2: Auto-pause all currently active tasks (except the target itself,
        // which may already be downloading).
        // Note: each pause_task() call invokes drain_queue(), which could promote a
        // queued task to active.  We collect active IDs first, then pause them.
        let active_ids: Vec<String> = self
            .active_tokens
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
            .active_tokens
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
        if !self.active_tokens.contains_key(&task_id) {
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
        if !self.active_tokens.contains_key(&task_id) {
            rinf::debug_print!(
                "[manager] boost: target task {} failed to start — cancelling boost mode",
                task_id
            );
            self.clear_priority().await;
            return;
        }

        rinf::debug_print!(
            "[manager] boost mode: priority={}, auto_paused={}",
            task_id,
            self.auto_paused_ids.len()
        );

        PriorityTaskChanged {
            priority_task_id: task_id,
            auto_paused_count: self.auto_paused_ids.len() as i32,
        }
        .send_signal_to_dart();
    }

    /// Cancel boost mode and resume all auto-paused tasks.
    async fn clear_priority(&mut self) {
        self.priority_task_id = None;
        let to_resume: Vec<String> = self.auto_paused_ids.drain().collect();
        rinf::debug_print!("[manager] boost cancelled, resuming {} tasks", to_resume.len());
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
                rinf::debug_print!("[manager] clear_priority: skipping completed task {}", id);
                continue;
            }
            self.resume_task(id).await;
        }
        // 在发出 PriorityTaskChanged 之前广播最新队列位置。
        // resume_task 对于无空余槽的任务只是将其入队，不会主动广播。
        // 此次广播确保 Dart 在收到 PriorityTaskChanged 时已知道哪些任务在队列中
        // （queuePosition > 0），使 pauseAll 能正确识别并暂停它们。
        self.broadcast_queue_positions();
        PriorityTaskChanged {
            priority_task_id: String::new(),
            auto_paused_count: 0,
        }
        .send_signal_to_dart();
    }
}

/// EMA smoothing factor.  α = 0.15 gives ~85 % weight to history.
/// With updates every ~200 ms this means ~6 ticks (≈ 1.2 s) to converge,
/// producing a visually smooth speed display.
const EMA_ALPHA: f64 = 0.15;

/// Minimum interval between forwarding progress to Dart (per task) to avoid
/// flooding the signal channel when many segments report simultaneously.
const MIN_DART_INTERVAL_MS: u128 = 500;

pub async fn progress_reporter(mut rx: mpsc::Receiver<ProgressUpdate>, db: Db) {
    let mut states: HashMap<String, TaskSpeedState> = HashMap::new();
    // Track last time we sent a signal to Dart per task (rate limiting).
    let mut last_dart_send: HashMap<String, std::time::Instant> = HashMap::new();
    // Track last DB persistence per task (independent of Dart updates).
    let mut last_db_save: HashMap<String, std::time::Instant> = HashMap::new();

    while let Some(update) = rx.recv().await {
        let now = std::time::Instant::now();

        // Latch file_name: once we get a non-empty name, remember it.
        let state = states.entry(update.task_id.clone()).or_insert_with(|| {
            TaskSpeedState {
                ema_speed: 0.0,
                last_downloaded: update.downloaded_bytes,
                last_time: now,
                file_name: String::new(),
                cached_segments: None,
                last_sent_status: -1, // never sent yet
                last_raw_status: update.status,
                speed_warmup_remaining: if update.status == 1 { 1 } else { 0 },
            }
        });

        if !update.file_name.is_empty() {
            state.file_name = update.file_name.clone();
        }

        // Always cache the latest segment snapshot, regardless of rate-limiting.
        if update.segment_details.is_some() {
            state.cached_segments = update.segment_details.clone();
        }

        // Compute EMA speed from downloaded delta.
        //
        // Resume / status-transition handling:
        // - Entering downloading (5/2 -> 1) may carry baseline jumps.
        // - Some sources send an initial status=1 with downloaded=0, then
        //   quickly jump to resumed bytes on the next update.
        // We therefore apply a short warmup window and skip speed calc until
        // baseline is stable, preventing large transient spikes.
        let entered_downloading = update.status == 1 && state.last_raw_status != 1;
        if entered_downloading {
            state.ema_speed = 0.0;
            state.speed_warmup_remaining = if update.downloaded_bytes > 0 { 1 } else { 2 };
        }

        let dt = now.duration_since(state.last_time).as_secs_f64();
        if update.status == 1 {
            let delta_i64 = update.downloaded_bytes - state.last_downloaded;
            if delta_i64 < 0 {
                // Non-monotonic fallback: reset baseline and hold one sample.
                state.ema_speed = 0.0;
                state.speed_warmup_remaining = 1;
            } else if state.speed_warmup_remaining > 0 {
                state.speed_warmup_remaining -= 1;
            } else if dt > 0.01 && delta_i64 > 0 {
                // Only update EMA when there is actual progress.  When delta == 0
                // (no new bytes in this tick), hold the last known speed instead
                // of decaying towards zero.  This prevents ETA from ballooning
                // near completion when segments finish and no new data arrives
                // while the task is still in downloading state.
                let instant_speed = delta_i64 as f64 / dt;
                state.ema_speed =
                    EMA_ALPHA * instant_speed + (1.0 - EMA_ALPHA) * state.ema_speed;
            }
        } else {
            state.ema_speed = 0.0;
            state.speed_warmup_remaining = 0;
        }
        state.last_downloaded = update.downloaded_bytes;
        state.last_time = now;
        state.last_raw_status = update.status;

        let smoothed_speed = state.ema_speed as i64;
        let resolved_name = state.file_name.clone();

        // For terminal states (completed / error / paused) always send immediately.
        // For downloading (status=1) and preparing (status=5), rate-limit to avoid flooding Dart.
        let is_terminal = update.status != 1 && update.status != 5;
        // Status transitions (e.g. preparing→downloading) must also be sent
        // immediately so the UI never skips an intermediate state.
        let is_status_change = update.status != state.last_sent_status;
        let should_send = is_terminal || is_status_change || {
            let last = last_dart_send.get(&update.task_id);
            last.is_none() || now.duration_since(*last.unwrap_or(&now)).as_millis() >= MIN_DART_INTERVAL_MS
        };

        // Always send if this update carries a newly resolved file_name.
        let has_new_name = !update.file_name.is_empty();

        if should_send || has_new_name {
            // Terminal states (completed / error / paused) should report zero
            // speed so the UI doesn't show a stale EMA value.
            let report_speed = if is_terminal { 0 } else { smoothed_speed };
            TaskProgress {
                task_id: update.task_id.clone(),
                status: update.status,
                downloaded_bytes: update.downloaded_bytes,
                total_bytes: update.total_bytes,
                speed: report_speed,
                file_name: resolved_name,
                save_dir: String::new(),
                url: String::new(),
                error_message: update.error_message.clone(),
            }
            .send_signal_to_dart();

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

                rinf::debug_print!(
                    "[seg-vis] sending SegmentProgress for task {}, {} segments, total_bytes={}",
                    update.task_id,
                    segs.len(),
                    update.total_bytes
                );
                SegmentProgress {
                    task_id: update.task_id.clone(),
                    total_bytes: update.total_bytes,
                    segment_count: segs.len() as i32,
                    segments: final_segs,
                }
                .send_signal_to_dart();
            } else {
                rinf::debug_print!(
                    "[seg-vis] NO cached segments for task {}, segment_details in update: {}",
                    update.task_id,
                    update.segment_details.is_some()
                );
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
            let task_last_save = last_db_save
                .entry(update.task_id.clone())
                .or_insert(now);
            if task_last_save.elapsed().as_secs() >= downloader::DB_SAVE_INTERVAL_SECS {
                let db_clone = db.clone();
                let tid = update.task_id.clone();
                let dl = update.downloaded_bytes;
                tokio::spawn(async move {
                    let _ = db_clone.update_task_progress(&tid, dl).await;
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
                let _ = db
                    .update_task_progress(&update.task_id, update.downloaded_bytes)
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
        if update.status == 2 || update.status == 3 || update.status == 4 {
            states.remove(&update.task_id);
            last_dart_send.remove(&update.task_id);
            last_db_save.remove(&update.task_id);
        }
    }
}
