//! 点播 HLS 的总大小估算——只服务于下载中的进度展示。
//!
//! HLS 在全部分段落盘前不知道产物大小。估值只进入下载中的进度帧与暂停 / 出错时
//! 落库的展示值,不参与任何完成判定、续传校验或产物大小:收尾前总量收敛为实际已
//! 落盘字节,完成帧与库内终值一律取实际产物大小。
//!
//! # 算法
//!
//! 总量 = 已知大小的分段之和 + 未知分段的预测。逐段证据按可信度从低到高:
//! EXT-X-BYTERANGE 声明长度 → 响应头 Content-Length(在途)→ 已下载待写盘的实际
//! 字节 → 已写盘字节(含 init 段);续传前缀按整体字节计入。并发下载时乱序窗口里
//! 已下完和在途的分段远多于已写盘前缀,因此起步阶段就有多段真实样本。
//!
//! 未知分段的预测:
//! - 每段都带 EXT-X-BITRATE 时,按标注字节(码率 × 时长)乘以已知段的「实际 / 标注」
//!   校准比;
//! - 否则默认按已知段的平均段大小 × 未知段数。真实流里段大小主要随画面内容而非
//!   EXTINF 的小幅差异变化,按时长外推反而更差;只有时长明显混杂(如广告插段,变异
//!   系数 ≥ 0.3)且样本足够时才拟合 `大小 = a + b × 时长`(关键帧等固定开销 + 按时长
//!   增长的部分),预测夹在「按段数」与「按时长」两种外推之间;
//! - 所选变体声明 AVERAGE-BANDWIDTH 时,把它当作 [`PRIOR_WEIGHT_SECS`] 秒的等效观测
//!   与数据预测加权(有独立音轨时先扣除音轨实测码率);BANDWIDTH 是峰值,只在没有
//!   任何证据时给出初值,不参与加权。
//!
//! 在 10 条公开点播流(Apple / Mux / JW / Unified / Shaka 等)的真实分段大小上回放
//! (8 连接):全程平均误差 2.4%,yt-dlp(段数外推 + 当前分段长度)7.9%、N_m3u8DL-RE
//! (完成段数外推)4.1%;下载 5% 之后的最差误差 20%,两者为 202% / 48%。单连接时
//! 5.7% 对两者的 7.9%。逐条流比较不劣于两者中较好的一个。
//!
//! # 不变式
//!
//! - 任一轨还有未落盘分段时,总量 ≥ 已落盘字节 + 剩余段数(每段至少 1 字节),
//!   进度严格小于 100%,不会提前满格;
//! - 全部分段落盘后总量恰等于已落盘字节,进度 = 100%,不会卡在 99%;
//! - 估值偏多偏少只影响展示。

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::HlsSegment;

/// 码率先验折算成的等效观测时长(秒)。在 10 条真实流上回放:120–300s 之间误差最低,
/// 更大则在先验偏离实际的流上拖累明显。
const PRIOR_WEIGHT_SECS: f64 = 300.0;
/// 仿射模型至少需要的逐段样本数。
const AFFINE_MIN_SAMPLES: f64 = 8.0;
/// 仿射模型要求的已知段时长变异系数下限:常规播放列表(EXTINF 抖动 < 0.2)上拟合只会
/// 过拟合噪声,只有明显混杂的时长才能可靠地分离固定开销与时长项。
const AFFINE_MIN_DURATION_CV: f64 = 0.3;

/// 一段大小证据的可信度;低等级不会覆盖高等级。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Evidence {
    /// 播放列表 EXT-X-BYTERANGE 声明的长度。
    Declared,
    /// 响应头里的 Content-Length(或 BYTERANGE 长度),body 尚在传输。
    Announced,
    /// 已下载(并解密)、等待按序写盘的实际字节。
    Downloaded,
    /// 已写盘字节(含随该段写入的 init 段)。
    Written,
}

/// 一条点播轨解析后不变的事实。
struct TrackShape {
    /// 每段时长(秒);非有限 / 非正的 EXTINF 记 0。
    secs: Vec<f64>,
    /// 每段 EXT-X-BITRATE 折算的标注字节;任一段缺失时为 `None`(权重不一致不可用)。
    tagged: Option<Vec<f64>>,
    /// 每段 EXT-X-BYTERANGE 声明长度。
    declared: Vec<Option<u64>>,
}

impl TrackShape {
    fn of(segments: &[HlsSegment]) -> Self {
        let secs: Vec<f64> = segments
            .iter()
            .map(|s| {
                let d = f64::from(s.duration);
                if d.is_finite() && d > 0.0 { d } else { 0.0 }
            })
            .collect();
        let tagged = segments
            .iter()
            .zip(&secs)
            .map(|(s, d)| {
                s.bitrate_kbps
                    .filter(|kbps| *kbps > 0 && *d > 0.0)
                    .map(|kbps| kbps as f64 * 125.0 * d)
            })
            .collect();
        Self {
            secs,
            tagged,
            declared: segments
                .iter()
                .map(|s| s.byte_range.map(|(_, length)| length))
                .collect(),
        }
    }

    fn len(&self) -> usize {
        self.secs.len()
    }

    fn total_secs(&self) -> f64 {
        self.secs.iter().sum()
    }

    fn tag(&self, idx: usize) -> f64 {
        self.tagged
            .as_ref()
            .and_then(|t| t.get(idx).copied())
            .unwrap_or(0.0)
    }
}

/// 逐段样本 `(时长, 字节)` 的累计矩,支持增删(证据升级时先删旧值)。
#[derive(Clone, Copy, Default)]
struct Moments {
    n: f64,
    d: f64,
    s: f64,
    dd: f64,
    ds: f64,
}

impl Moments {
    fn add(&mut self, d: f64, s: f64, sign: f64) {
        self.n += sign;
        self.d += sign * d;
        self.s += sign * s;
        self.dd += sign * d * d;
        self.ds += sign * d * s;
    }

    /// 拟合 `s = a + b·d`;样本不足、时长离散度不够或系数为负(物理上不合理)时 `None`。
    fn affine(&self) -> Option<(f64, f64)> {
        if self.n < AFFINE_MIN_SAMPLES {
            return None;
        }
        let mean_d = self.d / self.n;
        let mean_s = self.s / self.n;
        let var_d = self.dd / self.n - mean_d * mean_d;
        if mean_d <= 0.0 || var_d <= 0.0 || var_d.sqrt() / mean_d < AFFINE_MIN_DURATION_CV {
            return None;
        }
        let b = (self.ds / self.n - mean_d * mean_s) / var_d;
        let a = mean_s - b * mean_d;
        (a >= 0.0 && b >= 0.0).then_some((a, b))
    }
}

/// 一条轨的大小证据(受锁保护)。
struct TrackState {
    /// 续传前缀:已完整落盘、只知道合计字节的前若干段。
    prefix_segments: usize,
    prefix_bytes: f64,
    prefix_secs: f64,
    prefix_tagged: f64,
    /// 前缀之后每段的已知大小与证据等级。
    sizes: Vec<Option<(u64, Evidence)>>,
    /// 有逐段证据的样本(不含前缀)。
    known: Moments,
    known_tagged: f64,
    unknown_segments: usize,
    unknown_secs: f64,
    unknown_tagged: f64,
}

impl TrackState {
    fn new(shape: &TrackShape, prefix_segments: usize, prefix_bytes: i64) -> Self {
        let prefix_segments = prefix_segments.min(shape.len());
        let mut state = Self {
            prefix_segments,
            prefix_bytes: prefix_bytes.max(0) as f64,
            prefix_secs: shape.secs.iter().take(prefix_segments).sum(),
            prefix_tagged: (0..prefix_segments).map(|i| shape.tag(i)).sum(),
            sizes: vec![None; shape.len()],
            known: Moments::default(),
            known_tagged: 0.0,
            unknown_segments: 0,
            unknown_secs: 0.0,
            unknown_tagged: 0.0,
        };
        for (idx, secs) in shape.secs.iter().enumerate().skip(prefix_segments) {
            state.unknown_segments += 1;
            state.unknown_secs += secs;
            state.unknown_tagged += shape.tag(idx);
        }
        for (idx, declared) in shape.declared.iter().enumerate().skip(prefix_segments) {
            if let Some(length) = declared {
                state.observe(shape, idx, *length, Evidence::Declared);
            }
        }
        state
    }

    fn observe(&mut self, shape: &TrackShape, idx: usize, size: u64, evidence: Evidence) {
        if idx < self.prefix_segments {
            return;
        }
        let (Some(slot), Some(&d)) = (self.sizes.get(idx).copied(), shape.secs.get(idx)) else {
            return;
        };
        let tag = shape.tag(idx);
        match slot {
            Some((_, current)) if evidence < current => return,
            Some((old, _)) => {
                self.known.add(d, old as f64, -1.0);
                self.known_tagged -= tag;
            }
            None => {
                self.unknown_segments = self.unknown_segments.saturating_sub(1);
                self.unknown_secs -= d;
                self.unknown_tagged -= tag;
            }
        }
        self.known.add(d, size as f64, 1.0);
        self.known_tagged += tag;
        if let Some(entry) = self.sizes.get_mut(idx) {
            *entry = Some((size, evidence));
        }
    }

    fn facts(&self, shape: &TrackShape) -> TrackFacts {
        let unknown_secs = if self.unknown_segments == 0 {
            0.0
        } else {
            self.unknown_secs.max(0.0)
        };
        TrackFacts {
            known_segments: self.prefix_segments as f64 + self.known.n,
            known_bytes: self.prefix_bytes + self.known.s,
            known_secs: self.prefix_secs + self.known.d,
            unknown_segments: self.unknown_segments,
            unknown_secs,
            affine: self.known.affine(),
            tagged: shape.tagged.as_ref().map(|_| Tagged {
                known: self.prefix_tagged + self.known_tagged,
                unknown: self.unknown_tagged.max(0.0),
            }),
            total_secs: shape.total_secs(),
        }
    }
}

/// EXT-X-BITRATE 标注字节在已知 / 未知分段上的合计。
#[derive(Clone, Copy)]
struct Tagged {
    known: f64,
    unknown: f64,
}

/// 一次采样时单轨的证据汇总。
#[derive(Clone, Copy)]
struct TrackFacts {
    known_segments: f64,
    known_bytes: f64,
    known_secs: f64,
    unknown_segments: usize,
    unknown_secs: f64,
    affine: Option<(f64, f64)>,
    tagged: Option<Tagged>,
    total_secs: f64,
}

impl TrackFacts {
    /// 已知分段的实测码率(字节 / 秒)。
    fn measured_rate(&self) -> Option<f64> {
        (self.known_secs > 0.0).then(|| self.known_bytes / self.known_secs)
    }

    /// 只看本轨数据的未知分段预测;没有任何已知分段时 `None`。
    fn data_prediction(&self) -> Option<f64> {
        if self.unknown_segments == 0 {
            return Some(0.0);
        }
        if self.known_segments <= 0.0 {
            return None;
        }
        let unknown = self.unknown_segments as f64;
        let by_count = self.known_bytes / self.known_segments * unknown;
        let by_secs = self
            .measured_rate()
            .map_or(by_count, |rate| rate * self.unknown_secs);
        let (lo, hi) = (by_count.min(by_secs), by_count.max(by_secs));
        Some(match self.affine {
            Some((a, b)) => (a * unknown + b * self.unknown_secs).clamp(lo, hi),
            None => by_count,
        })
    }

    /// 本轨估算总字节;`prior_rate` 为可与数据加权的码率先验(字节 / 秒)。
    fn estimate(&self, prior_rate: Option<f64>) -> Option<f64> {
        let k = self.known_secs;
        let unknown = if let Some(tagged) = self.tagged {
            // 校准比向 1(标注本身)收缩,收缩强度同码率先验。
            let ratio = if tagged.known > 0.0 {
                self.known_bytes / tagged.known
            } else {
                1.0
            };
            tagged.unknown * (ratio * k + PRIOR_WEIGHT_SECS) / (k + PRIOR_WEIGHT_SECS)
        } else {
            // 未知分段没有可用时长时码率先验无从折算。
            let prior = prior_rate
                .filter(|_| self.unknown_secs > 0.0)
                .map(|rate| rate * self.unknown_secs);
            match (self.data_prediction(), prior) {
                (Some(data), Some(prior)) if k > 0.0 => {
                    (data * k + prior * PRIOR_WEIGHT_SECS) / (k + PRIOR_WEIGHT_SECS)
                }
                (Some(data), _) => data,
                (None, Some(prior)) => prior,
                (None, None) => return None,
            }
        };
        Some(self.known_bytes + unknown)
    }
}

/// 一条点播轨的进度与大小证据:写盘方 / 分段任务推进,进度采样方读取。
pub(super) struct TrackProgress {
    shape: TrackShape,
    written: AtomicI64,
    done: AtomicUsize,
    // usize::MAX until the writer has validated or reset its resume prefix.
    checkpointed: AtomicUsize,
    state: Mutex<TrackState>,
}

/// 一次读取到的单轨进度。
struct TrackSample {
    written: i64,
    done: usize,
    segments: usize,
    facts: TrackFacts,
}

impl TrackSample {
    /// 下限 = 已落盘字节 + 剩余段数。
    fn floor(&self) -> i64 {
        let remaining = i64::try_from(self.segments.saturating_sub(self.done)).unwrap_or(i64::MAX);
        self.written.saturating_add(remaining)
    }

    /// 按不变式收口:全部落盘 = 已落盘字节,否则不低于下限。
    fn settle(&self, estimate: Option<f64>) -> Option<i64> {
        if self.done >= self.segments {
            return Some(self.written);
        }
        estimate.map(|e| to_bytes(e).max(self.floor()))
    }
}

impl TrackProgress {
    pub(super) fn new(segments: &[HlsSegment]) -> Self {
        let shape = TrackShape::of(segments);
        let state = TrackState::new(&shape, 0, 0);
        Self {
            shape,
            written: AtomicI64::new(0),
            done: AtomicUsize::new(0),
            checkpointed: AtomicUsize::new(usize::MAX),
            state: Mutex::new(state),
        }
    }

    fn lock(&self) -> MutexGuard<'_, TrackState> {
        // 状态只有求和计数,中途 panic 也不会留下破坏不变式的半成品;展示值继续可用。
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 本次运行从续传前缀开始:前 `done` 段已落盘,共 `written` 字节。
    pub(super) fn resume(&self, written: i64, done: usize) {
        *self.lock() = TrackState::new(&self.shape, done, written);
        self.written.store(written, Ordering::Relaxed);
        self.done.store(done, Ordering::Release);
        self.mark_checkpoint(done);
    }

    pub(super) fn mark_checkpoint(&self, done: usize) {
        self.checkpointed.store(done, Ordering::Release);
    }

    fn checkpointed(&self) -> Option<u64> {
        let done = self.checkpointed.load(Ordering::Acquire);
        (done != usize::MAX).then_some(done as u64)
    }

    /// 第 `idx` 段的响应头已到,body 预计 `length` 字节。
    pub(super) fn announce(&self, idx: usize, length: u64) {
        self.lock()
            .observe(&self.shape, idx, length, Evidence::Announced);
    }

    /// 第 `idx` 段已下载(并解密)完毕,共 `length` 字节,等待按序写盘。
    pub(super) fn downloaded(&self, idx: usize, length: usize) {
        self.lock()
            .observe(&self.shape, idx, length as u64, Evidence::Downloaded);
    }

    /// 第 `idx` 段(连同其 init 段)写盘 `length` 字节,本轨累计已落盘 `written` 字节。
    ///
    /// 先写字节、再以 Release 发布段数:读到「全部落盘」的采样方必然读到最终字节数。
    pub(super) fn wrote(&self, idx: usize, length: i64, written: i64) {
        self.lock().observe(
            &self.shape,
            idx,
            u64::try_from(length).unwrap_or(0),
            Evidence::Written,
        );
        self.written.store(written, Ordering::Relaxed);
        self.done.store(idx + 1, Ordering::Release);
    }

    pub(super) fn written(&self) -> i64 {
        self.written.load(Ordering::Relaxed)
    }

    fn sample(&self) -> TrackSample {
        // 先读段数(Acquire)再读字节:字节只会比段数新,不会出现「段数已满、字节未到」。
        let done = self.done.load(Ordering::Acquire);
        let written = self.written();
        let facts = self.lock().facts(&self.shape);
        TrackSample {
            written,
            done,
            segments: self.shape.len(),
            facts,
        }
    }
}

/// 所选变体在主播放列表里声明的码率(bit/s)。
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct VariantBitrate {
    /// AVERAGE-BANDWIDTH:整条变体的平均码率,可与数据加权。
    pub(super) average: Option<u64>,
    /// BANDWIDTH:峰值码率,只在没有任何证据时给出初值。
    pub(super) peak: Option<u64>,
}

/// 整个点播任务(视频 + 可选独立音轨)的进度。
pub(super) struct TaskProgress {
    video: Arc<TrackProgress>,
    audio: Option<Arc<TrackProgress>>,
    bitrate: VariantBitrate,
}

impl TaskProgress {
    pub(super) fn new(
        video: Arc<TrackProgress>,
        audio: Option<Arc<TrackProgress>>,
        bitrate: VariantBitrate,
    ) -> Self {
        Self {
            video,
            audio,
            bitrate,
        }
    }

    /// 已按序写入与计划写入的媒体分片数；init 段不单独计数。
    pub(super) fn fragments(&self) -> (u64, u64) {
        let counts = |track: &TrackProgress| {
            (
                track.done.load(Ordering::Acquire) as u64,
                track.shape.len() as u64,
            )
        };
        let (video_done, video_total) = counts(&self.video);
        let (audio_done, audio_total) = self.audio.as_deref().map_or((0, 0), counts);
        (video_done + audio_done, video_total + audio_total)
    }

    pub(super) fn checkpointed_fragments(&self) -> Option<(u64, u64)> {
        let video = self.video.checkpointed()?;
        let (audio, audio_total) = match &self.audio {
            Some(track) => (track.checkpointed()?, track.shape.len() as u64),
            None => (0, 0),
        };
        Some((video + audio, self.video.shape.len() as u64 + audio_total))
    }

    /// `(已落盘字节, 总字节)`;总字节 0 = 未知。
    pub(super) fn snapshot(&self) -> (i64, i64) {
        let video = self.video.sample();
        let audio = self.audio.as_ref().map(|track| track.sample());
        let written = video
            .written
            .saturating_add(audio.as_ref().map_or(0, |a| a.written));
        let floor = video
            .floor()
            .saturating_add(audio.as_ref().map_or(0, TrackSample::floor));

        // AVERAGE-BANDWIDTH 覆盖整条变体(含独立音轨):有音轨时扣除音轨实测码率后才是视频
        // 的先验,音轨尚无样本时暂不使用。
        let average = self.bitrate.average.map(|bps| bps as f64 / 8.0);
        let video_prior = match &audio {
            None => average,
            Some(a) => average.zip(a.facts.measured_rate()).map(|(r, ar)| r - ar),
        }
        .filter(|rate| *rate > 0.0);

        let total = match video.settle(video.facts.estimate(video_prior)) {
            Some(video_total) => video_total.saturating_add(audio.as_ref().map_or(0, |a| {
                // 音轨尚无依据时按下限计入:进度保持 < 100%,首个音频证据到达后补上估值。
                a.settle(a.facts.estimate(None))
                    .unwrap_or_else(|| a.floor())
            })),
            None => self
                .bitrate
                .average
                .or(self.bitrate.peak)
                .filter(|_| video.facts.total_secs > 0.0)
                .map_or(0, |bps| {
                    to_bytes(bps as f64 / 8.0 * video.facts.total_secs).max(floor)
                }),
        };
        (written, total)
    }
}

/// 估值转字节:四舍五入后 `as` 转换(f64 → 整数饱和,NaN 记 0)。
fn to_bytes(value: f64) -> i64 {
    value.round() as i64
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{TaskProgress, TrackProgress, VariantBitrate};
    use crate::hls_downloader::HlsSegment;

    fn segment(duration: f32) -> HlsSegment {
        HlsSegment {
            uri: "https://example.com/s.ts".to_owned(),
            duration,
            key: None,
            byte_range: None,
            bitrate_kbps: None,
            discontinuity: false,
            map: None,
            sequence: 0,
        }
    }

    fn segments(durations: &[f32]) -> Vec<HlsSegment> {
        durations.iter().copied().map(segment).collect()
    }

    fn task(video: &Arc<TrackProgress>, bitrate: VariantBitrate) -> TaskProgress {
        TaskProgress::new(Arc::clone(video), None, bitrate)
    }

    /// 按序写盘 `sizes`,返回每段写盘后的 `(written, total)`。
    fn write_all(progress: &TaskProgress, video: &TrackProgress, sizes: &[i64]) -> Vec<(i64, i64)> {
        let mut written = 0;
        sizes
            .iter()
            .enumerate()
            .map(|(i, size)| {
                written += size;
                video.wrote(i, *size, written);
                progress.snapshot()
            })
            .collect()
    }

    #[test]
    fn unfinished_never_reaches_full_and_finished_is_exact() {
        // 码率剧烈波动 + 首段极小,估值严重偏小的场景。
        let video = Arc::new(TrackProgress::new(&segments(&[6.0, 6.0, 6.0, 6.0, 2.0])));
        let progress = task(&video, VariantBitrate::default());
        let sizes = [10, 9_000, 50, 8_000_000, 1];
        let frames = write_all(&progress, &video, &sizes);
        let (last, rest) = frames.split_last().expect("one frame per segment");
        for (written, total) in rest {
            assert!(
                total > written,
                "unfinished must stay below 100%: {written}/{total}"
            );
        }
        let actual: i64 = sizes.iter().sum();
        assert_eq!(
            *last,
            (actual, actual),
            "all segments written must be exactly 100%"
        );
    }

    #[test]
    fn unknown_without_any_evidence_or_bitrate() {
        let video = Arc::new(TrackProgress::new(&segments(&[4.0, 4.0])));
        assert_eq!(task(&video, VariantBitrate::default()).snapshot(), (0, 0));
    }

    #[test]
    fn in_flight_and_unwritten_segments_count_before_anything_is_written() {
        // 并发下载:队首段在途(只知道 Content-Length),后面两段已下完待写盘。
        let video = Arc::new(TrackProgress::new(&segments(&[4.0; 6])));
        let progress = task(&video, VariantBitrate::default());
        video.announce(0, 1_000);
        video.downloaded(1, 1_000);
        video.downloaded(2, 1_000);
        assert_eq!(progress.snapshot(), (0, 6_000));
        // 低等级证据(重试时的新响应头)不能覆盖已下载的实际字节。
        video.announce(1, 9_999);
        assert_eq!(progress.snapshot(), (0, 6_000));
    }

    #[test]
    fn affine_model_only_for_clearly_mixed_durations() {
        // 大小 = 50_000(关键帧等固定开销)+ 10_000 × 时长,剩余段时长构成与已知段不同:
        // 按段数外推偏小(44 万)、按时长外推偏大(73 万),仿射模型命中实际的 60 万。
        let durations = [
            2.0, 10.0, 2.0, 10.0, 2.0, 10.0, 2.0, 10.0, 10.0, 10.0, 10.0, 10.0,
        ];
        let size = |d: f32| 50_000 + 10_000 * d as i64;
        let video = Arc::new(TrackProgress::new(&segments(&durations)));
        let progress = task(&video, VariantBitrate::default());
        let mut written = 0;
        for (i, d) in durations.iter().take(8).enumerate() {
            written += size(*d);
            video.wrote(i, size(*d), written);
        }
        let actual: i64 = durations.iter().map(|d| size(*d)).sum();
        assert_eq!(progress.snapshot(), (written, actual));

        // EXTINF 只是小幅抖动:按平均段大小外推,短尾段不按时长打折。
        let jitter = Arc::new(TrackProgress::new(&segments(&[9.0, 11.0, 9.0, 11.0, 2.0])));
        let progress = task(&jitter, VariantBitrate::default());
        for i in 0..4 {
            jitter.wrote(i, 100, 100 * (i as i64 + 1));
        }
        assert_eq!(progress.snapshot(), (400, 500));
    }

    #[test]
    fn bitrate_tags_are_calibrated_by_measured_segments() {
        let mut tagged = segments(&[4.0, 4.0, 4.0, 4.0]);
        for (segment, kbps) in tagged.iter_mut().zip([1_000, 4_000, 2_000, 8_000]) {
            segment.bitrate_kbps = Some(kbps);
        }
        let video = Arc::new(TrackProgress::new(&tagged));
        let progress = task(&video, VariantBitrate::default());
        // 无数据:标注本身(kbps × 125 × 秒)。
        assert_eq!(progress.snapshot(), (0, 7_500_000));
        // 实测比标注大 10%:校准比向标注收缩(先验强度 300s),已知段按实际计入。
        video.wrote(0, 550_000, 550_000);
        let (written, total) = progress.snapshot();
        assert_eq!(written, 550_000);
        let unknown_tagged = 7_000_000.0_f64;
        let expected = 550_000.0 + unknown_tagged * (1.1 * 4.0 + 300.0) / (4.0 + 300.0);
        assert!(
            (total as f64 - expected).abs() <= 1.0,
            "{total} vs {expected}"
        );
    }

    #[test]
    fn average_bandwidth_is_blended_and_peak_only_seeds_the_start() {
        let video = Arc::new(TrackProgress::new(&segments(&[10.0; 60])));
        let peak_only = task(
            &video,
            VariantBitrate {
                average: None,
                peak: Some(8_000_000),
            },
        );
        // 600s × 1 MB/s。
        assert_eq!(peak_only.snapshot(), (0, 600_000_000));
        video.wrote(0, 500_000, 500_000);
        assert_eq!(
            peak_only.snapshot(),
            (500_000, 30_000_000),
            "peak bandwidth must not drag a measured estimate"
        );
        let average = task(
            &video,
            VariantBitrate {
                average: Some(8_000_000),
                peak: Some(16_000_000),
            },
        );
        // 未知 590s:数据 50 KB/s 权重 10s,先验 1 MB/s 权重 300s。
        let unknown = (50_000.0 * 590.0 * 10.0 + 1_000_000.0 * 590.0 * 300.0) / 310.0;
        let (_, total) = average.snapshot();
        assert!(
            (total as f64 - (500_000.0 + unknown)).abs() <= 1.0,
            "{total}"
        );
    }

    #[test]
    fn average_bandwidth_excludes_the_separate_audio_track() {
        let video = Arc::new(TrackProgress::new(&segments(&[10.0; 10])));
        let audio = Arc::new(TrackProgress::new(&segments(&[10.0; 10])));
        let progress = TaskProgress::new(
            Arc::clone(&video),
            Some(Arc::clone(&audio)),
            VariantBitrate {
                average: Some(1_160_000),
                peak: None,
            },
        );
        // 音轨实测 20 KB/s,变体平均 145 KB/s:视频先验 125 KB/s × 100s。
        audio.wrote(0, 200_000, 200_000);
        let (_, total) = progress.snapshot();
        let audio_total = 200_000 + 9 * 200_000;
        assert_eq!(total, 12_500_000 + audio_total);
    }

    #[test]
    fn fragment_progress_counts_resumed_and_written_media_on_both_tracks() {
        let video = Arc::new(TrackProgress::new(&segments(&[5.0; 3])));
        let audio = Arc::new(TrackProgress::new(&segments(&[5.0; 2])));
        video.resume(1_000, 1);
        let progress = TaskProgress::new(
            Arc::clone(&video),
            Some(Arc::clone(&audio)),
            VariantBitrate::default(),
        );
        assert_eq!(progress.checkpointed_fragments(), None);
        audio.resume(100, 1);
        assert_eq!(progress.checkpointed_fragments(), Some((2, 5)));
        assert_eq!(progress.fragments(), (2, 5));

        video.downloaded(2, 10_000);
        assert_eq!(progress.fragments(), (2, 5));
        video.wrote(1, 500, 1_500);
        video.wrote(2, 10_000, 11_500);
        assert_eq!(progress.fragments(), (4, 5));
        audio.wrote(1, 50, 150);
        assert_eq!(progress.fragments(), (5, 5));
        // Only the video checkpoint advanced; the audio tail will be redone.
        video.mark_checkpoint(3);
        assert_eq!(progress.checkpointed_fragments(), Some((4, 5)));
    }

    #[test]
    fn resume_prefix_counts_as_measured_bytes() {
        let video = Arc::new(TrackProgress::new(&segments(&[5.0; 10])));
        let progress = task(&video, VariantBitrate::default());
        video.resume(4_000, 4);
        assert_eq!(progress.snapshot(), (4_000, 10_000));
        video.wrote(4, 1_000, 5_000);
        assert_eq!(progress.snapshot(), (5_000, 10_000));
    }

    #[test]
    fn declared_byte_ranges_are_exact_and_clamped() {
        let mut ranged = segments(&[4.0, 4.0, 4.0]);
        for (i, segment) in ranged.iter_mut().enumerate() {
            segment.byte_range = Some((i as u64 * 10, 10));
        }
        let video = Arc::new(TrackProgress::new(&ranged));
        let progress = task(&video, VariantBitrate::default());
        assert_eq!(progress.snapshot(), (0, 30));
        // 声明长度偏小(服务器与播放列表不一致)也不得越过 100% 或提前满格。
        video.wrote(0, 500, 500);
        assert_eq!(progress.snapshot(), (500, 520));
        video.wrote(1, 500, 1_000);
        video.wrote(2, 500, 1_500);
        assert_eq!(progress.snapshot(), (1_500, 1_500));
    }

    #[test]
    fn audio_track_keeps_task_below_full_until_it_finishes() {
        let video = Arc::new(TrackProgress::new(&segments(&[5.0, 5.0])));
        let audio = Arc::new(TrackProgress::new(&segments(&[5.0, 5.0])));
        let progress = TaskProgress::new(
            Arc::clone(&video),
            Some(Arc::clone(&audio)),
            VariantBitrate::default(),
        );
        video.wrote(0, 1_000, 1_000);
        video.wrote(1, 1_000, 2_000);
        let (written, total) = progress.snapshot();
        assert_eq!(written, 2_000);
        assert!(total > written, "audio pending: {written}/{total}");
        audio.wrote(0, 100, 100);
        assert_eq!(progress.snapshot(), (2_100, 2_200));
        audio.wrote(1, 110, 210);
        assert_eq!(progress.snapshot(), (2_210, 2_210));
    }

    #[test]
    fn invalid_durations_fall_back_to_segment_count() {
        let video = Arc::new(TrackProgress::new(&segments(&[
            0.0,
            f32::NAN,
            -3.0,
            f32::INFINITY,
        ])));
        let progress = task(
            &video,
            VariantBitrate {
                average: Some(1_000_000),
                peak: Some(2_000_000),
            },
        );
        assert_eq!(
            progress.snapshot(),
            (0, 0),
            "no duration: bitrate gives nothing"
        );
        video.wrote(0, 100, 100);
        assert_eq!(progress.snapshot(), (100, 400));
    }

    #[test]
    fn huge_values_saturate_instead_of_wrapping() {
        let video = Arc::new(TrackProgress::new(&segments(&[0.001, 1.0e9])));
        let progress = task(
            &video,
            VariantBitrate {
                average: None,
                peak: Some(u64::MAX),
            },
        );
        assert_eq!(progress.snapshot(), (0, i64::MAX));
        video.wrote(0, i64::MAX / 2, i64::MAX / 2);
        assert_eq!(progress.snapshot(), (i64::MAX / 2, i64::MAX));
    }
}
