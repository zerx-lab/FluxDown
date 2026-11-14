//! 大批量删除本地任务：分块 `daemon.task.deleteMany`、进度、取消与结果汇总。
//!
//! 一次几千个任务的删除若合成一个 RPC，用户确认后既看不到进度也无法中途停下，daemon 单次
//! 事务与单条事件也过大。这里按 [`DELETE_CHUNK_SIZE`] 顺序下发多块，运行时无关（执行、
//! 睡眠、进度都由调用方注入），取消 / 重试复用 [`crate::submission`] 的同一套原语。
//!
//! 失败策略（与导入一致，取保守的一侧）：
//! - `Unavailable`（agent 队列拒绝，请求未被处理）按 [`retry_delay`] 退避重发同一块——
//!   `deleteMany` 对已删除 / 未知 id 幂等，重发安全；
//! - 重发用尽或 `Timeout`（结果不确定）视为连接中断：该块结果不确定，**放弃剩余分块**；
//! - 其它错误（如文件清理失败：daemon 保留该批里失败的任务、其余照删）只记该块失败，
//!   **继续后面的块**，块与块互相独立；
//! - 取消：不再发新块，在途块给 [`crate::submission`] 的收尾时间，已删除的任务不回滚。

use std::{future::Future, time::Duration};

use fluxdown_protocol::RpcErrorData;
use fluxdown_ui_i18n::Translator;

use crate::{
    batch::retry_delay,
    controller::{DownloadsCommand, DownloadsResult, PortFuture},
    strings::{DownloadStrings, error_text},
    submission::{CancelToken, Either, Settled, SubmitNotice, is_uncertain, race, settle},
};

/// 每次 `daemon.task.deleteMany` 的任务数。与新建的分块同为 256：单块的数据库事务、
/// 文件清理与回传的 `TasksDeleted` 事件都足够小，进度条粒度也合适。
pub(crate) const DELETE_CHUNK_SIZE: usize = 256;
/// 至少这么多个本地任务一起删除才走分块路径；单个任务沿用 `daemon.task.delete`。
pub(crate) const MIN_BATCH_DELETE: usize = 2;

/// 删除请求：同一 `delete_files` 的一组本地任务。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeletionRequest {
    pub(crate) task_ids: Vec<String>,
    pub(crate) delete_files: bool,
}

/// 从命令列表里拆出本地多任务删除：按 `delete_files` 把单任务 `Delete` 聚成请求
/// （同一 id 只保留一次，保持首次出现的顺序）。不足 `min_batch` 个的请求原样还原为单任务
/// 命令放回返回的剩余命令（保持既有的单任务路径），其余命令不动。
#[must_use]
pub(crate) fn split_local_deletes(
    commands: Vec<DownloadsCommand>,
    min_batch: usize,
) -> (Vec<DeletionRequest>, Vec<DownloadsCommand>) {
    let mut requests: Vec<DeletionRequest> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rest = Vec::new();
    for command in commands {
        let DownloadsCommand::Delete {
            task_id,
            delete_files,
        } = command
        else {
            rest.push(command);
            continue;
        };
        if !seen.insert((task_id.clone(), delete_files)) {
            continue;
        }
        match requests
            .iter_mut()
            .find(|request| request.delete_files == delete_files)
        {
            Some(request) => request.task_ids.push(task_id),
            None => requests.push(DeletionRequest {
                task_ids: vec![task_id],
                delete_files,
            }),
        }
    }
    let mut batches = Vec::new();
    for request in requests {
        if request.task_ids.len() >= min_batch {
            batches.push(request);
        } else {
            rest.extend(
                request
                    .task_ids
                    .into_iter()
                    .map(|task_id| DownloadsCommand::Delete {
                        task_id,
                        delete_files: request.delete_files,
                    }),
            );
        }
    }
    (batches, rest)
}

/// 删除进度：每块收到回复（成功或失败）后回调一次。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeletionProgress {
    pub(crate) total: usize,
    /// 已收到回复的任务数（按块累计，不含结果不确定的块）。
    pub(crate) done: usize,
}

/// 一次分块删除的结果；以 id 记录不确定项，由调用方按当前任务列表落实最终数字。
#[derive(Debug, Default)]
pub(crate) struct DeletionReport {
    total: usize,
    /// daemon 确认整块成功的任务数。
    acknowledged: usize,
    /// 回复了错误的块里的任务：daemon 可能已删掉其中一部分。
    failed_ids: Vec<String>,
    /// 没有拿到回复的在途块里的任务（连接中断 / 取消时放弃）。
    unconfirmed_ids: Vec<String>,
    /// 取消或中断后未下发的任务数。
    not_sent: usize,
    cancelled: bool,
    first_error: Option<RpcErrorData>,
}

/// 按任务列表现状落实的数字。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeletionTally {
    pub(crate) deleted: usize,
    /// 收到错误回复且仍在列表里的任务。
    pub(crate) failed: usize,
    /// 未下发或结果无法确认、且仍在列表里的任务。
    pub(crate) unfinished: usize,
}

impl DeletionReport {
    #[must_use]
    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// 汇总：被确认的整块算已删除；出错 / 不确定的块里的任务以 `is_gone` 判断是否已不在列表。
    #[must_use]
    pub(crate) fn tally(&self, is_gone: impl Fn(&str) -> bool) -> DeletionTally {
        let count_gone = |ids: &[String]| ids.iter().filter(|id| is_gone(id)).count();
        let failed_gone = count_gone(&self.failed_ids);
        let unconfirmed_gone = count_gone(&self.unconfirmed_ids);
        DeletionTally {
            deleted: self.acknowledged + failed_gone + unconfirmed_gone,
            failed: self.failed_ids.len() - failed_gone,
            unfinished: self.not_sent + self.unconfirmed_ids.len() - unconfirmed_gone,
        }
    }

    /// 汇总提示：成功 = 「已删除 N 个任务」；取消 = 「已取消删除：已删除 X / 共 N」；
    /// 有失败 = 「已删除 X / 共 N，Y 个失败: 原因」（一个也没删掉时只给原因）。
    #[must_use]
    pub(crate) fn notice(
        &self,
        tally: DeletionTally,
        strings: &DownloadStrings,
        translator: &Translator,
    ) -> SubmitNotice {
        let reason = self
            .first_error
            .as_ref()
            .map(|error| error_text(translator, error));
        let remaining = tally.failed + tally.unfinished;
        match (self.cancelled, reason) {
            (true, None) => SubmitNotice {
                ok: true,
                message: strings.format_delete_cancelled(tally.deleted, self.total),
            },
            (true, Some(reason)) => SubmitNotice {
                ok: false,
                message: format!(
                    "{}: {reason}",
                    strings.format_delete_cancelled(tally.deleted, self.total)
                ),
            },
            (false, None) => SubmitNotice {
                ok: true,
                message: strings.format_deleted_count(tally.deleted),
            },
            (false, Some(reason)) if tally.deleted == 0 => SubmitNotice {
                ok: false,
                message: reason,
            },
            (false, Some(reason)) => SubmitNotice {
                ok: false,
                message: format!(
                    "{}: {reason}",
                    strings.format_delete_partial(tally.deleted, self.total, remaining)
                ),
            },
        }
    }

    fn record_error(&mut self, error: RpcErrorData) {
        if self.first_error.is_none() {
            self.first_error = Some(error);
        }
    }
}

/// 一块的最终结局。
enum ChunkEnd {
    /// 拿到了回复（重发用尽的不确定错误也在此，由调用方判定是否中断）。
    Replied(Result<(), RpcErrorData>),
    /// 被取消：收尾时间内拿到回复则带上，否则该块是否生效未知。
    Cancelled(Option<Result<(), RpcErrorData>>),
}

fn acknowledged(result: Result<DownloadsResult, RpcErrorData>) -> Result<(), RpcErrorData> {
    result.map(|_| ())
}

/// 删除一块；`Unavailable` 时按 [`retry_delay`] 退避后重发同一块（幂等）。
async fn delete_chunk<E, S, SF>(
    task_ids: &[String],
    delete_files: bool,
    execute: &E,
    sleep: &S,
    cancel: &CancelToken,
) -> ChunkEnd
where
    E: Fn(DownloadsCommand) -> PortFuture<DownloadsResult>,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
{
    let mut retries = 0_u32;
    loop {
        let mut future = execute(DownloadsCommand::DeleteMany {
            task_ids: task_ids.to_vec(),
            delete_files,
        });
        let result = match settle(&mut future, cancel, sleep).await {
            Settled::Finished(result) => acknowledged(result),
            Settled::Cancelled(settled) => return ChunkEnd::Cancelled(settled.map(acknowledged)),
        };
        let Err(error) = result else {
            return ChunkEnd::Replied(Ok(()));
        };
        let Some(delay) = retry_delay(&error, retries) else {
            return ChunkEnd::Replied(Err(error));
        };
        retries += 1;
        let mut wait = Box::pin(sleep(delay));
        let mut cancelled = cancel.cancelled();
        if let Either::Right(()) = race(&mut wait, &mut cancelled).await {
            return ChunkEnd::Cancelled(None);
        }
    }
}

/// 按块删除全部任务，策略见模块文档。
pub(crate) async fn run_deletion<E, S, SF, P>(
    task_ids: &[String],
    delete_files: bool,
    execute: &E,
    sleep: &S,
    cancel: &CancelToken,
    progress: &mut P,
) -> DeletionReport
where
    E: Fn(DownloadsCommand) -> PortFuture<DownloadsResult>,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
    P: FnMut(&DeletionProgress),
{
    let total = task_ids.len();
    let mut report = DeletionReport {
        total,
        ..DeletionReport::default()
    };
    let mut sent = 0_usize;
    let mut replied = 0_usize;
    for chunk in task_ids.chunks(DELETE_CHUNK_SIZE) {
        if cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        sent += chunk.len();
        let stop = match delete_chunk(chunk, delete_files, execute, sleep, cancel).await {
            ChunkEnd::Replied(Ok(())) => {
                report.acknowledged += chunk.len();
                replied += chunk.len();
                false
            }
            // 重发用尽 / 超时：服务不可达，继续后面的块只会重复同样的等待。
            ChunkEnd::Replied(Err(error)) if is_uncertain(&error) => {
                report.unconfirmed_ids.extend_from_slice(chunk);
                report.record_error(error);
                true
            }
            ChunkEnd::Replied(Err(error)) => {
                report.failed_ids.extend_from_slice(chunk);
                report.record_error(error);
                replied += chunk.len();
                false
            }
            ChunkEnd::Cancelled(settled) => {
                report.cancelled = true;
                match settled {
                    Some(Ok(())) => {
                        report.acknowledged += chunk.len();
                        replied += chunk.len();
                    }
                    Some(Err(error)) if !is_uncertain(&error) => {
                        report.failed_ids.extend_from_slice(chunk);
                        report.record_error(error);
                        replied += chunk.len();
                    }
                    Some(Err(_)) | None => report.unconfirmed_ids.extend_from_slice(chunk),
                }
                true
            }
        };
        progress(&DeletionProgress {
            total,
            done: replied,
        });
        if stop {
            break;
        }
    }
    report.not_sent = total - sent;
    report
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        future::Future,
        task::{Context, Poll, Waker},
    };

    use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData};
    use fluxdown_ui_i18n::{I18nCatalog, I18nError};

    use super::*;
    use crate::batch::MAX_RETRIES;

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    fn ids(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("t{index}")).collect()
    }

    fn delete(id: &str, delete_files: bool) -> DownloadsCommand {
        DownloadsCommand::Delete {
            task_id: id.to_owned(),
            delete_files,
        }
    }

    fn ready(result: Result<(), RpcErrorData>) -> PortFuture<DownloadsResult> {
        Box::pin(std::future::ready(result.map(|()| DownloadsResult::Unit)))
    }

    fn unavailable() -> RpcErrorData {
        RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
    }

    fn internal() -> RpcErrorData {
        RpcErrorData::new(ApplicationErrorCode::Internal, false)
    }

    /// 记录每次调用的任务数；`script` 按调用序号给出结果（超出后成功）。
    fn run(
        total: usize,
        cancel: &CancelToken,
        script: impl Fn(usize, &CancelToken) -> Option<Result<(), RpcErrorData>>,
    ) -> (
        DeletionReport,
        Vec<usize>,
        Vec<DeletionProgress>,
        Vec<Duration>,
    ) {
        let calls = RefCell::new(Vec::new());
        let sleeps = RefCell::new(Vec::new());
        let mut progress_log = Vec::new();
        let task_ids = ids(total);
        let execute = |command: DownloadsCommand| {
            let DownloadsCommand::DeleteMany {
                task_ids,
                delete_files,
            } = command
            else {
                panic!("deletion must only send DeleteMany");
            };
            assert!(delete_files);
            let call = calls.borrow().len();
            calls.borrow_mut().push(task_ids.len());
            match script(call, cancel) {
                Some(result) => ready(result),
                None => Box::pin(std::future::pending()),
            }
        };
        let sleep = |delay: Duration| {
            sleeps.borrow_mut().push(delay);
            std::future::ready(())
        };
        let report = block_on(run_deletion(
            &task_ids,
            true,
            &execute,
            &sleep,
            cancel,
            &mut |progress: &DeletionProgress| progress_log.push(*progress),
        ));
        (
            report,
            calls.into_inner(),
            progress_log,
            sleeps.into_inner(),
        )
    }

    #[test]
    fn chunks_are_sent_in_order_with_progress_after_each_reply() {
        let (report, calls, progress, _) = run(
            DELETE_CHUNK_SIZE * 2 + 5,
            &CancelToken::default(),
            |_, _| Some(Ok(())),
        );
        assert_eq!(calls, [DELETE_CHUNK_SIZE, DELETE_CHUNK_SIZE, 5]);
        let total = DELETE_CHUNK_SIZE * 2 + 5;
        assert_eq!(
            progress,
            [
                DeletionProgress {
                    total,
                    done: DELETE_CHUNK_SIZE
                },
                DeletionProgress {
                    total,
                    done: DELETE_CHUNK_SIZE * 2
                },
                DeletionProgress { total, done: total },
            ]
        );
        assert_eq!(
            report.tally(|_| false),
            DeletionTally {
                deleted: total,
                failed: 0,
                unfinished: 0
            }
        );
        assert!(!report.cancelled());
    }

    #[test]
    fn empty_request_sends_nothing() {
        let (report, calls, progress, _) = run(0, &CancelToken::default(), |_, _| Some(Ok(())));
        assert!(calls.is_empty() && progress.is_empty());
        assert_eq!(report.total, 0);
    }

    #[test]
    fn cancel_stops_further_chunks_but_lets_the_in_flight_one_finish() {
        let cancel = CancelToken::default();
        let total = DELETE_CHUNK_SIZE * 3 + 7;
        let (report, calls, progress, _) = run(total, &cancel, |call, cancel| {
            if call == 1 {
                cancel.cancel();
            }
            Some(Ok(()))
        });
        assert_eq!(calls, [DELETE_CHUNK_SIZE, DELETE_CHUNK_SIZE]);
        assert_eq!(progress.len(), 2);
        assert!(report.cancelled());
        assert_eq!(
            report.tally(|_| false),
            DeletionTally {
                deleted: DELETE_CHUNK_SIZE * 2,
                failed: 0,
                unfinished: DELETE_CHUNK_SIZE + 7
            }
        );
    }

    #[test]
    fn cancelled_in_flight_chunk_without_reply_is_checked_against_the_list() {
        let cancel = CancelToken::default();
        let (report, calls, _, _) = run(DELETE_CHUNK_SIZE + 3, &cancel, |call, cancel| {
            if call == 0 {
                cancel.cancel();
                return None;
            }
            Some(Ok(()))
        });
        assert_eq!(calls, [DELETE_CHUNK_SIZE]);
        assert!(report.cancelled());
        // 没拿到回复的块：已从列表消失的算已删除，其余与未下发的一起算未完成。
        let gone = |id: &str| {
            id.trim_start_matches('t')
                .parse::<usize>()
                .is_ok_and(|n| n < 10)
        };
        assert_eq!(
            report.tally(gone),
            DeletionTally {
                deleted: 10,
                failed: 0,
                unfinished: DELETE_CHUNK_SIZE + 3 - 10
            }
        );
    }

    #[test]
    fn unavailable_chunk_is_retried_with_backoff_then_continues() {
        let (report, calls, _, sleeps) =
            run(DELETE_CHUNK_SIZE + 1, &CancelToken::default(), |call, _| {
                Some(if call < 2 { Err(unavailable()) } else { Ok(()) })
            });
        // 第一块被拒两次后成功，第二块照常。
        assert_eq!(
            calls,
            [DELETE_CHUNK_SIZE, DELETE_CHUNK_SIZE, DELETE_CHUNK_SIZE, 1]
        );
        assert_eq!(
            sleeps,
            [Duration::from_millis(200), Duration::from_millis(400)]
        );
        assert_eq!(report.tally(|_| false).deleted, DELETE_CHUNK_SIZE + 1);
        assert!(report.first_error.is_none());
    }

    #[test]
    fn exhausted_retries_abort_the_remaining_chunks() {
        let total = DELETE_CHUNK_SIZE * 3;
        let (report, calls, progress, _) = run(total, &CancelToken::default(), |call, _| {
            Some(if call == 0 {
                Ok(())
            } else {
                Err(unavailable())
            })
        });
        // 第一块成功；第二块首发 + 用尽的重发，之后不再发第三块。
        assert_eq!(calls.len(), 1 + 1 + MAX_RETRIES as usize);
        assert!(calls.iter().all(|count| *count == DELETE_CHUNK_SIZE));
        assert_eq!(progress.len(), 2);
        assert_eq!(progress[1].done, DELETE_CHUNK_SIZE);
        assert_eq!(
            report.tally(|_| false),
            DeletionTally {
                deleted: DELETE_CHUNK_SIZE,
                failed: 0,
                unfinished: DELETE_CHUNK_SIZE * 2
            }
        );
        assert!(!report.cancelled());
    }

    #[test]
    fn non_retryable_failure_marks_only_that_chunk_and_continues() {
        let total = DELETE_CHUNK_SIZE * 3;
        let (report, calls, progress, _) = run(total, &CancelToken::default(), |call, _| {
            Some(if call == 1 { Err(internal()) } else { Ok(()) })
        });
        assert_eq!(calls, [DELETE_CHUNK_SIZE; 3]);
        assert_eq!(progress.last().map(|progress| progress.done), Some(total));
        // daemon 保留了失败块里的任务：仍在列表里的算失败，其余（已被删掉的）算已删除。
        assert_eq!(
            report.tally(|id| id != "t300"),
            DeletionTally {
                deleted: total - 1,
                failed: 1,
                unfinished: 0
            }
        );
    }

    fn strings() -> Result<(DownloadStrings, Translator), I18nError> {
        let catalog = I18nCatalog::load_embedded()?;
        let translator = catalog.translator("en");
        Ok((DownloadStrings::from_translator(&translator), translator))
    }

    #[test]
    fn notice_covers_success_cancel_partial_and_total_failure() -> Result<(), I18nError> {
        let (strings, translator) = strings()?;
        let reason = error_text(&translator, &internal());

        let (ok, ..) = run(1000, &CancelToken::default(), |_, _| Some(Ok(())));
        let notice = ok.notice(ok.tally(|_| false), &strings, &translator);
        assert!(notice.ok);
        assert_eq!(notice.message, "Deleted 1,000 tasks");

        let cancel = CancelToken::default();
        let (cancelled, ..) = run(1000, &cancel, |call, cancel| {
            cancel.cancel();
            assert_eq!(call, 0);
            Some(Ok(()))
        });
        let notice = cancelled.notice(cancelled.tally(|_| false), &strings, &translator);
        assert!(notice.ok);
        assert_eq!(
            notice.message,
            format!("Deletion cancelled: deleted {DELETE_CHUNK_SIZE} of 1,000")
        );

        let (partial, ..) = run(600, &CancelToken::default(), |call, _| {
            Some(if call == 1 { Err(internal()) } else { Ok(()) })
        });
        let notice = partial.notice(partial.tally(|_| false), &strings, &translator);
        assert!(!notice.ok);
        assert_eq!(
            notice.message,
            format!(
                "Deleted {} of 600; {DELETE_CHUNK_SIZE} failed: {reason}",
                600 - DELETE_CHUNK_SIZE
            )
        );

        let (failed, ..) = run(10, &CancelToken::default(), |_, _| Some(Err(internal())));
        let notice = failed.notice(failed.tally(|_| false), &strings, &translator);
        assert!(!notice.ok);
        assert_eq!(notice.message, reason);
        Ok(())
    }

    #[test]
    fn split_groups_local_deletes_by_flag_and_keeps_small_requests_single() {
        let commands = vec![
            delete("a", false),
            DownloadsCommand::PauseAll,
            delete("b", false),
            delete("a", false),
            delete("c", true),
            delete("d", false),
        ];
        let (batches, rest) = split_local_deletes(commands, MIN_BATCH_DELETE);
        assert_eq!(
            batches,
            [DeletionRequest {
                task_ids: vec!["a".into(), "b".into(), "d".into()],
                delete_files: false
            }]
        );
        // 只剩一个的请求原样还原为单任务命令，其余命令不动。
        assert!(matches!(rest[0], DownloadsCommand::PauseAll));
        assert!(matches!(
            &rest[1],
            DownloadsCommand::Delete { task_id, delete_files: true } if task_id == "c"
        ));
        assert_eq!(rest.len(), 2);
    }
}
