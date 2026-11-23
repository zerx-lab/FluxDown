//! MPEG-TS 流式解复用。
//!
//! 逐包状态机、PES / ADTS / H.264 参数解析与以 AUD 为界的切帧。输入按块读取，样本经
//! [`SampleSink`] 逐个交出：常驻内存只有读窗口、当前帧、音频残片与样本索引表，
//! 与文件体积无关。
//!
//! 时间戳按 H.222.0 语义归属：PES 头的 PTS / DTS 属于**起点落在该 PES 内的第一个**
//! 访问单元（视频帧 / ADTS 帧），同一 PES 里的后续单元不带时间戳，由 [`super::timing`]
//! 按帧长推算。
//!
//! 单帧 / 音频残片 / 索引表超过下方上限时报错（调用方保留 `.ts`），让损坏或恶意的
//! 输入无法把内存撑大。

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read};

use tokio_util::sync::CancellationToken;

const TS_PACKET_SIZE: usize = 188;
const SYNC_BYTE: u8 = 0x47;
/// 单帧（相邻两个 AUD 之间的数据）上限；超过视为损坏流。
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// 音频 PES 累积缓冲上限：正常流里只残留不足一帧的数据。
const MAX_AUDIO_BUFFER_BYTES: usize = 16 * 1024 * 1024;
/// 视频 PES 时间戳 / 音视频样本条目上限：索引表常驻内存，4M 条 ≈ 60fps 下 18 小时。
pub(super) const MAX_TABLE_ENTRIES: usize = 4 * 1024 * 1024;
/// 每处理这么多个 TS 包检查一次取消。
const CANCEL_CHECK_PACKETS: u32 = 4096;

/// 解复用产出的样本消费者：视频为 AVCC（4 字节长度前缀）帧，音频为去掉 ADTS 头的 AAC 帧。
pub(super) trait SampleSink {
    fn video_sample(&mut self, data: &[u8]) -> io::Result<()>;
    fn audio_sample(&mut self, data: &[u8]) -> io::Result<()>;
}

/// 生成 moov 所需的流级元数据（样本字节本身不在这里）。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StreamMeta {
    /// 每个视频样本的 (PTS, DTS)，与视频样本一一对应；同一 PES 内第一帧之后的帧为 `(None, None)`。
    pub video_timestamps: Vec<(Option<u64>, Option<u64>)>,
    /// 每个音频样本的 PTS，与音频样本一一对应；同一 PES 内第一帧之后的帧为 `None`。
    pub audio_timestamps: Vec<Option<u64>>,
    /// ADTS 头声明的编码参数（整条流必须一致）；没有音频样本时为 `None`。
    pub audio_config: Option<AacConfig>,
    pub width: u16,
    pub height: u16,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
    /// 拼接后视频基本流的总字节数；为 0 时不产出 MP4（"No video data found"）。
    pub video_stream_len: u64,
}

/// ADTS `sampling_frequency_index` → 采样率（ISO/IEC 14496-3 表 1.18）。
const AAC_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// AAC 编码参数：MP4 的 `mp4a` / `esds` 与音轨时基都由它决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AacConfig {
    /// MPEG-4 Audio Object Type（ADTS profile + 1：1 Main / 2 LC / 3 SSR / 4 LTP）。
    pub object_type: u8,
    /// `sampling_frequency_index`，解析时已保证 < 13。
    pub frequency_index: u8,
    /// `channel_configuration`，解析时已保证为 1..=7。
    pub channel_config: u8,
}

impl AacConfig {
    pub fn sample_rate(self) -> u32 {
        AAC_SAMPLE_RATES
            .get(usize::from(self.frequency_index))
            .copied()
            .unwrap_or(48_000)
    }

    /// 输出声道数（配置 7 = 7.1 声道）。
    pub fn channels(self) -> u16 {
        if self.channel_config == 7 {
            8
        } else {
            u16::from(self.channel_config)
        }
    }

    /// 2 字节 AudioSpecificConfig：5 位 AOT + 4 位采样率索引 + 4 位声道配置 + 3 位 0。
    pub fn audio_specific_config(self) -> [u8; 2] {
        let bits = (u16::from(self.object_type) << 11)
            | (u16::from(self.frequency_index) << 7)
            | (u16::from(self.channel_config) << 3);
        bits.to_be_bytes()
    }
}

/// 一个 ADTS 帧头。
struct AdtsHeader {
    config: AacConfig,
    frame_length: usize,
    header_len: usize,
    raw_blocks: u8,
}

impl AdtsHeader {
    /// `bytes` 起点不是合法 ADTS 同步头（syncword、layer = 0、采样率索引有效）时返回 `None`。
    fn parse(bytes: &[u8]) -> Option<Self> {
        let header = bytes.get(..7)?;
        if header[0] != 0xFF || (header[1] & 0xF0) != 0xF0 || (header[1] & 0x06) != 0 {
            return None;
        }
        let frequency_index = (header[2] >> 2) & 0x0F;
        if usize::from(frequency_index) >= AAC_SAMPLE_RATES.len() {
            return None;
        }
        let protection_absent = (header[1] & 0x01) == 1;
        Some(Self {
            config: AacConfig {
                object_type: (header[2] >> 6) + 1,
                frequency_index,
                channel_config: ((header[2] & 0x01) << 2) | (header[3] >> 6),
            },
            frame_length: ((usize::from(header[3]) & 0x03) << 11)
                | (usize::from(header[4]) << 3)
                | (usize::from(header[5]) >> 5),
            header_len: if protection_absent { 7 } else { 9 },
            raw_blocks: (header[6] & 0x03) + 1,
        })
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn too_large(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::OutOfMemory, message)
}

/// 完整解复用一遍 TS 输入，样本按流内顺序交给 `sink`。
pub(super) fn demux<R: Read>(
    src: R,
    chunk: usize,
    sink: &mut dyn SampleSink,
    cancel: &CancellationToken,
) -> io::Result<StreamMeta> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(ErrorKind::Interrupted, "remux cancelled"));
    }
    let mut window = PacketWindow::new(src, chunk);
    let mut state = Demuxer::new();

    // 定位第一个同步字节。
    loop {
        if !window.ensure(1)? {
            return Err(invalid("No valid TS sync byte found"));
        }
        if window.first() == SYNC_BYTE {
            break;
        }
        window.advance(1);
    }

    let mut packets_since_check = 0u32;
    while window.ensure(TS_PACKET_SIZE)? {
        packets_since_check += 1;
        if packets_since_check >= CANCEL_CHECK_PACKETS {
            packets_since_check = 0;
            if cancel.is_cancelled() {
                return Err(io::Error::new(ErrorKind::Interrupted, "remux cancelled"));
            }
        }
        if window.first() != SYNC_BYTE {
            // 失步：逐字节找下一个同步字节。
            window.advance(1);
            while window.ensure(1)? && window.first() != SYNC_BYTE {
                window.advance(1);
            }
            continue;
        }
        state.on_packet(window.packet(), sink)?;
        window.advance(TS_PACKET_SIZE);
    }
    state.finish(sink)
}

/// 定长读窗口：只保证当前位置起至少一个 TS 包的可见字节。
struct PacketWindow<R> {
    src: R,
    buf: Box<[u8]>,
    start: usize,
    end: usize,
    eof: bool,
}

impl<R: Read> PacketWindow<R> {
    fn new(src: R, chunk: usize) -> Self {
        let capacity = chunk.max(TS_PACKET_SIZE * 2);
        Self {
            src,
            buf: vec![0; capacity].into_boxed_slice(),
            start: 0,
            end: 0,
            eof: false,
        }
    }

    /// 让窗口至少有 `need`（≤ 一个 TS 包）字节；读到 EOF 仍不足返回 `false`。
    fn ensure(&mut self, need: usize) -> io::Result<bool> {
        while self.end - self.start < need {
            if self.eof {
                return Ok(false);
            }
            if self.start > 0 {
                self.buf.copy_within(self.start..self.end, 0);
                self.end -= self.start;
                self.start = 0;
            }
            match self.src.read(&mut self.buf[self.end..]) {
                Ok(0) => self.eof = true,
                Ok(read) => self.end += read,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    fn first(&self) -> u8 {
        self.buf[self.start]
    }

    fn packet(&self) -> &[u8] {
        &self.buf[self.start..self.start + TS_PACKET_SIZE]
    }

    fn advance(&mut self, by: usize) {
        self.start += by;
    }
}

struct Demuxer {
    meta: StreamMeta,
    pat_pmt_parsed: bool,
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    audio_buffer: Vec<u8>,
    /// 当前音频 PES 的 (负载在 `audio_buffer` 中的起点, PTS)，交给第一个从该处起始的 ADTS 帧。
    audio_mark: Option<(usize, u64)>,
    frames: FrameSplitter,
    /// AVCC 转换的复用缓冲。
    avcc: Vec<u8>,
}

impl Demuxer {
    fn new() -> Self {
        Self {
            meta: StreamMeta {
                video_timestamps: Vec::new(),
                audio_timestamps: Vec::new(),
                audio_config: None,
                width: 1920,
                height: 1080,
                sps: None,
                pps: None,
                video_stream_len: 0,
            },
            pat_pmt_parsed: false,
            pmt_pid: None,
            video_pid: None,
            audio_pid: None,
            audio_buffer: Vec::new(),
            audio_mark: None,
            frames: FrameSplitter::default(),
            avcc: Vec::new(),
        }
    }

    fn on_packet(&mut self, packet: &[u8], sink: &mut dyn SampleSink) -> io::Result<()> {
        let pid = ((u16::from(packet[1]) & 0x1F) << 8) | u16::from(packet[2]);
        let payload_start = (packet[1] & 0x40) != 0;
        let has_adaptation = (packet[3] & 0x20) != 0;
        let has_payload = (packet[3] & 0x10) != 0;

        let mut payload_offset = 4;
        if has_adaptation {
            payload_offset += 1 + usize::from(packet[4]);
        }
        if !has_payload || payload_offset >= TS_PACKET_SIZE {
            return Ok(());
        }
        let payload = &packet[payload_offset..];

        if pid == 0 && !self.pat_pmt_parsed {
            if let Some(pmt) = parse_pat(payload, payload_start) {
                self.pmt_pid = Some(pmt);
            }
        } else if Some(pid) == self.pmt_pid && !self.pat_pmt_parsed {
            if let Some((video, audio)) = parse_pmt(payload, payload_start) {
                self.video_pid = Some(video);
                self.audio_pid = audio;
                self.pat_pmt_parsed = true;
            }
        } else if Some(pid) == self.video_pid {
            self.on_video(payload, payload_start, sink)?;
        } else if Some(pid) == self.audio_pid {
            self.on_audio(payload, payload_start, sink)?;
        }
        Ok(())
    }

    fn on_video(
        &mut self,
        payload: &[u8],
        payload_start: bool,
        sink: &mut dyn SampleSink,
    ) -> io::Result<()> {
        if payload_start && payload.len() >= 9 {
            self.frames
                .mark_pes(self.meta.video_stream_len, pes_timestamps(payload))?;
            let pes = pes_payload(payload);
            if !pes.is_empty() {
                // SPS / PPS 都已拿到时扫描不再改变任何状态，直接跳过。
                if self.meta.sps.is_none() || self.meta.pps.is_none() {
                    extract_h264_params(&mut self.meta, pes);
                }
                self.append_video(pes, sink)?;
            }
        } else if !payload_start && !payload.is_empty() {
            self.append_video(payload, sink)?;
        }
        Ok(())
    }

    fn append_video(&mut self, data: &[u8], sink: &mut dyn SampleSink) -> io::Result<()> {
        self.meta.video_stream_len += data.len() as u64;
        self.frames.push(data, &mut self.avcc, sink)
    }

    fn on_audio(
        &mut self,
        payload: &[u8],
        payload_start: bool,
        sink: &mut dyn SampleSink,
    ) -> io::Result<()> {
        if payload_start && payload.len() >= 9 {
            // 每个包之后都已切走完整帧，缓冲里只剩上一个 PES 不足一帧的残片。
            let (pts, _) = pes_timestamps(payload);
            self.audio_mark = pts.map(|pts| (self.audio_buffer.len(), pts));
            self.extend_audio(pes_payload(payload))?;
            self.drain_audio(sink)?;
        } else if !payload_start && !payload.is_empty() {
            self.extend_audio(payload)?;
            self.drain_audio(sink)?;
        }
        Ok(())
    }

    fn drain_audio(&mut self, sink: &mut dyn SampleSink) -> io::Result<()> {
        let consumed = drain_aac(
            &self.audio_buffer,
            &mut self.audio_mark,
            &mut self.meta,
            sink,
        )?;
        if consumed > 0 {
            self.audio_buffer.drain(..consumed);
            if let Some((start, _)) = &mut self.audio_mark {
                *start = start.saturating_sub(consumed);
            }
        }
        Ok(())
    }

    fn extend_audio(&mut self, data: &[u8]) -> io::Result<()> {
        if self.audio_buffer.len() + data.len() > MAX_AUDIO_BUFFER_BYTES {
            return Err(too_large("audio PES buffer exceeds limit"));
        }
        self.audio_buffer.extend_from_slice(data);
        Ok(())
    }

    fn finish(mut self, sink: &mut dyn SampleSink) -> io::Result<StreamMeta> {
        self.drain_audio(sink)?;
        self.frames.finish(&mut self.avcc, sink)?;
        self.meta.video_timestamps = std::mem::take(&mut self.frames.timestamps);
        Ok(self.meta)
    }
}

fn parse_pat(payload: &[u8], payload_start: bool) -> Option<u16> {
    if !payload_start || payload.len() < 13 {
        return None;
    }
    let offset = usize::from(payload[0]) + 1;
    if offset + 12 >= payload.len() {
        return None;
    }
    if payload[offset] != 0x00 {
        return None;
    }
    Some(((u16::from(payload[offset + 10]) & 0x1F) << 8) | u16::from(payload[offset + 11]))
}

/// 返回 (视频 PID, 音频 PID)；纯视频节目（独立音轨的 HLS 视频流）音频为 `None`。
fn parse_pmt(payload: &[u8], payload_start: bool) -> Option<(u16, Option<u16>)> {
    if !payload_start || payload.len() < 16 {
        return None;
    }
    let mut offset = usize::from(payload[0]) + 1;
    if offset + 12 >= payload.len() {
        return None;
    }
    if payload[offset] != 0x02 {
        return None;
    }
    let section_length = usize::from(
        ((u16::from(payload[offset + 1]) & 0x0F) << 8) | u16::from(payload[offset + 2]),
    );
    let program_info_length = usize::from(
        ((u16::from(payload[offset + 10]) & 0x0F) << 8) | u16::from(payload[offset + 11]),
    );
    offset += 12 + program_info_length;

    let mut video_pid = None;
    let mut audio_pid = None;
    while offset + 5 <= payload.len() && offset < section_length + 3 {
        let stream_type = payload[offset];
        let elementary_pid =
            ((u16::from(payload[offset + 1]) & 0x1F) << 8) | u16::from(payload[offset + 2]);
        let es_info_length = usize::from(
            ((u16::from(payload[offset + 3]) & 0x0F) << 8) | u16::from(payload[offset + 4]),
        );
        match stream_type {
            // H.264 / MPEG-2 视频
            0x1B | 0x02 if video_pid.is_none() => video_pid = Some(elementary_pid),
            // AAC / MPEG 音频
            0x0F | 0x03 if audio_pid.is_none() => audio_pid = Some(elementary_pid),
            _ => {}
        }
        offset += 5 + es_info_length;
    }
    video_pid.map(|video| (video, audio_pid))
}

/// PES 头之后的负载；不是合法 PES 起始时为空。
fn pes_payload(payload: &[u8]) -> &[u8] {
    if payload.len() < 9 || payload[..3] != [0x00, 0x00, 0x01] {
        return &[];
    }
    let start = 9 + usize::from(payload[8]);
    payload.get(start..).unwrap_or_default()
}

fn pes_timestamps(payload: &[u8]) -> (Option<u64>, Option<u64>) {
    if payload.len() < 9 || payload[..3] != [0x00, 0x00, 0x01] {
        return (None, None);
    }
    let flags = (payload[7] >> 6) & 0x03;
    let header_length = usize::from(payload[8]);
    if 9 + header_length > payload.len() {
        return (None, None);
    }
    let pts = (flags >= 2 && header_length >= 5).then(|| read_timestamp(&payload[9..14]));
    let dts = (flags == 3 && header_length >= 10).then(|| read_timestamp(&payload[14..19]));
    (pts, dts)
}

fn read_timestamp(bytes: &[u8]) -> u64 {
    ((u64::from(bytes[0]) & 0x0E) << 29)
        | (u64::from(bytes[1]) << 22)
        | ((u64::from(bytes[2]) & 0xFE) << 14)
        | (u64::from(bytes[3]) << 7)
        | ((u64::from(bytes[4]) & 0xFE) >> 1)
}

/// 在 `data[i..]` 处识别起始码，返回其长度（4 优先于 3）。
fn start_code_len(data: &[u8], i: usize) -> Option<usize> {
    if i + 3 < data.len() && data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
        Some(4)
    } else if i + 2 < data.len() && data[i..i + 3] == [0x00, 0x00, 0x01] {
        Some(3)
    } else {
        None
    }
}

/// `split_into_frames` / `convert_annexb_to_avcc` 的 NAL 结束判据（向前看 4 字节）。
fn is_nal_boundary(data: &[u8], at: usize) -> bool {
    data[at] == 0x00
        && data[at + 1] == 0x00
        && (data[at + 2] == 0x01 || (data[at + 2] == 0x00 && data[at + 3] == 0x01))
}

/// 只在每个视频 PES 的首包负载里找 SPS / PPS，各取第一次出现的那份。
fn extract_h264_params(meta: &mut StreamMeta, pes: &[u8]) {
    let mut i = 0;
    while i + 4 <= pes.len() {
        let Some(start_code) = start_code_len(pes, i) else {
            i += 1;
            continue;
        };
        let nal_start = i + start_code;
        if nal_start >= pes.len() {
            break;
        }
        let nal_type = pes[nal_start] & 0x1F;

        // 该处的结束判据与切帧不同：3 字节起始码只需 `nal_end + 2` 在界内。
        let mut nal_end = nal_start + 1;
        while nal_end + 2 < pes.len() {
            if pes[nal_end] == 0x00 && pes[nal_end + 1] == 0x00 {
                if pes[nal_end + 2] == 0x01 {
                    break;
                }
                if nal_end + 3 < pes.len() && pes[nal_end + 2] == 0x00 && pes[nal_end + 3] == 0x01 {
                    break;
                }
            }
            nal_end += 1;
        }
        let nal_end = nal_end.min(pes.len());

        if nal_type == 7 && meta.sps.is_none() {
            let sps = pes[nal_start..nal_end].to_vec();
            if let Some((width, height)) = parse_sps_resolution(&sps) {
                meta.width = width;
                meta.height = height;
            }
            meta.sps = Some(sps);
        } else if nal_type == 8 && meta.pps.is_none() {
            meta.pps = Some(pes[nal_start..nal_end].to_vec());
        }
        i = nal_end;
    }
}

/// SPS 分辨率解析。算术一律 wrapping：畸形 SPS 得到无意义的尺寸，但不 panic。
fn parse_sps_resolution(sps: &[u8]) -> Option<(u16, u16)> {
    if sps.len() < 4 {
        return None;
    }
    let profile_idc = sps[1];
    let mut bits = BitReader::new(&sps[4..]);
    bits.read_ue()?; // seq_parameter_set_id

    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128
    ) {
        let chroma_format_idc = bits.read_ue()?;
        if chroma_format_idc == 3 {
            bits.read_bit()?; // separate_colour_plane_flag
        }
        bits.read_ue()?; // bit_depth_luma_minus8
        bits.read_ue()?; // bit_depth_chroma_minus8
        bits.read_bit()?; // qpprime_y_zero_transform_bypass_flag
        if bits.read_bit()? {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for _ in 0..count {
                if bits.read_bit()? {
                    let size = if count < 6 { 16 } else { 64 };
                    let mut last_scale: i32 = 8;
                    let mut next_scale: i32 = 8;
                    for _ in 0..size {
                        if next_scale != 0 {
                            let delta_scale = bits.read_se()?;
                            next_scale =
                                last_scale.wrapping_add(delta_scale).wrapping_add(256) % 256;
                        }
                        if next_scale != 0 {
                            last_scale = next_scale;
                        }
                    }
                }
            }
        }
    }

    bits.read_ue()?; // log2_max_frame_num_minus4
    let pic_order_cnt_type = bits.read_ue()?;
    if pic_order_cnt_type == 0 {
        bits.read_ue()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if pic_order_cnt_type == 1 {
        bits.read_bit()?; // delta_pic_order_always_zero_flag
        bits.read_se()?; // offset_for_non_ref_pic
        bits.read_se()?; // offset_for_top_to_bottom_field
        let cycle = bits.read_ue()?;
        for _ in 0..cycle {
            bits.read_se()?; // offset_for_ref_frame[i]
        }
    }
    bits.read_ue()?; // max_num_ref_frames
    bits.read_bit()?; // gaps_in_frame_num_value_allowed_flag
    let width_in_mbs_minus1 = bits.read_ue()?;
    let height_in_map_units_minus1 = bits.read_ue()?;
    let frame_mbs_only = bits.read_bit()?;
    if !frame_mbs_only {
        bits.read_bit()?; // mb_adaptive_frame_field_flag
    }
    bits.read_bit()?; // direct_8x8_inference_flag
    let (mut left, mut right, mut top, mut bottom) = (0u32, 0u32, 0u32, 0u32);
    if bits.read_bit()? {
        left = bits.read_ue()?;
        right = bits.read_ue()?;
        top = bits.read_ue()?;
        bottom = bits.read_ue()?;
    }

    let width = width_in_mbs_minus1
        .wrapping_add(1)
        .wrapping_mul(16)
        .wrapping_sub(left.wrapping_add(right).wrapping_mul(2));
    let field_factor: u32 = if frame_mbs_only { 1 } else { 2 };
    let height = field_factor
        .wrapping_mul(height_in_map_units_minus1.wrapping_add(1))
        .wrapping_mul(16)
        .wrapping_sub(top.wrapping_add(bottom).wrapping_mul(2));
    Some((width as u16, height as u16))
}

/// 指数哥伦布码读取器。
struct BitReader<'a> {
    data: &'a [u8],
    byte_offset: usize,
    bit_offset: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_offset: 0,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Option<bool> {
        let byte = *self.data.get(self.byte_offset)?;
        let bit = (byte >> (7 - self.bit_offset)) & 1;
        self.bit_offset += 1;
        if self.bit_offset == 8 {
            self.bit_offset = 0;
            self.byte_offset += 1;
        }
        Some(bit != 0)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeros = 0;
        while !self.read_bit()? {
            leading_zeros += 1;
            if leading_zeros > 31 {
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let mut value = 1u32;
        for _ in 0..leading_zeros {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Some(value - 1)
    }

    fn read_se(&mut self) -> Option<i32> {
        let code = self.read_ue()?;
        Some(if code % 2 == 0 {
            -((code / 2) as i32)
        } else {
            code.div_ceil(2) as i32
        })
    }
}

/// 从累积缓冲里切出完整 ADTS 帧（去头）交给 `sink`，返回截至最后一个完整帧的字节数
/// （含长度不足以带负载、因而不产出样本的帧）。
///
/// `mark` 的 PTS 交给第一个起点不早于 `mark.0` 的帧（即起始于该 PES 内的第一帧）。
fn drain_aac(
    buffer: &[u8],
    mark: &mut Option<(usize, u64)>,
    meta: &mut StreamMeta,
    sink: &mut dyn SampleSink,
) -> io::Result<usize> {
    let mut offset = 0;
    let mut consumed = 0;
    while offset + 7 < buffer.len() {
        let Some(header) = AdtsHeader::parse(&buffer[offset..]) else {
            offset += 1;
            continue;
        };
        let frame_length = header.frame_length;
        if frame_length < 7 || offset + frame_length > buffer.len() {
            break;
        }
        let pts = match *mark {
            Some((start, pts)) if offset >= start => {
                *mark = None;
                Some(pts)
            }
            _ => None,
        };
        if header.header_len < frame_length {
            accept_aac_config(meta, &header)?;
            if meta.audio_timestamps.len() >= MAX_TABLE_ENTRIES {
                return Err(too_large("too many audio samples"));
            }
            meta.audio_timestamps.push(pts);
            sink.audio_sample(&buffer[offset + header.header_len..offset + frame_length])?;
        }
        offset += frame_length;
        consumed = offset;
    }
    Ok(consumed)
}

/// MP4 只能为一条音轨写一份解码参数：参数中途变化（如插播 48kHz 广告）或无法用
/// 2 字节 AudioSpecificConfig 描述时拒绝转换，调用方保留可正常播放的 `.ts`。
fn accept_aac_config(meta: &mut StreamMeta, header: &AdtsHeader) -> io::Result<()> {
    if header.config.channel_config == 0 {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "AAC channel layout carried in a PCE is not supported",
        ));
    }
    if header.raw_blocks != 1 {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "ADTS frames with multiple raw data blocks are not supported",
        ));
    }
    match meta.audio_config {
        None => meta.audio_config = Some(header.config),
        Some(config) if config != header.config => {
            return Err(invalid("AAC stream parameters change mid-stream"));
        }
        Some(_) => {}
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SplitPhase {
    /// 游标在下一个起始码（或待跳过的字节）处。
    Seek,
    /// 游标在当前 NAL 的起始码处，`nal_end` 为下一个待检查的结束位置。
    ScanEnd { nal_end: usize },
}

/// 视频 PES 的时间戳：(PTS, DTS)。
type PesTimestamps = (Option<u64>, Option<u64>);

/// `split_into_frames` 的流式版本：以 AUD 为界切帧，`window` 只留未消费的尾部。
///
/// 偏移一律以「拼接后视频基本流」计：`base` 是 `window[0]` 的流内偏移。
struct FrameSplitter {
    window: Vec<u8>,
    base: u64,
    pos: usize,
    phase: SplitPhase,
    frame: Vec<u8>,
    /// 当前帧第一个字节的流内偏移。
    frame_start: u64,
    /// 尚未归属到帧的视频 PES：(负载起点的流内偏移, 时间戳)。
    pes_marks: VecDeque<(u64, PesTimestamps)>,
    /// 已产出样本的时间戳，与视频样本一一对应。
    timestamps: Vec<PesTimestamps>,
    /// 流尾遇到「起始码后无数据」时停止切帧，其后的数据全部忽略。
    stopped: bool,
}

impl Default for FrameSplitter {
    fn default() -> Self {
        Self {
            window: Vec::new(),
            base: 0,
            pos: 0,
            phase: SplitPhase::Seek,
            frame: Vec::new(),
            frame_start: 0,
            pes_marks: VecDeque::new(),
            timestamps: Vec::new(),
            stopped: false,
        }
    }
}

impl FrameSplitter {
    /// 记一个视频 PES 起点；`offset` 为它的负载在视频基本流里的起始偏移。
    fn mark_pes(&mut self, offset: u64, timestamps: PesTimestamps) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        if self.pes_marks.len() >= MAX_TABLE_ENTRIES {
            return Err(too_large("too many video PES packets"));
        }
        self.pes_marks.push_back((offset, timestamps));
        Ok(())
    }

    fn push(
        &mut self,
        data: &[u8],
        avcc: &mut Vec<u8>,
        sink: &mut dyn SampleSink,
    ) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        if self.pos > 0 && self.pos * 2 >= self.window.len() {
            self.window.drain(..self.pos);
            if let SplitPhase::ScanEnd { nal_end } = &mut self.phase {
                *nal_end -= self.pos;
            }
            self.base += self.pos as u64;
            self.pos = 0;
        }
        self.window.extend_from_slice(data);
        self.run(false, avcc, sink)
    }

    fn finish(&mut self, avcc: &mut Vec<u8>, sink: &mut dyn SampleSink) -> io::Result<()> {
        self.run(true, avcc, sink)?;
        if !self.frame.is_empty() {
            self.emit(avcc, sink)?;
        }
        Ok(())
    }

    /// 交出当前帧：时间戳取帧起点所在 PES 的那份（该 PES 已被更早的帧取走则为空）。
    fn emit(&mut self, avcc: &mut Vec<u8>, sink: &mut dyn SampleSink) -> io::Result<()> {
        let mut timestamps = (None, None);
        while let Some(&(offset, stamp)) = self.pes_marks.front()
            && offset <= self.frame_start
        {
            timestamps = stamp;
            self.pes_marks.pop_front();
        }
        if emit_frame(&self.frame, avcc, sink)? {
            self.timestamps.push(timestamps);
        }
        self.frame.clear();
        Ok(())
    }

    /// 推进切帧状态机。`eof = false` 时窗口尾部未必是流尾：任何依赖流长度的判定
    /// 都等到看见足够字节（或 EOF）再做，结果与一次看到整条流完全一致。
    fn run(&mut self, eof: bool, avcc: &mut Vec<u8>, sink: &mut dyn SampleSink) -> io::Result<()> {
        while !self.stopped {
            match self.phase {
                SplitPhase::Seek => {
                    let available = self.window.len() - self.pos;
                    if available == 0 || (!eof && available < 5) {
                        return Ok(());
                    }
                    let Some(start_code) = start_code_len(&self.window, self.pos) else {
                        self.pos += 1;
                        continue;
                    };
                    let nal_start = self.pos + start_code;
                    if nal_start >= self.window.len() {
                        // 仅 EOF 时可达（未到 EOF 时至少可见 5 字节）。
                        self.stopped = true;
                        return Ok(());
                    }
                    let nal_type = self.window[nal_start] & 0x1F;
                    if nal_type == 9 && !self.frame.is_empty() {
                        self.emit(avcc, sink)?;
                    }
                    self.phase = SplitPhase::ScanEnd {
                        nal_end: nal_start + 1,
                    };
                }
                SplitPhase::ScanEnd { mut nal_end } => {
                    let mut found = false;
                    while nal_end + 3 < self.window.len() {
                        if is_nal_boundary(&self.window, nal_end) {
                            found = true;
                            break;
                        }
                        nal_end += 1;
                    }
                    if found {
                        self.take_into_frame(nal_end)?;
                        self.phase = SplitPhase::Seek;
                    } else if eof {
                        self.take_into_frame(self.window.len())?;
                        self.phase = SplitPhase::Seek;
                    } else {
                        // `nal_end` 之前确定属于当前 NAL，先并入帧，窗口只留向前看的尾巴。
                        self.take_into_frame(nal_end)?;
                        self.phase = SplitPhase::ScanEnd { nal_end };
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn take_into_frame(&mut self, end: usize) -> io::Result<()> {
        if self.frame.len() + (end - self.pos) > MAX_FRAME_BYTES {
            return Err(too_large("video frame exceeds limit"));
        }
        if self.frame.is_empty() {
            self.frame_start = self.base + self.pos as u64;
        }
        self.frame.extend_from_slice(&self.window[self.pos..end]);
        self.pos = end;
        Ok(())
    }
}

/// 把一帧 Annex B 转成 AVCC，非空时交给 `sink` 并返回 `true`（空帧不成为样本）。
fn emit_frame(frame: &[u8], avcc: &mut Vec<u8>, sink: &mut dyn SampleSink) -> io::Result<bool> {
    annexb_to_avcc(frame, avcc);
    if avcc.is_empty() {
        return Ok(false);
    }
    sink.video_sample(avcc)?;
    Ok(true)
}

/// `convert_annexb_to_avcc`：去掉 SPS / PPS / AUD，其余 NAL 以 4 字节大端长度前缀。
fn annexb_to_avcc(data: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let mut i = 0;
    while i < data.len() {
        let Some(start_code) = start_code_len(data, i) else {
            i += 1;
            continue;
        };
        let nal_start = i + start_code;
        if nal_start >= data.len() {
            break;
        }
        let nal_type = data[nal_start] & 0x1F;
        let mut nal_end = nal_start + 1;
        let mut found = false;
        while nal_end + 3 < data.len() {
            if is_nal_boundary(data, nal_end) {
                found = true;
                break;
            }
            nal_end += 1;
        }
        if !found {
            nal_end = data.len();
        }
        if nal_type != 7 && nal_type != 8 && nal_type != 9 {
            let nal = &data[nal_start..nal_end];
            if !nal.is_empty() {
                // 帧长受 MAX_FRAME_BYTES 约束，必然放得进 u32。
                out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                out.extend_from_slice(nal);
            }
        }
        i = nal_end;
    }
}
