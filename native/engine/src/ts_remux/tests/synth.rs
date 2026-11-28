//! 测试用 MPEG-TS 合成：随机流（失步垃圾、自适应字段、跨 PES 的帧、缺 AUD、ADTS
//! 残片、B 帧时间戳……）与时间戳完全受控的干净流。

pub(super) const PMT_PID: u16 = 0x1000;
pub(super) const VIDEO_PID: u16 = 0x100;
pub(super) const AUDIO_PID: u16 = 0x101;

/// ADTS `sampling_frequency_index`：48kHz / 44.1kHz。
pub(super) const SF_48000: u8 = 3;
pub(super) const SF_44100: u8 = 4;

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

pub(super) struct Rng(pub u64);

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

/// 一个 AAC-LC ADTS 帧（单个 raw data block）。
pub(super) fn adts_frame(
    frequency_index: u8,
    channels: u8,
    protection_absent: bool,
    payload: &[u8],
) -> Vec<u8> {
    let header = if protection_absent { 7 } else { 9 };
    let len = header + payload.len();
    let mut out = vec![
        0xFF,
        0xF0 | u8::from(protection_absent),
        (1 << 6) | (frequency_index << 2) | (channels >> 2),
        ((channels & 0x03) << 6) | ((len >> 11) & 0x03) as u8,
        (len >> 3) as u8,
        (((len & 7) << 5) as u8) | 0x1F,
        0xFC,
    ];
    if !protection_absent {
        out.extend_from_slice(&[0x12, 0x34]);
    }
    out.extend_from_slice(payload);
    out
}

fn random_adts(rng: &mut Rng, payload_len: usize) -> Vec<u8> {
    let protection_absent = rng.chance(80);
    let payload = rng.bytes(payload_len);
    adts_frame(SF_48000, 2, protection_absent, &payload)
}

pub(super) struct TsBuilder {
    pub out: Vec<u8>,
    counters: [u8; 0x2000],
    /// 随机插入失步垃圾、纯自适应字段包与随机长度的自适应字段。
    noisy: bool,
}

impl TsBuilder {
    pub fn new(noisy: bool) -> Self {
        Self {
            out: Vec::new(),
            counters: [0; 0x2000],
            noisy,
        }
    }

    fn packetize(&mut self, rng: &mut Rng, pid: u16, data: &[u8]) {
        let mut pos = 0;
        let mut first = true;
        while pos < data.len() || first {
            if self.noisy && rng.chance(2) {
                let garbage = rng.below(40) as usize + 1;
                self.out.extend(rng.bytes(garbage));
            }
            if self.noisy && rng.chance(3) {
                // 仅自适应字段、无负载的包。
                self.header(pid, false, 0x20);
                self.out.push(183);
                self.out.push(0x00);
                self.out.extend(std::iter::repeat_n(0xFF, 182));
            }
            let remaining = data.len() - pos;
            let random_af = if self.noisy && remaining > 184 && rng.chance(10) {
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

    pub fn psi(&mut self, rng: &mut Rng, with_audio: bool) {
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
        self.packetize(rng, 0, &pat);
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
        self.packetize(rng, PMT_PID, &pmt);
    }

    pub fn video_pes(&mut self, rng: &mut Rng, pts: Option<u64>, dts: Option<u64>, es: &[u8]) {
        self.packetize(rng, VIDEO_PID, &pes(0xE0, pts, dts, 0, es));
    }

    pub fn audio_pes(&mut self, rng: &mut Rng, pts: Option<u64>, es: &[u8]) {
        self.packetize(rng, AUDIO_PID, &pes(0xC0, pts, None, 0, es));
    }
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

/// 一个以 AUD 开头的干净视频帧（关键帧带 SPS / PPS），负载不含起始码样式的字节。
pub(super) fn clean_video_frame(keyframe: bool) -> Vec<u8> {
    let (sps, pps) = PARAMETER_SETS[0];
    let mut frame = vec![0, 0, 0, 1, 0x09, 0xF0];
    if keyframe {
        frame.extend_from_slice(&[0, 0, 0, 1]);
        frame.extend(hex(sps));
        frame.extend_from_slice(&[0, 0, 0, 1]);
        frame.extend(hex(pps));
    }
    frame.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }]);
    frame.extend(std::iter::repeat_n(0x5A, 64));
    frame
}

/// 随机合成一条 TS 流（48kHz 立体声 ADTS）。
pub(super) fn synth(seed: u64, big: bool) -> Vec<u8> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut ts = TsBuilder::new(true);
    if rng.chance(20) {
        let garbage = rng.below(500) as usize;
        ts.out.extend(rng.bytes(garbage));
    }
    let with_audio = !rng.chance(3);
    ts.psi(&mut rng, with_audio);

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
            ts.video_pes(&mut rng, pts, dts, &es[..cut]);
            ts.video_pes(&mut rng, None, None, &es[cut..]);
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
                audio_pending.extend(random_adts(&mut rng, payload));
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
                ts.audio_pes(&mut rng, pts, &chunk);
                audio_pts += 1920;
            }
        }
        if rng.chance(5) {
            ts.psi(&mut rng, with_audio);
        }
    }
    if rng.chance(30) {
        let tail = rng.below(187) as usize;
        ts.out.extend(rng.bytes(tail));
    }
    ts.out
}

/// 干净流的一个音频 PES：`frames` 个 ADTS 帧共用一个 PTS。
pub(super) struct AudioPes {
    pub pts: u64,
    pub frames: usize,
}

/// 干净流：每个视频 PES 一帧（`(PTS, DTS)`），音频 PES 按给定 PTS / 帧数，
/// 全部 ADTS 帧使用同一组参数。
pub(super) fn clean_stream(
    video: &[(u64, Option<u64>)],
    audio: &[AudioPes],
    frequency_index: u8,
    channels: u8,
) -> Vec<u8> {
    let mut rng = Rng(1);
    let mut ts = TsBuilder::new(false);
    ts.psi(&mut rng, !audio.is_empty());
    let frame = adts_frame(frequency_index, channels, true, &[0x21; 40]);
    for index in 0..video.len().max(audio.len()) {
        if let Some(&(pts, dts)) = video.get(index) {
            ts.video_pes(
                &mut rng,
                Some(pts),
                dts,
                &clean_video_frame(index % 25 == 0),
            );
        }
        if let Some(pes) = audio.get(index) {
            let es: Vec<u8> = frame.repeat(pes.frames);
            ts.audio_pes(&mut rng, Some(pes.pts), &es);
        }
    }
    ts.out
}
