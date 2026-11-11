//! BT 任务的逐文件状态：优先级与重命名表的落库格式、改动校验，以及「文件」页的行。
//!
//! 「选中哪些文件」的事实源仍是 `tasks.bt_selected_files`（建任务 / 续传 / 完成搬移都读它）；
//! 本模块在其上叠加两张稀疏表，都按种子内的文件下标（含 padding 文件的原始编号）寻址：
//! - `tasks.bt_file_priorities`：`下标:级别` 以 `,` 分隔，只记选中且不是「普通」的文件；
//! - `tasks.bt_file_renames`：JSON 对象 `{"下标": "任务目录内的新相对路径"}`。
//!
//! 级别取值与 librqbit / libtorrent 一致：`0` 不下载、`1` 普通、`6` 高、`7` 最高，其余按大小比较。

use std::collections::BTreeMap;
use std::path::Path;

/// 不下载。
pub const PRIORITY_SKIP: u8 = 0;
/// 普通（默认）。
pub const PRIORITY_NORMAL: u8 = 1;
/// 级别上限。
pub const PRIORITY_MAX: u8 = 7;

/// 单个路径组件的字节上限（主流文件系统的文件名上限）。
const MAX_COMPONENT_BYTES: usize = 255;

/// 「文件」页的一行（不含 padding 文件）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BtTaskFile {
    /// 种子内的文件下标。
    pub index: u32,
    /// 任务目录内的相对路径（`/` 分隔）；重命名过的文件为当前路径。
    pub path: String,
    pub size: u64,
    /// 已下载并校验通过的字节。
    pub downloaded: u64,
    pub priority: u8,
}

/// 一个 BT 任务的文件表。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BtTaskFiles {
    /// 能否改优先级 / 重命名：任务未完成、不在完成收尾中且已取得种子元数据。
    pub editable: bool,
    pub files: Vec<BtTaskFile>,
}

/// 种子里一个文件的静态事实，[`build_rows`] 与改动校验的输入。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TorrentFileFact {
    /// 任务目录内的当前相对路径（`/` 分隔）。
    pub path: String,
    pub size: u64,
    /// 文件在种子字节流里的起点（分片按它对应到文件）。
    pub offset: u64,
    /// padding 文件（BEP 47）：不下载、不展示、不可改。
    pub padding: bool,
}

/// 从种子字节读出的文件布局（任务不在会话里时「文件」页的数据来源）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TorrentLayout {
    /// 小写十六进制 info hash。
    pub info_hash: String,
    pub piece_length: u64,
    pub files: Vec<TorrentFileFact>,
}

/// 解析种子字节得到文件布局；不是合法种子返回 `None`。
#[must_use]
pub fn layout_from_torrent_bytes(torrent_bytes: &[u8]) -> Option<TorrentLayout> {
    let parsed = librqbit::torrent_from_bytes(torrent_bytes).ok()?;
    let info_hash = parsed.info_hash.as_string();
    let info = parsed.info.data.validate().ok()?;
    // 偏移取种子自己的分片空间：纯 v2 种子每个文件从分片边界开始，不是前一个文件的末尾。
    let files = info
        .iter_file_details_ext()
        .map(|detail| TorrentFileFact {
            path: detail.details.filename.to_vec().join("/"),
            size: detail.details.len,
            offset: detail.offset,
            padding: detail.details.attrs().padding,
        })
        .collect();
    Some(TorrentLayout {
        info_hash,
        piece_length: u64::from(info.info().piece_length),
        files,
    })
}

/// 由 have 位图折算每个文件已下载并校验的字节（下标对齐 `files`）。
///
/// 位图为 librqbit 快速恢复文件的格式：第 `i` 片 = 第 `i / 8` 字节的 `0x80 >> (i % 8)` 位；
/// 位图比种子短时缺的片按「没有」算。跨文件的片只把落在本文件内的那一段计给它。
#[must_use]
pub fn progress_from_bitfield(
    files: &[TorrentFileFact],
    piece_length: u64,
    have: &[u8],
) -> Vec<u64> {
    if piece_length == 0 {
        return vec![0; files.len()];
    }
    let has_piece = |piece: u64| {
        usize::try_from(piece / 8)
            .ok()
            .and_then(|byte| have.get(byte))
            .is_some_and(|byte| byte & (0x80u8 >> (piece % 8)) != 0)
    };
    files
        .iter()
        .map(|file| {
            if file.size == 0 {
                return 0;
            }
            let end = file.offset.saturating_add(file.size);
            let first = file.offset / piece_length;
            let last = (end - 1) / piece_length;
            (first..=last)
                .filter(|piece| has_piece(*piece))
                .map(|piece| {
                    let start = piece.saturating_mul(piece_length).max(file.offset);
                    let stop = (piece.saturating_add(1))
                        .saturating_mul(piece_length)
                        .min(end);
                    stop.saturating_sub(start)
                })
                .sum()
        })
        .collect()
}

/// 改优先级 / 重命名被拒的原因。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FileChangeError {
    #[error("file {0} does not exist in this torrent")]
    UnknownFile(u32),
    #[error("priority {0} is out of range (0-7)")]
    InvalidPriority(u8),
    #[error("at least one file must be downloaded")]
    NothingSelected,
    #[error("the file path is not valid")]
    InvalidPath,
    #[error("another file already uses this path")]
    PathConflict,
}

/// BT 任务级操作（改优先级 / 重命名 / 重新汇报 / 强制校验 / 移动 / 超级做种）失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum BtTaskOpError {
    /// 任务不存在或不是 BT 任务。
    #[error("task not found")]
    NotFound,
    /// 参数无效（下标 / 级别 / 路径）。
    #[error("{0}")]
    InvalidArgument(String),
    /// 任务当前状态不允许该操作（未运行、已完成、正在收尾、正在移动…）。
    #[error("{0}")]
    StateConflict(String),
    /// 目标位置已被占用（移动的目标目录里已有同名文件 / 文件夹）。
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Operation(String),
}

impl From<FileChangeError> for BtTaskOpError {
    fn from(error: FileChangeError) -> Self {
        Self::InvalidArgument(error.to_string())
    }
}

impl From<crate::db::DbError> for BtTaskOpError {
    fn from(error: crate::db::DbError) -> Self {
        Self::Operation(format!("{error:#}"))
    }
}

/// 解析落库的优先级表。格式不对、级别越界或「不下载 / 普通」（不该入表）的条目丢弃：
/// 旧库 / 手改库里的脏值不能让任务起不来。
#[must_use]
pub fn parse_priority_table(text: &str) -> BTreeMap<u32, u8> {
    text.split(',')
        .filter_map(|entry| {
            let (index, level) = entry.trim().split_once(':')?;
            let index = index.parse::<u32>().ok()?;
            let level = level.parse::<u8>().ok()?;
            (level > PRIORITY_NORMAL && level <= PRIORITY_MAX).then_some((index, level))
        })
        .collect()
}

/// 每个文件的级别 → 落库的优先级表（只记高于「普通」的文件；全普通为空串）。
#[must_use]
pub fn format_priority_table(levels: &[u8]) -> String {
    let mut out = String::new();
    for (index, level) in levels.iter().enumerate() {
        if *level > PRIORITY_NORMAL {
            if !out.is_empty() {
                out.push(',');
            }
            out.push_str(&format!("{index}:{level}"));
        }
    }
    out
}

/// 由落库的选择与优先级表得到每个文件的生效级别（下标对齐 `files`）。
///
/// `selection` 取 `Db::load_bt_selected_files` 的语义：`None`（尚未确认）与空列表都表示
/// 全选；否则只有列出的下标被选中。未选中与 padding 文件为 `0`，选中的文件取表中级别，
/// 不在表中为「普通」。
#[must_use]
pub fn file_levels(
    files: &[TorrentFileFact],
    selection: Option<&[i32]>,
    table: &BTreeMap<u32, u8>,
) -> Vec<u8> {
    let subset = selection.filter(|indices| !indices.is_empty());
    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            if file.padding {
                return PRIORITY_SKIP;
            }
            let selected = subset.is_none_or(|indices| {
                indices
                    .iter()
                    .any(|chosen| usize::try_from(*chosen).is_ok_and(|chosen| chosen == index))
            });
            if !selected {
                return PRIORITY_SKIP;
            }
            u32::try_from(index)
                .ok()
                .and_then(|index| table.get(&index).copied())
                .unwrap_or(PRIORITY_NORMAL)
        })
        .collect()
}

/// 把一批改动应用到 `levels`（下标对齐 `files`），返回新的级别表。
///
/// 任一条目无效（文件不存在 / 是 padding、级别越界）或改完后没有任何文件要下载，
/// 整批拒绝、不改任何值。同一文件出现多次取最后一次。
pub fn apply_priority_changes(
    files: &[TorrentFileFact],
    levels: &[u8],
    changes: &[(u32, u8)],
) -> Result<Vec<u8>, FileChangeError> {
    let mut next = levels.to_vec();
    next.resize(files.len(), PRIORITY_SKIP);
    for (index, level) in changes.iter().copied() {
        if level > PRIORITY_MAX {
            return Err(FileChangeError::InvalidPriority(level));
        }
        let slot = usize::try_from(index)
            .ok()
            .filter(|slot| files.get(*slot).is_some_and(|file| !file.padding))
            .and_then(|slot| next.get_mut(slot))
            .ok_or(FileChangeError::UnknownFile(index))?;
        *slot = level;
    }
    let any_selected = files
        .iter()
        .zip(&next)
        .any(|(file, level)| !file.padding && *level > PRIORITY_SKIP);
    if !any_selected {
        return Err(FileChangeError::NothingSelected);
    }
    Ok(next)
}

/// 级别表 → `Db::save_bt_selected_files` 的参数：选中文件的下标与「是否全选」。
/// 全选时下标列表为空（库里的「全部」哨兵）。
#[must_use]
pub fn selection_from_levels(files: &[TorrentFileFact], levels: &[u8]) -> (Vec<i32>, bool) {
    let selected = |index: usize| {
        levels
            .get(index)
            .is_some_and(|level| *level > PRIORITY_SKIP)
    };
    let all = files
        .iter()
        .enumerate()
        .all(|(index, file)| file.padding || selected(index));
    if all {
        return (Vec::new(), true);
    }
    let indices = files
        .iter()
        .enumerate()
        .filter(|(index, file)| !file.padding && selected(*index))
        .filter_map(|(index, _)| i32::try_from(index).ok())
        .collect();
    (indices, false)
}

/// 解析落库的重命名表；不是合法 JSON 对象或键不是下标时视为空表。
#[must_use]
pub fn parse_rename_table(text: &str) -> BTreeMap<u32, String> {
    if text.trim().is_empty() {
        return BTreeMap::new();
    }
    serde_json::from_str::<BTreeMap<u32, String>>(text).unwrap_or_default()
}

/// 重命名表 → 落库文本（空表为空串）。
#[must_use]
pub fn format_rename_table(table: &BTreeMap<u32, String>) -> String {
    if table.is_empty() {
        return String::new();
    }
    // `BTreeMap<u32, String>` 的序列化不会失败（键是整数、值是字符串）。
    serde_json::to_string(table).unwrap_or_default()
}

/// 校验并规范化用户给的新相对路径（`\` 视作分隔符），返回落库 / 交给会话的 `/` 分隔路径。
///
/// 拒绝：不存在或 padding 的文件；空路径、绝对路径、空 / `.` / `..` 组件、超长组件；
/// `windows_rules` 下额外拒绝 Windows 不能落盘的名字（保留名、`:`、控制字符、以空格或
/// `.` 结尾的组件——Windows 会悄悄去掉它们而与别的文件撞名）；与其他文件同名，或一方是
/// 另一方的上级目录。同名比较不区分大小写：大小写不敏感的文件系统（Windows / macOS 默认）
/// 上那就是同一个文件，在区分大小写的系统上多拒一个名字无害。
pub fn normalize_rename(
    files: &[TorrentFileFact],
    index: u32,
    requested: &str,
    windows_rules: bool,
) -> Result<String, FileChangeError> {
    let slot = usize::try_from(index)
        .ok()
        .filter(|slot| files.get(*slot).is_some_and(|file| !file.padding))
        .ok_or(FileChangeError::UnknownFile(index))?;

    let normalized = requested.replace('\\', "/");
    if normalized.is_empty() || normalized.contains('\0') {
        return Err(FileChangeError::InvalidPath);
    }
    for component in normalized.split('/') {
        let bad_tail = windows_rules && (component.ends_with(' ') || component.ends_with('.'));
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > MAX_COMPONENT_BYTES
            || bad_tail
        {
            return Err(FileChangeError::InvalidPath);
        }
    }
    if !crate::bt_downloader::torrent_relative_path_is_safe(Path::new(&normalized), windows_rules) {
        return Err(FileChangeError::InvalidPath);
    }

    let wanted = normalized.to_lowercase();
    let wanted_dir = format!("{wanted}/");
    for (other_slot, other) in files.iter().enumerate() {
        if other_slot == slot || other.padding {
            continue;
        }
        let taken = other.path.to_lowercase();
        if taken == wanted
            || taken.starts_with(&wanted_dir)
            || wanted.starts_with(&format!("{taken}/"))
        {
            return Err(FileChangeError::PathConflict);
        }
    }
    Ok(normalized)
}

/// 把重命名表套到种子自带的文件事实上，得到每个文件的当前路径（下标对齐 `files`）。
/// 表里指向不存在或 padding 文件的条目（旧库 / 手改库的脏值）忽略。
#[must_use]
pub fn with_renames(
    files: &[TorrentFileFact],
    table: &BTreeMap<u32, String>,
) -> Vec<TorrentFileFact> {
    files
        .iter()
        .enumerate()
        .map(|(slot, file)| {
            let renamed = u32::try_from(slot)
                .ok()
                .and_then(|index| table.get(&index))
                .filter(|_| !file.padding);
            match renamed {
                Some(path) => TorrentFileFact {
                    path: path.clone(),
                    ..file.clone()
                },
                None => file.clone(),
            }
        })
        .collect()
}

/// 在任务的 staging 目录里把一个文件从 `from` 挪到 `to`（都是 `/` 分隔的相对路径，须已经过
/// [`normalize_rename`]）。任务不在 BT 会话里时用：会话里的任务由 librqbit 自己改名。
///
/// 文件还没落盘（从未下载到它）时什么都不做，返回 `Ok(false)`。会创建 `to` 需要的上级
/// 目录，并清掉 `from` 留下的空目录。`to` 处已有文件时返回 `AlreadyExists`，原文件不动；
/// 只改大小写的改名在不区分大小写的文件系统上照常生效。阻塞调用。
pub fn rename_staged_file(stage: &Path, from: &str, to: &str) -> std::io::Result<bool> {
    let src = stage.join(from);
    let dst = stage.join(to);
    match std::fs::symlink_metadata(&src) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 先把源挪到同目录的临时名：此后 `to` 若还存在，就一定是另一个文件，而不是源自己
    // 换了大小写的名字（不区分大小写的文件系统上两者是同一个目录项）。
    let parked = src.with_file_name(format!(".fxrename-{}", uuid::Uuid::new_v4()));
    std::fs::rename(&src, &parked)?;
    let moved = match std::fs::symlink_metadata(&dst) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a file already exists at the new path",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(&parked, &dst)
        }
        Err(error) => Err(error),
    };
    if let Err(error) = moved {
        // 放回原处；放不回去时把临时名报出来，文件内容仍在。
        return Err(match std::fs::rename(&parked, &src) {
            Ok(()) => error,
            Err(restore) => std::io::Error::new(
                error.kind(),
                format!(
                    "{error}; the file was left at '{}' ({restore})",
                    parked.display()
                ),
            ),
        });
    }
    // 清掉旧路径留下的空目录，到 staging 根为止；非空目录删不掉，正好停下。
    let mut dir = src.parent();
    while let Some(current) = dir {
        if current == stage || std::fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
    Ok(true)
}

/// 组装「文件」页的行：跳过 padding 文件；`downloaded` 不超过文件大小，缺进度记 0。
#[must_use]
pub fn build_rows(files: &[TorrentFileFact], levels: &[u8], progress: &[u64]) -> Vec<BtTaskFile> {
    files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.padding)
        .filter_map(|(slot, file)| {
            Some(BtTaskFile {
                index: u32::try_from(slot).ok()?,
                path: file.path.clone(),
                size: file.size,
                downloaded: progress.get(slot).copied().unwrap_or(0).min(file.size),
                priority: levels.get(slot).copied().unwrap_or(PRIORITY_SKIP),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> TorrentFileFact {
        TorrentFileFact {
            path: path.to_owned(),
            size,
            offset: 0,
            padding: false,
        }
    }

    fn pad(size: u64) -> TorrentFileFact {
        TorrentFileFact {
            path: format!(".pad/{size}"),
            size,
            offset: 0,
            padding: true,
        }
    }

    #[test]
    fn bitfield_progress_splits_shared_pieces_between_files() {
        // 片长 100：a = [0,250)，b = [250,260)，空文件 c，d = [260,400)。共 4 片。
        let at = |path: &str, offset: u64, size: u64| TorrentFileFact {
            offset,
            ..file(path, size)
        };
        let files = [
            at("a", 0, 250),
            at("b", 250, 10),
            at("c", 260, 0),
            at("d", 260, 140),
        ];
        // 有第 0、2 片（msb0：1010_0000）。第 2 片 [200,300) 被 a、b、d 三个文件分掉。
        assert_eq!(
            progress_from_bitfield(&files, 100, &[0b1010_0000]),
            vec![150, 10, 0, 40]
        );
        // 全有：每个文件正好是它的大小（末片不满一片）。
        assert_eq!(
            progress_from_bitfield(&files, 100, &[0b1111_0000]),
            vec![250, 10, 0, 140]
        );
        // 位图比种子短（或为空）：缺的片按没有算；片长为 0 的坏元数据不除零。
        assert_eq!(progress_from_bitfield(&files, 100, &[]), vec![0, 0, 0, 0]);
        assert_eq!(progress_from_bitfield(&files, 0, &[0xff]), vec![0, 0, 0, 0]);
    }

    #[test]
    fn bitfield_progress_reads_pieces_past_the_first_byte() {
        // 片长 10，一个 100 字节的文件 = 10 片；只有第 9 片（第二个字节的第二位）。
        let files = [file("a", 100)];
        assert_eq!(
            progress_from_bitfield(&files, 10, &[0, 0b0100_0000]),
            vec![10]
        );
    }

    #[test]
    fn layout_is_read_from_torrent_bytes() {
        // 两个文件的多文件种子：`dir/a.bin`(5) 与 `b.bin`(7)，片长 16384。
        let torrent =
            b"d4:infod5:filesld6:lengthi5e4:pathl3:dir5:a.bineed6:lengthi7e4:pathl5:b.bineee\
                        4:name1:t12:piece lengthi16384e6:pieces20:AAAAAAAAAAAAAAAAAAAAee";
        let layout = layout_from_torrent_bytes(torrent).expect("valid torrent");
        assert_eq!(layout.piece_length, 16384);
        assert_eq!(layout.info_hash.len(), 40);
        assert_eq!(
            layout.files,
            vec![
                TorrentFileFact {
                    path: "dir/a.bin".to_owned(),
                    size: 5,
                    offset: 0,
                    padding: false,
                },
                TorrentFileFact {
                    path: "b.bin".to_owned(),
                    size: 7,
                    offset: 5,
                    padding: false,
                },
            ]
        );
        assert!(layout_from_torrent_bytes(b"not a torrent").is_none());
    }

    /// 视频 + padding + 字幕 + 说明：下标 1 是 padding。
    fn sample() -> Vec<TorrentFileFact> {
        vec![
            file("Show/E01.mkv", 1000),
            pad(24),
            file("Show/E01.srt", 10),
            file("Show/readme.txt", 5),
        ]
    }

    #[test]
    fn priority_table_roundtrips_only_levels_above_normal() {
        let levels = [1, 0, 6, 7, 2];
        let text = format_priority_table(&levels);
        assert_eq!(text, "2:6,3:7,4:2");
        assert_eq!(
            parse_priority_table(&text),
            BTreeMap::from([(2, 6), (3, 7), (4, 2)])
        );
        assert_eq!(format_priority_table(&[1, 1, 0]), "");
        assert!(parse_priority_table("").is_empty());
    }

    #[test]
    fn priority_table_drops_garbage_entries() {
        // 越界级别、非数字、缺分隔符、「不下载 / 普通」都不该入表。
        let table = parse_priority_table("0:6, 1:9,x:3,2,3:1,4:0, 5:7 ,6:-1,:,7:");
        assert_eq!(table, BTreeMap::from([(0, 6), (5, 7)]));
    }

    #[test]
    fn levels_follow_selection_then_table() {
        let files = sample();
        let table = BTreeMap::from([(0, 7), (3, 6)]);
        // 未确认 / 「全部」哨兵 = 全选；padding 恒为 0。
        assert_eq!(file_levels(&files, None, &table), vec![7, 0, 1, 6]);
        assert_eq!(file_levels(&files, Some(&[]), &table), vec![7, 0, 1, 6]);
        // 子集：没选中的文件即使表里有级别也是 0。
        assert_eq!(file_levels(&files, Some(&[0, 2]), &table), vec![7, 0, 1, 0]);
        // 选择里的脏下标（负数、越界、padding）不影响其余文件。
        assert_eq!(
            file_levels(&files, Some(&[-1, 99, 1, 2]), &BTreeMap::new()),
            vec![0, 0, 1, 0]
        );
    }

    #[test]
    fn priority_changes_apply_as_a_batch() {
        let files = sample();
        let levels = [1, 0, 1, 1];
        let next = apply_priority_changes(&files, &levels, &[(0, 7), (2, 0), (0, 6)])
            .expect("valid changes");
        assert_eq!(next, vec![6, 0, 0, 1], "the last change of a file wins");
    }

    #[test]
    fn invalid_priority_changes_are_rejected_whole() {
        let files = sample();
        let levels = [1, 0, 1, 1];
        assert_eq!(
            apply_priority_changes(&files, &levels, &[(0, 7), (9, 1)]),
            Err(FileChangeError::UnknownFile(9))
        );
        assert_eq!(
            apply_priority_changes(&files, &levels, &[(1, 1)]),
            Err(FileChangeError::UnknownFile(1)),
            "padding files cannot be changed"
        );
        assert_eq!(
            apply_priority_changes(&files, &levels, &[(0, 8)]),
            Err(FileChangeError::InvalidPriority(8))
        );
        assert_eq!(
            apply_priority_changes(&files, &levels, &[(0, 0), (2, 0), (3, 0)]),
            Err(FileChangeError::NothingSelected)
        );
        // 只剩一个文件时把它换成另一个仍然合法。
        assert_eq!(
            apply_priority_changes(&files, &[1, 0, 0, 0], &[(0, 0), (3, 1)]),
            Ok(vec![0, 0, 0, 1])
        );
    }

    #[test]
    fn a_short_level_list_counts_missing_files_as_skipped() {
        let files = sample();
        assert_eq!(
            apply_priority_changes(&files, &[1], &[(3, 6)]),
            Ok(vec![1, 0, 0, 6])
        );
    }

    #[test]
    fn selection_uses_the_all_sentinel_when_every_real_file_is_wanted() {
        let files = sample();
        assert_eq!(
            selection_from_levels(&files, &[1, 0, 6, 7]),
            (Vec::new(), true)
        );
        assert_eq!(
            selection_from_levels(&files, &[1, 0, 0, 7]),
            (vec![0, 3], false)
        );
        // padding 即使带了级别也不进选择。
        assert_eq!(
            selection_from_levels(&files, &[0, 5, 1, 0]),
            (vec![2], false)
        );
    }

    #[test]
    fn selection_and_table_roundtrip_through_storage() {
        let files = sample();
        let levels = vec![6, 0, 0, 1];
        let (indices, all) = selection_from_levels(&files, &levels);
        assert!(!all);
        let table = parse_priority_table(&format_priority_table(&levels));
        assert_eq!(file_levels(&files, Some(&indices), &table), levels);
    }

    #[test]
    fn rename_table_roundtrips_awkward_names() {
        let table = BTreeMap::from([
            (0, "字幕/第 1 集 \"final\".srt".to_owned()),
            (12, "a,b:c/{d}.txt".to_owned()),
        ]);
        let text = format_rename_table(&table);
        assert_eq!(parse_rename_table(&text), table);
        assert_eq!(format_rename_table(&BTreeMap::new()), "");
        assert!(parse_rename_table("").is_empty());
        assert!(parse_rename_table("not json").is_empty());
        assert!(parse_rename_table("[1,2]").is_empty());
        assert!(parse_rename_table(r#"{"x":"a"}"#).is_empty());
    }

    #[test]
    fn rename_normalizes_separators_and_allows_new_folders() {
        let files = sample();
        assert_eq!(
            normalize_rename(&files, 2, r"Subs\第 1 集.srt", true),
            Ok("Subs/第 1 集.srt".to_owned())
        );
        // 改回自己的名字（含只改大小写）不算冲突。
        assert_eq!(
            normalize_rename(&files, 0, "show/e01.MKV", true),
            Ok("show/e01.MKV".to_owned())
        );
    }

    #[test]
    fn rename_rejects_paths_that_escape_or_cannot_exist() {
        let files = sample();
        for bad in [
            "",
            "/etc/passwd",
            "../outside.txt",
            "a/../../b",
            "a//b",
            "a/./b",
            "a/",
            "nul\0byte",
        ] {
            assert_eq!(
                normalize_rename(&files, 0, bad, false),
                Err(FileChangeError::InvalidPath),
                "{bad:?}"
            );
        }
        let long = "x".repeat(256);
        assert_eq!(
            normalize_rename(&files, 0, &long, false),
            Err(FileChangeError::InvalidPath)
        );
        assert!(normalize_rename(&files, 0, &"x".repeat(255), false).is_ok());
        // 多字节名字按字节计长。
        assert_eq!(
            normalize_rename(&files, 0, &"字".repeat(86), false),
            Err(FileChangeError::InvalidPath)
        );
    }

    #[test]
    fn rename_applies_windows_rules_only_when_asked() {
        let files = sample();
        for name in [
            "C:evil.txt",
            "CON",
            "com1.txt",
            "trailing.",
            "trailing ",
            "a\tb",
        ] {
            assert_eq!(
                normalize_rename(&files, 0, name, true),
                Err(FileChangeError::InvalidPath),
                "{name:?}"
            );
        }
        for name in ["CON", "trailing.", "a:b"] {
            assert!(normalize_rename(&files, 0, name, false).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn rename_rejects_collisions_with_other_files() {
        let files = sample();
        // 同名（不区分大小写）。
        assert_eq!(
            normalize_rename(&files, 0, "show/E01.SRT", false),
            Err(FileChangeError::PathConflict)
        );
        // 新路径把别的文件当目录，或自己成了别的文件的上级目录。
        assert_eq!(
            normalize_rename(&files, 0, "Show/readme.txt/E01.mkv", false),
            Err(FileChangeError::PathConflict)
        );
        assert_eq!(
            normalize_rename(&files, 0, "Show", false),
            Err(FileChangeError::PathConflict)
        );
        // 只是前缀相同的兄弟文件不冲突。
        assert!(normalize_rename(&files, 0, "Show/E01.srt.bak", false).is_ok());
        assert!(normalize_rename(&files, 0, "Sho", false).is_ok());
    }

    #[test]
    fn rename_rejects_missing_and_padding_files() {
        let files = sample();
        assert_eq!(
            normalize_rename(&files, 1, "a.bin", false),
            Err(FileChangeError::UnknownFile(1))
        );
        assert_eq!(
            normalize_rename(&files, 4, "a.bin", false),
            Err(FileChangeError::UnknownFile(4))
        );
    }

    #[test]
    fn rows_skip_padding_and_clamp_progress() {
        let files = sample();
        let rows = build_rows(&files, &[7, 0, 0, 1], &[2000, 24, 4]);
        assert_eq!(
            rows,
            vec![
                BtTaskFile {
                    index: 0,
                    path: "Show/E01.mkv".to_owned(),
                    size: 1000,
                    downloaded: 1000,
                    priority: 7,
                },
                BtTaskFile {
                    index: 2,
                    path: "Show/E01.srt".to_owned(),
                    size: 10,
                    downloaded: 4,
                    priority: 0,
                },
                BtTaskFile {
                    index: 3,
                    path: "Show/readme.txt".to_owned(),
                    size: 5,
                    downloaded: 0,
                    priority: 1,
                },
            ]
        );
    }

    #[test]
    fn renames_overlay_current_paths_and_skip_dirty_entries() {
        let files = sample();
        let table = BTreeMap::from([
            (0u32, "Films/Pilot.mkv".to_owned()),
            // padding 文件与越界下标的条目是脏值。
            (1, "pad-renamed".to_owned()),
            (9, "ghost".to_owned()),
        ]);
        let current = with_renames(&files, &table);
        assert_eq!(current.len(), files.len());
        assert_eq!(current[0].path, "Films/Pilot.mkv");
        assert_eq!(current[0].size, files[0].size);
        assert_eq!(current[1], files[1]);
        assert_eq!(current[2], files[2]);
        // 改名后的路径参与后续的同名校验。
        assert_eq!(
            normalize_rename(&current, 2, "films/pilot.mkv", false),
            Err(FileChangeError::PathConflict)
        );
        assert!(normalize_rename(&current, 2, "Show/E01.mkv", false).is_ok());
    }

    fn stage_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fluxdown-bt-rename-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("stage dir");
        dir
    }

    fn tree(root: &Path) -> Vec<String> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("read dir") {
                let path = entry.expect("entry").path();
                let rel = path
                    .strip_prefix(root)
                    .expect("inside root")
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.is_dir() {
                    out.push(format!("{rel}/"));
                    walk(root, &path, out);
                } else {
                    out.push(rel);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    #[test]
    fn staged_rename_moves_the_file_and_prunes_empty_folders() {
        let stage = stage_dir("move");
        std::fs::create_dir_all(stage.join("Show/S01")).expect("dirs");
        std::fs::write(stage.join("Show/S01/E01.mkv"), b"video").expect("file");
        std::fs::write(stage.join("Show/readme.txt"), b"hi").expect("file");

        assert!(
            rename_staged_file(&stage, "Show/S01/E01.mkv", "字幕组/第 1 集.mkv").expect("rename")
        );
        assert_eq!(
            tree(&stage),
            ["Show/", "Show/readme.txt", "字幕组/", "字幕组/第 1 集.mkv"]
        );
        assert_eq!(
            std::fs::read(stage.join("字幕组/第 1 集.mkv")).expect("moved"),
            b"video"
        );
        // 最后一个文件离开后，旧目录整条清掉，但 staging 根留着。
        assert!(rename_staged_file(&stage, "Show/readme.txt", "readme.txt").expect("rename"));
        assert_eq!(
            tree(&stage),
            ["readme.txt", "字幕组/", "字幕组/第 1 集.mkv"]
        );
        std::fs::remove_dir_all(&stage).expect("cleanup");
    }

    #[test]
    fn staged_rename_of_a_file_that_is_not_on_disk_yet_is_a_no_op() {
        let stage = stage_dir("absent");
        assert!(!rename_staged_file(&stage, "a.bin", "sub/b.bin").expect("no-op"));
        assert!(
            tree(&stage).is_empty(),
            "no folders are created for nothing"
        );
        // staging 目录本身还不存在也一样。
        let missing = stage.join("never-created");
        assert!(!rename_staged_file(&missing, "a.bin", "b.bin").expect("no-op"));
        std::fs::remove_dir_all(&stage).expect("cleanup");
    }

    #[test]
    fn staged_rename_never_replaces_another_file() {
        let stage = stage_dir("taken");
        std::fs::write(stage.join("a.bin"), b"mine").expect("file");
        std::fs::create_dir_all(stage.join("sub")).expect("dir");
        std::fs::write(stage.join("sub/b.bin"), b"theirs").expect("file");

        let error = rename_staged_file(&stage, "a.bin", "sub/b.bin").expect_err("taken");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(tree(&stage), ["a.bin", "sub/", "sub/b.bin"]);
        assert_eq!(std::fs::read(stage.join("a.bin")).expect("kept"), b"mine");
        assert_eq!(
            std::fs::read(stage.join("sub/b.bin")).expect("kept"),
            b"theirs"
        );
        std::fs::remove_dir_all(&stage).expect("cleanup");
    }

    #[test]
    fn staged_rename_can_change_only_the_case() {
        let stage = stage_dir("case");
        std::fs::write(stage.join("readme.txt"), b"hi").expect("file");
        assert!(rename_staged_file(&stage, "readme.txt", "README.txt").expect("rename"));
        // 目录项的名字确实换成了新的大小写（在不区分大小写的文件系统上也是）。
        assert_eq!(tree(&stage), ["README.txt"]);
        std::fs::remove_dir_all(&stage).expect("cleanup");
    }
}
