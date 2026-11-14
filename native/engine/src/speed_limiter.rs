//! Global token-bucket speed limiter shared across all download tasks.
//!
//! The limiter uses a token-bucket algorithm with **time-based refill**:
//! - A background task measures *actual* wall-clock elapsed time between ticks
//!   and adds tokens proportionally, eliminating drift from `tokio::time::interval`.
//! - Tokens are capped at 250 ms worth of the configured limit, preventing
//!   excessive bursts after idle periods while tolerating normal tick jitter.
//! - Refill uses a CAS loop (no `fetch_add` + `store` race window).
//! - When `limit == 0`, the limiter is disabled (unlimited speed).
//!
//! The limiter is designed to be cheaply cloneable (`Arc` inside) so every
//! download segment can hold a handle without additional allocation.

use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// Shared, cheaply-cloneable speed limiter.
#[derive(Clone)]
pub struct SpeedLimiter {
    inner: Arc<Inner>,
    task_override: Option<Arc<TaskOverride>>,
}

/// Per-task token bucket. Its refill worker only runs while the override is
/// positive and waits without periodic wake-ups while the override is disabled.
struct TaskOverride {
    inner: Arc<Inner>,
    refill_started: AtomicBool,
}

struct Inner {
    /// Current speed limit in bytes/sec.  0 = unlimited.
    limit_bps: AtomicU64,
    /// Available tokens (bytes that may be consumed immediately).
    tokens: AtomicU64,
    /// `consume` 进入令牌等待的累计次数（见 [`SpeedLimiter::throttle_waits`]）。
    throttle_waits: AtomicU64,
    /// Notify waiters when tokens are replenished.
    notify: Notify,
    /// Notify the refill task to wake from its idle sleep when the limit
    /// changes from unlimited (0) to a positive value.
    refill_wake: Arc<Notify>,
}

impl Inner {
    fn new(limit_bps: u64) -> Self {
        Self {
            limit_bps: AtomicU64::new(limit_bps),
            tokens: AtomicU64::new(0),
            notify: Notify::new(),
            throttle_waits: AtomicU64::new(0),
            refill_wake: Arc::new(Notify::new()),
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Wake a dormant worker so it can observe that its weak reference has
        // expired and terminate without retaining this limiter.
        self.refill_wake.notify_one();
    }
}

/// Nominal refill interval — 50 ms gives smooth throughput without too many
/// wake-ups.  The *actual* refill amount is computed from wall-clock elapsed
/// time, so this only controls how often the task wakes.
const REFILL_INTERVAL_MS: u64 = 50;

/// Token bucket capacity expressed as a fraction of the per-second limit.
/// 250 ms worth of tokens (limit / 4) keeps bursts small while absorbing
/// 4–5 ticks of jitter without losing any tokens.
const CAP_DURATION_MS: u64 = 250;

impl SpeedLimiter {
    /// Create a new limiter with the given initial limit (bytes/sec).
    /// Pass `0` for unlimited.
    pub fn new(limit_bps: u64) -> Self {
        Self {
            inner: Arc::new(Inner::new(limit_bps)),
            task_override: None,
        }
    }

    /// Create a task-local limiter that uses this limiter whenever its
    /// override is `0`, and otherwise uses an independent bucket at the
    /// override limit.
    ///
    /// Clones share the override; the original limiter and other task
    /// overrides remain independent. A refill worker is started only when the
    /// initial override is positive.
    pub fn with_task_override(&self, initial_limit_bps: u64) -> Self {
        let limiter = Self {
            inner: Arc::clone(&self.inner),
            task_override: Some(Arc::new(TaskOverride {
                inner: Arc::new(Inner::new(initial_limit_bps)),
                refill_started: AtomicBool::new(false),
            })),
        };
        if initial_limit_bps > 0 {
            limiter.ensure_task_refill_task();
        }
        limiter
    }

    /// Update this task's override. `0` follows the original limiter's
    /// current limit; positive values replace it rather than stacking with it.
    ///
    /// This has no effect on a limiter that was not created by
    /// [`SpeedLimiter::with_task_override`].
    pub fn set_task_override(&self, limit_bps: u64) {
        let Some(task_override) = &self.task_override else {
            return;
        };
        task_override
            .inner
            .limit_bps
            .store(limit_bps, Ordering::Relaxed);
        task_override.inner.notify.notify_waiters();
        task_override.inner.refill_wake.notify_one();
        if limit_bps > 0 {
            self.ensure_task_refill_task();
        }
    }

    fn ensure_task_refill_task(&self) {
        let Some(task_override) = &self.task_override else {
            return;
        };
        if task_override.inner.limit_bps.load(Ordering::Relaxed) == 0
            || task_override
                .refill_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return;
        }

        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // A later positive update can retry when a runtime is available.
            task_override.refill_started.store(false, Ordering::Release);
            return;
        };
        let weak = Arc::downgrade(&task_override.inner);
        runtime.spawn(Self::refill_loop(weak, None));
    }

    /// Return the bucket and limit selected by the current override value.
    fn selected_limiter(&self) -> (&Inner, u64) {
        if let Some(task_override) = &self.task_override {
            let limit = task_override.inner.limit_bps.load(Ordering::Relaxed);
            if limit > 0 {
                return (&task_override.inner, limit);
            }
        }
        let limit = self.inner.limit_bps.load(Ordering::Relaxed);
        (&self.inner, limit)
    }

    /// Update the speed limit at runtime.  Takes effect on the next refill tick.
    pub fn set_limit(&self, limit_bps: u64) {
        self.inner.limit_bps.store(limit_bps, Ordering::Relaxed);
        // Wake any waiters so they re-evaluate immediately.
        self.inner.notify.notify_waiters();
        // Wake the refill task from its idle sleep (when transitioning from
        // unlimited to limited) so tokens start flowing without delay.
        self.inner.refill_wake.notify_one();
    }

    /// Current configured limit (bytes/sec).  0 = unlimited.
    pub fn limit(&self) -> u64 {
        self.selected_limiter().1
    }

    /// 自创建以来 `consume` 因令牌耗尽而进入等待的累计次数（单调递增，
    /// 零分配）。调用方取两次读数的差值即可得知窗口内限速器是否真正在节流：
    /// 限速值高于链路速率时令牌恒有余量，计数不增长。限速器为全局共享桶，
    /// 计数含其它任务的等待（共享同一瓶颈，对本任务同样是节流）。
    pub fn throttle_waits(&self) -> u64 {
        self.selected_limiter()
            .0
            .throttle_waits
            .load(Ordering::Relaxed)
    }

    /// Consume up to `requested` bytes worth of tokens.
    ///
    /// - If the limiter is disabled (limit == 0), returns `requested` immediately.
    /// - Otherwise waits until at least 1 token is available, then returns
    ///   `min(requested, available)`.  The caller should only process that many
    ///   bytes, then call `consume` again for the remainder.
    ///
    /// This design avoids holding an async lock and naturally distributes
    /// bandwidth among all concurrent callers via contention on the atomic.
    pub async fn consume(&self, requested: u64) -> u64 {
        if requested == 0 {
            return 0;
        }

        loop {
            let (inner, limit) = self.selected_limiter();
            if limit == 0 {
                // Unlimited — pass through.
                return requested;
            }

            // Try to take some tokens.
            let available = inner.tokens.load(Ordering::Acquire);
            if available > 0 {
                let take = requested.min(available);
                // CAS loop to atomically subtract tokens.
                match inner.tokens.compare_exchange_weak(
                    available,
                    available - take,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return take,
                    Err(_) => continue, // contention — retry
                }
            }

            // No tokens available — 记一次节流等待（coordinator 按窗口读取增量，
            // 判定该窗口是否被限速器节流），随后等待 refill task 通知。
            inner.throttle_waits.fetch_add(1, Ordering::Relaxed);
            // Bounded wait guards against the rare TOCTOU race where
            // notify_waiters() fires between our tokens.load() returning 0
            // and the notified().await registration: since notify_waiters()
            // only wakes *currently-registered* listeners, that notification
            // would be silently lost.  The timeout (REFILL_INTERVAL_MS + 10 ms)
            // ensures we retry within at most one extra refill cycle.
            tokio::select! {
                biased;
                () = inner.notify.notified() => {}
                () = tokio::time::sleep(std::time::Duration::from_millis(
                    REFILL_INTERVAL_MS + 10,
                )) => {}
            }
        }
    }

    /// Spawn the background refill task.  Must be called once after creation.
    /// The task runs until the `SpeedLimiter` (and all its clones) are dropped.
    pub fn spawn_refill_task(&self) {
        Self::spawn_refill_worker(&self.inner, Some(Duration::from_secs(2)));
    }

    fn spawn_refill_worker(inner: &Arc<Inner>, idle_sleep: Option<Duration>) {
        let weak = Arc::downgrade(inner);
        tokio::spawn(Self::refill_loop(weak, idle_sleep));
    }

    async fn refill_loop(weak: Weak<Inner>, idle_sleep: Option<Duration>) {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(REFILL_INTERVAL_MS));
        // The first tick completes immediately — skip it and record baseline.
        interval.tick().await;
        let mut last_refill = Instant::now();

        loop {
            interval.tick().await;
            let Some(inner) = weak.upgrade() else {
                // All SpeedLimiter handles dropped — exit.
                break;
            };

            let limit = inner.limit_bps.load(Ordering::Relaxed);
            if limit == 0 {
                // Unlimited — clear any accumulated tokens and wake waiters
                // (they will see limit==0 and pass through).
                inner.tokens.store(0, Ordering::Relaxed);
                inner.notify.notify_waiters();
                // Clone the Arc<Notify> so the worker can wait after releasing
                // its strong reference to `inner`.
                let refill_wake = Arc::clone(&inner.refill_wake);
                drop(inner);
                let wake = refill_wake.notified();
                if let Some(idle_sleep) = idle_sleep {
                    // The shared/base limiter retains its existing periodic
                    // idle check; task overrides sleep until explicitly woken.
                    tokio::select! {
                        () = tokio::time::sleep(idle_sleep) => {}
                        () = wake => {}
                    }
                } else {
                    wake.await;
                }
                // Reset baseline so the first limited tick doesn't get a
                // huge elapsed value.
                last_refill = Instant::now();
                continue;
            }

            // ── Time-based refill ───────────────────────────────────────
            // Measure actual wall-clock time since last refill to
            // compensate for interval jitter / missed-tick bursts.
            let now = Instant::now();
            let elapsed_us = (now - last_refill).as_micros() as u64;

            // refill = limit * elapsed_us / 1_000_000
            // Use u128 intermediate to avoid overflow for very large limits.
            let refill = ((limit as u128) * (elapsed_us as u128) / 1_000_000u128) as u64;

            if refill == 0 {
                // Extremely low limit + short interval — skip this tick.
                // Crucially, do NOT advance `last_refill` here: the elapsed
                // microseconds that were too small to form a whole token must
                // accumulate into the next tick's `elapsed_us`, otherwise this
                // fractional time is permanently discarded and the limiter
                // would systematically deliver below the configured rate at
                // very low limits (e.g. < ~20 B/s).
                continue;
            }

            // A whole token's worth of time has elapsed — advance the
            // baseline only now that the elapsed time has been accounted for.
            last_refill = now;

            // Cap: 250 ms worth of the current limit.  For very low limits
            // ensure at least 2× nominal-tick amount so tokens aren't
            // perpetually capped to zero.
            let nominal_tick = limit * REFILL_INTERVAL_MS / 1000;
            let cap = (limit * CAP_DURATION_MS / 1000)
                .max(nominal_tick * 2)
                .max(1);

            // ── Atomic CAS refill ───────────────────────────────────────
            // Add `refill` tokens and clamp to `cap` in a single atomic
            // step, eliminating the fetch_add + store race window.
            loop {
                let current = inner.tokens.load(Ordering::Acquire);
                let new_val = current.saturating_add(refill).min(cap);
                if new_val == current {
                    // Nothing to change (already at or above cap).
                    break;
                }
                match inner.tokens.compare_exchange_weak(
                    current,
                    new_val,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(_) => continue, // contention — retry
                }
            }

            inner.notify.notify_waiters();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::SpeedLimiter;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn unlimited_returns_full_request() {
        let limiter = SpeedLimiter::new(0); // unlimited
        limiter.spawn_refill_task();

        let got = limiter.consume(1_000_000).await;
        assert_eq!(
            got, 1_000_000,
            "unlimited limiter should return full requested amount"
        );
    }

    #[tokio::test]
    async fn zero_request_returns_zero() {
        let limiter = SpeedLimiter::new(1024);
        limiter.spawn_refill_task();

        let got = limiter.consume(0).await;
        assert_eq!(got, 0);
    }

    #[tokio::test]
    async fn limited_consumes_in_chunks() {
        let limiter = SpeedLimiter::new(10_000); // 10 KB/s
        limiter.spawn_refill_task();

        // Wait for one refill tick (~50ms) to have tokens available
        tokio::time::sleep(Duration::from_millis(100)).await;

        let got = limiter.consume(100_000).await; // request 100 KB
        // Should get at most ~1 KB (10KB/s × 0.1s), capped to 250ms bucket.
        assert!(got > 0, "should get some tokens");
        assert!(got < 100_000, "should not get full request under limit");
    }

    #[tokio::test]
    async fn set_limit_changes_behavior() {
        let limiter = SpeedLimiter::new(0); // start unlimited
        limiter.spawn_refill_task();

        let got = limiter.consume(1_000_000).await;
        assert_eq!(got, 1_000_000, "should be unlimited initially");

        // Switch to limited
        limiter.set_limit(1024);
        assert_eq!(limiter.limit(), 1024);

        // Switch back to unlimited
        limiter.set_limit(0);
        tokio::time::sleep(Duration::from_millis(60)).await; // wait for refill tick
        let got = limiter.consume(500_000).await;
        assert_eq!(got, 500_000, "should be unlimited again");
    }

    /// 令牌有余量时不计节流等待；只有桶被抽干、连接真的要等令牌才递增——
    /// coordinator 据此区分「限速值高于链路速率」与「限速器饱和」。
    #[tokio::test]
    async fn throttle_waits_only_count_real_token_waits() {
        let limiter = SpeedLimiter::new(1_000_000);
        limiter.spawn_refill_task();
        tokio::time::sleep(Duration::from_millis(150)).await;

        let got = limiter.consume(1_000).await;
        assert_eq!(got, 1_000);
        assert_eq!(limiter.throttle_waits(), 0, "令牌有余量不应计等待");

        for _ in 0..20 {
            limiter.consume(10_000_000).await;
            if limiter.throttle_waits() > 0 {
                break;
            }
        }
        assert!(limiter.throttle_waits() > 0, "抽干令牌桶后必须计入等待");
    }

    #[tokio::test]
    async fn limited_speed_is_approximately_correct() {
        let limit_bps: u64 = 50_000; // 50 KB/s
        let limiter = SpeedLimiter::new(limit_bps);
        limiter.spawn_refill_task();

        let start = Instant::now();
        let mut total = 0u64;
        let target = 25_000u64; // 25 KB — should take ~0.5s at 50 KB/s

        while total < target {
            let got = limiter.consume(target - total).await;
            total += got;
        }

        let elapsed = start.elapsed();
        // Should take roughly 0.3–1.5s (allowing wide margin for CI variance)
        assert!(
            elapsed > Duration::from_millis(200),
            "consumed {target} bytes in {elapsed:?} — too fast for {limit_bps} bps limit"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "consumed {target} bytes in {elapsed:?} — too slow, possible deadlock"
        );
    }

    /// Validates that throughput is within ±15 % of the configured limit at
    /// higher speeds (the scenario reported as inaccurate).
    #[tokio::test]
    async fn high_speed_accuracy() {
        let limit_bps: u64 = 5_000_000; // 5 MB/s
        let limiter = SpeedLimiter::new(limit_bps);
        limiter.spawn_refill_task();

        let duration = Duration::from_secs(2);
        let start = Instant::now();
        let mut total = 0u64;

        while start.elapsed() < duration {
            let got = limiter.consume(65_536).await;
            total += got;
        }

        let elapsed = start.elapsed().as_secs_f64();
        let actual_bps = total as f64 / elapsed;
        let ratio = actual_bps / limit_bps as f64;

        assert!(
            ratio > 0.85,
            "throughput {actual_bps:.0} B/s is too low vs limit {limit_bps} B/s (ratio {ratio:.3})"
        );
        assert!(
            ratio < 1.15,
            "throughput {actual_bps:.0} B/s exceeds limit {limit_bps} B/s (ratio {ratio:.3})"
        );
    }

    /// Regression for F008: at extremely low limits the per-tick refill
    /// truncates to 0; the fractional elapsed time must accumulate across ticks
    /// instead of being discarded, otherwise throughput stalls below the limit.
    #[tokio::test]
    async fn very_low_limit_accumulates_tokens_over_time() {
        // 10 B/s with a 50 ms tick yields ~0.5 B/refill, so the refill==0 branch
        // is exercised on most ticks. Tokens must still accumulate to ~1 B every
        // ~100 ms rather than perpetually rounding to zero.
        let limit_bps: u64 = 10;
        let limiter = SpeedLimiter::new(limit_bps);
        limiter.spawn_refill_task();

        let start = Instant::now();
        let mut total = 0u64;
        // Aim for ~5 bytes, which at 10 B/s should take roughly 0.5 s; bail out
        // after a generous deadline so a regression manifests as too-few bytes.
        let deadline = start + Duration::from_secs(3);
        while total < 5 && Instant::now() < deadline {
            let got = limiter.consume(5 - total).await;
            total += got;
        }

        assert!(
            total >= 5,
            "very low limit stalled: got only {total} bytes in {:?} — refill==0 elapsed time was discarded",
            start.elapsed()
        );
    }

    /// Tests that the speed limiter doesn't starve when multiple consumers
    /// compete for tokens. This is relevant to Bug #3 (multi-segment FTP).
    #[tokio::test]
    async fn multiple_concurrent_consumers_all_make_progress() {
        let limiter = SpeedLimiter::new(100_000); // 100 KB/s
        limiter.spawn_refill_task();

        let mut handles = Vec::new();
        for _ in 0..8 {
            let l = limiter.clone();
            handles.push(tokio::spawn(async move {
                let mut total = 0u64;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
                while tokio::time::Instant::now() < deadline {
                    let got = l.consume(4096).await;
                    total += got;
                }
                total
            }));
        }

        let mut totals = Vec::new();
        for h in handles {
            if let Ok(t) = h.await {
                totals.push(t);
            }
        }

        // All consumers should have made some progress
        for (i, t) in totals.iter().enumerate() {
            assert!(*t > 0, "consumer {i} got 0 bytes — starvation detected");
        }

        // Total across all consumers should be approximately 100 KB (1 s at 100 KB/s)
        let grand_total: u64 = totals.iter().sum();
        assert!(
            grand_total > 50_000,
            "total {grand_total} too low for 100KB/s limit over 1s"
        );
        assert!(
            grand_total < 200_000,
            "total {grand_total} exceeds limit — limiter broken"
        );
    }

    #[test]
    fn positive_task_override_uses_its_independent_bucket() {
        let base = SpeedLimiter::new(64);
        base.inner.tokens.store(500, Ordering::Relaxed);
        let limiter = base.with_task_override(128);
        let task_override = limiter.task_override.as_ref().unwrap();
        task_override.inner.tokens.store(17, Ordering::Relaxed);

        assert_eq!(limiter.limit(), 128);

        // Construct the wrapper outside a runtime so no refill worker can
        // change the seeded token count during this deterministic assertion.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        assert_eq!(runtime.block_on(limiter.consume(100)), 17);
        assert_eq!(
            base.inner.tokens.load(Ordering::Relaxed),
            500,
            "the task override must not consume the base bucket"
        );
        assert_eq!(base.limit(), 64);
    }

    #[test]
    fn zero_task_override_tracks_live_base_limit() {
        let base = SpeedLimiter::new(128);
        let limiter = base.with_task_override(0);
        assert_eq!(limiter.limit(), 128);

        base.set_limit(512);
        assert_eq!(limiter.limit(), 512);

        limiter.set_task_override(32);
        assert_eq!(limiter.limit(), 32);
        limiter.set_task_override(0);
        base.set_limit(0);
        assert_eq!(limiter.limit(), 0);
    }

    #[test]
    fn task_overrides_are_isolated_from_each_other_and_the_base() {
        let base = SpeedLimiter::new(100);
        let first = base.with_task_override(200);
        let second = base.with_task_override(300);

        first.set_task_override(250);
        assert_eq!(first.limit(), 250);
        assert_eq!(second.limit(), 300);
        assert_eq!(base.limit(), 100);

        second.set_task_override(0);
        assert_eq!(second.limit(), 100);
        assert_eq!(first.limit(), 250);
        assert_eq!(base.limit(), 100);
    }

    #[test]
    fn positive_task_override_changes_hot_and_is_shared_by_clones() {
        let base = SpeedLimiter::new(100);
        let limiter = base.with_task_override(20);
        let clone = limiter.clone();

        assert_eq!(limiter.limit(), 20);
        clone.set_task_override(40);
        assert_eq!(limiter.limit(), 40);
        limiter.set_task_override(60);
        assert_eq!(clone.limit(), 60);
        assert_eq!(base.limit(), 100);
    }

    #[tokio::test]
    async fn task_override_refill_starts_only_for_positive_limits() {
        let base = SpeedLimiter::new(100);
        let limiter = base.with_task_override(0);
        let task_override = limiter.task_override.as_ref().unwrap();

        assert!(!task_override.refill_started.load(Ordering::Relaxed));
        limiter.set_task_override(0);
        assert!(!task_override.refill_started.load(Ordering::Relaxed));

        limiter.set_task_override(20);
        assert!(task_override.refill_started.load(Ordering::Relaxed));
        limiter.set_task_override(40);
        assert!(task_override.refill_started.load(Ordering::Relaxed));
    }
}
