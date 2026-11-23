//! 已完成记录清理的宽限：任务刚完成或刚停止做种后，记录至少再留一小段时间。
//!
//! 各端的完成通知都是快照差分（Android / iOS 的 `NotificationTracker`、agent
//! `background_effects.rs` 的 `TaskTracker`）：完成帧之后几毫秒就到的
//! `TasksDeleted` 会把 status=3 的中间态合并掉，通知就不发。宽限内的 id 不进清理
//! 候选；清理跳过它们时安排一次到期唤醒，经宿主的 `retention_purge` 通道回到 actor
//! 再清。唤醒只跟在完成 / 停止做种之后，不是周期定时器（空闲静默）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// 完成或停止做种后，记录至少保留的时长。
pub(super) const RETENTION_GRACE: Duration = Duration::from_secs(10);

/// 宽限中的任务 id → 到期时刻。actor 与 BT runtime 上的续种任务共用一份。
#[derive(Clone, Default)]
pub(super) struct RetentionGrace {
    held: Arc<Mutex<HashMap<String, Instant>>>,
}

impl RetentionGrace {
    /// 从现在起把 `ids` 保留 `period`；已在宽限内的 id 顺延。顺带丢掉已到期的条目，
    /// 「从不」模式下也只留最近一个宽限内的条目。
    pub(super) fn hold<'a>(&self, ids: impl IntoIterator<Item = &'a str>, period: Duration) {
        let now = Instant::now();
        let until = now.checked_add(period).unwrap_or(now);
        let mut held = self.lock();
        held.retain(|_, release| *release > now);
        for id in ids {
            held.insert(id.to_owned(), until);
        }
    }

    /// 仍在宽限内的 id，以及其中最早的到期时刻。顺带丢掉已到期的条目。
    pub(super) fn pending(&self, now: Instant) -> (HashSet<String>, Option<Instant>) {
        let mut held = self.lock();
        held.retain(|_, release| *release > now);
        let next = held.values().min().copied();
        (held.keys().cloned().collect(), next)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Instant>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_ids_release_after_the_period() {
        let grace = RetentionGrace::default();
        grace.hold(["a"], Duration::from_secs(60));
        grace.hold(["b"], Duration::ZERO);
        let now = Instant::now();
        let (held, next) = grace.pending(now);
        assert!(held.contains("a"));
        assert!(!held.contains("b"), "零宽限立即到期");
        assert!(next.is_some_and(|at| at > now));

        let later = now + Duration::from_secs(61);
        let (held, next) = grace.pending(later);
        assert!(held.is_empty());
        assert_eq!(next, None);
    }
}
