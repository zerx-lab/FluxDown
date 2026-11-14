//! 批量导入：把一批链接按块（[`CREATE_CHUNK_SIZE`]）经 `daemon.task.createMany` 提交，语义与
//! GPUI `crates/downloads/src/submission.rs` 一致，供 Android / iOS 共用。
//!
//! - 每块一个全新的幂等键 `batchId`；结果不确定（`Unavailable` / `Timeout` / 连接中断）时用**同一个**
//!   `batchId` 指数退避（2 s / 4 s / 8 s）重发，最多 [`MAX_RETRIES`] 次；用尽后该块计失败，
//!   剩余分块不再发送（计「未提交」）。
//! - 取消在每块之前检查，并与在途分块竞速：在途块再给 [`CANCEL_GRACE`] 收尾，已落库的结果仍被计入，
//!   超时则按「未提交」计。已创建的任务保留（不撤销）。
//! - 进度每块结束 / 进入或离开重试等待时更新一次；UI 经 [`ImportHandle::next_update`] 拉取
//!   （只保留最新进度，最终结果不会被覆盖丢失）。

use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fluxdown_protocol::{
    ApplicationErrorCode, CategoryRouteDto, CreateTaskItem, CreateTaskOutcome, CreateTaskRequest,
    DaemonCreateTasksParams, DaemonCreateTasksResult, RpcErrorData,
};
use tokio::sync::{Mutex, watch};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::dto::CreateTaskRequestDto;
use crate::error::{ErrorCodeDto, FluxError, HostErrorDto};

/// 每次 `daemon.task.createMany` 的条目数（协议上限 512 的一半，单块事务与单次事件都足够小）。
pub const CREATE_CHUNK_SIZE: usize = 256;
/// 结果不确定时同一块的最大重发次数。
const MAX_RETRIES: u32 = 3;
/// 首次重发前的等待；之后每次翻倍（2 s、4 s、8 s）。
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
/// 取消后给在途分块的收尾时间：让已落库的结果回来，计数才准确。
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// 分块条数：客户端据此判断一次导入是否多块（只有多块才需要进度界面），不必另存一份常量。
#[uniffi::export]
#[must_use]
pub fn import_chunk_size() -> u32 {
    count(CREATE_CHUNK_SIZE)
}

/// 一条待导入的链接。
#[derive(Clone, Debug, Default, Eq, PartialEq, uniffi::Record)]
pub struct ImportItemDto {
    pub url: String,
    /// 空 = 从 URL / Content-Disposition 推断。
    pub file_name: String,
    /// `algo=hexhash`，空 = 跳过校验。
    pub checksum: String,
}

/// 共享同一份建任务选项的一组链接。`common` 里的 `url` / `file_name` / `checksum` 被忽略
/// （由条目决定）；分块不会跨组，一次导入可含多组（条目各带独立 Cookie / 请求头时按选项分组）。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct ImportBatchDto {
    pub common: CreateTaskRequestDto,
    pub items: Vec<ImportItemDto>,
}

/// 等待重发的状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ImportRetryDto {
    /// 即将进行的第几次重发（1 起）。
    pub attempt: u32,
    pub max: u32,
    /// 距重发的等待秒数。
    pub delay_secs: u32,
}

/// 导入进度。
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ImportProgressDto {
    pub total: u32,
    /// 已有结论（成功或失败）的条目数。
    pub done: u32,
    pub failed: u32,
    pub retry: Option<ImportRetryDto>,
}

/// 导入结果汇总。
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ImportSummaryDto {
    pub total: u32,
    /// 已创建（含 URL 去重命中的已有任务）。
    pub succeeded: u32,
    pub failed: u32,
    /// 取消 / 连接中断后放弃或结果无法确认的条目数。
    pub not_submitted: u32,
    /// 用户取消了导入（已创建的任务保留）。
    pub cancelled: bool,
    /// 首个失败原因（单条拒绝 / 整块失败），用于提示文案。
    pub first_error: Option<HostErrorDto>,
    /// 失败与未提交条目的序号（升序）：序号 = 传入的各组 `items` 按给定顺序拼接后的位置，
    /// 客户端据此把它们放回表单重试（已创建的不在其中）。
    pub unfinished: Vec<u32>,
}

/// 导入更新（拉取式）：先若干 `Progress`，最后一个必为 `Finished`。
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum ImportUpdateDto {
    Progress { progress: ImportProgressDto },
    Finished { summary: ImportSummaryDto },
}

/// 一次导入的句柄：拉取更新与取消。丢弃句柄不会取消导入。
#[derive(uniffi::Object)]
pub struct ImportHandle {
    cancel: CancellationToken,
    updates: Mutex<Cursor>,
}

struct Cursor {
    receiver: watch::Receiver<ImportUpdateDto>,
    finished: bool,
}

impl ImportHandle {
    /// 启动前的占位进度：首个 `next_update` 立即返回它（或已完成的结果）。
    pub(crate) fn new(
        total: usize,
        cancel: CancellationToken,
    ) -> (Self, watch::Sender<ImportUpdateDto>) {
        let (sender, mut receiver) = watch::channel(ImportUpdateDto::Progress {
            progress: ImportProgressDto {
                total: count(total),
                done: 0,
                failed: 0,
                retry: None,
            },
        });
        receiver.mark_changed();
        (
            Self {
                cancel,
                updates: Mutex::new(Cursor {
                    receiver,
                    finished: false,
                }),
            },
            sender,
        )
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl ImportHandle {
    /// 等待下一次更新；`Finished` 之后返回 `None`（运行时被强制关闭而没有结果时也是 `None`）。
    pub async fn next_update(&self) -> Option<ImportUpdateDto> {
        let mut cursor = self.updates.lock().await;
        if cursor.finished {
            return None;
        }
        if cursor.receiver.changed().await.is_err() {
            cursor.finished = true;
            return None;
        }
        let update = cursor.receiver.borrow_and_update().clone();
        if matches!(update, ImportUpdateDto::Finished { .. }) {
            cursor.finished = true;
        }
        Some(update)
    }
}

#[uniffi::export]
impl ImportHandle {
    /// 请求取消（幂等）：不再发送新的分块，在途分块至多再等 3 秒；已创建的任务保留。
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// 一块 `createMany` 的发送端（本机进程内 / 远端 `/rpc` 的实现见 `session.rs`）。
#[async_trait]
pub(crate) trait ChunkSender: Send + Sync {
    async fn send(
        &self,
        params: &DaemonCreateTasksParams,
    ) -> Result<DaemonCreateTasksResult, FluxError>;
}

/// 转成线协议的一组：`common` 的 `url` / `fileName` / `checksum` 必须为空。
pub(crate) struct ImportBatch {
    common: CreateTaskRequest,
    items: Vec<CreateTaskItem>,
    category_route: Option<CategoryRouteDto>,
}

impl From<ImportBatchDto> for ImportBatch {
    fn from(batch: ImportBatchDto) -> Self {
        Self::routed(batch, None)
    }
}

impl ImportBatch {
    /// 转成线协议的一组并按 `route` 决定保存目录与分类路由。
    fn routed(batch: ImportBatchDto, route: Option<&CategoryRouteDto>) -> Self {
        let (mut common, category_route) = crate::session::routed_request(batch.common, route);
        common.url.clear();
        common.file_name.clear();
        common.checksum.clear();
        Self {
            common,
            items: batch
                .items
                .into_iter()
                .map(|item| CreateTaskItem {
                    url: item.url,
                    file_name: item.file_name,
                    checksum: item.checksum,
                })
                .collect(),
            category_route,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// 结果不确定的错误：服务端可能已处理（连接中断 / 不可达也属此类，幂等键保证重发安全）。
fn is_uncertain(error: &FluxError) -> bool {
    match error {
        FluxError::Transport { .. } => true,
        FluxError::Rpc { code, .. } => {
            matches!(code, ErrorCodeDto::Unavailable | ErrorCodeDto::Timeout)
        }
        FluxError::Closed => false,
    }
}

/// 每块一个全新的幂等键：时间 + 进程内计数 + 随机数，跨进程 / 重启也不会撞键（≤ 128 字节）。
fn new_batch_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let random = rand::random::<u64>();
    format!("fd-{nanos:x}-{counter:x}-{random:016x}")
}

fn internal_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Internal, false)
}

/// 一次可取消执行的结局。
enum Settled<T> {
    /// 未取消，正常完成。
    Finished(T),
    /// 执行中被取消：收尾时间内完成则带结果，否则放弃该 future。
    Cancelled(Option<T>),
}

/// 与取消竞速；同时就绪时 future 优先（已落库的结果不被取消吞掉）。
async fn settle<F: Future>(
    mut future: Pin<&mut F>,
    cancel: &CancellationToken,
) -> Settled<F::Output> {
    tokio::select! {
        biased;
        output = future.as_mut() => return Settled::Finished(output),
        () = cancel.cancelled() => {}
    }
    tokio::select! {
        biased;
        output = future.as_mut() => Settled::Cancelled(Some(output)),
        () = sleep(CANCEL_GRACE) => Settled::Cancelled(None),
    }
}

/// 一块的最终结局。
enum ChunkEnd {
    /// 重发用尽（仍是不确定错误）也算已结算，由调用方放弃剩余分块。
    Settled(Result<DaemonCreateTasksResult, FluxError>),
    /// 被取消：收尾时间内拿到结果则带上，否则该块是否落库未知。
    Cancelled(Option<Result<DaemonCreateTasksResult, FluxError>>),
}

/// 提交一块并在结果不确定时用**同一个** `batchId` 指数退避重发（服务端按它幂等）。
async fn submit_chunk<S: ChunkSender + ?Sized>(
    sender: &S,
    params: &DaemonCreateTasksParams,
    cancel: &CancellationToken,
    mut on_retry: impl FnMut(Option<ImportRetryDto>),
) -> ChunkEnd {
    let mut attempt = 0_u32;
    loop {
        let mut call = pin!(sender.send(params));
        let result = match settle(call.as_mut(), cancel).await {
            Settled::Finished(result) => result,
            Settled::Cancelled(settled) => return ChunkEnd::Cancelled(settled),
        };
        match result {
            Err(error) if is_uncertain(&error) && attempt < MAX_RETRIES => {
                let delay = RETRY_BASE_DELAY * (1_u32 << attempt);
                attempt += 1;
                on_retry(Some(ImportRetryDto {
                    attempt,
                    max: MAX_RETRIES,
                    delay_secs: u32::try_from(delay.as_secs()).unwrap_or(u32::MAX),
                }));
                tokio::select! {
                    biased;
                    () = sleep(delay) => {}
                    () = cancel.cancelled() => return ChunkEnd::Cancelled(None),
                }
                on_retry(None);
            }
            other => return ChunkEnd::Settled(other),
        }
    }
}

/// 提交过程中累计的状态。条目序号 = 全部分组条目按给定顺序拼接后的位置。
#[derive(Default)]
struct Run {
    succeeded: usize,
    failed: usize,
    not_submitted: usize,
    cancelled: bool,
    first_error: Option<HostErrorDto>,
    /// 失败与未提交条目的序号（升序）。
    unfinished: Vec<u32>,
}

impl Run {
    fn done(&self) -> usize {
        self.succeeded + self.failed
    }

    fn mark_unfinished(&mut self, start: usize, len: usize) {
        self.unfinished.extend((start..start + len).map(count));
    }

    /// `start..start + len` 整段失败。
    fn fail_range(&mut self, start: usize, len: usize, error: HostErrorDto) {
        self.failed += len;
        self.first_error.get_or_insert(error);
        self.mark_unfinished(start, len);
    }

    /// `start..start + len` 未提交 / 无法确认。
    fn skip_range(&mut self, start: usize, len: usize) {
        self.not_submitted += len;
        self.mark_unfinished(start, len);
    }

    /// 一块 `createMany` 的逐条结果（块首序号 `start`）；缺失或空结果按内部错误计失败。
    fn record_outcomes(&mut self, start: usize, expected: usize, result: DaemonCreateTasksResult) {
        let mut outcomes = result.results.into_iter();
        for offset in 0..expected {
            match outcomes.next() {
                Some(CreateTaskOutcome {
                    task_id: Some(_), ..
                }) => self.succeeded += 1,
                Some(CreateTaskOutcome {
                    error: Some(error), ..
                }) => self.fail_range(start + offset, 1, error.into()),
                _ => self.fail_range(start + offset, 1, internal_error().into()),
            }
        }
    }

    fn summary(self, total: usize) -> ImportSummaryDto {
        ImportSummaryDto {
            total: count(total),
            succeeded: count(self.succeeded),
            failed: count(self.failed),
            not_submitted: count(self.not_submitted),
            cancelled: self.cancelled,
            first_error: self.first_error,
            unfinished: self.unfinished,
        }
    }
}

/// 按块提交全部条目。取消 / 重发用尽后放弃剩余分块，已创建的任务保留。
pub(crate) async fn run_import<S: ChunkSender + ?Sized>(
    sender: &S,
    batches: Vec<ImportBatch>,
    cancel: &CancellationToken,
    progress: impl Fn(ImportProgressDto),
) -> ImportSummaryDto {
    let total: usize = batches.iter().map(ImportBatch::len).sum();
    let mut run = Run::default();
    let mut remaining = total;
    'batches: for batch in batches {
        let ImportBatch {
            common,
            items,
            category_route,
        } = batch;
        for chunk in items.chunks(CREATE_CHUNK_SIZE) {
            if cancel.is_cancelled() {
                run.cancelled = true;
                run.skip_range(total - remaining, remaining);
                break 'batches;
            }
            let chunk_len = chunk.len();
            let start = total - remaining;
            remaining = remaining.saturating_sub(chunk_len);
            let params = DaemonCreateTasksParams {
                batch_id: new_batch_id(),
                common: common.clone(),
                items: chunk.to_vec(),
                category_route: category_route.clone(),
            };
            let base = ImportProgressDto {
                total: count(total),
                done: count(run.done()),
                failed: count(run.failed),
                retry: None,
            };
            let end = submit_chunk(sender, &params, cancel, |retry| {
                progress(ImportProgressDto { retry, ..base });
            })
            .await;
            let stop = match end {
                ChunkEnd::Settled(Ok(result)) => {
                    run.record_outcomes(start, chunk_len, result);
                    false
                }
                ChunkEnd::Settled(Err(error)) => {
                    // 重发用尽仍不确定 / 会话已关闭：后面的块只会重复同样的等待或失败。
                    let stop = is_uncertain(&error) || matches!(error, FluxError::Closed);
                    run.fail_range(start, chunk_len, error.into());
                    stop
                }
                ChunkEnd::Cancelled(settled) => {
                    run.cancelled = true;
                    match settled {
                        Some(Ok(result)) => run.record_outcomes(start, chunk_len, result),
                        Some(Err(error)) if !is_uncertain(&error) => {
                            run.fail_range(start, chunk_len, error.into());
                        }
                        Some(Err(_)) | None => run.skip_range(start, chunk_len),
                    }
                    true
                }
            };
            progress(ImportProgressDto {
                total: count(total),
                done: count(run.done()),
                failed: count(run.failed),
                retry: None,
            });
            if stop {
                run.skip_range(total - remaining, remaining);
                break 'batches;
            }
        }
    }
    run.summary(total)
}

/// 启动导入：在 `runtime` 上运行，返回句柄。`route` 为已连接主机当前的分类路由，仅对
/// `common.offered_save_dir` 为 `Some` 的组生效（见 [`crate::session::routed_request`]）。
pub(crate) fn start(
    runtime: &tokio::runtime::Handle,
    sender: Arc<dyn ChunkSender>,
    batches: Vec<ImportBatchDto>,
    route: Option<&CategoryRouteDto>,
) -> Arc<ImportHandle> {
    let batches: Vec<ImportBatch> = batches
        .into_iter()
        .map(|batch| ImportBatch::routed(batch, route))
        .filter(|batch| !batch.items.is_empty())
        .collect();
    let total: usize = batches.iter().map(ImportBatch::len).sum();
    let cancel = CancellationToken::new();
    let (handle, updates) = ImportHandle::new(total, cancel.clone());
    runtime.spawn(async move {
        let summary = run_import(sender.as_ref(), batches, &cancel, |progress| {
            updates.send_replace(ImportUpdateDto::Progress { progress });
        })
        .await;
        updates.send_replace(ImportUpdateDto::Finished { summary });
    });
    Arc::new(handle)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::sync::PoisonError;

    use fluxdown_protocol::ErrorReason;

    use super::*;

    /// 脚本化回复：按调用顺序消费，用尽后默认「全部成功」。
    enum Reply {
        Created,
        Fail(FluxError),
        Items(Vec<CreateTaskOutcome>),
        /// 先取消、再永不回复。
        CancelThenHang,
        /// 先取消、挂起一次后返回全部成功（收尾时间内到达）。
        CancelThenCreated,
    }

    struct Fake {
        replies: StdMutex<VecDeque<Reply>>,
        calls: StdMutex<Vec<DaemonCreateTasksParams>>,
        cancel: CancellationToken,
    }

    impl Fake {
        fn new(replies: Vec<Reply>, cancel: &CancellationToken) -> Self {
            Self {
                replies: StdMutex::new(replies.into()),
                calls: StdMutex::new(Vec::new()),
                cancel: cancel.clone(),
            }
        }

        fn calls(&self) -> Vec<DaemonCreateTasksParams> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    fn created(params: &DaemonCreateTasksParams) -> DaemonCreateTasksResult {
        DaemonCreateTasksResult {
            results: params
                .items
                .iter()
                .map(|item| CreateTaskOutcome {
                    task_id: Some(format!("t-{}", item.url)),
                    error: None,
                })
                .collect(),
        }
    }

    #[async_trait]
    impl ChunkSender for Fake {
        async fn send(
            &self,
            params: &DaemonCreateTasksParams,
        ) -> Result<DaemonCreateTasksResult, FluxError> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(params.clone());
            let reply = self
                .replies
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or(Reply::Created);
            match reply {
                Reply::Created => Ok(created(params)),
                Reply::Fail(error) => Err(error),
                Reply::Items(results) => Ok(DaemonCreateTasksResult { results }),
                Reply::CancelThenHang => {
                    self.cancel.cancel();
                    std::future::pending().await
                }
                Reply::CancelThenCreated => {
                    self.cancel.cancel();
                    tokio::task::yield_now().await;
                    Ok(created(params))
                }
            }
        }
    }

    fn unavailable() -> FluxError {
        FluxError::rpc(ErrorCodeDto::Unavailable, "unavailable")
    }

    fn request(save_dir: &str) -> CreateTaskRequestDto {
        CreateTaskRequestDto {
            // 调用方误填的条目字段必须被清掉：`common` 里不允许带 url / 文件名 / 校验值。
            url: "https://ignored/".to_owned(),
            file_name: "ignored.bin".to_owned(),
            save_dir: save_dir.to_owned(),
            segments: 4,
            queue_id: String::new(),
            start_paused: true,
            cookies: String::new(),
            referrer: String::new(),
            user_agent: String::new(),
            proxy_url: String::new(),
            checksum: "sha256=00".to_owned(),
            ignore_tls_errors: false,
            headers: std::collections::HashMap::new(),
            http_user: String::new(),
            http_password: String::new(),
            save_site_auth: false,
            unattended: false,
            offered_save_dir: None,
        }
    }

    fn batch(count: usize) -> ImportBatch {
        batch_named("a", count)
    }

    fn batch_named(prefix: &str, count: usize) -> ImportBatch {
        ImportBatchDto {
            common: request("/downloads"),
            items: (0..count)
                .map(|index| ImportItemDto {
                    url: format!("https://x/{prefix}{index}"),
                    file_name: String::new(),
                    checksum: String::new(),
                })
                .collect(),
        }
        .into()
    }

    async fn run(
        fake: &Fake,
        batches: Vec<ImportBatch>,
    ) -> (ImportSummaryDto, Vec<ImportProgressDto>) {
        let seen = StdMutex::new(Vec::new());
        let summary = run_import(fake, batches, &fake.cancel, |progress| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(progress);
        })
        .await;
        (
            summary,
            seen.into_inner().unwrap_or_else(PoisonError::into_inner),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn chunks_split_at_256_with_unique_batch_ids_and_shared_options() {
        for (count, expected) in [
            (1_usize, vec![1_usize]),
            (255, vec![255]),
            (256, vec![256]),
            (257, vec![256, 1]),
            (512, vec![256, 256]),
            (10_000, [vec![256; 39], vec![16]].concat()),
        ] {
            let fake = Fake::new(Vec::new(), &CancellationToken::new());
            let (summary, _) = run(&fake, vec![batch(count)]).await;
            let calls = fake.calls();
            let sizes: Vec<usize> = calls.iter().map(|call| call.items.len()).collect();
            assert_eq!(sizes, expected, "{count} items");
            let mut ids: Vec<&str> = calls.iter().map(|call| call.batch_id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), calls.len(), "batch ids must be unique");
            for call in &calls {
                assert!(
                    !call.batch_id.is_empty() && call.batch_id.len() <= 128,
                    "{}",
                    call.batch_id
                );
                assert_eq!(call.common.save_dir, "/downloads");
                assert_eq!(call.common.segments, 4);
                assert!(call.common.start_paused);
                assert!(
                    call.common.url.is_empty()
                        && call.common.file_name.is_empty()
                        && call.common.checksum.is_empty(),
                    "common must not carry item fields"
                );
            }
            assert_eq!(summary.total, count as u32);
            assert_eq!(summary.succeeded, count as u32);
            assert_eq!(summary.failed + summary.not_submitted, 0);
            assert!(!summary.cancelled);
            assert_eq!(summary.first_error, None);
            assert!(summary.unfinished.is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn chunks_never_span_batches_and_progress_counts_all_items() {
        let fake = Fake::new(Vec::new(), &CancellationToken::new());
        let (summary, progress) =
            run(&fake, vec![batch_named("a", 300), batch_named("b", 2)]).await;
        let sizes: Vec<usize> = fake.calls().iter().map(|call| call.items.len()).collect();
        assert_eq!(sizes, [256, 44, 2]);
        assert_eq!(summary.succeeded, 302);
        // 每块结束报告一次：done 单调递增，总数恒为 302。
        let done: Vec<u32> = progress.iter().map(|update| update.done).collect();
        assert_eq!(done, [256, 300, 302]);
        assert!(progress.iter().all(|update| update.total == 302));
    }

    #[tokio::test(start_paused = true)]
    async fn per_item_outcomes_feed_counts_and_first_error() {
        // 回复缺条目：按失败计，不能静默丢掉。
        let fake = Fake::new(vec![Reply::Items(Vec::new())], &CancellationToken::new());
        let (summary, _) = run(&fake, vec![batch(4)]).await;
        assert_eq!((summary.succeeded, summary.failed), (0, 4));
        assert_eq!(
            summary.first_error.map(|error| error.code),
            Some(ErrorCodeDto::Internal)
        );

        // 逐条结果：一条拒绝（带 reason / field），其余成功；首个错误保留 reason。
        let rejected = CreateTaskOutcome {
            task_id: None,
            error: Some(
                RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
                    .with_reason(ErrorReason::Unknown),
            ),
        };
        let ok = |id: &str| CreateTaskOutcome {
            task_id: Some(id.to_owned()),
            error: None,
        };
        let fake = Fake::new(
            vec![Reply::Items(vec![ok("t0"), rejected, ok("t2")])],
            &CancellationToken::new(),
        );
        let (summary, progress) = run(&fake, vec![batch(3)]).await;
        assert_eq!((summary.succeeded, summary.failed), (2, 1));
        assert_eq!(summary.not_submitted, 0);
        assert_eq!(summary.unfinished, [1]);
        assert!(!summary.cancelled);
        let error = summary.first_error.expect("first error");
        assert_eq!(error.code, ErrorCodeDto::InvalidArgument);
        assert!(!error.retryable);
        assert_eq!(
            progress.last(),
            Some(&ImportProgressDto {
                total: 3,
                done: 3,
                failed: 1,
                retry: None
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn definite_chunk_failure_continues_with_next_chunk() {
        let fake = Fake::new(
            vec![Reply::Fail(FluxError::rpc(
                ErrorCodeDto::InvalidArgument,
                "bad",
            ))],
            &CancellationToken::new(),
        );
        let (summary, _) = run(&fake, vec![batch(300)]).await;
        // 确定性失败（非不确定）：该块计失败，后面的块照常提交。
        assert_eq!(fake.calls().len(), 2);
        assert_eq!((summary.succeeded, summary.failed), (44, 256));
        assert_eq!(summary.not_submitted, 0);
        // 第一块（0..256）整块失败；第二块成功。
        assert_eq!(summary.unfinished, (0..256).collect::<Vec<u32>>());
    }

    #[tokio::test(start_paused = true)]
    async fn unfinished_indices_count_across_batches_in_given_order() {
        let rejected = CreateTaskOutcome {
            task_id: None,
            error: Some(RpcErrorData::new(
                ApplicationErrorCode::InvalidArgument,
                false,
            )),
        };
        let ok = CreateTaskOutcome {
            task_id: Some("t".to_owned()),
            error: None,
        };
        // 300 + 2 条：第三块（第二组）里第二条被拒 → 全局序号 301。
        let fake = Fake::new(
            vec![
                Reply::Created,
                Reply::Created,
                Reply::Items(vec![ok, rejected]),
            ],
            &CancellationToken::new(),
        );
        let (summary, _) = run(&fake, vec![batch_named("a", 300), batch_named("b", 2)]).await;
        assert_eq!((summary.succeeded, summary.failed), (301, 1));
        assert_eq!(summary.unfinished, [301]);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_resends_same_batch_id_with_backoff_and_stops_after_three() {
        let fake = Fake::new(
            vec![
                Reply::Fail(unavailable()),
                Reply::Fail(FluxError::transport("connection closed")),
                Reply::Fail(FluxError::rpc(ErrorCodeDto::Timeout, "timeout")),
                Reply::Fail(unavailable()),
            ],
            &CancellationToken::new(),
        );
        let started = tokio::time::Instant::now();
        let (summary, progress) = run(&fake, vec![batch(600)]).await;
        let calls = fake.calls();
        // 首发 + 3 次重发，全部同一个幂等键；之后放弃，不再尝试后续分块。
        assert_eq!(calls.len(), 4);
        assert!(calls.iter().all(|call| call.batch_id == calls[0].batch_id));
        assert_eq!(started.elapsed(), Duration::from_secs(2 + 4 + 8));
        let retries: Vec<ImportRetryDto> =
            progress.iter().filter_map(|update| update.retry).collect();
        assert_eq!(
            retries,
            [
                ImportRetryDto {
                    attempt: 1,
                    max: 3,
                    delay_secs: 2
                },
                ImportRetryDto {
                    attempt: 2,
                    max: 3,
                    delay_secs: 4
                },
                ImportRetryDto {
                    attempt: 3,
                    max: 3,
                    delay_secs: 8
                },
            ]
        );
        assert_eq!((summary.succeeded, summary.failed), (0, 256));
        assert_eq!(summary.not_submitted, 600 - 256);
        // 失败的第一块 + 放弃的其余分块：全部 600 条都还没完成。
        assert_eq!(summary.unfinished, (0..600).collect::<Vec<u32>>());
        assert!(!summary.cancelled);
        assert_eq!(
            summary.first_error.map(|error| error.code),
            Some(ErrorCodeDto::Unavailable)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_that_succeeds_counts_the_chunk_once() {
        let fake = Fake::new(
            vec![Reply::Fail(unavailable()), Reply::Fail(unavailable())],
            &CancellationToken::new(),
        );
        let (summary, progress) = run(&fake, vec![batch(3)]).await;
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|call| call.batch_id == calls[0].batch_id));
        assert_eq!((summary.succeeded, summary.failed), (3, 0));
        assert!(progress.iter().any(|update| update.retry.is_some()));
        // 离开重试等待时回报 retry = None，最后一条是已完成的进度。
        assert_eq!(
            progress.last(),
            Some(&ImportProgressDto {
                total: 3,
                done: 3,
                failed: 0,
                retry: None
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn closed_session_stops_remaining_chunks() {
        let fake = Fake::new(
            vec![Reply::Fail(FluxError::Closed)],
            &CancellationToken::new(),
        );
        let (summary, _) = run(&fake, vec![batch(600)]).await;
        assert_eq!(fake.calls().len(), 1);
        assert_eq!((summary.succeeded, summary.failed), (0, 256));
        assert_eq!(summary.not_submitted, 600 - 256);
        assert_eq!(summary.unfinished, (0..600).collect::<Vec<u32>>());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_before_start_sends_nothing_and_reports_everything_unsubmitted() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let fake = Fake::new(Vec::new(), &cancel);
        let (summary, _) = run(&fake, vec![batch(300)]).await;
        assert!(fake.calls().is_empty());
        assert!(summary.cancelled);
        assert_eq!(summary.not_submitted, 300);
        assert_eq!(summary.unfinished, (0..300).collect::<Vec<u32>>());
        assert_eq!(summary.succeeded, 0);
        assert_eq!(summary.total, 300);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_between_chunks_keeps_confirmed_tasks_and_stops_further_chunks() {
        let cancel = CancellationToken::new();
        let fake = Fake::new(Vec::new(), &cancel);
        let summary = run_import(&fake, vec![batch(700)], &cancel, |progress| {
            if progress.done >= 256 {
                cancel.cancel();
            }
        })
        .await;
        assert_eq!(fake.calls().len(), 1);
        assert!(summary.cancelled);
        assert_eq!(summary.succeeded, 256);
        assert_eq!(summary.not_submitted, 700 - 256);
        // 第一块已创建：只剩 256 之后的条目。
        assert_eq!(summary.unfinished, (256..700).collect::<Vec<u32>>());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_chunk_waits_for_in_flight_result_so_counts_stay_accurate() {
        let cancel = CancellationToken::new();
        let fake = Fake::new(vec![Reply::CancelThenCreated], &cancel);
        let started = tokio::time::Instant::now();
        let (summary, _) = run(&fake, vec![batch(600)]).await;
        // 回复在收尾时间内到达：计入，随后不再发后续分块。
        assert!(started.elapsed() < CANCEL_GRACE);
        assert_eq!(fake.calls().len(), 1);
        assert!(summary.cancelled);
        assert_eq!(summary.succeeded, 256);
        assert_eq!(summary.not_submitted, 600 - 256);
        assert_eq!(summary.unfinished, (256..600).collect::<Vec<u32>>());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_chunk_drops_unsettled_request_after_grace_and_counts_it_unsubmitted() {
        let cancel = CancellationToken::new();
        let fake = Fake::new(vec![Reply::CancelThenHang], &cancel);
        let started = tokio::time::Instant::now();
        let (summary, _) = run(&fake, vec![batch(600)]).await;
        assert_eq!(started.elapsed(), CANCEL_GRACE);
        assert_eq!(fake.calls().len(), 1);
        assert!(summary.cancelled);
        assert_eq!((summary.succeeded, summary.failed), (0, 0));
        assert_eq!(summary.not_submitted, 600);
        assert_eq!(summary.unfinished, (0..600).collect::<Vec<u32>>());
        assert_eq!(summary.first_error, None);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_retry_wait_stops_without_resending() {
        let cancel = CancellationToken::new();
        let fake = Fake::new(
            vec![Reply::Fail(unavailable()), Reply::Fail(unavailable())],
            &cancel,
        );
        let summary = run_import(&fake, vec![batch(10)], &cancel, |progress| {
            if progress.retry.is_some() {
                cancel.cancel();
            }
        })
        .await;
        assert_eq!(fake.calls().len(), 1);
        assert!(summary.cancelled);
        assert_eq!(summary.not_submitted, 10);
        assert_eq!(summary.unfinished, (0..10).collect::<Vec<u32>>());
        assert_eq!((summary.succeeded, summary.failed), (0, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn handle_streams_progress_then_finished_and_then_ends() {
        let runtime = tokio::runtime::Handle::current();
        let cancel_probe = CancellationToken::new();
        let fake = Arc::new(Fake::new(Vec::new(), &cancel_probe));
        let handle = start(
            &runtime,
            fake.clone(),
            vec![ImportBatchDto {
                common: request("/downloads"),
                items: (0..300)
                    .map(|index| ImportItemDto {
                        url: format!("https://x/{index}"),
                        ..ImportItemDto::default()
                    })
                    .collect(),
            }],
            None,
        );
        let mut last_done = 0;
        let summary = loop {
            match handle.next_update().await.expect("update before Finished") {
                ImportUpdateDto::Progress { progress } => {
                    assert_eq!(progress.total, 300);
                    assert!(progress.done >= last_done, "progress never goes backwards");
                    last_done = progress.done;
                }
                ImportUpdateDto::Finished { summary } => break summary,
            }
        };
        assert_eq!(summary.succeeded, 300);
        assert_eq!(handle.next_update().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn handle_cancel_reaches_the_runner() {
        let runtime = tokio::runtime::Handle::current();
        let fake = Arc::new(Fake::new(Vec::new(), &CancellationToken::new()));
        let handle = start(
            &runtime,
            fake.clone(),
            vec![ImportBatchDto {
                common: request("/downloads"),
                items: (0..700)
                    .map(|index| ImportItemDto {
                        url: format!("https://x/{index}"),
                        ..ImportItemDto::default()
                    })
                    .collect(),
            }],
            None,
        );
        handle.cancel();
        handle.cancel();
        let summary = loop {
            if let ImportUpdateDto::Finished { summary } =
                handle.next_update().await.expect("update before Finished")
            {
                break summary;
            }
        };
        // 取消先于首块：什么都没发。
        assert!(summary.cancelled);
        assert_eq!(summary.not_submitted, 700);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn empty_import_finishes_immediately() {
        let runtime = tokio::runtime::Handle::current();
        let fake = Arc::new(Fake::new(Vec::new(), &CancellationToken::new()));
        let handle = start(
            &runtime,
            fake,
            vec![ImportBatchDto {
                common: request("/d"),
                items: Vec::new(),
            }],
            None,
        );
        // 首个更新可能是占位进度，随后必为 Finished（全 0）。
        let summary = loop {
            if let ImportUpdateDto::Finished { summary } =
                handle.next_update().await.expect("update before Finished")
            {
                break summary;
            }
        };
        assert_eq!(summary.total, 0);
        assert!(!summary.cancelled);
    }

    #[test]
    fn offered_save_dir_routes_the_batch_with_the_host_category_route() {
        let route = CategoryRouteDto {
            rules: Vec::new(),
            other_dir: "/other".to_owned(),
            ..Default::default()
        };
        let dto = |offered: Option<&str>, save_dir: &str| {
            let mut common = request(save_dir);
            common.offered_save_dir = offered.map(str::to_owned);
            ImportBatchDto {
                common,
                items: vec![ImportItemDto::default()],
            }
        };
        let routed = ImportBatch::routed(dto(Some("/default"), "/other"), Some(&route));
        assert_eq!(routed.common.save_dir, "/default");
        assert_eq!(routed.category_route.as_ref(), Some(&route));

        let chosen = ImportBatch::routed(dto(Some("/default"), "/chosen"), Some(&route));
        assert_eq!(chosen.common.save_dir, "/chosen");
        assert_eq!(chosen.category_route, None);

        let unrouted = ImportBatch::routed(dto(None, "/other"), Some(&route));
        assert_eq!(unrouted.common.save_dir, "/other");
        assert_eq!(unrouted.category_route, None);
    }
}
