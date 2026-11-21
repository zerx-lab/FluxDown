//! MP4 文件头（ftyp + moov + mdat 头）构建。
//!
//! mdat 内先放全部视频样本、再放全部音频样本，各占一个 chunk。这里只需要样本大小
//! （解复用第一遍统计）与时间线（[`super::timing`]），样本字节由第二遍直接写到各自偏移，
//! 不进内存。音轨的采样率、声道数与 AudioSpecificConfig 全部取自 ADTS 头，媒体时基
//! 即采样率。时长 / 偏移超出 32 位盒字段时报错，不写出错误时长。
//!
//! 盒布局最初移植自 ts2mp4，许可：MIT License, Copyright (c) 2026 Seongchul Ahn
//! （<https://github.com/aciddust/ts2mp4>）。

use std::io::{self, ErrorKind};

use super::demux::{AacConfig, StreamMeta};
use super::timing::{self, MOVIE_TIMESCALE, Runs, TrackTiming};

const FTYP: [u8; 28] = [
    0x00, 0x00, 0x00, 0x1C, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0x00, 0x00, 0x02, 0x00,
    b'i', b's', b'o', b'm', b'i', b's', b'o', b'2', b'm', b'p', b'4', b'1',
];
const MDAT_HEADER_SIZE: usize = 8;

/// 单位矩阵（mvhd / tkhd 共用）。
const MATRIX: [u8; 36] = [
    0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00,
];

const DINF: [u8; 36] = [
    0x00, 0x00, 0x00, 0x24, b'd', b'i', b'n', b'f', 0x00, 0x00, 0x00, 0x1C, b'd', b'r', b'e', b'f',
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0C, b'u', b'r', b'l', b' ',
    0x00, 0x00, 0x00, 0x01,
];

const VMHD: [u8; 20] = [
    0x00, 0x00, 0x00, 0x14, b'v', b'm', b'h', b'd', 0x00, 0x00, 0x00, 0x01, // version + flags
    0x00, 0x00, // graphics mode
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // opcolor
];

const SMHD: [u8; 16] = [
    0x00, 0x00, 0x00, 0x10, b's', b'm', b'h', b'd', 0x00, 0x00, 0x00, 0x00, // version + flags
    0x00, 0x00, // balance
    0x00, 0x00, // reserved
];

/// 写出的两条轨的时长与音频参数，供宿主记日志与做音画时长自检。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mp4Summary {
    /// 视频轨呈现结束时刻（含起始推迟），毫秒。
    pub video_ms: u64,
    pub audio: Option<AudioSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioSummary {
    /// 音频轨呈现结束时刻（含起始推迟），毫秒。
    pub end_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

struct AudioTrack {
    timing: TrackTiming,
    config: AacConfig,
    /// 起始推迟（电影时基）。
    delay: u64,
}

/// 两条轨的时间线与对齐结果；构建 moov 前算一次。
struct Plan {
    video: TrackTiming,
    video_delay: u64,
    audio: Option<AudioTrack>,
}

impl Plan {
    fn new(meta: &StreamMeta, video_samples: usize, audio_samples: usize) -> io::Result<Self> {
        if meta.video_timestamps.len() != video_samples
            || meta.audio_timestamps.len() != audio_samples
        {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "sample timestamps do not match samples",
            ));
        }
        let mut video = timing::video(&meta.video_timestamps);
        if audio_samples == 0 {
            return Ok(Self {
                video,
                video_delay: 0,
                audio: None,
            });
        }
        let config = meta.audio_config.ok_or_else(|| {
            io::Error::new(ErrorKind::InvalidData, "AAC samples without configuration")
        })?;
        let mut audio = timing::audio(&meta.audio_timestamps, config.sample_rate());
        let (video_delay, audio_delay) = timing::align(&mut video, &mut audio);
        Ok(Self {
            video,
            video_delay,
            audio: Some(AudioTrack {
                timing: audio,
                config,
                delay: audio_delay,
            }),
        })
    }

    fn video_end(&self) -> u64 {
        self.video_delay + self.video.presented_movie()
    }

    fn audio_end(&self) -> Option<u64> {
        self.audio
            .as_ref()
            .map(|audio| audio.delay + audio.timing.presented_movie())
    }

    fn summary(&self) -> Mp4Summary {
        let to_ms = |ticks: u64| ticks / u64::from(MOVIE_TIMESCALE / 1000);
        Mp4Summary {
            video_ms: to_ms(self.video_end()),
            audio: self.audio.as_ref().map(|audio| AudioSummary {
                end_ms: to_ms(audio.delay + audio.timing.presented_movie()),
                sample_rate: audio.config.sample_rate(),
                channels: audio.config.channels(),
            }),
        }
    }
}

fn overflow(what: &'static str) -> io::Error {
    io::Error::new(ErrorKind::Unsupported, what)
}

fn duration_u32(value: u64) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| overflow("MP4 duration exceeds 32-bit range"))
}

fn count_u32(len: usize) -> io::Result<u32> {
    u32::try_from(len).map_err(|_| overflow("MP4 sample count exceeds 32-bit range"))
}

/// 给负载套上盒头（4 字节大端长度 + 类型）。
fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// 构建 `ftyp + moov + mdat 头`。调用方保证文件总长不超过 `u32::MAX`
/// （偏移与 mdat 长度按 32 位写入）。
pub(super) fn build_header(
    meta: &StreamMeta,
    video_sizes: &[u32],
    audio_sizes: &[u32],
) -> io::Result<(Vec<u8>, Mp4Summary)> {
    let plan = Plan::new(meta, video_sizes.len(), audio_sizes.len())?;
    let video_total: u64 = video_sizes.iter().map(|&size| u64::from(size)).sum();
    let audio_total: u64 = audio_sizes.iter().map(|&size| u64::from(size)).sum();
    let video_data_end =
        usize::try_from(video_total).map_err(|_| overflow("MP4 media exceeds address space"))?;

    let probe = build_moov(&plan, meta, video_sizes, audio_sizes, 0, video_data_end)?;
    let moov = build_moov(
        &plan,
        meta,
        video_sizes,
        audio_sizes,
        probe.len(),
        video_data_end,
    )?;

    let mdat_size = MDAT_HEADER_SIZE as u64 + video_total + audio_total;
    let mdat_size =
        u32::try_from(mdat_size).map_err(|_| overflow("MP4 mdat exceeds 32-bit range"))?;

    let mut header = Vec::with_capacity(FTYP.len() + moov.len() + MDAT_HEADER_SIZE);
    header.extend_from_slice(&FTYP);
    header.extend_from_slice(&moov);
    header.extend_from_slice(&mdat_size.to_be_bytes());
    header.extend_from_slice(b"mdat");
    Ok((header, plan.summary()))
}

fn build_moov(
    plan: &Plan,
    meta: &StreamMeta,
    video_sizes: &[u32],
    audio_sizes: &[u32],
    moov_size: usize,
    video_data_end: usize,
) -> io::Result<Vec<u8>> {
    let base_offset = FTYP.len() + moov_size + MDAT_HEADER_SIZE;
    let duration = plan.audio_end().map_or(plan.video_end(), |audio_end| {
        audio_end.max(plan.video_end())
    });

    let mut moov = build_mvhd(duration_u32(duration)?, plan.audio.is_some());
    let video = Track {
        id: 1,
        timing: &plan.video,
        delay: plan.video_delay,
        handler: b"vide",
        media_header: &VMHD,
        volume: [0x00, 0x00],
        width: meta.width,
        height: meta.height,
    };
    moov.extend_from_slice(&video.build(build_video_stsd(meta), video_sizes, base_offset)?);
    if let Some(audio) = &plan.audio {
        let track = Track {
            id: 2,
            timing: &audio.timing,
            delay: audio.delay,
            handler: b"soun",
            media_header: &SMHD,
            volume: [0x01, 0x00],
            width: 0,
            height: 0,
        };
        moov.extend_from_slice(&track.build(
            build_audio_stsd(audio.config),
            audio_sizes,
            base_offset + video_data_end,
        )?);
    }
    Ok(boxed(b"moov", &moov))
}

fn build_mvhd(duration: u32, has_audio: bool) -> Vec<u8> {
    let next_track_id: u32 = if has_audio { 3 } else { 2 };
    let mut out = vec![
        0x00, 0x00, 0x00, 0x6C, b'm', b'v', b'h', b'd', //
        0x00, 0x00, 0x00, 0x00, // version + flags
        0x00, 0x00, 0x00, 0x00, // creation time
        0x00, 0x00, 0x00, 0x00, // modification time
    ];
    out.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
    out.extend_from_slice(&duration.to_be_bytes());
    out.extend_from_slice(&[
        0x00, 0x01, 0x00, 0x00, // rate
        0x01, 0x00, // volume
        0x00, 0x00, // reserved
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
    ]);
    out.extend_from_slice(&MATRIX);
    out.extend_from_slice(&[0x00; 24]); // pre-defined
    out.extend_from_slice(&next_track_id.to_be_bytes());
    out
}

fn build_tkhd(track_id: u32, duration: u32, volume: [u8; 2], width: u16, height: u16) -> Vec<u8> {
    let mut out = vec![
        0x00, 0x00, 0x00, 0x5C, b't', b'k', b'h', b'd', //
        0x00, 0x00, 0x00, 0x07, // version + flags (enabled)
        0x00, 0x00, 0x00, 0x00, // creation time
        0x00, 0x00, 0x00, 0x00, // modification time
    ];
    out.extend_from_slice(&track_id.to_be_bytes());
    out.extend_from_slice(&[0x00; 4]); // reserved
    out.extend_from_slice(&duration.to_be_bytes());
    out.extend_from_slice(&[0x00; 8]); // reserved
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // layer + alternate group
    out.extend_from_slice(&volume);
    out.extend_from_slice(&[0x00, 0x00]); // reserved
    out.extend_from_slice(&MATRIX);
    out.extend_from_slice(&(u32::from(width) << 16).to_be_bytes());
    out.extend_from_slice(&(u32::from(height) << 16).to_be_bytes());
    out
}

fn build_mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut out = vec![
        0x00, 0x00, 0x00, 0x20, b'm', b'd', b'h', b'd', //
        0x00, 0x00, 0x00, 0x00, // version + flags
        0x00, 0x00, 0x00, 0x00, // creation time
        0x00, 0x00, 0x00, 0x00, // modification time
    ];
    out.extend_from_slice(&timescale.to_be_bytes());
    out.extend_from_slice(&duration.to_be_bytes());
    out.extend_from_slice(&[0x55, 0xC4, 0x00, 0x00]); // language + pre-defined
    out
}

fn build_hdlr(handler: &[u8; 4]) -> Vec<u8> {
    let mut out = vec![
        0x00, 0x00, 0x00, 0x21, b'h', b'd', b'l', b'r', //
        0x00, 0x00, 0x00, 0x00, // version + flags
        0x00, 0x00, 0x00, 0x00, // pre-defined
    ];
    out.extend_from_slice(handler);
    out.extend_from_slice(&[0x00; 13]); // reserved + empty name
    out
}

/// 一条轨的盒参数。
struct Track<'a> {
    id: u32,
    timing: &'a TrackTiming,
    /// 起始推迟（电影时基），写成空编辑。
    delay: u64,
    handler: &'a [u8; 4],
    media_header: &'a [u8],
    volume: [u8; 2],
    width: u16,
    height: u16,
}

impl Track<'_> {
    fn build(&self, stsd: Vec<u8>, sizes: &[u32], chunk_offset: usize) -> io::Result<Vec<u8>> {
        let mut stbl = stsd;
        sample_tables(&mut stbl, self.timing, sizes, chunk_offset)?;

        let mut minf = self.media_header.to_vec();
        minf.extend_from_slice(&DINF);
        minf.extend_from_slice(&boxed(b"stbl", &stbl));

        let mut mdia = build_mdhd(
            self.timing.timescale,
            duration_u32(self.timing.media_duration())?,
        );
        mdia.extend_from_slice(&build_hdlr(self.handler));
        mdia.extend_from_slice(&boxed(b"minf", &minf));

        let duration = duration_u32(self.delay + self.timing.presented_movie())?;
        let mut trak = build_tkhd(self.id, duration, self.volume, self.width, self.height);
        if let Some(edts) = build_edts(self.delay, self.timing)? {
            trak.extend_from_slice(&edts);
        }
        trak.extend_from_slice(&boxed(b"mdia", &mdia));
        Ok(boxed(b"trak", &trak))
    }
}

/// 编辑列表：起始推迟写空编辑（media_time = −1），首个呈现样本不在媒体时间 0 时
/// 从它开始播放。两者都没有时不写。
fn build_edts(delay: u64, timing: &TrackTiming) -> io::Result<Option<Vec<u8>>> {
    if delay == 0 && timing.media_start == 0 {
        return Ok(None);
    }
    let mut entries: Vec<(u32, i32)> = Vec::with_capacity(2);
    if delay > 0 {
        entries.push((duration_u32(delay)?, -1));
    }
    let media_time = i32::try_from(timing.media_start)
        .map_err(|_| overflow("MP4 edit media time exceeds 32-bit range"))?;
    entries.push((duration_u32(timing.presented_movie())?, media_time));

    let mut elst = vec![0x00; 4]; // version + flags
    elst.extend_from_slice(&count_u32(entries.len())?.to_be_bytes());
    for (segment_duration, media_time) in entries {
        elst.extend_from_slice(&segment_duration.to_be_bytes());
        elst.extend_from_slice(&media_time.to_be_bytes());
        elst.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // media rate = 1.0
    }
    Ok(Some(boxed(b"edts", &boxed(b"elst", &elst))))
}

fn runs_box(kind: &[u8; 4], runs: &[(u32, u32)]) -> io::Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(8 + runs.len() * 8);
    payload.extend_from_slice(&[0x00; 4]); // version + flags
    payload.extend_from_slice(&count_u32(runs.len())?.to_be_bytes());
    for &(count, value) in runs {
        payload.extend_from_slice(&count.to_be_bytes());
        payload.extend_from_slice(&value.to_be_bytes());
    }
    Ok(boxed(kind, &payload))
}

/// `stts` + `ctts`（有合成偏移时）+ `stsc`（全部样本一个 chunk）+ `stsz` + `stco`（单偏移）。
fn sample_tables(
    out: &mut Vec<u8>,
    timing: &TrackTiming,
    sizes: &[u32],
    chunk_offset: usize,
) -> io::Result<()> {
    let count = count_u32(sizes.len())?;

    out.extend_from_slice(&runs_box(b"stts", &Runs::from_values(&timing.durations).0)?);
    if !timing.ctts.0.is_empty() {
        out.extend_from_slice(&runs_box(b"ctts", &timing.ctts.0)?);
    }

    let mut stsc = vec![
        0x00, 0x00, 0x00, 0x00, // version + flags
        0x00, 0x00, 0x00, 0x01, // entry count
        0x00, 0x00, 0x00, 0x01, // first chunk
    ];
    stsc.extend_from_slice(&count.to_be_bytes());
    stsc.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // sample description index
    out.extend_from_slice(&boxed(b"stsc", &stsc));

    let mut stsz = Vec::with_capacity(12 + sizes.len() * 4);
    stsz.extend_from_slice(&[0x00; 8]); // version + flags, sample size (0 = variable)
    stsz.extend_from_slice(&count.to_be_bytes());
    for &size in sizes {
        stsz.extend_from_slice(&size.to_be_bytes());
    }
    out.extend_from_slice(&boxed(b"stsz", &stsz));

    let chunk_offset = u32::try_from(chunk_offset)
        .map_err(|_| overflow("MP4 chunk offset exceeds 32-bit range"))?;
    let mut stco = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    stco.extend_from_slice(&chunk_offset.to_be_bytes());
    out.extend_from_slice(&boxed(b"stco", &stco));
    Ok(())
}

fn build_video_stsd(meta: &StreamMeta) -> Vec<u8> {
    let mut avc1 = vec![
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
        0x00, 0x01, // data reference index
        0x00, 0x00, 0x00, 0x00, // pre-defined + reserved
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // pre-defined
    ];
    avc1.extend_from_slice(&meta.width.to_be_bytes());
    avc1.extend_from_slice(&meta.height.to_be_bytes());
    avc1.extend_from_slice(&[
        0x00, 0x48, 0x00, 0x00, // horizontal resolution
        0x00, 0x48, 0x00, 0x00, // vertical resolution
        0x00, 0x00, 0x00, 0x00, // reserved
        0x00, 0x01, // frame count
    ]);
    avc1.extend_from_slice(&[0x00; 32]); // compressor name
    avc1.extend_from_slice(&[0x00, 0x18, 0xFF, 0xFF]); // depth + pre-defined

    if let (Some(sps), Some(pps)) = (&meta.sps, &meta.pps) {
        let mut avcc = vec![0x01];
        match sps.get(1..4) {
            Some(profile) if sps.len() >= 4 => avcc.extend_from_slice(profile),
            _ => avcc.extend_from_slice(&[0x64, 0x00, 0x1F]),
        }
        avcc.push(0xFF); // NAL 长度 4 字节
        avcc.push(0xE1); // 1 个 SPS
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(sps);
        avcc.push(0x01); // 1 个 PPS
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(pps);
        avc1.extend_from_slice(&boxed(b"avcC", &avcc));
    }

    let mut stsd = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    stsd.extend_from_slice(&boxed(b"avc1", &avc1));
    boxed(b"stsd", &stsd)
}

fn build_audio_stsd(config: AacConfig) -> Vec<u8> {
    let sample_rate = config.sample_rate();
    // 16.16 定点字段放不下 88.2k / 96k，按 ffmpeg 的做法写 0，以 mdhd 时基为准。
    let rate_field = if sample_rate <= u32::from(u16::MAX) {
        sample_rate << 16
    } else {
        0
    };
    let mut mp4a = vec![
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
        0x00, 0x01, // data reference index
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
    ];
    mp4a.extend_from_slice(&config.channels().to_be_bytes());
    mp4a.extend_from_slice(&[
        0x00, 0x10, // sample size = 16
        0x00, 0x00, 0x00, 0x00, // pre-defined + reserved
    ]);
    mp4a.extend_from_slice(&rate_field.to_be_bytes());

    let [asc_high, asc_low] = config.audio_specific_config();
    let esds = [
        0x00, 0x00, 0x00, 0x00, // version + flags
        0x03, 0x19, 0x00, 0x01, 0x00, // ES_Descriptor
        0x04, 0x11, 0x40, 0x15, // DecoderConfigDescriptor: AAC, audio stream
        0x00, 0x03, 0x00, // buffer size
        0x00, 0x01, 0xF4, 0x00, // max bitrate
        0x00, 0x01, 0xF4, 0x00, // avg bitrate
        0x05, 0x02, asc_high, asc_low, // DecoderSpecificInfo: AudioSpecificConfig
        0x06, 0x01, 0x02, // SLConfigDescriptor
    ];
    mp4a.extend_from_slice(&boxed(b"esds", &esds));

    let mut stsd = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    stsd.extend_from_slice(&boxed(b"mp4a", &mp4a));
    boxed(b"stsd", &stsd)
}
