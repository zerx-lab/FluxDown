//! 多段临时文件的落盘布局：启动时整文件预分配，还是随写入增长。
//!
//! FAT32 / exFAT / HFS+ 等没有空洞的文件系统上，把文件延伸到 `len` 必须物理
//! 分配并写零 `[EOF, len)`：Linux exfat / vfat 在 ftruncate 扩展时就同步零填充
//! （`cont_write_begin`），macOS exFAT 推迟到首次高偏移写入时零填充 `[VDL, offset)`。
//! 两者耗时都与距离成正比——整文件预分配或从 `k·N/n` 起写的均匀分段，在
//! ~15 MB/s 的 SD 卡上让 1 GB 任务在首字节前卡近一分钟（#974，Android exFAT SD 卡）。
//!
//! 这类目标改走 [`FileLayout::GrowOnWrite`]：不预分配，全新任务只规划一个段，
//! 拆分时持有者最多保留 [`FRONTIER_WINDOW_BYTES`]、帮手紧接其后起写——所有写入
//! 落在 EOF 附近的窗口内，零填充只覆盖窗口里尚未到达的字节，且很快被真实数据
//! 覆盖（多在回写前，仍停留在页缓存里）。

use std::path::Path;

use crate::downloader::DownloadError;

/// [`FileLayout::GrowOnWrite`] 下拆分时持有者最多保留的字节数。
///
/// 每个窗口 = 一次新 Range 请求；窗口越大请求越少，但并发 worker 领先 EOF 的
/// 零填充上界（约 worker 数 × 窗口）越大。8 MiB 在 1~2 MB/s 单连接下约 4~8s 一次
/// 重建连接，握手开销 < 5%。
pub(super) const FRONTIER_WINDOW_BYTES: i64 = 8 * 1024 * 1024;

/// 探测用的试延伸长度。非稀疏文件系统上代价是写零 1 MiB，可忽略。
#[cfg(unix)]
const EXTEND_PROBE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileLayout {
    /// 延伸文件不写零（稀疏 / fallocate / NTFS sparse）：启动时整文件预分配，
    /// 均匀分段与按速率均衡拆分照常。
    Preallocated,
    /// 延伸文件需物理写零：不预分配，写入贴着 EOF 推进。
    GrowOnWrite,
}

impl FileLayout {
    /// 启动 / 就地扩容时是否把临时文件预分配到规划总量。
    pub(super) fn preallocates(self) -> bool {
        matches!(self, Self::Preallocated)
    }

    /// 拆分时持有者保留量上限；`None` = 不限（按持有者 / 帮手速率均衡）。
    pub(super) fn split_window(self) -> Option<i64> {
        match self {
            Self::Preallocated => None,
            Self::GrowOnWrite => Some(FRONTIER_WINDOW_BYTES),
        }
    }

    /// 新建段表时的段数：随写增长时只建一个覆盖全文件的段，其余连接由拆分
    /// 逐个贴近前沿放出（均匀分段的高偏移起点会立即触发大范围零填充）。
    pub(super) fn fresh_plan_count(self, planned: i32) -> i32 {
        match self {
            Self::Preallocated => planned,
            Self::GrowOnWrite => 1,
        }
    }
}

/// 判定 `dest` 的布局。`dest` 不存在时创建为空文件（随后的预分配 / worker 写入
/// 本就会创建它）。已至少有 `target_len` 字节（上一会话已预分配）时无需延伸，
/// 任何段布局都不会再触发零填充，按 [`FileLayout::Preallocated`] 处理。
///
/// Unix：在当前末尾试延伸 [`EXTEND_PROBE_BYTES`]，看分配块是否随之增长（稀疏
/// 文件系统不增长），随后截回原长——既有数据不受影响。`st_blocks` 在 Linux /
/// Android / macOS 上以 512 字节为单位；判据取一半容忍末簇已有余量。
/// Windows：NTFS 走 sparse 标记，非 NTFS 退回物理预分配（见 `preallocate_file_len`），
/// 保持原行为。
#[cfg(unix)]
pub(super) async fn detect(dest: &Path, target_len: u64) -> Result<FileLayout, DownloadError> {
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || probe_unix(&dest, target_len))
        .await
        .map_err(|e| DownloadError::Other(format!("layout probe task panicked: {e}")))?
        .map_err(DownloadError::Io)
}

/// 见 unix 版文档：Windows 保持原有预分配策略。
#[cfg(not(unix))]
pub(super) async fn detect(_dest: &Path, _target_len: u64) -> Result<FileLayout, DownloadError> {
    Ok(FileLayout::Preallocated)
}

#[cfg(unix)]
fn probe_unix(dest: &Path, target_len: u64) -> std::io::Result<FileLayout> {
    use std::os::unix::fs::MetadataExt;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dest)?;
    let before = file.metadata()?;
    if before.len() >= target_len {
        return Ok(FileLayout::Preallocated);
    }
    file.set_len(before.len().saturating_add(EXTEND_PROBE_BYTES))?;
    let after = file.metadata();
    // 先截回原长再处理元数据错误：探测绝不改变文件长度。
    file.set_len(before.len())?;
    let grown = after?
        .blocks()
        .saturating_sub(before.blocks())
        .saturating_mul(512);
    Ok(if grown >= EXTEND_PROBE_BYTES / 2 {
        FileLayout::GrowOnWrite
    } else {
        FileLayout::Preallocated
    })
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{FileLayout, detect};

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fluxdown-layout-{tag}-{}", uuid::Uuid::new_v4()))
    }

    /// 系统临时目录（APFS / ext4 / tmpfs）延伸不写零：必须保持整文件预分配，
    /// 误判会让所有常规磁盘失去均匀分段。探测不得改变已有部分文件的长度与内容。
    #[tokio::test]
    async fn sparse_temp_dir_keeps_preallocation_and_preserves_partial_file() {
        let dir = unique_dir("sparse");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("payload.bin");
        std::fs::write(&path, b"partial-data").unwrap();

        let layout = detect(&path, 64 * 1024 * 1024).await.unwrap();

        assert_eq!(layout, FileLayout::Preallocated);
        assert_eq!(std::fs::read(&path).unwrap(), b"partial-data");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
