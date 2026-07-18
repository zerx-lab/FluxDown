//! BitTorrent seeding lifecycle.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::bt_downloader::BtHandle;
use crate::logger::log_info;

/// Numeric code indicating an active seeder (not a stop reason).
pub const SEEDING_STATUS_ACTIVE: i32 = 1;

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
    /// Maximum time spent seeding, in minutes. `0` disables the limit.
    pub seed_time_limit_minutes: u64,
    /// Maximum time allowed with zero upload speed, in minutes. `0` disables.
    pub inactive_time_limit_minutes: u64,
    /// How to combine the enabled conditions.
    pub operator: SeedingLimitOperator,
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
    /// Default limits: share to a 1.0 ratio **or** seed for 72 hours.
    fn default() -> Self {
        Self {
            ratio_limit: 1.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 72 * 60,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::Or,
        }
    }
}

/// One actively seeding torrent.
pub struct SeedingEntry {
    pub handle: BtHandle,
    pub started_at: Instant,
    /// Last instant at which the seeder had non-zero upload activity.
    pub last_upload_instant: Instant,
    /// Total uploaded bytes observed at `last_upload_instant`.
    pub last_uploaded_bytes: i64,
    /// Total uploaded bytes when the download completed and seeding started.
    /// Used to compute the post-completion ratio.
    pub uploaded_at_completion: i64,
    pub stop_reason: SeedingStopReason,
}

impl std::fmt::Debug for SeedingEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedingEntry")
            .field("started_at", &self.started_at)
            .field("last_upload_instant", &self.last_upload_instant)
            .field("last_uploaded_bytes", &self.last_uploaded_bytes)
            .field("uploaded_at_completion", &self.uploaded_at_completion)
            .field("stop_reason", &self.stop_reason)
            .finish_non_exhaustive()
    }
}

/// Snapshot of live upload state needed by [`SeedingManager::evaluate_limits`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SeedingUploadSnapshot {
    pub total_uploaded: i64,
    pub total_downloaded: i64,
    pub upload_speed_bps: i64,
}

/// Manages the lifecycle of seeding BT torrents.
pub struct SeedingManager {
    seeders: Mutex<HashMap<String, SeedingEntry>>,
    /// Maximum number of tasks allowed to seed at the same time.
    /// `0` means unlimited.
    max_seeding_tasks: usize,
}

impl SeedingManager {
    /// Create an empty seeding manager.
    ///
    /// `max_seeding_tasks`: maximum number of tasks allowed to seed at the
    /// same time. `0` means unlimited.
    pub fn new(max_seeding_tasks: usize) -> Self {
        Self {
            seeders: Mutex::new(HashMap::new()),
            max_seeding_tasks,
        }
    }

    /// Register a completed BT task as an active seeder.
    ///
    /// Returns `true` if the task was registered. Returns `false` if the
    /// configured `max_seeding_tasks` limit has already been reached; the
    /// caller should pause the torrent in that case.
    pub async fn register(
        &self,
        task_id: String,
        handle: BtHandle,
        uploaded_at_completion: i64,
    ) -> bool {
        let short = task_id.get(..8).unwrap_or(&task_id);
        let mut guard = self.seeders.lock().await;
        if self.max_seeding_tasks > 0 && guard.len() >= self.max_seeding_tasks {
            log_info!(
                "[bt-seeding] task={} not registered: max seeding tasks ({}) reached",
                short,
                self.max_seeding_tasks
            );
            return false;
        }
        log_info!("[bt-seeding] task={} registered for seeding", short);
        let now = Instant::now();
        let entry = SeedingEntry {
            handle,
            started_at: now,
            last_upload_instant: now,
            last_uploaded_bytes: uploaded_at_completion,
            uploaded_at_completion,
            stop_reason: SeedingStopReason::None,
        };
        guard.insert(task_id, entry);
        true
    }

    /// Remove a seeding entry and return it, if present.
    pub async fn unregister(&self, task_id: &str) -> Option<SeedingEntry> {
        let mut guard = self.seeders.lock().await;
        guard.remove(task_id)
    }

    /// Get a clone of the handle for the given task, if it is seeding.
    pub async fn get_handle(&self, task_id: &str) -> Option<BtHandle> {
        let guard = self.seeders.lock().await;
        guard.get(task_id).map(|entry| Arc::clone(&entry.handle))
    }

    /// Returns `true` if the task is currently registered as a seeder.
    pub async fn is_seeding(&self, task_id: &str) -> bool {
        let guard = self.seeders.lock().await;
        guard.contains_key(task_id)
    }

    /// Number of currently active seeders.
    pub async fn active_count(&self) -> usize {
        let guard = self.seeders.lock().await;
        guard.len()
    }

    /// Snapshot of all task IDs currently seeding.
    pub async fn all_task_ids(&self) -> Vec<String> {
        let guard = self.seeders.lock().await;
        guard.keys().cloned().collect()
    }

    /// Evaluate seeders against the configured limits. Returns Vec of
    /// `(task_id, reason)` for seeders that should be stopped.
    pub async fn evaluate_limits(
        &self,
        config: &SeedingLimitConfig,
        snapshot: impl Fn(&str) -> SeedingUploadSnapshot,
    ) -> Vec<(String, SeedingStopReason)> {
        if !config.has_enabled_conditions() {
            return Vec::new();
        }

        let now = Instant::now();
        let mut guard = self.seeders.lock().await;
        let mut stops = Vec::new();
        for (task_id, entry) in guard.iter_mut() {
            let snap = snapshot(task_id);

            // Any upload activity resets the inactive timer.
            if snap.upload_speed_bps > 0 || snap.total_uploaded > entry.last_uploaded_bytes {
                entry.last_upload_instant = now;
                entry.last_uploaded_bytes = snap.total_uploaded;
            }

            let reason = evaluate_entry(
                now,
                entry.started_at,
                entry.last_upload_instant,
                entry.uploaded_at_completion,
                snap.total_uploaded,
                snap.total_downloaded,
                snap.upload_speed_bps,
                config,
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
        Self::new(0)
    }
}

/// Pure helper: decide whether a single seeding entry should stop.
#[allow(clippy::too_many_arguments)]
fn evaluate_entry(
    now: Instant,
    started_at: Instant,
    last_upload_instant: Instant,
    uploaded_at_completion: i64,
    total_uploaded: i64,
    total_downloaded: i64,
    upload_speed_bps: i64,
    config: &SeedingLimitConfig,
) -> SeedingStopReason {
    let ratio_enabled = config.ratio_limit > 0.0;
    let post_ratio_enabled = config.post_ratio_limit > 0.0;
    let seed_time_enabled = config.seed_time_limit_minutes > 0;
    let inactive_enabled = config.inactive_time_limit_minutes > 0;

    if !ratio_enabled && !post_ratio_enabled && !seed_time_enabled && !inactive_enabled {
        return SeedingStopReason::None;
    }

    let total_downloaded = total_downloaded.max(1) as f64;
    let ratio_reached =
        ratio_enabled && (total_uploaded as f64 / total_downloaded) >= config.ratio_limit;
    let post_ratio_reached = post_ratio_enabled
        && ((total_uploaded - uploaded_at_completion) as f64 / total_downloaded)
            >= config.post_ratio_limit;

    let seed_time_reached = seed_time_enabled
        && started_at.elapsed() >= Duration::from_secs(config.seed_time_limit_minutes * 60);

    let inactive_reached = inactive_enabled
        && upload_speed_bps == 0
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

    #[test]
    fn total_ratio_reached() {
        let config = SeedingLimitConfig {
            ratio_limit: 1.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::Or,
        };
        let now = Instant::now();
        let reason = evaluate_entry(now, now, now, 0, 200, 100, 0, &config);
        assert_eq!(reason, SeedingStopReason::RatioReached);
    }

    #[test]
    fn seed_time_reached() {
        let config = SeedingLimitConfig {
            ratio_limit: 0.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 10,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::Or,
        };
        let now = Instant::now();
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(20 * 60),
            now,
            0,
            0,
            1,
            0,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::TimeReached);
    }

    #[test]
    fn inactive_time_reached() {
        let config = SeedingLimitConfig {
            ratio_limit: 0.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 5,
            operator: SeedingLimitOperator::Or,
        };
        let now = Instant::now();
        let reason = evaluate_entry(
            now,
            now,
            now - Duration::from_secs(6 * 60),
            0,
            100,
            100,
            0,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::InactiveTimeReached);
    }

    #[test]
    fn inactive_time_not_reached_if_uploaded_recently() {
        let config = SeedingLimitConfig {
            ratio_limit: 0.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 5,
            operator: SeedingLimitOperator::Or,
        };
        let now = Instant::now();
        // Seeding started a long time ago, but the last upload was only
        // 1 minute ago, so the 5-minute inactive window has not elapsed.
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(60 * 60),
            now - Duration::from_secs(60),
            0,
            100,
            100,
            0,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::None);
    }

    #[test]
    fn inactive_time_counts_since_last_upload_not_total_seed_time() {
        let config = SeedingLimitConfig {
            ratio_limit: 0.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 5,
            operator: SeedingLimitOperator::Or,
        };
        let now = Instant::now();
        // Seeder uploaded for 4 minutes after registration, then stalled
        // for 1 minute. Total seeding time is 5 minutes, but the continuous
        // zero-upload window is only 1 minute, so it must NOT be stopped.
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(5 * 60),
            now - Duration::from_secs(60),
            0,
            100,
            100,
            0,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::None);

        // After the stall reaches the full 5-minute limit, it should stop.
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(9 * 60),
            now - Duration::from_secs(5 * 60),
            0,
            100,
            100,
            0,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::InactiveTimeReached);
    }

    #[test]
    fn and_combination_requires_all_enabled() {
        let config = SeedingLimitConfig {
            ratio_limit: 1.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 10,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::And,
        };
        let now = Instant::now();
        // Ratio reached, but seed time not yet.
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(5 * 60),
            now,
            0,
            200,
            100,
            1000,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::None);

        // Both reached.
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(20 * 60),
            now,
            0,
            200,
            100,
            1000,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::RatioReached);
    }

    #[test]
    fn or_combination_stops_on_any() {
        let config = SeedingLimitConfig {
            ratio_limit: 2.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 10,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::Or,
        };
        let now = Instant::now();
        // Ratio not reached, but seed time reached.
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(20 * 60),
            now,
            0,
            100,
            100,
            0,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::TimeReached);
    }

    #[test]
    fn no_enabled_conditions_never_stops() {
        let config = SeedingLimitConfig {
            ratio_limit: 0.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::And,
        };
        let now = Instant::now();
        let reason = evaluate_entry(
            now,
            now - Duration::from_secs(365 * 24 * 60 * 60),
            now,
            0,
            1_000_000,
            1,
            0,
            &config,
        );
        assert_eq!(reason, SeedingStopReason::None);
    }

    #[tokio::test]
    async fn manager_returns_no_stops_when_empty() {
        let manager = SeedingManager::new(0);
        let config = SeedingLimitConfig::default();
        let stops = manager
            .evaluate_limits(&config, |_| SeedingUploadSnapshot::default())
            .await;
        assert!(stops.is_empty());
    }

    #[tokio::test]
    async fn manager_respects_disabled_conditions() {
        let manager = SeedingManager::new(0);
        let config = SeedingLimitConfig {
            ratio_limit: 0.0,
            post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 0,
            operator: SeedingLimitOperator::Or,
        };
        let stops = manager
            .evaluate_limits(&config, |_| SeedingUploadSnapshot {
                total_uploaded: 200,
                total_downloaded: 100,
                upload_speed_bps: 0,
            })
            .await;
        assert!(stops.is_empty());
    }
}
