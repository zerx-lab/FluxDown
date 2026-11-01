//! 「新建下载」表单确认后的提交：本机建任务 / 外部捕获确认 / 种子上传，或把链接下发到其他设备。
//!
//! 纯规则（命令拆分、分块、重试、取消、结果汇总、提示文案）集中在这里，主窗口下载页与无主窗口时的
//! 宿主直提交共用同一条路径，成功 / 失败提示不会因入口不同而分叉。
//!
//! 普通链接按 [`CREATE_CHUNK_SIZE`] 分块，每块一次幂等的 `daemon.task.createMany`；运行时无关
//! （执行、睡眠、进度都由调用方注入），取消与进度见 [`SubmissionHooks`]。

use std::{
    future::Future,
    hash::{BuildHasher as _, Hasher as _, RandomState},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fluxdown_protocol::{
    ApplicationErrorCode, CaptureResolveParams, CategoryRouteDto, CreateTaskItem,
    CreateTaskOutcome, CreateTaskRequest, DaemonCreateTasksParams, DaemonCreateTasksResult,
    LinkDispatchParams, RemoteDispatchParams, RpcErrorData,
};
use fluxdown_ui_i18n::Translator;

use crate::{
    controller::{
        DownloadsCommand, DownloadsResult, LAST_DOWNLOAD_TARGET_PREF, LAST_SAVE_DIR_PREF,
        PortFuture,
    },
    model::{devices::DispatchTarget, dispatch::DispatchSummary},
    strings::{NewDownloadStrings, error_text},
};

/// 每次 `daemon.task.createMany` 的条目数（协议上限 512 的一半，单块事务与单次事件都足够小）。
pub const CREATE_CHUNK_SIZE: usize = 256;
/// 结果不确定（`Unavailable` / `Timeout`）时同一块的最大重发次数。
const MAX_RETRIES: u32 = 3;
/// 首次重发前的等待；之后每次翻倍（2 s、4 s、8 s）。
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
/// 取消后给在途分块的收尾时间：让已落库的结果回来，计数才准确。
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// 外部捕获条目的确认：事务 id + 表单产出的建任务参数。
#[derive(Clone, Debug)]
pub struct CapturedTask {
    pub transaction_id: String,
    pub request: CreateTaskRequest,
}

/// 下发到其他设备的一条链接。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteItem {
    pub url: String,
    /// 空 / `None` = 由目标设备按 URL 推断。
    pub file_name: Option<String>,
}

/// 下发到其他设备的提交内容：目标只接收链接 / 文件名 / 保存目录，Cookie、请求头、代理、
/// 分段、队列等只对本机有意义的选项不随下发。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSubmission {
    /// 必须是云设备或已配对设备（`Local` 走本机提交）。
    pub target: DispatchTarget,
    /// 提示文案用的目标显示名。
    pub device_label: String,
    /// 云设备当前离线（下发被云端排队，上线后才执行）。
    pub target_offline: bool,
    pub items: Vec<RemoteItem>,
    /// 目标设备上的保存目录；`None` = 目标设备默认目录。
    pub save_dir: Option<String>,
    /// 已随本次下发处理的外部捕获事务：链接经下发送达，agent 里对应的待确认捕获据此忽略清除。
    pub captures: Vec<String>,
}

/// 表单里与种子文件相关的提交选项（种子文件本身不带这些）。空串 = 由 agent 用默认值。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TorrentFileOptions {
    pub save_dir: String,
    pub queue_id: String,
    pub start_paused: bool,
}

/// 表单确认后的提交内容。
#[derive(Clone, Debug)]
pub enum NewDownloadSubmission {
    /// 一份共享选项 + 逐条链接：普通链接按块 `daemon.task.createMany`，外部捕获条目逐条
    /// `agent.capture.resolve` 确认（捕获带完整请求）。
    Tasks {
        /// 整批共享的选项，`url` / `file_name` / `checksum` 必须为空（由条目决定）。
        common: Box<CreateTaskRequest>,
        items: Vec<CreateTaskItem>,
        captures: Vec<CapturedTask>,
        /// 本机分类目录路由；`None` = 不路由（没有分类配置目录）。
        category_route: Option<CategoryRouteDto>,
        /// 表单初始保存目录（未做任何分类预填），用于判定用户是否改过目录。
        offered_save_dir: String,
    },
    /// 预解析清单确认后只建一个组；捕获事务由 agent 合并并消费。
    Group {
        request: Box<fluxdown_protocol::CreateGroupRequest>,
        context: Box<CreateTaskRequest>,
        transaction_id: Option<String>,
    },
    /// 本机 `.torrent` 文件，交给 agent 读取上传；用户主动打开，走 BT 文件选择，
    /// 并带上表单当前的保存目录 / 队列 / 开始状态。
    TorrentFiles {
        paths: Vec<PathBuf>,
        options: TorrentFileOptions,
    },
    /// 逐条下发到云账号其他设备 / 已配对设备。
    Remote(RemoteSubmission),
}

/// 整批共享选项 + 待提交条目。
struct TaskBatch {
    common: CreateTaskRequest,
    items: Vec<CreateTaskItem>,
    /// 本机分类目录路由：随整批发给 daemon，由引擎按文件名 / 类型落入分类目录。
    category_route: Option<CategoryRouteDto>,
}

/// 表单提交的保存目录仍跟随默认（空 / 初始目录 / 某个分类目录）时，给整批挂上分类路由，
/// 并把 `save_dir` 改为未命中分类时使用的基础目录；用户另选了目录则不路由。
fn route_for_batch(
    common: &mut CreateTaskRequest,
    route: Option<CategoryRouteDto>,
    offered_save_dir: &str,
) -> Option<CategoryRouteDto> {
    let route = route?;
    if !route.follows(&common.save_dir, offered_save_dir) {
        return None;
    }
    common.save_dir = route.base_dir(&common.save_dir, offered_save_dir);
    Some(route)
}

/// 一次提交拆出的命令：分块批量建任务 + 逐条命令（计入结果），尽力而为的命令失败不影响结果。
struct SubmissionPlan {
    batch: Option<TaskBatch>,
    commands: Vec<DownloadsCommand>,
    best_effort: Vec<DownloadsCommand>,
    remote: Option<RemoteInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteInfo {
    device: String,
    offline: bool,
    /// 经局域网直连下发（一次性投递，进度要在目标设备上看）。
    direct: bool,
}

impl NewDownloadSubmission {
    /// 提交后任务立即开始（非「稍后下载」）；本机种子文件由 agent 直接开始，下发的任务
    /// 由目标设备接单开始。
    #[must_use]
    pub fn starts_immediately(&self) -> bool {
        match self {
            Self::Tasks {
                common,
                items,
                captures,
                ..
            } => {
                (items.is_empty() || !common.start_paused)
                    && captures.iter().all(|capture| !capture.request.start_paused)
            }
            Self::Group { request, .. } => !request.start_paused,
            Self::TorrentFiles { options, .. } => !options.start_paused,
            Self::Remote(_) => true,
        }
    }

    /// 链接数超过一块：需要多次 RPC，值得给出进度与取消入口。
    #[must_use]
    pub fn exceeds_one_chunk(&self) -> bool {
        matches!(self, Self::Tasks { items, .. } if items.len() > CREATE_CHUNK_SIZE)
    }

    /// 普通链接（`createMany` 条目）数。
    #[must_use]
    pub fn task_count(&self) -> usize {
        match self {
            Self::Tasks { items, .. } => items.len(),
            Self::Group { .. } | Self::TorrentFiles { .. } | Self::Remote(_) => 0,
        }
    }

    fn target(&self) -> DispatchTarget {
        match self {
            Self::Remote(remote) => remote.target.clone(),
            Self::Tasks { .. } | Self::Group { .. } | Self::TorrentFiles { .. } => {
                DispatchTarget::Local
            }
        }
    }

    /// 「上次保存目录」偏好写入：只记本机目录（远端目录属于目标设备，不能污染本机默认）。
    fn remember_save_dir_command(&self) -> Option<DownloadsCommand> {
        let save_dir = match self {
            Self::Tasks {
                common,
                items,
                captures,
                ..
            } => {
                let source = if items.is_empty() {
                    &captures.first()?.request
                } else {
                    common.as_ref()
                };
                source.save_dir.clone()
            }
            Self::Group { request, .. } => request.save_dir.clone(),
            _ => return None,
        };
        Some(DownloadsCommand::SetLocalPreference {
            key: LAST_SAVE_DIR_PREF,
            value: serde_json::Value::String(save_dir),
        })
    }

    fn plan(self) -> SubmissionPlan {
        let mut best_effort = Vec::new();
        best_effort.extend(self.remember_save_dir_command());
        best_effort.push(DownloadsCommand::SetLocalPreference {
            key: LAST_DOWNLOAD_TARGET_PREF,
            value: serde_json::Value::String(self.target().to_pref()),
        });
        match self {
            Self::Tasks {
                common,
                items,
                captures,
                category_route,
                offered_save_dir,
            } => {
                let mut common = *common;
                let category_route =
                    route_for_batch(&mut common, category_route, &offered_save_dir);
                let disable_category_route = category_route.is_none();
                SubmissionPlan {
                    batch: (!items.is_empty()).then_some(TaskBatch {
                        common,
                        items,
                        category_route,
                    }),
                    commands: captures
                        .into_iter()
                        .map(|capture| {
                            DownloadsCommand::CaptureResolve(Box::new(CaptureResolveParams {
                                transaction_id: capture.transaction_id,
                                accepted: true,
                                disable_category_route,
                                request: Some(capture.request),
                            }))
                        })
                        .collect(),
                    best_effort,
                    remote: None,
                }
            }
            Self::Group {
                request,
                context,
                transaction_id,
            } => SubmissionPlan {
                batch: None,
                commands: vec![DownloadsCommand::CreateGroup {
                    request,
                    context,
                    transaction_id,
                }],
                best_effort,
                remote: None,
            },
            Self::TorrentFiles { paths, options } => SubmissionPlan {
                batch: None,
                commands: paths
                    .iter()
                    .map(|path| DownloadsCommand::SubmitTorrentFile {
                        path: path.display().to_string(),
                        silent: false,
                        save_dir: Some(options.save_dir.clone()).filter(|dir| !dir.is_empty()),
                        queue_id: Some(options.queue_id.clone()).filter(|id| !id.is_empty()),
                        start_paused: Some(options.start_paused),
                    })
                    .collect(),
                best_effort,
                remote: None,
            },
            Self::Remote(remote) => {
                let RemoteSubmission {
                    target,
                    device_label,
                    target_offline,
                    items,
                    save_dir,
                    captures,
                } = remote;
                let direct = matches!(target, DispatchTarget::Paired(_));
                let commands = items
                    .into_iter()
                    .filter_map(|item| match &target {
                        DispatchTarget::Cloud(device_id) => {
                            Some(DownloadsCommand::RemoteDispatch(RemoteDispatchParams {
                                to_device: device_id.clone(),
                                url: item.url,
                                file_name: item.file_name,
                                save_dir: save_dir.clone(),
                            }))
                        }
                        DispatchTarget::Paired(fingerprint) => {
                            Some(DownloadsCommand::LinkDispatch(LinkDispatchParams {
                                fingerprint: fingerprint.clone(),
                                url: item.url,
                                file_name: item.file_name,
                                save_dir: save_dir.clone(),
                            }))
                        }
                        DispatchTarget::Local => None,
                    })
                    .collect();
                // 捕获链接已经下发：忽略 agent 里对应的待确认事务，避免悬挂到超时。
                best_effort.extend(captures.into_iter().map(|transaction_id| {
                    DownloadsCommand::CaptureResolve(Box::new(CaptureResolveParams {
                        transaction_id,
                        accepted: false,
                        disable_category_route: true,
                        request: None,
                    }))
                }));
                SubmissionPlan {
                    batch: None,
                    commands,
                    best_effort,
                    remote: Some(RemoteInfo {
                        device: device_label,
                        offline: target_offline,
                        direct,
                    }),
                }
            }
        }
    }
}

/// 建任务类命令返回的 `{taskId}` / `{taskIds}` 才是本机新任务 ID；下发命令返回的是目标
/// 设备上的任务，不能当作本机任务（会误弹进度窗口）。
fn creates_local_task(command: &DownloadsCommand) -> bool {
    matches!(
        command,
        DownloadsCommand::Create(_)
            | DownloadsCommand::CaptureResolve(_)
            | DownloadsCommand::SubmitTorrentFile { .. }
            | DownloadsCommand::SeedLocal { .. }
    )
}

/// 结果不确定的错误：服务端可能已处理。
pub(crate) fn is_uncertain(error: &RpcErrorData) -> bool {
    matches!(
        error.code,
        ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout
    )
}

/// 提交取消信号，可克隆共享；取消后不再发起新的分块，在途分块只再等一小段时间。
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<CancelState>);

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CancelToken {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        let wakers = std::mem::take(&mut *lock(&self.0.wakers));
        for waker in wakers {
            waker.wake();
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) fn cancelled(&self) -> Cancelled<'_> {
        Cancelled(self)
    }
}

pub(crate) struct Cancelled<'a>(&'a CancelToken);

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let token = self.0;
        if token.is_cancelled() {
            return Poll::Ready(());
        }
        {
            let mut wakers = lock(&token.0.wakers);
            if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
                wakers.push(cx.waker().clone());
            }
        }
        // 登记与取消竞争：登记后再确认一次，避免错过唤醒。
        if token.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

pub(crate) enum Either<A, B> {
    Left(A),
    Right(B),
}

/// 同时轮询两个 future：先完成者胜出，同时就绪时左侧优先（已落库的结果不被取消吞掉）。
/// 落败者保持原样，可继续轮询。
pub(crate) async fn race<A, B>(left: &mut A, right: &mut B) -> Either<A::Output, B::Output>
where
    A: Future + Unpin,
    B: Future + Unpin,
{
    std::future::poll_fn(|cx| {
        if let Poll::Ready(value) = Pin::new(&mut *left).poll(cx) {
            return Poll::Ready(Either::Left(value));
        }
        if let Poll::Ready(value) = Pin::new(&mut *right).poll(cx) {
            return Poll::Ready(Either::Right(value));
        }
        Poll::Pending
    })
    .await
}

/// 一次可取消执行的结局。
pub(crate) enum Settled<T> {
    /// 未取消，正常完成。
    Finished(T),
    /// 执行中被取消：收尾时间内完成则带结果，否则放弃该 future（传输层会跳过未发出的命令）。
    Cancelled(Option<T>),
}

pub(crate) async fn settle<F, S, SF>(
    future: &mut F,
    cancel: &CancelToken,
    sleep: &S,
) -> Settled<F::Output>
where
    F: Future + Unpin,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
{
    let mut cancelled = cancel.cancelled();
    match race(future, &mut cancelled).await {
        Either::Left(output) => Settled::Finished(output),
        Either::Right(()) => {
            let mut grace = Box::pin(sleep(CANCEL_GRACE));
            match race(future, &mut grace).await {
                Either::Left(output) => Settled::Cancelled(Some(output)),
                Either::Right(()) => Settled::Cancelled(None),
            }
        }
    }
}

/// 等待重发时的状态（供界面显示）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryState {
    /// 即将进行的第几次重发（1 起）。
    pub attempt: u32,
    pub max: u32,
    /// 距重发的等待时长。
    pub delay: Duration,
}

/// 分块提交进度，每块结束 / 进入或离开重试等待时回调一次。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmissionProgress {
    pub total: usize,
    /// 已有结论（成功或失败）的条目数。
    pub done: usize,
    pub failed: usize,
    pub retry: Option<RetryState>,
}

/// 提交的运行时注入：取消信号、睡眠（重试退避 / 取消收尾）与进度回调。
/// 提交本身不依赖具体执行器，睡眠由调用方用所在执行器的定时器提供。
pub struct SubmissionHooks<S, P> {
    cancel: CancelToken,
    sleep: S,
    progress: P,
}

impl<S> SubmissionHooks<S, fn(&SubmissionProgress)> {
    #[must_use]
    pub fn new<SF>(sleep: S) -> Self
    where
        S: Fn(Duration) -> SF,
        SF: Future<Output = ()>,
    {
        Self {
            cancel: CancelToken::default(),
            sleep,
            progress: |_| {},
        }
    }
}

impl<S, P> SubmissionHooks<S, P> {
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    #[must_use]
    pub fn with_progress<Q>(self, progress: Q) -> SubmissionHooks<S, Q> {
        SubmissionHooks {
            cancel: self.cancel,
            sleep: self.sleep,
            progress,
        }
    }
}

/// 提交结果汇总。
#[derive(Debug)]
pub struct SubmissionReport {
    created_task_ids: Vec<String>,
    summary: DispatchSummary,
    remote: Option<RemoteInfo>,
    cancelled: bool,
    /// 未提交（取消 / 连接中断后放弃）或结果无法确认的条目数。
    not_submitted: usize,
}

/// 提交完成后给用户的提示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitNotice {
    /// `false` = 至少一条失败（用错误样式展示）。
    pub ok: bool,
    /// 空串 = 无需提示（全部新建成功：任务已出现在列表中，不再弹 toast 遮挡列表）。
    pub message: String,
}

impl SubmissionReport {
    /// 本机新建出的任务 ID（下发不产生本机任务）。
    #[must_use]
    pub fn created_task_ids(&self) -> &[String] {
        &self.created_task_ids
    }

    #[must_use]
    pub fn failed(&self) -> bool {
        self.summary.has_failure()
    }

    /// 用户取消了导入（已创建的任务保留）。
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// 未提交或无法确认的条目数。
    #[must_use]
    pub fn not_submitted(&self) -> usize {
        self.not_submitted
    }

    /// 汇总提示：本机成功 = 「任务已添加」；取消 = 「已取消导入：已添加 X / 共 N」；
    /// 下发成功 / 部分成功 / 全部失败各自带目标设备名；失败附首个错误的原因文案
    ///（按 `reason`，缺失再按码）。
    #[must_use]
    pub fn notice(&self, translator: &Translator) -> SubmitNotice {
        let strings = NewDownloadStrings::from_translator(translator);
        let summary = &self.summary;
        let reason = summary
            .first_error
            .as_ref()
            .map(|error| error_text(translator, error));
        match (&self.remote, reason) {
            (Some(remote), None) => SubmitNotice {
                ok: true,
                message: strings.format_dispatched(
                    summary.succeeded,
                    &remote.device,
                    remote.offline,
                    remote.direct,
                ),
            },
            (Some(remote), Some(reason)) => SubmitNotice {
                ok: false,
                message: if summary.succeeded > 0 {
                    format!(
                        "{}: {reason}",
                        strings.format_partial(summary.succeeded, summary.failed, &remote.device)
                    )
                } else {
                    format!("{}: {reason}", strings.dispatch_failed)
                },
            },
            (None, reason) => self.local_notice(&strings, reason),
        }
    }

    fn local_notice(&self, strings: &NewDownloadStrings, reason: Option<String>) -> SubmitNotice {
        let added = self.summary.succeeded;
        let total = added + self.summary.failed + self.not_submitted;
        match (self.cancelled, reason) {
            (true, None) => SubmitNotice {
                ok: true,
                message: strings.format_import_cancelled(added, total),
            },
            (true, Some(reason)) => SubmitNotice {
                ok: false,
                message: format!(
                    "{}: {reason}",
                    strings.format_import_cancelled(added, total)
                ),
            },
            (false, None) => SubmitNotice {
                ok: true,
                message: String::new(),
            },
            (false, Some(reason)) if added == 0 => SubmitNotice {
                ok: false,
                message: reason,
            },
            (false, Some(reason)) => SubmitNotice {
                ok: false,
                message: format!(
                    "{}: {reason}",
                    strings.format_import_partial(added, total, self.summary.failed)
                ),
            },
        }
    }
}

/// 提交过程中累计的状态。
#[derive(Default)]
struct Run {
    summary: DispatchSummary,
    created_task_ids: Vec<String>,
    cancelled: bool,
    not_submitted: usize,
}

impl Run {
    fn record_command(&mut self, creates: bool, result: Result<DownloadsResult, RpcErrorData>) {
        self.summary.record(&result);
        if creates && let Ok(result) = &result {
            self.created_task_ids.extend(result.created_task_ids());
        }
    }

    /// 一块 `createMany` 的逐条结果；缺失或空结果按内部错误计失败。
    fn record_outcomes(&mut self, expected: usize, result: DaemonCreateTasksResult) {
        let mut outcomes = result.results.into_iter();
        for _ in 0..expected {
            match outcomes.next() {
                Some(CreateTaskOutcome {
                    task_id: Some(task_id),
                    ..
                }) => {
                    self.summary.record_succeeded(1);
                    self.created_task_ids.push(task_id);
                }
                Some(CreateTaskOutcome {
                    error: Some(error), ..
                }) => self.summary.record_failed(1, &error),
                _ => self.summary.record_failed(1, &internal_error()),
            }
        }
    }

    /// 不再执行的命令：外部捕获改为忽略，避免待确认事务悬挂到超时。
    fn skip(&mut self, command: &DownloadsCommand, best_effort: &mut Vec<DownloadsCommand>) {
        self.not_submitted += 1;
        best_effort.extend(capture_rejection(command));
    }
}

fn capture_rejection(command: &DownloadsCommand) -> Option<DownloadsCommand> {
    match command {
        DownloadsCommand::CaptureResolve(params) if params.accepted => Some(
            DownloadsCommand::CaptureResolve(Box::new(CaptureResolveParams {
                transaction_id: params.transaction_id.clone(),
                accepted: false,
                disable_category_route: true,
                request: None,
            })),
        ),
        _ => None,
    }
}

fn internal_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Internal, false)
}

/// 每块一个全新的幂等键：时间 + 进程内计数 + 随机哈希，跨进程 / 重启也不会撞键。
fn new_batch_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let random = RandomState::new().build_hasher().finish();
    format!("fd-{nanos:x}-{counter:x}-{random:016x}")
}

fn create_many_result(
    result: Result<DownloadsResult, RpcErrorData>,
) -> Result<DaemonCreateTasksResult, RpcErrorData> {
    match result? {
        DownloadsResult::CreateMany(result) => Ok(result),
        _ => Err(internal_error()),
    }
}

/// 一块的最终结局。
enum ChunkEnd {
    /// 重发用尽（仍是不确定错误）也算已结算，由调用方放弃剩余分块。
    Settled(Result<DaemonCreateTasksResult, RpcErrorData>),
    /// 被取消：收尾时间内拿到结果则带上，否则该块是否落库未知。
    Cancelled(Option<Result<DaemonCreateTasksResult, RpcErrorData>>),
}

/// 提交一块并在结果不确定时用**同一个** `batch_id` 指数退避重发（服务端按它幂等）。
async fn submit_chunk<E, S, SF>(
    params: &DaemonCreateTasksParams,
    execute: &E,
    sleep: &S,
    cancel: &CancelToken,
    on_retry: &mut impl FnMut(Option<RetryState>),
) -> ChunkEnd
where
    E: Fn(DownloadsCommand) -> PortFuture<DownloadsResult>,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
{
    let mut attempt = 0_u32;
    loop {
        let mut future = execute(DownloadsCommand::CreateMany(Box::new(params.clone())));
        let result = match settle(&mut future, cancel, sleep).await {
            Settled::Finished(result) => create_many_result(result),
            Settled::Cancelled(settled) => {
                return ChunkEnd::Cancelled(settled.map(create_many_result));
            }
        };
        match result {
            Err(error) if is_uncertain(&error) && attempt < MAX_RETRIES => {
                let delay = RETRY_BASE_DELAY * (1_u32 << attempt);
                attempt += 1;
                on_retry(Some(RetryState {
                    attempt,
                    max: MAX_RETRIES,
                    delay,
                }));
                let mut wait = Box::pin(sleep(delay));
                let mut cancelled = cancel.cancelled();
                if let Either::Right(()) = race(&mut wait, &mut cancelled).await {
                    return ChunkEnd::Cancelled(None);
                }
                on_retry(None);
            }
            other => return ChunkEnd::Settled(other),
        }
    }
}

/// 按块提交全部条目。取消 / 重发用尽后放弃剩余分块，已创建的任务保留。
async fn submit_batches<E, S, SF, P>(
    batch: TaskBatch,
    execute: &E,
    sleep: &S,
    cancel: &CancelToken,
    progress: &mut P,
    run: &mut Run,
) where
    E: Fn(DownloadsCommand) -> PortFuture<DownloadsResult>,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
    P: FnMut(&SubmissionProgress),
{
    let TaskBatch {
        common,
        items,
        category_route,
    } = batch;
    let total = items.len();
    let mut pending = items.into_iter();
    let mut remaining = total;
    while remaining > 0 {
        if cancel.is_cancelled() {
            run.cancelled = true;
            run.not_submitted += remaining;
            return;
        }
        let chunk: Vec<CreateTaskItem> = pending.by_ref().take(CREATE_CHUNK_SIZE).collect();
        let chunk_len = chunk.len();
        remaining = remaining.saturating_sub(chunk_len);
        let params = DaemonCreateTasksParams {
            batch_id: new_batch_id(),
            common: common.clone(),
            items: chunk,
            category_route: category_route.clone(),
        };
        let base = SubmissionProgress {
            total,
            done: run.summary.succeeded + run.summary.failed,
            failed: run.summary.failed,
            retry: None,
        };
        let end = submit_chunk(&params, execute, sleep, cancel, &mut |retry: Option<
            RetryState,
        >| {
            progress(&SubmissionProgress { retry, ..base });
        })
        .await;
        let stop = match end {
            ChunkEnd::Settled(Ok(result)) => {
                run.record_outcomes(chunk_len, result);
                false
            }
            ChunkEnd::Settled(Err(error)) => {
                run.summary.record_failed(chunk_len, &error);
                // 重发用尽仍不确定：服务不可达，继续后面的块只会重复同样的等待。
                is_uncertain(&error)
            }
            ChunkEnd::Cancelled(settled) => {
                run.cancelled = true;
                match settled {
                    Some(Ok(result)) => run.record_outcomes(chunk_len, result),
                    Some(Err(error)) if !is_uncertain(&error) => {
                        run.summary.record_failed(chunk_len, &error);
                    }
                    Some(Err(_)) | None => run.not_submitted += chunk_len,
                }
                true
            }
        };
        progress(&SubmissionProgress {
            total,
            done: run.summary.succeeded + run.summary.failed,
            failed: run.summary.failed,
            retry: None,
        });
        if stop {
            run.not_submitted += remaining;
            return;
        }
    }
}

/// 执行提交并汇总：普通链接分块 `createMany`（进度 / 取消 / 重发见 [`SubmissionHooks`]），
/// 其余命令顺序执行；偏好写入 / 捕获清理尽力而为，放在主命令之后，失败不影响结果也不计入汇总。
/// 取消只影响尚未发出或在途的主命令，已创建的任务保留。
pub async fn run_submission<E, S, SF, P>(
    submission: NewDownloadSubmission,
    execute: E,
    hooks: SubmissionHooks<S, P>,
) -> SubmissionReport
where
    E: Fn(DownloadsCommand) -> PortFuture<DownloadsResult>,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
    P: FnMut(&SubmissionProgress),
{
    let SubmissionHooks {
        cancel,
        sleep,
        mut progress,
    } = hooks;
    let SubmissionPlan {
        batch,
        commands,
        mut best_effort,
        remote,
    } = submission.plan();
    let mut run = Run::default();
    if let Some(batch) = batch {
        submit_batches(batch, &execute, &sleep, &cancel, &mut progress, &mut run).await;
    }
    let mut commands = commands.into_iter();
    for command in commands.by_ref() {
        if run.cancelled || cancel.is_cancelled() {
            run.cancelled = true;
            run.skip(&command, &mut best_effort);
            break;
        }
        let creates = creates_local_task(&command);
        let rejection = capture_rejection(&command);
        let mut future = execute(command);
        match settle(&mut future, &cancel, &sleep).await {
            Settled::Finished(result) => run.record_command(creates, result),
            Settled::Cancelled(settled) => {
                run.cancelled = true;
                match settled {
                    Some(result) => run.record_command(creates, result),
                    None => {
                        run.not_submitted += 1;
                        best_effort.extend(rejection);
                    }
                }
                break;
            }
        }
    }
    for command in commands {
        run.skip(&command, &mut best_effort);
    }
    for command in best_effort {
        if let Err(error) = execute(command).await {
            // 偏好和捕获清理不影响已创建任务，但失败需要诊断记录。
            eprintln!(
                "download submission best-effort command failed: {:?} ({:?})",
                error.code, error.reason
            );
        }
    }
    SubmissionReport {
        created_task_ids: run.created_task_ids,
        summary: run.summary,
        remote,
        cancelled: run.cancelled,
        not_submitted: run.not_submitted,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        future::Future,
        task::{Context, Poll, Waker},
        time::Duration,
    };

    use fluxdown_protocol::{ApplicationErrorCode, ErrorReason, RpcErrorData};
    use fluxdown_ui_i18n::{I18nCatalog, I18nError};

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    fn remote(target: DispatchTarget, urls: &[&str], save_dir: Option<&str>) -> RemoteSubmission {
        RemoteSubmission {
            target,
            device_label: "Mac".to_owned(),
            target_offline: false,
            items: urls
                .iter()
                .map(|url| RemoteItem {
                    url: (*url).to_owned(),
                    file_name: None,
                })
                .collect(),
            save_dir: save_dir.map(str::to_owned),
            captures: vec!["cap-1".to_owned()],
        }
    }

    #[test]
    fn remote_submission_dispatches_each_link_with_target_default_dir_omitted() {
        let plan = NewDownloadSubmission::Remote(remote(
            DispatchTarget::Cloud("dev-b".to_owned()),
            &["https://a/1", "https://a/2"],
            None,
        ))
        .plan();
        assert_eq!(plan.commands.len(), 2);
        for command in &plan.commands {
            let DownloadsCommand::RemoteDispatch(params) = command else {
                panic!("cloud target must use remote.dispatch");
            };
            assert_eq!(params.to_device, "dev-b");
            assert_eq!(params.save_dir, None);
        }
        // 偏好写入 + 捕获清理都是尽力而为，且没有污染本机保存目录偏好。
        assert!(plan.best_effort.iter().all(|command| !matches!(
            command,
            DownloadsCommand::SetLocalPreference { key, .. } if *key == LAST_SAVE_DIR_PREF
        )));
        assert!(plan.best_effort.iter().any(|command| matches!(
            command,
            DownloadsCommand::CaptureResolve(params) if !params.accepted && params.transaction_id == "cap-1"
        )));
        assert!(plan.best_effort.iter().any(|command| matches!(
            command,
            DownloadsCommand::SetLocalPreference { key, value }
                if *key == LAST_DOWNLOAD_TARGET_PREF && value == "cloud:dev-b"
        )));
    }

    #[test]
    fn paired_target_uses_link_dispatch_with_explicit_dir() {
        let plan = NewDownloadSubmission::Remote(remote(
            DispatchTarget::Paired("fp1".to_owned()),
            &["magnet:?xt=urn:btih:abc"],
            Some("/mnt/dl"),
        ))
        .plan();
        let [DownloadsCommand::LinkDispatch(params)] = plan.commands.as_slice() else {
            panic!("paired target must use link.dispatch");
        };
        assert_eq!(params.fingerprint, "fp1");
        assert_eq!(params.save_dir.as_deref(), Some("/mnt/dl"));
    }

    #[test]
    fn dispatch_summary_reports_partial_failure_and_never_yields_local_task_ids()
    -> Result<(), I18nError> {
        let catalog = std::sync::Arc::new(I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        let calls = RefCell::new(Vec::new());
        let report = block_on(run_submission(
            NewDownloadSubmission::Remote(remote(
                DispatchTarget::Cloud("dev-b".to_owned()),
                &["https://a/1", "https://a/2", "https://a/3"],
                None,
            )),
            |command| {
                let index = calls.borrow().len();
                calls
                    .borrow_mut()
                    .push(matches!(command, DownloadsCommand::RemoteDispatch(_)));
                Box::pin(async move {
                    if index == 1 {
                        Err(RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
                            .with_reason(ErrorReason::TargetDeviceOffline))
                    } else {
                        // 下发结果里的 `taskId` 属于目标设备，不能当本机任务。
                        Ok(DownloadsResult::Value(
                            serde_json::json!({"taskId": "remote-1"}),
                        ))
                    }
                })
            },
            hooks(),
        ));
        assert!(report.created_task_ids().is_empty());
        assert!(report.failed());
        let notice = report.notice(&translator);
        assert!(!notice.ok);
        assert!(notice.message.contains("Mac"), "{}", notice.message);
        assert!(
            notice
                .message
                .contains("The target device is offline, so the command can't be delivered."),
            "{}",
            notice.message
        );
        // 3 条下发 + 目标偏好 + 捕获清理均已执行。
        assert_eq!(calls.borrow().len(), 5);
        Ok(())
    }

    #[test]
    fn all_dispatched_offline_target_mentions_it_will_run_later() -> Result<(), I18nError> {
        let catalog = std::sync::Arc::new(I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        let mut submission = remote(
            DispatchTarget::Cloud("dev-b".to_owned()),
            &["https://a/1"],
            None,
        );
        submission.target_offline = true;
        let report = block_on(run_submission(
            NewDownloadSubmission::Remote(submission),
            |_| Box::pin(async { Ok(DownloadsResult::Unit) }),
            hooks(),
        ));
        let notice = report.notice(&translator);
        assert!(notice.ok);
        assert!(notice.message.contains("offline"), "{}", notice.message);
        Ok(())
    }

    #[test]
    fn direct_dispatch_toast_says_progress_is_on_the_target_device() -> Result<(), I18nError> {
        let catalog = std::sync::Arc::new(I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        let report = block_on(run_submission(
            NewDownloadSubmission::Remote(remote(
                DispatchTarget::Paired("fp1".to_owned()),
                &["https://a/1"],
                None,
            )),
            |_| Box::pin(async { Ok(DownloadsResult::Unit) }),
            hooks(),
        ));
        let notice = report.notice(&translator);
        assert!(notice.ok);
        assert_eq!(
            notice.message,
            translator.text_with("linkDispatchedToDevice", &[("device", "Mac")])
        );
        Ok(())
    }

    #[test]
    fn local_failure_shows_reason_text_and_success_collects_created_ids() -> Result<(), I18nError> {
        let catalog = std::sync::Arc::new(I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        let submission = tasks(1);
        let report = block_on(run_submission(
            submission.clone(),
            |command| created_all(&command),
            hooks(),
        ));
        assert_eq!(report.created_task_ids(), ["t-https://a/0"]);
        assert!(report.notice(&translator).ok);

        let report = block_on(run_submission(
            submission,
            |_| Box::pin(async { Err(RpcErrorData::new(ApplicationErrorCode::Unavailable, true)) }),
            hooks(),
        ));
        let notice = report.notice(&translator);
        assert!(!notice.ok);
        assert_eq!(notice.message, translator.text("localServiceDisconnected"));
        Ok(())
    }

    fn instant(_: Duration) -> std::future::Ready<()> {
        std::future::ready(())
    }

    type InstantHooks =
        SubmissionHooks<fn(Duration) -> std::future::Ready<()>, fn(&SubmissionProgress)>;

    fn hooks() -> InstantHooks {
        SubmissionHooks::new(instant)
    }

    fn blank_request(save_dir: &str) -> CreateTaskRequest {
        serde_json::from_value(serde_json::json!({"url": "", "saveDir": save_dir}))
            .expect("request fixture")
    }

    fn tasks(count: usize) -> NewDownloadSubmission {
        NewDownloadSubmission::Tasks {
            common: Box::new(blank_request("/downloads")),
            items: (0..count)
                .map(|index| CreateTaskItem {
                    url: format!("https://a/{index}"),
                    ..CreateTaskItem::default()
                })
                .collect(),
            captures: Vec::new(),
            category_route: None,
            offered_save_dir: String::new(),
        }
    }

    /// 每条都成功的 `createMany` 回复：任务 id 为 `t-<url>`。
    fn created_all(command: &DownloadsCommand) -> PortFuture<DownloadsResult> {
        let DownloadsCommand::CreateMany(params) = command else {
            return Box::pin(async { Ok(DownloadsResult::Unit) });
        };
        let results = params
            .items
            .iter()
            .map(|item| CreateTaskOutcome {
                task_id: Some(format!("t-{}", item.url)),
                error: None,
            })
            .collect();
        Box::pin(async move {
            Ok(DownloadsResult::CreateMany(DaemonCreateTasksResult {
                results,
            }))
        })
    }

    fn unavailable() -> RpcErrorData {
        RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
    }

    fn many(command: &DownloadsCommand) -> Option<&DaemonCreateTasksParams> {
        match command {
            DownloadsCommand::CreateMany(params) => Some(params),
            _ => None,
        }
    }

    #[test]
    fn chunks_split_at_256_with_unique_batch_ids_and_shared_options() {
        for (count, expected) in [
            (1_usize, vec![1_usize]),
            (255, vec![255]),
            (256, vec![256]),
            (257, vec![256, 1]),
            (512, vec![256, 256]),
            (10_000, [vec![256; 39], vec![16]].concat()),
        ] {
            let seen = RefCell::new(Vec::<(String, usize, String, String)>::new());
            let report = block_on(run_submission(
                tasks(count),
                |command| {
                    if let Some(params) = many(&command) {
                        seen.borrow_mut().push((
                            params.batch_id.clone(),
                            params.items.len(),
                            params.common.save_dir.clone(),
                            params.common.url.clone(),
                        ));
                    }
                    created_all(&command)
                },
                hooks(),
            ));
            let seen = seen.into_inner();
            let sizes: Vec<usize> = seen.iter().map(|call| call.1).collect();
            assert_eq!(sizes, expected, "{count} items");
            let mut ids: Vec<&str> = seen.iter().map(|call| call.0.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), seen.len(), "batch ids must be unique");
            for call in &seen {
                assert!(!call.0.is_empty() && call.0.len() <= 128, "{}", call.0);
                assert_eq!(call.2, "/downloads");
                assert!(call.3.is_empty(), "common must not carry a url");
            }
            assert_eq!(report.created_task_ids().len(), count);
            assert!(!report.failed() && !report.cancelled());
            assert_eq!(report.not_submitted(), 0);
        }
    }

    #[test]
    fn single_entry_overrides_reach_the_wire_item_and_common() {
        use crate::model::new_download::{DraftOptions, UrlEntry, build_requests};
        let built = build_requests(
            &[UrlEntry {
                url: "https://a/x".to_owned(),
                file_name: "ignored.bin".to_owned(),
                checksum: "md5=ff".to_owned(),
            }],
            &DraftOptions {
                rename: "renamed.bin".to_owned(),
                checksum: "sha-256=ab".to_owned(),
                http_user: "u".to_owned(),
                http_password: "p".to_owned(),
                save_site_auth: true,
                ..DraftOptions::default()
            },
        );
        let submission = NewDownloadSubmission::Tasks {
            common: Box::new(built.common),
            items: built.items,
            captures: Vec::new(),
            category_route: None,
            offered_save_dir: String::new(),
        };
        let sent = RefCell::new(None);
        block_on(run_submission(
            submission,
            |command| {
                if let Some(params) = many(&command) {
                    *sent.borrow_mut() = Some(params.clone());
                }
                created_all(&command)
            },
            hooks(),
        ));
        let Some(params) = sent.into_inner() else {
            panic!("single entry must be submitted through createMany");
        };
        assert_eq!(params.items.len(), 1);
        assert_eq!(params.items[0].file_name, "renamed.bin");
        assert_eq!(params.items[0].checksum, "sha-256=ab");
        assert_eq!(params.common.http_user, "u");
        assert!(params.common.save_site_auth);
    }

    #[test]
    fn per_item_outcomes_feed_counts_and_created_ids() -> Result<(), I18nError> {
        let catalog = std::sync::Arc::new(I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        // 回复缺条目：按失败计，不能静默丢掉。
        let report = block_on(run_submission(
            tasks(4),
            |_| {
                Box::pin(async {
                    Ok(DownloadsResult::CreateMany(DaemonCreateTasksResult {
                        results: Vec::new(),
                    }))
                })
            },
            hooks(),
        ));
        assert_eq!(report.summary.succeeded, 0);
        assert_eq!(report.summary.failed, 4);
        assert!(report.failed());

        // 逐条结果：一条拒绝，其余成功。
        let report = block_on(run_submission(
            tasks(3),
            |command| {
                let Some(params) = many(&command) else {
                    return Box::pin(async { Ok(DownloadsResult::Unit) });
                };
                let results = (0..params.items.len())
                    .map(|index| {
                        if index == 1 {
                            CreateTaskOutcome {
                                task_id: None,
                                error: Some(RpcErrorData::new(
                                    ApplicationErrorCode::InvalidArgument,
                                    false,
                                )),
                            }
                        } else {
                            CreateTaskOutcome {
                                task_id: Some(format!("t{index}")),
                                error: None,
                            }
                        }
                    })
                    .collect();
                Box::pin(async move {
                    Ok(DownloadsResult::CreateMany(DaemonCreateTasksResult {
                        results,
                    }))
                })
            },
            hooks(),
        ));
        assert_eq!(report.created_task_ids(), ["t0", "t2"]);
        assert_eq!(report.summary.succeeded, 2);
        assert_eq!(report.summary.failed, 1);
        let notice = report.notice(&translator);
        assert!(!notice.ok);
        assert!(
            notice.message.starts_with("Added 2 of 3; 1 failed"),
            "{}",
            notice.message
        );
        Ok(())
    }

    #[test]
    fn retry_resends_same_batch_id_with_backoff_and_stops_after_three() {
        let batch_ids = RefCell::new(Vec::<String>::new());
        let delays = RefCell::new(Vec::<Duration>::new());
        let report = block_on(run_submission(
            tasks(600),
            |command| {
                if let Some(params) = many(&command) {
                    batch_ids.borrow_mut().push(params.batch_id.clone());
                }
                Box::pin(async { Err(unavailable()) })
            },
            SubmissionHooks::new(|delay| {
                delays.borrow_mut().push(delay);
                std::future::ready(())
            }),
        ));
        let batch_ids = batch_ids.into_inner();
        // 首发 + 3 次重发，全部同一个幂等键；之后放弃，不再尝试后续分块。
        assert_eq!(batch_ids.len(), 4);
        assert!(batch_ids.iter().all(|id| id == &batch_ids[0]));
        assert_eq!(
            delays.into_inner(),
            [
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8)
            ]
        );
        assert_eq!(report.summary.failed, 256);
        assert_eq!(report.not_submitted(), 600 - 256);
        assert!(report.failed() && !report.cancelled());
    }

    #[test]
    fn retry_that_succeeds_counts_the_chunk_once() {
        let attempts = RefCell::new(Vec::<String>::new());
        let progress = RefCell::new(Vec::<SubmissionProgress>::new());
        let report = block_on(run_submission(
            tasks(3),
            |command| {
                let Some(params) = many(&command) else {
                    return Box::pin(async { Ok(DownloadsResult::Unit) });
                };
                attempts.borrow_mut().push(params.batch_id.clone());
                if attempts.borrow().len() < 3 {
                    Box::pin(async { Err(unavailable()) })
                } else {
                    created_all(&command)
                }
            },
            hooks().with_progress(|update: &SubmissionProgress| {
                progress.borrow_mut().push(*update);
            }),
        ));
        let attempts = attempts.into_inner();
        assert_eq!(attempts.len(), 3);
        assert!(attempts.iter().all(|id| id == &attempts[0]));
        assert_eq!(report.created_task_ids().len(), 3);
        assert!(!report.failed());
        let progress = progress.into_inner();
        assert!(progress.iter().any(|update| update.retry.is_some()));
        assert_eq!(
            progress.last(),
            Some(&SubmissionProgress {
                total: 3,
                done: 3,
                failed: 0,
                retry: None
            })
        );
    }

    #[test]
    fn cancel_before_start_sends_nothing_and_reports_everything_unsubmitted()
    -> Result<(), I18nError> {
        let catalog = std::sync::Arc::new(I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        let cancel = CancelToken::default();
        cancel.cancel();
        let calls = RefCell::new(0_usize);
        let report = block_on(run_submission(
            tasks(300),
            |command| {
                if many(&command).is_some() {
                    *calls.borrow_mut() += 1;
                }
                created_all(&command)
            },
            hooks().with_cancel(cancel),
        ));
        assert_eq!(calls.into_inner(), 0);
        assert!(report.cancelled());
        assert_eq!(report.not_submitted(), 300);
        assert!(report.created_task_ids().is_empty());
        let notice = report.notice(&translator);
        assert!(notice.ok);
        assert_eq!(notice.message, "Import cancelled: added 0 of 300");
        Ok(())
    }

    #[test]
    fn cancel_between_chunks_keeps_confirmed_tasks_and_stops_further_chunks() {
        let cancel = CancelToken::default();
        let calls = RefCell::new(0_usize);
        let report = block_on(run_submission(
            tasks(700),
            |command| {
                if many(&command).is_some() {
                    *calls.borrow_mut() += 1;
                }
                created_all(&command)
            },
            hooks()
                .with_cancel(cancel.clone())
                .with_progress(|update: &SubmissionProgress| {
                    if update.done >= 256 {
                        cancel.cancel();
                    }
                }),
        ));
        assert_eq!(calls.into_inner(), 1);
        assert!(report.cancelled());
        assert_eq!(report.created_task_ids().len(), 256);
        assert_eq!(report.summary.succeeded, 256);
        assert_eq!(report.not_submitted(), 700 - 256);
    }

    /// 第一次轮询挂起、之后才就绪的回复：模拟「已发出、结果稍后到达」。
    fn late(result: PortFuture<DownloadsResult>) -> PortFuture<DownloadsResult> {
        let mut result = result;
        let mut polled = false;
        Box::pin(std::future::poll_fn(move |cx| {
            if polled {
                result.as_mut().poll(cx)
            } else {
                polled = true;
                Poll::Pending
            }
        }))
    }

    #[test]
    fn cancel_during_chunk_waits_for_in_flight_result_so_counts_stay_accurate() {
        let cancel = CancelToken::default();
        let graces = RefCell::new(Vec::<Duration>::new());
        let report = block_on(run_submission(
            tasks(600),
            |command| {
                // 第一块已发出，用户随即取消；回复在收尾时间内到达。
                cancel.cancel();
                late(created_all(&command))
            },
            SubmissionHooks::new(|delay| {
                graces.borrow_mut().push(delay);
                std::future::pending::<()>()
            })
            .with_cancel(cancel.clone()),
        ));
        assert_eq!(graces.into_inner(), [Duration::from_secs(3)]);
        assert!(report.cancelled());
        assert_eq!(report.created_task_ids().len(), 256);
        assert_eq!(report.not_submitted(), 600 - 256);
    }

    #[test]
    fn cancel_during_chunk_drops_unsettled_request_and_counts_it_unsubmitted() {
        let cancel = CancelToken::default();
        let calls = RefCell::new(0_usize);
        let report = block_on(run_submission(
            tasks(600),
            |command| {
                if many(&command).is_none() {
                    return Box::pin(async { Ok(DownloadsResult::Unit) });
                }
                *calls.borrow_mut() += 1;
                cancel.cancel();
                // 永不回复；收尾时间（立即到期）后被放弃。
                Box::pin(std::future::pending::<Result<DownloadsResult, RpcErrorData>>())
            },
            hooks().with_cancel(cancel.clone()),
        ));
        assert_eq!(calls.into_inner(), 1);
        assert!(report.cancelled());
        assert!(report.created_task_ids().is_empty());
        assert!(!report.failed());
        assert_eq!(report.not_submitted(), 600);
    }

    #[test]
    fn cancel_during_retry_wait_stops_without_resending() {
        let cancel = CancelToken::default();
        let calls = RefCell::new(0_usize);
        let report = block_on(run_submission(
            tasks(10),
            |command| {
                if many(&command).is_some() {
                    *calls.borrow_mut() += 1;
                }
                Box::pin(async { Err(unavailable()) })
            },
            SubmissionHooks::new(|_| std::future::pending::<()>())
                .with_cancel(cancel.clone())
                .with_progress(|update: &SubmissionProgress| {
                    if update.retry.is_some() {
                        cancel.cancel();
                    }
                }),
        ));
        assert_eq!(calls.into_inner(), 1);
        assert!(report.cancelled());
        assert_eq!(report.not_submitted(), 10);
        assert!(!report.failed());
    }

    #[test]
    fn cancelled_captures_are_rejected_best_effort() {
        let cancel = CancelToken::default();
        cancel.cancel();
        let submission = NewDownloadSubmission::Tasks {
            common: Box::new(blank_request("")),
            items: Vec::new(),
            captures: vec![CapturedTask {
                transaction_id: "cap-1".to_owned(),
                request: blank_request(""),
            }],
            category_route: None,
            offered_save_dir: String::new(),
        };
        let resolved = RefCell::new(Vec::<(String, bool)>::new());
        let report = block_on(run_submission(
            submission,
            |command| {
                if let DownloadsCommand::CaptureResolve(params) = &command {
                    resolved
                        .borrow_mut()
                        .push((params.transaction_id.clone(), params.accepted));
                }
                Box::pin(async { Ok(DownloadsResult::Unit) })
            },
            hooks().with_cancel(cancel),
        ));
        assert!(report.cancelled());
        assert_eq!(report.not_submitted(), 1);
        assert_eq!(resolved.into_inner(), [("cap-1".to_owned(), false)]);
    }

    #[test]
    fn cancel_token_wakes_a_pending_waiter() {
        let token = CancelToken::default();
        let mut waiter = std::pin::pin!(token.cancelled());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(waiter.as_mut().poll(&mut cx).is_pending());
        token.cancel();
        assert!(waiter.as_mut().poll(&mut cx).is_ready());
    }

    #[test]
    fn torrent_files_are_user_initiated_and_carry_form_options() {
        let plan = NewDownloadSubmission::TorrentFiles {
            paths: vec![
                PathBuf::from("/tmp/a.torrent"),
                PathBuf::from("/tmp/b.torrent"),
            ],
            options: TorrentFileOptions {
                save_dir: "/data/bt".to_owned(),
                queue_id: "q1".to_owned(),
                start_paused: true,
            },
        }
        .plan();
        assert_eq!(plan.commands.len(), 2);
        for command in &plan.commands {
            let DownloadsCommand::SubmitTorrentFile {
                silent,
                save_dir,
                queue_id,
                start_paused,
                ..
            } = command
            else {
                panic!("torrent files must use submitTorrentFile");
            };
            assert!(
                !silent,
                "user-initiated torrents must go through file selection"
            );
            assert_eq!(save_dir.as_deref(), Some("/data/bt"));
            assert_eq!(queue_id.as_deref(), Some("q1"));
            assert_eq!(*start_paused, Some(true));
        }
    }

    #[test]
    fn torrent_files_with_blank_form_options_defer_to_agent_defaults() {
        let plan = NewDownloadSubmission::TorrentFiles {
            paths: vec![PathBuf::from("/tmp/a.torrent")],
            options: TorrentFileOptions::default(),
        }
        .plan();
        let [
            DownloadsCommand::SubmitTorrentFile {
                save_dir, queue_id, ..
            },
        ] = plan.commands.as_slice()
        else {
            panic!("one torrent, one command");
        };
        assert_eq!(save_dir, &None);
        assert_eq!(queue_id, &None);
    }

    fn route() -> CategoryRouteDto {
        CategoryRouteDto {
            rules: vec![fluxdown_protocol::CategoryRuleDto {
                match_mode: "extension".to_owned(),
                extensions: vec!["mp4".to_owned()],
                save_dir: "/data/video".to_owned(),
                ..Default::default()
            }],
            other_dir: "/data/other".to_owned(),
        }
    }

    fn routed_plan(
        submitted: &str,
        offered: &str,
        route: Option<CategoryRouteDto>,
    ) -> (Option<CategoryRouteDto>, String, Option<DownloadsCommand>) {
        let plan = NewDownloadSubmission::Tasks {
            common: Box::new(blank_request(submitted)),
            items: vec![CreateTaskItem::default()],
            captures: Vec::new(),
            category_route: route,
            offered_save_dir: offered.to_owned(),
        }
        .plan();
        let batch = plan.batch.expect("batch");
        let remembered = plan.best_effort.into_iter().next();
        (batch.category_route, batch.common.save_dir, remembered)
    }

    #[test]
    fn default_save_dir_is_routed_with_offered_base() {
        let (route_out, save_dir, remembered) =
            routed_plan("/downloads", "/downloads", Some(route()));
        assert_eq!(route_out, Some(route()));
        assert_eq!(save_dir, "/downloads");
        // 偏好记录用户看到 / 提交的表单值。
        assert!(matches!(
            remembered,
            Some(DownloadsCommand::SetLocalPreference { key: LAST_SAVE_DIR_PREF, value })
                if value == serde_json::Value::String("/downloads".to_owned())
        ));
    }

    #[test]
    fn category_dir_submitted_falls_back_to_offered_base() {
        let (route_out, save_dir, _) = routed_plan("/data/video", "/downloads", Some(route()));
        assert_eq!(route_out, Some(route()));
        assert_eq!(save_dir, "/downloads");
    }

    #[test]
    fn user_picked_dir_is_not_routed() {
        let (route_out, save_dir, _) = routed_plan("/elsewhere", "/downloads", Some(route()));
        assert_eq!(route_out, None);
        assert_eq!(save_dir, "/elsewhere");
    }

    #[test]
    fn capture_confirmation_disables_category_route_after_manual_directory_choice() {
        fn disable_flag(route: Option<CategoryRouteDto>, save_dir: &str) -> bool {
            let plan = NewDownloadSubmission::Tasks {
                common: Box::new(blank_request(save_dir)),
                items: Vec::new(),
                captures: vec![CapturedTask {
                    transaction_id: "capture".to_owned(),
                    request: blank_request(save_dir),
                }],
                category_route: route,
                offered_save_dir: "/downloads".to_owned(),
            }
            .plan();
            let Some(DownloadsCommand::CaptureResolve(params)) = plan.commands.first() else {
                panic!("capture confirmation command");
            };
            params.disable_category_route
        }

        assert!(!disable_flag(Some(route()), "/downloads"));
        assert!(disable_flag(None, "/data/video"));
    }

    #[test]
    fn missing_categories_never_route() {
        let (route_out, save_dir, _) = routed_plan("/downloads", "/downloads", None);
        assert_eq!(route_out, None);
        assert_eq!(save_dir, "/downloads");
    }
}
