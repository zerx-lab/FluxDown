//! 流式 MPEG-TS → MP4 重封装（HLS 收尾在 ffmpeg 不可用 / 失败时的兜底）。
//!
//! 输出与 `ts2mp4::convert_ts_to_mp4`（0.3.2）逐字节一致（`tests.rs` 差分验证），
//! 但不把整份 TS 与中间产物读进内存（整份转换的峰值约为文件体积的 6~7 倍）：
//!
//! 1. 第一遍流式解复用，只统计样本大小与时间戳等元数据；
//! 2. 据此构建 `ftyp + moov + mdat 头`（moov 在前，便于边下边播）；
//! 3. 第二遍再解复用，把视频样本与音频样本分别写到各自偏移（两个独立文件句柄）。
//!
//! 常驻内存只有读窗口、一帧数据与样本索引表，与文件体积无关；输出超过 32 位 MP4
//! 盒字段可表示的 4GiB 时拒绝转换（调用方保留 `.ts`），不会写出损坏文件。

mod demux;
mod mp4;
#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

use tokio_util::sync::CancellationToken;

use demux::SampleSink;

/// 读窗口大小。
const READ_CHUNK: usize = 256 * 1024;
/// 视频 / 音频写缓冲各自的大小。
const WRITE_BUFFER: usize = 256 * 1024;

/// 把 `src`（TS）重封装为 `dst`（MP4，已存在则截断）。阻塞 IO，须在阻塞线程调用。
///
/// 失败时 `dst` 可能残留半成品，由调用方清理；取消返回 `ErrorKind::Interrupted`。
pub(crate) fn remux_file(src: &Path, dst: &Path, cancel: &CancellationToken) -> io::Result<()> {
    remux_with(|| File::open(src), dst, READ_CHUNK, cancel)
}

fn remux_with<R: Read>(
    mut open: impl FnMut() -> io::Result<R>,
    dst: &Path,
    chunk: usize,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let mut sizes = SampleSizes::default();
    let meta = demux::demux(open()?, chunk, &mut sizes, cancel)?;
    if meta.video_stream_len == 0 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "No video data found",
        ));
    }

    let header = mp4::build_header(&meta, &sizes.video, &sizes.audio)?;
    let header_len = header.len() as u64;
    let video_total: u64 = sizes.video.iter().map(|&size| u64::from(size)).sum();
    let audio_total: u64 = sizes.audio.iter().map(|&size| u64::from(size)).sum();
    let total = header_len + video_total + audio_total;
    if total > u64::from(u32::MAX) {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "MP4 output would exceed 4 GiB (32-bit box offsets)",
        ));
    }

    let video_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    let audio_file = OpenOptions::new().write(true).open(dst)?;
    let mut video = BufWriter::with_capacity(WRITE_BUFFER, video_file);
    video.write_all(&header)?;
    let mut audio = BufWriter::with_capacity(WRITE_BUFFER, audio_file);
    audio.seek(SeekFrom::Start(header_len + video_total))?;

    let mut writer = SampleWriter {
        video,
        audio,
        expected: &sizes,
        next_video: 0,
        next_audio: 0,
    };
    let replay = demux::demux(open()?, chunk, &mut writer, cancel)?;
    if replay != meta
        || writer.next_video != sizes.video.len()
        || writer.next_audio != sizes.audio.len()
    {
        return Err(diverged());
    }
    writer.video.flush()?;
    writer.audio.flush()?;
    drop(writer);

    let written = std::fs::metadata(dst)?.len();
    if written != total {
        return Err(io::Error::new(
            ErrorKind::UnexpectedEof,
            format!("MP4 output is {written} bytes, expected {total}"),
        ));
    }
    Ok(())
}

fn diverged() -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        "TS input changed between remux passes",
    )
}

/// 第一遍：只记样本大小。
#[derive(Default)]
struct SampleSizes {
    video: Vec<u32>,
    audio: Vec<u32>,
}

fn sample_size(data: &[u8], table: &[u32]) -> io::Result<u32> {
    if table.len() >= demux::MAX_TABLE_ENTRIES {
        return Err(io::Error::new(ErrorKind::OutOfMemory, "too many samples"));
    }
    u32::try_from(data.len())
        .map_err(|_| io::Error::new(ErrorKind::Unsupported, "sample exceeds 4 GiB"))
}

impl SampleSink for SampleSizes {
    fn video_sample(&mut self, data: &[u8]) -> io::Result<()> {
        let size = sample_size(data, &self.video)?;
        self.video.push(size);
        Ok(())
    }

    fn audio_sample(&mut self, data: &[u8]) -> io::Result<()> {
        let size = sample_size(data, &self.audio)?;
        self.audio.push(size);
        Ok(())
    }
}

/// 第二遍：按第一遍的大小表核对并写出样本。
struct SampleWriter<'a> {
    video: BufWriter<File>,
    audio: BufWriter<File>,
    expected: &'a SampleSizes,
    next_video: usize,
    next_audio: usize,
}

fn check_next(expected: &[u32], next: &mut usize, data: &[u8]) -> io::Result<()> {
    match expected.get(*next) {
        Some(&size) if usize::try_from(size).ok() == Some(data.len()) => {
            *next += 1;
            Ok(())
        }
        _ => Err(diverged()),
    }
}

impl SampleSink for SampleWriter<'_> {
    fn video_sample(&mut self, data: &[u8]) -> io::Result<()> {
        check_next(&self.expected.video, &mut self.next_video, data)?;
        self.video.write_all(data)
    }

    fn audio_sample(&mut self, data: &[u8]) -> io::Result<()> {
        check_next(&self.expected.audio, &mut self.next_audio, data)?;
        self.audio.write_all(data)
    }
}
