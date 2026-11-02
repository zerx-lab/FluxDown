//! 防吸血宿主：把 [`BtGuard`] 接到引擎事件、数据库与 BT 会话。
//!
//! 两类任务，都不使用周期定时器唤醒空闲进程：
//!
//! - **泵**（引擎级，常驻但只在被 guard 的 waker 唤醒时工作）：合并防抖后取走 guard 的待办
//!   [`GuardAction`] 并执行（断开已封禁对端 / 发引擎事件 / 解封后让会话重新拨号），再把待落库
//!   内容写库。落库失败退避后重试，不会空转。
//! - **会话任务**（随 BT 会话创建、随其关闭而结束）：低频释放到期封禁（`readmit`）、BT 会话启动与
//!   存活期间每 6 小时请求一次云名单、每 24 小时刷新过滤文件订阅。会话一关就没有任何定时器。
//!
//! 管理类调用（条目增删、云名单 / 订阅应用）只改 guard 内存；后续的事件、断开、落库都由泵经 guard
//! 的 waker 触发，宿主不需要再逐条通知。

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::Duration;

use librqbit::{PeerPolicy, Session};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::{
    BtGuard, BtGuardConfig, FilterSourceStatus, GuardAction, GuardPolicy, LoadSummary, RuleMode,
    filter_fetch,
};
use crate::db::{Db, DbError};
use crate::events::{EngineEvent, EventSink};
use crate::proxy_config::ProxyConfig;

/// 引擎关停时等待泵做最终落库的上限。
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

/// 宿主节拍。生产用 [`Default`]；测试注入毫秒级值。
#[derive(Clone, Copy, Debug)]
pub struct HostTimings {
    /// 被唤醒后先等这么久再处理，合并同一波事件（Changed 事件 / 落库的防抖）。
    pub coalesce: Duration,
    /// 落库失败后的退避。
    pub flush_retry: Duration,
    /// 会话存活期间释放到期封禁的间隔。
    pub readmit_interval: Duration,
    /// 会话存活期间请求云名单的间隔（会话启动时另外立即请求一次）。
    pub cloud_interval: Duration,
    /// 会话存活期间刷新过滤文件订阅的间隔。
    pub filter_interval: Duration,
}

impl Default for HostTimings {
    fn default() -> Self {
        Self {
            coalesce: Duration::from_millis(500),
            flush_retry: Duration::from_secs(30),
            readmit_interval: Duration::from_secs(60),
            cloud_interval: Duration::from_secs(6 * 3600),
            filter_interval: Duration::from_secs(24 * 3600),
        }
    }
}

struct ActiveSession {
    generation: u64,
    session: Weak<Session>,
    closing: CancellationToken,
}

struct Inner {
    guard: Arc<BtGuard>,
    policy: Arc<dyn PeerPolicy>,
    db: Db,
    sink: Arc<dyn EventSink>,
    timings: HostTimings,
    /// guard 的 waker：有待办时唤醒泵。
    wake: Arc<Notify>,
    cancel: CancellationToken,
    session: Mutex<Option<ActiveSession>>,
    next_generation: AtomicU64,
    proxy: RwLock<ProxyConfig>,
    /// 串行化所有落库：批次按取走顺序提交，避免「后取的先提交」造成旧状态覆盖新状态。
    flush_gate: tokio::sync::Mutex<()>,
    /// 串行化订阅刷新（周期 / 手动 / 配置变更）。
    refresh_gate: tokio::sync::Mutex<()>,
    filter_now: Notify,
    cloud_now: Notify,
}

fn wants_cloud(config: &BtGuardConfig) -> bool {
    config.enabled && config.cloud_enabled && config.cloud_mode != RuleMode::Off
}

impl Inner {
    fn active_session(&self) -> Option<Arc<Session>> {
        let slot = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let active = slot.as_ref()?;
        if active.closing.is_cancelled() {
            return None;
        }
        active.session.upgrade()
    }

    fn apply_throttle(&self, session: &Session) {
        let kib = self.guard.config().throttle_kib;
        session.set_throttled_upload_bps(NonZeroU32::new(kib.saturating_mul(1024)));
    }

    /// 取走并执行 guard 的待办动作。
    fn drain_actions(&self) {
        let actions = self.guard.take_actions();
        if actions.is_empty() {
            return;
        }
        let session = self.active_session();
        for action in actions {
            match action {
                GuardAction::DisconnectBanned => {
                    if let Some(session) = &session {
                        let closed = session.disconnect_peers_where(|ip| self.guard.is_banned(ip));
                        if closed > 0 {
                            crate::log_info!("[BtGuard] disconnected {closed} banned peer(s)");
                        }
                    }
                }
                GuardAction::Changed => {
                    self.sink.emit(EngineEvent::BtGuardChanged);
                    // 封禁可能刚被解除 / 过滤层被替换：让会话重新拨号之前被拒绝的对端。
                    if let Some(session) = &session {
                        session.readmit_refused_peers();
                    }
                }
                GuardAction::EvidenceReady => {
                    let count = u32::try_from(self.guard.pending_evidence()).unwrap_or(u32::MAX);
                    self.sink.emit(EngineEvent::BtGuardEvidenceReady { count });
                }
            }
        }
    }

    async fn flush(&self) -> Result<(), DbError> {
        let _gate = self.flush_gate.lock().await;
        if !self.guard.needs_flush() {
            return Ok(());
        }
        self.guard.flush(&self.db).await
    }

    /// 会话关闭 / 引擎关停的落库：连仅因上传累加而变脏的账本也写入。
    async fn flush_all(&self) -> Result<(), DbError> {
        let _gate = self.flush_gate.lock().await;
        if !self.guard.needs_flush_all() {
            return Ok(());
        }
        self.guard.flush_all(&self.db).await
    }

    /// 启动加载失败过则在每次 BT 会话启动时重试：成功后内存状态换成库内状态并解除落库门控。
    /// 与落库共用闸，避免加载替换状态的同时有批次在取走。
    async fn retry_load(&self) {
        if !self.guard.load_failed() {
            return;
        }
        let _gate = self.flush_gate.lock().await;
        if !self.guard.load_failed() {
            return;
        }
        match self.guard.load(&self.db).await {
            Ok(summary) => {
                crate::log_info!(
                    "[BtGuard] retried load succeeded: {} entries, {} ledgers, {} strikes",
                    summary.entries,
                    summary.ledgers,
                    summary.strikes
                );
                let mut st = self.guard.lock();
                st.push_action(GuardAction::Changed);
                st.push_action(GuardAction::DisconnectBanned);
            }
            Err(error) => {
                crate::logger::report_warning("bt-guard", "retry load persisted state", &error);
            }
        }
    }

    fn request_cloud_refresh(&self) {
        if !wants_cloud(&self.guard.config()) {
            return;
        }
        let have_sequence = u64::try_from(self.guard.cloud_sequence()).unwrap_or(0);
        self.sink
            .emit(EngineEvent::BtGuardCloudRefreshWanted { have_sequence });
    }

    /// 刷新过滤文件订阅。`max_age_secs` 为 `Some` 时跳过「近期成功」的源；`when_disabled` 为
    /// `false` 时防吸血关闭则不联网（关闭期间过滤层本就不生效）。
    async fn refresh_filters(
        &self,
        max_age_secs: Option<i64>,
        when_disabled: bool,
    ) -> Vec<FilterSourceStatus> {
        let _gate = self.refresh_gate.lock().await;
        if !when_disabled && !self.guard.config().enabled {
            return self.guard.filter_sources();
        }
        let proxy = self
            .proxy
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        filter_fetch::refresh_sources(&self.guard, &proxy, max_age_secs).await
    }

    fn end_session(&self, generation: u64) {
        let mut slot = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|a| a.generation == generation) {
            *slot = None;
            drop(slot);
            // 仅在没有更新的会话顶替时清连接态，否则会抹掉新会话刚建立的连接。
            self.guard.clear_connections();
        }
    }
}

/// 常驻泵：被 guard 唤醒 → 合并防抖 → 执行动作 → 落库。
async fn run_pump(inner: Arc<Inner>) {
    loop {
        tokio::select! {
            () = inner.cancel.cancelled() => break,
            () = inner.wake.notified() => {}
        }
        tokio::select! {
            () = inner.cancel.cancelled() => break,
            () = tokio::time::sleep(inner.timings.coalesce) => {}
        }
        inner.drain_actions();
        if let Err(error) = inner.flush().await {
            crate::logger::report_warning("bt-guard", "persist state", &error);
            tokio::select! {
                () = inner.cancel.cancelled() => break,
                () = tokio::time::sleep(inner.timings.flush_retry) => {}
            }
        }
    }
    // 引擎关停：把已发生的变化落库。
    match tokio::time::timeout(SHUTDOWN_WAIT, inner.flush_all()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => crate::logger::report_warning("bt-guard", "final persist", &error),
        Err(_) => crate::log_warn!("[BtGuard] final persist timed out"),
    }
}

fn periodic(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

/// 会话任务：随会话 `closing` 信号结束。
async fn run_session(
    inner: Arc<Inner>,
    generation: u64,
    session: Weak<Session>,
    closing: CancellationToken,
) {
    let filter_max_age = i64::try_from(inner.timings.filter_interval.as_secs()).unwrap_or(i64::MAX);
    let mut ticks = SessionTicks {
        readmit: periodic(inner.timings.readmit_interval),
        cloud: periodic(inner.timings.cloud_interval),
        filters: periodic(inner.timings.filter_interval),
    };

    inner.retry_load().await;
    inner.request_cloud_refresh();
    tokio::select! {
        () = closing.cancelled() => {}
        () = inner.cancel.cancelled() => {}
        _ = inner.refresh_filters(Some(filter_max_age), false) => {}
    }
    if !closing.is_cancelled() && !inner.cancel.is_cancelled() {
        run_session_loop(&inner, &session, &closing, &mut ticks, filter_max_age).await;
    }
    inner.end_session(generation);
    // 会话关闭是落库点之一。
    if let Err(error) = inner.flush_all().await {
        crate::logger::report_warning("bt-guard", "persist state on session close", &error);
    }
}

struct SessionTicks {
    readmit: tokio::time::Interval,
    cloud: tokio::time::Interval,
    filters: tokio::time::Interval,
}

async fn run_session_loop(
    inner: &Arc<Inner>,
    session: &Weak<Session>,
    closing: &CancellationToken,
    ticks: &mut SessionTicks,
    filter_max_age: i64,
) {
    loop {
        tokio::select! {
            () = closing.cancelled() => return,
            () = inner.cancel.cancelled() => return,
            _ = ticks.readmit.tick() => {
                if let Some(session) = session.upgrade() {
                    session.readmit_refused_peers();
                }
            }
            _ = ticks.cloud.tick() => inner.request_cloud_refresh(),
            () = inner.cloud_now.notified() => inner.request_cloud_refresh(),
            _ = ticks.filters.tick() => {
                tokio::select! {
                    () = closing.cancelled() => return,
                    () = inner.cancel.cancelled() => return,
                    _ = inner.refresh_filters(None, false) => {}
                }
            }
            () = inner.filter_now.notified() => {
                tokio::select! {
                    () = closing.cancelled() => return,
                    () = inner.cancel.cancelled() => return,
                    _ = inner.refresh_filters(Some(filter_max_age), false) => {}
                }
            }
        }
    }
}

/// 引擎级防吸血宿主。创建时需要处于 tokio 运行时内（会拉起泵）。
pub struct BtGuardHost {
    inner: Arc<Inner>,
    pump: Mutex<Option<JoinHandle<()>>>,
}

impl BtGuardHost {
    /// 用默认节拍创建，并拉起泵。
    #[must_use]
    pub fn new(guard: Arc<BtGuard>, db: Db, sink: Arc<dyn EventSink>, proxy: ProxyConfig) -> Self {
        Self::with_timings(guard, db, sink, proxy, HostTimings::default())
    }

    #[must_use]
    pub fn with_timings(
        guard: Arc<BtGuard>,
        db: Db,
        sink: Arc<dyn EventSink>,
        proxy: ProxyConfig,
        timings: HostTimings,
    ) -> Self {
        let wake = Arc::new(Notify::new());
        guard.set_waker(Arc::clone(&wake));
        let policy: Arc<dyn PeerPolicy> = Arc::new(GuardPolicy::new(Arc::clone(&guard)));
        let inner = Arc::new(Inner {
            guard,
            policy,
            db,
            sink,
            timings,
            wake,
            cancel: CancellationToken::new(),
            session: Mutex::new(None),
            next_generation: AtomicU64::new(0),
            proxy: RwLock::new(proxy),
            flush_gate: tokio::sync::Mutex::new(()),
            refresh_gate: tokio::sync::Mutex::new(()),
            filter_now: Notify::new(),
            cloud_now: Notify::new(),
        });
        let pump = tokio::spawn(run_pump(Arc::clone(&inner)));
        Self {
            inner,
            pump: Mutex::new(Some(pump)),
        }
    }

    #[must_use]
    pub fn guard(&self) -> &Arc<BtGuard> {
        &self.inner.guard
    }

    /// 装进 `SessionOptions::peer_policy` 的策略对象。
    #[must_use]
    pub fn policy(&self) -> Arc<dyn PeerPolicy> {
        Arc::clone(&self.inner.policy)
    }

    #[must_use]
    pub fn config(&self) -> Arc<BtGuardConfig> {
        self.inner.guard.config()
    }

    /// 启动加载持久化的条目 / 账本 / strike。
    ///
    /// # Errors
    /// 读库失败。
    pub async fn load(&self) -> Result<LoadSummary, DbError> {
        let summary = self.inner.guard.load(&self.inner.db).await?;
        crate::log_info!(
            "[BtGuard] loaded {} entries ({} skipped), {} ledgers, {} strikes",
            summary.entries,
            summary.skipped_entries,
            summary.ledgers,
            summary.strikes
        );
        Ok(summary)
    }

    /// 热更新配置：不重建 BT 会话。受限队列速率立即下发；新增的订阅 URL / 刚开启的云名单立即
    /// 触发一次拉取（仅会话存活时；否则下一次会话启动时统一处理）。
    pub fn set_config(&self, config: BtGuardConfig) {
        let before = self.inner.guard.config();
        self.inner.guard.set_config(config);
        let after = self.inner.guard.config();
        let Some(session) = self.inner.active_session() else {
            return;
        };
        if before.throttle_kib != after.throttle_kib {
            self.inner.apply_throttle(&session);
        }
        if after.enabled {
            let known: HashSet<String> = if before.enabled {
                before.filter_url_list().into_iter().collect()
            } else {
                HashSet::new()
            };
            if after
                .filter_url_list()
                .iter()
                .any(|url| !known.contains(url))
            {
                self.inner.filter_now.notify_one();
            }
        }
        if wants_cloud(&after) && !wants_cloud(&before) {
            self.inner.cloud_now.notify_one();
        }
    }

    /// 订阅拉取使用的出口（全局代理 / 站点规则）。
    pub fn set_proxy(&self, proxy: ProxyConfig) {
        *self
            .inner
            .proxy
            .write()
            .unwrap_or_else(PoisonError::into_inner) = proxy;
    }

    /// 挂接一个 BT 会话：下发受限队列速率、拉起会话任务。`closing` 取消即结束会话任务。
    /// 必须在 tokio 运行时内调用。
    pub fn attach(&self, session: &Arc<Session>, closing: CancellationToken) {
        let inner = Arc::clone(&self.inner);
        let generation = inner.next_generation.fetch_add(1, Ordering::AcqRel) + 1;
        inner.apply_throttle(session);
        *inner.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(ActiveSession {
            generation,
            session: Arc::downgrade(session),
            closing: closing.clone(),
        });
        tokio::spawn(run_session(
            inner,
            generation,
            Arc::downgrade(session),
            closing,
        ));
    }

    /// 是否有存活（未进入关停）的 BT 会话。
    #[must_use]
    pub fn session_active(&self) -> bool {
        self.inner.active_session().is_some()
    }

    /// 立即把待落库内容写库（管理类调用返回前持久化）。与泵共用同一把落库闸。
    ///
    /// # Errors
    /// 写库失败（内容保留在 guard 内，泵会重试）。
    pub async fn flush_now(&self) -> Result<(), DbError> {
        self.inner.flush().await
    }

    /// 手动刷新全部过滤文件订阅（无视近期成功），返回各源状态。
    pub async fn refresh_filters_now(&self) -> Vec<FilterSourceStatus> {
        self.inner.refresh_filters(None, true).await
    }

    /// 停泵并做最终落库。幂等。
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let pump = self
            .pump
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(pump) = pump
            && let Err(error) = pump.await
            && !error.is_cancelled()
        {
            crate::logger::report_error("bt-guard", "join pump", &error);
        }
    }
}

impl Drop for BtGuardHost {
    fn drop(&mut self) {
        // 泵与会话任务随之结束（泵在退出前尽力做最终落库）。
        self.inner.cancel.cancel();
    }
}
