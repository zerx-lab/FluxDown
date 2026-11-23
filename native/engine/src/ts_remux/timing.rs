//! 样本时间线：把 TS 时间戳（90kHz、33 位回绕、可能不连续）换算成 MP4 的
//! `stts` / `ctts` / `elst` 所需的数值。
//!
//! - 视频（时基 90kHz）：样本时长 = 相邻 DTS（缺省用 PTS）之差。回退、重置或超过
//!   [`DISCONTINUITY_90K`] 的跳变（`EXT-X-DISCONTINUITY`、插播）记为断点，与缺时间戳的帧
//!   一样按名义帧长（相邻差的中位数）衔接；不超过该值的真实空洞原样保留。
//! - 音频（时基 = 采样率）：每个 AAC 帧 1024 采样。PES 时间戳比按帧长推算的位置晚
//!   超过半帧时视为空洞，差值并入前一帧时长；回退或超过阈值的跳变记为断点、重定锚点。
//! - 两轨起点按各自首个呈现时刻对齐，晚开始的一轨用空编辑推迟。两轨断点数相同时
//!   逐个配对：同一断点后两轨在源时间轴上的相对位置不变，落后的一轨把差值并入断点前
//!   最后一个样本，各段内容时长不一致（编码器补齐、插播素材）不会跨段累积成不同步。

/// 电影时基（`mvhd` / `tkhd` / `elst` 段时长），同时是视频媒体时基。
pub(super) const MOVIE_TIMESCALE: u32 = 90_000;
/// 每个 AAC 帧（ADTS 单个 raw data block）的采样数。
pub(super) const AAC_FRAME_SAMPLES: u32 = 1024;
const PTS_MODULUS: u64 = 1 << 33;
/// 相邻时间戳前跳超过 10 秒视为不连续，而不是内容空洞。
const DISCONTINUITY_90K: i64 = 10 * MOVIE_TIMESCALE as i64;
/// 视频没有任何可用时间戳时的帧长（30fps）。
const FALLBACK_VIDEO_DELTA: u32 = 3000;

/// 游程编码的 (样本数, 值) 表，即 `stts` / `ctts` 条目。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Runs(pub Vec<(u32, u32)>);

impl Runs {
    pub fn from_values(values: &[u32]) -> Self {
        let mut runs = Self::default();
        for &value in values {
            runs.push(value);
        }
        runs
    }

    fn push(&mut self, value: u32) {
        if let Some((count, last)) = self.0.last_mut()
            && *last == value
            && *count < u32::MAX
        {
            *count += 1;
            return;
        }
        self.0.push((1, value));
    }
}

/// 时间戳不连续处：`sample` 是新段第一个样本，`source` 是它的源时间戳（视频为 DTS），
/// `position` 是它按前一段推算的媒体时间。
#[derive(Debug, Clone, Copy)]
struct Break {
    sample: usize,
    source: u64,
    position: u64,
}

/// 一条轨的时间线（媒体时基下）。
#[derive(Debug)]
pub(super) struct TrackTiming {
    pub timescale: u32,
    /// 逐样本时长。
    pub durations: Vec<u32>,
    /// 合成时间偏移；全为 0 时为空（不写 `ctts`）。
    pub ctts: Runs,
    /// 首个呈现样本的媒体时间（`elst` 的 media_time）。
    pub media_start: u64,
    /// `media_start` 对应的源时间戳（90kHz、33 位）；整条轨没有时间戳时为 `None`。
    pub source_start: Option<u64>,
    breaks: Vec<Break>,
}

impl TrackTiming {
    /// 全部样本时长之和（媒体时基）。
    pub fn media_duration(&self) -> u64 {
        self.durations.iter().map(|&d| u64::from(d)).sum()
    }

    /// 呈现时长（电影时基）。
    pub fn presented_movie(&self) -> u64 {
        self.to_movie(self.media_duration().saturating_sub(self.media_start))
    }

    fn to_movie(&self, media: u64) -> u64 {
        rescale(media, MOVIE_TIMESCALE, self.timescale)
    }
}

/// `value` 从时基 `from` 换算到 `to`（四舍五入）。
fn rescale(value: u64, to: u32, from: u32) -> u64 {
    let from = u128::from(from.max(1));
    let scaled = (u128::from(value) * u128::from(to) + from / 2) / from;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// 33 位时间戳差 `to − from`，按回绕取绝对值最小的有符号值。
fn pts_delta(from: u64, to: u64) -> i64 {
    let delta = to.wrapping_sub(from) & (PTS_MODULUS - 1);
    if delta >= PTS_MODULUS / 2 {
        delta as i64 - PTS_MODULUS as i64
    } else {
        delta as i64
    }
}

/// 33 位时间戳加有符号偏移（回绕）。
fn pts_add(base: u64, delta: i64) -> u64 {
    (base as i64)
        .wrapping_add(delta)
        .rem_euclid(PTS_MODULUS as i64) as u64
}

fn decode_time(stamp: (Option<u64>, Option<u64>)) -> Option<u64> {
    stamp.1.or(stamp.0)
}

/// 名义帧长：相邻两个已知解码时间之差（按间隔帧数均摊）的中位数。
fn nominal_video_delta(timestamps: &[(Option<u64>, Option<u64>)]) -> u32 {
    let mut deltas = Vec::new();
    let mut prev: Option<(usize, u64)> = None;
    for (index, &stamp) in timestamps.iter().enumerate() {
        let Some(dts) = decode_time(stamp) else {
            continue;
        };
        if let Some((prev_index, prev_dts)) = prev {
            let diff = pts_delta(prev_dts, dts);
            let frames = (index - prev_index) as i64;
            if diff > 0 && diff <= DISCONTINUITY_90K && diff / frames > 0 {
                deltas.push((diff / frames) as u32);
            }
        }
        prev = Some((index, dts));
    }
    if deltas.is_empty() {
        return FALLBACK_VIDEO_DELTA;
    }
    let middle = deltas.len() / 2;
    *deltas.select_nth_unstable(middle).1
}

/// 视频轨时间线；`timestamps` 与视频样本一一对应。
pub(super) fn video(timestamps: &[(Option<u64>, Option<u64>)]) -> TrackTiming {
    let nominal = nominal_video_delta(timestamps);
    let mut durations = Vec::with_capacity(timestamps.len());
    let mut ctts = Runs::default();
    let mut has_offsets = false;
    let mut breaks = Vec::new();
    let mut media_time: u64 = 0;
    let mut media_start: Option<u64> = None;
    // 媒体时间 0 对应的源解码时间。
    let mut origin: Option<u64> = None;
    // 当前样本的源解码时间（缺时间戳时按名义帧长推算）。
    let mut cursor: Option<u64> = None;

    for (index, &stamp) in timestamps.iter().enumerate() {
        let dts = decode_time(stamp);
        if origin.is_none()
            && let Some(dts) = dts
        {
            origin = Some(pts_add(dts, -(media_time as i64)));
            cursor = Some(dts);
        }

        let offset = match (stamp.0, dts) {
            (Some(pts), Some(dts)) => {
                let offset = pts_delta(dts, pts);
                if (0..=DISCONTINUITY_90K).contains(&offset) {
                    offset as u32
                } else {
                    0
                }
            }
            _ => 0,
        };
        has_offsets |= offset != 0;
        ctts.push(offset);
        let composition = media_time + u64::from(offset);
        media_start = Some(media_start.map_or(composition, |start| start.min(composition)));

        let next = timestamps.get(index + 1).copied().and_then(decode_time);
        let delta = match (cursor, next) {
            (Some(current), Some(next)) => {
                let diff = pts_delta(current, next);
                cursor = Some(next);
                if diff > 0 && diff <= DISCONTINUITY_90K {
                    diff as u32
                } else {
                    breaks.push(Break {
                        sample: index + 1,
                        source: next,
                        position: media_time + u64::from(nominal),
                    });
                    nominal
                }
            }
            (Some(current), None) => {
                cursor = Some(pts_add(current, i64::from(nominal)));
                nominal
            }
            (None, _) => nominal,
        };
        durations.push(delta);
        media_time += u64::from(delta);
    }

    let media_start = media_start.unwrap_or(0);
    TrackTiming {
        timescale: MOVIE_TIMESCALE,
        durations,
        ctts: if has_offsets { ctts } else { Runs::default() },
        media_start,
        source_start: origin.map(|origin| pts_add(origin, media_start as i64)),
        breaks,
    }
}

/// 音频轨时间线；`timestamps` 与 AAC 帧一一对应，时基为 `sample_rate`。
pub(super) fn audio(timestamps: &[Option<u64>], sample_rate: u32) -> TrackTiming {
    let frame = u64::from(AAC_FRAME_SAMPLES);
    let half_frame_90k = (rescale(frame, MOVIE_TIMESCALE, sample_rate) / 2) as i64;
    let mut durations = Vec::with_capacity(timestamps.len());
    let mut breaks = Vec::new();
    let mut position: u64 = 0;
    // (源时间戳, 对应的媒体位置)：推算后续帧应在的源时刻。
    let mut anchor: Option<(u64, u64)> = None;
    let mut origin: Option<u64> = None;

    for (index, &pts) in timestamps.iter().enumerate() {
        if anchor.is_none()
            && let Some(pts) = pts
        {
            anchor = Some((pts, position));
            origin = Some(pts_add(
                pts,
                -(rescale(position, MOVIE_TIMESCALE, sample_rate) as i64),
            ));
        }

        let mut duration = frame;
        let next = timestamps.get(index + 1).copied().flatten();
        if let (Some(next), Some((anchor_pts, anchor_position))) = (next, anchor) {
            let next_position = position + frame;
            let elapsed = rescale(
                next_position - anchor_position,
                MOVIE_TIMESCALE,
                sample_rate,
            );
            let drift = pts_delta(pts_add(anchor_pts, elapsed as i64), next);
            if drift > half_frame_90k && drift <= DISCONTINUITY_90K {
                // 空洞：把缺的时长并入本帧，后续帧回到源时间轴上。
                duration += rescale(drift as u64, sample_rate, MOVIE_TIMESCALE);
                anchor = Some((next, position + duration));
            } else if drift < -half_frame_90k || drift > DISCONTINUITY_90K {
                breaks.push(Break {
                    sample: index + 1,
                    source: next,
                    position: next_position,
                });
                anchor = Some((next, next_position));
            }
        }
        // 单帧时长 ≤ 1024 + 10 秒 × 96kHz，远小于 u32 上限。
        durations.push(duration as u32);
        position += duration;
    }

    TrackTiming {
        timescale: sample_rate,
        durations,
        ctts: Runs::default(),
        media_start: 0,
        source_start: origin,
        breaks,
    }
}

/// 两轨的起始推迟量（电影时基）：(视频, 音频)。起点相差超过不连续阈值时视为无关，不推迟。
fn start_delays(video: &TrackTiming, audio: &TrackTiming) -> (u64, u64) {
    let (Some(video_start), Some(audio_start)) = (video.source_start, audio.source_start) else {
        return (0, 0);
    };
    let delta = pts_delta(video_start, audio_start);
    if delta > 0 && delta <= DISCONTINUITY_90K {
        (0, delta as u64)
    } else if delta < 0 && -delta <= DISCONTINUITY_90K {
        ((-delta) as u64, 0)
    } else {
        (0, 0)
    }
}

/// 某条轨在一个断点处的「源时间 → 输出呈现时间」偏移（电影时基，不含源时间本身）：
/// 输出时间 = 源时间 + 返回值。
fn break_offset(track: &TrackTiming, delay: u64, at: &Break, extended: u64) -> i64 {
    let position = track.to_movie(at.position) + extended;
    (delay + position) as i64 - track.to_movie(track.media_start) as i64
}

/// 对齐两轨：返回 (视频推迟, 音频推迟)（电影时基），并在配对的断点处延长落后一轨的
/// 断点前样本。
pub(super) fn align(video: &mut TrackTiming, audio: &mut TrackTiming) -> (u64, u64) {
    let (video_delay, audio_delay) = start_delays(video, audio);
    if video.breaks.len() != audio.breaks.len() {
        return (video_delay, audio_delay);
    }
    // 已在前面断点上延长的量（电影时基），后续断点的推算位置随之后移。
    let mut video_extended: u64 = 0;
    let mut audio_extended: u64 = 0;
    for index in 0..video.breaks.len() {
        let (video_break, audio_break) = (video.breaks[index], audio.breaks[index]);
        let source_gap = pts_delta(audio_break.source, video_break.source);
        if source_gap.abs() > DISCONTINUITY_90K {
            continue;
        }
        let video_offset = break_offset(video, video_delay, &video_break, video_extended);
        let audio_offset = break_offset(audio, audio_delay, &audio_break, audio_extended);
        // 输出时间 = 源时间 + 偏移；同一源时刻两轨输出时间之差。
        let skew = video_offset - audio_offset - source_gap;
        if skew.abs() > DISCONTINUITY_90K {
            continue;
        }
        if skew > 0 {
            // 同一源时刻音频输出得早：推迟音频。
            let samples = rescale(skew as u64, audio.timescale, MOVIE_TIMESCALE);
            if extend_sample(audio, audio_break.sample, samples) {
                audio_extended += audio.to_movie(samples);
            }
        } else if skew < 0 && extend_sample(video, video_break.sample, (-skew) as u64) {
            // 视频输出得早：推迟视频。
            video_extended += (-skew) as u64;
        }
    }
    (video_delay, audio_delay)
}

/// 把 `amount`（媒体时基）并入断点 `sample` 之前的那个样本。
fn extend_sample(track: &mut TrackTiming, sample: usize, amount: u64) -> bool {
    let Some(duration) = sample
        .checked_sub(1)
        .and_then(|index| track.durations.get_mut(index))
    else {
        return false;
    };
    match u32::try_from(amount)
        .ok()
        .and_then(|amount| duration.checked_add(amount))
    {
        Some(extended) => {
            *duration = extended;
            true
        }
        None => false,
    }
}
