//! 目录内文件替换：解压后的内容根 → 安装目录。
//!
//! 只替换更新包里**存在**的文件，其余（`portable_data/` 用户数据、用户自加文件）原样保留。
//! 每个已存在的目标先改名为 `<name>.fluxdown-old`（Windows 允许改名正在运行的 exe / DLL，
//! Unix 改名不影响已运行的进程），再把新文件复制进去（`fs::copy` 保留 Unix 执行位）。
//! 任一步失败则按日志逆序回滚：删掉新文件、把旧文件改回原名、移除新建的目录。
//! 成功后旧文件**不**立即删除（Windows 删不掉运行中的 exe；macOS / Linux 上进程仍可能按路径
//! 懒加载资源），由 [`super::cleanup_leftovers`] 在下次启动时清理。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{InstallError, archive};

/// 改名让位后的旧文件后缀。
pub(super) const ASIDE_SUFFIX: &str = ".fluxdown-old";
/// Windows 便携版的用户数据目录：更新永远不碰。
pub(super) const PROTECTED_DIR: &str = "portable_data";

/// 瞬时锁（杀毒软件扫描、句柄尚未释放）的重试次数与初始退避。
const RETRIES: u32 = 5;
const INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(1);

/// 要替换哪些文件。
#[derive(Clone, Copy, Debug)]
pub(super) enum Selection<'a> {
    /// 包内全部文件；`required` 是必须存在于包根的文件（防止装错包）。
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    All { required: &'a [&'a str] },
    /// 只替换包根下这些文件，且必须全部存在。
    Only(&'a [&'a str]),
}

/// 解压 `package` 到 `work_dir/extract`，再把选中的文件替换进 `dest_dir`。
pub(super) fn apply_archive(
    package: &Path,
    work_dir: &Path,
    dest_dir: &Path,
    selection: Selection<'_>,
) -> Result<(), InstallError> {
    let root = archive::extract(package, &work_dir.join("extract"))?;
    replace_files(&root, dest_dir, selection)
}

pub(super) fn replace_files(
    src_root: &Path,
    dest_dir: &Path,
    selection: Selection<'_>,
) -> Result<(), InstallError> {
    replace_files_with(src_root, dest_dir, selection, &|from, to| {
        fs::copy(from, to).map(drop)
    })
}

/// 回滚日志条目。
#[derive(Debug)]
enum Journal {
    /// 新建的文件（回滚时删除）。
    Created(PathBuf),
    /// 已让位的旧文件：`target` 现在装着新文件（或半个新文件），`aside` 是旧文件。
    Renamed { target: PathBuf, aside: PathBuf },
    /// 新建的目录（回滚时在为空时删除）。
    Dir(PathBuf),
}

fn replace_files_with(
    src_root: &Path,
    dest_dir: &Path,
    selection: Selection<'_>,
    copy: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), InstallError> {
    let mut files = Vec::new();
    collect_files(src_root, Path::new(""), &mut files)?;
    #[cfg(windows)]
    {
        // Never remove the startup-gate marker, even between rename and copy.
        let marker = Path::new("portable");
        let is_marker = |file: &PathBuf| file.as_os_str().eq_ignore_ascii_case(marker.as_os_str());
        if files.iter().any(is_marker)
            && dest_dir
                .join(marker)
                .try_exists()
                .map_err(InstallError::storage("inspect portable marker"))?
        {
            files.retain(|file| !is_marker(file));
        }
    }
    files.sort();
    let files = select(files, selection)?;
    let mut journal = Vec::new();
    for relative in &files {
        if let Err((operation, error)) =
            replace_one(src_root, dest_dir, relative, copy, &mut journal)
        {
            let failures = rollback(journal);
            let mut context = format!("{operation} {}", relative.display());
            let rollback_incomplete = !failures.is_empty();
            if rollback_incomplete {
                context.push_str(&format!("; rollback incomplete: {}", failures.join("; ")));
            }
            return Err(InstallError::Replace {
                context,
                source: error,
                #[cfg(windows)]
                rollback_incomplete,
            });
        }
    }
    Ok(())
}

fn select(files: Vec<PathBuf>, selection: Selection<'_>) -> Result<Vec<PathBuf>, InstallError> {
    let has_root_file = |name: &str| files.iter().any(|file| file == Path::new(name));
    match selection {
        Selection::All { required } => {
            if let Some(missing) = required.iter().find(|name| !has_root_file(name)) {
                return Err(InstallError::InvalidPackage(format!(
                    "package does not contain {missing}"
                )));
            }
            Ok(files)
        }
        Selection::Only(names) => {
            if let Some(missing) = names.iter().find(|name| !has_root_file(name)) {
                return Err(InstallError::InvalidPackage(format!(
                    "package does not contain {missing}"
                )));
            }
            Ok(files
                .into_iter()
                .filter(|file| names.iter().any(|name| file == Path::new(name)))
                .collect())
        }
    }
}

/// 递归收集 `root` 下的普通文件（相对路径）；跳过受保护目录，遇链接报错。
fn collect_files(root: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> Result<(), InstallError> {
    let dir = root.join(relative);
    let context = format!("read {}", dir.display());
    for entry in fs::read_dir(&dir).map_err(InstallError::storage(context.clone()))? {
        let entry = entry.map_err(InstallError::storage(context.clone()))?;
        let name = entry.file_name();
        let child = relative.join(&name);
        let file_type = entry
            .file_type()
            .map_err(InstallError::storage(context.clone()))?;
        #[cfg(windows)]
        let protected = name.eq_ignore_ascii_case(PROTECTED_DIR);
        #[cfg(not(windows))]
        let protected = name == PROTECTED_DIR;
        if relative.as_os_str().is_empty() && protected {
            continue;
        }
        if file_type.is_dir() {
            collect_files(root, &child, out)?;
        } else if file_type.is_file() {
            out.push(child);
        } else {
            return Err(InstallError::InvalidPackage(format!(
                "package entry {} is not a regular file",
                child.display()
            )));
        }
    }
    Ok(())
}

fn replace_one(
    src_root: &Path,
    dest_dir: &Path,
    relative: &Path,
    copy: &dyn Fn(&Path, &Path) -> io::Result<()>,
    journal: &mut Vec<Journal>,
) -> Result<(), (&'static str, io::Error)> {
    let from = src_root.join(relative);
    let to = dest_dir.join(relative);
    if let Some(parent) = to.parent() {
        ensure_dir(parent, journal).map_err(|error| ("create parent for", error))?;
    }
    match fs::symlink_metadata(&to) {
        Ok(_) => {
            let aside = aside_path(&to);
            remove_stale(&aside).map_err(|error| ("remove previous backup for", error))?;
            retry(|| fs::rename(&to, &aside)).map_err(|error| ("move aside", error))?;
            journal.push(Journal::Renamed {
                target: to.clone(),
                aside,
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            journal.push(Journal::Created(to.clone()));
        }
        Err(error) => return Err(("inspect", error)),
    }
    retry(|| copy(&from, &to)).map_err(|error| ("copy", error))
}

fn aside_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(ASIDE_SUFFIX);
    PathBuf::from(name)
}

/// 清掉上次遗留的同名旧文件，否则改名可能因目标已存在而失败（目录不能被改名覆盖）。
fn remove_stale(aside: &Path) -> io::Result<()> {
    match fs::symlink_metadata(aside) {
        Ok(meta) if meta.is_dir() => retry(|| fs::remove_dir_all(aside)),
        Ok(_) => retry(|| fs::remove_file(aside)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// 创建缺失的目录并记入日志（自上而下）。
fn ensure_dir(dir: &Path, journal: &mut Vec<Journal>) -> io::Result<()> {
    let mut missing = Vec::new();
    for ancestor in dir.ancestors() {
        if ancestor.as_os_str().is_empty() || ancestor.exists() {
            break;
        }
        missing.push(ancestor.to_path_buf());
    }
    for path in missing.into_iter().rev() {
        fs::create_dir(&path)?;
        journal.push(Journal::Dir(path));
    }
    Ok(())
}

/// 逆序回滚，返回没能还原的步骤描述（空 = 完全还原）。
fn rollback(journal: Vec<Journal>) -> Vec<String> {
    let mut failures = Vec::new();
    for entry in journal.into_iter().rev() {
        match entry {
            Journal::Created(path) => {
                if let Err(error) = remove_path(&path) {
                    failures.push(format!("remove {}: {error}", path.display()));
                }
            }
            Journal::Renamed { target, aside } => {
                if let Err(error) = remove_path(&target) {
                    failures.push(format!("remove {}: {error}", target.display()));
                    continue;
                }
                if let Err(error) = retry(|| fs::rename(&aside, &target)) {
                    failures.push(format!(
                        "restore {} from {}: {error}",
                        target.display(),
                        aside.display()
                    ));
                }
            }
            Journal::Dir(path) => {
                // 目录非空说明里面还有别的文件（用户的或还没回滚完的），保留即可。
                if let Err(error) = fs::remove_dir(&path) {
                    tracing::debug!(path = %path.display(), %error, "kept directory during update rollback");
                }
            }
        }
    }
    failures
}

/// 删除文件或目录；不存在视为成功。
fn remove_path(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 瞬时错误：Windows 上杀毒软件 / 搜索索引短暂占用文件（拒绝访问、共享冲突、锁冲突、映射节被占用）。
fn is_transient(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::ResourceBusy | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    cfg!(windows)
        && (error.kind() == io::ErrorKind::PermissionDenied
            || matches!(error.raw_os_error(), Some(5 | 32 | 33 | 1224)))
}

fn retry<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut backoff = INITIAL_BACKOFF;
    for _ in 0..RETRIES {
        match operation() {
            Err(error) if is_transient(&error) => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            other => return other,
        }
    }
    operation()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::update::install::test_support::TempDir;

    fn write(path: &Path, content: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)
    }

    fn read(path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn leftovers(dir: &Path) -> io::Result<Vec<String>> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(ASIDE_SUFFIX) {
                found.push(name);
            }
        }
        found.sort();
        Ok(found)
    }

    #[test]
    fn replaces_present_files_and_keeps_everything_else() -> io::Result<()> {
        let dir = TempDir::new("replace-ok")?;
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        write(&src.join("fluxdown-agent"), "agent-new")?;
        write(&src.join("lib").join("engine.dll"), "engine-new")?;
        write(&src.join("brand-new.txt"), "brand-new")?;
        // 包里即使带了 portable_data 也绝不能碰用户数据。
        let packaged_data = if cfg!(windows) {
            "PORTABLE_DATA"
        } else {
            PROTECTED_DIR
        };
        write(&src.join(packaged_data).join("state.db"), "from-package")?;
        #[cfg(windows)]
        write(&src.join("PORTABLE"), "new marker")?;
        write(&dest.join("fluxdown-agent"), "agent-old")?;
        write(&dest.join("lib").join("engine.dll"), "engine-old")?;
        write(&dest.join("user-notes.txt"), "mine")?;
        write(&dest.join(PROTECTED_DIR).join("state.db"), "user-data")?;
        #[cfg(windows)]
        write(&dest.join("portable"), "existing marker")?;

        replace_files(
            &src,
            &dest,
            Selection::All {
                required: &["fluxdown-agent"],
            },
        )
        .map_err(io::Error::other)?;

        assert_eq!(read(&dest.join("fluxdown-agent"))?, "agent-new");
        assert_eq!(read(&dest.join("lib").join("engine.dll"))?, "engine-new");
        assert_eq!(read(&dest.join("brand-new.txt"))?, "brand-new");
        assert_eq!(read(&dest.join("user-notes.txt"))?, "mine");
        #[cfg(windows)]
        {
            assert_eq!(read(&dest.join("portable"))?, "existing marker");
            let entries = fs::read_dir(&dest)?.collect::<io::Result<Vec<_>>>()?;
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry.file_name().eq_ignore_ascii_case("portable"))
                    .count(),
                1
            );
        }
        assert_eq!(
            read(&dest.join(PROTECTED_DIR).join("state.db"))?,
            "user-data"
        );
        assert_eq!(leftovers(&dest)?, vec!["fluxdown-agent.fluxdown-old"]);
        assert_eq!(
            read(&dest.join("fluxdown-agent.fluxdown-old"))?,
            "agent-old"
        );
        assert_eq!(
            read(&dest.join("lib").join("engine.dll.fluxdown-old"))?,
            "engine-old"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copied_files_keep_their_exec_bits() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("replace-exec")?;
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        write(&src.join("fluxdown-agent"), "new")?;
        fs::set_permissions(
            src.join("fluxdown-agent"),
            fs::Permissions::from_mode(0o755),
        )?;
        write(&dest.join("fluxdown-agent"), "old")?;
        replace_files(&src, &dest, Selection::Only(&["fluxdown-agent"]))
            .map_err(io::Error::other)?;
        let mode = fs::metadata(dest.join("fluxdown-agent"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        Ok(())
    }

    #[test]
    fn only_selection_replaces_just_the_named_root_files() -> io::Result<()> {
        let dir = TempDir::new("replace-only")?;
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        write(&src.join("fluxdown-agent"), "agent-new")?;
        write(&src.join("fluxdownd"), "daemon-new")?;
        write(&src.join("README.txt"), "readme-new")?;
        write(&dest.join("fluxdown-agent"), "agent-old")?;
        write(&dest.join("fluxdownd"), "daemon-old")?;
        write(&dest.join("README.txt"), "readme-old")?;

        replace_files(
            &src,
            &dest,
            Selection::Only(&["fluxdown-agent", "fluxdownd"]),
        )
        .map_err(io::Error::other)?;

        assert_eq!(read(&dest.join("fluxdown-agent"))?, "agent-new");
        assert_eq!(read(&dest.join("fluxdownd"))?, "daemon-new");
        assert_eq!(read(&dest.join("README.txt"))?, "readme-old");
        assert_eq!(
            leftovers(&dest)?,
            vec!["fluxdown-agent.fluxdown-old", "fluxdownd.fluxdown-old"]
        );
        Ok(())
    }

    #[test]
    fn missing_required_file_rejects_the_package_before_touching_anything() -> io::Result<()> {
        let dir = TempDir::new("replace-missing")?;
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        write(&src.join("fluxdown-agent"), "agent-new")?;
        write(&dest.join("fluxdown-agent"), "agent-old")?;

        let only = replace_files(
            &src,
            &dest,
            Selection::Only(&["fluxdown-agent", "fluxdownd"]),
        );
        assert!(matches!(only, Err(InstallError::InvalidPackage(_))));
        let all = replace_files(
            &src,
            &dest,
            Selection::All {
                required: &["other"],
            },
        );
        assert!(matches!(all, Err(InstallError::InvalidPackage(_))));
        assert_eq!(read(&dest.join("fluxdown-agent"))?, "agent-old");
        assert!(leftovers(&dest)?.is_empty());
        Ok(())
    }

    #[test]
    fn injected_failure_rolls_back_every_replaced_file_and_created_directory() -> io::Result<()> {
        let dir = TempDir::new("replace-rollback")?;
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        write(&src.join("a.bin"), "a-new")?;
        write(&src.join("b.bin"), "b-new")?;
        write(&src.join("c.bin"), "c-new")?;
        write(&src.join("newdir").join("z.bin"), "z-new")?;
        write(&dest.join("a.bin"), "a-old")?;
        write(&dest.join("b.bin"), "b-old")?;
        write(&dest.join("c.bin"), "c-old")?;
        write(&dest.join(PROTECTED_DIR).join("state.db"), "user-data")?;

        // 复制第 3 个文件时失败（非瞬时错误，不重试）；失败的那个已被写出半截。
        let calls = Cell::new(0_u32);
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                fs::write(to, "half-written")?;
                return Err(io::Error::other("injected copy failure"));
            }
            fs::copy(from, to).map(drop)
        };
        let error = replace_files_with(&src, &dest, Selection::All { required: &[] }, &failing)
            .err()
            .ok_or_else(|| io::Error::other("replacement must fail"))?;
        assert!(matches!(error, InstallError::Replace { .. }), "{error:#}");
        assert!(format!("{error:#}").contains("injected copy failure"));

        assert_eq!(read(&dest.join("a.bin"))?, "a-old");
        assert_eq!(read(&dest.join("b.bin"))?, "b-old");
        assert_eq!(read(&dest.join("c.bin"))?, "c-old");
        assert_eq!(
            read(&dest.join(PROTECTED_DIR).join("state.db"))?,
            "user-data"
        );
        assert!(!dest.join("newdir").exists(), "created directory removed");
        assert!(leftovers(&dest)?.is_empty(), "no aside files remain");
        Ok(())
    }

    #[test]
    fn stale_aside_files_from_a_previous_update_do_not_block_replacement() -> io::Result<()> {
        let dir = TempDir::new("replace-stale")?;
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        write(&src.join("app.bin"), "new")?;
        write(&dest.join("app.bin"), "old")?;
        write(&dest.join("app.bin.fluxdown-old"), "ancient")?;
        replace_files(&src, &dest, Selection::All { required: &[] }).map_err(io::Error::other)?;
        assert_eq!(read(&dest.join("app.bin"))?, "new");
        assert_eq!(read(&dest.join("app.bin.fluxdown-old"))?, "old");
        Ok(())
    }
}
