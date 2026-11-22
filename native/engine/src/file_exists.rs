//! 「文件已存在」策略的纯决策部分：覆盖授权、逐任务决定、启动序幕动作选择。
//!
//! 本模块不做 IO；磁盘探测与等待由 `download_manager` 的启动序幕完成，
//! 此处只把已探测到的事实映射为动作，便于确定性测试。

use crate::download_manager::FileExistsBehavior;
use crate::selection::FileExistsChoice;

/// 覆盖旧文件的授权范围。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OverwritePolicy {
    /// 不覆盖：同名最终文件按编号改名避让。
    #[default]
    Never,
    /// 全局 overwrite：任何落盘名都可替换同名旧文件。
    Any,
    /// 逐任务授权：只绑定询问时的文件名；完成期名字被精修成别的名字则不授权。
    Only(String),
}

impl OverwritePolicy {
    /// `name` 对应的同名旧普通文件是否允许被替换。
    pub fn permits(&self, name: &str) -> bool {
        match self {
            Self::Never => false,
            Self::Any => true,
            Self::Only(asked) => asked == name,
        }
    }
}

/// 持久化的逐任务决定（`tasks.exists_decision`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistsDecision {
    None,
    Rename,
    Overwrite,
    Skip,
    /// 重新下载时旧路径不归任务所有，必须先重新占名。
    RestartRename,
}

impl ExistsDecision {
    pub fn from_db(value: &str) -> Self {
        match value {
            "rename" => Self::Rename,
            "overwrite" => Self::Overwrite,
            "skip" => Self::Skip,
            "restart_rename" => Self::RestartRename,
            _ => Self::None,
        }
    }

    pub fn as_db(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Rename => "rename",
            Self::Overwrite => "overwrite",
            Self::Skip => "skip",
            Self::RestartRename => "restart_rename",
        }
    }

    pub(crate) fn has_unowned_path(self) -> bool {
        matches!(self, Self::Skip | Self::RestartRename)
    }

    /// 询问答复对应的持久化决定；`Cancel` 不持久化。
    pub fn from_choice(choice: FileExistsChoice) -> Option<Self> {
        match choice {
            FileExistsChoice::Rename => Some(Self::Rename),
            FileExistsChoice::Overwrite => Some(Self::Overwrite),
            FileExistsChoice::Skip => Some(Self::Skip),
            FileExistsChoice::Cancel => None,
        }
    }
}

/// 协议对「询问」的支持程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskSupport {
    /// 不可询问（HLS、BT、DASH 轨对）。
    None,
    /// 可询问且可跳过（HTTP/HTTPS/FTP）。
    WithSkip,
    /// 可询问但不可跳过（ED2K、DASH `.mpd`）。
    NoSkip,
}

/// 序幕探测到的事实。
#[derive(Debug, Clone, Copy)]
pub struct ExistsFacts {
    /// 目标路径上存在普通最终文件。
    pub final_file_exists: bool,
    /// `.fdownloading` 已在磁盘上或被兄弟任务预订。
    pub temp_conflict: bool,
    pub decision: ExistsDecision,
    pub global: FileExistsBehavior,
    pub support: AskSupport,
    pub unattended: bool,
    pub can_prompt: bool,
}

/// 序幕动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistsAction {
    /// 走常规 dedup（冲突则编号改名）。
    Dedup,
    /// 保留原名，完成认领时替换旧文件。
    Overwrite,
    /// 不下载，采纳已有文件。
    Skip,
    /// 让出并发槽并询问。
    Ask { allow_skip: bool },
}

/// 优先级：无冲突 → 跳过（逐任务决定或全局 skip）→ 硬冲突 → 逐任务决定 → 全局策略 →
/// 询问的各降级条件 → 询问。
///
/// 跳过只看最终文件是否存在：`.fdownloading` / 兄弟预订不影响「已有成品、不再下载」的
/// 判定（与引入询问前的全局 skip 行为一致）；覆盖与询问遇到硬冲突一律编号改名。
pub fn decide(facts: &ExistsFacts) -> ExistsAction {
    if !facts.final_file_exists {
        return ExistsAction::Dedup;
    }
    let skip_ok = facts.support == AskSupport::WithSkip;
    let wants_skip = match facts.decision {
        ExistsDecision::Skip => true,
        ExistsDecision::None => facts.global == FileExistsBehavior::Skip && skip_ok,
        ExistsDecision::Rename | ExistsDecision::Overwrite | ExistsDecision::RestartRename => false,
    };
    if wants_skip {
        return ExistsAction::Skip;
    }
    if facts.temp_conflict {
        return ExistsAction::Dedup;
    }
    match facts.decision {
        ExistsDecision::Rename | ExistsDecision::RestartRename => return ExistsAction::Dedup,
        ExistsDecision::Overwrite => return ExistsAction::Overwrite,
        ExistsDecision::Skip | ExistsDecision::None => {}
    }
    match facts.global {
        FileExistsBehavior::Rename => ExistsAction::Dedup,
        FileExistsBehavior::Overwrite => ExistsAction::Overwrite,
        FileExistsBehavior::Skip => {
            if skip_ok {
                ExistsAction::Skip
            } else {
                ExistsAction::Dedup
            }
        }
        FileExistsBehavior::Ask => {
            if facts.support == AskSupport::None || facts.unattended || !facts.can_prompt {
                ExistsAction::Dedup
            } else {
                ExistsAction::Ask {
                    allow_skip: skip_ok,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ExistsFacts {
        ExistsFacts {
            final_file_exists: true,
            temp_conflict: false,
            decision: ExistsDecision::None,
            global: FileExistsBehavior::Ask,
            support: AskSupport::WithSkip,
            unattended: false,
            can_prompt: true,
        }
    }

    #[test]
    fn precedence_table() {
        let ask = ExistsAction::Ask { allow_skip: true };
        assert_eq!(decide(&base()), ask);
        let mut f = base();
        f.final_file_exists = false;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        let mut f = base();
        f.temp_conflict = true;
        f.global = FileExistsBehavior::Overwrite;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        let mut f = base();
        f.decision = ExistsDecision::Overwrite;
        f.global = FileExistsBehavior::Rename;
        assert_eq!(decide(&f), ExistsAction::Overwrite);
        f.decision = ExistsDecision::Skip;
        assert_eq!(decide(&f), ExistsAction::Skip);
        f.decision = ExistsDecision::Rename;
        f.global = FileExistsBehavior::Overwrite;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        let mut f = base();
        f.global = FileExistsBehavior::Skip;
        assert_eq!(decide(&f), ExistsAction::Skip);
        f.support = AskSupport::NoSkip;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        f.global = FileExistsBehavior::Overwrite;
        assert_eq!(decide(&f), ExistsAction::Overwrite);
    }

    #[test]
    fn ask_degrades_to_dedup() {
        let mut f = base();
        f.support = AskSupport::None;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        let mut f = base();
        f.unattended = true;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        let mut f = base();
        f.can_prompt = false;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        let mut f = base();
        f.support = AskSupport::NoSkip;
        assert_eq!(decide(&f), ExistsAction::Ask { allow_skip: false });
    }

    #[test]
    fn skip_ignores_temp_conflicts_but_overwrite_and_ask_do_not() {
        let mut f = base();
        f.temp_conflict = true;
        f.global = FileExistsBehavior::Skip;
        assert_eq!(decide(&f), ExistsAction::Skip);
        f.global = FileExistsBehavior::Ask;
        assert_eq!(decide(&f), ExistsAction::Dedup);
        f.decision = ExistsDecision::Skip;
        assert_eq!(decide(&f), ExistsAction::Skip);
        f.decision = ExistsDecision::Overwrite;
        assert_eq!(decide(&f), ExistsAction::Dedup);
    }

    #[test]
    fn overwrite_policy_binds_asked_name() {
        assert!(!OverwritePolicy::Never.permits("a"));
        assert!(OverwritePolicy::Any.permits("a"));
        let only = OverwritePolicy::Only("a.zip".into());
        assert!(only.permits("a.zip"));
        assert!(!only.permits("b.zip"));
    }
}
