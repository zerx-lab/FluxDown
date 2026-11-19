//! 一批下载命令的合并与重试规则（纯逻辑，不含任何 UI / 异步）。
//!
//! 同类多任务的暂停 / 继续 / 删除合并成一次批量 RPC：agent 对 `daemon.*` 请求串行排队，
//! 队列有上限，逐任务发请求在数百个任务时必然被 `Unavailable` 拒绝，且每条删除还会各自
//! 触发一次完整任务快照。无法合并的命令由调用方按 [`MAX_IN_FLIGHT`] 限制并发，并对
//! 幂等命令按 [`retry_delay`] 退避重试。

use std::{collections::HashSet, time::Duration};

use fluxdown_protocol::{
    ApplicationErrorCode, REMOTE_COMMAND_MANY_MAX, RemoteCommandAction, RemoteCommandManyParams,
    RemoteCommandParams, RpcErrorData,
};

use crate::controller::DownloadsCommand;

/// 同一批命令同时在途的请求数上限（小于 agent 单通道排队容量）。
pub(crate) const MAX_IN_FLIGHT: usize = 16;

/// 单个命令被拒绝（`Unavailable` 且可重试）后的最大重试次数。
pub(crate) const MAX_RETRIES: u32 = 4;
const RETRY_BASE: Duration = Duration::from_millis(200);

/// 被拒绝后的退避时长；`None` 表示不再重试（错误不可重试，或次数用尽）。
/// 时长按重试次数翻倍。
#[must_use]
pub(crate) fn retry_delay(error: &RpcErrorData, retries_done: u32) -> Option<Duration> {
    (error.retryable
        && error.code == ApplicationErrorCode::Unavailable
        && retries_done < MAX_RETRIES)
        .then(|| RETRY_BASE * 2_u32.pow(retries_done))
}

/// 幂等命令的副本，供退避重试重新下发；非幂等命令（建任务、切换加速、重新下载等）
/// 返回 `None`，失败即终态，避免请求其实已被处理时重复生效。
#[must_use]
pub(crate) fn retry_copy(command: &DownloadsCommand) -> Option<DownloadsCommand> {
    Some(match command {
        DownloadsCommand::Pause { task_id } => DownloadsCommand::Pause {
            task_id: task_id.clone(),
        },
        DownloadsCommand::Resume { task_id } => DownloadsCommand::Resume {
            task_id: task_id.clone(),
        },
        DownloadsCommand::Delete {
            task_id,
            delete_files,
        } => DownloadsCommand::Delete {
            task_id: task_id.clone(),
            delete_files: *delete_files,
        },
        DownloadsCommand::PauseMany { task_ids } => DownloadsCommand::PauseMany {
            task_ids: task_ids.clone(),
        },
        DownloadsCommand::ResumeMany { task_ids } => DownloadsCommand::ResumeMany {
            task_ids: task_ids.clone(),
        },
        DownloadsCommand::DeleteMany {
            task_ids,
            delete_files,
        } => DownloadsCommand::DeleteMany {
            task_ids: task_ids.clone(),
            delete_files: *delete_files,
        },
        DownloadsCommand::MoveToQueue { task_id, queue_id } => DownloadsCommand::MoveToQueue {
            task_id: task_id.clone(),
            queue_id: queue_id.clone(),
        },
        DownloadsCommand::IgnorePluginRetry { task_id } => DownloadsCommand::IgnorePluginRetry {
            task_id: task_id.clone(),
        },
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    Pause,
    Resume,
    Delete {
        delete_files: bool,
    },
    /// 远程任务（`agent.remote.commandMany`）；`delete_files` 仅 `Delete` 有意义，其余恒为 `false`。
    Remote {
        action: RemoteCommandAction,
        delete_files: bool,
    },
}

struct Group {
    kind: GroupKind,
    ids: Vec<String>,
    seen: HashSet<String>,
}

enum Slot {
    Group(usize),
    Single(DownloadsCommand),
}

/// 把同类多任务命令合并为批量命令：
/// - 暂停 / 继续各合并为一条；删除按 `delete_files` 分别合并（至多两条）；
/// - 远程任务命令（无自带 `command_id`）按动作（删除再按 `delete_files`）合并为
///   `agent.remote.commandMany`，每条至多 [`REMOTE_COMMAND_MANY_MAX`] 个任务；
/// - 同一任务 ID 在同类里只保留一次；
/// - 合并后只剩一个任务时保持原来的单任务命令（宿主据此识别「单任务继续」）；
/// - 其余命令原样保留。
///
/// 输出按各组 / 各命令首次出现的位置排列。同一批命令互不依赖（调用方保证），
/// 因此同类合并不改变结果。
#[must_use]
pub(crate) fn coalesce_commands(commands: Vec<DownloadsCommand>) -> Vec<DownloadsCommand> {
    let mut groups: Vec<Group> = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    for command in commands {
        let (kind, task_id) = match command {
            DownloadsCommand::Pause { task_id } => (GroupKind::Pause, task_id),
            DownloadsCommand::Resume { task_id } => (GroupKind::Resume, task_id),
            DownloadsCommand::Delete {
                task_id,
                delete_files,
            } => (GroupKind::Delete { delete_files }, task_id),
            DownloadsCommand::RemoteCommand(RemoteCommandParams {
                task_id,
                action,
                command_id: None,
                delete_files,
            }) => (
                GroupKind::Remote {
                    action,
                    delete_files: action == RemoteCommandAction::Delete && delete_files,
                },
                task_id,
            ),
            other => {
                slots.push(Slot::Single(other));
                continue;
            }
        };
        let index = match groups.iter().position(|group| group.kind == kind) {
            Some(index) => index,
            None => {
                groups.push(Group {
                    kind,
                    ids: Vec::new(),
                    seen: HashSet::new(),
                });
                slots.push(Slot::Group(groups.len() - 1));
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        if group.seen.insert(task_id.clone()) {
            group.ids.push(task_id);
        }
    }
    let mut groups: Vec<Option<Group>> = groups.into_iter().map(Some).collect();
    slots
        .into_iter()
        .flat_map(|slot| match slot {
            Slot::Single(command) => vec![command],
            Slot::Group(index) => groups
                .get_mut(index)
                .and_then(Option::take)
                .map(group_commands)
                .unwrap_or_default(),
        })
        .collect()
}

fn group_commands(group: Group) -> Vec<DownloadsCommand> {
    let Group { kind, ids, .. } = group;
    let only = match ids.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    match (kind, only) {
        (GroupKind::Pause, Some(task_id)) => vec![DownloadsCommand::Pause { task_id }],
        (GroupKind::Pause, None) => vec![DownloadsCommand::PauseMany { task_ids: ids }],
        (GroupKind::Resume, Some(task_id)) => vec![DownloadsCommand::Resume { task_id }],
        (GroupKind::Resume, None) => vec![DownloadsCommand::ResumeMany { task_ids: ids }],
        (GroupKind::Delete { delete_files }, Some(task_id)) => {
            vec![DownloadsCommand::Delete {
                task_id,
                delete_files,
            }]
        }
        (GroupKind::Delete { delete_files }, None) => vec![DownloadsCommand::DeleteMany {
            task_ids: ids,
            delete_files,
        }],
        (
            GroupKind::Remote {
                action,
                delete_files,
            },
            Some(task_id),
        ) => vec![DownloadsCommand::RemoteCommand(RemoteCommandParams {
            task_id,
            action,
            command_id: None,
            delete_files,
        })],
        (
            GroupKind::Remote {
                action,
                delete_files,
            },
            None,
        ) => ids
            .chunks(REMOTE_COMMAND_MANY_MAX)
            .map(|chunk| {
                DownloadsCommand::RemoteCommandMany(RemoteCommandManyParams {
                    task_ids: chunk.to_vec(),
                    action,
                    delete_files,
                })
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        ApplicationErrorCode, REMOTE_COMMAND_MANY_MAX, RemoteCommandAction, RemoteCommandParams,
        RpcErrorData,
    };

    use super::{MAX_RETRIES, coalesce_commands, retry_copy, retry_delay};
    use crate::controller::DownloadsCommand;

    fn pause(id: &str) -> DownloadsCommand {
        DownloadsCommand::Pause {
            task_id: id.to_owned(),
        }
    }

    fn resume(id: &str) -> DownloadsCommand {
        DownloadsCommand::Resume {
            task_id: id.to_owned(),
        }
    }

    fn delete(id: &str, delete_files: bool) -> DownloadsCommand {
        DownloadsCommand::Delete {
            task_id: id.to_owned(),
            delete_files,
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn same_kind_tasks_merge_into_one_batch_command() {
        let merged = coalesce_commands(vec![pause("a"), pause("b"), pause("c")]);
        assert_eq!(merged.len(), 1);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::PauseMany { task_ids } if *task_ids == ids(&["a", "b", "c"])
        ));
    }

    #[test]
    fn delete_groups_split_by_delete_files() {
        let merged = coalesce_commands(vec![
            delete("a", false),
            delete("b", true),
            delete("c", false),
            delete("d", true),
        ]);
        assert_eq!(merged.len(), 2);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::DeleteMany { task_ids, delete_files: false }
                if *task_ids == ids(&["a", "c"])
        ));
        assert!(matches!(
            &merged[1],
            DownloadsCommand::DeleteMany { task_ids, delete_files: true }
                if *task_ids == ids(&["b", "d"])
        ));
    }

    #[test]
    fn single_task_keeps_its_single_command() {
        let merged = coalesce_commands(vec![resume("only")]);
        assert_eq!(merged.len(), 1);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::Resume { task_id } if task_id == "only"
        ));
    }

    #[test]
    fn duplicate_ids_collapse_and_leave_a_single_command() {
        let merged = coalesce_commands(vec![delete("a", false), delete("a", false)]);
        assert_eq!(merged.len(), 1);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::Delete { task_id, delete_files: false } if task_id == "a"
        ));
    }

    #[test]
    fn mixed_batch_keeps_first_seen_order_and_passes_others_through() {
        let merged = coalesce_commands(vec![
            pause("a"),
            DownloadsCommand::OpenTask {
                task_id: "x".to_owned(),
            },
            resume("b"),
            pause("c"),
            DownloadsCommand::MoveToQueue {
                task_id: "m".to_owned(),
                queue_id: "q".to_owned(),
            },
        ]);
        assert_eq!(merged.len(), 4);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::PauseMany { task_ids } if *task_ids == ids(&["a", "c"])
        ));
        assert!(matches!(&merged[1], DownloadsCommand::OpenTask { .. }));
        assert!(matches!(
            &merged[2],
            DownloadsCommand::Resume { task_id } if task_id == "b"
        ));
        assert!(matches!(&merged[3], DownloadsCommand::MoveToQueue { .. }));
    }

    #[test]
    fn empty_batch_stays_empty() {
        assert!(coalesce_commands(Vec::new()).is_empty());
    }

    #[test]
    fn only_retryable_unavailable_is_retried_with_doubling_backoff() {
        let busy = RpcErrorData::new(ApplicationErrorCode::Unavailable, true);
        let first = retry_delay(&busy, 0);
        let second = retry_delay(&busy, 1);
        assert!(first.is_some_and(|first| second.is_some_and(|second| second == first * 2)));
        assert_eq!(retry_delay(&busy, MAX_RETRIES), None);

        let not_retryable = RpcErrorData::new(ApplicationErrorCode::Unavailable, false);
        assert_eq!(retry_delay(&not_retryable, 0), None);
        let other = RpcErrorData::new(ApplicationErrorCode::InvalidArgument, true);
        assert_eq!(retry_delay(&other, 0), None);
    }

    #[test]
    fn only_idempotent_commands_can_be_replayed() {
        assert!(retry_copy(&pause("a")).is_some());
        assert!(retry_copy(&delete("a", true)).is_some());
        assert!(
            retry_copy(&DownloadsCommand::DeleteMany {
                task_ids: ids(&["a", "b"]),
                delete_files: true,
            })
            .is_some_and(|copy| matches!(
                copy,
                DownloadsCommand::DeleteMany { task_ids, delete_files: true }
                    if task_ids == ids(&["a", "b"])
            ))
        );
        assert!(
            retry_copy(&DownloadsCommand::ToggleBoost {
                task_id: "a".to_owned()
            })
            .is_none()
        );
        assert!(retry_copy(&DownloadsCommand::PauseAll).is_none());
    }

    fn remote(id: &str, action: RemoteCommandAction, delete_files: bool) -> DownloadsCommand {
        DownloadsCommand::RemoteCommand(RemoteCommandParams {
            task_id: id.to_owned(),
            action,
            command_id: None,
            delete_files,
        })
    }

    #[test]
    fn remote_commands_of_one_action_merge_into_remote_batch() {
        let merged = coalesce_commands(vec![
            remote("a", RemoteCommandAction::Pause, false),
            remote("b", RemoteCommandAction::Resume, false),
            remote("c", RemoteCommandAction::Pause, false),
            remote("a", RemoteCommandAction::Pause, false),
        ]);
        assert_eq!(merged.len(), 2);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::RemoteCommandMany(params)
                if params.action == RemoteCommandAction::Pause
                    && !params.delete_files
                    && params.task_ids == ids(&["a", "c"])
        ));
        assert!(matches!(
            &merged[1],
            DownloadsCommand::RemoteCommand(params)
                if params.task_id == "b" && params.action == RemoteCommandAction::Resume
        ));
    }

    #[test]
    fn remote_deletes_split_by_delete_files_and_other_actions_ignore_it() {
        let merged = coalesce_commands(vec![
            remote("a", RemoteCommandAction::Delete, false),
            remote("b", RemoteCommandAction::Delete, true),
            remote("c", RemoteCommandAction::Delete, false),
            remote("d", RemoteCommandAction::Delete, true),
            remote("e", RemoteCommandAction::Pause, true),
            remote("f", RemoteCommandAction::Pause, false),
        ]);
        assert_eq!(merged.len(), 3);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::RemoteCommandMany(params)
                if !params.delete_files && params.task_ids == ids(&["a", "c"])
        ));
        assert!(matches!(
            &merged[1],
            DownloadsCommand::RemoteCommandMany(params)
                if params.delete_files && params.task_ids == ids(&["b", "d"])
        ));
        assert!(matches!(
            &merged[2],
            DownloadsCommand::RemoteCommandMany(params)
                if !params.delete_files && params.task_ids == ids(&["e", "f"])
        ));
    }

    #[test]
    fn remote_batches_are_chunked_and_local_commands_stay_separate() {
        let mut commands: Vec<DownloadsCommand> = (0..=REMOTE_COMMAND_MANY_MAX)
            .map(|index| remote(&format!("r{index}"), RemoteCommandAction::Delete, false))
            .collect();
        commands.push(pause("local"));
        let merged = coalesce_commands(commands);
        assert_eq!(merged.len(), 3);
        assert!(matches!(
            &merged[0],
            DownloadsCommand::RemoteCommandMany(params)
                if params.task_ids.len() == REMOTE_COMMAND_MANY_MAX
        ));
        assert!(matches!(
            &merged[1],
            DownloadsCommand::RemoteCommandMany(params)
                if params.task_ids == vec![format!("r{REMOTE_COMMAND_MANY_MAX}")]
        ));
        assert!(matches!(
            &merged[2],
            DownloadsCommand::Pause { task_id } if task_id == "local"
        ));
    }

    #[test]
    fn remote_command_with_own_command_id_is_not_merged_or_replayed() {
        let with_id = DownloadsCommand::RemoteCommand(RemoteCommandParams {
            task_id: "a".to_owned(),
            action: RemoteCommandAction::Pause,
            command_id: Some("id".to_owned()),
            delete_files: false,
        });
        let merged = coalesce_commands(vec![
            with_id,
            remote("b", RemoteCommandAction::Pause, false),
        ]);
        assert_eq!(merged.len(), 2);
        assert!(
            matches!(&merged[0], DownloadsCommand::RemoteCommand(params) if params.task_id == "a")
        );
        assert!(
            matches!(&merged[1], DownloadsCommand::RemoteCommand(params) if params.task_id == "b")
        );
        assert!(retry_copy(&remote("a", RemoteCommandAction::Pause, false)).is_none());
    }
}
