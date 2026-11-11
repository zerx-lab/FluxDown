//! BT 任务数据的搬移（`daemon.task.moveStorage`）：落盘动作与中断恢复日志。
//!
//! 一次搬移 = 把旧保存目录下属于任务的若干顶层条目挪到新保存目录下的同名位置。未完成任务
//! 的条目是它的 staging 目录；已完成任务的条目是 `file_name` 与登记的产物（扁平落盘的其余
//! 文件）。动手前先把意图记进 config 表（[`journal_key`]），新位置落库后删除；进程中途退出
//! 时日志留存，下次启动据此把搬移继续做完——此时目标处的同名文件只可能是上次留下的半成品，
//! 直接用源盖掉（见 [`move_entries`] 的 `resume`）。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::bt_downloader::{move_path, move_path_with_file_replace};
use crate::logger::log_warn;

/// 搬移日志在 config 表里的键前缀，后接任务 ID。
pub const JOURNAL_PREFIX: &str = "bt_move_";

/// 任务的搬移日志键。
#[must_use]
pub fn journal_key(task_id: &str) -> String {
    format!("{JOURNAL_PREFIX}{task_id}")
}

/// 一次搬移的意图：新位置落库之前一直留在 config 表里。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveJournal {
    /// 旧保存目录。
    pub from: String,
    /// 新保存目录。
    pub to: String,
    /// 要挪的顶层条目名（两个目录下同名）。
    pub names: Vec<String>,
    /// 搬移开始时任务在做种（或排队做种）：搬完后重新挂载做种。
    #[serde(default)]
    pub reseed: bool,
}

impl MoveJournal {
    /// 解析落库的日志；不是合法日志返回 `None`。
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// 落库文本。
    #[must_use]
    pub fn encode(&self) -> String {
        // 字段只有字符串、字符串列表与布尔值，序列化不会失败。
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// 搬移失败的结果。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MoveError {
    /// 目标目录里已有同名条目，什么都没动。
    #[error("'{0}' already exists in the destination folder")]
    Conflict(String),
    /// 搬移失败，已挪过去的条目都挪回了原处。
    #[error("{0}")]
    RolledBack(String),
    /// 搬移失败且没能全部挪回：数据分散在新旧两个目录，日志须保留以便下次继续。
    #[error("{0}")]
    Split(String),
}

/// 目标目录里第一个已存在的同名条目（没有返回 `None`）。阻塞调用。
#[must_use]
pub fn find_conflict(to: &Path, names: &[String]) -> Option<String> {
    names
        .iter()
        .find(|name| std::fs::symlink_metadata(to.join(name)).is_ok())
        .cloned()
}

/// 单个条目搬移失败：`ours_at_dst` 表示目标处可能留有本任务的文件（目录搬到一半），
/// 回滚时要把它们挪回来；为 `false` 时目标处的东西不属于本任务，绝不能碰。
struct EntryFailure {
    error: std::io::Error,
    ours_at_dst: bool,
}

fn move_entry(src: &Path, dst: &Path, resume: bool) -> Result<(), EntryFailure> {
    if resume {
        // 目标处的同名文件是上次中断留下的半成品，源仍是完整的那份。
        return move_path_with_file_replace(src, dst, true).map_err(|error| EntryFailure {
            error,
            ours_at_dst: true,
        });
    }
    if !src.is_dir() {
        // 以 create_new 原子占名：被别的写者抢先时失败，不覆盖对方；失败时不留半成品。
        return move_path(src, dst).map_err(|error| EntryFailure {
            error,
            ours_at_dst: false,
        });
    }
    // 目录先由我们自己建出来：已存在即冲突；建成之后里面的东西就都是本任务的。
    std::fs::create_dir(dst).map_err(|error| EntryFailure {
        error,
        ours_at_dst: false,
    })?;
    move_path(src, dst).map_err(|error| EntryFailure {
        error,
        ours_at_dst: true,
    })
}

/// 把 `names` 里存在于 `from` 下的条目逐个挪到 `to` 下，返回实际挪动的条目数。阻塞调用。
///
/// 同卷是重命名；跨卷逐文件复制后删除源。`resume = false`（用户刚发起）：调用方须已用
/// [`find_conflict`] 确认目标处没有同名条目，这里仍以不覆盖的方式落盘，中途失败会把已挪的
/// 条目挪回原处。`resume = true`（接着做上次中断的搬移）：已经不在 `from` 下的条目视为
/// 挪完，目标处的同名文件用源覆盖；失败不回滚（上一次已经挪走的那部分本就回不到一个
/// 一致的起点），由调用方保留日志下次再试。
///
/// # Errors
/// [`MoveError::RolledBack`]：失败且已恢复原状；[`MoveError::Split`]：数据分散在两处。
pub fn move_entries(
    from: &Path,
    to: &Path,
    names: &[String],
    resume: bool,
) -> Result<usize, MoveError> {
    let mut moved: Vec<&String> = Vec::new();
    for name in names {
        let src = from.join(name);
        if std::fs::symlink_metadata(&src).is_err() {
            continue;
        }
        let dst = to.join(name);
        match move_entry(&src, &dst, resume) {
            Ok(()) => moved.push(name),
            Err(failure) => {
                let reason = format!(
                    "failed to move '{}' to '{}': {}",
                    src.display(),
                    dst.display(),
                    failure.error
                );
                if resume {
                    return Err(MoveError::Split(reason));
                }
                if failure.ours_at_dst {
                    moved.push(name);
                }
                return Err(roll_back(from, to, &moved, reason));
            }
        }
    }
    Ok(moved.len())
}

/// 把已挪到 `to` 的条目挪回 `from`（后挪的先回）。全部回去返回 `RolledBack`，否则 `Split`。
fn roll_back(from: &Path, to: &Path, moved: &[&String], reason: String) -> MoveError {
    let mut stuck = Vec::new();
    for name in moved.iter().rev() {
        let at_new = to.join(name);
        if std::fs::symlink_metadata(&at_new).is_err() {
            continue;
        }
        // 目录可能只挪过去一部分：并回原目录，已回到原处的同名文件以挪过去的那份为准
        // （两份内容相同，或原处那份是复制后没删掉的残留）。
        if let Err(error) = move_path_with_file_replace(&at_new, &from.join(name), true) {
            log_warn!(
                "[BT] move rollback failed for '{}': {}",
                at_new.display(),
                error
            );
            stuck.push((*name).clone());
        }
    }
    if stuck.is_empty() {
        MoveError::RolledBack(reason)
    } else {
        MoveError::Split(format!(
            "{reason}; could not move back: {} (still in '{}')",
            stuck.join(", "),
            to.display()
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct Dirs {
        root: PathBuf,
        from: PathBuf,
        to: PathBuf,
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.root) {
                eprintln!("test dir cleanup failed: {error}");
            }
        }
    }

    fn dirs(tag: &str) -> Dirs {
        let root =
            std::env::temp_dir().join(format!("fluxdown-bt-move-{tag}-{}", uuid::Uuid::new_v4()));
        let from = root.join("from");
        let to = root.join("to");
        std::fs::create_dir_all(&from).expect("from");
        std::fs::create_dir_all(&to).expect("to");
        Dirs { root, from, to }
    }

    fn put(base: &Path, rel: &str, content: &[u8]) {
        let path = base.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("parents");
        std::fs::write(path, content).expect("write");
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
                    let content = std::fs::read(&path).expect("read");
                    out.push(format!("{rel}={}", String::from_utf8_lossy(&content)));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn journal_roundtrips_and_tolerates_old_or_broken_text() {
        let journal = MoveJournal {
            from: "/data/旧".to_owned(),
            to: "D:\\new dir".to_owned(),
            names: names(&["Show", "extra.srt"]),
            reseed: true,
        };
        assert_eq!(MoveJournal::parse(&journal.encode()), Some(journal));
        // `reseed` 缺省为 false。
        assert_eq!(
            MoveJournal::parse(r#"{"from":"a","to":"b","names":[]}"#),
            Some(MoveJournal {
                from: "a".to_owned(),
                to: "b".to_owned(),
                names: Vec::new(),
                reseed: false,
            })
        );
        assert_eq!(MoveJournal::parse(""), None);
        assert_eq!(MoveJournal::parse("{\"from\":1}"), None);
        assert_eq!(journal_key("t1"), "bt_move_t1");
    }

    #[test]
    fn moves_files_and_folders_and_leaves_everything_else() {
        let d = dirs("fresh");
        put(&d.from, "Show/S01/E01.mkv", b"video");
        put(&d.from, "Show/readme.txt", b"hi");
        put(&d.from, "extra.srt", b"subs");
        put(&d.from, "someone-elses.bin", b"keep");
        put(&d.to, "already-here.txt", b"theirs");

        let list = names(&["Show", "extra.srt", "never-landed.nfo"]);
        assert_eq!(find_conflict(&d.to, &list), None);
        assert_eq!(move_entries(&d.from, &d.to, &list, false), Ok(2));

        assert_eq!(tree(&d.from), ["someone-elses.bin=keep"]);
        assert_eq!(
            tree(&d.to),
            [
                "Show/",
                "Show/S01/",
                "Show/S01/E01.mkv=video",
                "Show/readme.txt=hi",
                "already-here.txt=theirs",
                "extra.srt=subs",
            ]
        );
    }

    #[test]
    fn conflict_is_reported_by_name_for_files_and_folders() {
        let d = dirs("conflict");
        put(&d.to, "Show/other.txt", b"x");
        put(&d.to, "b.bin", b"x");
        assert_eq!(
            find_conflict(&d.to, &names(&["a.bin", "Show"])),
            Some("Show".to_owned())
        );
        assert_eq!(
            find_conflict(&d.to, &names(&["b.bin", "Show"])),
            Some("b.bin".to_owned())
        );
        assert_eq!(find_conflict(&d.to, &names(&["a.bin"])), None);
    }

    #[test]
    fn failure_part_way_moves_everything_back_and_spares_foreign_data() {
        // 预检之后才冒出来的同名条目：文件与目录两种，都不能被覆盖或被「回滚」带走。
        for (foreign, kind) in [("b.bin", "file"), ("Pack/theirs.txt", "dir")] {
            let d = dirs(&format!("rollback-{kind}"));
            put(&d.from, "a/one.bin", b"1");
            put(&d.from, "b.bin", b"2");
            put(&d.from, "Pack/mine.txt", b"3");
            put(&d.to, foreign, b"foreign");
            let before = tree(&d.from);
            let order = if kind == "file" {
                names(&["a", "b.bin", "Pack"])
            } else {
                names(&["a", "Pack", "b.bin"])
            };

            let error = move_entries(&d.from, &d.to, &order, false).expect_err("must fail");
            assert!(matches!(error, MoveError::RolledBack(_)), "{error:?}");
            assert_eq!(tree(&d.from), before, "{kind}: source is whole again");
            let foreign_entry = format!("{foreign}=foreign");
            let left: Vec<String> = tree(&d.to)
                .into_iter()
                .filter(|entry| !entry.ends_with('/'))
                .collect();
            assert_eq!(left, [foreign_entry], "{kind}: only the foreign data stays");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_rollback_that_cannot_put_things_back_reports_where_the_data_is() {
        use std::os::unix::fs::PermissionsExt;

        let d = dirs("split");
        // a.bin 已挪到新目录；旧目录随后变成只读，回滚放不回去。
        put(&d.to, "a.bin", b"1");
        std::fs::set_permissions(&d.from, std::fs::Permissions::from_mode(0o555))
            .expect("read-only");
        // root 无视目录权限：这种环境下测不出放不回去的情况。
        let read_only = std::fs::write(d.from.join("probe"), b"").is_err();
        if read_only {
            let error = roll_back(&d.from, &d.to, &[&"a.bin".to_owned()], "boom".to_owned());
            match error {
                MoveError::Split(message) => {
                    assert!(
                        message.contains("boom") && message.contains("a.bin"),
                        "{message}"
                    );
                }
                other => panic!("expected a split result, got {other:?}"),
            }
            assert_eq!(
                std::fs::read(d.to.join("a.bin")).expect("still there"),
                b"1"
            );
        }
        std::fs::set_permissions(&d.from, std::fs::Permissions::from_mode(0o755)).expect("restore");
    }

    #[test]
    fn resume_finishes_an_interrupted_move_and_replaces_half_copies() {
        let d = dirs("resume");
        // 上次：`done.bin` 已挪走；`Pack` 挪了一半（a 已到，b 复制到一半、源还在）；
        // `late.bin` 还没开始。
        put(&d.to, "done.bin", b"done");
        put(&d.to, "Pack/a.bin", b"a");
        put(&d.to, "Pack/b.bin", b"hal");
        put(&d.from, "Pack/b.bin", b"whole-b");
        put(&d.from, "Pack/sub/c.bin", b"c");
        put(&d.from, "late.bin", b"late");

        let list = names(&["done.bin", "Pack", "late.bin"]);
        assert_eq!(move_entries(&d.from, &d.to, &list, true), Ok(2));
        assert!(tree(&d.from).is_empty(), "{:?}", tree(&d.from));
        assert_eq!(
            tree(&d.to),
            [
                "Pack/",
                "Pack/a.bin=a",
                "Pack/b.bin=whole-b",
                "Pack/sub/",
                "Pack/sub/c.bin=c",
                "done.bin=done",
                "late.bin=late",
            ]
        );
        // 再跑一遍（日志没来得及删就又重启）什么都不动。
        assert_eq!(move_entries(&d.from, &d.to, &list, true), Ok(0));
    }
}
