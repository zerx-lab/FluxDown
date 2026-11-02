//! daemon 物化投影、事件游标与广播的单一同步边界。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fluxdown_protocol::{
    DaemonEvent, DaemonSnapshot, EventFrame, ServiceEvent, Snapshot, SnapshotBody, TaskDto,
    WsServerMsg, apply_daemon_event,
};
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug)]
struct EventState {
    epoch: String,
    sequence: u64,
    snapshot: DaemonSnapshot,
    accounting: RuntimeAccounting,
    expiry_running: bool,
}

/// 线性化 daemon 快照与增量事件。
#[derive(Clone)]
pub struct DaemonEventHub {
    state: Arc<Mutex<EventState>>,
    events: broadcast::Sender<EventFrame>,
    /// 可移植视图会话存储：事件映射（渲染 → 帧）与 RPC（open / snapshot / close）共享。
    plugin_views: Arc<crate::plugin_views::PluginViewStore>,
}

impl DaemonEventHub {
    /// 创建新的事件 epoch。
    #[must_use]
    pub fn new(snapshot: DaemonSnapshot, capacity: usize) -> Self {
        let (events, _) = broadcast::channel(capacity);
        let mut state = EventState {
            epoch: Uuid::new_v4().to_string(),
            sequence: 0,
            accounting: RuntimeAccounting::new(&snapshot.tasks, Instant::now()),
            snapshot,
            expiry_running: false,
        };
        update_runtime_stats(&mut state, Instant::now());
        Self {
            state: Arc::new(Mutex::new(state)),
            events,
            plugin_views: Arc::default(),
        }
    }

    /// 可移植视图会话存储（与 [`DaemonEngineEventSink`] 共享同一份）。
    #[must_use]
    pub fn plugin_views(&self) -> &Arc<crate::plugin_views::PluginViewStore> {
        &self.plugin_views
    }

    /// 只订阅增量广播（不克隆快照）；滞后时接收者自行按 [`Self::snapshot`] 复核。
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<EventFrame> {
        self.events.subscribe()
    }

    /// 先订阅广播，再原子克隆快照；调用方丢弃不大于快照 sequence 的帧。
    #[must_use]
    pub fn subscribe_and_snapshot(&self) -> (broadcast::Receiver<EventFrame>, Snapshot) {
        let receiver = self.events.subscribe();
        let state = lock_or_recover(&self.state);
        let snapshot = Snapshot {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            body: SnapshotBody::Daemon(Box::new(state.snapshot.clone())),
        };
        (receiver, snapshot)
    }

    /// 原子读取当前**完整**快照与游标（含任务表，O(任务数) 克隆）。只给进程内使用与测试；
    /// 线上一律走 [`Self::wire_snapshot`]。
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let state = lock_or_recover(&self.state);
        Snapshot {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            body: SnapshotBody::Daemon(Box::new(state.snapshot.clone())),
        }
    }

    /// `system.snapshot` 的线上快照：只带头（任务表与队列位置表换成 `taskTotal` /
    /// `queueTotal`），不克隆任务表。表经 [`Self::task_page`] 分页取。
    #[must_use]
    pub fn wire_snapshot(&self) -> Snapshot {
        let state = lock_or_recover(&self.state);
        Snapshot {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            body: SnapshotBody::Daemon(Box::new(state.snapshot.paged_header())),
        }
    }

    /// `system.taskPage`：在同一临界区内读出事件位置与一页行。
    ///
    /// # Errors
    ///
    /// 游标无法解析。
    pub fn task_page(
        &self,
        params: &fluxdown_protocol::TablePageParams,
    ) -> Result<fluxdown_protocol::TablePage, fluxdown_protocol::InvalidPageCursor> {
        let state = lock_or_recover(&self.state);
        fluxdown_protocol::TablePage::build(&state.snapshot, &state.epoch, state.sequence, params)
    }

    /// 在临界区内只读访问当前投影，只提取调用方需要的字段（不克隆整份快照）。
    /// 闭包内不得再调用本 hub（非重入锁），也不得 `await`。
    pub fn inspect<R>(&self, read: impl FnOnce(&DaemonSnapshot) -> R) -> R {
        let state = lock_or_recover(&self.state);
        read(&state.snapshot)
    }

    /// 先更新物化投影，再递增 sequence 并广播对应帧。
    pub fn publish(&self, event: DaemonEvent) -> EventFrame {
        let mut state = lock_or_recover(&self.state);
        let frame = self.publish_with_stats_locked_at(&mut state, event, Instant::now());
        self.start_expiry(&mut state);
        frame
    }

    /// 发布事件；仅当它改变了运行时统计时，才在同一临界区内紧随发布
    /// `RuntimeStatsChanged`。进度帧每个活动任务约每 0.5s 一次，这条路径
    /// 不能为读取统计去克隆整份快照，也不该广播与上一帧相同的统计。
    pub fn publish_with_runtime_stats(&self, event: DaemonEvent) {
        self.publish(event);
    }

    /// 更新「等待自动重试」任务数；数值不变不发布。
    pub fn set_retry_pending_tasks(&self, count: u32) {
        let mut state = lock_or_recover(&self.state);
        if state.snapshot.runtime_stats.retry_pending_tasks == count {
            return;
        }
        let mut stats = state.snapshot.runtime_stats.clone();
        stats.retry_pending_tasks = count;
        self.publish_locked(&mut state, DaemonEvent::RuntimeStatsChanged(stats));
    }

    fn publish_locked(&self, state: &mut EventState, event: DaemonEvent) -> EventFrame {
        let frame = self.publish_locked_at(state, event, Instant::now());
        self.start_expiry(state);
        frame
    }

    fn publish_with_stats_locked_at(
        &self,
        state: &mut EventState,
        event: DaemonEvent,
        now: Instant,
    ) -> EventFrame {
        let before = runtime_measurements(&state.snapshot.runtime_stats);
        let metadata_changed = match &event {
            DaemonEvent::SnapshotReplaced(snapshot) => {
                snapshot.runtime_stats != state.snapshot.runtime_stats
                    || snapshot.config.values.get("default_save_dir")
                        != state.snapshot.config.values.get("default_save_dir")
            }
            DaemonEvent::ConfigChanged(config) => {
                config.values.get("default_save_dir")
                    != state.snapshot.config.values.get("default_save_dir")
            }
            _ => false,
        };
        let is_stats = matches!(event, DaemonEvent::RuntimeStatsChanged(_));
        let frame = self.publish_locked_at(state, event, now);
        if !is_stats
            && (metadata_changed || runtime_measurements(&state.snapshot.runtime_stats) != before)
        {
            let stats = state.snapshot.runtime_stats.clone();
            self.publish_locked_at(state, DaemonEvent::RuntimeStatsChanged(stats), now);
        }
        frame
    }

    fn publish_locked_at(
        &self,
        state: &mut EventState,
        mut event: DaemonEvent,
        now: Instant,
    ) -> EventFrame {
        state.accounting.apply(&event, now);
        apply_daemon_event(&mut state.snapshot, &event);
        update_runtime_stats(state, now);
        if let DaemonEvent::RuntimeStatsChanged(stats) = &mut event {
            stats.clone_from(&state.snapshot.runtime_stats);
        }
        state.sequence = state.sequence.saturating_add(1);
        let frame = EventFrame {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            previous: None,
            // 投影已用完整事件更新；广播 / 线上形态里整表事件换成头或重载宣告，任何一帧
            // 都不随任务数增长（订阅者经 `system.taskPage` 分页取表）。
            event: ServiceEvent::Daemon(event.into_wire()),
        };
        // 与序号递增处于同一临界区，接收者不会收到乱序帧。
        // 无订阅者是正常空闲状态，不克隆帧或逐条记录进度日志。
        if self.events.receiver_count() != 0 && self.events.send(frame.clone()).is_err() {
            tracing::trace!("daemon event subscribers disconnected before publish");
        }
        frame
    }

    // One weakly owned timer per traffic burst. It exits when the bounded window
    // empties, so a completed task still contributes without keeping the daemon
    // alive or requiring another task event to clear the footer.
    fn start_expiry(&self, state: &mut EventState) {
        if state.expiry_running || !state.accounting.has_traffic() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        state.expiry_running = true;
        let weak = Arc::downgrade(&self.state);
        let events = self.events.clone();
        let plugin_views = self.plugin_views.clone();
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(TRAFFIC_BUCKET).await;
                let Some(shared) = weak.upgrade() else {
                    return;
                };
                let hub = Self {
                    state: shared,
                    events: events.clone(),
                    plugin_views: plugin_views.clone(),
                };
                let mut state = lock_or_recover(&hub.state);
                hub.expire_locked_at(&mut state, Instant::now());
                if !state.accounting.has_traffic() {
                    state.expiry_running = false;
                    return;
                }
            }
        });
    }

    fn expire_locked_at(&self, state: &mut EventState, now: Instant) {
        let before = runtime_measurements(&state.snapshot.runtime_stats);
        update_runtime_stats(state, now);
        if runtime_measurements(&state.snapshot.runtime_stats) != before {
            let stats = state.snapshot.runtime_stats.clone();
            self.publish_locked_at(state, DaemonEvent::RuntimeStatsChanged(stats), now);
        }
    }

    /// 原子替换投影并发布替换事件。
    pub fn replace_snapshot(&self, snapshot: DaemonSnapshot) -> EventFrame {
        self.publish(DaemonEvent::SnapshotReplaced(snapshot))
    }
}

// Fixed buckets bound both memory and per-event work independently of the
// number of tasks. Rates are bytes actually transferred, not a sum of task
// EMAs that disappears on completion. Each progress frame's delta is spread
// uniformly over the interval since that task's previous sample (a task
// reports at most every ~500 ms, and a small file may finish in one lump), so
// lumpy reporting does not turn into a sawtooth. The rate averages a
// two-second window that ends `REPORT_LAG` before now, so every bucket in it
// has already been reported; the published value changes at most every
// `PUBLISH_PERIOD`, which keeps the footer readable.
const TRAFFIC_BUCKET: Duration = Duration::from_millis(100);
const TRAFFIC_BUCKET_US: u128 = 100_000;
/// Ring capacity: must cover lag + window + the longest spread.
const TRAFFIC_BUCKETS: usize = 40;
/// Averaging window (2 s).
const RATE_WINDOW_BUCKETS: u128 = 20;
/// Window end lag: per-task progress frames are at least 500 ms apart.
const REPORT_LAG_BUCKETS: u128 = 7;
/// Republish cadence for the rate (500 ms).
const PUBLISH_PERIOD_BUCKETS: u128 = 5;
/// Longest interval one delta is spread over (paused / long-silent tasks).
const SPREAD_CAP_US: u128 = 1_000_000;

#[derive(Debug, Clone, Copy, Default)]
struct TrafficBucket {
    tick: u128,
    downloaded: i64,
    uploaded: i64,
}

impl TrafficBucket {
    fn has_bytes(&self) -> bool {
        self.downloaded != 0 || self.uploaded != 0
    }
}

#[derive(Debug)]
struct TaskSample {
    status: i32,
    downloaded: i64,
    uploaded: i64,
    explicit_baseline: bool,
    /// The engine reports received body bytes for this transfer interval
    /// (`EngineEvent::TaskTransferred`): they are the traffic, and progress
    /// deltas only move the high-water mark. Cleared when the task leaves the
    /// active states or gets a new explicit baseline.
    transfer_measured: bool,
    /// When this task's bytes were last accounted (or it became active):
    /// the start of the interval the next delta is spread over.
    last_at: Option<Instant>,
}

#[derive(Debug)]
struct RuntimeAccounting {
    tasks: HashMap<String, TaskSample>,
    active: u32,
    pending: u32,
    origin: Instant,
    buckets: [TrafficBucket; TRAFFIC_BUCKETS],
    /// First bucket of the current traffic burst: the average never spans the
    /// idle time before it, so a fresh burst is not diluted.
    burst_start: Option<u128>,
    /// `(publish period, download bps, upload bps)` last computed.
    published: Option<(u128, i64, i64)>,
}

fn is_active(status: i32) -> bool {
    matches!(status, 1 | 5)
}

/// `value * part / whole` without overflow (`whole > 0`).
fn proportion(value: i64, part: u128, whole: u128) -> i64 {
    let scaled = i128::from(value) * i128::try_from(part).unwrap_or(i128::MAX)
        / i128::try_from(whole).unwrap_or(i128::MAX);
    i64::try_from(scaled).unwrap_or(i64::MAX)
}

impl RuntimeAccounting {
    fn new(tasks: &[TaskDto], now: Instant) -> Self {
        let mut accounting = Self {
            tasks: HashMap::with_capacity(tasks.len()),
            active: 0,
            pending: 0,
            origin: now,
            buckets: [TrafficBucket::default(); TRAFFIC_BUCKETS],
            burst_start: None,
            published: None,
        };
        accounting.reconcile(tasks);
        accounting
    }

    fn remove_status(&mut self, status: i32) {
        self.active = self.active.saturating_sub(u32::from(is_active(status)));
        self.pending = self.pending.saturating_sub(u32::from(status == 0));
    }

    fn add_status(&mut self, status: i32) {
        self.active = self.active.saturating_add(u32::from(is_active(status)));
        self.pending = self.pending.saturating_add(u32::from(status == 0));
    }

    fn transfer_baseline(&mut self, task_id: &str, downloaded_bytes: i64) {
        if let Some(sample) = self.tasks.get_mut(task_id) {
            sample.downloaded = downloaded_bytes.max(0);
            sample.explicit_baseline = true;
            sample.transfer_measured = false;
        }
    }

    fn settle_download(&mut self, task_id: &str, downloaded_bytes: i64, now: Instant) {
        if let Some(sample) = self.tasks.get_mut(task_id) {
            let delta = if sample.transfer_measured {
                0
            } else {
                downloaded_bytes.saturating_sub(sample.downloaded).max(0)
            };
            sample.downloaded = sample.downloaded.max(downloaded_bytes);
            let since = sample.last_at.replace(now);
            self.record_traffic(delta, 0, since, now);
        }
    }

    /// Received body bytes since the task's previous frame. Marks the task as
    /// transfer-measured so its progress deltas are not counted twice.
    fn record_transfer(&mut self, task_id: &str, bytes: i64, now: Instant) {
        if let Some(sample) = self.tasks.get_mut(task_id) {
            sample.transfer_measured = true;
            let since = sample.last_at.replace(now);
            self.record_traffic(bytes.max(0), 0, since, now);
        }
    }

    fn micros(&self, at: Instant) -> u128 {
        at.saturating_duration_since(self.origin).as_micros()
    }

    fn tick(&self, at: Instant) -> u128 {
        self.micros(at) / TRAFFIC_BUCKET_US
    }

    /// Whether any bucket that can still reach the rate window holds bytes.
    fn recent_traffic(&self, tick: u128) -> bool {
        let horizon = tick.saturating_sub(REPORT_LAG_BUCKETS + RATE_WINDOW_BUCKETS);
        self.buckets
            .iter()
            .any(|bucket| bucket.has_bytes() && bucket.tick >= horizon)
    }

    /// Spread `downloaded`/`uploaded` uniformly over `[since, now]` (capped at
    /// [`SPREAD_CAP_US`]); bytes are conserved exactly.
    fn record_traffic(
        &mut self,
        downloaded: i64,
        uploaded: i64,
        since: Option<Instant>,
        now: Instant,
    ) {
        if downloaded == 0 && uploaded == 0 {
            return;
        }
        let end = self.micros(now);
        let start = since
            .map_or(end, |since| self.micros(since))
            .max(end.saturating_sub(SPREAD_CAP_US))
            .min(end);
        let (first_tick, last_tick) = (start / TRAFFIC_BUCKET_US, end / TRAFFIC_BUCKET_US);
        if !self.recent_traffic(last_tick) {
            self.burst_start = Some(first_tick);
            self.published = None;
        }
        let span = end - start;
        let (mut downloaded_left, mut uploaded_left) = (downloaded, uploaded);
        for tick in first_tick..=last_tick {
            let (down, up) = if tick == last_tick {
                (downloaded_left, uploaded_left)
            } else {
                let low = start.max(tick * TRAFFIC_BUCKET_US);
                let high = end.min((tick + 1) * TRAFFIC_BUCKET_US);
                let share = high.saturating_sub(low);
                (
                    proportion(downloaded, share, span),
                    proportion(uploaded, share, span),
                )
            };
            downloaded_left = downloaded_left.saturating_sub(down);
            uploaded_left = uploaded_left.saturating_sub(up);
            self.add_to_bucket(tick, down, up);
        }
    }

    fn add_to_bucket(&mut self, tick: u128, downloaded: i64, uploaded: i64) {
        let slot = (tick % TRAFFIC_BUCKETS as u128) as usize;
        let bucket = &mut self.buckets[slot];
        if bucket.tick > tick {
            // Older than the ring (cannot happen within the spread cap).
            return;
        }
        if bucket.tick != tick {
            *bucket = TrafficBucket {
                tick,
                ..TrafficBucket::default()
            };
        }
        bucket.downloaded = bucket.downloaded.saturating_add(downloaded);
        // Completed torrents (status=3) still upload while seeding.
        bucket.uploaded = bucket.uploaded.saturating_add(uploaded);
    }

    fn baseline(&mut self, task: &TaskDto, now: Instant) {
        let previous_status = if let Some(sample) = self.tasks.get_mut(&task.task_id) {
            let previous = sample.status;
            sample.status = task.status;
            // Once the transport supplied its actual starting point, a DB
            // checkpoint cannot replace it (it can lag a reset or be ahead of
            // progress still waiting in the reporter's channel).
            if !sample.explicit_baseline {
                sample.downloaded = sample.downloaded.max(task.downloaded_bytes);
            }
            sample.uploaded = sample.uploaded.max(task.uploaded_bytes);
            if is_active(task.status) && !is_active(previous) {
                sample.last_at = Some(now);
            }
            if !is_active(task.status) {
                sample.transfer_measured = false;
            }
            Some(previous)
        } else {
            self.tasks.insert(
                task.task_id.clone(),
                TaskSample {
                    status: task.status,
                    downloaded: task.downloaded_bytes.max(0),
                    uploaded: task.uploaded_bytes.max(0),
                    explicit_baseline: false,
                    transfer_measured: false,
                    last_at: is_active(task.status).then_some(now),
                },
            );
            None
        };
        if let Some(status) = previous_status {
            self.remove_status(status);
        }
        self.add_status(task.status);
    }

    fn reconcile(&mut self, tasks: &[TaskDto]) {
        let live: std::collections::HashSet<&str> =
            tasks.iter().map(|task| task.task_id.as_str()).collect();
        self.tasks.retain(|id, _| live.contains(id.as_str()));
        // Full replacements are the only O(N) accounting path.
        self.active = 0;
        self.pending = 0;
        for task in tasks {
            if let Some(sample) = self.tasks.get_mut(&task.task_id) {
                sample.status = task.status;
                if !sample.explicit_baseline {
                    sample.downloaded = sample.downloaded.max(task.downloaded_bytes);
                }
                sample.uploaded = sample.uploaded.max(task.uploaded_bytes);
                if !is_active(task.status) {
                    sample.transfer_measured = false;
                }
            } else {
                self.tasks.insert(
                    task.task_id.clone(),
                    TaskSample {
                        status: task.status,
                        downloaded: task.downloaded_bytes.max(0),
                        uploaded: task.uploaded_bytes.max(0),
                        explicit_baseline: false,
                        transfer_measured: false,
                        last_at: None,
                    },
                );
            }
            self.add_status(task.status);
        }
    }

    fn remove(&mut self, id: &str) {
        if let Some(sample) = self.tasks.remove(id) {
            self.remove_status(sample.status);
        }
    }

    fn apply(&mut self, event: &DaemonEvent, now: Instant) {
        match event {
            DaemonEvent::SnapshotReplaced(snapshot) => self.reconcile(&snapshot.tasks),
            DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks }) => self.reconcile(tasks),
            DaemonEvent::TaskChanged(task) => self.baseline(task, now),
            DaemonEvent::TasksChanged(tasks) => {
                for task in tasks {
                    self.baseline(task, now);
                }
            }
            DaemonEvent::TaskDeleted { task_id } => self.remove(task_id),
            DaemonEvent::TasksDeleted { task_ids } => {
                for task_id in task_ids {
                    self.remove(task_id);
                }
            }
            DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                uploaded_bytes,
                error_message,
                ..
            }) => {
                if *status == 4 && error_message == "deleted" {
                    self.remove(task_id);
                    return;
                }
                // The protocol projection also ignores unknown/deleted tasks.
                // Counting an unannounced first sample would count restored
                // bytes as traffic, and allow late frames to resurrect totals.
                let Some(sample) = self.tasks.get_mut(task_id) else {
                    return;
                };
                let old_status = sample.status;
                sample.status = *status;
                // Transfer-measured tasks already accounted their received
                // bytes; their progress is written bytes (or a final size).
                let downloaded = if sample.transfer_measured {
                    0
                } else {
                    downloaded_bytes.saturating_sub(sample.downloaded).max(0)
                };
                let uploaded = uploaded_bytes.saturating_sub(sample.uploaded).max(0);
                // Keep high water marks within a transfer interval. Only the
                // engine's ordered, explicit baseline can reset that interval;
                // arbitrary regressing samples and snapshots cannot.
                sample.downloaded = sample.downloaded.max(*downloaded_bytes);
                sample.uploaded = sample.uploaded.max(*uploaded_bytes);
                if !is_active(*status) {
                    sample.transfer_measured = false;
                }
                let since = sample.last_at.replace(now);
                self.remove_status(old_status);
                self.add_status(*status);
                self.record_traffic(downloaded, uploaded, since, now);
            }
            _ => {}
        }
    }

    /// Download / upload rate (bytes/s) over the lagged window; recomputed at
    /// most once per [`PUBLISH_PERIOD_BUCKETS`].
    fn rates(&mut self, now: Instant) -> (i64, i64) {
        let tick = self.tick(now);
        for bucket in &mut self.buckets {
            if tick.saturating_sub(bucket.tick) >= TRAFFIC_BUCKETS as u128 {
                *bucket = TrafficBucket::default();
            }
        }
        let period = tick / PUBLISH_PERIOD_BUCKETS;
        if let Some((published, downloaded, uploaded)) = self.published
            && published == period
        {
            return (downloaded, uploaded);
        }
        let end = tick.saturating_sub(REPORT_LAG_BUCKETS);
        let window_start = end.saturating_sub(RATE_WINDOW_BUCKETS);
        let first = self
            .burst_start
            .map_or(window_start, |burst| burst.max(window_start));
        if !self.recent_traffic(tick) {
            self.burst_start = None;
        }
        let buckets = end.saturating_sub(first);
        if buckets == 0 {
            // The burst has not reached the reported window yet.
            return (0, 0);
        }
        let (mut downloaded, mut uploaded) = (0_i64, 0_i64);
        for bucket in &self.buckets {
            if bucket.has_bytes() && bucket.tick >= first && bucket.tick < end {
                downloaded = downloaded.saturating_add(bucket.downloaded);
                uploaded = uploaded.saturating_add(bucket.uploaded);
            }
        }
        let per_second = 1_000_000 / TRAFFIC_BUCKET_US;
        let rates = (
            proportion(downloaded, per_second, buckets),
            proportion(uploaded, per_second, buckets),
        );
        self.published = Some((period, rates.0, rates.1));
        rates
    }

    fn has_traffic(&self) -> bool {
        self.buckets.iter().any(TrafficBucket::has_bytes)
    }

    /// Every byte still held in the ring (exact accounting, rate-independent).
    #[cfg(test)]
    fn window_bytes(&self) -> (i64, i64) {
        self.buckets.iter().fold((0, 0), |(down, up), bucket| {
            (down + bucket.downloaded, up + bucket.uploaded)
        })
    }
}

fn runtime_measurements(stats: &fluxdown_protocol::DaemonRuntimeStatsDto) -> (u32, u32, i64, i64) {
    (
        stats.active_tasks,
        stats.pending_tasks,
        stats.total_download_bps,
        stats.total_upload_bps,
    )
}

fn update_runtime_stats(state: &mut EventState, now: Instant) {
    state.snapshot.runtime_stats.active_tasks = state.accounting.active;
    state.snapshot.runtime_stats.pending_tasks = state.accounting.pending;
    let (downloaded, uploaded) = state.accounting.rates(now);
    state.snapshot.runtime_stats.total_download_bps = downloaded;
    state.snapshot.runtime_stats.total_upload_bps = uploaded;
    if let Some(save_dir) = state
        .snapshot
        .config
        .values
        .get("default_save_dir")
        .filter(|path| !path.trim().is_empty() && **path != state.snapshot.runtime_stats.save_dir)
    {
        state.snapshot.runtime_stats.save_dir.clone_from(save_dir);
    }
}

/// 将引擎事件无阻塞转换并发布到 daemon 事件中心。
pub struct DaemonEngineEventSink(pub DaemonEventHub);

impl fluxdown_engine::events::EventSink for DaemonEngineEventSink {
    fn emit(&self, event: fluxdown_engine::events::EngineEvent) {
        use fluxdown_engine::events::EngineEvent;
        match event {
            EngineEvent::TaskTransferred { task_id, bytes } => {
                // Host-local like the baseline: the progress frame that follows
                // publishes the updated runtime stats.
                lock_or_recover(&self.0.state).accounting.record_transfer(
                    &task_id,
                    bytes,
                    Instant::now(),
                );
                return;
            }
            EngineEvent::TaskTransferBaseline {
                task_id,
                downloaded_bytes,
                previous_downloaded_bytes,
            } => {
                let mut state = lock_or_recover(&self.0.state);
                if let Some(previous) = previous_downloaded_bytes {
                    state
                        .accounting
                        .settle_download(&task_id, previous, Instant::now());
                }
                state
                    .accounting
                    .transfer_baseline(&task_id, downloaded_bytes);
                return;
            }
            EngineEvent::TaskChanged(task) => {
                self.0.publish_with_runtime_stats(DaemonEvent::TaskChanged(
                    fluxdown_engine_protocol::task_info_to_dto(*task),
                ));
                return;
            }
            EngineEvent::TasksChanged(tasks) => {
                self.0.publish_with_runtime_stats(DaemonEvent::TasksChanged(
                    tasks
                        .into_iter()
                        .map(fluxdown_engine_protocol::task_info_to_dto)
                        .collect(),
                ));
                return;
            }
            EngineEvent::TasksDeleted(task_ids) => {
                self.0
                    .publish_with_runtime_stats(DaemonEvent::TasksDeleted { task_ids });
                return;
            }
            EngineEvent::TaskRuntimeChanged(runtime) => {
                self.0.publish(DaemonEvent::TaskRuntimeChanged(
                    fluxdown_engine_protocol::task_runtime_to_dto(runtime),
                ));
                return;
            }
            EngineEvent::TaskActivityAdded(activity) => {
                self.0.publish(DaemonEvent::TaskActivityAdded(
                    fluxdown_engine_protocol::task_activity_to_dto(activity),
                ));
                return;
            }
            EngineEvent::RetryPendingChanged { count } => {
                self.0.set_retry_pending_tasks(count);
                return;
            }
            EngineEvent::TaskNotice(notice) => {
                self.0.publish(DaemonEvent::TaskNotice(
                    fluxdown_engine_protocol::task_notice_to_dto(notice),
                ));
                return;
            }
            EngineEvent::BtGuardChanged => {
                self.0.publish(DaemonEvent::BtGuardChanged);
                return;
            }
            EngineEvent::BtGuardCloudRefreshWanted { have_sequence } => {
                self.0
                    .publish(DaemonEvent::BtGuardCloudRefreshWanted { have_sequence });
                return;
            }
            EngineEvent::BtGuardEvidenceReady { count } => {
                self.0.publish(DaemonEvent::BtGuardEvidenceReady { count });
                return;
            }
            _ => {}
        }

        let message = match event {
            EngineEvent::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                total_bytes,
                speed,
                file_name,
                save_dir,
                url,
                error_message,
                upload_speed_bps,
                uploaded_bytes,
                seeding_status,
                seeding_message,
                seeding_time_secs,
                ..
            } => WsServerMsg::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                total_bytes,
                speed,
                upload_speed: upload_speed_bps,
                file_name,
                save_dir,
                url,
                error_message,
                uploaded_bytes,
                seeding_status,
                seeding_message,
                seeding_time_secs,
            },
            EngineEvent::TasksSnapshot(tasks) => WsServerMsg::TasksSnapshot {
                tasks: tasks
                    .into_iter()
                    .map(fluxdown_engine_protocol::task_info_to_dto)
                    .collect(),
            },
            EngineEvent::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments,
            } => WsServerMsg::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments: segments
                    .into_iter()
                    .map(fluxdown_engine_protocol::segment_detail_to_dto)
                    .collect(),
            },
            EngineEvent::TaskMetaProbed {
                task_id,
                file_name,
                total_bytes,
            } => WsServerMsg::TaskMetaProbed {
                task_id,
                file_name,
                total_bytes,
            },
            EngineEvent::QueuePositionsChanged(positions) => WsServerMsg::QueuePositionsChanged {
                positions: positions
                    .into_iter()
                    .map(fluxdown_engine_protocol::queue_position_to_dto)
                    .collect(),
            },
            EngineEvent::QueueOrderChanged { removed, added } => WsServerMsg::QueueOrderChanged {
                removed,
                added: added
                    .into_iter()
                    .map(fluxdown_engine_protocol::queue_order_entry_to_dto)
                    .collect(),
            },
            EngineEvent::QueuesChanged(queues) => WsServerMsg::QueuesChanged {
                queues: queues
                    .into_iter()
                    .map(fluxdown_engine_protocol::queue_info_to_dto)
                    .collect(),
            },
            EngineEvent::TaskQueueChanged { task_id, queue_id } => {
                WsServerMsg::TaskQueueChanged { task_id, queue_id }
            }
            EngineEvent::TaskRouteChanged { task_id, route } => {
                WsServerMsg::TaskRouteChanged { task_id, route }
            }
            EngineEvent::PriorityTaskChanged {
                priority_task_id,
                auto_paused_count,
            } => WsServerMsg::PriorityTaskChanged {
                priority_task_id,
                auto_paused_count,
            },
            EngineEvent::SegmentSplit {
                task_id,
                parent_index,
                parent_new_end,
                child_index,
                child_start,
                child_end,
                is_proactive,
                total_segments,
            } => WsServerMsg::SegmentSplit {
                task_id,
                parent_index,
                parent_new_end,
                child_index,
                child_start,
                child_end,
                is_proactive,
                total_segments,
            },
            EngineEvent::TaskCdnEvent {
                task_id,
                kind,
                host,
                nodes,
                ip,
                reason,
                candidates,
                alive,
                cap,
                auto_cap,
            } => WsServerMsg::TaskCdnEvent {
                task_id,
                kind,
                host,
                nodes: nodes
                    .into_iter()
                    .map(fluxdown_engine_protocol::cdn_node_info_to_dto)
                    .collect(),
                ip,
                reason,
                candidates,
                alive,
                cap,
                auto_cap,
            },
            EngineEvent::BtDataFinished { .. } => return,
            EngineEvent::PluginViewRendered { session, tree } => {
                if let Some(frame) = self.0.plugin_views.rendered(&session, &tree) {
                    self.0.publish(DaemonEvent::PluginViewFrame(frame));
                }
                return;
            }
            EngineEvent::PluginViewFailed { session, message } => {
                if let Some(frame) = self.0.plugin_views.failed(&session, &message) {
                    self.0.publish(DaemonEvent::PluginViewFrame(frame));
                }
                return;
            }
            EngineEvent::PluginViewClosed { session, reason } => {
                self.0.plugin_views.close(&session);
                self.0
                    .publish(DaemonEvent::PluginViewClosed { session, reason });
                return;
            }
            EngineEvent::PluginViewNavigate { session, target } => {
                let frame = self
                    .0
                    .plugin_views
                    .effect(&session, fluxdown_protocol::ViewEffect::Navigate { target });
                self.0.publish(DaemonEvent::PluginViewFrame(frame));
                return;
            }
            EngineEvent::PluginToast {
                identity,
                level,
                text,
            } => {
                self.0.publish(DaemonEvent::PluginToast {
                    identity,
                    level,
                    text,
                });
                return;
            }
            #[cfg(feature = "plugins")]
            EngineEvent::PluginsChanged(plugins) => {
                self.0.publish(DaemonEvent::PluginsChanged(
                    plugins
                        .into_iter()
                        .map(fluxdown_engine_protocol::plugin_info_to_dto)
                        .collect(),
                ));
                return;
            }
            EngineEvent::PluginAutoDisabled { identity, reason } => {
                WsServerMsg::PluginAutoDisabled { identity, reason }
            }
            EngineEvent::DuplicateTorrentDetected {
                task_id,
                existing_task_id,
                existing_name,
            } => WsServerMsg::DuplicateTorrent {
                task_id,
                existing_task_id,
                existing_name,
            },
            EngineEvent::PluginHookActivity {
                task_id,
                plugin_id,
                running,
            } => WsServerMsg::PluginHookActivity {
                task_id,
                plugin_id,
                running,
            },
            EngineEvent::GroupsChanged(groups) => WsServerMsg::GroupsChanged {
                groups: groups
                    .into_iter()
                    .map(fluxdown_engine_protocol::group_info_to_dto)
                    .collect(),
            },
            EngineEvent::RssSourcesChanged(sources) => WsServerMsg::RssSourcesChanged {
                sources: sources
                    .into_iter()
                    .map(fluxdown_engine_protocol::rss_source_info_to_dto)
                    .collect(),
            },
            EngineEvent::RssItemsChanged {
                source_id,
                items,
                notify_titles,
            } => WsServerMsg::RssItemsChanged {
                source_id,
                items: items
                    .into_iter()
                    .map(fluxdown_engine_protocol::rss_item_info_to_dto)
                    .collect(),
                notify_titles,
            },
            EngineEvent::RssFeedValidated {
                request_id,
                url,
                feed_title,
                items,
                error,
            } => WsServerMsg::RssFeedValidated {
                request_id,
                url,
                feed_title,
                items: items
                    .into_iter()
                    .map(fluxdown_engine_protocol::rss_item_info_to_dto)
                    .collect(),
                error,
            },
            EngineEvent::WebhookDeliveriesChanged(deliveries) => {
                WsServerMsg::WebhookDeliveriesChanged {
                    deliveries: deliveries
                        .into_iter()
                        .map(fluxdown_engine_protocol::webhook_delivery_to_dto)
                        .collect(),
                }
            }
            EngineEvent::FileMissingChanged(updates) => WsServerMsg::FileMissingChanged {
                updates: updates
                    .into_iter()
                    .map(
                        |(task_id, missing)| fluxdown_protocol::FileMissingUpdateDto {
                            task_id,
                            missing,
                        },
                    )
                    .collect(),
            },
            other => {
                tracing::debug!(?other, "daemon ignored unknown engine event");
                return;
            }
        };
        let updates_runtime = matches!(
            message,
            WsServerMsg::TaskProgress { .. } | WsServerMsg::TasksSnapshot { .. }
        );
        let event = match message {
            WsServerMsg::QueuesChanged { queues } => DaemonEvent::QueuesChanged(queues),
            WsServerMsg::GroupsChanged { groups } => DaemonEvent::GroupsChanged(groups),
            WsServerMsg::WebhookDeliveriesChanged { deliveries } => {
                DaemonEvent::WebhooksChanged(deliveries)
            }
            message => DaemonEvent::Engine(message),
        };
        if updates_runtime {
            self.0.publish_with_runtime_stats(event);
        } else {
            self.0.publish(event);
        }
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use fluxdown_engine::{
        events::{EngineEvent, EventSink},
        model::QueueInfo,
    };
    use fluxdown_protocol::{
        DaemonEvent, DaemonSnapshot, ServiceEvent, SnapshotBody, TaskDto, WsServerMsg,
    };

    use super::{DaemonEngineEventSink, DaemonEventHub};

    /// 引擎的队列顺序增量经 hub 以 `QueueOrderChanged` 发出，并由共享 reducer 更新快照里的
    /// `queue_positions`（名次 = 按 `order` 升序的下标 + 1）。
    #[test]
    fn queue_order_increments_reach_subscribers_and_update_the_snapshot() {
        use fluxdown_engine::model::{QueueOrderEntry, QueuePosition};

        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 8);
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        let sink = DaemonEngineEventSink(hub.clone());
        let positions = |hub: &DaemonEventHub| {
            let SnapshotBody::Daemon(snapshot) = hub.snapshot().body else {
                panic!("daemon snapshot expected");
            };
            snapshot
                .queue_positions
                .iter()
                .map(|entry| (entry.task_id.clone(), entry.position, entry.order))
                .collect::<Vec<_>>()
        };

        sink.emit(EngineEvent::QueuePositionsChanged(
            ["a", "b", "c"]
                .iter()
                .zip([10_i64, 20, 30])
                .zip(1..)
                .map(|((id, order), position)| QueuePosition {
                    task_id: (*id).to_owned(),
                    position,
                    order,
                })
                .collect(),
        ));
        subscriber.try_recv().expect("full table frame");
        assert_eq!(
            positions(&hub),
            [
                ("a".into(), 1, 10),
                ("b".into(), 2, 20),
                ("c".into(), 3, 30)
            ]
        );

        // 队首出队 + 在两者之间插入新任务：其余任务的键不变，名次随之前移 / 后移。
        sink.emit(EngineEvent::QueueOrderChanged {
            removed: vec!["a".to_owned()],
            added: vec![QueueOrderEntry {
                task_id: "d".to_owned(),
                order: 25,
            }],
        });
        let frame = subscriber.try_recv().expect("incremental frame");
        let ServiceEvent::Daemon(DaemonEvent::Engine(WsServerMsg::QueueOrderChanged {
            removed,
            added,
        })) = frame.event
        else {
            panic!("queue order increments must reach subscribers as queueOrderChanged");
        };
        assert_eq!(removed, ["a"]);
        assert_eq!((added[0].task_id.as_str(), added[0].order), ("d", 25));
        assert_eq!(
            positions(&hub),
            [
                ("b".into(), 1, 20),
                ("d".into(), 2, 25),
                ("c".into(), 3, 30)
            ]
        );
    }

    #[test]
    fn queue_engine_changes_reach_subscribers_as_queue_domain_events() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 8);
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        let sink = DaemonEngineEventSink(hub.clone());
        let queue = QueueInfo {
            queue_id: "work".into(),
            name: "Work".into(),
            speed_limit_kbps: 0,
            upload_limit_kbps: 0,
            max_concurrent: 0,
            default_save_dir: String::new(),
            position: 1,
            default_segments: 0,
            default_user_agent: String::new(),
            is_running: true,
            schedule_enabled: false,
            schedule_start: String::new(),
            schedule_stop: String::new(),
            schedule_days: 127,
        };
        for queues in [
            vec![queue.clone()],
            vec![QueueInfo {
                name: "Renamed".into(),
                is_running: false,
                ..queue
            }],
            vec![],
        ] {
            sink.emit(EngineEvent::QueuesChanged(queues));
            let frame = subscriber
                .try_recv()
                .expect("subscriber receives queue change");
            let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::QueuesChanged(changed)) =
                frame.event
            else {
                panic!("queue changes must use the domain event consumed by live windows");
            };
            let SnapshotBody::Daemon(snapshot) = hub.snapshot().body else {
                panic!("daemon snapshot expected");
            };
            assert_eq!(
                serde_json::to_value(snapshot.queues).unwrap(),
                serde_json::to_value(changed).unwrap()
            );
        }
    }

    #[test]
    fn groups_and_webhook_deliveries_reach_subscribers_as_domain_events() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 8);
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        let sink = DaemonEngineEventSink(hub.clone());
        sink.emit(EngineEvent::GroupsChanged(vec![
            fluxdown_engine::model::GroupInfo {
                group_id: "group-1".into(),
                name: "Collection".into(),
                source_url: String::new(),
                save_dir: String::new(),
                created_at: String::new(),
            },
        ]));
        let frame = subscriber
            .try_recv()
            .expect("subscriber receives group change");
        let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::GroupsChanged(groups)) =
            frame.event
        else {
            panic!("groups must use the domain event consumed by live windows");
        };
        assert_eq!(groups[0].name, "Collection");

        sink.emit(EngineEvent::WebhookDeliveriesChanged(vec![
            fluxdown_engine::webhook::WebhookDelivery {
                delivery_id: "delivery-1".into(),
                timestamp_ms: 1,
                event: "task.completed".into(),
                endpoint_id: String::new(),
                endpoint_name: String::new(),
                url: String::new(),
                request_headers: String::new(),
                request_body: String::new(),
                status_code: 200,
                response_body: String::new(),
                latency_ms: 1,
                attempts: 1,
                success: true,
                error: String::new(),
            },
        ]));
        let frame = subscriber
            .try_recv()
            .expect("subscriber receives delivery change");
        let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::WebhooksChanged(deliveries)) =
            frame.event
        else {
            panic!("deliveries must use the domain event consumed by live settings");
        };
        assert_eq!(deliveries[0].delivery_id, "delivery-1");
    }

    #[test]
    fn task_notice_maps_to_the_dedicated_daemon_event() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 8);
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        let sink = DaemonEngineEventSink(hub);
        sink.emit(EngineEvent::TaskNotice(
            fluxdown_engine::webhook::TaskNotice {
                delivery_id: "d-1".into(),
                event: "task.completed".into(),
                timestamp_ms: 42,
                queue_id: "default".into(),
                queue_name: "Default".into(),
                task: Some(fluxdown_engine::webhook::WebhookTask {
                    id: "t-1".into(),
                    file_name: "a.bin".into(),
                    url: "https://example.com/a.bin".into(),
                    save_dir: "/tmp".into(),
                    total_bytes: 9,
                    status: 3,
                    error_message: String::new(),
                }),
            },
        ));
        let frame = subscriber.try_recv().expect("notice is published");
        let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::TaskNotice(notice)) = frame.event
        else {
            panic!("task notices use the dedicated event, not an engine message");
        };
        assert_eq!(notice.delivery_id, "d-1");
        assert_eq!(notice.event, "task.completed");
        assert_eq!(notice.timestamp_ms, 42);
        let task = notice.task.expect("task snapshot");
        assert_eq!(
            (task.id.as_str(), task.total_bytes, task.status),
            ("t-1", 9, 3)
        );
    }

    #[test]
    fn retry_pending_count_reaches_runtime_stats_once_per_change() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 8);
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        let sink = DaemonEngineEventSink(hub.clone());
        sink.emit(EngineEvent::RetryPendingChanged { count: 2 });
        sink.emit(EngineEvent::RetryPendingChanged { count: 2 });
        let frame = subscriber.try_recv().expect("a changed count is published");
        let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::RuntimeStatsChanged(stats)) =
            frame.event
        else {
            panic!("retry-pending count travels in the runtime stats projection");
        };
        assert_eq!(stats.retry_pending_tasks, 2);
        assert!(
            subscriber.try_recv().is_err(),
            "an unchanged count must not be republished"
        );
        let SnapshotBody::Daemon(snapshot) = hub.snapshot().body else {
            panic!("daemon snapshot expected");
        };
        assert_eq!(snapshot.runtime_stats.retry_pending_tasks, 2);

        sink.emit(EngineEvent::RetryPendingChanged { count: 0 });
        let SnapshotBody::Daemon(snapshot) = hub.snapshot().body else {
            panic!("daemon snapshot expected");
        };
        assert_eq!(snapshot.runtime_stats.retry_pending_tasks, 0);
    }

    fn engine_task(id: &str, status: i32) -> fluxdown_engine::model::TaskInfo {
        fluxdown_engine::model::TaskInfo {
            task_id: id.to_owned(),
            url: format!("https://example.com/{id}"),
            file_name: format!("{id}.bin"),
            save_dir: "/tmp".to_owned(),
            status,
            downloaded_bytes: 0,
            total_bytes: 0,
            error_message: String::new(),
            created_at: "1".to_owned(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            segments: 0,
            queue_order: 1,
            uploaded_bytes: 0,
            uploaded_at_completion: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
            seed_ratio_limit_milli: -2,
            seed_post_ratio_limit_milli: -2,
            seed_time_limit_minutes: -2,
            seed_inactive_time_limit_minutes: -2,
            seed_upload_limit_bps: 0,
            referrer: String::new(),
            group_id: String::new(),
            rss_source_id: String::new(),
            origin_url: String::new(),
            auto_route: String::new(),
            source_bytes: Default::default(),
        }
    }

    /// 批量创建事实只发一帧 `TasksChanged`，投影与运行统计按任务逐个入账。
    #[test]
    fn tasks_changed_is_one_frame_and_accounts_every_task() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 16);
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        let sink = DaemonEngineEventSink(hub.clone());
        sink.emit(EngineEvent::TasksChanged(vec![
            engine_task("a", 0),
            engine_task("b", 0),
            engine_task("c", 2),
        ]));
        let frame = subscriber.try_recv().expect("one batch frame");
        let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::TasksChanged(tasks)) = frame.event
        else {
            panic!("the batch must travel as a single TasksChanged event");
        };
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.task_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        let stats_frame = subscriber
            .try_recv()
            .expect("runtime stats follow the batch");
        let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::RuntimeStatsChanged(stats)) =
            stats_frame.event
        else {
            panic!("pending count changed, so runtime stats must follow");
        };
        assert_eq!(stats.pending_tasks, 2);
        assert!(subscriber.try_recv().is_err(), "no per-task frames");
        let SnapshotBody::Daemon(snapshot) = hub.snapshot().body else {
            panic!("daemon snapshot expected");
        };
        assert_eq!(snapshot.tasks.len(), 3);

        // 状态迁移按任务账本结算：pending → downloading 只移动对应计数。
        sink.emit(EngineEvent::TasksChanged(vec![engine_task("a", 1)]));
        let SnapshotBody::Daemon(snapshot) = hub.snapshot().body else {
            panic!("daemon snapshot expected");
        };
        assert_eq!(
            (
                snapshot.runtime_stats.active_tasks,
                snapshot.runtime_stats.pending_tasks
            ),
            (1, 1)
        );
    }

    /// 批量删除事实只发一帧 `TasksDeleted`：投影整批移除，运行统计按被删任务结算。
    #[test]
    fn tasks_deleted_is_one_frame_and_releases_accounting() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 16);
        let sink = DaemonEngineEventSink(hub.clone());
        sink.emit(EngineEvent::TasksChanged(vec![
            engine_task("a", 1),
            engine_task("b", 0),
            engine_task("c", 0),
        ]));
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        sink.emit(EngineEvent::TasksDeleted(vec![
            "a".to_owned(),
            "b".to_owned(),
        ]));
        let frame = subscriber.try_recv().expect("one batch frame");
        let fluxdown_protocol::ServiceEvent::Daemon(DaemonEvent::TasksDeleted { task_ids }) =
            frame.event
        else {
            panic!("the batch must travel as a single TasksDeleted event");
        };
        assert_eq!(task_ids, ["a", "b"]);
        let SnapshotBody::Daemon(snapshot) = hub.snapshot().body else {
            panic!("daemon snapshot expected");
        };
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .map(|task| task.task_id.as_str())
                .collect::<Vec<_>>(),
            ["c"]
        );
        assert_eq!(
            (
                snapshot.runtime_stats.active_tasks,
                snapshot.runtime_stats.pending_tasks
            ),
            (0, 1)
        );
    }

    #[test]
    fn subscribe_snapshot_cursor_discards_prior_frames() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 8);
        let first = hub.publish(DaemonEvent::TaskDeleted {
            task_id: "old".to_owned(),
        });
        let (_receiver, snapshot) = hub.subscribe_and_snapshot();
        assert_eq!(snapshot.sequence, first.sequence);
        assert!(matches!(snapshot.body, SnapshotBody::Daemon(_)));
        let second = hub.publish(DaemonEvent::TaskDeleted {
            task_id: "new".to_owned(),
        });
        assert_eq!(second.sequence, snapshot.sequence + 1);
        assert_eq!(second.epoch, snapshot.epoch);
    }

    #[test]
    fn delete_sentinel_removes_task_from_atomic_projection() -> Result<(), serde_json::Error> {
        let task = serde_json::from_value::<TaskDto>(serde_json::json!({
            "taskId": "task-1",
            "url": "https://example.com/file",
            "fileName": "file",
            "saveDir": "/tmp",
            "status": 1,
            "downloadedBytes": 10,
            "totalBytes": 100,
            "errorMessage": "",
            "createdAt": "1",
            "proxyUrl": "",
            "queueId": "main",
            "checksum": ""
        }))?;
        let hub = DaemonEventHub::new(
            DaemonSnapshot {
                tasks: vec![task],
                ..DaemonSnapshot::default()
            },
            8,
        );
        let frame = hub.publish(DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: "task-1".to_owned(),
            status: 4,
            downloaded_bytes: 10,
            total_bytes: 100,
            speed: 0,
            file_name: "file".to_owned(),
            save_dir: "/tmp".to_owned(),
            upload_speed: 0,
            url: "https://example.com/file".to_owned(),
            error_message: "deleted".to_owned(),
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        }));
        assert_eq!(frame.sequence, 1);
        let snapshot = hub.snapshot();
        let SnapshotBody::Daemon(snapshot) = snapshot.body else {
            panic!("daemon hub returned agent snapshot");
        };
        assert!(snapshot.tasks.is_empty());
        Ok(())
    }

    fn traffic_task(id: &str, status: i32, downloaded: i64, uploaded: i64) -> TaskDto {
        serde_json::from_value(serde_json::json!({
            "taskId":id, "url":"https://example.com/live", "fileName":"live",
            "saveDir":"/tmp", "status":status, "downloadedBytes":downloaded,
            "uploadedBytes":uploaded, "totalBytes":10485760,
            "errorMessage":"", "createdAt":"1", "proxyUrl":"", "queueId":"main", "checksum":""
        }))
        .expect("traffic task fixture")
    }

    /// 整表替换在广播里只带头 / 重载宣告（一帧不随任务数增长），投影与分页仍是完整的；
    /// 行数不超过内联上限的整表事件保持内联。
    #[test]
    fn oversized_table_events_broadcast_headers_but_keep_the_full_projection() {
        use fluxdown_protocol::{
            TABLE_INLINE_LIMIT, TablePageParams, TableSection, TaskDto as Dto,
        };
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 16);
        let mut subscriber = hub.subscribe();
        let total = TABLE_INLINE_LIMIT + 7;
        let tasks: Vec<Dto> = (0..total)
            .map(|index| traffic_task(&format!("t{index:05}"), 2, 0, 0))
            .collect();
        hub.publish(DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks }));
        let frame = subscriber.try_recv().expect("table frame");
        let ServiceEvent::Daemon(DaemonEvent::TablesReloading { tasks, queue }) = frame.event
        else {
            panic!("an oversized TasksSnapshot must broadcast a reload notice");
        };
        assert_eq!((tasks, queue), (Some(total as u64), None));

        let SnapshotBody::Daemon(full) = hub.snapshot().body else {
            panic!("daemon snapshot");
        };
        assert_eq!(full.tasks.len(), total, "the projection keeps every task");
        let SnapshotBody::Daemon(header) = hub.wire_snapshot().body else {
            panic!("daemon snapshot");
        };
        assert!(header.tasks.is_empty());
        assert_eq!(header.task_total, Some(total as u64));

        let mut seen = 0;
        let mut cursor = None;
        loop {
            let page = hub
                .task_page(&TablePageParams {
                    section: TableSection::Tasks,
                    cursor: cursor.take(),
                    limit: Some(1_000),
                })
                .expect("page");
            assert!(page.tasks.len() <= 1_000);
            seen += page.tasks.len();
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen, total);

        // 小表仍内联，客户端不必进入加载态。
        hub.publish(DaemonEvent::Engine(WsServerMsg::TasksSnapshot {
            tasks: vec![traffic_task("only", 2, 0, 0)],
        }));
        assert!(matches!(
            subscriber.try_recv().expect("small table frame").event,
            ServiceEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TasksSnapshot { .. }))
        ));
    }

    /// 替换事件广播里不带任务表，投影本身被完整替换。
    #[test]
    fn replaced_snapshots_broadcast_as_headers() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 16);
        let mut subscriber = hub.subscribe();
        hub.replace_snapshot(DaemonSnapshot {
            tasks: vec![traffic_task("a", 2, 0, 0), traffic_task("b", 2, 0, 0)],
            ..DaemonSnapshot::default()
        });
        let ServiceEvent::Daemon(DaemonEvent::SnapshotReplaced(header)) =
            subscriber.try_recv().expect("replacement frame").event
        else {
            panic!("replacement frame");
        };
        assert!(header.tasks.is_empty());
        assert_eq!(header.task_total, Some(2));
        assert_eq!(hub.inspect(|snapshot| snapshot.tasks.len()), 2);
    }

    fn traffic_progress(id: &str, status: i32, downloaded: i64, uploaded: i64) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: id.into(),
            status,
            downloaded_bytes: downloaded,
            uploaded_bytes: uploaded,
            total_bytes: 10485760,
            speed: 0,
            upload_speed: 0,
            file_name: String::new(),
            save_dir: String::new(),
            url: String::new(),
            error_message: String::new(),
            seeding_status: i32::from(status == 3),
            seeding_message: String::new(),
            seeding_time_secs: 0,
        })
    }

    /// 做种上传帧按引擎采样节奏到达时，总上传速率必须读作真实速率（#939：
    /// 5 秒一帧时状态栏在约 5 倍真实速率与 0 之间跳动，看似突破上传限速）。
    #[test]
    fn seeding_frames_at_engine_sample_cadence_read_as_real_upload_rate() {
        const MIB: i64 = 1024 * 1024;
        let cadence = fluxdown_engine::bt_seeding::SEEDING_UPLOAD_SAMPLE_INTERVAL;
        let cadence_ticks = u64::try_from(cadence.as_millis() / 100).unwrap_or(u64::MAX);
        assert!(cadence_ticks >= 1);
        let start = std::time::Instant::now();
        let mut accounting =
            super::RuntimeAccounting::new(&[traffic_task("seed", 3, 10 * MIB, 0)], start);
        // 1 MiB/s 的稳定做种上传（会话级限速下的真实流量）。
        for tick in 1..=300_u64 {
            let now = start + std::time::Duration::from_millis(tick * 100);
            if tick % cadence_ticks == 0 {
                let uploaded = i64::try_from(tick).unwrap_or_default() * MIB / 10;
                accounting.apply(&traffic_progress("seed", 3, 10 * MIB, uploaded), now);
            }
            if tick >= 40 {
                assert_eq!(accounting.rates(now), (0, MIB), "tick {tick}");
            }
        }
    }

    #[test]
    fn fast_completions_accumulate_real_bytes_then_expire_without_task_events() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 64);
        let mut receiver = hub.subscribe();
        let mut state = super::lock_or_recover(&hub.state);
        let start = state.accounting.origin;
        for index in 0..20 {
            let id = format!("small-{index}");
            hub.publish_locked_at(
                &mut state,
                DaemonEvent::TaskChanged(traffic_task(&id, 0, 0, 0)),
                start,
            );
            // No nonzero task EMA, and no intermediate progress is required.
            hub.publish_locked_at(
                &mut state,
                traffic_progress(&id, 3, 128 * 1024, 0),
                start + std::time::Duration::from_millis(20),
            );
        }
        // The window ends `REPORT_LAG` behind now: the burst shows up once its
        // bucket is complete and reported, as its real rate over that bucket.
        hub.expire_locked_at(&mut state, start + std::time::Duration::from_millis(800));
        assert_eq!(
            state.snapshot.runtime_stats.total_download_bps,
            20 * 128 * 1024 * 10
        );
        assert_eq!(state.snapshot.runtime_stats.active_tasks, 0);
        assert_eq!(state.snapshot.runtime_stats.pending_tasks, 0);
        hub.expire_locked_at(&mut state, start + std::time::Duration::from_secs(5));
        assert_eq!(state.snapshot.runtime_stats.total_download_bps, 0);
        let mut published = Vec::new();
        while let Ok(frame) = receiver.try_recv() {
            if let ServiceEvent::Daemon(DaemonEvent::RuntimeStatsChanged(stats)) = frame.event {
                published.push(stats.total_download_bps);
            }
        }
        assert_eq!(
            published,
            [20 * 128 * 1024 * 10, 0],
            "expiry publishes the rate and then zero even without another task"
        );
    }

    #[test]
    fn persisted_resume_baselines_and_regressing_frames_do_not_invent_traffic() {
        let start = std::time::Instant::now();
        let mut accounting = super::RuntimeAccounting::new(
            &[traffic_task("resumed", 2, 8_000_000, 9_000_000)],
            start,
        );
        assert_eq!(accounting.window_bytes(), (0, 0));
        for bytes in [0, 8_000_000, 8_000_000] {
            accounting.apply(&traffic_progress("resumed", 1, bytes, 9_000_000), start);
        }
        assert_eq!(accounting.window_bytes(), (0, 0));
        accounting.apply(&traffic_progress("resumed", 1, 8_001_000, 9_000_200), start);
        for bytes in [8_001_000, 1, 8_000_900, 8_001_000] {
            accounting.apply(&traffic_progress("resumed", 1, bytes, 9_000_200), start);
        }
        assert_eq!(accounting.window_bytes(), (1_000, 200));
        assert_eq!((accounting.active, accounting.pending), (1, 0));
        accounting.apply(
            &DaemonEvent::TaskChanged(traffic_task("resumed", 2, 8_005_000, 9_005_000)),
            start,
        );
        accounting.apply(&traffic_progress("resumed", 1, 8_005_000, 9_005_000), start);
        assert_eq!(accounting.window_bytes(), (1_000, 200));
    }

    #[test]
    fn explicit_baselines_count_restarts_and_resume_rollback_without_counting_restored_bytes() {
        let start = std::time::Instant::now();
        let mut accounting =
            super::RuntimeAccounting::new(&[traffic_task("same-id", 3, 8_000_000, 0)], start);
        // Restart uses the same ID but actually truncates the file.
        accounting.transfer_baseline("same-id", 0);
        accounting.apply(&traffic_progress("same-id", 1, 0, 0), start);
        accounting.apply(&traffic_progress("same-id", 1, 128 * 1024, 0), start);
        assert_eq!(accounting.window_bytes().0, 128 * 1024);
        // A DB snapshot can still carry the previous checkpoint. It cannot
        // overwrite the transport's explicitly established measurement base.
        accounting.reconcile(&[traffic_task("same-id", 1, 8_000_000, 0)]);
        accounting.apply(&traffic_progress("same-id", 1, 256 * 1024, 0), start);
        assert_eq!(accounting.window_bytes().0, 256 * 1024);
        // A true resume restores a larger prefix, without transferring it.
        accounting.transfer_baseline("same-id", 4_000_000);
        accounting.apply(&traffic_progress("same-id", 1, 4_000_000, 0), start);
        assert_eq!(accounting.window_bytes().0, 256 * 1024);
        // The HTTP response may reject Range and start again at zero.
        accounting.transfer_baseline("same-id", 0);
        accounting.apply(&traffic_progress("same-id", 1, 128 * 1024, 0), start);
        assert_eq!(accounting.window_bytes().0, 384 * 1024);
        // Checkpoint alignment also intentionally retransmits some old bytes.
        accounting.transfer_baseline("same-id", 64 * 1024);
        accounting.apply(&traffic_progress("same-id", 1, 128 * 1024, 0), start);
        assert_eq!(accounting.window_bytes().0, 448 * 1024);
        assert_eq!((accounting.active, accounting.pending), (1, 0));
    }

    #[test]
    fn transfer_baseline_is_host_local_and_precedes_ordinary_progress() {
        let hub = DaemonEventHub::new(
            DaemonSnapshot {
                tasks: vec![traffic_task("same-id", 1, 8_000_000, 0)],
                ..Default::default()
            },
            16,
        );
        let mut subscriber = hub.subscribe();
        let sink = DaemonEngineEventSink(hub.clone());
        sink.emit(EngineEvent::TaskTransferBaseline {
            task_id: "same-id".into(),
            downloaded_bytes: 0,
            previous_downloaded_bytes: None,
        });
        assert_eq!(hub.snapshot().sequence, 0);
        assert!(
            subscriber.try_recv().is_err(),
            "baseline adds no wire event"
        );
        let mut state = super::lock_or_recover(&hub.state);
        let now = state.accounting.origin;
        hub.publish_with_stats_locked_at(
            &mut state,
            traffic_progress("same-id", 1, 128 * 1024, 0),
            now,
        );
        assert_eq!(state.accounting.window_bytes().0, 128 * 1024);
    }

    /// Tasks that buffer bodies before an ordered write (HLS) report received
    /// bytes: those are the traffic, while segment-sized write jumps and the
    /// final (remuxed) size are not. Leaving the active states restores
    /// ordinary progress accounting for the same task id.
    #[test]
    fn transfer_measured_tasks_count_received_bytes_not_progress_jumps() {
        const MIB: i64 = 1024 * 1024;
        let start = std::time::Instant::now();
        let mut accounting = super::RuntimeAccounting::new(&[traffic_task("hls", 1, 0, 0)], start);
        accounting.record_transfer("hls", MIB, start);
        accounting.apply(&traffic_progress("hls", 1, 0, 0), start);
        assert_eq!(accounting.window_bytes().0, MIB);
        accounting.record_transfer("hls", MIB, start);
        accounting.apply(&traffic_progress("hls", 1, 2 * MIB, 0), start);
        assert_eq!(accounting.window_bytes().0, 2 * MIB);
        accounting.apply(&traffic_progress("hls", 3, 5 * MIB, 0), start);
        assert_eq!(accounting.window_bytes().0, 2 * MIB);
        accounting.apply(
            &DaemonEvent::TaskChanged(traffic_task("hls", 1, 5 * MIB, 0)),
            start,
        );
        accounting.apply(&traffic_progress("hls", 1, 6 * MIB, 0), start);
        assert_eq!(accounting.window_bytes().0, 3 * MIB);
    }

    #[test]
    fn consecutive_range_baselines_settle_coalesced_window_tails_exactly_once() {
        const MIB: i64 = 1024 * 1024;
        let start = std::time::Instant::now();
        for forwards_intermediate_sample in [false, true] {
            let mut accounting =
                super::RuntimeAccounting::new(&[traffic_task("ranges", 1, MIB, 0)], start);
            accounting.transfer_baseline("ranges", MIB);
            for window in 1..=3 {
                let now = start + std::time::Duration::from_millis(window as u64 * 100);
                let end = MIB + window * 16 * MIB;
                if forwards_intermediate_sample {
                    accounting.apply(&traffic_progress("ranges", 1, end - MIB, 0), now);
                }
                // The raw EOF sample may have been coalesced by the reporter.
                // The next baseline settles that old interval before reset.
                accounting.settle_download("ranges", end, now);
                accounting.transfer_baseline("ranges", end);
                accounting.apply(&traffic_progress("ranges", 1, end, 0), now);
                assert_eq!(accounting.window_bytes().0, window * 16 * MIB);
            }
            assert_eq!(
                accounting.rates(start + std::time::Duration::from_secs(5)),
                (0, 0)
            );
        }
    }

    #[test]
    fn actual_ftp_offsets_replace_task_checkpoints_without_inventing_restored_traffic() {
        const MIB: i64 = 1024 * 1024;
        let start = std::time::Instant::now();
        // Missing/truncated file, physical prefix ahead of the task row, and
        // repaired segment checkpoints all establish their actual offset.
        for actual_offset in [0, 12 * MIB, 4 * MIB] {
            let mut accounting =
                super::RuntimeAccounting::new(&[traffic_task("ftp", 2, 8 * MIB, 0)], start);
            accounting.transfer_baseline("ftp", 8 * MIB);
            accounting.apply(&traffic_progress("ftp", 1, 8 * MIB, 0), start);
            accounting.settle_download("ftp", 8 * MIB, start);
            accounting.transfer_baseline("ftp", actual_offset);
            accounting.apply(&traffic_progress("ftp", 1, actual_offset, 0), start);
            assert_eq!(accounting.window_bytes(), (0, 0));
            accounting.apply(&traffic_progress("ftp", 1, actual_offset + 65536, 0), start);
            assert_eq!(accounting.window_bytes(), (65536, 0));
            // A REST-rejected retry truncates the file, then transfers bytes
            // below both the old checkpoint and the prior attempt's maximum.
            accounting.transfer_baseline("ftp", 0);
            accounting.apply(&traffic_progress("ftp", 1, 65536, 0), start);
            assert_eq!(accounting.window_bytes(), (131072, 0));
        }
    }

    #[test]
    fn adopted_local_files_have_no_traffic_but_real_terminal_bytes_still_count() {
        let start = std::time::Instant::now();
        let mut accounting = super::RuntimeAccounting::new(
            &[
                traffic_task("adopted", 0, 0, 0),
                traffic_task("downloaded", 0, 0, 0),
            ],
            start,
        );
        accounting.transfer_baseline("adopted", 8 * 1024 * 1024);
        accounting.apply(&traffic_progress("adopted", 3, 8 * 1024 * 1024, 0), start);
        assert_eq!(accounting.window_bytes(), (0, 0));
        accounting.apply(&traffic_progress("downloaded", 3, 128 * 1024, 0), start);
        assert_eq!(accounting.window_bytes(), (128 * 1024, 0));
        assert_eq!((accounting.active, accounting.pending), (0, 0));
    }

    #[test]
    fn snapshots_deletion_and_seeding_keep_counts_and_window_consistent() {
        let start = std::time::Instant::now();
        let mut accounting = super::RuntimeAccounting::new(
            &[
                traffic_task("active", 1, 0, 0),
                traffic_task("pending", 0, 0, 0),
                traffic_task("seed", 3, 100, 400),
            ],
            start,
        );
        assert_eq!((accounting.active, accounting.pending), (1, 1));
        accounting.apply(&traffic_progress("active", 3, 100, 0), start);
        accounting.apply(&traffic_progress("seed", 3, 100, 600), start);
        assert_eq!(accounting.window_bytes(), (100, 200));
        assert_eq!((accounting.active, accounting.pending), (0, 1));
        // A stale full snapshot must not make old bytes count twice.
        accounting.apply(
            &DaemonEvent::Engine(WsServerMsg::TasksSnapshot {
                tasks: vec![
                    traffic_task("active", 5, 0, 0),
                    traffic_task("new", 0, 500, 0),
                ],
            }),
            start,
        );
        accounting.apply(&traffic_progress("active", 1, 100, 0), start);
        accounting.apply(&traffic_progress("new", 1, 500, 0), start);
        assert_eq!(accounting.window_bytes(), (100, 200));
        assert_eq!((accounting.active, accounting.pending), (2, 0));
        accounting.apply(
            &DaemonEvent::TaskDeleted {
                task_id: "active".into(),
            },
            start,
        );
        accounting.apply(&traffic_progress("active", 1, 20_000, 0), start);
        assert_eq!((accounting.active, accounting.pending), (1, 0));
        assert_eq!(accounting.window_bytes(), (100, 200));
        let mut deleted = traffic_progress("new", 4, 500, 0);
        if let DaemonEvent::Engine(WsServerMsg::TaskProgress { error_message, .. }) = &mut deleted {
            *error_message = "deleted".into();
        }
        accounting.apply(&deleted, start);
        assert_eq!((accounting.active, accounting.pending), (0, 0));
        accounting.apply(
            &DaemonEvent::SnapshotReplaced(DaemonSnapshot {
                tasks: vec![traffic_task("restored", 0, 9_000_000, 0)],
                ..Default::default()
            }),
            start,
        );
        assert_eq!((accounting.active, accounting.pending), (0, 1));
        assert_eq!(accounting.window_bytes(), (100, 200));
        assert_eq!(
            accounting.rates(start + std::time::Duration::from_secs(5)),
            (0, 0)
        );
    }

    /// A task reporting a lump every 500 ms (the engine's per-task cadence)
    /// must read as a flat rate, not a sawtooth, and fall to zero afterwards.
    #[test]
    fn lumpy_half_second_reports_read_as_a_steady_rate() {
        const MIB: i64 = 1024 * 1024;
        let start = std::time::Instant::now();
        let mut accounting =
            super::RuntimeAccounting::new(&[traffic_task("steady", 0, 0, 0)], start);
        accounting.apply(
            &DaemonEvent::TaskChanged(traffic_task("steady", 1, 0, 0)),
            start,
        );
        for tick in 1..=200_u64 {
            let now = start + std::time::Duration::from_millis(tick * 100);
            if tick % 5 == 0 {
                let bytes = i64::try_from(tick / 5).unwrap_or_default() * 5 * MIB;
                accounting.apply(&traffic_progress("steady", 1, bytes, 0), now);
            }
            if tick >= 30 {
                assert_eq!(accounting.rates(now), (10 * MIB, 0), "tick {tick}");
            }
        }
        assert_eq!(
            accounting.rates(start + std::time::Duration::from_secs(25)),
            (0, 0)
        );
    }

    /// Small files finishing in one frame each must not make the total jitter.
    #[test]
    fn back_to_back_small_files_read_as_a_steady_rate() {
        const MIB: i64 = 1024 * 1024;
        let start = std::time::Instant::now();
        let mut accounting = super::RuntimeAccounting::new(&[], start);
        for index in 0..1000_u64 {
            let id = format!("small-{index}");
            let begin = start + std::time::Duration::from_millis(index * 10);
            let end = begin + std::time::Duration::from_millis(10);
            accounting.apply(&DaemonEvent::TaskChanged(traffic_task(&id, 1, 0, 0)), begin);
            accounting.apply(&traffic_progress(&id, 3, MIB, 0), end);
            if index % 10 == 9 && index >= 299 {
                assert_eq!(accounting.rates(end), (100 * MIB, 0), "file {index}");
            }
        }
    }

    #[test]
    fn runtime_stats_frame_follows_progress_only_when_stats_change() -> Result<(), serde_json::Error>
    {
        let task: TaskDto = serde_json::from_value(serde_json::json!({
            "taskId":"task-live", "url":"https://example.com/live", "fileName":"live",
            "saveDir":"/tmp", "status":1, "downloadedBytes":10, "totalBytes":100,
            "errorMessage":"", "createdAt":"1", "proxyUrl":"", "queueId":"main", "checksum":""
        }))?;
        let hub = DaemonEventHub::new(
            DaemonSnapshot {
                tasks: vec![task],
                ..DaemonSnapshot::default()
            },
            16,
        );
        let (mut subscriber, _) = hub.subscribe_and_snapshot();
        let progress = |downloaded_bytes, speed| {
            DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id: "task-live".to_owned(),
                status: 1,
                downloaded_bytes,
                total_bytes: 100,
                speed,
                upload_speed: 0,
                file_name: "live".to_owned(),
                save_dir: "/tmp".to_owned(),
                url: "https://example.com/live".to_owned(),
                error_message: String::new(),
                uploaded_bytes: 0,
                seeding_status: 0,
                seeding_message: String::new(),
                seeding_time_secs: 0,
            })
        };
        let mut drain = || {
            std::iter::from_fn(|| subscriber.try_recv().ok())
                .map(|frame| match frame.event {
                    ServiceEvent::Daemon(DaemonEvent::RuntimeStatsChanged(stats)) => {
                        Some(stats.total_download_bps)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let mut state = super::lock_or_recover(&hub.state);
        let now = state.accounting.origin;
        let at = |millis| now + std::time::Duration::from_millis(millis);

        // Bytes in the still-open, not yet reported window publish nothing.
        hub.publish_with_stats_locked_at(&mut state, progress(20, 100), now);
        assert_eq!(drain(), [None]);
        // Duplicate progress does not count its bytes twice, regardless of EMA.
        hub.publish_with_stats_locked_at(&mut state, progress(20, 250), now);
        assert_eq!(drain(), [None]);
        // 20 bytes spread over 800 ms: 2 land in bucket 0 next to the first 10.
        hub.publish_with_stats_locked_at(&mut state, progress(40, 250), at(800));
        assert_eq!(drain(), [None, Some(120)]);
        // Within the same 500 ms publish period the rate is not republished.
        hub.publish_with_stats_locked_at(&mut state, progress(60, 250), at(900));
        assert_eq!(drain(), [None]);
        // Unrelated events can also advance time; they must not silently clear
        // the materialized rate before the expiry timer gets to publish zero.
        hub.publish_with_stats_locked_at(
            &mut state,
            DaemonEvent::TaskDeleted {
                task_id: "unknown".into(),
            },
            at(5_000),
        );
        assert_eq!(drain(), [None, Some(0)]);
        Ok(())
    }
    #[test]
    fn parallel_publish_preserves_broadcast_sequence_and_snapshot_cursor() {
        let task: TaskDto = serde_json::from_value(serde_json::json!({
            "taskId":"shared", "url":"https://example.com/x", "fileName":"x", "saveDir":"/tmp",
            "status":1, "downloadedBytes":0, "totalBytes":100, "errorMessage":"",
            "createdAt":"1", "proxyUrl":"", "queueId":"main", "checksum":""
        }))
        .expect("task fixture");
        let hub = DaemonEventHub::new(
            DaemonSnapshot {
                tasks: vec![task],
                ..Default::default()
            },
            1024,
        );
        let (mut subscriber, before) = hub.subscribe_and_snapshot();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let hub = hub.clone();
                scope.spawn(move || {
                    for _ in 0..100 {
                        hub.publish(DaemonEvent::TaskRuntimeChanged(
                            fluxdown_protocol::TaskRuntimeDto {
                                task_id: "shared".into(),
                                active_transfers: Some(1),
                                ..Default::default()
                            },
                        ));
                    }
                });
            }
        });
        for expected in 1..=400 {
            let frame = subscriber
                .try_recv()
                .expect("all publications arrive without a gap");
            assert_eq!(frame.epoch, before.epoch);
            assert_eq!(frame.sequence, expected);
        }
        assert_eq!(hub.snapshot().sequence, 400);
        let (_, after) = hub.subscribe_and_snapshot();
        assert_eq!(after.sequence, 400);
        let SnapshotBody::Daemon(body) = after.body else {
            panic!("daemon snapshot")
        };
        assert_eq!(body.task_runtime["shared"].active_transfers, Some(1));
    }

    #[test]
    fn completed_tasks_never_accumulate_runtime_in_the_snapshot_header() {
        let hub = DaemonEventHub::new(DaemonSnapshot::default(), 4096);
        let task = |id: &str, status: i32| -> TaskDto {
            serde_json::from_value(serde_json::json!({
                "taskId": id, "url": "https://example.com/x", "fileName": "x", "saveDir": "/tmp",
                "status": status, "downloadedBytes": 0, "totalBytes": 100, "errorMessage": "",
                "createdAt": "1", "proxyUrl": "", "queueId": "main", "checksum": ""
            }))
            .expect("task fixture")
        };
        let sample = |id: &str, sequence: u64| fluxdown_protocol::TaskRuntimeDto {
            task_id: id.into(),
            sample_sequence: sequence,
            active_transfers: Some(1),
            ..Default::default()
        };
        for index in 0..300_u64 {
            let id = format!("t{index}");
            hub.publish(DaemonEvent::TaskChanged(task(&id, 1)));
            hub.publish(DaemonEvent::TaskRuntimeChanged(sample(&id, index + 1)));
            hub.publish(DaemonEvent::TaskChanged(task(&id, 3)));
            // 完成后迟到的采样不得把条目加回快照。
            hub.publish(DaemonEvent::TaskRuntimeChanged(sample(&id, index + 10_000)));
        }
        let SnapshotBody::Daemon(body) = hub.snapshot().body else {
            panic!("daemon snapshot")
        };
        assert_eq!(body.tasks.len(), 300);
        assert!(body.task_runtime.is_empty());
        assert!(body.paged_header().task_runtime.is_empty());
    }
}
