use std::collections::HashMap;
use std::path::PathBuf;

use futures_util::FutureExt;
use reqwest::Client;
use rinf::RustSignal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::Db;
use crate::downloader::{self, DownloadParams, ProgressUpdate, SegmentProgressInfo};
use crate::ftp_downloader;
use crate::signals::{AllTasks, SegmentDetail, SegmentProgress, TaskProgress};
use crate::speed_limiter::SpeedLimiter;

/// Determine if a URL uses the FTP protocol (case-insensitive).
fn is_ftp_url(url: &str) -> bool {
    url.len() >= 6 && url[..6].eq_ignore_ascii_case("ftp://")
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
}

pub struct DownloadManager {
    db: Db,
    client: Client,
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
    /// Maximum number of concurrent active downloads.  0 = unlimited.
    max_concurrent: usize,
    /// FIFO queue of tasks waiting for a free slot.
    pending_queue: Vec<QueuedTask>,
    /// Global speed limiter shared with all download tasks.
    speed_limiter: SpeedLimiter,
}

impl DownloadManager {
    pub fn new(db: Db, max_concurrent: usize, speed_limit_bps: u64) -> Result<Self, downloader::DownloadError> {
        let client = downloader::build_client()?;
        let (tx, rx) = mpsc::channel(256);
        let (done_tx, done_rx) = mpsc::channel(64);
        let limiter = SpeedLimiter::new(speed_limit_bps);
        limiter.spawn_refill_task();
        Ok(Self {
            db,
            client,
            active_tokens: HashMap::new(),
            active_handles: HashMap::new(),
            generation: 0,
            progress_tx: tx,
            progress_rx: Some(rx),
            done_tx,
            done_rx: Some(done_rx),
            max_concurrent,
            pending_queue: Vec::new(),
            speed_limiter: limiter,
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

    /// Update global speed limit (bytes/sec).  Takes effect immediately on
    /// all active and future downloads.  0 = unlimited.
    pub fn set_speed_limit(&mut self, bps: u64) {
        self.speed_limiter.set_limit(bps);
    }

    // -----------------------------------------------------------------------
    // Concurrency helpers
    // -----------------------------------------------------------------------

    /// Whether we have a free slot for a new download.
    fn has_capacity(&self) -> bool {
        self.max_concurrent == 0 || self.active_tokens.len() < self.max_concurrent
    }

    /// Try to start tasks from the pending queue until we run out of capacity.
    async fn drain_queue(&mut self) {
        while self.has_capacity() {
            let Some(queued) = self.pending_queue.first() else {
                break;
            };
            // Check if task is already running (edge case: resume while queued).
            if self.active_tokens.contains_key(&queued.task_id) {
                self.pending_queue.remove(0);
                continue;
            }
            let queued = self.pending_queue.remove(0);
            if queued.is_resume {
                self.do_resume_task(&queued.task_id).await;
            } else {
                self.do_start_task(
                    queued.task_id,
                    queued.url,
                    queued.save_dir,
                    queued.file_name,
                    queued.segments,
                    queued.cookies,
                )
                .await;
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
            }
        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
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

    pub async fn create_task(
        &mut self,
        url: String,
        save_dir: String,
        file_name: String,
        segments: i32,
        cookies: String,
    ) {
        let task_id = Uuid::new_v4().to_string();
        // When segments <= 0 ("auto"), store 0 in DB and let the downloader
        // dynamically calculate the optimal count after probing file size,
        // CPU cores, and bandwidth.
        let seg = if segments <= 0 { 0 } else { segments };

        if let Err(e) = self
            .db
            .insert_task(&task_id, &url, &file_name, &save_dir, seg, 0)
            .await
        {
            rinf::debug_print!("insert_task error: {}", e);
            return;
        }

        TaskProgress {
            task_id: task_id.clone(),
            status: 0,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            file_name: file_name.clone(),
            save_dir: save_dir.clone(),
            url: url.clone(),
            error_message: String::new(),
        }
        .send_signal_to_dart();

        // Check concurrency limit before starting.
        if self.has_capacity() {
            self.do_start_task(task_id, url, save_dir, file_name, seg, cookies)
                .await;
        } else {
            rinf::debug_print!(
                "[manager] queuing task {} (active={}, max={})",
                task_id,
                self.active_tokens.len(),
                self.max_concurrent
            );
            self.pending_queue.push(QueuedTask {
                task_id,
                url,
                save_dir,
                file_name,
                segments: seg,
                is_resume: false,
                cookies,
            });
        }
    }

    /// Internal: actually spawn the download task (no concurrency check).
    async fn do_start_task(
        &mut self,
        task_id: String,
        url: String,
        save_dir: String,
        file_name: String,
        segments: i32,
        cookies: String,
    ) {
        self.generation += 1;
        let spawn_gen = self.generation;
        let cancel_token = CancellationToken::new();
        self.active_tokens
            .insert(task_id.clone(), (cancel_token.clone(), spawn_gen));

        let use_ftp = is_ftp_url(&url);

        let params = DownloadParams {
            task_id: task_id.clone(),
            url,
            save_dir,
            file_name,
            segment_count: segments,
            is_resume: false,
            db: self.db.clone(),
            client: self.client.clone(),
            progress_tx: self.progress_tx.clone(),
            cancel_token,
            speed_limiter: self.speed_limiter.clone(),
            cookies,
        };

        let done_tx = self.done_tx.clone();
        let panic_progress_tx = self.progress_tx.clone();
        let panic_task_id = task_id.clone();
        let panic_db = self.db.clone();

        let handle = tokio::spawn(async move {
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
                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "internal panic".to_string()
                };
                rinf::debug_print!("[download] PANIC in task {}: {}", panic_task_id, msg);
                let _ = panic_db.update_task_status(&panic_task_id, 4, &msg).await;
                let _ = panic_progress_tx
                    .send(ProgressUpdate {
                        task_id: panic_task_id.clone(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: 4,
                        error_message: msg,
                        file_name: String::new(),
                        segment_details: None,
                    })
                    .await;
            }

            let _ = done_tx.send(TaskDone { task_id: panic_task_id, generation: spawn_gen }).await;
        });
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

        // Check concurrency limit before starting.
        if self.has_capacity() {
            self.do_resume_task(task_id).await;
        } else {
            rinf::debug_print!(
                "[manager] queuing resume for task {} (active={}, max={})",
                task_id,
                self.active_tokens.len(),
                self.max_concurrent
            );
            // Load task info for the queue entry.
            if let Ok(Some(t)) = self.db.load_task_by_id(task_id).await {
                self.pending_queue.push(QueuedTask {
                    task_id: task_id.to_string(),
                    url: t.url,
                    save_dir: t.save_dir,
                    file_name: t.file_name,
                    segments: 0, // not used for resume
                    is_resume: true,
                    cookies: String::new(), // cookies not available for resume from DB
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

        let tid = task_id.to_string();
        let params = DownloadParams {
            task_id: tid.clone(),
            url: task.url,
            save_dir: task.save_dir,
            file_name: task.file_name,
            segment_count: seg_count,
            is_resume: true,
            db: self.db.clone(),
            client: self.client.clone(),
            progress_tx: self.progress_tx.clone(),
            cancel_token,
            speed_limiter: self.speed_limiter.clone(),
            cookies: String::new(), // cookies not available for resume from DB
        };

        let done_tx = self.done_tx.clone();
        let panic_progress_tx = self.progress_tx.clone();
        let panic_task_id = tid.clone();
        let panic_db = self.db.clone();

        let handle = tokio::spawn(async move {
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
                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "internal panic".to_string()
                };
                rinf::debug_print!("[download] PANIC in task {}: {}", panic_task_id, msg);
                let _ = panic_db.update_task_status(&panic_task_id, 4, &msg).await;
                let _ = panic_progress_tx
                    .send(ProgressUpdate {
                        task_id: panic_task_id.clone(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: 4,
                        error_message: msg,
                        file_name: String::new(),
                        segment_details: None,
                    })
                    .await;
            }

            let _ = done_tx.send(TaskDone { task_id: panic_task_id, generation: spawn_gen }).await;
        });
        self.active_handles.insert(tid, handle);
    }

    pub async fn cancel_task(&mut self, task_id: &str) {
        // Remove from pending queue if queued.
        if let Some(pos) = self.pending_queue.iter().position(|q| q.task_id == task_id) {
            self.pending_queue.remove(pos);
        }

        if let Some((token, _gen)) = self.active_tokens.remove(task_id) {
            token.cancel();
        }
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
            // Always clean up the in-progress temp file (.fdownloading)
            let temp_path = PathBuf::from(format!("{}{}", path.display(), downloader::TEMP_EXT));
            let _ = tokio::fs::remove_file(&temp_path).await;

            if delete_files {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }

        let _ = self.db.delete_task(task_id).await;

        // A slot freed up — try to start queued tasks.
        self.drain_queue().await;
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
            TaskProgress {
                task_id: update.task_id.clone(),
                status: update.status,
                downloaded_bytes: update.downloaded_bytes,
                total_bytes: update.total_bytes,
                speed: smoothed_speed,
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
        if update.status == 1 {
            let task_last_save = last_db_save
                .entry(update.task_id.clone())
                .or_insert(now);
            if task_last_save.elapsed().as_secs() >= downloader::DB_SAVE_INTERVAL_SECS {
                let _ = db
                    .update_task_progress(&update.task_id, update.downloaded_bytes)
                    .await;
                *task_last_save = now;
            }
        }

        // When a task completes, persist final downloaded_bytes to DB so that
        // subsequent app restarts load the correct 100% progress value.
        if update.status == 3 && update.total_bytes > 0 {
            let _ = db
                .update_task_progress(&update.task_id, update.total_bytes)
                .await;
        }

        // Clean up finished tasks.
        if update.status == 3 || update.status == 4 {
            states.remove(&update.task_id);
            last_dart_send.remove(&update.task_id);
            last_db_save.remove(&update.task_id);
        }
    }
}
