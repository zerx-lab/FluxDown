//! 任务详情「文件」页（BT）的纯逻辑：优先级档位映射、批量改优先级的差量、
//! 「至少保留一个文件」守卫、多选（单击 / 切换 / 连选）与路径展示拆分。

use std::collections::{BTreeSet, HashMap};

use fluxdown_protocol::{
    BT_FILE_PRIORITY_HIGH, BT_FILE_PRIORITY_MAXIMUM, BT_FILE_PRIORITY_NORMAL,
    BT_FILE_PRIORITY_SKIP, BtFilePriorityDto, BtTaskFileDto,
};

/// 界面上的四档优先级。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PriorityLevel {
    Skip,
    Normal,
    High,
    Maximum,
}

impl PriorityLevel {
    pub(crate) const ALL: [Self; 4] = [Self::Skip, Self::Normal, Self::High, Self::Maximum];

    /// 线上原始值 → 档位：界面四档以外的值（`2..=5` 及越界值）按距离最近的档位显示。
    pub(crate) fn of(raw: u8) -> Self {
        match raw {
            BT_FILE_PRIORITY_SKIP => Self::Skip,
            // 2、3 离 1 更近；4、5 离 6 更近。
            1..=3 => Self::Normal,
            4..=6 => Self::High,
            _ => Self::Maximum,
        }
    }

    pub(crate) fn raw(self) -> u8 {
        match self {
            Self::Skip => BT_FILE_PRIORITY_SKIP,
            Self::Normal => BT_FILE_PRIORITY_NORMAL,
            Self::High => BT_FILE_PRIORITY_HIGH,
            Self::Maximum => BT_FILE_PRIORITY_MAXIMUM,
        }
    }

    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Skip => "btFilePrioritySkip",
            Self::Normal => "btFilePriorityNormal",
            Self::High => "btFilePriorityHigh",
            Self::Maximum => "btFilePriorityMaximum",
        }
    }
}

/// 一组目标文件的优先级概况。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetLevel {
    /// 没有目标文件（选区为空或下标都已不存在）。
    None,
    /// 全部同一档。
    Same(PriorityLevel),
    /// 档位不一。
    Mixed,
}

/// 目标文件的档位是否一致（按显示档位比较，`2` 与 `1` 同算「普通」）。
pub(crate) fn target_level(files: &[BtTaskFileDto], targets: &BTreeSet<u32>) -> TargetLevel {
    let mut found = None;
    for file in files.iter().filter(|file| targets.contains(&file.index)) {
        let level = PriorityLevel::of(file.priority);
        match found {
            None => found = Some(level),
            Some(previous) if previous != level => return TargetLevel::Mixed,
            Some(_) => {}
        }
    }
    found.map_or(TargetLevel::None, TargetLevel::Same)
}

/// 把目标文件设为 `level` 需要下发的条目：只含原始值确有变化的文件。
pub(crate) fn priority_changes(
    files: &[BtTaskFileDto],
    targets: &BTreeSet<u32>,
    level: PriorityLevel,
) -> Vec<BtFilePriorityDto> {
    let priority = level.raw();
    files
        .iter()
        .filter(|file| targets.contains(&file.index) && file.priority != priority)
        .map(|file| BtFilePriorityDto {
            index: file.index,
            priority,
        })
        .collect()
}

/// 按文件下标索引的变更表（一次构建，逐文件 O(1) 查找）。
fn change_map(changes: &[BtFilePriorityDto]) -> HashMap<u32, u8> {
    changes
        .iter()
        .map(|change| (change.index, change.priority))
        .collect()
}

/// 应用 `changes` 后是否所有文件都是「不下载」（种子至少要保留一个文件）。
pub(crate) fn leaves_nothing_to_download(
    files: &[BtTaskFileDto],
    changes: &[BtFilePriorityDto],
) -> bool {
    if files.is_empty() {
        return false;
    }
    let changes = change_map(changes);
    files.iter().all(|file| {
        changes.get(&file.index).copied().unwrap_or(file.priority) == BT_FILE_PRIORITY_SKIP
    })
}

/// 乐观更新：把 `changes` 写进本地行。
pub(crate) fn apply_changes(files: &mut [BtTaskFileDto], changes: &[BtFilePriorityDto]) {
    let changes = change_map(changes);
    for file in files.iter_mut() {
        if let Some(priority) = changes.get(&file.index) {
            file.priority = *priority;
        }
    }
}

/// 已下载比例 `0.0..=1.0`；大小为 0 的文件视为已完成。
pub(crate) fn progress_fraction(downloaded: u64, size: u64) -> f64 {
    if size == 0 {
        return 1.;
    }
    // u64 → f64 对超大值有精度损失，但比例只用于显示。
    downloaded.min(size) as f64 / size as f64
}

/// 百分比文本，向下取整到 0.1%（未完成不会显示成 100.0%）。
pub(crate) fn progress_percent_text(fraction: f64) -> String {
    let tenths = (fraction.clamp(0., 1.) * 1000.).floor();
    format!("{:.1}%", tenths / 10.)
}

/// 相对路径拆成 `(目录, 文件名)`；线上路径以 `/` 分隔，反斜杠属于文件名本身。
pub(crate) fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// 文件页多选：按文件 `index` 记录，轮询刷新后保留，下标消失时剪除。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileSelection {
    selected: BTreeSet<u32>,
    /// 连选锚点（最近一次单击 / 切换的文件）。
    anchor: Option<u32>,
}

impl FileSelection {
    pub(crate) fn selected(&self) -> &BTreeSet<u32> {
        &self.selected
    }

    pub(crate) fn contains(&self, index: u32) -> bool {
        self.selected.contains(&index)
    }

    pub(crate) fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    /// 单击一行。`order` 为当前显示顺序的文件下标；`shift` 从锚点连选，`toggle`（Cmd / Ctrl）
    /// 切换单行或把连选范围并入现有选区。
    pub(crate) fn click(&mut self, order: &[u32], index: u32, shift: bool, toggle: bool) {
        let anchor_pos = self
            .anchor
            .and_then(|anchor| order.iter().position(|item| *item == anchor));
        let index_pos = order.iter().position(|item| *item == index);
        if shift && let (Some(from), Some(to)) = (anchor_pos, index_pos) {
            let (start, end) = if from <= to { (from, to) } else { (to, from) };
            if !toggle {
                self.selected.clear();
            }
            if let Some(range) = order.get(start..=end) {
                self.selected.extend(range.iter().copied());
            }
            return;
        }
        if toggle {
            if !self.selected.remove(&index) {
                self.selected.insert(index);
            }
        } else {
            self.selected.clear();
            self.selected.insert(index);
        }
        self.anchor = Some(index);
    }

    /// 剪掉已不存在的下标（`present` 为当前全部文件下标）。
    pub(crate) fn prune(&mut self, present: &[u32]) {
        let present: BTreeSet<u32> = present.iter().copied().collect();
        self.selected.retain(|index| present.contains(index));
        if self.anchor.is_some_and(|anchor| !present.contains(&anchor)) {
            self.anchor = None;
        }
    }

    /// 右键菜单的作用对象：右键行在选区内 → 整个选区；否则只有这一行。
    pub(crate) fn targets_for(&self, index: u32) -> BTreeSet<u32> {
        if self.selected.contains(&index) {
            self.selected.clone()
        } else {
            BTreeSet::from([index])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(index: u32, priority: u8) -> BtTaskFileDto {
        BtTaskFileDto {
            index,
            path: format!("dir/{index}.bin"),
            size: 10,
            downloaded: 0,
            priority,
        }
    }

    fn set(items: &[u32]) -> BTreeSet<u32> {
        items.iter().copied().collect()
    }

    #[test]
    fn priority_level_maps_both_ways() {
        for level in PriorityLevel::ALL {
            assert_eq!(PriorityLevel::of(level.raw()), level);
        }
        assert_eq!(PriorityLevel::Skip.raw(), 0);
        assert_eq!(PriorityLevel::Normal.raw(), 1);
        assert_eq!(PriorityLevel::High.raw(), 6);
        assert_eq!(PriorityLevel::Maximum.raw(), 7);
    }

    #[test]
    fn unknown_priority_values_show_nearest_level() {
        assert_eq!(PriorityLevel::of(2), PriorityLevel::Normal);
        assert_eq!(PriorityLevel::of(3), PriorityLevel::Normal);
        assert_eq!(PriorityLevel::of(4), PriorityLevel::High);
        assert_eq!(PriorityLevel::of(5), PriorityLevel::High);
        assert_eq!(PriorityLevel::of(200), PriorityLevel::Maximum);
    }

    #[test]
    fn target_level_detects_mixed() {
        let files = [file(0, 1), file(1, 2), file(2, 7), file(3, 0)];
        assert_eq!(target_level(&files, &set(&[])), TargetLevel::None);
        assert_eq!(target_level(&files, &set(&[9])), TargetLevel::None);
        // 1 与 2 同属「普通」。
        assert_eq!(
            target_level(&files, &set(&[0, 1])),
            TargetLevel::Same(PriorityLevel::Normal)
        );
        assert_eq!(target_level(&files, &set(&[0, 2])), TargetLevel::Mixed);
    }

    #[test]
    fn priority_changes_only_lists_changed_targets() {
        let files = [file(0, 1), file(1, 6), file(2, 2), file(3, 1)];
        let changes = priority_changes(&files, &set(&[0, 1, 2]), PriorityLevel::Normal);
        assert_eq!(
            changes,
            [
                BtFilePriorityDto {
                    index: 1,
                    priority: 1
                },
                BtFilePriorityDto {
                    index: 2,
                    priority: 1
                },
            ]
        );
        assert!(priority_changes(&files, &set(&[0, 3]), PriorityLevel::Normal).is_empty());
    }

    #[test]
    fn keep_one_guard() {
        let files = [file(0, 1), file(1, 0)];
        let skip_all = priority_changes(&files, &set(&[0, 1]), PriorityLevel::Skip);
        assert!(leaves_nothing_to_download(&files, &skip_all));
        let skip_one = priority_changes(&files, &set(&[1]), PriorityLevel::Skip);
        assert!(!leaves_nothing_to_download(&files, &skip_one));
        assert!(!leaves_nothing_to_download(&[], &[]));
        let mut applied = files.clone();
        apply_changes(&mut applied, &skip_all);
        assert!(applied.iter().all(|file| file.priority == 0));
    }

    /// 5 万个文件全选改档：结果正确，且乱序变更同样生效（实现按下标建表，线性复杂度）。
    #[test]
    fn batch_edits_on_50k_files() {
        const COUNT: u32 = 50_000;
        let files: Vec<_> = (0..COUNT).map(|index| file(index, 1)).collect();
        let all: BTreeSet<u32> = (0..COUNT).collect();
        let skip_all = priority_changes(&files, &all, PriorityLevel::Skip);
        assert_eq!(skip_all.len(), files.len());
        assert!(leaves_nothing_to_download(&files, &skip_all));
        let high = priority_changes(&files, &all, PriorityLevel::High);
        let mut applied = files.clone();
        apply_changes(&mut applied, &high);
        assert!(applied.iter().all(|file| file.priority == 6));
        assert!(!leaves_nothing_to_download(&applied, &[]));
        // 乱序的变更同样生效（不依赖与文件同序）。
        let mut reversed = skip_all.clone();
        reversed.reverse();
        apply_changes(&mut applied, &reversed);
        assert!(applied.iter().all(|file| file.priority == 0));
    }

    #[test]
    fn selection_click_toggle_range_and_prune() {
        let order = [5, 3, 9, 1, 7];
        let mut selection = FileSelection::default();
        selection.click(&order, 3, false, false);
        assert_eq!(selection.selected(), &set(&[3]));
        // Shift 连选按显示顺序，而非下标大小。
        selection.click(&order, 1, true, false);
        assert_eq!(selection.selected(), &set(&[3, 9, 1]));
        // 反向连选替换选区，锚点不变。
        selection.click(&order, 5, true, false);
        assert_eq!(selection.selected(), &set(&[5, 3]));
        selection.click(&order, 7, false, true);
        assert_eq!(selection.selected(), &set(&[5, 3, 7]));
        selection.click(&order, 3, false, true);
        assert_eq!(selection.selected(), &set(&[5, 7]));
        // Cmd + Shift：从锚点（3）把范围并入。
        selection.click(&order, 9, true, true);
        assert_eq!(selection.selected(), &set(&[5, 3, 9, 7]));
        assert_eq!(selection.targets_for(9), set(&[5, 3, 9, 7]));
        assert_eq!(selection.targets_for(1), set(&[1]));
        selection.prune(&[5, 9, 1]);
        assert_eq!(selection.selected(), &set(&[5, 9]));
        // 锚点 3 已不存在：Shift 退化为单击。
        selection.click(&order, 1, true, false);
        assert_eq!(selection.selected(), &set(&[1]));
        selection.clear();
        assert!(selection.selected().is_empty());
        assert!(!selection.contains(1));
    }

    #[test]
    fn progress_fraction_handles_zero_and_huge_sizes() {
        assert_eq!(progress_fraction(0, 0), 1.);
        assert_eq!(progress_percent_text(progress_fraction(0, 0)), "100.0%");
        let size = 5 * 1024 * 1024 * 1024_u64;
        assert_eq!(
            progress_percent_text(progress_fraction(size / 2, size)),
            "50.0%"
        );
        assert_eq!(
            progress_percent_text(progress_fraction(size - 1, size)),
            "99.9%"
        );
        // 已下载超出大小（不应发生）按 100% 封顶。
        assert_eq!(progress_fraction(size + 1, size), 1.);
        assert_eq!(progress_percent_text(progress_fraction(0, size)), "0.0%");
    }

    #[test]
    fn split_path_cases() {
        assert_eq!(split_path("a/b/movie.mkv"), ("a/b", "movie.mkv"));
        assert_eq!(split_path("README"), ("", "README"));
        assert_eq!(split_path("src/archive.tar.gz"), ("src", "archive.tar.gz"));
        assert_eq!(
            split_path("C:\\Users\\x\\file.txt"),
            ("", "C:\\Users\\x\\file.txt")
        );
        assert_eq!(
            split_path("动画/第 1 集  🎬.mp4"),
            ("动画", "第 1 集  🎬.mp4")
        );
        assert_eq!(split_path("dir/"), ("dir", ""));
    }
}
