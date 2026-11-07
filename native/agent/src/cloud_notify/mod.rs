//! 云端推送通知（FluxCloud 代发）的 agent 侧：只消费 daemon 的 `TaskNotice`，不转发给 UI。
//!
//! - 渠道配置、额度与投递全部在云端；本模块只负责四件事：
//!   1. 设备本地偏好（上报开关 / `includeUrl` / `includeSaveDir`，默认全关，存 agent 私有状态、不进同步目录）；
//!   2. 收到 `TaskNotice` 后按「登录 + 开关 + 套餐 + 渠道匹配」过滤、按隐私开关裁剪，进 256 条有界内存
//!      队列（不落盘），攒 1s 批量上报，网络 / 5xx 指数退避重试 3 次；
//!   3. 维护 `AgentSnapshot.cloud_notify`（云端概览缓存）并以 `CloudNotifyChanged` 整体推送；
//!      登录 / 登出 / 账号切换、云端 SSE `notify.changed`、渠道变更成功后刷新，`get` 发现缓存超过 30s 后台刷新；
//!   4. 云端投递记录第一页（`recent_deliveries`）：登录 / 启动 / `refresh` / SSE（重）连上 / `notify.changed` /
//!      `resync` 时拉取整页覆盖；SSE `notify.delivery` 按 `id` 增量 upsert，按时间降序截断 50 条。

mod pipeline;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluxdown_protocol::{
    AgentEvent, CloudNotifyChannelCreateParams, CloudNotifyChannelDto,
    CloudNotifyChannelUpdateParams, CloudNotifyDeliveriesPage, CloudNotifyDeliveriesParams,
    CloudNotifyDeliveryDto, CloudNotifyEmailCodeResult, CloudNotifyPrivacyParams,
    CloudNotifyStateDto, CloudNotifyTelegramBindDto, CloudNotifyTelegramBindStatusDto,
    CloudNotifyTestResult, TaskNoticeDto,
};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use self::pipeline::{
    BATCH_LIMIT, BATCH_WINDOW, EventQueue, QUEUE_CAPACITY, RETRY_LIMIT, WireEvent,
    first_page_params, is_retryable, merge_deliveries, retry_delay, set_first_page, should_report,
    trim_for_upload,
};
use crate::cloud::{CloudApi, CloudError, RequestEpoch};
use crate::event_hub::AgentEventHub;
use crate::state::{AgentState, CloudNotifyPrefs, StateStore};

/// 概览缓存的有效期：`get` 发现超过它就后台刷新。
const CACHE_TTL: Duration = Duration::from_secs(30);
/// daemon → 本模块的 `TaskNotice` 通道容量（事件稀疏；满则丢弃并记日志，绝不阻塞投影）。
const NOTICE_CHANNEL_CAPACITY: usize = 1024;

/// daemon 事件投影用的发送端：`TaskNotice` 只经它进入本模块。
pub(crate) type NoticeSender = mpsc::Sender<TaskNoticeDto>;
/// 本模块的接收端，交给 [`CloudNotifyService::run`]。
pub(crate) type NoticeReceiver = mpsc::Receiver<TaskNoticeDto>;

/// 建立 `TaskNotice` 通道。
pub(crate) fn notice_channel() -> (NoticeSender, NoticeReceiver) {
    mpsc::channel(NOTICE_CHANNEL_CAPACITY)
}

/// 云端 SSE 流里与推送相关的事件（由 `RemoteTaskService` 转交，见 [`NotifyFeed`]）。
enum FeedEvent {
    /// `notify.changed`：渠道 / 额度 / 种类开关有变。
    Changed,
    /// `resync`：服务端要求整体重同步。
    Resync,
    /// SSE（重）连接成功：重连期间可能错过了增量。
    Connected,
    /// `notify.delivery`：变更后的完整投递记录；`RequestEpoch` 防止旧会话的事件落到新账号。
    Deliveries(Vec<CloudNotifyDeliveryDto>, RequestEpoch),
}

/// 云端 SSE 流到推送模块的入口（无界：事件稀疏且单条很小，绝不阻塞 SSE 读取）。
#[derive(Clone)]
pub struct NotifyFeed(mpsc::UnboundedSender<FeedEvent>);

impl NotifyFeed {
    fn send(&self, event: FeedEvent) {
        if self.0.send(event).is_err() {
            tracing::debug!("cloud notify feed is closed; dropped an SSE event");
        }
    }

    pub(crate) fn changed(&self) {
        self.send(FeedEvent::Changed);
    }

    pub(crate) fn resync(&self) {
        self.send(FeedEvent::Resync);
    }

    pub(crate) fn connected(&self) {
        self.send(FeedEvent::Connected);
    }

    pub(crate) fn deliveries(&self, items: Vec<CloudNotifyDeliveryDto>, epoch: RequestEpoch) {
        self.send(FeedEvent::Deliveries(items, epoch));
    }
}

/// daemon 事件进入 agent 的唯一分流点：`TaskNotice` 只交给云端推送与（桌面宿主的）提示音 /
/// 失败通知，其余照常并入 agent 事件流。
/// 因此 UI 订阅者永远看不到 `TaskNotice`（其载荷含任务元数据，是否上报由本机开关决定）。
pub(crate) fn route_daemon_event(
    events: &AgentEventHub,
    notices: &NoticeSender,
    sounds: Option<&crate::sound::SoundSender>,
    failures: Option<&crate::failure_notice::FailureSender>,
    event: fluxdown_protocol::DaemonEvent,
) {
    match event {
        fluxdown_protocol::DaemonEvent::TaskNotice(notice) => {
            if let Some(sounds) = sounds {
                crate::sound::offer(sounds, &notice);
            }
            if let Some(failures) = failures {
                crate::failure_notice::offer(failures, &notice);
            }
            if let Err(error) = notices.try_send(notice) {
                tracing::warn!(error = %error, "cloud notify intake unavailable; dropped a task notice");
            }
        }
        event => {
            events.apply_daemon_event(event);
        }
    }
}

/// 由持久化偏好构造启动时的 `AgentSnapshot.cloud_notify`（概览在登录后拉取）。
#[must_use]
pub fn initial_state(prefs: CloudNotifyPrefs) -> CloudNotifyStateDto {
    CloudNotifyStateDto {
        reporting: prefs.reporting,
        include_url: prefs.include_url,
        include_save_dir: prefs.include_save_dir,
        ..CloudNotifyStateDto::default()
    }
}

fn prefs_of(state: &CloudNotifyStateDto) -> CloudNotifyPrefs {
    CloudNotifyPrefs {
        reporting: state.reporting,
        include_url: state.include_url,
        include_save_dir: state.include_save_dir,
    }
}

#[derive(Default)]
struct Inner {
    queue: Option<EventQueue<WireEvent>>,
    /// 队列所属会话；会话变化（登录 / 登出 / 换号）时队列作废，旧账号的事件不会发给新账号。
    queue_epoch: Option<RequestEpoch>,
    flush_scheduled: bool,
    fetched_at: Option<Instant>,
    /// 目录最近一次成功拉取；与概览缓存独立（目录公开、登出不清）。
    catalog_fetched_at: Option<Instant>,
    /// 进行中的目录拉取数（去重 `get` 触发的后台刷新）。
    catalog_refreshing: usize,
    /// 进行中的概览拉取数（`loading` = 大于 0）。
    refreshing: usize,
}

impl Inner {
    fn queue(&mut self) -> &mut EventQueue<WireEvent> {
        self.queue
            .get_or_insert_with(|| EventQueue::new(QUEUE_CAPACITY))
    }
}

pub struct CloudNotifyService {
    cloud: CloudApi,
    events: AgentEventHub,
    state: Arc<tokio::sync::Mutex<AgentState>>,
    store: Arc<StateStore>,
    cancel: CancellationToken,
    inner: Mutex<Inner>,
    /// 序列化「读快照 → 改 → 发布」，保证 `CloudNotifyChanged` 的整体替换不丢更新。
    publish_lock: Mutex<()>,
    /// 云端 SSE 变更 / 重连 / 重同步时唤醒「概览 + 目录 + 投递第一页」整体刷新。
    refresh_signal: Notify,
    /// 只刷新投递第一页（SSE 重连、增量触发截断后校正游标）。
    deliveries_signal: Notify,
    /// 只刷新目录（未登录 / `get` 发现目录过期）。
    catalog_signal: Notify,
    /// 队列首条入队后唤醒攒批上报。
    flush_signal: Notify,
    feed_tx: mpsc::UnboundedSender<FeedEvent>,
    feed_rx: Mutex<Option<mpsc::UnboundedReceiver<FeedEvent>>>,
}

impl CloudNotifyService {
    #[must_use]
    pub fn new(
        cloud: CloudApi,
        events: AgentEventHub,
        state: Arc<tokio::sync::Mutex<AgentState>>,
        store: Arc<StateStore>,
        cancel: CancellationToken,
    ) -> Self {
        let (feed_tx, feed_rx) = mpsc::unbounded_channel();
        Self {
            cloud,
            events,
            state,
            store,
            cancel,
            inner: Mutex::new(Inner::default()),
            publish_lock: Mutex::new(()),
            refresh_signal: Notify::new(),
            deliveries_signal: Notify::new(),
            catalog_signal: Notify::new(),
            flush_signal: Notify::new(),
            feed_tx,
            feed_rx: Mutex::new(Some(feed_rx)),
        }
    }

    /// 交给 `RemoteTaskService`：把云端 SSE 的推送相关事件转进来。
    #[must_use]
    pub fn feed(&self) -> NotifyFeed {
        NotifyFeed(self.feed_tx.clone())
    }

    /// 后台循环：消费 `TaskNotice`、响应会话变化与云端变更信号。
    pub(crate) async fn run(self: Arc<Self>, mut notices: NoticeReceiver) {
        let Some(mut feed) = self
            .feed_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            tracing::error!("cloud notify service started twice");
            return;
        };
        let mut session_changes = self.cloud.session_changes();
        session_changes.borrow_and_update();
        // 后台工作（刷新 / 攒批上报）都挂在这个 JoinSet 里：`run` 返回前全部收回，
        // 不让游离任务继续持有状态存储（嵌入式宿主要靠它释放 agent 数据目录锁）。
        let mut tasks = tokio::task::JoinSet::new();
        // 启动时已恢复登录态：先拉一次概览，渠道匹配才有缓存可用。
        self.on_session_changed().await;
        // 目录是公开信息：未登录也要拉（已登录时由上面的概览刷新一并带上）。
        if !self.cloud.is_authenticated().await {
            self.request_catalog_refresh();
        }
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => break,
                changed = session_changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    session_changes.borrow_and_update();
                    self.on_session_changed().await;
                }
                () = self.refresh_signal.notified() => {
                    let service = Arc::clone(&self);
                    tasks.spawn(async move {
                        if let Err(error) = service.refresh().await {
                            tracing::debug!(error = %error, "cloud notify overview refresh failed");
                        }
                    });
                }
                () = self.catalog_signal.notified() => {
                    let service = Arc::clone(&self);
                    tasks.spawn(async move { service.refresh_catalog_logged().await });
                }
                () = self.flush_signal.notified() => {
                    tasks.spawn(Arc::clone(&self).flush_loop());
                }
                () = self.deliveries_signal.notified() => {
                    let service = Arc::clone(&self);
                    tasks.spawn(async move {
                        if let Err(error) = service.refresh_first_page().await {
                            tracing::debug!(error = %error, "cloud notify deliveries refresh failed");
                        }
                    });
                }
                Some(event) = feed.recv() => match event {
                    FeedEvent::Changed | FeedEvent::Resync => self.request_refresh(),
                    FeedEvent::Connected => self.request_deliveries_refresh(),
                    FeedEvent::Deliveries(items, epoch) => {
                        self.apply_delivery_items(items, epoch).await;
                    }
                },
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
                notice = notices.recv() => match notice {
                    Some(notice) => self.ingest(notice).await,
                    None => break,
                },
            }
        }
        tasks.shutdown().await;
    }

    // ---------------------------------------------------------------- 状态投影

    fn lock_inner(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 当前投影（`AgentSnapshot.cloud_notify`）。
    fn current(&self) -> CloudNotifyStateDto {
        self.events
            .inspect(|snapshot| snapshot.cloud_notify.clone())
    }

    /// 修改投影并在变化时整体推送 `CloudNotifyChanged`。
    fn update(&self, change: impl FnOnce(&mut CloudNotifyStateDto)) -> CloudNotifyStateDto {
        let _guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = self.current();
        let mut next = current.clone();
        change(&mut next);
        if next != current {
            self.events
                .publish(AgentEvent::CloudNotifyChanged(next.clone()));
        }
        next
    }

    fn begin_refresh(&self) {
        self.lock_inner().refreshing += 1;
        self.update(|state| state.loading = true);
    }

    /// 收尾一次概览拉取：递减进行中计数，并把 `change` 与 `loading` 一并发布。
    fn end_refresh(&self, change: impl FnOnce(&mut CloudNotifyStateDto)) {
        let loading = {
            let mut inner = self.lock_inner();
            inner.refreshing = inner.refreshing.saturating_sub(1);
            inner.refreshing > 0
        };
        self.update(|state| {
            change(state);
            state.loading = loading;
        });
    }

    // ---------------------------------------------------------------- 概览刷新

    /// 立即向云端拉取目录与概览并推送（`agent.cloudNotify.refresh`）。目录失败只记日志、
    /// 保留旧值，不影响概览的结果与错误原因；未登录只刷新目录。
    pub async fn refresh(&self) -> Result<CloudNotifyStateDto, CloudError> {
        if !self.cloud.is_authenticated().await {
            self.clear_projection();
            self.refresh_catalog_logged().await;
            return Ok(self.current());
        }
        let epoch = self.cloud.request_epoch();
        self.begin_refresh();
        let scoped = self.cloud.at_epoch(epoch);
        let params = first_page_params();
        let (result, deliveries, ()) = tokio::join!(
            scoped.notify_overview(),
            scoped.notify_deliveries(&params),
            self.refresh_catalog_logged()
        );
        let page = deliveries
            .map_err(
                |error| tracing::debug!(error = %error, "cloud notify deliveries refresh failed"),
            )
            .ok();
        // 会话在请求期间结束（登出 / 换号）：结果作废，只收尾 loading。
        let session = self.cloud.lock_epoch(epoch).await;
        match (result, session) {
            (Ok(overview), Ok(_guard)) => {
                self.lock_inner().fetched_at = Some(Instant::now());
                self.end_refresh(|state| {
                    state.overview = Some(overview);
                    state.last_error_reason = None;
                    state.updated_at_unix_ms = Some(now_unix_ms());
                    if let Some(page) = page {
                        set_first_page(state, page);
                    }
                });
                Ok(self.current())
            }
            (Err(error), Ok(_guard)) => {
                let reason = error.reason();
                self.end_refresh(|state| {
                    state.last_error_reason = reason;
                    if let Some(page) = page {
                        set_first_page(state, page);
                    }
                });
                Err(error)
            }
            (_, Err(error)) => {
                self.end_refresh(|_| {});
                Err(error)
            }
        }
    }

    /// 只拉投递第一页并整页覆盖 `recent_deliveries` / 游标；失败保留旧值。
    async fn refresh_first_page(&self) -> Result<(), CloudError> {
        if !self.cloud.is_authenticated().await {
            return Ok(());
        }
        let epoch = self.cloud.request_epoch();
        let page = self
            .cloud
            .at_epoch(epoch)
            .notify_deliveries(&first_page_params())
            .await?;
        let _guard = self.cloud.lock_epoch(epoch).await?;
        self.update(|state| set_first_page(state, page));
        Ok(())
    }

    /// SSE `notify.delivery`：按 `id` upsert、降序、截断；截断丢了较旧记录时第一页游标已失效，
    /// 重拉第一页校正。会话已变化的事件丢弃。
    async fn apply_delivery_items(&self, items: Vec<CloudNotifyDeliveryDto>, epoch: RequestEpoch) {
        let Ok(_guard) = self.cloud.lock_epoch(epoch).await else {
            return;
        };
        let mut truncated = false;
        self.update(|state| truncated = merge_deliveries(&mut state.recent_deliveries, items));
        if truncated {
            self.request_deliveries_refresh();
        }
    }

    /// 匿名拉取渠道种类目录；成功才覆盖投影，失败保留旧值（`None` 仍为 `None`）。
    async fn refresh_catalog(&self) -> Result<(), CloudError> {
        self.lock_inner().catalog_refreshing += 1;
        let result = self.cloud.notify_catalog().await;
        {
            let mut inner = self.lock_inner();
            inner.catalog_refreshing = inner.catalog_refreshing.saturating_sub(1);
            if result.is_ok() {
                inner.catalog_fetched_at = Some(Instant::now());
            }
        }
        let kinds = result?;
        self.update(|state| state.catalog = Some(kinds));
        Ok(())
    }

    async fn refresh_catalog_logged(&self) {
        if let Err(error) = self.refresh_catalog().await {
            tracing::debug!(error = %error, "cloud notify catalog refresh failed");
        }
    }

    /// 请求后台刷新目录（由 [`Self::run`] 的任务集执行）。
    fn request_catalog_refresh(&self) {
        self.catalog_signal.notify_one();
    }

    fn request_refresh(&self) {
        self.refresh_signal.notify_one();
    }

    /// 请求后台刷新投递第一页（由 [`Self::run`] 的任务集执行）。
    fn request_deliveries_refresh(&self) {
        self.deliveries_signal.notify_one();
    }

    /// 登出 / 会话结束：丢弃概览缓存与队列（偏好保留）。
    fn clear_projection(&self) {
        {
            let mut inner = self.lock_inner();
            inner.fetched_at = None;
            inner.queue_epoch = None;
            if let Some(queue) = inner.queue.as_mut() {
                queue.clear();
            }
        }
        self.update(|state| {
            state.overview = None;
            state.loading = false;
            state.last_error_reason = None;
            state.updated_at_unix_ms = None;
            state.recent_deliveries.clear();
            state.recent_next_cursor = None;
        });
    }

    async fn on_session_changed(self: &Arc<Self>) {
        self.clear_projection();
        if self.cloud.is_authenticated().await {
            self.request_refresh();
        }
    }

    // ---------------------------------------------------------------- 事件上报

    async fn ingest(self: &Arc<Self>, notice: TaskNoticeDto) {
        let logged_in = self.cloud.is_authenticated().await;
        let accepted = self.events.inspect(|snapshot| {
            let state = &snapshot.cloud_notify;
            should_report(
                logged_in,
                prefs_of(state),
                state.catalog.as_deref(),
                state.overview.as_ref(),
                &notice.event,
            )
        });
        if !accepted {
            return;
        }
        let prefs = prefs_of(&self.current());
        let wire = trim_for_upload(&notice, prefs);
        let epoch = self.cloud.request_epoch();
        let (dropped, spawn_flush) = {
            let mut inner = self.lock_inner();
            if inner.queue_epoch != Some(epoch) {
                inner.queue().clear();
                inner.queue_epoch = Some(epoch);
            }
            let dropped = inner.queue().push(wire);
            let spawn_flush = !inner.flush_scheduled;
            inner.flush_scheduled = true;
            (dropped, spawn_flush)
        };
        if dropped > 0 {
            tracing::warn!(
                dropped,
                "cloud notify queue is full; dropped the oldest event"
            );
        }
        if spawn_flush {
            self.flush_signal.notify_one();
        }
    }

    /// 首条入队后等 1s 攒批，随后分批上报直到队列清空。
    async fn flush_loop(self: Arc<Self>) {
        tokio::select! {
            () = self.cancel.cancelled() => return,
            () = tokio::time::sleep(BATCH_WINDOW) => {}
        }
        loop {
            let (batch, epoch) = {
                let mut inner = self.lock_inner();
                let batch = inner.queue().take_batch(BATCH_LIMIT);
                if batch.is_empty() {
                    inner.flush_scheduled = false;
                    return;
                }
                (batch, inner.queue_epoch)
            };
            let Some(epoch) = epoch else { continue };
            if !self.send_batch(&batch, epoch).await {
                return;
            }
        }
    }

    /// 上报一批；返回 `false` 表示应停止刷队列（agent 正在退出）。
    async fn send_batch(self: &Arc<Self>, batch: &[WireEvent], epoch: RequestEpoch) -> bool {
        let api = self.cloud.at_epoch(epoch);
        let mut attempt = 0;
        loop {
            match api.notify_report_events(batch).await {
                Ok(response) => {
                    self.apply_report(response, epoch).await;
                    return true;
                }
                Err(error) if is_retryable(&error) && attempt < RETRY_LIMIT => {
                    tokio::select! {
                        () = self.cancel.cancelled() => {
                            self.lock_inner().flush_scheduled = false;
                            return false;
                        }
                        () = tokio::time::sleep(retry_delay(attempt)) => {}
                    }
                    attempt += 1;
                }
                Err(error) => {
                    // 事件不落盘：永久失败 / 重试耗尽即放弃这一批。
                    tracing::warn!(
                        error = %error,
                        events = batch.len(),
                        "cloud notify report failed; batch dropped"
                    );
                    if error.code.as_deref() == Some("notify_disabled") {
                        self.request_refresh();
                    }
                    return true;
                }
            }
        }
    }

    /// 用响应里的用量更新投影；`no_channel` / `disabled` 说明本机缓存过期，顺手重拉概览。
    async fn apply_report(
        self: &Arc<Self>,
        response: crate::cloud::NotifyReportResponse,
        epoch: RequestEpoch,
    ) {
        if let Some(usage) = response.usage
            && let Ok(_guard) = self.cloud.lock_epoch(epoch).await
        {
            self.update(|state| {
                if let Some(overview) = state.overview.as_mut() {
                    overview.usage = usage;
                }
            });
        }
        if response
            .results
            .iter()
            .any(|result| matches!(result.outcome.as_str(), "no_channel" | "disabled"))
        {
            self.request_refresh();
        }
    }

    // ---------------------------------------------------------------- RPC

    /// `agent.cloudNotify.get`：读本机缓存；空缓存或超过 30s 时后台刷新。
    pub async fn get(self: &Arc<Self>) -> CloudNotifyStateDto {
        let state = self.current();
        let authenticated = self.cloud.is_authenticated().await;
        let (overview_stale, catalog_stale) = {
            let inner = self.lock_inner();
            (
                inner.refreshing == 0
                    && inner
                        .fetched_at
                        .is_none_or(|fetched| fetched.elapsed() > CACHE_TTL),
                inner.catalog_refreshing == 0
                    && (state.catalog.is_none()
                        || inner
                            .catalog_fetched_at
                            .is_none_or(|fetched| fetched.elapsed() > CACHE_TTL)),
            )
        };
        if authenticated && overview_stale {
            // 概览刷新一并带上目录。
            self.request_refresh();
        } else if catalog_stale {
            self.request_catalog_refresh();
        }
        state
    }

    /// `agent.cloudNotify.setReporting`。
    pub async fn set_reporting(
        self: &Arc<Self>,
        enabled: bool,
    ) -> Result<CloudNotifyStateDto, CloudError> {
        let prefs = self
            .persist_prefs(|prefs| prefs.reporting = enabled)
            .await?;
        if !enabled {
            let mut inner = self.lock_inner();
            inner.queue_epoch = None;
            if let Some(queue) = inner.queue.as_mut() {
                queue.clear();
            }
        }
        let state = self.update(|state| apply_prefs(state, prefs));
        if enabled && state.overview.is_none() {
            self.request_refresh();
        }
        Ok(state)
    }

    /// `agent.cloudNotify.setPrivacy`。
    pub async fn set_privacy(
        &self,
        params: CloudNotifyPrivacyParams,
    ) -> Result<CloudNotifyStateDto, CloudError> {
        let prefs = self
            .persist_prefs(|prefs| {
                prefs.include_url = params.include_url;
                prefs.include_save_dir = params.include_save_dir;
            })
            .await?;
        Ok(self.update(|state| apply_prefs(state, prefs)))
    }

    async fn persist_prefs(
        &self,
        change: impl FnOnce(&mut CloudNotifyPrefs),
    ) -> Result<CloudNotifyPrefs, CloudError> {
        let prefs = {
            let mut state = self.state.lock().await;
            change(&mut state.cloud_notify);
            state.cloud_notify
        };
        self.store
            .persist(&self.state)
            .await
            .map_err(CloudError::from_state)?;
        Ok(prefs)
    }

    /// 渠道变更成功后的统一收尾：刷新概览（失败只记日志，变更本身已成功）。
    async fn refresh_after_change(&self) {
        if let Err(error) = self.refresh().await {
            tracing::debug!(error = %error, "cloud notify refresh after change failed");
        }
    }

    pub async fn create_channel(
        &self,
        params: &CloudNotifyChannelCreateParams,
    ) -> Result<CloudNotifyChannelDto, CloudError> {
        let channel = self.cloud.notify_create_channel(params).await?;
        self.refresh_after_change().await;
        Ok(channel)
    }

    pub async fn update_channel(
        &self,
        params: &CloudNotifyChannelUpdateParams,
    ) -> Result<CloudNotifyChannelDto, CloudError> {
        let channel = self.cloud.notify_update_channel(params).await?;
        self.refresh_after_change().await;
        Ok(channel)
    }

    pub async fn delete_channel(&self, id: &str) -> Result<(), CloudError> {
        self.cloud.notify_delete_channel(id).await?;
        self.refresh_after_change().await;
        Ok(())
    }

    pub async fn test_channel(&self, id: &str) -> Result<CloudNotifyTestResult, CloudError> {
        self.cloud.notify_test_channel(id).await
    }

    /// 向待添加的通知邮箱发验证码（账号邮箱无需验证）。
    pub async fn send_email_code(
        &self,
        address: &str,
    ) -> Result<CloudNotifyEmailCodeResult, CloudError> {
        self.cloud.notify_send_email_code(address).await
    }

    /// 校验通知邮箱验证码；通过后该地址可用于本账号任一邮件渠道的 `addresses`。
    pub async fn verify_email(&self, address: &str, code: &str) -> Result<(), CloudError> {
        self.cloud.notify_verify_email(address, code).await
    }

    pub async fn telegram_bind_start(&self) -> Result<CloudNotifyTelegramBindDto, CloudError> {
        self.cloud.notify_telegram_bind().await
    }

    /// 绑定成功（渠道已在云端建好）时刷新概览，UI 立即看到新渠道。
    pub async fn telegram_bind_status(
        &self,
        code: &str,
    ) -> Result<CloudNotifyTelegramBindStatusDto, CloudError> {
        let status = self.cloud.notify_telegram_bind_status(code).await?;
        if status.status == "bound" {
            self.refresh_after_change().await;
        }
        Ok(status)
    }

    pub async fn deliveries(
        &self,
        params: &CloudNotifyDeliveriesParams,
    ) -> Result<CloudNotifyDeliveriesPage, CloudError> {
        self.cloud.notify_deliveries(params).await
    }
}

fn apply_prefs(state: &mut CloudNotifyStateDto, prefs: CloudNotifyPrefs) {
    state.reporting = prefs.reporting;
    state.include_url = prefs.include_url;
    state.include_save_dir = prefs.include_save_dir;
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}
