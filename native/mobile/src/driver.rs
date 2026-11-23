//! 传输无关的会话驱动：握手校验、`system.snapshot`、握手期间帧缓冲、游标判定、重同步、
//! 重连退避与 800ms 离线宽限。端口自 `crates/app/src/agent_client.rs` 与
//! `web/src/lib/rpc/client.ts`；只依赖 [`crate::link`] 的 trait，因此可用脚本化连接单测。
//!
//! 向 UI 的出口是一个有界 `mpsc`：UI 消费慢时驱动在 `send` 上等待（背压），远端传输层的
//! 事件缓冲溢出后会按「事件缺口」重连重拉，保证不会无界堆积。

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::method::{SYSTEM_SNAPSHOT, SYSTEM_TASK_PAGE};
use fluxdown_protocol::{
    CategoryRouteDto, EventFrame, PROTOCOL_VERSION, RpcRequest, ServiceRole, Snapshot, TablePage,
    TableSync,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_util::sync::CancellationToken;

use crate::dto::{HostInfoDto, HostSignalDto};
use crate::error::{ErrorCodeDto, FluxError, HostErrorDto};
use crate::link::{
    Caller, CloseInfo, ConnectFailure, Connector, Established, EventSource, LinkEvent,
};
use crate::projection::{FrameOutcome, Projection};

/// 当前连接对命令可见的状态。
pub(crate) enum LinkState {
    /// 尚未连上或正在重连：命令等待（宽限内排队）。
    Connecting,
    Ready(Arc<dyn Caller>),
    /// 会话已结束：命令立即失败。
    Closed,
}

/// 驱动的时间与容量参数（测试可缩放）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// 断线后多久未恢复才向 UI 发 `Stale`。
    pub stale_grace: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// 握手期间缓冲的事件帧上限；溢出重新取快照。
    pub buffer_cap: usize,
    /// 同一连接上连续重同步的上限，超过后重连。
    pub max_resyncs: u32,
    /// 连接存活超过该时长才算「健康」，重置退避。
    pub healthy_after: Duration,
    pub snapshot_timeout: Duration,
    /// 任务表分页载入：单页 `system.taskPage` 的超时。
    pub page_timeout: Duration,
    /// 任务表分页载入：一轮（任务 + 队列位置全部页）的总时限；超时重连。
    pub load_timeout: Duration,
    /// 连续「立即重连」的上限，防止对端反复 4009 造成空转。
    pub max_fast_reconnects: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            stale_grace: Duration::from_millis(800),
            backoff_min: Duration::from_millis(500),
            backoff_max: Duration::from_secs(15),
            buffer_cap: 65_536,
            max_resyncs: 5,
            healthy_after: Duration::from_secs(10),
            snapshot_timeout: Duration::from_secs(30),
            page_timeout: Duration::from_secs(30),
            load_timeout: Duration::from_secs(600),
            max_fast_reconnects: 5,
        }
    }
}

/// 指数退避：`min → max` 翻倍，乘以调用方给出的抖动系数。
pub(crate) struct Backoff {
    min: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub(crate) fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            next: min,
        }
    }

    pub(crate) fn next_delay(&mut self, jitter: f64) -> Duration {
        let base = self.next;
        self.next = self.next.saturating_mul(2).min(self.max);
        base.mul_f64(jitter)
    }

    pub(crate) fn reset(&mut self) {
        self.next = self.min;
    }
}

/// ±20% 抖动。
fn jitter() -> f64 {
    rand::random_range(0.8..=1.2)
}

/// 驱动终止原因。
enum Stop {
    /// 会话被关闭。
    Cancelled,
    /// UI 侧已丢弃信号接收端。
    ReceiverGone,
    /// 不可恢复错误；只在致命路径分配，避免放大实时同步的 Result。
    Fatal(Box<HostErrorDto>),
    /// 首次连接 / 同步失败：由 `open_*` 作为错误返回，不进入重试。
    FirstFailed(String),
}

impl Stop {
    fn fatal(error: HostErrorDto) -> Self {
        Self::Fatal(Box::new(error))
    }
}

/// 一次连接生命周期的结局。
enum SessionEnd {
    Stop(Stop),
    Reconnect { immediate: bool, reason: String },
}

enum SyncOutcome {
    Ready(Box<Snapshot>, Vec<fluxdown_protocol::EventFrame>),
    /// 缓冲溢出或快照不是 agent 角色：在同一连接上重新取快照。
    Resync,
    End(SessionEnd),
}

enum LiveEnd {
    Resync,
    End(SessionEnd),
}

pub(crate) struct Driver {
    pub connector: Arc<dyn Connector>,
    pub hello: RpcRequest,
    pub signals: mpsc::Sender<HostSignalDto>,
    pub link: watch::Sender<LinkState>,
    pub cancel: CancellationToken,
    /// 首次同步结果；`open_*` 等待它以便把鉴权 / 网络错误同步返回给调用方。
    pub first: Option<oneshot::Sender<Result<(), FluxError>>>,
    pub timing: Timing,
    /// 主机当前偏好 `custom_categories` 构建的分类路由，供 `HostSession::create_task` 读取。
    pub routes: watch::Sender<Option<CategoryRouteDto>>,
}

/// 驱动的可变运行状态。
struct State {
    backoff: Backoff,
    fast_reconnects: u32,
    /// 曾经成功同步过：之后的断线才需要向 UI 报告 `Stale`。
    ever_synced: bool,
    stale_at: Option<Instant>,
    stale_sent: bool,
}

impl Driver {
    pub(crate) async fn run(mut self) {
        let mut state = State {
            backoff: Backoff::new(self.timing.backoff_min, self.timing.backoff_max),
            fast_reconnects: 0,
            ever_synced: false,
            stale_at: None,
            stale_sent: false,
        };
        let stop = self.run_loop(&mut state).await;
        match stop {
            Stop::Fatal(error) => {
                if let Some(first) = self.first.take() {
                    if first.send(Err((*error).into())).is_err() {
                        tracing::debug!("open caller went away before the fatal error");
                    }
                } else if self
                    .signals
                    .send(HostSignalDto::Fatal { error: *error })
                    .await
                    .is_err()
                {
                    tracing::debug!("fatal signal dropped: receiver gone");
                }
            }
            Stop::FirstFailed(reason) => {
                if let Some(first) = self.first.take()
                    && first.send(Err(FluxError::transport(reason))).is_err()
                {
                    tracing::debug!("open caller went away before the failure");
                }
            }
            Stop::Cancelled | Stop::ReceiverGone => {}
        }
        // `first` 仍在说明会话在首次同步前被关闭：发送端丢弃即让等待者得到 `Closed`。
        self.link.send_replace(LinkState::Closed);
    }

    async fn run_loop(&mut self, state: &mut State) -> Stop {
        loop {
            if self.cancel.is_cancelled() {
                return Stop::Cancelled;
            }
            let established = match self.connect(state).await {
                Err(stop) => return stop,
                Ok(Err(ConnectFailure::Fatal(error))) => return Stop::fatal(error),
                Ok(Err(ConnectFailure::Retry(reason))) => {
                    tracing::warn!(%reason, "host connect failed");
                    if self.first.is_some() {
                        return Stop::FirstFailed(reason);
                    }
                    if let Err(stop) = self.backoff_sleep(state).await {
                        return stop;
                    }
                    continue;
                }
                Ok(Ok(established)) => established,
            };

            let started = Instant::now();
            let end = self.session(state, established).await;
            let alive = started.elapsed();
            match end {
                SessionEnd::Stop(stop) => return stop,
                SessionEnd::Reconnect { immediate, reason } => {
                    tracing::info!(%reason, immediate, "host session ended; reconnecting");
                    self.link.send_replace(LinkState::Connecting);
                    if self.first.is_some() {
                        return Stop::FirstFailed(reason);
                    }
                    self.mark_disconnected(state);
                    if alive >= self.timing.healthy_after {
                        state.backoff.reset();
                        state.fast_reconnects = 0;
                    }
                    if immediate && state.fast_reconnects < self.timing.max_fast_reconnects {
                        state.fast_reconnects += 1;
                    } else if let Err(stop) = self.backoff_sleep(state).await {
                        return stop;
                    }
                }
            }
        }
    }

    async fn connect(
        &mut self,
        state: &mut State,
    ) -> Result<Result<Established, ConnectFailure>, Stop> {
        let connector = Arc::clone(&self.connector);
        let hello = self.hello.clone();
        self.race(state, async move { connector.connect(hello).await })
            .await
    }

    async fn backoff_sleep(&mut self, state: &mut State) -> Result<(), Stop> {
        let delay = state.backoff.next_delay(jitter());
        self.race(state, sleep(delay)).await
    }

    /// 等待 `future`，期间离线宽限到期则先发 `Stale`；会话关闭立即返回。
    async fn race<F: Future>(&mut self, state: &mut State, future: F) -> Result<F::Output, Stop> {
        tokio::pin!(future);
        loop {
            let stale_at = if state.stale_sent {
                None
            } else {
                state.stale_at
            };
            tokio::select! {
                output = &mut future => return Ok(output),
                () = self.cancel.cancelled() => return Err(Stop::Cancelled),
                () = sleep_until(stale_at.unwrap_or_else(Instant::now)), if stale_at.is_some() => {
                    state.stale_sent = true;
                    state.stale_at = None;
                    self.emit(HostSignalDto::Stale).await?;
                }
            }
        }
    }

    /// 向 UI 发信号（带背压）；会话关闭 / 接收端消失时返回终止原因。
    async fn emit(&self, signal: HostSignalDto) -> Result<(), Stop> {
        tokio::select! {
            () = self.cancel.cancelled() => Err(Stop::Cancelled),
            sent = self.signals.send(signal) => sent.map_err(|_| Stop::ReceiverGone),
        }
    }

    fn mark_disconnected(&self, state: &mut State) {
        if state.ever_synced && state.stale_at.is_none() && !state.stale_sent {
            state.stale_at = Some(Instant::now() + self.timing.stale_grace);
        }
    }

    /// 快照已送达 UI：离线状态结束，首次同步结果通知 `open_*`。
    fn recovered(&mut self, state: &mut State) {
        state.stale_at = None;
        state.stale_sent = false;
        state.ever_synced = true;
        if let Some(first) = self.first.take()
            && first.send(Ok(())).is_err()
        {
            tracing::debug!("open caller went away before the first snapshot");
        }
    }

    async fn session(&mut self, state: &mut State, established: Established) -> SessionEnd {
        let Established {
            hello,
            caller,
            mut events,
        } = established;
        if hello.role != ServiceRole::Agent || hello.protocol_version != PROTOCOL_VERSION {
            return SessionEnd::Stop(Stop::fatal(HostErrorDto::new(
                ErrorCodeDto::ProtocolIncompatible,
                format!(
                    "host speaks protocol {} as {:?}; this app needs protocol {PROTOCOL_VERSION} as agent",
                    hello.protocol_version, hello.role
                ),
            )));
        }
        let info = HostInfoDto::from(&hello);
        self.link
            .send_replace(LinkState::Ready(Arc::clone(&caller)));

        let mut resyncs = 0_u32;
        loop {
            let outcome = self.sync(&caller, events.as_mut()).await;
            let (snapshot, buffered) = match outcome {
                SyncOutcome::End(end) => return end,
                SyncOutcome::Resync => {
                    resyncs += 1;
                    if resyncs > self.timing.max_resyncs {
                        return resync_loop();
                    }
                    continue;
                }
                SyncOutcome::Ready(snapshot, buffered) => (snapshot, buffered),
            };
            let Ok(mut projection) = Projection::from_snapshot(info.clone(), *snapshot) else {
                tracing::warn!("host returned a non-agent snapshot; resyncing");
                resyncs += 1;
                if resyncs > self.timing.max_resyncs {
                    return resync_loop();
                }
                continue;
            };
            self.publish_route(&projection);

            let snapshot_signal = HostSignalDto::Snapshot {
                snapshot: projection.snapshot_dto(),
            };
            if let Err(stop) = self.emit(snapshot_signal).await {
                return SessionEnd::Stop(stop);
            }
            self.recovered(state);

            let loaded = self
                .bootstrap(&caller, &mut projection, events.as_mut(), buffered)
                .await;
            let live = match loaded {
                Ok(()) => self.live(&caller, &mut projection, events.as_mut()).await,
                Err(end) => end,
            };
            match live {
                LiveEnd::End(end) => return end,
                LiveEnd::Resync => {}
            }
            resyncs += 1;
            if resyncs > self.timing.max_resyncs {
                return resync_loop();
            }
        }
    }

    /// 取快照并缓冲同时到达的事件帧。
    async fn sync(&self, caller: &Arc<dyn Caller>, events: &mut dyn EventSource) -> SyncOutcome {
        let call = timeout(
            self.timing.snapshot_timeout,
            caller.call(SYSTEM_SNAPSHOT, None),
        );
        tokio::pin!(call);
        let mut buffered = Vec::new();
        let mut overflowed = false;
        let result = loop {
            tokio::select! {
                result = &mut call => break result,
                event = events.next() => match event {
                    LinkEvent::Frame(frame) => {
                        if buffered.len() < self.timing.buffer_cap {
                            buffered.push(*frame);
                        } else {
                            overflowed = true;
                        }
                    }
                    LinkEvent::Lagged => {
                        return SyncOutcome::End(SessionEnd::Reconnect {
                            immediate: true,
                            reason: "event stream lagged".to_owned(),
                        });
                    }
                    LinkEvent::Closed(info) => return SyncOutcome::End(end_for_close(info)),
                },
                () = self.cancel.cancelled() => {
                    return SyncOutcome::End(SessionEnd::Stop(Stop::Cancelled));
                }
            }
        };
        let value = match result {
            Err(_elapsed) => {
                return SyncOutcome::End(SessionEnd::Reconnect {
                    immediate: false,
                    reason: "system.snapshot timed out".to_owned(),
                });
            }
            Ok(Err(
                error @ FluxError::Rpc {
                    code: ErrorCodeDto::Unauthorized | ErrorCodeDto::ProtocolIncompatible,
                    ..
                },
            )) => {
                return SyncOutcome::End(SessionEnd::Stop(Stop::fatal(error.into())));
            }
            Ok(Err(error)) => {
                return SyncOutcome::End(SessionEnd::Reconnect {
                    immediate: false,
                    reason: format!("system.snapshot failed: {error}"),
                });
            }
            Ok(Ok(value)) => value,
        };
        if overflowed {
            tracing::warn!("event buffer overflowed during snapshot; resyncing");
            return SyncOutcome::Resync;
        }
        match serde_json::from_value::<Snapshot>(value) {
            Ok(snapshot) => SyncOutcome::Ready(Box::new(snapshot), buffered),
            Err(error) => SyncOutcome::End(SessionEnd::Reconnect {
                immediate: false,
                reason: format!("undecodable snapshot: {error}"),
            }),
        }
    }

    /// 稳态：逐帧过游标并下发；帧使投影进入加载态（整表替换）时就地分页重载。
    async fn live(
        &self,
        caller: &Arc<dyn Caller>,
        projection: &mut Projection,
        events: &mut dyn EventSource,
    ) -> LiveEnd {
        loop {
            let event = tokio::select! {
                biased;
                () = self.cancel.cancelled() => {
                    return LiveEnd::End(SessionEnd::Stop(Stop::Cancelled));
                }
                event = events.next() => event,
            };
            match event {
                LinkEvent::Frame(frame) => {
                    let reloaded = match self.apply(projection, *frame).await {
                        Ok(sync) => sync,
                        Err(end) => return end,
                    };
                    if let Some(sync) = reloaded
                        && let Err(end) = self.reload(caller, projection, events, sync).await
                    {
                        return end;
                    }
                }
                LinkEvent::Lagged => {
                    return LiveEnd::End(SessionEnd::Reconnect {
                        immediate: true,
                        reason: "event stream lagged".to_owned(),
                    });
                }
                LinkEvent::Closed(info) => return LiveEnd::End(end_for_close(info)),
            }
        }
    }

    /// 握手后的首轮收尾：快照头带 `taskTotal` / `queueTotal` 时分页载入表，再回放握手期间的帧。
    /// 快照（加载态或完整）已发给 UI，本函数只补「载入完成」的完整快照与缓冲帧产生的信号。
    async fn bootstrap(
        &self,
        caller: &Arc<dyn Caller>,
        projection: &mut Projection,
        events: &mut dyn EventSource,
        buffered: Vec<EventFrame>,
    ) -> Result<(), LiveEnd> {
        // epoch 不同的帧早于快照，丢弃。
        let epoch = projection.epoch().to_owned();
        let buffered = buffered.into_iter().filter(|frame| frame.epoch == epoch);
        match projection.table_sync() {
            Some(mut sync) => {
                for frame in buffered {
                    push_backlog(&mut sync, frame)?;
                }
                self.reload(caller, projection, events, sync).await
            }
            None => {
                self.replay(caller, projection, events, buffered.collect())
                    .await
            }
        }
    }

    /// 载入一轮表并回放期间缓冲的帧。
    async fn reload(
        &self,
        caller: &Arc<dyn Caller>,
        projection: &mut Projection,
        events: &mut dyn EventSource,
        sync: TableSync,
    ) -> Result<(), LiveEnd> {
        let backlog = self.load_tables(caller, projection, events, sync).await?;
        self.replay(caller, projection, events, backlog).await
    }

    /// 按游标规则逐帧回放；回放中遇到又一个整表替换帧则应用它、把余下的帧并入新一轮载入的
    /// 缓冲，载入完成后继续回放。
    async fn replay(
        &self,
        caller: &Arc<dyn Caller>,
        projection: &mut Projection,
        events: &mut dyn EventSource,
        frames: Vec<EventFrame>,
    ) -> Result<(), LiveEnd> {
        let mut queue = VecDeque::from(frames);
        while let Some(frame) = queue.pop_front() {
            let Some(mut sync) = self.apply(projection, frame).await? else {
                continue;
            };
            for pending in queue.drain(..) {
                push_backlog(&mut sync, pending)?;
            }
            queue = VecDeque::from(self.load_tables(caller, projection, events, sync).await?);
        }
        Ok(())
    }

    /// 应用一帧并下发信号；该帧使投影进入加载态时返回新一轮载入。
    async fn apply(
        &self,
        projection: &mut Projection,
        frame: EventFrame,
    ) -> Result<Option<TableSync>, LiveEnd> {
        match projection.accept(frame) {
            FrameOutcome::Skip => Ok(None),
            FrameOutcome::Resync(reason) => {
                tracing::debug!(?reason, "frame requires resync");
                Err(LiveEnd::Resync)
            }
            FrameOutcome::Applied(signals) => {
                self.publish_route(projection);
                for signal in signals {
                    self.emit(signal)
                        .await
                        .map_err(|stop| LiveEnd::End(SessionEnd::Stop(stop)))?;
                }
                Ok(projection.table_sync())
            }
        }
    }

    /// 把投影里当前的分类路由发布给会话（建任务时据此路由）；未变化不通知。
    fn publish_route(&self, projection: &Projection) {
        let route = projection.category_route();
        self.routes.send_if_modified(|current| {
            let changed = *current != route;
            if changed {
                *current = route;
            }
            changed
        });
    }

    /// 经 `system.taskPage` 翻完所有待载入的表，装入投影并下发完整快照，返回期间缓冲的帧。
    ///
    /// 载入期间到达的帧只缓冲不应用；每页有独立超时，整轮有总时限（大表不会被单次快照超时
    /// 误杀）。epoch 变化 / 游标不前进 / 缓冲溢出走重同步，传输层错误走重连。
    async fn load_tables(
        &self,
        caller: &Arc<dyn Caller>,
        projection: &mut Projection,
        events: &mut dyn EventSource,
        mut sync: TableSync,
    ) -> Result<Vec<EventFrame>, LiveEnd> {
        let deadline = Instant::now() + self.timing.load_timeout;
        while let Some(request) = sync.next_request() {
            let params = serde_json::to_value(&request).map_err(|error| {
                reconnect_end(false, format!("encode system.taskPage params: {error}"))
            })?;
            let call = timeout(
                self.timing.page_timeout,
                caller.call(SYSTEM_TASK_PAGE, Some(params)),
            );
            tokio::pin!(call);
            let result = loop {
                tokio::select! {
                    biased;
                    () = self.cancel.cancelled() => {
                        return Err(LiveEnd::End(SessionEnd::Stop(Stop::Cancelled)));
                    }
                    () = sleep_until(deadline) => {
                        return Err(reconnect_end(false, "task table load timed out".to_owned()));
                    }
                    result = &mut call => break result,
                    event = events.next() => match event {
                        LinkEvent::Frame(frame) => {
                            if frame.epoch != sync.epoch() {
                                tracing::debug!("epoch changed during table load; resyncing");
                                return Err(LiveEnd::Resync);
                            }
                            push_backlog(&mut sync, *frame)?;
                        }
                        LinkEvent::Lagged => {
                            return Err(reconnect_end(true, "event stream lagged".to_owned()));
                        }
                        LinkEvent::Closed(info) => return Err(LiveEnd::End(end_for_close(info))),
                    },
                }
            };
            let value = match result {
                Err(_elapsed) => {
                    return Err(reconnect_end(false, "system.taskPage timed out".to_owned()));
                }
                Ok(Err(
                    error @ FluxError::Rpc {
                        code: ErrorCodeDto::Unauthorized | ErrorCodeDto::ProtocolIncompatible,
                        ..
                    },
                )) => {
                    return Err(LiveEnd::End(SessionEnd::Stop(Stop::fatal(error.into()))));
                }
                Ok(Err(error)) => {
                    return Err(reconnect_end(
                        false,
                        format!("system.taskPage failed: {error}"),
                    ));
                }
                Ok(Ok(value)) => value,
            };
            let page = serde_json::from_value::<TablePage>(value)
                .map_err(|error| reconnect_end(false, format!("undecodable task page: {error}")))?;
            if let Err(error) = sync.accept(page) {
                tracing::warn!(?error, "task table load failed; resyncing");
                return Err(LiveEnd::Resync);
            }
        }
        let (signal, backlog) = projection.finish_load(sync);
        self.emit(signal)
            .await
            .map_err(|stop| LiveEnd::End(SessionEnd::Stop(stop)))?;
        Ok(backlog)
    }
}

fn reconnect_end(immediate: bool, reason: String) -> LiveEnd {
    LiveEnd::End(SessionEnd::Reconnect { immediate, reason })
}

/// 载入期间缓冲一帧；超限放弃本轮、整体重同步。
fn push_backlog(sync: &mut TableSync, frame: EventFrame) -> Result<(), LiveEnd> {
    sync.push_frame(frame).map_err(|error| {
        tracing::warn!(?error, "table load backlog overflowed; resyncing");
        LiveEnd::Resync
    })
}

fn resync_loop() -> SessionEnd {
    SessionEnd::Reconnect {
        immediate: false,
        reason: "too many consecutive resyncs".to_owned(),
    }
}

fn end_for_close(info: CloseInfo) -> SessionEnd {
    match info {
        CloseInfo::EventGap => SessionEnd::Reconnect {
            immediate: true,
            reason: "event gap".to_owned(),
        },
        CloseInfo::ServiceQuit => SessionEnd::Stop(Stop::fatal(HostErrorDto::new(
            ErrorCodeDto::Unavailable,
            "host stopped (service-quit)",
        ))),
        CloseInfo::Lost(reason) => SessionEnd::Reconnect {
            immediate: false,
            reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Backoff;

    #[test]
    fn backoff_doubles_from_500ms_to_15s_and_resets() {
        let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(15));
        let delays: Vec<u64> = (0..8)
            .map(|_| u64::try_from(backoff.next_delay(1.0).as_millis()).unwrap_or(u64::MAX))
            .collect();
        assert_eq!(
            delays,
            [500, 1000, 2000, 4000, 8000, 15_000, 15_000, 15_000]
        );
        backoff.reset();
        assert_eq!(backoff.next_delay(1.0), Duration::from_millis(500));
    }

    #[test]
    fn backoff_applies_jitter_factor() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(15));
        assert_eq!(backoff.next_delay(1.2), Duration::from_millis(1200));
        assert_eq!(backoff.next_delay(0.8), Duration::from_millis(1600));
    }
}
