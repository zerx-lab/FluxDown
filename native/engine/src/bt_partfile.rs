//! BT 部分选择做种的 "parts" 边车（sidecar）。
//!
//! 背景：librqbit 的 `FilesystemStorage::init` 会为 torrent 的**每个**非
//! padding 文件 `create(true)`，与 `only_files` 无关。完成搬移后重启续种
//! （re-add）时 `output_folder` 指向真实 save_dir，于是所有未选文件会以
//! 0 字节占位的形式重建在用户目录里；同时跨文件边界 piece 中未选文件的
//! 字节已随 staging 目录删除，边界 piece 的校验/上传只能拿到零填充。
//!
//! 解法：完成时为每个 BT 任务写一个边车文件（`<task_id>.parts`，与
//! librqbit 会话数据同目录），内容包含：
//!
//! 1. **选中文件映射**：file_id → 最终磁盘路径（save_dir 相对）。搬移会
//!    扁平化/去重/重命名，torrent 内部布局与磁盘布局从此解耦；
//! 2. **边界字节**：跨文件边界 piece 中落在未选文件内的字节区间，从
//!    staging 副产物提取（此时 piece 已整体重哈希通过，字节可信）。
//!
//! 续种 re-add 时注入 [`PartsSeedStorageFactory`] 作为该 torrent 的
//! `storage_factory`：选中文件直接映射到最终路径打开（绝不 create），
//! 未选文件的读取路由到只读边车 blob——磁盘上不再出现任何未选文件，
//! 边界 piece 校验与上传均为真实数据。

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
#[cfg(unix)]
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use librqbit::storage::filesystem::FilesystemStorageFactory;
use librqbit::storage::{BoxStorageFactory, StorageFactory, TorrentStorage};
use librqbit::{ManagedTorrentShared, TorrentMetadata};
use serde::{Deserialize, Serialize};

use crate::logger::{log_info, log_warn};

// ---------------------------------------------------------------------------
// 文件格式
// ---------------------------------------------------------------------------
//
//   [0..8)   magic  b"FXPARTS1"
//   [8..12)  header_len: u32 LE
//   [12..12+header_len)  JSON header（PartsHeader）
//   [12+header_len..)    blob（各 SegmentEntry 的字节按 blob_offset 排布）

const PARTS_MAGIC: &[u8; 8] = b"FXPARTS1";
const PARTS_VERSION: u32 = 1;
/// header JSON 长度上限（正常几 KB；超限视为损坏，防解析放大）。
const MAX_HEADER_LEN: u32 = 16 * 1024 * 1024;

/// librqbit 会话持久化目录：`<app_data_dir>/bt_session`（session.json /
/// `{hash}.bitv` / `{hash}.torrent` / dht.json / 边车）。
pub fn persistence_dir(app_data_dir: &str) -> PathBuf {
    PathBuf::from(app_data_dir).join("bt_session")
}

/// 边车文件路径：`<app_data_dir>/bt_session/<task_id>.parts`。
/// 与 librqbit 会话数据（session.json / {hash}.bitv / dht.json）同目录，
/// task_id 为 UUID，不会与上述文件名冲突。
pub fn sidecar_path(app_data_dir: &str, task_id: &str) -> PathBuf {
    persistence_dir(app_data_dir).join(format!("{task_id}.parts"))
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PartsHeader {
    version: u32,
    /// torrent info-hash（hex），加载时与 re-add 的种子交叉校验。
    info_hash: String,
    piece_length: u32,
    total_length: u64,
    selected: Vec<SelectedEntry>,
    segments: Vec<SegmentEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SelectedEntry {
    file_id: usize,
    /// save_dir 相对路径，`/` 分隔（跨平台稳定；加载时逐组件校验）。
    rel_path: String,
    len: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SegmentEntry {
    file_id: usize,
    /// 区间在该文件内的起始偏移。
    offset: u64,
    len: u64,
    /// 区间数据在 blob 内的偏移（相对 blob 起点）。
    blob_offset: u64,
}

// ---------------------------------------------------------------------------
// 完成时：边界区间计算 + 边车写入
// ---------------------------------------------------------------------------

/// 写边车所需的单文件元数据快照（index == librqbit file_id）。
#[derive(Debug, Clone)]
pub struct PartsFileMeta {
    /// torrent 内相对路径（staging 布局与之一致，用于提取副产物）。
    pub relative_path: PathBuf,
    pub len: u64,
    pub offset_in_torrent: u64,
    pub piece_range: Range<u32>,
    pub padding: bool,
}

/// 计算未选文件中被「需要的 piece」覆盖的字节区间。
///
/// librqbit 的 needed piece 集合 = 选中文件 piece_range 的并集
/// （`compute_selected_pieces`）。未选文件只有首/尾 piece 可能与其他文件
/// 共享（内部 piece 完全落在自身范围内），故每个未选文件最多产生两个区间；
/// 若整个文件被单个边界 piece 覆盖，则区间即整个文件。
fn boundary_segments(
    files: &[PartsFileMeta],
    selected: &HashSet<usize>,
    piece_length: u64,
    total_length: u64,
) -> Vec<(usize, u64, u64)> {
    let sel_ranges: Vec<&Range<u32>> = files
        .iter()
        .enumerate()
        .filter(|(id, f)| selected.contains(id) && !f.padding)
        .map(|(_, f)| &f.piece_range)
        .collect();
    let needed = |p: u32| sel_ranges.iter().any(|r| r.contains(&p));

    let mut out: Vec<(usize, u64, u64)> = Vec::new();
    for (id, f) in files.iter().enumerate() {
        if selected.contains(&id) || f.padding || f.len == 0 || f.piece_range.is_empty() {
            continue;
        }
        let first = f.piece_range.start;
        let last = f.piece_range.end - 1;
        let mut candidates = vec![first];
        if last != first {
            candidates.push(last);
        }
        for p in candidates {
            if !needed(p) {
                continue;
            }
            let piece_start = u64::from(p) * piece_length;
            let piece_end = (piece_start + piece_length).min(total_length);
            let file_end = f.offset_in_torrent + f.len;
            let start = piece_start.max(f.offset_in_torrent);
            let end = piece_end.min(file_end);
            if start < end {
                out.push((id, start - f.offset_in_torrent, end - start));
            }
        }
    }
    out
}

/// 边车写入请求（完成路径在 piece 重哈希通过后、staging 删除前构造）。
pub struct SidecarWriteRequest {
    pub sidecar_path: PathBuf,
    pub info_hash_hex: String,
    pub piece_length: u32,
    pub total_length: u64,
    /// 全部文件的元数据快照，index == file_id。
    pub files: Vec<PartsFileMeta>,
    /// (file_id, 最终磁盘绝对路径)，仅选中的非 padding 文件。
    pub selected: Vec<(usize, PathBuf)>,
    pub save_dir: PathBuf,
    pub stage_dir: PathBuf,
}

/// 把 dst 绝对路径规约为 save_dir 相对、`/` 分隔的字符串。
fn rel_path_string(dst: &Path, save_dir: &Path) -> Result<String, String> {
    let rel = dst
        .strip_prefix(save_dir)
        .map_err(|_| format!("dst '{}' not under save_dir", dst.display()))?;
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(s) => {
                let s = s
                    .to_str()
                    .ok_or_else(|| format!("non-utf8 path component in '{}'", rel.display()))?;
                parts.push(s.to_string());
            }
            other => {
                return Err(format!("unexpected path component {other:?}"));
            }
        }
    }
    if parts.is_empty() {
        return Err("empty relative path".to_string());
    }
    Ok(parts.join("/"))
}

/// 校验并还原 save_dir 相对路径。逐组件词法校验（拒绝 `..`/盘符/分隔符），
/// 防止损坏或被篡改的边车把文件映射到 save_dir 之外。
fn resolve_rel_path(rel: &str, save_dir: &Path) -> Result<PathBuf, String> {
    let mut path = save_dir.to_path_buf();
    let mut any = false;
    for comp in rel.split('/') {
        if comp.is_empty()
            || comp == "."
            || comp == ".."
            || comp.contains('\\')
            || comp.contains(':')
        {
            return Err(format!("unsafe path component '{comp}' in '{rel}'"));
        }
        path.push(comp);
        any = true;
    }
    if !any {
        return Err("empty relative path".to_string());
    }
    Ok(path)
}

/// 完成时写边车：提取边界字节 + 记录选中文件映射。
///
/// 同步阻塞 I/O——调用方须置于 `spawn_blocking`。写入走临时文件 + rename，
/// 失败只影响下次续种的体验（回退到无边车的旧行为），不阻塞完成。
/// 返回写入的边界区间数。
pub fn write_sidecar(req: &SidecarWriteRequest) -> Result<usize, String> {
    let selected_ids: HashSet<usize> = req.selected.iter().map(|(id, _)| *id).collect();
    let segs = boundary_segments(
        &req.files,
        &selected_ids,
        u64::from(req.piece_length),
        req.total_length,
    );

    let mut selected_entries: Vec<SelectedEntry> = Vec::with_capacity(req.selected.len());
    for (id, dst) in &req.selected {
        let meta = req
            .files
            .get(*id)
            .ok_or_else(|| format!("selected file_id {id} out of range"))?;
        selected_entries.push(SelectedEntry {
            file_id: *id,
            rel_path: rel_path_string(dst, &req.save_dir)?,
            len: meta.len,
        });
    }

    // blob 偏移：按区间顺序累加。
    let mut segment_entries: Vec<SegmentEntry> = Vec::with_capacity(segs.len());
    let mut blob_cursor: u64 = 0;
    for (id, offset, len) in &segs {
        segment_entries.push(SegmentEntry {
            file_id: *id,
            offset: *offset,
            len: *len,
            blob_offset: blob_cursor,
        });
        blob_cursor += *len;
    }

    let header = PartsHeader {
        version: PARTS_VERSION,
        info_hash: req.info_hash_hex.clone(),
        piece_length: req.piece_length,
        total_length: req.total_length,
        selected: selected_entries,
        segments: segment_entries,
    };
    let header_json = serde_json::to_vec(&header).map_err(|e| format!("serialize header: {e}"))?;
    let header_len = u32::try_from(header_json.len()).map_err(|_| "header too large")?;
    if header_len > MAX_HEADER_LEN {
        return Err("header too large".to_string());
    }

    if let Some(parent) = req.sidecar_path.parent() {
        crate::output::ensure_dir_sync(parent).map_err(|e| e.to_string())?;
    }
    let tmp_path = req.sidecar_path.with_extension("parts.tmp");
    let mut out =
        File::create(&tmp_path).map_err(|e| format!("create '{}': {e}", tmp_path.display()))?;
    let write_err = |e: std::io::Error| format!("write '{}': {e}", tmp_path.display());
    out.write_all(PARTS_MAGIC).map_err(write_err)?;
    out.write_all(&header_len.to_le_bytes())
        .map_err(write_err)?;
    out.write_all(&header_json).map_err(write_err)?;

    // 逐区间从 staging 副产物拷贝字节。piece 重哈希已通过，读失败即异常，
    // 整体放弃（不写半截边车）。
    let mut copy_buf = vec![0u8; 256 * 1024];
    for (id, offset, len) in &segs {
        let meta = &req.files[*id];
        let src_path = req.stage_dir.join(&meta.relative_path);
        let mut src = File::open(&src_path)
            .map_err(|e| format!("open staging '{}': {e}", src_path.display()))?;
        src.seek(SeekFrom::Start(*offset))
            .map_err(|e| format!("seek staging '{}': {e}", src_path.display()))?;
        let mut remaining = *len;
        while remaining > 0 {
            let chunk = usize::try_from(remaining.min(copy_buf.len() as u64))
                .map_err(|_| "chunk overflow".to_string())?;
            src.read_exact(&mut copy_buf[..chunk])
                .map_err(|e| format!("read staging '{}': {e}", src_path.display()))?;
            out.write_all(&copy_buf[..chunk]).map_err(write_err)?;
            remaining -= chunk as u64;
        }
    }
    out.sync_all().map_err(write_err)?;
    drop(out);
    std::fs::rename(&tmp_path, &req.sidecar_path).map_err(|e| {
        format!(
            "rename '{}' -> '{}': {e}",
            tmp_path.display(),
            req.sidecar_path.display()
        )
    })?;
    Ok(segs.len())
}

/// 删除任务的边车文件（NotFound 静默）。
pub fn remove_sidecar(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => log_info!("[BT] removed parts sidecar {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log_warn!(
            "[BT] failed to remove parts sidecar {}: {e}",
            path.display()
        ),
    }
}

// ---------------------------------------------------------------------------
// 续种时：边车加载 + 自定义 storage
// ---------------------------------------------------------------------------

/// blob 内一个已提取区间（blob_pos 为边车文件内的绝对偏移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SegSpec {
    offset: u64,
    len: u64,
    blob_pos: u64,
}

struct SeedLayout {
    sidecar_path: PathBuf,
    info_hash: String,
    /// file_id → 最终磁盘绝对路径。
    selected: HashMap<usize, PathBuf>,
    /// file_id → 边界区间（按 offset 升序）。
    parts: HashMap<usize, Vec<SegSpec>>,
    max_file_id: usize,
}

/// 从边车构建的做种 storage 工厂。注入 `AddTorrentOptions.storage_factory`
/// 后，该 torrent 的所有文件 I/O 都不再经过 librqbit 的 FilesystemStorage。
#[derive(Clone)]
pub struct PartsSeedStorageFactory {
    inner: Arc<SeedLayout>,
}

/// 读取边车并构建 storage 工厂。
///
/// - 文件不存在 → `Ok(None)`（老任务/全选任务，按平台默认的续种方式打开原路径）；
/// - 存在但损坏/不安全 → `Err`（调用方记日志后按平台默认的续种方式打开原路径）。
pub fn load_seed_factory(
    sidecar_path: &Path,
    save_dir: &Path,
) -> Result<Option<PartsSeedStorageFactory>, String> {
    Ok(
        load_seed_layout(sidecar_path, save_dir)?.map(|layout| PartsSeedStorageFactory {
            inner: Arc::new(layout),
        }),
    )
}

/// 读取边车的只读视图（完整性校验用），语义同 [`load_seed_factory`]。
pub fn load_sidecar_layout(
    sidecar_path: &Path,
    save_dir: &Path,
) -> Result<Option<SidecarLayout>, String> {
    Ok(load_seed_layout(sidecar_path, save_dir)?.map(|inner| SidecarLayout { inner }))
}

/// 边车的只读视图：选中文件的最终路径 + 未选文件的边界字节。
pub struct SidecarLayout {
    inner: SeedLayout,
}

impl SidecarLayout {
    /// 写入边车时记录的 info-hash（hex，可能为空）。
    pub fn info_hash(&self) -> &str {
        &self.inner.info_hash
    }

    /// 选中文件 `file_id` 的最终磁盘路径。
    pub fn selected_path(&self, file_id: usize) -> Option<&Path> {
        self.inner.selected.get(&file_id).map(PathBuf::as_path)
    }

    /// 打开边界字节读取器（只读；无任何边界区间时不打开 blob）。
    pub fn into_boundary_reader(self) -> std::io::Result<BoundaryReader> {
        let blob = if self.inner.parts.values().any(|segs| !segs.is_empty()) {
            Some(File::open(&self.inner.sidecar_path)?)
        } else {
            None
        };
        Ok(BoundaryReader {
            blob,
            parts: self.inner.parts,
        })
    }
}

/// 未选文件边界字节的只读来源（边车 blob）。
pub struct BoundaryReader {
    blob: Option<File>,
    parts: HashMap<usize, Vec<SegSpec>>,
}

impl BoundaryReader {
    /// 读取未选文件 `file_id` 的 `[offset, offset+buf.len())`；未提取的空洞零填充
    /// （需要的边界 piece 不会落在空洞上，落上即哈希不符）。
    pub fn read(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let segments = self.parts.get(&file_id).map_or(&[][..], Vec::as_slice);
        match &self.blob {
            Some(blob) => read_parts_ranges(blob, segments, offset, buf).map(|_| ()),
            None => {
                buf.fill(0);
                Ok(())
            }
        }
    }
}

fn load_seed_layout(sidecar_path: &Path, save_dir: &Path) -> Result<Option<SeedLayout>, String> {
    let mut f = match File::open(sidecar_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("open '{}': {e}", sidecar_path.display())),
    };
    let read_err = |e: std::io::Error| format!("read '{}': {e}", sidecar_path.display());
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic).map_err(read_err)?;
    if &magic != PARTS_MAGIC {
        return Err("bad magic".to_string());
    }
    let mut len_buf = [0u8; 4];
    f.read_exact(&mut len_buf).map_err(read_err)?;
    let header_len = u32::from_le_bytes(len_buf);
    if header_len == 0 || header_len > MAX_HEADER_LEN {
        return Err(format!("bad header length {header_len}"));
    }
    let mut header_buf = vec![0u8; header_len as usize];
    f.read_exact(&mut header_buf).map_err(read_err)?;
    let header: PartsHeader =
        serde_json::from_slice(&header_buf).map_err(|e| format!("parse header: {e}"))?;
    if header.version != PARTS_VERSION {
        return Err(format!("unsupported version {}", header.version));
    }
    let blob_base = 12u64 + u64::from(header_len);
    let file_len = f
        .metadata()
        .map_err(|e| format!("stat '{}': {e}", sidecar_path.display()))?
        .len();
    let blob_len = file_len.saturating_sub(blob_base);

    let mut selected: HashMap<usize, PathBuf> = HashMap::with_capacity(header.selected.len());
    let mut max_file_id = 0usize;
    for entry in &header.selected {
        let path = resolve_rel_path(&entry.rel_path, save_dir)?;
        if selected.insert(entry.file_id, path).is_some() {
            return Err(format!("duplicate selected file_id {}", entry.file_id));
        }
        max_file_id = max_file_id.max(entry.file_id);
    }
    if selected.is_empty() {
        return Err("sidecar has no selected files".to_string());
    }
    let mut parts: HashMap<usize, Vec<SegSpec>> = HashMap::new();
    for entry in &header.segments {
        if selected.contains_key(&entry.file_id) {
            return Err(format!("segment for selected file_id {}", entry.file_id));
        }
        let end = entry
            .blob_offset
            .checked_add(entry.len)
            .ok_or("segment overflow")?;
        if end > blob_len {
            return Err(format!("segment beyond blob end ({end} > {blob_len})"));
        }
        max_file_id = max_file_id.max(entry.file_id);
        parts.entry(entry.file_id).or_default().push(SegSpec {
            offset: entry.offset,
            len: entry.len,
            blob_pos: blob_base + entry.blob_offset,
        });
    }
    for segs in parts.values_mut() {
        segs.sort_by_key(|s| s.offset);
    }

    Ok(Some(SeedLayout {
        sidecar_path: sidecar_path.to_path_buf(),
        info_hash: header.info_hash,
        selected,
        parts,
        max_file_id,
    }))
}

/// 把工厂装箱为 `BoxStorageFactory` 的自定义包装。
///
/// 不用 librqbit 的 `StorageFactoryExt::boxed()`，因为 JSON session
/// 持久化（`update_db`）按 TypeId 白名单只接受 `FilesystemStorageFactory`，
/// 其余 storage 在 `add_torrent` 时直接整体失败（"storages other than
/// FilesystemStorageFactory are not supported"）。持久化对 storage 的
/// 全部依赖只是这个类型判断——序列化内容（trackers/info_hash/only_files/
/// paused/output_folder + 种子字节文件）与 storage 实现无关，因此这里在
/// `is_type_id` 上报 `FilesystemStorageFactory` 的 TypeId 以通过白名单。
///
/// 安全性：唯一消费该持久化条目的是 librqbit 启动时的会话恢复（用默认
/// FilesystemStorage 重建），而 FluxDown 每次启动都在创建会话**之前**
/// 删除 session.json（`clear_stale_session_state`），恢复路径从不会
/// 消费这些条目；即便极端情况下被恢复，也只是回退到默认 storage 的
/// 降级行为（重建占位文件），不会破坏数据。
#[derive(Clone)]
enum FsMasqueradeFactory {
    Sidecar(PartsSeedStorageFactory),
    #[cfg(windows)]
    OriginalPaths(Option<Vec<usize>>),
}

impl StorageFactory for FsMasqueradeFactory {
    type Storage = Box<dyn TorrentStorage>;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<Self::Storage> {
        match self {
            Self::Sidecar(factory) => Ok(Box::new(factory.create(shared, metadata)?)),
            #[cfg(windows)]
            Self::OriginalPaths(selected) => Ok(Box::new(PartsSeedStorage::from_paths(
                shared,
                metadata,
                selected.as_deref(),
            )?)),
        }
    }

    fn is_type_id(&self, type_id: TypeId) -> bool {
        type_id == TypeId::of::<FilesystemStorageFactory>()
            || type_id == TypeId::of::<PartsSeedStorageFactory>()
    }

    fn clone_box(&self) -> BoxStorageFactory {
        Box::new(self.clone())
    }
}

#[cfg(windows)]
pub(crate) fn original_paths_seed_factory(selected: Option<Vec<usize>>) -> BoxStorageFactory {
    Box::new(FsMasqueradeFactory::OriginalPaths(selected))
}

impl PartsSeedStorageFactory {
    /// 供注入 `AddTorrentOptions.storage_factory`。
    pub fn into_boxed(self) -> BoxStorageFactory {
        Box::new(FsMasqueradeFactory::Sidecar(self))
    }
}

impl StorageFactory for PartsSeedStorageFactory {
    type Storage = PartsSeedStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<PartsSeedStorage> {
        let torrent_hash = shared.info_hash.as_string();
        if !self.inner.info_hash.is_empty()
            && !self.inner.info_hash.eq_ignore_ascii_case(&torrent_hash)
        {
            anyhow::bail!(
                "parts sidecar info-hash mismatch ({} != {})",
                self.inner.info_hash,
                torrent_hash
            );
        }
        let n = metadata.file_infos.len();
        if self.inner.max_file_id >= n {
            anyhow::bail!(
                "parts sidecar references file_id {} but torrent has {} files",
                self.inner.max_file_id,
                n
            );
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(n);
        for (id, fi) in metadata.file_infos.iter().enumerate() {
            if fi.attrs.padding {
                slots.push(Slot::Padding);
            } else if let Some(path) = self.inner.selected.get(&id) {
                slots.push(Slot::Selected {
                    path: path.clone(),
                    file: RwLock::new(None),
                });
            } else {
                slots.push(Slot::Parts {
                    segments: Arc::new(self.inner.parts.get(&id).cloned().unwrap_or_default()),
                });
            }
        }
        Ok(PartsSeedStorage {
            slots,
            blob_path: self.inner.sidecar_path.clone(),
            blob: RwLock::new(None),
            warned_uncovered: AtomicBool::new(false),
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().into_boxed()
    }
}

enum Slot {
    Selected {
        path: PathBuf,
        file: RwLock<Option<File>>,
    },
    Parts {
        segments: Arc<Vec<SegSpec>>,
    },
    Padding,
}

/// 做种专用 storage：选中文件按边车映射的最终路径打开（绝不创建），
/// 未选文件路由到边车 blob，padding 恒零。
pub struct PartsSeedStorage {
    slots: Vec<Slot>,
    blob_path: PathBuf,
    blob: RwLock<Option<File>>,
    warned_uncovered: AtomicBool,
}

fn poisoned() -> anyhow::Error {
    anyhow::anyhow!("parts storage lock poisoned")
}

/// 严格定位读：短读视为错误（librqbit Windows 端的 seek_read 宽容短读，
/// 这里不复刻该缺陷）。
fn pread_file(f: &File, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
    #[cfg(target_family = "unix")]
    {
        f.read_exact_at(buf, offset)?;
        Ok(())
    }
    #[cfg(target_family = "windows")]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                anyhow::bail!("short read at offset {}", offset + done as u64);
            }
            done += n;
        }
        Ok(())
    }
}

/// 从 blob 区间集合中读取 `[offset, offset+buf.len())`。
/// 未覆盖的空洞零填充；返回是否出现过空洞。
///
/// 空洞并非必然异常：初始全量校验会哈希未选文件的内部 piece（不在
/// needed 集合内，哈希不匹配只是标记为「没有」），此时读到的正是空洞。
fn read_parts_ranges(
    blob: &File,
    segments: &[SegSpec],
    offset: u64,
    buf: &mut [u8],
) -> anyhow::Result<bool> {
    let end = offset + buf.len() as u64;
    let mut cur = offset;
    let mut had_gap = false;
    for seg in segments {
        let seg_end = seg.offset + seg.len;
        if seg_end <= cur {
            continue;
        }
        if seg.offset >= end {
            break;
        }
        if seg.offset > cur {
            let gap_end = seg.offset.min(end);
            let (a, b) = (
                usize::try_from(cur - offset)?,
                usize::try_from(gap_end - offset)?,
            );
            buf[a..b].fill(0);
            had_gap = true;
            cur = gap_end;
            if cur >= end {
                break;
            }
        }
        let take_end = seg_end.min(end);
        if take_end > cur {
            let (a, b) = (
                usize::try_from(cur - offset)?,
                usize::try_from(take_end - offset)?,
            );
            pread_file(blob, seg.blob_pos + (cur - seg.offset), &mut buf[a..b])?;
            cur = take_end;
        }
        if cur >= end {
            break;
        }
    }
    if cur < end {
        let a = usize::try_from(cur - offset)?;
        buf[a..].fill(0);
        had_gap = true;
    }
    Ok(had_gap)
}

#[cfg(unix)]
fn write_parts_ranges(
    blob: &File,
    segments: &[SegSpec],
    offset: u64,
    buf: &[u8],
) -> anyhow::Result<bool> {
    let end = offset + buf.len() as u64;
    let mut covered: u64 = 0;
    for seg in segments {
        let seg_end = seg.offset + seg.len;
        let start = seg.offset.max(offset);
        let stop = seg_end.min(end);
        if start >= stop {
            continue;
        }
        let (a, b) = (
            usize::try_from(start - offset)?,
            usize::try_from(stop - offset)?,
        );
        blob.write_all_at(&buf[a..b], seg.blob_pos + (start - seg.offset))?;
        covered += stop - start;
    }
    Ok(covered < buf.len() as u64)
}

impl PartsSeedStorage {
    #[cfg(windows)]
    pub(crate) fn from_paths(
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
        selected: Option<&[usize]>,
    ) -> anyhow::Result<Self> {
        let folder = shared.output_folder();
        let selected = selected.map(|ids| ids.iter().copied().collect::<HashSet<_>>());
        let mut slots = Vec::with_capacity(metadata.file_infos.len());
        for (id, info) in metadata.file_infos.iter().enumerate() {
            if info.attrs.padding {
                slots.push(Slot::Padding);
                continue;
            }
            let rel = shared
                .file_relative_path(metadata, id)
                .ok_or_else(|| anyhow::anyhow!("missing torrent file {id}"))?;
            anyhow::ensure!(
                crate::bt_downloader::torrent_relative_path_is_safe(&rel, cfg!(windows)),
                "unsafe torrent path: {}",
                rel.display()
            );
            let path = folder.join(rel);
            if selected.as_ref().is_some_and(|ids| !ids.contains(&id)) && !path.try_exists()? {
                slots.push(Slot::Parts {
                    segments: Arc::new(Vec::new()),
                });
            } else {
                slots.push(Slot::Selected {
                    path,
                    file: RwLock::new(None),
                });
            }
        }
        Ok(Self {
            slots,
            blob_path: PathBuf::new(),
            blob: RwLock::new(None),
            warned_uncovered: AtomicBool::new(false),
        })
    }

    fn warn_uncovered_once(&self, file_id: usize, offset: u64, len: usize, op: &str) {
        if !self.warned_uncovered.swap(true, Ordering::Relaxed) {
            log_info!(
                "[BT] parts storage: {op} outside extracted ranges (file_id={file_id} offset={offset} len={len}) — zero-filled (expected for non-needed pieces during full recheck)"
            );
        }
    }

    fn blob_read<R>(&self, f: impl FnOnce(&File) -> anyhow::Result<R>) -> anyhow::Result<R> {
        let guard = self.blob.read().map_err(|_| poisoned())?;
        let file = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("parts blob not opened (storage taken?)"))?;
        f(file)
    }
}

impl TorrentStorage for PartsSeedStorage {
    fn init(
        &mut self,
        _shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        // 选中文件：只打开、绝不创建。缺失/被移动 → 直接失败，续种走
        // 「本地数据不完整」的既有失败路径，不落任何新文件。
        for slot in &self.slots {
            if let Slot::Selected { path, file } = slot {
                let f = File::options()
                    .read(true)
                    .write(cfg!(not(windows)))
                    .open(path)
                    .map_err(|e| anyhow::anyhow!("open final file '{}': {e}", path.display()))?;
                *file.write().map_err(|_| poisoned())? = Some(f);
            }
        }
        let needs_blob = self
            .slots
            .iter()
            .any(|s| matches!(s, Slot::Parts { segments } if !segments.is_empty()));
        if needs_blob {
            let f = File::options()
                .read(true)
                .write(cfg!(not(windows)))
                .open(&self.blob_path)
                .map_err(|e| {
                    anyhow::anyhow!("open parts sidecar '{}': {e}", self.blob_path.display())
                })?;
            *self.blob.write().map_err(|_| poisoned())? = Some(f);
        }
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        match self.slots.get(file_id) {
            Some(Slot::Selected { file, path }) => {
                let guard = file.read().map_err(|_| poisoned())?;
                let f = guard
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("file '{}' not opened", path.display()))?;
                pread_file(f, offset, buf)
            }
            Some(Slot::Parts { segments }) => {
                let had_gap =
                    self.blob_read(|blob| read_parts_ranges(blob, segments, offset, buf))?;
                if had_gap {
                    self.warn_uncovered_once(file_id, offset, buf.len(), "read");
                }
                Ok(())
            }
            Some(Slot::Padding) => {
                buf.fill(0);
                Ok(())
            }
            None => anyhow::bail!("no such file_id {file_id}"),
        }
    }

    #[cfg(unix)]
    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        match self.slots.get(file_id) {
            Some(Slot::Selected { file, path }) => {
                let guard = file.read().map_err(|_| poisoned())?;
                let f = guard
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("file '{}' not opened", path.display()))?;
                f.write_all_at(buf, offset)?;
                Ok(())
            }
            Some(Slot::Parts { segments }) => {
                let dropped =
                    self.blob_read(|blob| write_parts_ranges(blob, segments, offset, buf))?;
                if dropped {
                    self.warn_uncovered_once(file_id, offset, buf.len(), "write");
                }
                Ok(())
            }
            Some(Slot::Padding) => Ok(()),
            None => anyhow::bail!("no such file_id {file_id}"),
        }
    }

    #[cfg(not(unix))]
    fn pwrite_all(&self, _file_id: usize, _offset: u64, _buf: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!("completed torrent storage is read-only")
    }

    fn remove_file(&self, file_id: usize, _filename: &Path) -> anyhow::Result<()> {
        // 删除任务（含文件）时按映射删真实产物；未选文件本就不存在。
        match self.slots.get(file_id) {
            Some(Slot::Selected { path, .. }) => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(anyhow::anyhow!("remove '{}': {e}", path.display())),
            },
            _ => Ok(()),
        }
    }

    fn remove_directory_if_empty(&self, _path: &Path) -> anyhow::Result<()> {
        // librqbit 传入的是 torrent 内部布局的目录，与磁盘实际布局
        //（扁平化/重命名后）无对应关系；不做任何删除。
        Ok(())
    }

    fn file_length(&self, file_id: usize) -> anyhow::Result<Option<u64>> {
        match self.slots.get(file_id) {
            Some(Slot::Selected { file, path }) => {
                let guard = file.read().map_err(|_| poisoned())?;
                let file = guard
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("file '{}' not opened", path.display()))?;
                Ok(Some(file.metadata()?.len()))
            }
            // Neither boundary fragments nor padding represent a complete file on disk.
            Some(Slot::Parts { .. } | Slot::Padding) => Ok(None),
            None => anyhow::bail!("no such file_id {file_id}"),
        }
    }

    fn ensure_file_length(&self, file_id: usize, len: u64) -> anyhow::Result<()> {
        if let Some(actual) = self.file_length(file_id)?
            && actual != len
        {
            anyhow::bail!("file {file_id} length {actual} != expected {len}; refusing to resize");
        }
        Ok(())
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        let slots = self
            .slots
            .iter()
            .map(|s| {
                Ok(match s {
                    Slot::Selected { path, file } => Slot::Selected {
                        path: path.clone(),
                        file: RwLock::new(file.write().map_err(|_| poisoned())?.take()),
                    },
                    Slot::Parts { segments } => Slot::Parts {
                        segments: Arc::clone(segments),
                    },
                    Slot::Padding => Slot::Padding,
                })
            })
            .collect::<anyhow::Result<Vec<Slot>>>()?;
        Ok(Box::new(Self {
            slots,
            blob_path: self.blob_path.clone(),
            blob: RwLock::new(self.blob.write().map_err(|_| poisoned())?.take()),
            warned_uncovered: AtomicBool::new(self.warned_uncovered.load(Ordering::Relaxed)),
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        PartsFileMeta, SegSpec, SidecarWriteRequest, boundary_segments, load_seed_factory,
        read_parts_ranges, resolve_rel_path, sidecar_path, write_sidecar,
    };
    use std::collections::HashSet;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};

    fn unique_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown_parts_test_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// 三文件布局，piece 长 4:
    ///   A: bytes [0,10)   pieces 0..3
    ///   B: bytes [10,16)  pieces 2..4
    ///   C: bytes [16,24)  pieces 4..6
    fn abc_files() -> Vec<PartsFileMeta> {
        vec![
            PartsFileMeta {
                relative_path: PathBuf::from("A.bin"),
                len: 10,
                offset_in_torrent: 0,
                piece_range: 0..3,
                padding: false,
            },
            PartsFileMeta {
                relative_path: PathBuf::from("B.bin"),
                len: 6,
                offset_in_torrent: 10,
                piece_range: 2..4,
                padding: false,
            },
            PartsFileMeta {
                relative_path: PathBuf::from("C.bin"),
                len: 8,
                offset_in_torrent: 16,
                piece_range: 4..6,
                padding: false,
            },
        ]
    }

    #[test]
    fn boundary_segments_selects_only_shared_boundary_pieces() {
        // 只选 B：piece 2 与 A 尾部共享（A[8..10)），piece 3 完全在 B 内，
        // C 从 piece 4 开始不与 B 共享 → 只提取 A 的 2 字节。
        let files = abc_files();
        let selected: HashSet<usize> = [1].into_iter().collect();
        let segs = boundary_segments(&files, &selected, 4, 24);
        assert_eq!(segs, vec![(0usize, 8u64, 2u64)]);
    }

    #[test]
    fn boundary_segments_tiny_file_fully_inside_needed_piece() {
        // piece 长 16，三个文件都在 piece 0 内：选 A → B、C 全量提取。
        let files = vec![
            PartsFileMeta {
                relative_path: PathBuf::from("A.bin"),
                len: 6,
                offset_in_torrent: 0,
                piece_range: 0..1,
                padding: false,
            },
            PartsFileMeta {
                relative_path: PathBuf::from("B.bin"),
                len: 4,
                offset_in_torrent: 6,
                piece_range: 0..1,
                padding: false,
            },
            PartsFileMeta {
                relative_path: PathBuf::from("C.bin"),
                len: 3,
                offset_in_torrent: 10,
                piece_range: 0..1,
                padding: false,
            },
        ];
        let selected: HashSet<usize> = [0].into_iter().collect();
        let segs = boundary_segments(&files, &selected, 16, 13);
        assert_eq!(segs, vec![(1, 0, 4), (2, 0, 3)]);
    }

    #[test]
    fn boundary_segments_skips_padding_and_selected() {
        let mut files = abc_files();
        files[0].padding = true;
        let selected: HashSet<usize> = [1].into_iter().collect();
        // A 变 padding → 不提取。
        assert!(boundary_segments(&files, &selected, 4, 24).is_empty());
    }

    #[test]
    fn resolve_rel_path_rejects_traversal() {
        let save = Path::new("save");
        assert!(resolve_rel_path("a/b.bin", save).is_ok());
        assert!(resolve_rel_path("../evil", save).is_err());
        assert!(resolve_rel_path("a/../b", save).is_err());
        assert!(resolve_rel_path("", save).is_err());
        assert!(resolve_rel_path("c:/x", save).is_err());
        assert!(resolve_rel_path("a\\b", save).is_err());
    }

    #[test]
    fn sidecar_roundtrip_and_ranges() {
        let dir = unique_dir("roundtrip");
        let stage = dir.join("stage");
        let save = dir.join("save");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::create_dir_all(&save).unwrap();
        // staging 副产物：A 尾部 2 字节位于 [8,10)。
        std::fs::write(stage.join("A.bin"), b"0123456789").unwrap();
        // 选中 B，最终扁平搬到 save/B.bin。
        std::fs::write(save.join("B.bin"), b"BBBBBB").unwrap();

        let sidecar = dir.join("task-1.parts");
        let req = SidecarWriteRequest {
            sidecar_path: sidecar.clone(),
            info_hash_hex: "aa".repeat(20),
            piece_length: 4,
            total_length: 24,
            files: abc_files(),
            selected: vec![(1, save.join("B.bin"))],
            save_dir: save.clone(),
            stage_dir: stage,
        };
        assert_eq!(write_sidecar(&req).unwrap(), 1);

        let factory = load_seed_factory(&sidecar, &save).unwrap().unwrap();
        assert_eq!(factory.inner.selected.get(&1), Some(&save.join("B.bin")));
        let segs = factory.inner.parts.get(&0).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!((segs[0].offset, segs[0].len), (8, 2));

        // blob 内容即 A[8..10) == "89"。
        let mut f = File::open(&sidecar).unwrap();
        f.seek(SeekFrom::Start(segs[0].blob_pos)).unwrap();
        let mut got = [0u8; 2];
        f.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"89");

        // read_parts_ranges: 覆盖区间取 blob，空洞零填充。
        let mut buf = [0xFFu8; 4];
        let had_gap = read_parts_ranges(&f, segs, 6, &mut buf).unwrap();
        assert!(had_gap);
        assert_eq!(&buf, &[0, 0, b'8', b'9']);

        #[cfg(unix)]
        {
            use super::write_parts_ranges;
            // 覆盖内写入生效，区间外丢弃。
            let fw = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&sidecar)
                .unwrap();
            let dropped = write_parts_ranges(&fw, segs, 8, b"XY").unwrap();
            assert!(!dropped);
            let dropped = write_parts_ranges(&fw, segs, 0, b"zz").unwrap();
            assert!(dropped);
            let mut buf = [0u8; 2];
            read_parts_ranges(&fw, segs, 8, &mut buf).unwrap();
            assert_eq!(&buf, b"XY");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_missing_sidecar_is_none_and_corrupt_is_err() {
        let dir = unique_dir("load");
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("nope.parts");
        assert!(load_seed_factory(&missing, &dir).unwrap().is_none());

        let corrupt = dir.join("bad.parts");
        std::fs::write(&corrupt, b"garbage!").unwrap();
        assert!(load_seed_factory(&corrupt, &dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_parts_ranges_handles_multi_segments() {
        let dir = unique_dir("multiseg");
        std::fs::create_dir_all(&dir).unwrap();
        let blob_path = dir.join("blob.bin");
        std::fs::write(&blob_path, b"HELLOWORLD").unwrap();
        let segs = vec![
            SegSpec {
                offset: 0,
                len: 5,
                blob_pos: 0,
            },
            SegSpec {
                offset: 10,
                len: 5,
                blob_pos: 5,
            },
        ];
        let f = File::open(&blob_path).unwrap();
        let mut buf = [0xAAu8; 15];
        let had_gap = read_parts_ranges(&f, &segs, 0, &mut buf).unwrap();
        assert!(had_gap);
        assert_eq!(&buf[0..5], b"HELLO");
        assert_eq!(&buf[5..10], &[0u8; 5]);
        assert_eq!(&buf[10..15], b"WORLD");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_path_layout() {
        let p = sidecar_path("appdata", "task-x");
        assert!(p.ends_with(Path::new("bt_session").join("task-x.parts")));
    }
}
