//! 流式重封装与 `ts2mp4::convert_ts_to_mp4` 的差分测试：随机合成的 TS 流（失步垃圾、
//! 自适应字段、跨 PES 的帧、缺 AUD、ADTS 残片、B 帧时间戳……）在多种读窗口大小下
//! 必须产出逐字节相同的 MP4，或与之同样失败。

use std::io::{Cursor, ErrorKind};
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use super::{READ_CHUNK, remux_with};

const PMT_PID: u16 = 0x1000;
const VIDEO_PID: u16 = 0x100;
const AUDIO_PID: u16 = 0x101;

/// x264 实际产出的 SPS / PPS（含裁剪、High / High 4:2:2 / Main / Baseline）。
const PARAMETER_SETS: [(&str, &str); 4] = [
    (
        "67640028acd940780227e5c044000003000400000300c83c60c658",
        "68ebe3cb22c0",
    ),
    (
        "6742c00ad90426c044000003000400000300c83c489920",
        "68cb83cb20",
    ),
    (
        "674d401feca02802dd80880000030008000003019078c18cb0",
        "68ebe3cb20",
    ),
    (
        "677a001ebcd940b43db011000003000100000300320f162d96",
        "68ebe3cb22c0",
    ),
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2).expect("hex pair"), 16).expect("hex"))
        .collect()
}

fn start_code(rng: &mut Rng) -> &'static [u8] {
    if rng.chance(50) {
        &[0, 0, 0, 1]
    } else {
        &[0, 0, 1]
    }
}

fn timestamp(prefix: u8, value: u64) -> [u8; 5] {
    [
        (prefix << 4) | (((value >> 29) & 0x0E) as u8) | 1,
        (value >> 22) as u8,
        (((value >> 14) & 0xFE) as u8) | 1,
        (value >> 7) as u8,
        (((value << 1) & 0xFE) as u8) | 1,
    ]
}

fn pes(stream_id: u8, pts: Option<u64>, dts: Option<u64>, extra: usize, data: &[u8]) -> Vec<u8> {
    let (flags, mut header) = match (pts, dts) {
        (Some(pts), Some(dts)) => {
            let mut h = timestamp(3, pts).to_vec();
            h.extend_from_slice(&timestamp(1, dts));
            (0xC0, h)
        }
        (Some(pts), None) => (0x80, timestamp(2, pts).to_vec()),
        _ => (0x00, Vec::new()),
    };
    header.extend(std::iter::repeat_n(0xFF, extra));
    let body_len = 3 + header.len() + data.len();
    let length = if stream_id == 0xE0 || body_len > 0xFFFF {
        0
    } else {
        body_len as u16
    };
    let mut out = vec![0, 0, 1, stream_id];
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&[0x80, flags, header.len() as u8]);
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    out
}

fn adts(rng: &mut Rng, payload_len: usize) -> Vec<u8> {
    let protection_absent = rng.chance(80);
    let header = if protection_absent { 7 } else { 9 };
    let len = header + payload_len;
    let mut out = vec![
        0xFF,
        0xF0 | u8::from(protection_absent),
        (1 << 6) | (3 << 2),
        (2 << 6) | ((len >> 11) & 0x03) as u8,
        (len >> 3) as u8,
        (((len & 7) << 5) as u8) | 0x1F,
        0xFC,
    ];
    if !protection_absent {
        out.extend_from_slice(&[0x12, 0x34]);
    }
    out.extend(rng.bytes(payload_len));
    out
}

struct TsBuilder {
    out: Vec<u8>,
    counters: [u8; 0x2000],
}

impl TsBuilder {
    fn packetize(&mut self, rng: &mut Rng, pid: u16, data: &[u8]) {
        let mut pos = 0;
        let mut first = true;
        while pos < data.len() || first {
            if rng.chance(2) {
                let garbage = rng.below(40) as usize + 1;
                self.out.extend(rng.bytes(garbage));
            }
            if rng.chance(3) {
                // 仅自适应字段、无负载的包。
                self.header(pid, false, 0x20);
                self.out.push(183);
                self.out.push(0x00);
                self.out.extend(std::iter::repeat_n(0xFF, 182));
            }
            let remaining = data.len() - pos;
            let random_af = if remaining > 184 && rng.chance(10) {
                Some(rng.below(30) as usize)
            } else {
                None
            };
            let (af_len, take) = match random_af {
                Some(len) => (Some(len), 184 - 1 - len),
                None if remaining >= 184 => (None, 184),
                None => (Some(184 - 1 - remaining), remaining),
            };
            let afc = if af_len.is_some() { 0x30 } else { 0x10 };
            self.header(pid, first, afc);
            if let Some(len) = af_len {
                self.out.push(len as u8);
                if len > 0 {
                    self.out.push(0x00);
                    self.out.extend(std::iter::repeat_n(0xFF, len - 1));
                }
            }
            self.out.extend_from_slice(&data[pos..pos + take]);
            pos += take;
            first = false;
        }
    }

    fn header(&mut self, pid: u16, payload_start: bool, afc: u8) {
        let counter = &mut self.counters[usize::from(pid)];
        self.out.extend_from_slice(&[
            0x47,
            (u8::from(payload_start) << 6) | ((pid >> 8) as u8 & 0x1F),
            pid as u8,
            afc | (*counter & 0x0F),
        ]);
        *counter = counter.wrapping_add(1);
    }
}

fn psi(rng: &mut Rng, ts: &mut TsBuilder, with_audio: bool) {
    let pat = [
        0x00,
        0x00,
        0xB0,
        0x0D,
        0x00,
        0x01,
        0xC1,
        0x00,
        0x00,
        0x00,
        0x01,
        0xE0 | (PMT_PID >> 8) as u8,
        PMT_PID as u8,
        0x11,
        0x22,
        0x33,
        0x44,
    ];
    ts.packetize(rng, 0, &pat);
    let mut streams = vec![
        0x1B,
        0xE0 | (VIDEO_PID >> 8) as u8,
        VIDEO_PID as u8,
        0xF0,
        0x00,
    ];
    if with_audio {
        streams.extend_from_slice(&[
            0x0F,
            0xE0 | (AUDIO_PID >> 8) as u8,
            AUDIO_PID as u8,
            0xF0,
            0x00,
        ]);
    }
    let section_length = 9 + streams.len() + 4;
    let mut pmt = vec![
        0x00,
        0x02,
        0xB0,
        section_length as u8,
        0x00,
        0x01,
        0xC1,
        0x00,
        0x00,
        0xE1,
        0x00,
        0xF0,
        0x00,
    ];
    pmt.extend_from_slice(&streams);
    pmt.extend_from_slice(&[0x55, 0x66, 0x77, 0x88]);
    ts.packetize(rng, PMT_PID, &pmt);
}

fn video_frame(rng: &mut Rng, keyframe: bool, sps: &[u8], pps: &[u8], big: bool) -> Vec<u8> {
    let mut frame = Vec::new();
    if rng.chance(90) {
        frame.extend_from_slice(start_code(rng));
        frame.extend_from_slice(&[0x09, 0xF0]);
    }
    if keyframe {
        frame.extend_from_slice(start_code(rng));
        frame.extend_from_slice(sps);
        frame.extend_from_slice(start_code(rng));
        frame.extend_from_slice(pps);
    }
    for _ in 0..=rng.below(3) {
        frame.extend_from_slice(start_code(rng));
        frame.push(if keyframe { 0x65 } else { 0x41 });
        let len = if big {
            rng.below(200_000) as usize + 1
        } else {
            rng.below(3000) as usize + 1
        };
        frame.extend(rng.bytes(len));
    }
    if rng.chance(3) {
        frame.extend(std::iter::repeat_n(0, rng.below(4) as usize));
    }
    frame
}

/// 随机合成一条 TS 流。
fn synth(seed: u64, big: bool) -> Vec<u8> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut ts = TsBuilder {
        out: Vec::new(),
        counters: [0; 0x2000],
    };
    if rng.chance(20) {
        let garbage = rng.below(500) as usize;
        ts.out.extend(rng.bytes(garbage));
    }
    let with_audio = !rng.chance(3);
    psi(&mut rng, &mut ts, with_audio);

    let (sps, pps) = PARAMETER_SETS[rng.below(PARAMETER_SETS.len() as u64) as usize];
    let (sps, pps) = (hex(sps), hex(pps));
    let frames = if big { 12 } else { rng.below(60) as usize + 1 };
    let gop = rng.below(12) + 1;
    let with_dts = rng.chance(50);
    let step = [1500u64, 3000, 3003, 3600][rng.below(4) as usize];
    let base = 126_000 + rng.below(90_000);

    let mut audio_pts = base + rng.below(9000);
    let mut audio_pending: Vec<u8> = Vec::new();
    let mut k = 0usize;
    while k < frames {
        let mut es = video_frame(&mut rng, (k as u64).is_multiple_of(gop), &sps, &pps, big);
        let dts = base + k as u64 * step;
        let pts = dts + if with_dts { step * rng.below(3) } else { 0 };
        if rng.chance(10) && k + 1 < frames {
            k += 1;
            es.extend(video_frame(
                &mut rng,
                (k as u64).is_multiple_of(gop),
                &sps,
                &pps,
                big,
            ));
        }
        let pts = (!rng.chance(5)).then_some(pts);
        let dts = if with_dts { pts.map(|_| dts) } else { None };
        if rng.chance(10) && es.len() > 2 {
            let cut = rng.below(es.len() as u64 - 1) as usize + 1;
            ts.packetize(&mut rng, VIDEO_PID, &pes(0xE0, pts, dts, 0, &es[..cut]));
            ts.packetize(&mut rng, VIDEO_PID, &pes(0xE0, None, None, 0, &es[cut..]));
        } else {
            let extra = if rng.chance(10) {
                rng.below(8) as usize
            } else {
                0
            };
            ts.packetize(&mut rng, VIDEO_PID, &pes(0xE0, pts, dts, extra, &es));
        }
        k += 1;

        if with_audio {
            for _ in 0..rng.below(4) {
                if rng.chance(3) {
                    let garbage = rng.below(20) as usize;
                    audio_pending.extend(rng.bytes(garbage));
                }
                let payload = if rng.chance(5) {
                    0
                } else {
                    rng.below(400) as usize
                };
                audio_pending.extend(adts(&mut rng, payload));
            }
            if !audio_pending.is_empty() && rng.chance(70) {
                // 有时把最后一帧切开，残片留给下一个 PES。
                let cut = if rng.chance(20) {
                    rng.below(audio_pending.len() as u64) as usize + 1
                } else {
                    audio_pending.len()
                };
                let chunk: Vec<u8> = audio_pending.drain(..cut).collect();
                let pts = (!rng.chance(5)).then_some(audio_pts);
                ts.packetize(&mut rng, AUDIO_PID, &pes(0xC0, pts, None, 0, &chunk));
                audio_pts += 1920;
            }
        }
        if rng.chance(5) {
            psi(&mut rng, &mut ts, with_audio);
        }
    }
    if rng.chance(30) {
        let tail = rng.below(187) as usize;
        ts.out.extend(rng.bytes(tail));
    }
    ts.out
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fluxdown-ts-remux-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// 以多种读窗口大小运行流式实现，输出必须与 `ts2mp4::convert_ts_to_mp4` 逐字节相同。
fn assert_matches_ts2mp4(ts: &[u8], dir: &Path, label: &str) {
    let expected = ts2mp4::convert_ts_to_mp4(ts).ok();
    let out = dir.join("out.mp4");
    let cancel = CancellationToken::new();
    for chunk in [376, 377, 4099, READ_CHUNK] {
        let actual = remux_with(|| Ok(Cursor::new(ts)), &out, chunk, &cancel)
            .ok()
            .map(|()| std::fs::read(&out).expect("read output"));
        assert!(
            actual == expected,
            "{label} chunk={chunk}: streaming={:?} ts2mp4={:?} bytes",
            actual.as_ref().map(Vec::len),
            expected.as_ref().map(Vec::len),
        );
    }
}

#[test]
fn random_streams_match_ts2mp4_byte_for_byte() {
    let dir = scratch("random");
    let mut converted = 0;
    for seed in 0..400 {
        let ts = synth(seed, false);
        if ts2mp4::convert_ts_to_mp4(&ts).is_ok() {
            converted += 1;
        }
        assert_matches_ts2mp4(&ts, &dir, &format!("seed {seed}"));
    }
    // 生成器绝大多数用例应是可转换的，否则差分没有覆盖到写出路径。
    assert!(converted > 300, "only {converted} convertible cases");
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
}

#[test]
fn large_frames_spanning_many_windows_match_ts2mp4() {
    let dir = scratch("large");
    for seed in 1000..1006 {
        assert_matches_ts2mp4(&synth(seed, true), &dir, &format!("seed {seed}"));
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

/// 真实编码器产出的 H.264 + AAC TS（需 `FLUXDOWN_TEST_FFMPEG` 指向带 libx264 的 ffmpeg）。
#[test]
fn ffmpeg_encoded_stream_matches_ts2mp4() {
    let Some(ffmpeg) = std::env::var_os("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG");
        return;
    };
    let dir = scratch("ffmpeg");
    let ts_path = dir.join("clip.ts");
    let status = std::process::Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=30:duration=4",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
        ])
        .args([
            "-c:v", "libx264", "-bf", "2", "-g", "30", "-c:a", "aac", "-f", "mpegts",
        ])
        .arg(&ts_path)
        .status();
    if !status.is_ok_and(|s| s.success()) {
        eprintln!("[skip] ffmpeg 无法编码 H.264 测试素材");
        std::fs::remove_dir_all(&dir).expect("remove scratch dir");
        return;
    }
    let ts = std::fs::read(&ts_path).expect("read fixture");
    assert!(ts2mp4::convert_ts_to_mp4(&ts).is_ok());
    assert_matches_ts2mp4(&ts, &dir, "ffmpeg fixture");
    std::fs::remove_dir_all(&dir).expect("remove scratch dir");
}
