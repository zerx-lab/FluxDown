//! Windows 下 BT staging 文件的 sparse 存储包装。
//!
//! librqbit 的 `FilesystemStorage` 在初检结束后对选中文件 `set_len` 到完整
//! 大小。NTFS 上非 sparse 文件的 `set_len` 会立即预留全部簇——磁盘可用
//! 空间一次性掉完；且此后每次在高偏移写入都按 valid-data-length 语义把
//! `[VDL, offset)` 之间零填充。BT 的 piece 到达顺序近乎随机，首个高偏移
//! piece 会触发对整个前缀的零写，大文件种子上这是数量级的写放大，也是
//! 下载速率周期性跌落的直接来源。
//!
//! 本包装在 `FilesystemStorage::init` 创建文件之后、任何 `set_len`/写入
//! 之前给每个非 padding 文件打上 `FSCTL_SET_SPARSE`：sparse 文件的
//! `set_len` 只改逻辑大小不预留簇，随机偏移写入没有 VDL 零填充，未写
//! 区域读出为零。全部数据写完后文件实际占用与逻辑大小一致，只留下一个
//! 无害的 sparse 属性；同卷 rename 保留该属性，跨卷 copy 落成普通文件。
//!
//! ext4 / APFS 的 `set_len` 天然稀疏，非 Windows 平台不编译本模块。
//! `mark_sparse` 同时供 HTTP 多段下载预分配临时文件时复用（同一 VDL 机制）。
//!
//! 代价与取舍：sparse 文件放弃了「预分配即占位」带来的两个次要收益——
//! 提前暴露 ENOSPC（改为写入时逐 piece 暴露，librqbit 按致命错误处理，
//! 任务进错误态可重试）与簇的连续性（NTFS 按写入顺序分配，BT 随机写
//! 本就难以连续）。换来的是即点即下、零预留等待、零 VDL 写放大。

#[cfg(feature = "bt")]
use std::any::TypeId;
use std::path::Path;
#[cfg(feature = "bt")]
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex, RwLock, Weak},
};

#[cfg(feature = "bt")]
use librqbit::storage::filesystem::FilesystemStorageFactory;
#[cfg(feature = "bt")]
use librqbit::storage::{
    BoxStorageFactory, MoveStorageOutcome, MoveStorageRequest, StorageFactory, TorrentStorage,
};
#[cfg(feature = "bt")]
use librqbit::{ManagedTorrentShared, TorrentMetadata};

#[cfg(feature = "bt")]
use crate::logger::log_info;

#[cfg(feature = "bt")]
/// 构造注入 `AddTorrentOptions.storage_factory` 的 sparse 包装工厂。`init` 之后按种子
/// 当前的输出目录与文件路径（含重命名）给各文件打 sparse 标记。
pub fn sparse_fs_factory() -> BoxStorageFactory {
    Box::new(SparseFsFactory {
        inner: FilesystemStorageFactory::default(),
    })
}

#[cfg(feature = "bt")]
struct SparseFsFactory {
    inner: FilesystemStorageFactory,
}

#[cfg(feature = "bt")]
impl StorageFactory for SparseFsFactory {
    // `BoxStorageFactory` 要求关联 Storage 为 `Box<dyn TorrentStorage>`。
    type Storage = Box<dyn TorrentStorage>;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<Self::Storage> {
        let storage = Box::new(self.inner.create(shared, metadata)?);
        Ok(Box::new(SparseFsStorage::register(shared, storage)))
    }

    fn is_type_id(&self, type_id: TypeId) -> bool {
        // 伪装为 `FilesystemStorageFactory` 以通过 JSON session 持久化的
        // TypeId 白名单，否则 add_torrent 整体失败。约定与安全性论证同
        // `bt_partfile::FsMasqueradeFactory`：session.json 在每次启动时
        // 先于会话创建被整体清除，其恢复路径从不消费这些条目；本包装
        // 之下就是真正的 FilesystemStorage，即便被按默认 storage 恢复，
        // 行为也一致（仅少一个 sparse 标记）。
        type_id == TypeId::of::<FilesystemStorageFactory>() || type_id == TypeId::of::<Self>()
    }

    fn clone_box(&self) -> BoxStorageFactory {
        Box::new(Self { inner: self.inner })
    }
}

#[cfg(feature = "bt")]
/// 下载时向文件系统存储转发 I/O，并在初始化后标记 sparse。完成搬移后，
/// 同一个 torrent 的上传读请求切换到只读成品，已有对端连接继续使用。
pub struct SparseFsStorage {
    key: usize,
    inner: Arc<StorageCell>,
}

#[cfg(feature = "bt")]
type StorageCell = RwLock<Box<dyn TorrentStorage>>;

// Only the completion path needs to find the live storage from the torrent handle.
// A shared torrent outlives its active storage. Its address distinguishes concurrent sessions.
#[cfg(feature = "bt")]
static ACTIVE: LazyLock<Mutex<HashMap<usize, Weak<StorageCell>>>> = LazyLock::new(Mutex::default);

#[cfg(feature = "bt")]
impl SparseFsStorage {
    fn register(shared: &ManagedTorrentShared, storage: Box<dyn TorrentStorage>) -> Self {
        let key = std::ptr::from_ref(shared).addr();
        let inner = Arc::new(RwLock::new(storage));
        ACTIVE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Arc::downgrade(&inner));
        Self { key, inner }
    }

    fn read(&self) -> anyhow::Result<std::sync::RwLockReadGuard<'_, Box<dyn TorrentStorage>>> {
        self.inner
            .read()
            .map_err(|_| anyhow::anyhow!("torrent storage lock poisoned"))
    }
}

#[cfg(feature = "bt")]
impl Drop for SparseFsStorage {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .get(&self.key)
            .is_some_and(|w| w.ptr_eq(&Arc::downgrade(&self.inner)))
        {
            active.remove(&self.key);
        }
    }
}

#[cfg(feature = "bt")]
pub(crate) fn switch_to_seed_files(
    handle: &crate::bt_downloader::BtHandle,
    sidecar: Option<crate::bt_partfile::PartsSeedStorageFactory>,
) -> anyhow::Result<()> {
    let shared = handle.shared();
    let selected = handle.only_files();
    let storage = handle.with_metadata(|metadata| {
        let mut files = match &sidecar {
            Some(factory) => factory.create(shared, metadata)?,
            None => crate::bt_partfile::PartsSeedStorage::from_paths(
                shared,
                metadata,
                selected.as_deref(),
            )?,
        };
        files.init(shared, metadata)?;
        Ok::<_, anyhow::Error>(files)
    })??;
    // Serialize with take(): rqbit transfers ownership during pause/resume.
    let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    let cell = active
        .get(&std::ptr::from_ref(shared).addr())
        .and_then(Weak::upgrade)
        .ok_or_else(|| anyhow::anyhow!("active torrent storage missing"))?;
    *cell
        .write()
        .map_err(|_| anyhow::anyhow!("torrent storage lock poisoned"))? = Box::new(storage);
    Ok(())
}

#[cfg(feature = "bt")]
impl TorrentStorage for SparseFsStorage {
    fn init(
        &mut self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        self.inner
            .write()
            .map_err(|_| anyhow::anyhow!("torrent storage lock poisoned"))?
            .init(shared, metadata)?;
        // 文件此刻已创建/打开（0 字节或带既有数据），尚未 set_len——
        // 正是打 sparse 标记的窗口。对已有数据的文件重复打标记是幂等
        // 操作，既有分配不受影响。
        let folder = shared.output_folder();
        for (index, fi) in metadata.file_infos.iter().enumerate() {
            if fi.attrs.padding {
                continue;
            }
            let relative = shared
                .file_relative_path(metadata, index)
                .unwrap_or_else(|| fi.relative_filename.clone());
            let path = folder.join(relative);
            if let Err(e) = mark_sparse(&path) {
                // 失败仅记日志：非 sparse 只是回到「全量预留 + VDL 零填充」
                // 的旧行为，不影响数据正确性（FAT32/exFAT 不支持 sparse）。
                log_info!("[BT] failed to mark '{}' sparse: {}", path.display(), e);
            }
        }
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.read()?.pread_exact(file_id, offset, buf)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        self.read()?.pwrite_all(file_id, offset, buf)
    }

    fn pwrite_all_vectored(
        &self,
        file_id: usize,
        offset: u64,
        bufs: [std::io::IoSlice<'_>; 2],
    ) -> anyhow::Result<usize> {
        self.read()?.pwrite_all_vectored(file_id, offset, bufs)
    }

    fn remove_file(&self, file_id: usize, filename: &Path) -> anyhow::Result<()> {
        self.read()?.remove_file(file_id, filename)
    }

    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        self.read()?.remove_directory_if_empty(path)
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> anyhow::Result<()> {
        self.read()?.ensure_file_length(file_id, length)
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let inner = Arc::new(RwLock::new(self.read()?.take()?));
        active.insert(self.key, Arc::downgrade(&inner));
        Ok(Box::new(Self {
            key: self.key,
            inner,
        }))
    }

    fn file_length(&self, file_id: usize) -> anyhow::Result<Option<u64>> {
        self.read()?.file_length(file_id)
    }

    fn rename_file(&self, file_id: usize, from: &Path, to: &Path) -> anyhow::Result<()> {
        self.read()?.rename_file(file_id, from, to)
    }

    fn move_storage(&self, request: &MoveStorageRequest<'_>) -> anyhow::Result<MoveStorageOutcome> {
        self.read()?.move_storage(request)
    }

    fn finish_move_storage(&self, commit: bool) -> anyhow::Result<()> {
        self.read()?.finish_move_storage(commit)
    }

    fn forget_cached_data(&self) -> anyhow::Result<()> {
        self.read()?.forget_cached_data()
    }
}

/// 给已存在的文件打 NTFS sparse 标记。
///
/// `FSCTL_SET_SPARSE` 不带输入缓冲区即「置位」。属性是文件级持久标记：
/// 之后的 `set_len` 扩展不预留簇，任意偏移写入没有 VDL 零填充。
pub(crate) fn mark_sparse(path: &Path) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut returned: u32 = 0;
    // SAFETY: `file` 是本函数持有的合法文件句柄，生命周期覆盖整个调用；
    // FSCTL_SET_SPARSE 允许空输入/输出缓冲区（NULL + 0 长度）；
    // lpBytesReturned 指向栈上有效的 u32；同步调用，OVERLAPPED 为 NULL。
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

    fn unique_test_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown_bt_sparse_test_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn file_attributes(path: &Path) -> u32 {
        use std::os::windows::fs::MetadataExt;
        std::fs::symlink_metadata(path)
            .expect("read sparse file attributes")
            .file_attributes()
    }

    #[test]
    fn mark_sparse_sets_attribute_and_survives_set_len() {
        let dir = unique_test_dir("attr");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("payload.bin");
        std::fs::write(&path, b"").unwrap();

        mark_sparse(&path).unwrap();
        assert_ne!(
            file_attributes(&path) & FILE_ATTRIBUTE_SPARSE_FILE,
            0,
            "sparse attribute must be set after mark_sparse"
        );

        // set_len 之后属性保留，逻辑大小生效。
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(64 * 1024 * 1024).unwrap();
        drop(f);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            64 * 1024 * 1024,
            "logical size must follow set_len"
        );
        assert_ne!(
            file_attributes(&path) & FILE_ATTRIBUTE_SPARSE_FILE,
            0,
            "sparse attribute must survive set_len"
        );

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "remove sparse-file test directory {}: {error}",
                dir.display()
            );
        }
    }

    #[test]
    fn mark_sparse_missing_file_reports_error() {
        let dir = unique_test_dir("missing");
        std::fs::create_dir_all(&dir).unwrap();
        let err = mark_sparse(&dir.join("no_such_file.bin")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "remove sparse-file test directory {}: {error}",
                dir.display()
            );
        }
    }
}
