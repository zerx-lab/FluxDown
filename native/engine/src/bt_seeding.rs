//! BitTorrent seeding lifecycle.
//!
//! 完成的 torrent 保留 librqbit 句柄继续做种。活动做种数受
//! `seed_max_active` 上限约束（0 = 不限制）：超出上限的完成任务进入
//! FIFO 做种队列（librqbit 侧暂停，不上传），有槽位释放时按序激活；
//! 上限热更新后由周期性 `reconcile` 升/降级补齐差额。
//!
//! 做种时长跨暂停/重启**累计**：每个做种者以落库的累计秒数为基线
//! （`seed_time_base_secs`），叠加本次激活以来的墙钟时长；排队/暂停
//! 期间不计时。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::bt_downloader::BtHandle;
use crate::logger::log_info;

/// Numeric code indicating an active seeder (not a stop reason).
pub const SEEDING_STATUS_ACTIVE: i32 = 1;

/// Numeric code indicating a completed torrent waiting for a free seeding
/// slot (`seed_max_active` reached). Not a stop reason.
pub const SEEDING_STATUS_QUEUED: i32 = 8;

/// Auxiliary message persisted alongside [`SEEDING_STATUS_QUEUED`].
pub const SEEDING_QUEUED_MESSAGE: &str = "queued for seeding";

/// Interval between periodic evaluations of BT seeding ratio/time limits.
pub const SEEDING_EVAL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Reason why a seeding entry was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedingStopReason {
    /// Still seeding, not stopped.
    None,
    /// Upload-to-download ratio limit reached.
    RatioReached,
    /// Seeding time limit reached.
    TimeReached,
    /// Inactive seeding time limit reached (zero upload speed).
    InactiveTimeReached,
    /// Explicitly stopped by the user.
    UserStopped,
    /// Underlying task was deleted.
    TaskDeleted,
    /// Whole BT session was released.
    SessionReleased,
}

impl SeedingStopReason {
    /// Numeric code used for persistence / FFI.
    pub fn as_i32(self) -> i32 {
        match self {
            Self::None => 0,
            Self::RatioReached => 2,
            Self::TimeReached => 3,
            Self::UserStopped => 4,
            Self::TaskDeleted => 5,
            Self::SessionReleased => 6,
            Self::InactiveTimeReached => 7,
        }
    }

    /// Human-readable stop reason.
    pub fn message(self) -> &'static str {
        match self {
            Self::None => "",
            Self::RatioReached => "seed ratio reached",
            Self::TimeReached => "seed time reached",
            Self::InactiveTimeReached => "seed inactive time reached",
            Self::UserStopped => "stopped by user",
            Self::TaskDeleted => "task deleted",
            Self::SessionReleased => "BT session released",
        }
    }
}

/// Logical operator used to combine multiple seeding limit conditions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SeedingLimitOperator {
    /// Stop seeding only when all enabled conditions are reached.
    And,
    /// Stop seeding when any enabled condition is reached.
    #[default]
    Or,
}

/// What to do once a seeding limit is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeedingThenAction {
    /// Stop seeding and keep the completed task (default).
    #[default]
    Stop,
    /// Delete the task but keep the downloaded file(s).
    DeleteTask,
    /// Delete the task and remove the downloaded file(s).
    DeleteTaskAndFiles,
}

impl SeedingThenAction {
    /// Parse the persisted setting value. Unknown values fall back to [`Stop`].
    pub fn parse(value: &str) -> Self {
        match value {
            "delete" => Self::DeleteTask,
            "delete_files" => Self::DeleteTaskAndFiles,
            _ => Self::Stop,
        }
    }

    /// Persisted setting value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::DeleteTask => "delete",
            Self::DeleteTaskAndFiles => "delete_files",
        }
    }
}

/// Configuration for when a seeding torrent should be stopped.
///
/// A limit value of `0` disables that condition. When no conditions are
/// enabled, seeding never stops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeedingLimitConfig {
    /// Total upload-to-download ratio threshold (`uploaded / downloaded`).
    /// `0.0` disables the ratio limit.
    pub ratio_limit: f64,
    /// Post-completion upload-to-download ratio threshold
    /// (`(uploaded - uploaded_at_completion) / downloaded`). `0.0` disables.
    pub post_ratio_limit: f64,
    /// Maximum cumulative seeding time, in minutes. `0` disables the limit.
    pub seed_time_limit_minutes: u64,
    /// Maximum time allowed with zero upload speed, in minutes. `0` disables.
    pub inactive_time_limit_minutes: u64,
    /// How to combine the enabled conditions.
    pub operator: SeedingLimitOperator,
    /// What to do once a limit is reached.
    pub then_action: SeedingThenAction,
}

impl SeedingLimitConfig {
    /// Returns `true` if at least one limit condition is enabled.
    pub fn has_enabled_conditions(&self) -> bool {
        self.ratio_limit > 0.0
            || self.post_ratio_limit > 0.0
            || self.seed_time_limit_minutes > 0
            || self.inactive_time_limit_minutes > 0
    }
}

impl Default for SeedingLimitConfig {
    /// 默认所有限制均禁用：完成的任务持续做种，直到用户手动停止。
    fn default() -> Self {
        Self {
            ratio_limit: 0.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::Or,
            then_action: SeedingThenAction::Stop,
        }
    }
}

/// 任务级做种限制覆盖的哨兵：跟随全局配置。
pub const SEED_LIMIT_INHERIT: i64 = -2;
/// 任务级做种限制覆盖的哨兵：不限制（禁用该条件）。
pub const SEED_LIMIT_UNLIMITED: i64 = -1;

/// Per-task overrides for the global seeding limits.
///
/// Sentinel semantics per field: `-2` = inherit the global value, `-1` =
/// unlimited (condition disabled), `>= 0` = custom value (`0` behaves as
/// unlimited because the engine treats zero limits as disabled). Ratio
/// values are stored in thousandths (`1500` = ratio 1.5) so the persisted
/// representation stays integral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedLimitOverrides {
    pub ratio_limit_milli: i64,
    pub post_ratio_limit_milli: i64,
    pub seed_time_limit_minutes: i64,
    pub inactive_time_limit_minutes: i64,
}

impl Default for SeedLimitOverrides {
    /// 默认全部跟随全局。
    fn default() -> Self {
        Self {
            ratio_limit_milli: SEED_LIMIT_INHERIT,
            post_ratio_limit_milli: SEED_LIMIT_INHERIT,
            seed_time_limit_minutes: SEED_LIMIT_INHERIT,
            inactive_time_limit_minutes: SEED_LIMIT_INHERIT,
        }
    }
}

impl SeedLimitOverrides {
    /// Returns `true` when every field inherits the global configuration.
    pub fn is_all_inherit(&self) -> bool {
        *self == Self::default()
    }

    /// Resolve the effective limit config for one task: overrides replace the
    /// matching global values; operator and then-action stay global.
    pub fn apply(&self, global: &SeedingLimitConfig) -> SeedingLimitConfig {
        fn ratio(v: i64, global: f64) -> f64 {
            match v {
                SEED_LIMIT_INHERIT => global,
                v if v <= 0 => 0.0,
                v => v as f64 / 1000.0,
            }
        }
        fn minutes(v: i64, global: u64) -> u64 {
            match v {
                SEED_LIMIT_INHERIT => global,
                v if v <= 0 => 0,
                v => v as u64,
            }
        }
        SeedingLimitConfig {
            ratio_limit: ratio(self.ratio_limit_milli, global.ratio_limit),
            post_ratio_limit: ratio(self.post_ratio_limit_milli, global.post_ratio_limit),
            seed_time_limit_minutes: minutes(
                self.seed_time_limit_minutes,
                global.seed_time_limit_minutes,
            ),
            inactive_time_limit_minutes: minutes(
                self.inactive_time_limit_minutes,
                global.inactive_time_limit_minutes,
            ),
            operator: global.operator,
            then_action: global.then_action,
        }
    }
}

/// One actively seeding torrent.
pub struct SeedingEntry {
    pub handle: BtHandle,
    /// Cumulative seeding seconds persisted before this activation stint.
    pub seed_time_base_secs: i64,
    /// Instant this seeding stint started (activation time). Queued/paused
    /// periods are excluded from the cumulative seeding time.
    pub stint_started: Instant,
    /// Last instant at which the seeder had non-zero upload activity.
    pub last_upload_instant: Instant,
    /// Total uploaded bytes observed at `last_upload_instant`.
    pub last_uploaded_bytes: i64,
    /// Total uploaded bytes when the download completed and seeding started.
    /// Used to compute the post-completion ratio.
    pub uploaded_at_completion: i64,
    /// Session-local counter baseline used to accumulate uploads across
    /// librqbit counter resets (pause/resume or session rebuild).
    pub last_session_uploaded: i64,
    pub stop_reason: SeedingStopReason,
}

impl SeedingEntry {
    /// Cumulative seeding seconds including the current stint.
    fn effective_seed_time_secs(&self, now: Instant) -> i64 {
        self.seed_time_base_secs
            .saturating_add(now.duration_since(self.stint_started).as_secs() as i64)
    }
}

impl std::fmt::Debug for SeedingEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedingEntry")
            .field("seed_time_base_secs", &self.seed_time_base_secs)
            .field("last_upload_instant", &self.last_upload_instant)
            .field("last_uploaded_bytes", &self.last_uploaded_bytes)
            .field("uploaded_at_completion", &self.uploaded_at_completion)
            .field("last_session_uploaded", &self.last_session_uploaded)
            .field("stop_reason", &self.stop_reason)
            .finish_non_exhaustive()
    }
}

/// A completed torrent waiting for a free seeding slot. No session upload
/// baseline is kept: queued torrents are paused, and unpausing always resets
/// librqbit's per-session upload counter, so activation restarts from 0.
struct QueuedSeed {
    handle: BtHandle,
    uploaded_at_completion: i64,
    seed_time_base_secs: i64,
}

/// Outcome of [`SeedingManager::register`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedingRegistration {
    /// Registered and immediately active (a free slot was available).
    Activated,
    /// Registered but queued: `seed_max_active` is reached. The caller must
    /// pause the torrent; a later `reconcile` activates it in FIFO order.
    Queued,
    /// The task was already registered (active or queued).
    AlreadyPresent,
}

/// State snapshot returned by [`SeedingManager::unregister`].
#[derive(Debug, Clone, Copy)]
pub struct UnregisteredSeed {
    /// `true` when the entry was actively seeding (vs waiting in the queue).
    pub was_active: bool,
    /// Final cumulative seeding seconds (base + current stint for active
    /// entries). The caller should persist this value.
    pub seed_time_secs: i64,
}

/// Snapshot of live upload state needed by [`SeedingManager::evaluate_limits`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SeedingUploadSnapshot {
    /// Cumulative uploaded bytes persisted in the database.
    pub total_uploaded: i64,
    /// Total downloaded bytes recorded for the task.
    pub total_downloaded: i64,
    /// Total torrent size in bytes. Used as the ratio divisor fallback when
    /// `total_downloaded` is implausibly small (restored/rechecked data).
    pub total_size: i64,
    /// Current upload speed in bytes per second.
    pub upload_speed_bps: i64,
}

/// Internal collections guarded by one lock: the active map plus the FIFO
/// wait queue. A task id lives in at most one of the two.
#[derive(Default)]
struct SeedingState {
    active: HashMap<String, SeedingEntry>,
    queued: VecDeque<(String, QueuedSeed)>,
}

/// Manages the lifecycle of seeding BT torrents.
pub struct SeedingManager {
    state: Mutex<SeedingState>,
    /// Max simultaneously active seeders. `0` = unlimited.
    cap: AtomicUsize,
}

fn short_id(task_id: &str) -> &str {
    task_id.get(..8).unwrap_or(task_id)
}

impl SeedingManager {
    /// Create an empty seeding manager with no active-seeder cap.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SeedingState::default()),
            cap: AtomicUsize::new(0),
        }
    }

    /// Update the max active seeder cap (`0` = unlimited). Takes effect on
    /// the next `register`/`reconcile`.
    pub fn set_cap(&self, cap: usize) {
        self.cap.store(cap, Ordering::Relaxed);
    }

    /// Current max active seeder cap (`0` = unlimited).
    pub fn cap(&self) -> usize {
        self.cap.load(Ordering::Relaxed)
    }

    fn has_free_slot(&self, active_len: usize) -> bool {
        let cap = self.cap();
        cap == 0 || active_len < cap
    }

    /// Register a completed BT task as a seeder.
    ///
    /// With a free slot the task becomes active immediately; otherwise it is
    /// appended to the wait queue (the caller must pause the torrent).
    /// `seed_time_base_secs` is the persisted cumulative seeding time.
    pub async fn register(
        &self,
        task_id: String,
        handle: BtHandle,
        uploaded_at_completion: i64,
        last_session_uploaded: i64,
        seed_time_base_secs: i64,
    ) -> SeedingRegistration {
        let mut guard = self.state.lock().await;
        if guard.active.contains_key(&task_id) || guard.queued.iter().any(|(id, _)| id == &task_id)
        {
            return SeedingRegistration::AlreadyPresent;
        }
        if self.has_free_slot(guard.active.len()) {
            log_info!(
                "[bt-seeding] task={} registered for seeding (active)",
                short_id(&task_id)
            );
            let now = Instant::now();
            guard.active.insert(
                task_id,
                SeedingEntry {
                    handle,
                    seed_time_base_secs,
                    stint_started: now,
                    last_upload_instant: now,
                    last_uploaded_bytes: uploaded_at_completion,
                    uploaded_at_completion,
                    last_session_uploaded,
                    stop_reason: SeedingStopReason::None,
                },
            );
            SeedingRegistration::Activated
        } else {
            log_info!(
                "[bt-seeding] task={} queued for seeding (cap {} reached)",
                short_id(&task_id),
                self.cap()
            );
            guard.queued.push_back((
                task_id,
                QueuedSeed {
                    handle,
                    uploaded_at_completion,
                    seed_time_base_secs,
                },
            ));
            SeedingRegistration::Queued
        }
    }

    /// Rebalance active seeders against the cap.
    ///
    /// Promotes queued seeds (FIFO) while slots are free and demotes the most
    /// recently activated seeders while over cap (their elapsed stint is
    /// folded into the cumulative base; they re-enter the queue front).
    /// Returns `(activated_ids, demoted)`——demoted 项附带结算后的累计做种
    /// 秒数供调用方落库；调用方还需 unpause/pause torrent 并持久化状态迁移。
    pub async fn reconcile(&self) -> (Vec<String>, Vec<(String, i64)>) {
        let mut guard = self.state.lock().await;
        let cap = self.cap();
        let mut activated = Vec::new();
        let mut demoted: Vec<(String, i64)> = Vec::new();

        // Promote while there is room.
        while self.has_free_slot(guard.active.len()) {
            let Some((task_id, queued)) = guard.queued.pop_front() else {
                break;
            };
            let now = Instant::now();
            guard.active.insert(
                task_id.clone(),
                SeedingEntry {
                    handle: queued.handle,
                    seed_time_base_secs: queued.seed_time_base_secs,
                    stint_started: now,
                    last_upload_instant: now,
                    last_uploaded_bytes: queued.uploaded_at_completion,
                    uploaded_at_completion: queued.uploaded_at_completion,
                    // Paused-then-unpaused torrents restart librqbit's upload
                    // counter from zero — so does the delta baseline.
                    last_session_uploaded: 0,
                    stop_reason: SeedingStopReason::None,
                },
            );
            log_info!(
                "[bt-seeding] task={} promoted from seeding queue",
                short_id(&task_id)
            );
            activated.push(task_id);
        }

        // Demote while over cap (cap shrank at runtime). Most recently
        // activated seeders yield first and keep queue-front priority.
        if cap > 0 {
            let now = Instant::now();
            while guard.active.len() > cap {
                let Some(task_id) = guard
                    .active
                    .iter()
                    .max_by_key(|(_, e)| e.stint_started)
                    .map(|(id, _)| id.clone())
                else {
                    break;
                };
                let Some(entry) = guard.active.remove(&task_id) else {
                    break;
                };
                let folded = entry.effective_seed_time_secs(now);
                guard.queued.push_front((
                    task_id.clone(),
                    QueuedSeed {
                        handle: entry.handle,
                        uploaded_at_completion: entry.uploaded_at_completion,
                        seed_time_base_secs: folded,
                    },
                ));
                log_info!(
                    "[bt-seeding] task={} demoted to seeding queue (cap {})",
                    short_id(&task_id),
                    cap
                );
                demoted.push((task_id, folded));
            }
        }

        (activated, demoted)
    }

    /// Apply a fresh live-upload snapshot and return the delta that should be
    /// added to the persisted `uploaded_bytes` counter.
    ///
    /// Returns `None` when `snapshot_uploaded` is negative (should not happen)
    /// or the seeder is not actively seeding. The caller should skip DB writes
    /// when this returns `None`.
    ///
    /// librqbit resets its internal upload counter when a torrent is paused
    /// or when the session is rebuilt, so this method detects counter
    /// regression and resets the baseline to avoid negative deltas.
    pub async fn apply_upload_snapshot(
        &self,
        task_id: &str,
        snapshot_uploaded: i64,
        upload_speed_bps: i64,
    ) -> Option<i64> {
        if snapshot_uploaded < 0 {
            return None;
        }
        let mut guard = self.state.lock().await;
        let entry = guard.active.get_mut(task_id)?;

        // Counter reset (pause/resume or new session): start a new baseline.
        if snapshot_uploaded < entry.last_session_uploaded {
            entry.last_session_uploaded = 0;
        }
        let delta = snapshot_uploaded - entry.last_session_uploaded;
        if delta >= 0 {
            entry.last_session_uploaded = snapshot_uploaded;
        }

        // `last_uploaded_bytes` 只保存 DB 累计尺度，由 `evaluate_limits`
        // 独占写入；这里的入参是会话计数器（尺度更小），据它检测到的上传
        // 活动只刷新不活跃计时器，或直接依据正的增量。
        if upload_speed_bps > 0 || delta > 0 {
            entry.last_upload_instant = Instant::now();
        }

        Some(delta)
    }

    /// Remove a seeding entry (active or queued) and report its final
    /// cumulative seeding time for persistence.
    pub async fn unregister(&self, task_id: &str) -> Option<UnregisteredSeed> {
        let mut guard = self.state.lock().await;
        if let Some(entry) = guard.active.remove(task_id) {
            return Some(UnregisteredSeed {
                was_active: true,
                seed_time_secs: entry.effective_seed_time_secs(Instant::now()),
            });
        }
        let pos = guard.queued.iter().position(|(id, _)| id == task_id)?;
        let (_, queued) = guard.queued.remove(pos)?;
        Some(UnregisteredSeed {
            was_active: false,
            seed_time_secs: queued.seed_time_base_secs,
        })
    }

    /// Get a clone of the handle for the given task, if actively seeding.
    pub async fn get_handle(&self, task_id: &str) -> Option<BtHandle> {
        let guard = self.state.lock().await;
        guard
            .active
            .get(task_id)
            .map(|entry| Arc::clone(&entry.handle))
    }

    /// Returns `true` if the task is actively seeding (not queued).
    pub async fn is_seeding(&self, task_id: &str) -> bool {
        let guard = self.state.lock().await;
        guard.active.contains_key(task_id)
    }

    /// Number of registered seeders, active plus queued.
    pub async fn total_count(&self) -> usize {
        let guard = self.state.lock().await;
        guard.active.len() + guard.queued.len()
    }

    /// Snapshot of actively seeding task IDs.
    pub async fn active_task_ids(&self) -> Vec<String> {
        let guard = self.state.lock().await;
        guard.active.keys().cloned().collect()
    }

    /// Snapshot of every registered task ID (active first, then queued).
    pub async fn all_task_ids(&self) -> Vec<String> {
        let guard = self.state.lock().await;
        guard
            .active
            .keys()
            .cloned()
            .chain(guard.queued.iter().map(|(id, _)| id.clone()))
            .collect()
    }

    /// Effective cumulative seeding seconds per active seeder, for periodic
    /// persistence. Queued entries are excluded (their base is already
    /// persisted and does not advance).
    pub async fn seed_time_snapshot(&self) -> Vec<(String, i64)> {
        let now = Instant::now();
        let guard = self.state.lock().await;
        guard
            .active
            .iter()
            .map(|(id, e)| (id.clone(), e.effective_seed_time_secs(now)))
            .collect()
    }

    /// Evaluate active seeders against their effective limits. `resolve`
    /// returns the per-task effective config plus a live upload snapshot;
    /// tasks whose effective config has no enabled conditions never stop.
    /// Returns Vec of `(task_id, reason)` for seeders that should be stopped.
    /// Queued seeds are frozen (no uploads, no time accrual) and are never
    /// stopped here.
    pub async fn evaluate_limits(
        &self,
        resolve: impl Fn(&str) -> (SeedingLimitConfig, SeedingUploadSnapshot),
    ) -> Vec<(String, SeedingStopReason)> {
        let now = Instant::now();
        let mut guard = self.state.lock().await;
        let mut stops = Vec::new();
        for (task_id, entry) in guard.active.iter_mut() {
            let (config, snap) = resolve(task_id);

            // Any upload activity resets the inactive timer.
            if snap.upload_speed_bps > 0 || snap.total_uploaded > entry.last_uploaded_bytes {
                entry.last_upload_instant = now;
                entry.last_uploaded_bytes = snap.total_uploaded;
            }

            if !config.has_enabled_conditions() {
                continue;
            }

            let reason = evaluate_entry(
                now,
                entry.effective_seed_time_secs(now),
                entry.last_upload_instant,
                entry.uploaded_at_completion,
                snap,
                &config,
            );
            if reason != SeedingStopReason::None {
                entry.stop_reason = reason;
                stops.push((task_id.clone(), reason));
            }
        }
        stops
    }
}

impl Default for SeedingManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Ratio of `uploaded` against the effective divisor: downloaded bytes,
/// falling back to the full torrent size when the recorded download count is
/// implausibly small (< 1% of the size, e.g. data restored from disk after a
/// recheck). A non-positive divisor with non-zero upload counts as an
/// infinite ratio so any enabled ratio limit fires immediately.
fn ratio_of(uploaded: i64, total_downloaded: i64, total_size: i64) -> f64 {
    let mut divisor = total_downloaded;
    if divisor < total_size / 100 {
        divisor = total_size;
    }
    if divisor <= 0 {
        if uploaded > 0 { f64::INFINITY } else { 0.0 }
    } else {
        uploaded as f64 / divisor as f64
    }
}

/// Pure helper: decide whether a single seeding entry should stop.
/// `seed_time_secs` is the cumulative seeding time including the current
/// stint; paused/queued periods are excluded by construction.
fn evaluate_entry(
    now: Instant,
    seed_time_secs: i64,
    last_upload_instant: Instant,
    uploaded_at_completion: i64,
    snap: SeedingUploadSnapshot,
    config: &SeedingLimitConfig,
) -> SeedingStopReason {
    let ratio_enabled = config.ratio_limit > 0.0;
    let post_ratio_enabled = config.post_ratio_limit > 0.0;
    let seed_time_enabled = config.seed_time_limit_minutes > 0;
    let inactive_enabled = config.inactive_time_limit_minutes > 0;

    if !ratio_enabled && !post_ratio_enabled && !seed_time_enabled && !inactive_enabled {
        return SeedingStopReason::None;
    }

    let ratio_reached = ratio_enabled
        && ratio_of(snap.total_uploaded, snap.total_downloaded, snap.total_size)
            >= config.ratio_limit;
    let post_ratio_reached = post_ratio_enabled
        && ratio_of(
            snap.total_uploaded.saturating_sub(uploaded_at_completion),
            snap.total_downloaded,
            snap.total_size,
        ) >= config.post_ratio_limit;

    let seed_time_reached =
        seed_time_enabled && seed_time_secs >= (config.seed_time_limit_minutes * 60) as i64;

    let inactive_reached = inactive_enabled
        && snap.upload_speed_bps == 0
        && now.duration_since(last_upload_instant)
            >= Duration::from_secs(config.inactive_time_limit_minutes * 60);

    match config.operator {
        SeedingLimitOperator::And => {
            let all_reached = (!ratio_enabled || ratio_reached)
                && (!post_ratio_enabled || post_ratio_reached)
                && (!seed_time_enabled || seed_time_reached)
                && (!inactive_enabled || inactive_reached);
            if all_reached {
                // Preserve deterministic priority for the primary reason.
                if ratio_reached || post_ratio_reached {
                    SeedingStopReason::RatioReached
                } else if seed_time_reached {
                    SeedingStopReason::TimeReached
                } else {
                    SeedingStopReason::InactiveTimeReached
                }
            } else {
                SeedingStopReason::None
            }
        }
        SeedingLimitOperator::Or => {
            if ratio_reached || post_ratio_reached {
                SeedingStopReason::RatioReached
            } else if seed_time_reached {
                SeedingStopReason::TimeReached
            } else if inactive_reached {
                SeedingStopReason::InactiveTimeReached
            } else {
                SeedingStopReason::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(uploaded: i64, downloaded: i64, size: i64, speed: i64) -> SeedingUploadSnapshot {
        SeedingUploadSnapshot {
            total_uploaded: uploaded,
            total_downloaded: downloaded,
            total_size: size,
            upload_speed_bps: speed,
        }
    }

    fn config_or(
        ratio: f64,
        post_ratio: f64,
        time_min: u64,
        inactive_min: u64,
    ) -> SeedingLimitConfig {
        SeedingLimitConfig {
            ratio_limit: ratio,
            post_ratio_limit: post_ratio,
            seed_time_limit_minutes: time_min,
            inactive_time_limit_minutes: inactive_min,
            operator: SeedingLimitOperator::Or,
            then_action: SeedingThenAction::Stop,
        }
    }

    #[test]
    fn defaults_have_all_limits_disabled() {
        let config = SeedingLimitConfig::default();
        assert!(!config.has_enabled_conditions());
        assert_eq!(config.operator, SeedingLimitOperator::Or);
        assert_eq!(config.then_action, SeedingThenAction::Stop);
    }

    #[test]
    fn total_ratio_reached() {
        let config = config_or(1.0, 0.0, 0, 0);
        let now = Instant::now();
        let reason = evaluate_entry(now, 0, now, 0, snap(200, 100, 100, 0), &config);
        assert_eq!(reason, SeedingStopReason::RatioReached);
    }

    #[test]
    fn ratio_divisor_falls_back_to_total_size() {
        // Downloaded counter is implausibly small (<1% of size): the divisor
        // falls back to the torrent size, so 200 uploaded of a 10_000 torrent
        // stays far below a 1.0 ratio even though uploaded >> downloaded.
        let config = config_or(1.0, 0.0, 0, 0);
        let now = Instant::now();
        let reason = evaluate_entry(now, 0, now, 0, snap(200, 10, 10_000, 0), &config);
        assert_eq!(reason, SeedingStopReason::None);

        // With uploads reaching the full size, the ratio limit fires.
        let reason = evaluate_entry(now, 0, now, 0, snap(10_000, 10, 10_000, 0), &config);
        assert_eq!(reason, SeedingStopReason::RatioReached);
    }

    #[test]
    fn zero_divisor_with_uploads_counts_as_infinite_ratio() {
        let config = config_or(2.0, 0.0, 0, 0);
        let now = Instant::now();
        let reason = evaluate_entry(now, 0, now, 0, snap(1, 0, 0, 0), &config);
        assert_eq!(reason, SeedingStopReason::RatioReached);

        // No uploads and no data: ratio is 0, nothing fires.
        let reason = evaluate_entry(now, 0, now, 0, snap(0, 0, 0, 0), &config);
        assert_eq!(reason, SeedingStopReason::None);
    }

    #[test]
    fn seed_time_reached_uses_cumulative_seconds() {
        let config = config_or(0.0, 0.0, 10, 0);
        let now = Instant::now();
        let reason = evaluate_entry(now, 20 * 60, now, 0, snap(0, 1, 1, 0), &config);
        assert_eq!(reason, SeedingStopReason::TimeReached);

        let reason = evaluate_entry(now, 5 * 60, now, 0, snap(0, 1, 1, 0), &config);
        assert_eq!(reason, SeedingStopReason::None);
    }

    #[test]
    fn inactive_time_reached() {
        let config = config_or(0.0, 0.0, 0, 5);
        let now = Instant::now();
        let reason = evaluate_entry(
            now,
            0,
            now - Duration::from_secs(6 * 60),
            0,
            snap(100, 100, 100, 0),
            &config,
        );
        assert_eq!(reason, SeedingStopReason::InactiveTimeReached);
    }

    #[test]
    fn inactive_time_not_reached_if_uploaded_recently() {
        let config = config_or(0.0, 0.0, 0, 5);
        let now = Instant::now();
        let reason = evaluate_entry(
            now,
            60 * 60,
            now - Duration::from_secs(60),
            0,
            snap(100, 100, 100, 0),
            &config,
        );
        assert_eq!(reason, SeedingStopReason::None);
    }

    #[test]
    fn and_combination_requires_all_enabled() {
        let config = SeedingLimitConfig {
            ratio_limit: 1.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 10,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::And,
            then_action: SeedingThenAction::Stop,
        };
        let now = Instant::now();
        // Ratio reached, but seed time not yet.
        let reason = evaluate_entry(now, 5 * 60, now, 0, snap(200, 100, 100, 1000), &config);
        assert_eq!(reason, SeedingStopReason::None);

        // Both reached.
        let reason = evaluate_entry(now, 20 * 60, now, 0, snap(200, 100, 100, 1000), &config);
        assert_eq!(reason, SeedingStopReason::RatioReached);
    }

    #[test]
    fn or_combination_stops_on_any() {
        let config = config_or(2.0, 0.0, 10, 0);
        let now = Instant::now();
        // Ratio not reached, but seed time reached.
        let reason = evaluate_entry(now, 20 * 60, now, 0, snap(100, 100, 100, 0), &config);
        assert_eq!(reason, SeedingStopReason::TimeReached);
    }

    #[test]
    fn no_enabled_conditions_never_stops() {
        let config = config_or(0.0, 0.0, 0, 0);
        let now = Instant::now();
        let reason = evaluate_entry(
            now,
            365 * 24 * 60 * 60,
            now,
            0,
            snap(1_000_000, 1, 1, 0),
            &config,
        );
        assert_eq!(reason, SeedingStopReason::None);
    }

    #[tokio::test]
    async fn manager_returns_no_stops_when_empty() {
        let manager = SeedingManager::new();
        let config = config_or(1.0, 0.0, 60, 0);
        let stops = manager
            .evaluate_limits(|_| (config, SeedingUploadSnapshot::default()))
            .await;
        assert!(stops.is_empty());
    }

    #[tokio::test]
    async fn manager_respects_disabled_conditions() {
        let manager = SeedingManager::new();
        let config = SeedingLimitConfig::default();
        let stops = manager
            .evaluate_limits(|_| (config, snap(200, 100, 100, 0)))
            .await;
        assert!(stops.is_empty());
    }

    #[test]
    fn overrides_default_is_all_inherit() {
        let o = SeedLimitOverrides::default();
        assert!(o.is_all_inherit());
        let global = config_or(1.5, 0.0, 30, 5);
        assert_eq!(o.apply(&global), global);
    }

    #[test]
    fn overrides_unlimited_disables_conditions() {
        let o = SeedLimitOverrides {
            ratio_limit_milli: SEED_LIMIT_UNLIMITED,
            post_ratio_limit_milli: SEED_LIMIT_UNLIMITED,
            seed_time_limit_minutes: SEED_LIMIT_UNLIMITED,
            inactive_time_limit_minutes: SEED_LIMIT_UNLIMITED,
        };
        let global = config_or(1.5, 2.0, 30, 5);
        let effective = o.apply(&global);
        assert!(!effective.has_enabled_conditions());
        // 组合方式与达标动作始终取全局。
        assert_eq!(effective.operator, global.operator);
        assert_eq!(effective.then_action, global.then_action);
    }

    #[test]
    fn overrides_custom_values_replace_global() {
        let o = SeedLimitOverrides {
            ratio_limit_milli: 2500,
            post_ratio_limit_milli: SEED_LIMIT_INHERIT,
            seed_time_limit_minutes: 90,
            inactive_time_limit_minutes: SEED_LIMIT_UNLIMITED,
        };
        let global = config_or(1.0, 0.5, 30, 5);
        let effective = o.apply(&global);
        assert_eq!(effective.ratio_limit, 2.5);
        assert_eq!(effective.post_ratio_limit, 0.5);
        assert_eq!(effective.seed_time_limit_minutes, 90);
        assert_eq!(effective.inactive_time_limit_minutes, 0);
    }

    #[tokio::test]
    async fn per_task_override_enables_limit_when_global_disabled() {
        // 全局全关，但任务自定义 ratio 1.0：该任务应按覆盖值停止。
        let manager = SeedingManager::new();
        let global = SeedingLimitConfig::default();
        let overrides = SeedLimitOverrides {
            ratio_limit_milli: 1000,
            ..SeedLimitOverrides::default()
        };
        assert!(!global.has_enabled_conditions());
        let effective = overrides.apply(&global);
        assert!(effective.has_enabled_conditions());
        let stops = manager
            .evaluate_limits(|_| (effective, snap(200, 100, 100, 0)))
            .await;
        // 空管理器无做种者——纯覆盖解析已在上面断言；此处仅验证签名可用。
        assert!(stops.is_empty());
    }
}
