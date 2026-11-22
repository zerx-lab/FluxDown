//! `agent.remote.commandMany`：同一动作批量作用于多条远程任务。
//!
//! 云端支持 `POST /tasks/commands` 时每 [`CLOUD_BATCH_MAX`] 条一次请求；旧版云端（404 / 405）
//! 本进程内改为逐条 `POST /tasks/{id}/command`，并发上限 [`FALLBACK_CONCURRENCY`]。投影更新
//! （删除 / 已不存在的行移除、取消置 `canceled`）在全部请求结束后一次落地、一次落盘。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use fluxdown_protocol::{
    ApplicationErrorCode, ErrorReason, REMOTE_COMMAND_MANY_MAX, RemoteCommandAction,
    RemoteCommandFailure, RemoteCommandManyParams, RemoteCommandManyResult, RemoteCommandParams,
    RemoteTaskStatus, RpcErrorData,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{RemoteError, RemoteTaskService, action_wire};
use crate::cloud::CloudError;

/// 云端批量端点单次请求的条数上限（FluxCloud `MAX_BATCH_COMMAND_IDS`）。
const CLOUD_BATCH_MAX: usize = 200;
/// 旧版云端逐条回退时的并发请求数。
const FALLBACK_CONCURRENCY: usize = 4;

/// 单条记录在云端的处理结果。
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    Done,
    /// 云端已没有这条记录。
    Gone,
    Failed(RpcErrorData),
}

impl RemoteTaskService {
    /// 批量远程指令；语义与逐条 [`Self::command`] 相同，单条失败不影响其余条目。
    ///
    /// # Errors
    ///
    /// 条数超过 [`REMOTE_COMMAND_MANY_MAX`] 返回 `InvalidArgument`；会话已切换或本地状态无法
    /// 落盘时返回对应错误（此时云端可能已处理部分条目，下一次快照会收敛投影）。
    pub async fn command_many(
        &self,
        params: RemoteCommandManyParams,
    ) -> Result<RemoteCommandManyResult, RemoteError> {
        if params.task_ids.len() > REMOTE_COMMAND_MANY_MAX {
            return Err(RemoteError::InvalidArgument {
                field: "taskIds",
                reason: None,
                message: format!(
                    "at most {REMOTE_COMMAND_MANY_MAX} task ids per request, got {}",
                    params.task_ids.len()
                ),
            });
        }
        let epoch = self.cloud.request_epoch();
        let action = params.action;
        let finalizes = matches!(
            action,
            RemoteCommandAction::Cancel | RemoteCommandAction::Delete
        );
        let mut seen = HashSet::new();
        let ids = params
            .task_ids
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .collect::<Vec<_>>();
        let projection = self
            .tasks()
            .await
            .into_iter()
            .map(|task| (task.id, (task.status, task.to_device)))
            .collect::<HashMap<_, _>>();
        let local_device = self.local_device_id().await;

        let mut outcomes = HashMap::<String, Outcome>::with_capacity(ids.len());
        // 已由逐条路径处理完（含投影更新）的条目。
        let mut settled = HashMap::<String, Result<(), RpcErrorData>>::new();
        let mut via_cloud = Vec::with_capacity(ids.len());
        for id in &ids {
            if id.trim().is_empty() {
                let mut error = RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false);
                error.field = Some("taskIds".to_owned());
                outcomes.insert(id.clone(), Outcome::Failed(error));
                continue;
            }
            match projection.get(id) {
                None if !finalizes => {
                    outcomes.insert(id.clone(), Outcome::Gone);
                }
                Some((RemoteTaskStatus::Unknown, _)) => {
                    outcomes.insert(
                        id.clone(),
                        Outcome::Failed(RemoteError::TaskUnavailable(id.clone()).into_rpc_error()),
                    );
                }
                // 本机执行的任务要先处理本机 daemon 任务：走逐条路径（实际中极少）。
                Some((_, to_device)) if *to_device == local_device => {
                    let result = self
                        .command(RemoteCommandParams {
                            task_id: id.clone(),
                            action,
                            command_id: None,
                            delete_files: params.delete_files,
                        })
                        .await
                        .map_err(RemoteError::into_rpc_error);
                    settled.insert(id.clone(), result);
                }
                _ => via_cloud.push(id.clone()),
            }
        }

        let mut chunks = via_cloud.chunks(CLOUD_BATCH_MAX);
        while let Some(chunk) = chunks.next() {
            match self
                .cloud_batch(action, chunk, params.delete_files, epoch)
                .await
            {
                Ok(results) => outcomes.extend(results),
                Err(error) => {
                    // 整块失败（网络 / 鉴权等）：其余块不再尝试，同一错误一并回报。
                    let data = error.to_rpc_error();
                    for id in chunk.iter().chain(chunks.by_ref().flatten()) {
                        outcomes.insert(id.clone(), Outcome::Failed(data.clone()));
                    }
                }
            }
        }

        let mut forget = Vec::new();
        let mut canceled = Vec::new();
        for (id, outcome) in &outcomes {
            match outcome {
                Outcome::Gone => forget.push(id.clone()),
                Outcome::Done if action == RemoteCommandAction::Delete => forget.push(id.clone()),
                Outcome::Done if action == RemoteCommandAction::Cancel => {
                    canceled.push(id.clone());
                }
                Outcome::Done | Outcome::Failed(_) => {}
            }
        }
        if !forget.is_empty() || !canceled.is_empty() {
            self.settle(&forget, &canceled, epoch).await?;
        }

        let mut result = RemoteCommandManyResult::default();
        for id in ids {
            let verdict = match settled.remove(&id) {
                Some(verdict) => verdict,
                None => match outcomes.remove(&id) {
                    Some(Outcome::Done) => Ok(()),
                    Some(Outcome::Gone) if finalizes => Ok(()),
                    Some(Outcome::Gone) => Err(RemoteError::Gone(id.clone()).into_rpc_error()),
                    Some(Outcome::Failed(error)) => Err(error),
                    None => Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)),
                },
            };
            match verdict {
                Ok(()) => result.succeeded.push(id),
                Err(error) => result
                    .failed
                    .push(RemoteCommandFailure { task_id: id, error }),
            }
        }
        Ok(result)
    }

    /// 一块（≤ [`CLOUD_BATCH_MAX`] 条）交给云端；旧版云端回退逐条并发下发。
    async fn cloud_batch(
        &self,
        action: RemoteCommandAction,
        chunk: &[String],
        delete_files: bool,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Vec<(String, Outcome)>, CloudError> {
        let delete_files = action == RemoteCommandAction::Delete && delete_files;
        if !self.batch_unsupported.load(Ordering::Relaxed) {
            let sent = self
                .cloud
                .at_epoch(epoch)
                .command_remote_many(&json!({
                    "action": action_wire(action),
                    "taskIds": chunk,
                    "deleteFiles": delete_files,
                }))
                .await;
            match sent {
                Ok(value) => return Ok(parse_batch_results(chunk, &value)),
                Err(error) if matches!(error.status, Some(404 | 405)) => {
                    tracing::info!(
                        "FluxCloud has no batch command endpoint; sending remote commands one by one"
                    );
                    self.batch_unsupported.store(true, Ordering::Relaxed);
                }
                Err(error) => return Err(error),
            }
        }
        let cloud = self.cloud.at_epoch(epoch);
        let results = futures_util::stream::iter(chunk.iter().cloned())
            .map(|id| {
                let cloud = cloud.clone();
                async move {
                    let sent = cloud
                        .command_remote(
                            &id,
                            &json!({
                                "action": action_wire(action),
                                "commandId": Uuid::new_v4().to_string(),
                                "deleteFiles": delete_files,
                            }),
                        )
                        .await;
                    let outcome = match sent {
                        Ok(_) => Outcome::Done,
                        Err(error) if error.status == Some(404) => Outcome::Gone,
                        Err(error) => Outcome::Failed(error.to_rpc_error()),
                    };
                    (id, outcome)
                }
            })
            .buffer_unordered(FALLBACK_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        Ok(results)
    }

    /// 一次临界区内落地批量结果：移除删除成功 / 云端已没有的行，取消成功的行置 `canceled`。
    async fn settle(
        &self,
        forget: &[String],
        canceled: &[String],
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), RemoteError> {
        self.mutate_tasks(epoch, true, |tasks, runtime| {
            let now = std::time::Instant::now();
            let gone = forget.iter().map(String::as_str).collect::<HashSet<_>>();
            let cancel = canceled.iter().map(String::as_str).collect::<HashSet<_>>();
            for id in forget {
                runtime.removed_at.insert(id.clone(), now);
                runtime.touched_at.remove(id);
            }
            let before = tasks.len();
            tasks.retain(|task| !gone.contains(task.id.as_str()));
            let mut changed = tasks.len() != before;
            for task in tasks.iter_mut().filter(|task| {
                cancel.contains(task.id.as_str())
                    && !matches!(
                        task.status,
                        RemoteTaskStatus::Completed
                            | RemoteTaskStatus::Failed
                            | RemoteTaskStatus::Canceled
                    )
            }) {
                task.status = RemoteTaskStatus::Canceled;
                task.speed = 0;
                task.error = None;
                runtime.touched_at.insert(task.id.clone(), now);
                changed = true;
            }
            changed
        })
        .await
        .map(drop)
    }
}

/// `{"results":[{"taskId","outcome"}]}` → 按块内 id 的结果；缺失 / 未知结果记为内部错误。
fn parse_batch_results(chunk: &[String], value: &Value) -> Vec<(String, Outcome)> {
    let mut by_id = HashMap::new();
    for item in value
        .get("results")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
    {
        let (Some(id), Some(outcome)) = (
            item.get("taskId").and_then(Value::as_str),
            item.get("outcome").and_then(Value::as_str),
        ) else {
            continue;
        };
        let outcome = match outcome {
            "ok" => Outcome::Done,
            "notFound" => Outcome::Gone,
            "targetOffline" => Outcome::Failed(
                RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
                    .with_reason(ErrorReason::TargetDeviceOffline),
            ),
            other => {
                tracing::warn!(outcome = other, "unknown batch command outcome");
                Outcome::Failed(RpcErrorData::new(ApplicationErrorCode::Internal, false))
            }
        };
        by_id.insert(id.to_owned(), outcome);
    }
    chunk
        .iter()
        .map(|id| {
            let outcome = by_id.remove(id).unwrap_or_else(|| {
                tracing::warn!(task = %id, "batch command response is missing a task");
                Outcome::Failed(RpcErrorData::new(ApplicationErrorCode::Internal, false))
            });
            (id.clone(), outcome)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_results_map_outcomes_and_missing_ids() {
        let chunk = ["a", "b", "c", "d", "e"].map(str::to_owned);
        let parsed = parse_batch_results(
            &chunk,
            &json!({"results": [
                {"taskId": "a", "outcome": "ok"},
                {"taskId": "b", "outcome": "notFound"},
                {"taskId": "c", "outcome": "targetOffline"},
                {"taskId": "d", "outcome": "fromTheFuture"},
                {"taskId": "zzz", "outcome": "ok"}
            ]}),
        );
        assert_eq!(parsed[0], ("a".to_owned(), Outcome::Done));
        assert_eq!(parsed[1], ("b".to_owned(), Outcome::Gone));
        let Outcome::Failed(offline) = &parsed[2].1 else {
            panic!("offline target fails");
        };
        assert_eq!(offline.reason, Some(ErrorReason::TargetDeviceOffline));
        assert!(
            matches!(&parsed[3].1, Outcome::Failed(error) if error.code == ApplicationErrorCode::Internal)
        );
        assert!(
            matches!(&parsed[4].1, Outcome::Failed(error) if error.code == ApplicationErrorCode::Internal)
        );
        assert_eq!(parsed.len(), 5);
    }
}
