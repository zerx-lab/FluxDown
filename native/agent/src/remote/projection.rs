//! 远程任务投影的纯合并规则（无 I/O，便于单测）。
//!
//! FluxCloud `GET /tasks/remote` 返回全部未终态任务（永不截断）+ 最近若干终态任务：快照里没有
//! 的行就是云端已没有的记录。唯一的例外是快照请求发出之后本进程才写入的行（下发结果 / 实时事件
//! / 指令落地）——它们比快照新，以本地为准；同理，快照请求之后本进程移除的行不得被快照带回。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use fluxdown_protocol::RemoteTaskDto;

/// 按 id 插入或覆盖一行。
pub(super) fn upsert(tasks: &mut Vec<RemoteTaskDto>, task: RemoteTaskDto) {
    match tasks.iter_mut().find(|existing| existing.id == task.id) {
        Some(existing) => *existing = task,
        None => tasks.push(task),
    }
}

/// 用快照替换投影。
///
/// - 快照里的行：快照请求之后本地移除过 → 丢弃；之后本地写入过且本地仍有 → 保留本地版本；
///   否则取快照版本。
/// - 只在本地的行：快照请求之后写入过才保留（快照可能早于这次下发）；否则视为云端已删除。
///   `touched_at` 可给多张表（行写入时刻、执行端接单绑定时刻），任一晚于快照请求即算新写入。
///   进程重启后没有任何写入记录，持久化的旧行在第一次快照即收敛。
pub(super) fn merge_snapshot(
    snapshot: Vec<RemoteTaskDto>,
    previous: &[RemoteTaskDto],
    touched_at: &[&HashMap<String, Instant>],
    removed_at: &HashMap<String, Instant>,
    requested_at: Instant,
) -> Vec<RemoteTaskDto> {
    let after = |at: Option<&Instant>| at.is_some_and(|at| *at > requested_at);
    let touched = |id: &str| touched_at.iter().any(|map| after(map.get(id)));
    let local = previous
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::with_capacity(snapshot.len());
    let mut merged = Vec::with_capacity(snapshot.len().max(previous.len()));
    for task in snapshot {
        if !seen.insert(task.id.clone()) || after(removed_at.get(&task.id)) {
            continue;
        }
        match local.get(task.id.as_str()) {
            Some(newer) if touched(&task.id) => merged.push((*newer).clone()),
            _ => merged.push(task),
        }
    }
    for task in previous {
        if !seen.contains(&task.id) && touched(&task.id) && !after(removed_at.get(&task.id)) {
            merged.push(task.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fluxdown_protocol::RemoteTaskStatus;
    use serde_json::json;

    use super::*;

    fn task(id: &str, status: &str) -> RemoteTaskDto {
        serde_json::from_value(json!({"id": id, "toDevice": "d2", "status": status})).expect("task")
    }

    fn ids(tasks: &[RemoteTaskDto]) -> Vec<(&str, RemoteTaskStatus)> {
        tasks
            .iter()
            .map(|task| (task.id.as_str(), task.status))
            .collect()
    }

    #[test]
    fn rows_the_cloud_no_longer_has_are_dropped_on_the_first_snapshot() {
        // 重启后的持久化投影：没有任何写入记录。
        let previous = [
            task("pending", "pending"),
            task("failed", "failed"),
            task("kept", "downloading"),
        ];
        let merged = merge_snapshot(
            vec![task("kept", "paused")],
            &previous,
            &[&HashMap::new()],
            &HashMap::new(),
            Instant::now(),
        );
        assert_eq!(ids(&merged), [("kept", RemoteTaskStatus::Paused)]);
    }

    #[test]
    fn writes_after_the_snapshot_request_win_over_the_snapshot() {
        let requested_at = Instant::now();
        let later = requested_at + Duration::from_millis(5);
        let previous = [
            task("dispatched", "pending"),
            task("canceled", "canceled"),
            task("stale", "pending"),
        ];
        let touched = HashMap::from([
            ("dispatched".to_owned(), later),
            ("canceled".to_owned(), later),
            ("stale".to_owned(), requested_at - Duration::from_millis(5)),
        ]);
        let merged = merge_snapshot(
            vec![task("canceled", "downloading")],
            &previous,
            &[&touched],
            &HashMap::new(),
            requested_at,
        );
        assert_eq!(
            ids(&merged),
            [
                ("canceled", RemoteTaskStatus::Canceled),
                ("dispatched", RemoteTaskStatus::Pending),
            ]
        );
    }

    #[test]
    fn removals_during_the_snapshot_are_not_resurrected_and_duplicates_collapse() {
        let requested_at = Instant::now();
        let removed = HashMap::from([(
            "deleted".to_owned(),
            requested_at + Duration::from_millis(1),
        )]);
        let merged = merge_snapshot(
            vec![
                task("deleted", "pending"),
                task("a", "pending"),
                task("a", "failed"),
            ],
            &[],
            &[],
            &removed,
            requested_at,
        );
        assert_eq!(ids(&merged), [("a", RemoteTaskStatus::Pending)]);
    }

    #[test]
    fn upsert_replaces_in_place_or_appends() {
        let mut tasks = vec![task("a", "pending")];
        upsert(&mut tasks, task("a", "accepted"));
        upsert(&mut tasks, task("b", "pending"));
        assert_eq!(
            ids(&tasks),
            [
                ("a", RemoteTaskStatus::Accepted),
                ("b", RemoteTaskStatus::Pending)
            ]
        );
    }
}
