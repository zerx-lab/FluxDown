//! MPEG-TS 流式解复用。
//!
//! 逐包状态机、PES / ADTS / H.264 参数解析与以 AUD 为界的切帧。输入按块读取，样本经
//! [`SampleSink`] 逐个交出：常驻内存只有读窗口、当前帧、音频残片与样本索引表，
//! 与文件体积无关。产出的样本序列与 `ts2mp4::convert_ts_to_mp4`（0.3.2）一致。
//!
//! 单帧 / 音频残片 / 索引表超过下方上限时报错（调用方保留 `.ts`），让损坏或恶意的
//! 输入无法把内存撑大。

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
    /// 每个视频 PES 起始包的 (PTS, DTS)。
    pub frame_timestamps: Vec<(Option<u64>, Option<u64>)>,
    /// 每个音频样本的 PTS（与音频样本一一对应）。
    pub audio_timestamps: Vec<Option<u64>>,
    pub width: u16,
    pub height: u16,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
    /// 拼接后视频基本流的总字节数；为 0 时不产出 MP4（"No video data found"）。
    pub video_stream_len: u64,
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
    current_audio_pts: Option<u64>,
    frames: FrameSplitter,
    /// AVCC 转换的复用缓冲。
    avcc: Vec<u8>,
}

impl Demuxer {
    fn new() -> Self {
        Self {
            meta: StreamMeta {
                frame_timestamps: Vec::new(),
                audio_timestamps: Vec::new(),
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
            current_audio_pts: None,
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
                self.audio_pid = Some(audio);
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
            if self.meta.frame_timestamps.len() >= MAX_TABLE_ENTRIES {
                return Err(too_large("too many video PES packets"));
            }
            self.meta.frame_timestamps.push(pes_timestamps(payload));
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
            // 上一个 PES 的残留先按它自己的 PTS 收尾；即便没切出帧也丢弃已消费字节。
            if !self.audio_buffer.is_empty() {
                let drained = drain_aac(
                    &self.audio_buffer,
                    self.current_audio_pts,
                    &mut self.meta.audio_timestamps,
                    sink,
                )?;
                self.audio_buffer.drain(..drained.consumed);
            }
            let (pts, _) = pes_timestamps(payload);
            self.current_audio_pts = pts;
            self.extend_audio(pes_payload(payload))?;
            let drained = drain_aac(
                &self.audio_buffer,
                pts,
                &mut self.meta.audio_timestamps,
                sink,
            )?;
            if drained.frames > 0 {
                self.audio_buffer.drain(..drained.consumed);
            }
        } else if !payload_start && !payload.is_empty() {
            self.extend_audio(payload)?;
            let drained = drain_aac(
                &self.audio_buffer,
                self.current_audio_pts,
                &mut self.meta.audio_timestamps,
                sink,
            )?;
            if drained.frames > 0 {
                self.audio_buffer.drain(..drained.consumed);
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
        if !self.audio_buffer.is_empty() {
            drain_aac(
                &self.audio_buffer,
                self.current_audio_pts,
                &mut self.meta.audio_timestamps,
                sink,
            )?;
        }
        self.frames.finish(&mut self.avcc, sink)?;
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

fn parse_pmt(payload: &[u8], payload_start: bool) -> Option<(u16, u16)> {
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
    match (video_pid, audio_pid) {
        (Some(video), Some(audio)) => Some((video, audio)),
        _ => None,
    }
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

struct AacDrain {
    frames: usize,
    consumed: usize,
}

/// 从累积缓冲里切出完整 ADTS 帧（去头）交给 `sink`，并为每帧记一条 PTS。
/// `consumed` 截至最后一个完整帧（含长度不足以带负载、因而不产出样本的帧）。
fn drain_aac(
    buffer: &[u8],
    pts: Option<u64>,
    timestamps: &mut Vec<Option<u64>>,
    sink: &mut dyn SampleSink,
) -> io::Result<AacDrain> {
    let mut offset = 0;
    let mut consumed = 0;
    let mut frames = 0;
    while offset + 7 < buffer.len() {
        if buffer[offset] != 0xFF || (buffer[offset + 1] & 0xF0) != 0xF0 {
            offset += 1;
            continue;
        }
        let protection_absent = (buffer[offset + 1] & 0x01) == 1;
        let frame_length = ((usize::from(buffer[offset + 3]) & 0x03) << 11)
            | (usize::from(buffer[offset + 4]) << 3)
            | (usize::from(buffer[offset + 5]) >> 5);
        if frame_length < 7 || offset + frame_length > buffer.len() {
            break;
        }
        let header_size = if protection_absent { 7 } else { 9 };
        if header_size < frame_length {
            if timestamps.len() >= MAX_TABLE_ENTRIES {
                return Err(too_large("too many audio samples"));
            }
            timestamps.push(pts);
            sink.audio_sample(&buffer[offset + header_size..offset + frame_length])?;
            frames += 1;
        }
        offset += frame_length;
        consumed = offset;
    }
    Ok(AacDrain { frames, consumed })
}

#[derive(Clone, Copy)]
enum SplitPhase {
    /// 游标在下一个起始码（或待跳过的字节）处。
    Seek,
    /// 游标在当前 NAL 的起始码处，`nal_end` 为下一个待检查的结束位置。
    ScanEnd { nal_end: usize },
}

/// `split_into_frames` 的流式版本：以 AUD 为界切帧，`window` 只留未消费的尾部。
struct FrameSplitter {
    window: Vec<u8>,
    pos: usize,
    phase: SplitPhase,
    frame: Vec<u8>,
    /// 流尾遇到「起始码后无数据」时停止切帧，其后的数据全部忽略。
    stopped: bool,
}

impl Default for FrameSplitter {
    fn default() -> Self {
        Self {
            window: Vec::new(),
            pos: 0,
            phase: SplitPhase::Seek,
            frame: Vec::new(),
            stopped: false,
        }
    }
}

impl FrameSplitter {
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
            self.pos = 0;
        }
        self.window.extend_from_slice(data);
        self.run(false, avcc, sink)
    }

    fn finish(&mut self, avcc: &mut Vec<u8>, sink: &mut dyn SampleSink) -> io::Result<()> {
        self.run(true, avcc, sink)?;
        if !self.frame.is_empty() {
            emit_frame(&self.frame, avcc, sink)?;
            self.frame.clear();
        }
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
                        emit_frame(&self.frame, avcc, sink)?;
                        self.frame.clear();
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
        self.frame.extend_from_slice(&self.window[self.pos..end]);
        self.pos = end;
        Ok(())
    }
}

/// 把一帧 Annex B 转成 AVCC，非空时交给 `sink`（空帧不成为样本）。
fn emit_frame(frame: &[u8], avcc: &mut Vec<u8>, sink: &mut dyn SampleSink) -> io::Result<()> {
    annexb_to_avcc(frame, avcc);
    if avcc.is_empty() {
        return Ok(());
    }
    sink.video_sample(avcc)
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
