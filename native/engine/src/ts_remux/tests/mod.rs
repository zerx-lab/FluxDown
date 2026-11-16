//! 流式 TS → MP4 重封装测试：随机流在任意读窗口下结果一致且盒结构自洽；干净流逐项
//! 断言 AAC 参数、样本时长、不连续 / 空洞处理与编辑列表；真实编码素材走 ffmpeg。

mod mp4_reader;
mod synth;

use std::io::{Cursor, ErrorKind};
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use super::{Mp4Summary, READ_CHUNK, remux_with};
use synth::{AudioPes, Rng, SF_44100, SF_48000, TsBuilder, adts_frame, clean_stream, synth};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fluxdown-ts-remux-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn remux_bytes(ts: &[u8], dir: &Path, chunk: usize) -> Result<(Vec<u8>, Mp4Summary), ErrorKind> {
    let out = dir.join("out.mp4");
    let cancel = CancellationToken::new();
    remux_with(|| Ok(Cursor::new(ts)), &out, chunk, &cancel)
        .map(|summary| (std::fs::read(&out).expect("read output"), summary))
        .map_err(|e| e.kind())
}

/// 不同读窗口大小的结果必须完全相同（流式切分不改变解析结果），成功时盒结构自洽。
/// 返回是否成功转换。
fn assert_window_independent(ts: &[u8], dir: &Path, label: &str) -> bool {
    let reference = remux_bytes(ts, dir, READ_CHUNK);
    for chunk in [376, 377, 4099] {
        let actual = remux_bytes(ts, dir, chunk);
        assert!(
            actual == reference,
            "{label} chunk={chunk} differs from READ_CHUNK"
        );
    }
    match reference {
        Ok((file, _)) => {
            mp4_reader::parse(&file);
            true
        }
        Err(_) => false,
    }
}

#[test]
fn random_streams_remux_identically_across_read_windows() {
    let dir = scratch("random");
    let converted = (0..400)
        .filter(|&seed| {
            assert_window_independent(&synth(seed, false), &dir, &format!("seed {seed}"))
        })
        .count();
    // 生成器绝大多数用例应可转换，否则没有覆盖到写出路径。
    assert!(converted > 300, "only {converted} convertible cases");
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
}

#[test]
fn large_frames_spanning_many_windows_remux_identically() {
    let dir = scratch("large");
    for seed in 1000..1006 {
        assert!(assert_window_independent(
            &synth(seed, true),
            &dir,
            &format!("seed {seed}")
        ));
    }
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
}

#[test]
fn cancelled_remux_stops_with_interrupted() {
    let dir = scratch("cancel");
    let ts = synth(1000, true);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = remux_with(|| Ok(Cursor::new(&ts)), &dir.join("out.mp4"), 4096, &cancel);
    assert_eq!(result.map_err(|e| e.kind()), Err(ErrorKind::Interrupted));
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
}

const BASE: u64 = 126_000;

/// 25fps、无 B 帧的视频时间戳。
fn cfr_video(frames: usize, start: u64) -> Vec<(u64, Option<u64>)> {
    (0..frames as u64)
        .map(|i| (start + i * 3600, None))
        .collect()
}

/// 每个 PES 一个 AAC 帧、PTS 连续的音频。
fn contiguous_audio(frames: usize, start: u64, sample_rate: u64) -> Vec<AudioPes> {
    (0..frames as u64)
        .map(|i| AudioPes {
            pts: start + i * 1024 * 90_000 / sample_rate,
            frames: 1,
        })
        .collect()
}

fn remux_clean(ts: &[u8], name: &str) -> (mp4_reader::Movie, Mp4Summary) {
    let dir = scratch(name);
    let (file, summary) = remux_bytes(ts, &dir, READ_CHUNK).expect("remux clean stream");
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
    (mp4_reader::parse(&file), summary)
}

/// #1136：44.1kHz 的音轨曾被写成 48kHz（stsd / ASC / 1920 tick 帧长），播放加速 1.088 倍。
#[test]
fn audio_track_uses_sample_rate_and_channels_from_adts() {
    for (frequency_index, rate, channels, asc) in [
        (SF_44100, 44_100u32, 2u8, [0x12, 0x10]),
        (SF_44100, 44_100, 1, [0x12, 0x08]),
        (SF_48000, 48_000, 2, [0x11, 0x90]),
    ] {
        let audio_frames = 2 * rate as usize / 1024;
        let ts = clean_stream(
            &cfr_video(50, BASE),
            &contiguous_audio(audio_frames, BASE, u64::from(rate)),
            frequency_index,
            channels,
        );
        let (movie, summary) = remux_clean(&ts, &format!("asc-{rate}-{channels}"));
        let audio = movie.track(b"soun");
        let entry = audio.audio.as_ref().expect("mp4a entry");
        assert_eq!(audio.timescale, rate);
        assert_eq!(audio.stts, [(audio_frames as u32, 1024)]);
        assert_eq!(entry.sample_rate_field, rate << 16);
        assert_eq!(entry.channels, u16::from(channels));
        assert_eq!(entry.audio_specific_config, asc);
        let audio_summary = summary.audio.expect("audio summary");
        assert_eq!(audio_summary.sample_rate, rate);
        assert!(
            summary.video_ms.abs_diff(audio_summary.end_ms) < 30,
            "{summary:?}"
        );
    }
}

/// 一个 PES 装多个 ADTS 帧时只有第一帧带 PTS，其余按帧长推算，不能被当成空洞或重叠。
#[test]
fn multi_frame_audio_pes_stays_contiguous() {
    let audio: Vec<AudioPes> = (0..40u64)
        .map(|i| AudioPes {
            pts: BASE + i * 3 * 1024 * 90_000 / 44_100,
            frames: 3,
        })
        .collect();
    let ts = clean_stream(&cfr_video(70, BASE), &audio, SF_44100, 2);
    let (movie, _) = remux_clean(&ts, "multi-frame");
    assert_eq!(movie.track(b"soun").stts, [(120, 1024)]);
}

/// 音频 PTS 前跳（丢片）时把空缺并入前一帧时长，后续音频不提前。
#[test]
fn audio_gap_is_absorbed_into_previous_frame() {
    let mut audio = contiguous_audio(60, BASE, 44_100);
    for pes in &mut audio[30..] {
        pes.pts += 45_000; // 0.5 秒
    }
    let ts = clean_stream(&cfr_video(60, BASE), &audio, SF_44100, 2);
    let (movie, _) = remux_clean(&ts, "audio-gap");
    assert_eq!(
        movie.track(b"soun").stts,
        [(29, 1024), (1, 1024 + 22_050), (30, 1024)]
    );
}

/// PTS 重置（`EXT-X-DISCONTINUITY` 拼接）不能把视频帧长估小：#1136 实测 3559 tick。
#[test]
fn timestamp_reset_keeps_nominal_frame_duration() {
    let mut video = cfr_video(50, BASE);
    video.extend(cfr_video(50, BASE));
    let mut audio = contiguous_audio(86, BASE, 44_100);
    audio.extend(contiguous_audio(86, BASE, 44_100));
    let ts = clean_stream(&video, &audio, SF_44100, 2);
    let (movie, summary) = remux_clean(&ts, "reset");
    let video = movie.track(b"vide");
    assert_eq!(video.stts, [(100, 3600)]);
    assert_eq!(video.mdhd_duration, 100 * 3600);
    // 第一段音频 86 帧比视频 2 秒短 278 tick（136 采样），断点前补齐。
    assert_eq!(
        movie.track(b"soun").stts,
        [(85, 1024), (1, 1024 + 136), (86, 1024)]
    );
    assert_eq!(summary.video_ms, 4000);
}

/// 不连续前一段音频比视频长（编码器补齐 / 插播素材）时，断点后两轨重新对齐：
/// 落后的视频把差值并入断点前最后一帧，差异不跨段累积。
#[test]
fn tracks_realign_after_timestamp_reset() {
    let mut video = cfr_video(50, BASE);
    video.extend(cfr_video(50, BASE));
    let mut audio = contiguous_audio(90, BASE, 44_100);
    audio.extend(contiguous_audio(86, BASE, 44_100));
    let ts = clean_stream(&video, &audio, SF_44100, 2);
    let (movie, summary) = remux_clean(&ts, "realign");
    // 第一段音频 90 帧 = 188082 tick，视频 50 帧 = 180000 tick。
    assert_eq!(
        movie.track(b"vide").stts,
        [(49, 3600), (1, 3600 + 8082), (50, 3600)]
    );
    assert_eq!(movie.track(b"soun").stts, [(176, 1024)]);
    let audio_end = summary.audio.expect("audio").end_ms;
    assert!(summary.video_ms.abs_diff(audio_end) < 10, "{summary:?}");
}

/// 33 位 PTS 回绕处的相邻差仍按真实帧长计。
#[test]
fn pts_wraparound_is_continuous() {
    let start = (1u64 << 33) - 10 * 3600;
    let video: Vec<(u64, Option<u64>)> = (0..30u64)
        .map(|i| ((start + i * 3600) & ((1 << 33) - 1), None))
        .collect();
    let ts = clean_stream(&video, &[], SF_44100, 2);
    let (movie, _) = remux_clean(&ts, "wrap");
    assert_eq!(movie.track(b"vide").stts, [(30, 3600)]);
}

/// B 帧：ctts 按样本写非负偏移，编辑列表从首个呈现帧开始播放。
#[test]
fn b_frames_get_composition_offsets_and_edit_list() {
    // 解码序 I P B B P B B …：P 晚 3 帧呈现，B 晚 1 帧。
    let video: Vec<(u64, Option<u64>)> = (0..31u64)
        .map(|i| {
            let dts = BASE + i * 3600;
            let reorder = match i % 3 {
                0 if i == 0 => 1,
                1 => 3,
                _ => 0,
            };
            (dts + reorder * 3600, Some(dts))
        })
        .collect();
    let ts = clean_stream(&video, &[], SF_44100, 2);
    let (movie, _) = remux_clean(&ts, "bframes");
    let video = movie.track(b"vide");
    let ctts = video.ctts.as_ref().expect("ctts");
    assert_eq!(&ctts[..3], [(1, 3600), (1, 10_800), (2, 0)]);
    assert_eq!(video.edits, [(31 * 3600 - 3600, 3600)]);
    assert_eq!(video.tkhd_duration, 31 * 3600 - 3600);
}

/// 音频晚于视频开始时用空编辑推迟音频，而不是裁掉音频开头。
#[test]
fn late_audio_is_delayed_with_empty_edit() {
    let ts = clean_stream(
        &cfr_video(50, BASE),
        &contiguous_audio(80, BASE + 9000, 44_100),
        SF_44100,
        2,
    );
    let (movie, summary) = remux_clean(&ts, "late-audio");
    let audio = movie.track(b"soun");
    let presented = (80u64 * 1024 * 90_000).div_ceil(44_100) as u32;
    assert_eq!(audio.edits.len(), 2);
    assert_eq!(audio.edits[0], (9000, -1));
    assert_eq!(audio.edits[1].1, 0);
    assert!(audio.edits[1].0.abs_diff(presented) <= 1);
    assert!(movie.track(b"vide").edits.is_empty());
    assert_eq!(movie.mvhd_duration, audio.tkhd_duration.max(50 * 3600));
    assert_eq!(
        summary.audio.expect("audio").end_ms,
        (9000 + u64::from(audio.edits[1].0)) / 90
    );
}

/// 单条音轨只能有一组解码参数：中途切换采样率时拒绝转换（调用方保留 `.ts`）。
#[test]
fn sample_rate_change_mid_stream_is_rejected() {
    let mut rng = Rng(7);
    let mut ts = TsBuilder::new(false);
    ts.psi(&mut rng, true);
    for (i, frequency_index) in [SF_44100, SF_44100, SF_48000].into_iter().enumerate() {
        let pts = BASE + i as u64 * 3600;
        ts.video_pes(&mut rng, Some(pts), None, &synth::clean_video_frame(i == 0));
        ts.audio_pes(
            &mut rng,
            Some(pts),
            &adts_frame(frequency_index, 2, true, &[0x21; 40]),
        );
    }
    let dir = scratch("rate-change");
    assert_eq!(
        remux_bytes(&ts.out, &dir, READ_CHUNK).map(|_| ()),
        Err(ErrorKind::InvalidData)
    );
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
}

/// 真实编码器产出的 H.264（B 帧）+ 44.1kHz AAC TS，单段与 PTS 重置拼接两种
/// （需 `FLUXDOWN_TEST_FFMPEG` 指向带 libx264 的 ffmpeg）。成片须能被 ffmpeg 无错解码。
#[test]
fn ffmpeg_encoded_44k_stream_stays_in_sync() {
    let Some(ffmpeg) = std::env::var_os("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG");
        return;
    };
    let dir = scratch("ffmpeg");
    let ts_path = dir.join("clip.ts");
    let status = std::process::Command::new(&ffmpeg)
        .args(["-y", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=6",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=6",
        ])
        .args([
            "-c:v", "libx264", "-bf", "2", "-g", "50", "-c:a", "aac", "-f", "mpegts",
        ])
        .arg(&ts_path)
        .status();
    if !status.is_ok_and(|s| s.success()) {
        eprintln!("[skip] ffmpeg 无法编码 H.264 测试素材");
        std::fs::remove_dir_all(&dir).expect("remove scratch dir");
        return;
    }
    let single = std::fs::read(&ts_path).expect("read fixture");
    let joined = single.repeat(2);
    for (label, ts, expected_ms) in [("single", &single, 6000u64), ("joined", &joined, 12_000)] {
        let (file, summary) = remux_bytes(ts, &dir, READ_CHUNK).expect("remux fixture");
        let movie = mp4_reader::parse(&file);
        assert_eq!(movie.track(b"soun").timescale, 44_100, "{label}");
        let audio = summary.audio.expect("audio");
        assert!(
            summary.video_ms.abs_diff(expected_ms) <= 100,
            "{label} {summary:?}"
        );
        assert!(
            summary.video_ms.abs_diff(audio.end_ms) <= 100,
            "{label} {summary:?}"
        );

        let out = dir.join(format!("{label}.mp4"));
        std::fs::write(&out, &file).expect("write mp4");
        let decoded = std::process::Command::new(&ffmpeg)
            .args(["-v", "error", "-xerror", "-i"])
            .arg(&out)
            .args(["-f", "null", "-"])
            .output()
            .expect("run ffmpeg decode");
        assert!(
            decoded.status.success() && decoded.stderr.is_empty(),
            "{label}: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
    }
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
}
