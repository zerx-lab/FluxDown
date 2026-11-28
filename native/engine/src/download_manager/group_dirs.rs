//! 任务组的目录事实：组根目录的推导，以及删除成员（带文件）后清理组留下的空目录。
//!
//! 组名只用于显示，不建目录。组根 = 基础保存目录 + 全体成员相对路径的最长公共目录
//! （[`common_member_dir`]）：分享里只有一个文件夹时，组根就是这个文件夹，组内不再重复一层；
//! 成员相对路径去掉这段前缀（[`strip_member_dir`]）后落在组根下，磁盘布局与清单路径一致。
//!
//! 删除成功后从每个成员的保存目录向上逐级移除**空**目录，止于组根推导前的基础目录（基础目录
//! 本身是用户选的 / 分类结果，不删）：折叠出的公共目录（可能多级）都是引擎建的，会一并清掉；
//! 没有折叠时组根就是基础目录，原样保留。旧版组没有记录基础目录，回退为止于组根，组根只在
//! 目录名与组名一致（旧版按组名建的目录）时可删。目录里只剩系统生成的元数据文件
//! （`.DS_Store` 等）也视为空；有任何其他条目的目录原样保留。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use super::DownloadManager;

/// 系统 / 文件管理器自动生成、不算用户数据的文件名（大小写不敏感）。
const DISPOSABLE_ENTRIES: [&str; 3] = [".ds_store", "thumbs.db", "desktop.ini"];

/// 一个成员的清理范围：从 `dir` 向上到 `root`（`include_root` 决定组根本身是否可删）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GroupDirTarget {
    pub(super) task_id: String,
    pub(super) dir: PathBuf,
    pub(super) root: PathBuf,
    pub(super) include_root: bool,
}

/// 成员相对路径（`/` 分隔）的一个目录段；空段与 `.` 不算，`..` / 含 `\` 的段不参与折叠。
fn member_segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
}

fn collapsible(segment: &str) -> bool {
    segment != ".." && !segment.contains('\\')
}

/// 全体成员相对路径的最长公共目录段；没有成员、任一成员位于根或任一路径含不可折叠的段
/// （`..`、反斜杠）时为空。
pub(super) fn common_member_dir<'a, I>(paths: I) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut paths = paths.into_iter();
    let Some(first) = paths.next() else {
        return Vec::new();
    };
    if !member_segments(first).all(collapsible) {
        return Vec::new();
    }
    let mut common: Vec<&str> = member_segments(first).collect();
    for path in paths {
        if common.is_empty() || !member_segments(path).all(collapsible) {
            return Vec::new();
        }
        let shared = common
            .iter()
            .zip(member_segments(path))
            .take_while(|(left, right)| *left == right)
            .count();
        common.truncate(shared);
    }
    common
}

/// 去掉公共目录的前 `depth` 段后的成员相对路径（`/` 分隔，空 = 组根）。
pub(super) fn strip_member_dir(path: &str, depth: usize) -> String {
    member_segments(path)
        .skip(depth)
        .collect::<Vec<_>>()
        .join("/")
}

/// `base` 拼上公共目录段（空 = `base` 原样）。
pub(super) fn join_member_dir(base: &str, segments: &[&str]) -> String {
    if segments.is_empty() {
        return base.to_owned();
    }
    let mut path = PathBuf::from(base);
    path.extend(segments);
    path.to_string_lossy().into_owned()
}

/// 旧版组（未记录基础目录）的组根是不是组自己的文件夹：目录名与组名（或其清洗形式）一致。
fn legacy_group_owns_root(name: &str, root: &Path) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    root.file_name().is_some_and(|dir| {
        let dir = dir.to_string_lossy();
        dir == name || dir == crate::naming::sanitize_filename(name)
    })
}

/// 一个组的清理边界 `(止点, 止点本身是否可删)`：新组止于基础目录且不删它；旧组止于组根。
fn prune_bound(facts: &crate::db::GroupDirFacts) -> Option<(PathBuf, bool)> {
    if !facts.base_dir.trim().is_empty() {
        return Some((PathBuf::from(&facts.base_dir), false));
    }
    if facts.save_dir.trim().is_empty() {
        return None;
    }
    let root = PathBuf::from(&facts.save_dir);
    let owns_root = legacy_group_owns_root(&facts.name, &root);
    Some((root, owns_root))
}

impl DownloadManager {
    /// 删除前登记组成员的目录（任务行删除后就查不到组归属了）；读库失败只记日志，不阻止删除。
    pub(super) async fn group_dir_targets(&self, task_ids: &[String]) -> Vec<GroupDirTarget> {
        let tasks = match self.db.load_tasks_by_ids(task_ids).await {
            Ok(tasks) => tasks,
            Err(error) => {
                crate::logger::report_warning("download-manager", "load group members", &error);
                return Vec::new();
            }
        };
        if tasks.iter().all(|task| task.group_id.is_empty()) {
            return Vec::new();
        }
        let groups: HashMap<String, (PathBuf, bool)> = match self.db.load_group_dir_facts().await {
            Ok(groups) => groups
                .into_iter()
                .filter_map(|facts| Some((facts.group_id.clone(), prune_bound(&facts)?)))
                .collect(),
            Err(error) => {
                crate::logger::report_warning("download-manager", "load task groups", &error);
                return Vec::new();
            }
        };
        tasks
            .into_iter()
            .filter_map(|task| {
                let (root, include_root) = groups.get(&task.group_id)?;
                Some(GroupDirTarget {
                    task_id: task.task_id,
                    dir: PathBuf::from(task.save_dir),
                    root: root.clone(),
                    include_root: *include_root,
                })
            })
            .collect()
    }

    /// 已删除成员的空目录清理（阻塞 I/O 放到 blocking 线程）。
    pub(super) async fn prune_group_dirs(&self, targets: Vec<GroupDirTarget>, deleted: &[String]) {
        let deleted: HashSet<&str> = deleted.iter().map(String::as_str).collect();
        let targets: Vec<GroupDirTarget> = targets
            .into_iter()
            .filter(|target| deleted.contains(target.task_id.as_str()))
            .collect();
        if targets.is_empty() {
            return;
        }
        if let Err(error) = tokio::task::spawn_blocking(move || prune_targets(&targets)).await {
            crate::logger::report_warning("download-manager", "prune group directories", &error);
        }
    }
}

/// 同一组的成员共享上层目录：先去重，再从最深的目录开始清理。
fn prune_targets(targets: &[GroupDirTarget]) {
    let mut seen = HashSet::new();
    let mut unique: Vec<&GroupDirTarget> = targets
        .iter()
        .filter(|target| seen.insert((&target.dir, &target.root)))
        .collect();
    unique.sort_by_key(|target| std::cmp::Reverse(target.dir.components().count()));
    for target in unique {
        prune_empty_dirs(&target.dir, &target.root, target.include_root);
    }
}

/// 从 `dir` 向上逐级删除空目录，不越过 `root`（`include_root` 为假时 `root` 本身不删）；
/// 遇到非空目录即停。
pub(super) fn prune_empty_dirs(dir: &Path, root: &Path, include_root: bool) {
    let mut current = dir;
    while current.starts_with(root) {
        let is_root = current == root;
        if (is_root && !include_root) || !remove_if_empty(current) || is_root {
            return;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return,
        }
    }
}

/// 目录为空（或只剩可丢弃的系统文件）时删除它；目录已不存在视为成功。
fn remove_if_empty(dir: &Path) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(error) => {
            crate::logger::report_warning("download-manager", "read group directory", &error);
            return false;
        }
    };
    let mut disposable = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                crate::logger::report_warning("download-manager", "read group directory", &error);
                return false;
            }
        };
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !DISPOSABLE_ENTRIES.contains(&name.as_str()) {
            return false;
        }
        disposable.push(entry.path());
    }
    for path in disposable {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("download-manager", "remove folder metadata", &error);
            return false;
        }
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            crate::logger::report_warning(
                "download-manager",
                "remove empty group directory",
                &error,
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::path::Path;

    use super::{
        common_member_dir, join_member_dir, legacy_group_owns_root, prune_bound, prune_empty_dirs,
        strip_member_dir,
    };
    use crate::db::GroupDirFacts;

    fn fixture(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fluxdown-group-dirs-{name}-{}", std::process::id()));
        if let Err(error) = fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("reset fixture: {error}");
        }
        fs::create_dir_all(&dir).expect("create fixture");
        dir
    }

    #[test]
    fn removes_empty_tree_up_to_owned_group_root() {
        let base = fixture("owned");
        let root = base.join("Pack");
        let sub = root.join("extras").join("b");
        fs::create_dir_all(&sub).expect("create subdirs");
        fs::write(root.join("extras").join(".DS_Store"), b"x").expect("write metadata");

        prune_empty_dirs(&sub, &root, true);

        assert!(!root.exists(), "empty group folder is removed");
        assert!(base.exists(), "the user's base directory stays");
        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn keeps_user_directory_roots_and_directories_with_user_files() {
        let base = fixture("kept");
        let sub = base.join("extras");
        fs::create_dir_all(&sub).expect("create subdir");

        // 组根就是用户选的目录：只清到其下一级。
        prune_empty_dirs(&sub, &base, false);
        assert!(!sub.exists());
        assert!(base.exists(), "the user's directory stays even when empty");

        let root = base.join("Pack");
        fs::create_dir_all(root.join("a")).expect("create subdir");
        fs::write(root.join("keep.bin"), b"x").expect("write user file");
        prune_empty_dirs(&root.join("a"), &root, true);
        assert!(!root.join("a").exists());
        assert!(root.join("keep.bin").exists(), "non-empty group root stays");
        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn multi_level_collapsed_root_is_pruned_up_to_the_base() {
        let base = fixture("collapsed");
        let root = base.join("Season 1").join("Extras");
        fs::create_dir_all(root.join("a")).expect("create member dir");
        let facts = GroupDirFacts {
            group_id: "g".to_owned(),
            name: "Extras".to_owned(),
            save_dir: root.to_string_lossy().into_owned(),
            base_dir: base.to_string_lossy().into_owned(),
        };
        let (stop, include_stop) = prune_bound(&facts).expect("bound");

        prune_empty_dirs(&root.join("a"), &stop, include_stop);

        assert!(
            !base.join("Season 1").exists(),
            "every collapsed level is removed"
        );
        assert!(base.exists(), "the base directory stays");
        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn uncollapsed_root_named_like_the_group_is_kept() {
        let facts = GroupDirFacts {
            group_id: "g".to_owned(),
            name: "Inception".to_owned(),
            save_dir: "/m/Inception".to_owned(),
            base_dir: "/m/Inception".to_owned(),
        };
        assert_eq!(
            prune_bound(&facts),
            Some((Path::new("/m/Inception").to_path_buf(), false))
        );
    }

    #[test]
    fn legacy_group_root_is_owned_only_when_named_after_the_group() {
        assert!(legacy_group_owns_root(
            "下载工具箱",
            Path::new("/d/下载工具箱")
        ));
        assert!(legacy_group_owns_root("a:b", Path::new("/d/a_b")));
        assert!(!legacy_group_owns_root(
            "百度网盘分享",
            Path::new("/d/Downloads")
        ));
        assert!(!legacy_group_owns_root("", Path::new("/d/Downloads")));
        let legacy = GroupDirFacts {
            name: "Pack".to_owned(),
            save_dir: "/d/Pack".to_owned(),
            ..GroupDirFacts::default()
        };
        assert_eq!(
            prune_bound(&legacy),
            Some((Path::new("/d/Pack").to_path_buf(), true))
        );
        assert_eq!(prune_bound(&GroupDirFacts::default()), None);
    }

    #[test]
    fn common_member_dir_collapses_only_shared_safe_prefixes() {
        assert_eq!(
            common_member_dir(["下载工具箱", "下载工具箱/sub", "下载工具箱/"]),
            ["下载工具箱"]
        );
        assert_eq!(common_member_dir(["a/b/c", "a/b/d"]), ["a", "b"]);
        assert!(
            common_member_dir(["a", ""]).is_empty(),
            "a root member stops collapsing"
        );
        assert!(common_member_dir(["a", "b"]).is_empty());
        assert!(common_member_dir(["../x", "../x"]).is_empty());
        assert!(common_member_dir(["a/../x", "a/../x"]).is_empty());
        assert!(common_member_dir(["a/b", "a/../b"]).is_empty());
        assert!(common_member_dir::<[&str; 0]>([]).is_empty());
    }

    #[test]
    fn members_keep_their_disk_location_after_collapsing() {
        let common = common_member_dir(["show/s1", "show"]);
        assert_eq!(common, ["show"]);
        let root = join_member_dir("/d", &common);
        assert_eq!(Path::new(&root), Path::new("/d/show"));
        assert_eq!(strip_member_dir("show/s1", common.len()), "s1");
        assert_eq!(strip_member_dir("show", common.len()), "");
        assert_eq!(join_member_dir("/d", &[]), "/d");
    }
}
