use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::FutureExt;
use reqwest::Client;
use rinf::RustSignal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bt_downloader::{self, BtConfig, BtDownloadParams, SharedBtSession, TorrentSource};
use crate::db::Db;
use crate::downloader::{self, DownloadParams, ProgressUpdate, SegmentProgressInfo};
use crate::ftp_downloader;
use crate::proxy_config::ProxyConfig;
use crate::signals::{AllTasks, SegmentDetail, SegmentProgress, TaskProgress};
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
    /// Raw .torrent file bytes (empty for magnet/HTTP/FTP tasks).
    torrent_file_bytes: Vec<u8>,
    /// Per-task proxy URL override (e.g. "socks5://user:pass@host:port").
    /// Empty = use global proxy setting.
    proxy_url: String,
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
}

impl DownloadManager {
    pub fn new(
        db: Db,
        max_concurrent: usize,
        speed_limit_bps: u64,
        default_save_dir: String,
        app_data_dir: String,
        bt_config: BtConfig,
        proxy_config: ProxyConfig,
    ) -> Result<Self, downloader::DownloadError> {
        let client = downloader::build_client(&proxy_config)?;
        let (tx, rx) = mpsc::channel(256);
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
            default_save_dir,
            app_data_dir,
            bt_config,
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
        let new_client = downloader::build_client(&config)?;
        self.client = new_client;
        self.proxy_config = config;
        Ok(())
    }

    /// Get a reference to the current proxy configuration.
    #[allow(dead_code)]
    pub fn proxy_config(&self) -> &ProxyConfig {
        &self.proxy_config
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

    /// Try to start tasks from the pending queue until we run out of capacity.
    async fn drain_queue(&mut self) {
        while self.has_capacity() {
            let Some(queued) = self.pending_queue.front() else {
                break;
            };
            // Check if task is already running (edge case: resume while queued).
            if self.active_tokens.contains_key(&queued.task_id) {
                self.pending_queue.pop_front();
                continue;
            }
            let Some(queued) = self.pending_queue.pop_front() else {
                break;
            };
            if queued.is_resume {
                self.do_resume_task(&queued.task_id).await;
            } else {
                self.do_start_task(queued).await;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Public task operations
    // -----------------------------------------------------------------------

    /// Remove a finished task from active_tokens (called by actor loop).
    /// Only removes the entry if the generation matches, preventing a stale
    /// `TaskDone` from an old spawn from accidentally removing a newer token.
    pub async fn on_task_done(&mut self, task_id: &str, generation: u64) {
        if let Some((_, stored_gen)) = self.active_tokens.get(task_id)
            && *stored_gen == generation {
                self.active_tokens.remove(task_id);
                self.active_handles.remove(task_id);
                self.bt_task_ids.remove(task_id);
            }
        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
        self.maybe_wal_checkpoint().await;
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

    pub async fn load_and_send_all_tasks(&self) {
        // 启动时将残留的 downloading/pending 状态矫正为 paused
        // 因为重启后没有活跃的下载线程
        if let Err(e) = self.db.reset_incomplete_tasks_to_paused().await {
            rinf::debug_print!("reset_incomplete_tasks_to_paused error: {}", e);
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
        torrent_file_bytes: Vec<u8>,
        proxy_url: String,
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
            .insert_task(&task_id, &db_url, &file_name, &save_dir, seg, 0, &proxy_url)
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
            torrent_file_bytes,
            proxy_url,
        };
        if is_bt || self.has_capacity() {
            self.do_start_task(queued).await;
            // If do_start_task failed early (e.g. BT session init), the slot
            // was freed — drain the queue so pending tasks can proceed.
            self.drain_queue().await;
        } else {
            rinf::debug_print!(
                "[manager] queuing task {} (active={}, max={})",
                queued.task_id,
                self.active_tokens.len(),
                self.max_concurrent
            );
            self.pending_queue.push_back(queued);
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
            torrent_file_bytes,
            proxy_url,
        } = queued;
        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();
        self.active_tokens
            .insert(task_id.clone(), (cancel_token.clone(), spawn_gen));

        let use_ftp = is_ftp_url(&url);
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
            // Resolve proxy: per-task proxy_url overrides global config.
            // `.resolve()` expands System mode into a concrete Manual config
            // so that FTP downloader (which reads host/port directly) works.
            let (task_client, task_proxy) = if proxy_url.is_empty() {
                (self.client.clone(), self.proxy_config.resolve())
            } else {
                let pc = ProxyConfig::from_proxy_url(&proxy_url);
                match downloader::build_client(&pc) {
                    Ok(c) => (c, pc),
                    Err(e) => {
                        rinf::debug_print!(
                            "[manager] failed to build per-task proxy client: {}",
                            e
                        );
                        // Fallback to global
                        (self.client.clone(), self.proxy_config.resolve())
                    }
                }
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
                speed_limiter: self.speed_limiter.clone(),
                cookies,
                proxy_config: task_proxy,
            };

            tokio::spawn(async move {
                let result = if use_ftp {
                    std::panic::AssertUnwindSafe(ftp_downloader::run_ftp_download(params))
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
        }
    }

    pub async fn resume_task(&mut self, task_id: &str) {
        if self.active_tokens.contains_key(task_id) {
            return; // already running
        }

        // Also check if already in the pending queue.
        if self.pending_queue.iter().any(|q| q.task_id == task_id) {
            return;
        }

        // BT tasks bypass the HTTP/FTP concurrency queue.
        let is_bt = self.db.load_task_by_id(task_id).await
            .ok()
            .flatten()
            .map(|t| is_bt_url(&t.url))
            .unwrap_or(false);

        if is_bt || self.has_capacity() {
            self.do_resume_task(task_id).await;
            // If do_resume_task failed early (e.g. BT session init), drain
            // the queue so pending tasks can proceed.
            self.drain_queue().await;
        } else {
            rinf::debug_print!(
                "[manager] queuing resume for task {} (active={}, max={})",
                task_id,
                self.active_tokens.len(),
                self.max_concurrent
            );
            // Load task info for the queue entry.
            if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
                self.pending_queue.push_back(QueuedTask {
                    task_id: task_id.to_string(),
                    url: t.url,
                    save_dir: t.save_dir,
                    file_name: t.file_name,
                    segments: 0, // not used for resume
                    is_resume: true,
                    cookies: String::new(), // cookies not available for resume from DB
                    torrent_file_bytes: Vec::new(), // loaded from DB in do_resume_task
                    proxy_url: t.proxy_url,
                });
            }
        }
    }

    /// Internal: actually spawn the resume (no concurrency check).
    async fn do_resume_task(&mut self, task_id: &str) {
        let task = match self.db.load_task_by_id(task_id).await {
            Ok(Some(t)) => t,
            _ => return,
        };

        // Read actual segment count from DB.  0 means "auto" — the downloader
        // will dynamically calculate the optimal count.
        let seg_count: i32 = self.db.get_task_segments(task_id).await.unwrap_or_default();

        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();
        self.active_tokens
            .insert(task_id.to_string(), (cancel_token.clone(), spawn_gen));

        let use_ftp = is_ftp_url(&task.url);
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
            // Resolve proxy: per-task proxy_url overrides global config.
            // `.resolve()` expands System mode into a concrete Manual config
            // so that FTP downloader (which reads host/port directly) works.
            let (task_client, task_proxy) = if task.proxy_url.is_empty() {
                (self.client.clone(), self.proxy_config.resolve())
            } else {
                let pc = ProxyConfig::from_proxy_url(&task.proxy_url);
                match downloader::build_client(&pc) {
                    Ok(c) => (c, pc),
                    Err(e) => {
                        rinf::debug_print!(
                            "[manager] failed to build per-task proxy client on resume: {}",
                            e
                        );
                        (self.client.clone(), self.proxy_config.resolve())
                    }
                }
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
                speed_limiter: self.speed_limiter.clone(),
                cookies: String::new(),
                proxy_config: task_proxy,
            };

            tokio::spawn(async move {
                let result = if use_ftp {
                    std::panic::AssertUnwindSafe(ftp_downloader::run_ftp_download(params))
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
                    bt.delete_task(task_id, delete_files).await;
                }
                // Fallback: if librqbit didn't delete files (e.g. session
                // wasn't initialised), clean up manually.
                if delete_files && path.exists() {
                    if path.is_dir() {
                        let _ = tokio::fs::remove_dir_all(&path).await;
                    } else {
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
            } else {
                // HTTP / FTP: always clean up the in-progress temp file
                let temp_path =
                    PathBuf::from(format!("{}{}", path.display(), downloader::TEMP_EXT));
                let _ = tokio::fs::remove_file(&temp_path).await;

                if delete_files {
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

        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
        self.maybe_wal_checkpoint().await;
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
            }
        });

        if !update.file_name.is_empty() {
            state.file_name = update.file_name.clone();
        }

        // Always cache the latest segment snapshot, regardless of rate-limiting.
        if update.segment_details.is_some() {
            state.cached_segments = update.segment_details.clone();
        }

        // Compute EMA speed from downloaded delta (works correctly with
        // multi-segment: each segment adds to total_downloaded atomically).
        let dt = now.duration_since(state.last_time).as_secs_f64();
        if dt > 0.01 && update.status == 1 {
            let delta = (update.downloaded_bytes - state.last_downloaded).max(0) as f64;
            let instant_speed = delta / dt;
            state.ema_speed = EMA_ALPHA * instant_speed + (1.0 - EMA_ALPHA) * state.ema_speed;
        }
        state.last_downloaded = update.downloaded_bytes;
        state.last_time = now;

        let smoothed_speed = state.ema_speed as i64;
        let resolved_name = state.file_name.clone();

        // For terminal states (completed / error / paused) always send immediately.
        // For downloading (status=1) and preparing (status=5), rate-limit to avoid flooding Dart.
        let is_terminal = update.status != 1 && update.status != 5;
        let should_send = is_terminal || {
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

        // When a task completes, persist final downloaded_bytes to DB so that
        // subsequent app restarts load the correct 100% progress value.
        // Completion writes are awaited (not fire-and-forget) to guarantee
        // the final value is persisted before we clean up state.
        if update.status == 3 && update.total_bytes > 0 {
            let _ = db
                .update_task_progress(&update.task_id, update.total_bytes)
                .await;
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
