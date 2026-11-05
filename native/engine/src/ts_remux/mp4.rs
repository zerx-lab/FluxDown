//! MP4 文件头（ftyp + moov + mdat 头）构建。
//!
//! 字节布局与 `ts2mp4::convert_ts_to_mp4`（0.3.2）一致：mdat 内先放全部视频样本、
//! 再放全部音频样本，各占一个 chunk。这里只需要样本大小（解复用第一遍统计），
//! 样本字节由第二遍直接写到各自偏移，不进内存。时长相乘溢出时报错，不写出错误时长。
//!
//! 盒构建逻辑移植自 ts2mp4，许可：MIT License, Copyright (c) 2026 Seongchul Ahn
//! （<https://github.com/aciddust/ts2mp4>）。

use std::io::{self, ErrorKind};

use super::demux::StreamMeta;

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

fn overflow(what: &'static str) -> io::Error {
    io::Error::new(ErrorKind::Unsupported, what)
}

fn mul(a: u32, b: u32) -> io::Result<u32> {
    a.checked_mul(b)
        .ok_or_else(|| overflow("MP4 duration exceeds 32-bit range"))
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
) -> io::Result<Vec<u8>> {
    let video_total: u64 = video_sizes.iter().map(|&size| u64::from(size)).sum();
    let audio_total: u64 = audio_sizes.iter().map(|&size| u64::from(size)).sum();
    let video_data_end =
        usize::try_from(video_total).map_err(|_| overflow("MP4 media exceeds address space"))?;

    let probe = build_moov(meta, video_sizes, audio_sizes, 0, video_data_end)?;
    let moov = build_moov(meta, video_sizes, audio_sizes, probe.len(), video_data_end)?;

    let mdat_size = MDAT_HEADER_SIZE as u64 + video_total + audio_total;
    let mdat_size =
        u32::try_from(mdat_size).map_err(|_| overflow("MP4 mdat exceeds 32-bit range"))?;

    let mut header = Vec::with_capacity(FTYP.len() + moov.len() + MDAT_HEADER_SIZE);
    header.extend_from_slice(&FTYP);
    header.extend_from_slice(&moov);
    header.extend_from_slice(&mdat_size.to_be_bytes());
    header.extend_from_slice(b"mdat");
    Ok(header)
}

/// 视频样本时长（90kHz）：按 PTS 范围的 CFR 估计，PTS 不足时回落 3000（30fps）。
fn video_sample_delta(frame_timestamps: &[(Option<u64>, Option<u64>)]) -> u32 {
    let mut min: Option<u64> = None;
    let mut max: Option<u64> = None;
    let mut count: u64 = 0;
    for &(pts, _) in frame_timestamps {
        if let Some(pts) = pts {
            count += 1;
            min = Some(min.map_or(pts, |m| m.min(pts)));
            max = Some(max.map_or(pts, |m| m.max(pts)));
        }
    }
    if let (Some(min), Some(max)) = (min, max)
        && count >= 2
        && max > min
    {
        let delta = (max - min) / (count - 1);
        if delta > 0 {
            return delta as u32;
        }
    }
    3000
}

fn build_moov(
    meta: &StreamMeta,
    video_sizes: &[u32],
    audio_sizes: &[u32],
    moov_size: usize,
    video_data_end: usize,
) -> io::Result<Vec<u8>> {
    let video_min_pts = meta
        .frame_timestamps
        .iter()
        .filter_map(|&(pts, _)| pts)
        .min();
    let audio_min_pts = meta.audio_timestamps.iter().filter_map(|&pts| pts).min();
    let global_min_pts = match (video_min_pts, audio_min_pts) {
        (Some(v), Some(a)) => v.min(a),
        (Some(v), None) => v,
        (None, Some(a)) => a,
        (None, None) => 0,
    };
    let base_offset = FTYP.len() + moov_size + MDAT_HEADER_SIZE;
    let has_audio = !audio_sizes.is_empty();

    let video_delta = video_sample_delta(&meta.frame_timestamps);
    let duration = mul(count_u32(video_sizes.len())?, video_delta)?;

    let mut moov = build_mvhd(duration, has_audio);
    let offsets = composition_offsets(&meta.frame_timestamps, global_min_pts);
    moov.extend_from_slice(&build_video_trak(meta, video_sizes, &offsets, base_offset)?);
    if has_audio {
        moov.extend_from_slice(&build_audio_trak(
            meta,
            audio_sizes,
            global_min_pts,
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
        0x00, 0x01, 0x5F, 0x90, // timescale = 90000
    ];
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

fn build_mdhd(duration: u32) -> Vec<u8> {
    let mut out = vec![
        0x00, 0x00, 0x00, 0x20, b'm', b'd', b'h', b'd', //
        0x00, 0x00, 0x00, 0x00, // version + flags
        0x00, 0x00, 0x00, 0x00, // creation time
        0x00, 0x00, 0x00, 0x00, // modification time
        0x00, 0x01, 0x5F, 0x90, // timescale = 90000
    ];
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

/// `stts`（单条目）+ `stsc`（全部样本一个 chunk）+ `stsz` + `stco`（单偏移）。
fn sample_tables(
    out: &mut Vec<u8>,
    sizes: &[u32],
    delta: u32,
    chunk_offset: usize,
) -> io::Result<()> {
    let count = count_u32(sizes.len())?;

    let mut stts = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    stts.extend_from_slice(&count.to_be_bytes());
    stts.extend_from_slice(&delta.to_be_bytes());
    out.extend_from_slice(&boxed(b"stts", &stts));

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

    let mut stco = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    stco.extend_from_slice(&(chunk_offset as u32).to_be_bytes());
    out.extend_from_slice(&boxed(b"stco", &stco));
    Ok(())
}

fn build_video_trak(
    meta: &StreamMeta,
    sizes: &[u32],
    composition_offsets: &[i32],
    chunk_offset: usize,
) -> io::Result<Vec<u8>> {
    let delta = video_sample_delta(&meta.frame_timestamps);
    let count = count_u32(sizes.len())?;
    let duration = mul(count, delta)?;

    let mut stbl = build_video_stsd(meta);
    sample_tables(&mut stbl, sizes, delta, chunk_offset)?;
    if !composition_offsets.is_empty() && composition_offsets.iter().any(|&o| o != 0) {
        let mut ctts = Vec::with_capacity(8 + composition_offsets.len() * 8);
        ctts.extend_from_slice(&[0x00; 4]); // version + flags
        // 条目数字段写样本数，条目按 PES 时间戳逐条写（两者可能不等，播放器以条目为准）。
        ctts.extend_from_slice(&count.to_be_bytes());
        for &offset in composition_offsets {
            ctts.extend_from_slice(&1u32.to_be_bytes());
            ctts.extend_from_slice(&(offset as u32).to_be_bytes());
        }
        stbl.extend_from_slice(&boxed(b"ctts", &ctts));
    }

    let mut minf = vec![
        0x00, 0x00, 0x00, 0x14, b'v', b'm', b'h', b'd', 0x00, 0x00, 0x00, 0x01, // vmhd
        0x00, 0x00, // graphics mode
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // opcolor
    ];
    minf.extend_from_slice(&DINF);
    minf.extend_from_slice(&boxed(b"stbl", &stbl));

    let mut mdia = build_mdhd(duration);
    mdia.extend_from_slice(&build_hdlr(b"vide"));
    mdia.extend_from_slice(&boxed(b"minf", &minf));

    let mut trak = build_tkhd(1, duration, [0x00, 0x00], meta.width, meta.height);
    trak.extend_from_slice(&boxed(b"mdia", &mdia));
    Ok(boxed(b"trak", &trak))
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

fn build_audio_trak(
    meta: &StreamMeta,
    sizes: &[u32],
    global_min_pts: u64,
    chunk_offset: usize,
) -> io::Result<Vec<u8>> {
    let count = count_u32(sizes.len())?;
    // 音轨固定描述为 48kHz 立体声 AAC-LC：1024 采样 @ 48kHz = 90kHz 时基下 1920。
    let duration = mul(count, 1920)?;

    let mut stbl = build_audio_stsd();
    sample_tables(&mut stbl, sizes, 1920, chunk_offset)?;

    let mut minf = vec![
        0x00, 0x00, 0x00, 0x10, b's', b'm', b'h', b'd', 0x00, 0x00, 0x00, 0x00, // smhd
        0x00, 0x00, // balance
        0x00, 0x00, // reserved
    ];
    minf.extend_from_slice(&DINF);
    minf.extend_from_slice(&boxed(b"stbl", &stbl));

    let mut mdia = build_mdhd(duration);
    mdia.extend_from_slice(&build_hdlr(b"soun"));
    mdia.extend_from_slice(&boxed(b"minf", &minf));

    let mut trak = build_tkhd(2, duration, [0x01, 0x00], 0, 0);
    trak.extend_from_slice(&boxed(b"mdia", &mdia));

    // 音频晚于全局最早 PTS 时加编辑列表。
    if let Some(Some(first_audio_pts)) = meta.audio_timestamps.first()
        && *first_audio_pts > global_min_pts
    {
        let delay_90khz = *first_audio_pts - global_min_pts;
        let delay_48khz = ((delay_90khz as i64 * 48000) / 90000) as i32;
        let media_duration_48khz = mul(count, 1024)?;
        let segment_duration_90khz = u64::from(media_duration_48khz) * 90000 / 48000;

        // elst 盒头长度字段固定写 36（实际 28 字节），外层 edts 长度按实际字节计。
        let mut elst = vec![0x00, 0x00, 0x00, 0x24, b'e', b'l', b's', b't'];
        elst.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
        elst.extend_from_slice(&(segment_duration_90khz as u32).to_be_bytes());
        elst.extend_from_slice(&(delay_48khz as u32).to_be_bytes());
        elst.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // media rate = 1.0
        trak.extend_from_slice(&boxed(b"edts", &elst));
    }
    Ok(boxed(b"trak", &trak))
}

fn build_audio_stsd() -> Vec<u8> {
    let mut mp4a = vec![
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
        0x00, 0x01, // data reference index
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
        0x00, 0x02, // channel count = 2
        0x00, 0x10, // sample size = 16
        0x00, 0x00, 0x00, 0x00, // pre-defined + reserved
        0xBB, 0x80, 0x00, 0x00, // sample rate = 48000 << 16
    ];
    let esds = [
        0x00, 0x00, 0x00, 0x00, // version + flags
        0x03, 0x19, 0x00, 0x01, 0x00, // ES_Descriptor
        0x04, 0x11, 0x40, 0x15, // DecoderConfigDescriptor: AAC, audio stream
        0x00, 0x03, 0x00, // buffer size
        0x00, 0x01, 0xF4, 0x00, // max bitrate
        0x00, 0x01, 0xF4, 0x00, // avg bitrate
        0x05, 0x02, 0x11, 0x90, // AudioSpecificConfig: AAC-LC, 48kHz, stereo
        0x06, 0x01, 0x02, // SLConfigDescriptor
    ];
    mp4a.extend_from_slice(&boxed(b"esds", &esds));

    let mut stsd = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
    stsd.extend_from_slice(&boxed(b"mp4a", &mp4a));
    boxed(b"stsd", &stsd)
}

/// 合成时间偏移：按 PES 逐条计算 PTS − DTS（i32 截断），再整体减去首条。
fn composition_offsets(timestamps: &[(Option<u64>, Option<u64>)], global_min_pts: u64) -> Vec<i32> {
    let min = global_min_pts as i64;
    let mut offsets: Vec<i32> = timestamps
        .iter()
        .map(|&(pts, dts)| match (pts, dts) {
            (Some(pts), Some(dts)) => {
                let pts = (pts as i64 - min) as i32;
                let dts = (dts as i64 - min) as i32;
                pts.wrapping_sub(dts)
            }
            _ => 0,
        })
        .collect();
    if let Some(&first) = offsets.first() {
        for offset in &mut offsets {
            *offset = offset.wrapping_sub(first);
        }
    }
    offsets
}
