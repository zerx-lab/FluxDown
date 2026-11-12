//! 界面速度的统一口径：固定时间窗采样 + EMA 平滑。
//!
//! 下载速度（`download_manager::progress_reporter`）与 BT 上传速度（下载期与做种期）
//! 都按这里的窗口与系数折算，限速值与显示值才可比：限速器允许短时突发（librqbit 桶容量
//! 为 1 秒的配额），若按亚秒窗口取瞬时值，突发会显示成数倍于限速的速度。

use std::time::Instant;

/// EMA 平滑系数。与 1 秒采样窗配合，阶跃变化约 3–4 个窗口收敛到 90%。
pub(crate) const EMA_ALPHA: f64 = 0.4;

/// 稳态采样窗（ms）：窗口内累计字节，窗口到期才折算一次速度。
pub(crate) const SPEED_SAMPLE_INTERVAL_MS: u128 = 1_000;

/// 下载起步的种子短窗（ms），只用于进入下载态后的第一个估计（见 `progress_reporter`）。
pub(crate) const SPEED_SEED_INTERVAL_MS: u128 = 300;

/// 整窗无新字节时的衰减系数：每窗减半。
pub(crate) const SPEED_DECAY_FACTOR: f64 = 0.5;

/// 衰减到此值（B/s）以下即归零。
pub(crate) const SPEED_ZERO_FLOOR: f64 = 1024.0;

/// 单调字节计数器（如 librqbit 的 `uploaded_bytes`）的显示速率。
///
/// 从 0 起按 EMA 爬升，不直接采用首个窗口：限速器在空闲后放出的整桶突发只会被摊进
/// 窗口均值，不会显示成超出限速的尖峰；持续的超限照常在数个窗口内收敛到真实值。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CounterRate {
    /// 当前采样窗起点（计数器读数, 时刻）；尚无读数时为 `None`。
    window_start: Option<(u64, Instant)>,
    ema: f64,
}

impl CounterRate {
    /// 喂入一次计数器读数，返回当前显示速率（B/s）。
    ///
    /// 读数回退（暂停 / 恢复或会话重建后计数器从 0 起）视为新一轮计数：重设窗口起点，
    /// 保留已有估计。
    pub(crate) fn observe(&mut self, counter: u64, now: Instant) -> i64 {
        let Some((start_bytes, start_at)) = self.window_start else {
            self.window_start = Some((counter, now));
            return self.bps();
        };
        if counter < start_bytes {
            self.window_start = Some((counter, now));
            return self.bps();
        }
        let elapsed = now.saturating_duration_since(start_at);
        if elapsed.as_millis() < SPEED_SAMPLE_INTERVAL_MS {
            return self.bps();
        }
        let delta = counter - start_bytes;
        if delta > 0 {
            let window_speed = delta as f64 / elapsed.as_secs_f64();
            self.ema = EMA_ALPHA * window_speed + (1.0 - EMA_ALPHA) * self.ema;
        } else {
            self.ema *= SPEED_DECAY_FACTOR;
            if self.ema < SPEED_ZERO_FLOOR {
                self.ema = 0.0;
            }
        }
        self.window_start = Some((counter, now));
        self.bps()
    }

    /// 当前显示速率（B/s）。
    pub(crate) fn bps(&self) -> i64 {
        self.ema as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MIB: u64 = 1024 * 1024;

    /// 每 `step_ms` 喂一次读数，`rate` 为每秒字节；返回最后的显示速率。
    fn feed(
        meter: &mut CounterRate,
        start: Instant,
        counter: &mut u64,
        from_ms: u64,
        to_ms: u64,
        step_ms: u64,
        rate: u64,
    ) -> i64 {
        let mut shown = meter.bps();
        let mut t = from_ms;
        while t < to_ms {
            t += step_ms;
            *counter += rate * step_ms / 1000;
            shown = meter.observe(*counter, start + Duration::from_millis(t));
        }
        shown
    }

    #[test]
    fn limiter_burst_after_idle_never_shows_above_the_burst_window_mean() {
        // 1 MiB/s 限速：空闲后整桶（1 MiB）在 100ms 内放出，随后按限速匀速。
        let start = Instant::now();
        let mut meter = CounterRate::default();
        let mut counter = 0;
        meter.observe(counter, start);
        counter += MIB;
        let mut peak = meter.observe(counter, start + Duration::from_millis(100));
        let mut t = 100;
        while t < 10_000 {
            t += 100;
            counter += MIB / 10;
            peak = peak.max(meter.observe(counter, start + Duration::from_millis(t)));
        }
        // 首窗均值 ≈ 1.9 MiB/s，经 EMA 后显示不超过限速。
        assert!(
            peak <= MIB as i64,
            "peak {peak} must stay at or below the limit"
        );
    }

    #[test]
    fn sustained_overshoot_converges_to_the_real_rate() {
        let start = Instant::now();
        let mut meter = CounterRate::default();
        let mut counter = 0;
        meter.observe(counter, start);
        let real = MIB * 13 / 10;
        let shown = feed(&mut meter, start, &mut counter, 0, 6_000, 100, real);
        let ratio = shown as f64 / real as f64;
        assert!(ratio > 0.9 && ratio <= 1.0, "shown {shown} vs real {real}");
    }

    #[test]
    fn counter_reset_rebaselines_without_negative_or_spiking_rate() {
        let start = Instant::now();
        let mut meter = CounterRate::default();
        let mut counter = 0;
        meter.observe(counter, start);
        let before = feed(&mut meter, start, &mut counter, 0, 5_000, 100, MIB);
        // 暂停 / 恢复：计数器从 0 重新计。
        let mut counter = 0;
        let after_reset = meter.observe(counter, start + Duration::from_millis(5_100));
        assert_eq!(after_reset, before);
        let after = feed(&mut meter, start, &mut counter, 5_100, 10_000, 100, MIB);
        assert!(after <= MIB as i64 && after > (MIB as i64) * 9 / 10);
    }

    #[test]
    fn stalled_counter_decays_to_zero() {
        let start = Instant::now();
        let mut meter = CounterRate::default();
        let mut counter = 0;
        meter.observe(counter, start);
        feed(&mut meter, start, &mut counter, 0, 5_000, 100, MIB);
        let shown = feed(&mut meter, start, &mut counter, 5_000, 20_000, 500, 0);
        assert_eq!(shown, 0);
    }
}
