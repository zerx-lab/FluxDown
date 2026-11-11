//! 已完成 BT 任务的完整性校验：按种子 piece 表全量重哈希最终落盘的数据。
//!
//! 文件映射优先用 parts 边车（选中文件 → 最终路径，未选文件的边界字节在边车 blob）；
//! 无边车的老任务按完成布局推断（`save_dir/file_name` 是目录则按种子相对路径展开，
//! 只有一个文件时即该文件本身），需要未选文件字节的边界 piece 记为跳过而不是损坏。
//! 只读：不改任务状态、不触碰 librqbit 会话。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::task_jobs::TaskJobGuard;
use bytes::Bytes;

use crate::bt_downloader::{
    VerifyFileSpec, VerifySource, torrent_relative_path_is_safe, verify_pieces_core,
};
use crate::bt_partfile;
use crate::logger::{log_info, log_warn};

/// 校验结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtIntegrityStatus {
    /// 全部需要的 piece 哈希一致。
    Intact,
    /// 至少一个需要的 piece 哈希不符或数据不可读（含文件缺失 / 截断）。
    Damaged,
    /// 本地没有种子元数据，或没有任何可比对的 piece。
    Unverifiable,
}

/// 一次完整性校验的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtIntegrityReport {
    pub status: BtIntegrityStatus,
    /// 种子 piece 总数（未取得元数据时为 0）。
    pub total_pieces: u64,
    /// 哈希一致的 piece 数。
    pub verified_pieces: u64,
    /// 损坏的 piece 数。
    pub bad_pieces: u64,
    /// 未校验的 piece 数：不属于所选文件，或缺少可比对的字节。
    pub skipped_pieces: u64,
    /// 被损坏 piece 覆盖的选中文件（种子内相对路径，`/` 分隔，种子顺序）。
    pub damaged_files: Vec<String>,
}

impl BtIntegrityReport {
    fn unverifiable() -> Self {
        Self {
            status: BtIntegrityStatus::Unverifiable,
            total_pieces: 0,
            verified_pieces: 0,
            bad_pieces: 0,
            skipped_pieces: 0,
            damaged_files: Vec::new(),
        }
    }
}

/// 校验无法开始或中途终止。
#[derive(Debug, thiserror::Error)]
pub enum BtIntegrityError {
    #[error("task not found")]
    NotFound,
    #[error("only completed BT tasks can be verified")]
    NotEligible,
    #[error("an integrity check is already running for this task")]
    Busy,
    #[error("integrity check cancelled")]
    Cancelled,
    #[error("integrity check failed: {0}")]
    Internal(String),
}

/// 种子元数据来源（按优先级）。
pub(crate) struct MetadataSources {
    /// 会话中缓存句柄的种子字节（做种中 / 刚完成）。
    pub(crate) live: Option<Bytes>,
    /// 数据库 `torrent_files`（.torrent 建的任务）。
    pub(crate) stored: Option<Bytes>,
    /// librqbit 会话持久化的 `{info_hash}.torrent` 与期望 info-hash（磁力任务）。
    pub(crate) cached: Option<(PathBuf, String)>,
}

/// 一次落盘校验的输入（actor 内收集，阻塞线程执行）。
pub(crate) struct LandedCheck {
    pub(crate) task_id: String,
    pub(crate) save_dir: PathBuf,
    /// 任务落盘的顶层名（`save_dir` 下）。
    pub(crate) file_name: String,
    /// 选中的文件下标；`None` = 全选。
    pub(crate) selection: Option<Vec<usize>>,
    pub(crate) sidecar_path: PathBuf,
    pub(crate) metadata: MetadataSources,
}

/// 在阻塞线程上执行校验。返回的 future 可在 actor 之外 `await`；future 被丢弃时
/// 校验在下一个 piece 处停止。
pub(crate) async fn run(
    check: LandedCheck,
    guard: TaskJobGuard,
) -> Result<BtIntegrityReport, BtIntegrityError> {
    let token = guard.token();
    let task_id = check.task_id.clone();
    log_info!("[BT] integrity check started: task={}", task_id);
    let joined =
        tokio::task::spawn_blocking(move || check_landed(&check, || token.is_cancelled())).await;
    drop(guard);
    let result = joined.map_err(|error| {
        BtIntegrityError::Internal(format!("verification worker failed: {error}"))
    })?;
    match &result {
        Ok(report) => log_info!(
            "[BT] integrity check finished: task={} status={:?} verified={} bad={} skipped={}",
            task_id,
            report.status,
            report.verified_pieces,
            report.bad_pieces,
            report.skipped_pieces
        ),
        Err(error) => log_info!("[BT] integrity check ended: task={} {}", task_id, error),
    }
    result
}

/// 按来源优先级取种子字节；缓存文件的 info-hash 必须与任务一致。
fn load_metadata(sources: &MetadataSources) -> Option<Bytes> {
    if let Some(bytes) = sources.live.as_ref().or(sources.stored.as_ref())
        && !bytes.is_empty()
    {
        return Some(bytes.clone());
    }
    let (path, expected_hash) = sources.cached.as_ref()?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log_warn!(
                "[BT] integrity check: cannot read cached metadata '{}': {}",
                path.display(),
                error
            );
            return None;
        }
    };
    match librqbit::torrent_from_bytes(&bytes) {
        Ok(parsed)
            if parsed
                .info_hash
                .as_string()
                .eq_ignore_ascii_case(expected_hash) => {}
        Ok(_) => {
            log_warn!(
                "[BT] integrity check: cached metadata '{}' belongs to another torrent",
                path.display()
            );
            return None;
        }
        Err(error) => {
            log_warn!(
                "[BT] integrity check: cached metadata '{}' unreadable: {}",
                path.display(),
                error
            );
            return None;
        }
    }
    Some(Bytes::from(bytes))
}

/// 种子内的一个文件。
struct TorrentFile {
    rel: PathBuf,
    display: String,
    /// 文件首字节在种子分片空间里的偏移（纯 v2 种子每个文件从分片边界开始）。
    offset: u64,
    len: u64,
    padding: bool,
}

fn check_landed(
    check: &LandedCheck,
    is_cancelled: impl Fn() -> bool,
) -> Result<BtIntegrityReport, BtIntegrityError> {
    let Some(bytes) = load_metadata(&check.metadata) else {
        return Ok(BtIntegrityReport::unverifiable());
    };
    let parsed = match librqbit::torrent_from_bytes(&bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            log_warn!(
                "[BT] integrity check: torrent metadata unreadable: {}",
                error
            );
            return Ok(BtIntegrityReport::unverifiable());
        }
    };
    let torrent_hash = parsed.info_hash.as_string();
    // v2 / 混合种子的逐片哈希在根级 `piece layers` 里；磁力 / 缺层的种子可能还没带全
    // （层由对端补齐后才写回）：缺的层不算错，只校验能校验的片，其余记为跳过；带了却不对的层仍拒。
    let info = match parsed.validate_with_known_piece_layers() {
        Ok(info) => info,
        Err(error) => {
            log_warn!("[BT] integrity check: torrent metadata invalid: {}", error);
            return Ok(BtIntegrityReport::unverifiable());
        }
    };
    let files: Vec<TorrentFile> = info
        .iter_file_details_ext()
        .map(|fd| TorrentFile {
            rel: fd.details.filename.to_pathbuf(),
            display: fd.details.filename.to_vec().join("/"),
            offset: fd.offset,
            len: fd.details.len,
            padding: fd.details.attrs().padding,
        })
        .collect();

    let sidecar = match bt_partfile::load_sidecar_layout(&check.sidecar_path, &check.save_dir) {
        Ok(Some(layout))
            if layout.info_hash().is_empty()
                || layout.info_hash().eq_ignore_ascii_case(&torrent_hash) =>
        {
            Some(layout)
        }
        Ok(Some(layout)) => {
            log_warn!(
                "[BT] integrity check: parts sidecar info-hash {} != torrent {}; ignoring it",
                layout.info_hash(),
                torrent_hash
            );
            None
        }
        Ok(None) => None,
        Err(error) => {
            log_warn!("[BT] integrity check: parts sidecar unusable: {}", error);
            None
        }
    };

    let is_selected = |index: usize| {
        check
            .selection
            .as_ref()
            .is_none_or(|selection| selection.contains(&index))
    };
    let real_files = files.iter().filter(|file| !file.padding).count();
    let root = check.save_dir.join(&check.file_name);
    let root_is_dir = root.is_dir();
    // 无边车时的落盘路径推断（完成布局：容器目录 / 单文件 / 扁平多文件）。
    let fallback_path = |file: &TorrentFile| -> VerifySource {
        if !torrent_relative_path_is_safe(&file.rel, cfg!(windows)) {
            return VerifySource::Unavailable;
        }
        if root_is_dir {
            VerifySource::File(root.join(&file.rel))
        } else if real_files == 1 {
            VerifySource::File(root.clone())
        } else {
            file.rel
                .file_name()
                .map_or(VerifySource::Unavailable, |name| {
                    VerifySource::File(check.save_dir.join(name))
                })
        }
    };

    let (selected_paths, boundary) = match sidecar {
        Some(layout) => {
            let paths: HashMap<usize, PathBuf> = (0..files.len())
                .filter_map(|index| {
                    layout
                        .selected_path(index)
                        .map(|path| (index, path.to_path_buf()))
                })
                .collect();
            let boundary = match layout.into_boundary_reader() {
                Ok(reader) => Some(Arc::new(reader)),
                Err(error) => {
                    log_warn!(
                        "[BT] integrity check: parts sidecar blob unreadable: {}",
                        error
                    );
                    None
                }
            };
            (Some(paths), boundary)
        }
        None => (None, None),
    };

    let specs: Vec<VerifyFileSpec> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let selected = !file.padding && is_selected(index);
            let source = if file.padding {
                VerifySource::Zeros
            } else if selected {
                match selected_paths.as_ref().and_then(|paths| paths.get(&index)) {
                    Some(path) => VerifySource::File(path.clone()),
                    None => fallback_path(file),
                }
            } else {
                match &boundary {
                    Some(reader) => VerifySource::Sidecar(Arc::clone(reader)),
                    None => VerifySource::Unavailable,
                }
            };
            VerifyFileSpec {
                source,
                offset: file.offset,
                len: file.len,
                selected,
            }
        })
        .collect();

    let outcome = verify_pieces_core(info.lengths(), &specs, is_cancelled, |piece| {
        info.piece_verifier(piece)
    });
    if outcome.cancelled {
        return Err(BtIntegrityError::Cancelled);
    }
    let bad_pieces = outcome.bad.len() as u64;
    let status = if bad_pieces > 0 {
        BtIntegrityStatus::Damaged
    } else if outcome.checked == 0 {
        BtIntegrityStatus::Unverifiable
    } else {
        BtIntegrityStatus::Intact
    };
    Ok(BtIntegrityReport {
        status,
        total_pieces: outcome.checked + bad_pieces + outcome.skipped,
        verified_pieces: outcome.checked,
        bad_pieces,
        skipped_pieces: outcome.skipped,
        damaged_files: outcome
            .damaged_files
            .iter()
            .filter_map(|&index| files.get(index).map(|file| file.display.clone()))
            .collect(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::{Path, PathBuf};

    use bytes::Bytes;

    use super::{BtIntegrityError, BtIntegrityStatus, LandedCheck, MetadataSources, check_landed};

    const PIECE: usize = 4;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_bt_verify_{tag}_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bstr(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(format!("{}:", bytes.len()).as_bytes());
        out.extend_from_slice(bytes);
    }

    fn bint(out: &mut Vec<u8>, value: usize) {
        out.extend_from_slice(format!("i{value}e").as_bytes());
    }

    /// 手工 bencode 的 v1 种子：`files` 为空时是单文件种子（数据 = `single`）。
    fn torrent(name: &str, files: &[(&str, &[u8])], single: Option<&[u8]>) -> Vec<u8> {
        let data: Vec<u8> = match single {
            Some(bytes) => bytes.to_vec(),
            None => files.iter().flat_map(|(_, bytes)| bytes.to_vec()).collect(),
        };
        let mut pieces = Vec::new();
        for chunk in data.chunks(PIECE) {
            use sha1::Digest;
            pieces.extend_from_slice(&sha1::Sha1::digest(chunk));
        }
        let mut out = b"d4:infod".to_vec();
        match single {
            Some(bytes) => {
                bstr(&mut out, b"length");
                bint(&mut out, bytes.len());
            }
            None => {
                bstr(&mut out, b"files");
                out.push(b'l');
                for (path, bytes) in files {
                    out.push(b'd');
                    bstr(&mut out, b"length");
                    bint(&mut out, bytes.len());
                    bstr(&mut out, b"path");
                    out.push(b'l');
                    for part in path.split('/') {
                        bstr(&mut out, part.as_bytes());
                    }
                    out.extend_from_slice(b"ee");
                }
                out.push(b'e');
            }
        }
        bstr(&mut out, b"name");
        bstr(&mut out, name.as_bytes());
        bstr(&mut out, b"piece length");
        bint(&mut out, PIECE);
        bstr(&mut out, b"pieces");
        bstr(&mut out, &pieces);
        out.extend_from_slice(b"ee");
        out
    }

    fn check(
        save_dir: &Path,
        file_name: &str,
        selection: Option<Vec<usize>>,
        bytes: Option<Vec<u8>>,
    ) -> LandedCheck {
        LandedCheck {
            task_id: "t".to_owned(),
            save_dir: save_dir.to_path_buf(),
            file_name: file_name.to_owned(),
            selection,
            sidecar_path: save_dir.join("absent.parts"),
            metadata: MetadataSources {
                live: None,
                stored: bytes.map(Bytes::from),
                cached: None,
            },
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// 多文件种子落在 `save_dir/<file_name>/` 下：完好 → Intact；改一个字节 →
    /// 只有覆盖它的 piece 与文件被报告。
    #[test]
    fn container_layout_reports_only_the_damaged_file() {
        let dir = temp_dir("container");
        let files: [(&str, &[u8]); 2] = [("a.bin", b"AAAABBBB"), ("sub/b.bin", b"CCCCDD")];
        let meta = torrent("Pack", &files, None);
        write(&dir.join("Pack (1)/a.bin"), files[0].1);
        write(&dir.join("Pack (1)/sub/b.bin"), files[1].1);

        let report =
            check_landed(&check(&dir, "Pack (1)", None, Some(meta.clone())), || false).unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Intact);
        assert_eq!((report.total_pieces, report.verified_pieces), (4, 4));

        write(&dir.join("Pack (1)/sub/b.bin"), b"CCCCDX");
        let report = check_landed(&check(&dir, "Pack (1)", None, Some(meta)), || false).unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Damaged);
        assert_eq!((report.bad_pieces, report.verified_pieces), (1, 3));
        assert_eq!(report.damaged_files, vec!["sub/b.bin".to_owned()]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 单文件种子按任务落盘名（可能被重命名）读取；缺失即损坏。
    #[test]
    fn single_file_uses_landed_name_and_flags_missing_data() {
        let dir = temp_dir("single");
        let data = b"0123456789";
        let meta = torrent("orig.iso", &[], Some(data));
        write(&dir.join("renamed.iso"), data);
        let report = check_landed(
            &check(&dir, "renamed.iso", None, Some(meta.clone())),
            || false,
        )
        .unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Intact);
        assert_eq!(report.verified_pieces, 3);

        std::fs::remove_file(dir.join("renamed.iso")).unwrap();
        let report = check_landed(&check(&dir, "renamed.iso", None, Some(meta)), || false).unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Damaged);
        assert_eq!(report.bad_pieces, 3);
        assert_eq!(report.damaged_files, vec!["orig.iso".to_owned()]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 没有元数据时给出「无法校验」，而不是误报损坏或通过。
    #[test]
    fn missing_metadata_is_unverifiable() {
        let dir = temp_dir("nometa");
        let report = check_landed(&check(&dir, "x", None, None), || false).unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Unverifiable);
        assert_eq!(report.total_pieces, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 无边车的部分选择：跨到未选文件的边界 piece 没有可比对字节，记为跳过而非损坏。
    #[test]
    fn partial_selection_without_sidecar_skips_boundary_pieces() {
        let dir = temp_dir("partial");
        // piece 4：a = 6 字节（piece 0、1），b = 6 字节（piece 1、2）；只选 a。
        let files: [(&str, &[u8]); 2] = [("a.bin", b"AAAAAA"), ("b.bin", b"BBBBBB")];
        let meta = torrent("Pack", &files, None);
        write(&dir.join("Pack/a.bin"), files[0].1);
        let report =
            check_landed(&check(&dir, "Pack", Some(vec![0]), Some(meta)), || false).unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Intact);
        assert_eq!(
            (
                report.verified_pieces,
                report.bad_pieces,
                report.skipped_pieces
            ),
            (1, 0, 2)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 边车把选中文件映射到扁平化后的最终路径，并提供未选文件的边界字节：
    /// 边界 piece 照常校验，未选文件不必在磁盘上存在。
    #[test]
    fn sidecar_maps_flattened_paths_and_supplies_boundary_bytes() {
        let dir = temp_dir("sidecar");
        let stage = dir.join(".stage");
        let files: [(&str, &[u8]); 2] = [("a.bin", b"AAAAAA"), ("b.bin", b"BBBBBB")];
        let meta = torrent("Pack", &files, None);
        write(&stage.join("a.bin"), files[0].1);
        write(&stage.join("b.bin"), files[1].1);
        let landed = dir.join("a (1).bin");
        write(&landed, files[0].1);
        let sidecar = dir.join("t.parts");
        let hash = librqbit::torrent_from_bytes(&meta)
            .unwrap()
            .info_hash
            .as_string();
        crate::bt_partfile::write_sidecar(&crate::bt_partfile::SidecarWriteRequest {
            sidecar_path: sidecar.clone(),
            info_hash_hex: hash,
            piece_length: PIECE as u32,
            total_length: 12,
            files: vec![
                crate::bt_partfile::PartsFileMeta {
                    relative_path: "a.bin".into(),
                    len: 6,
                    offset_in_torrent: 0,
                    piece_range: 0..2,
                    padding: false,
                },
                crate::bt_partfile::PartsFileMeta {
                    relative_path: "b.bin".into(),
                    len: 6,
                    offset_in_torrent: 6,
                    piece_range: 1..3,
                    padding: false,
                },
            ],
            selected: vec![(0, landed.clone())],
            save_dir: dir.clone(),
            stage_dir: stage.clone(),
        })
        .unwrap();
        std::fs::remove_dir_all(&stage).unwrap();

        let mut input = check(&dir, "a (1).bin", Some(vec![0]), Some(meta));
        input.sidecar_path = sidecar;
        let report = check_landed(&input, || false).unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Intact);
        assert_eq!(
            (
                report.verified_pieces,
                report.bad_pieces,
                report.skipped_pieces
            ),
            (2, 0, 1)
        );

        write(&landed, b"AAAAAX");
        let report = check_landed(&input, || false).unwrap();
        assert_eq!(report.status, BtIntegrityStatus::Damaged);
        assert_eq!(report.damaged_files, vec!["a.bin".to_owned()]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancellation_stops_the_check() {
        let dir = temp_dir("cancel");
        let data = b"0123456789";
        let meta = torrent("x.bin", &[], Some(data));
        write(&dir.join("x.bin"), data);
        let error = check_landed(&check(&dir, "x.bin", None, Some(meta)), || true).unwrap_err();
        assert!(matches!(error, BtIntegrityError::Cancelled));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
