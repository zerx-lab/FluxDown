//! `PluginManager` 的常驻宿主侧：宿主激活 / 回收、可移植视图会话、渲染节流、命令与
//! RPC、定时器，以及 `flux.tasks.*` / `flux.settings.update` 的审计与限流。
//!
//! 本文件以 `#[path]` 子模块的形式挂在 [`super`]（`manager.rs`）下，因此可直接访问
//! `PluginManager` 的私有字段；宿主相关状态集中在 [`HostState`]，由 manager 持有。
//!
//! 空闲静默：节拍任务只在存在宿主时运行，最后一个宿主释放后自行退出；没有会话 / 定时
//! 器 / 事件时不产生任何周期性工作。

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use tokio::task::AbortHandle;

use super::{
    CIRCUIT_BREAKER_THRESHOLD, PluginManager, build_typed_settings_json, load_setting_values_db,
};
use crate::events::EngineEvent;
use crate::logger::log_info;
use crate::plugin::audit::{AuditLog, PluginAuditRow};
use crate::plugin::manifest::{
    PERMISSION_AUTH, PERMISSION_FFMPEG, PERMISSION_PAGE_SCRIPTS, PERMISSION_TASKS,
    PERMISSION_YTDLP, PluginManifest, ViewLocation,
};
use crate::plugin::runtime::{
    HostCall, HostContext, HostEvents, HostFuture, HostReply, PluginEntryKind, PluginError,
    PluginEvent, PluginHost, PluginScript, PluginTaskPort, StartedTasks,
};

/// 非 background 宿主在「无会话、无定时器、无进行中调用」持续这么久后回收。
const IDLE_RECLAIM: Duration = Duration::from_secs(5 * 60);
/// 会话在这么久没有 keepalive 后关闭。
const SESSION_TTL: Duration = Duration::from_secs(90);
/// 节拍间隔（keepalive 过期检查 + 空闲回收检查）。
const TICK: Duration = Duration::from_secs(30);
/// 同一会话两次渲染的最小间隔。
const RENDER_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// `flux.tasks.create` 的限流窗口与额度（每插件）。
const CREATE_WINDOW: Duration = Duration::from_secs(60);
const CREATE_LIMIT: usize = 20;

// Bound downloaded source buffers waiting for the decoder, across all plugin hosts.
static IMAGE_LOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
const IMAGE_QUEUE_TIMEOUT: Duration = Duration::from_secs(3);

async fn acquire_image_load(
    semaphore: &tokio::sync::Semaphore,
) -> Result<tokio::sync::SemaphorePermit<'_>, PluginError> {
    tokio::time::timeout(IMAGE_QUEUE_TIMEOUT, semaphore.acquire())
        .await
        .map_err(|_| PluginError::Overloaded)?
        .map_err(|_| PluginError::Overloaded)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 面向事件 / JS 的错误文本：`Runtime` 只给原因，其余用 `Display`。
fn plain(error: &PluginError) -> String {
    match error {
        PluginError::Runtime(message) => message.clone(),
        other => other.to_string(),
    }
}

fn fatal(error: &PluginError) -> bool {
    matches!(
        error,
        PluginError::Timeout | PluginError::MemoryLimitExceeded
    )
}

/// 宿主 + manifest 指纹：manifest 或 `main` 源码变化时，旧宿主必须立即释放。
fn host_fingerprint(manifest: &PluginManifest, source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{manifest:?}").hash(&mut hasher);
    source.hash(&mut hasher);
    hasher.finish()
}

struct HostEntry {
    host: Arc<dyn PluginHost>,
    fingerprint: u64,
    background: bool,
    /// 进行中的调用数（含排队）。
    inflight: Arc<AtomicUsize>,
    /// 最近一次观察到「忙」的时刻（空闲回收的起点）。
    last_busy: Instant,
}

struct SessionEntry {
    identity: String,
    last_keepalive: Instant,
    /// `open_view` 已返回；此前到达的渲染结果只暂存，避免先于首帧到达 UI。
    opened: bool,
    last_render: Option<Instant>,
    /// 节流期间暂存的最新树。
    pending: Option<serde_json::Value>,
    flush_scheduled: bool,
    refresh_pending: bool,
    /// 每次入队分配的单调票号；晚到的旧结果直接丢弃。
    next_ticket: u64,
    last_ticket: u64,
}

/// 每插件 `flux.tasks.create` 的滑动窗口限流。
struct CreateLimiter {
    hits: Mutex<VecDeque<Instant>>,
}

impl CreateLimiter {
    fn new() -> Self {
        Self {
            hits: Mutex::new(VecDeque::new()),
        }
    }

    fn try_acquire(&self, now: Instant) -> bool {
        let mut hits = lock(&self.hits);
        while hits
            .front()
            .is_some_and(|at| now.duration_since(*at) >= CREATE_WINDOW)
        {
            hits.pop_front();
        }
        if hits.len() >= CREATE_LIMIT {
            return false;
        }
        hits.push_back(now);
        true
    }
}

/// manager 持有的全部宿主状态。
pub(super) struct HostState {
    hosts: Mutex<HashMap<String, HostEntry>>,
    images: super::super::images::ImageResources,
    sessions: Mutex<HashMap<String, SessionEntry>>,
    timers: Mutex<HashMap<(String, u32), AbortHandle>>,
    limiters: Mutex<HashMap<String, Arc<CreateLimiter>>>,
    /// 按 identity 串行化宿主启动，避免并发激活启动两份。
    activation: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    task_port: OnceLock<Arc<dyn PluginTaskPort>>,
    ticker: AtomicBool,
    /// 后台宿主启动队列（`start_background_hosts` 之后才存在）；`load_all` 对账时
    /// 经它请求启动，避免在 `load_all` 的 future 里直接依赖宿主启动路径。
    background_tx: OnceLock<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl HostState {
    pub(super) fn new() -> Self {
        Self {
            hosts: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            images: super::super::images::ImageResources::default(),
            timers: Mutex::new(HashMap::new()),
            limiters: Mutex::new(HashMap::new()),
            activation: Mutex::new(HashMap::new()),
            task_port: OnceLock::new(),
            ticker: AtomicBool::new(false),
            background_tx: OnceLock::new(),
        }
    }
}

/// RAII：进行中调用计数。
struct InflightGuard(Arc<AtomicUsize>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 已入队的调用：future + 进行中计数守卫。
struct Enqueued {
    fut: HostFuture,
    guard: InflightGuard,
}

/// 把宿主回调转给 manager（弱引用，manager 释放后全部变成空操作）。
struct HostEventsImpl {
    manager: Weak<PluginManager>,
}

#[async_trait::async_trait]
impl HostEvents for HostEventsImpl {
    fn request_render(&self, session: &str) {
        if let Some(manager) = self.manager.upgrade() {
            manager.schedule_render(session);
        }
    }

    fn toast(&self, identity: &str, level: &str, text: &str) {
        if let Some(manager) = self.manager.upgrade() {
            manager.sink.emit(EngineEvent::PluginToast {
                identity: identity.to_string(),
                level: level.to_string(),
                text: text.to_string(),
            });
        }
    }

    fn timer(&self, identity: &str, id: u32, delay: Duration) {
        if let Some(manager) = self.manager.upgrade() {
            manager.schedule_timer(identity, id, delay);
        }
    }

    fn timer_clear(&self, identity: &str, id: u32) {
        if let Some(manager) = self.manager.upgrade() {
            lock(&manager.host.timers)
                .remove(&(identity.to_string(), id))
                .inspect(AbortHandle::abort);
        }
    }

    async fn settings_update(
        &self,
        identity: &str,
        entries: Vec<(String, String)>,
    ) -> Result<(), PluginError> {
        let Some(manager) = self.manager.upgrade() else {
            return Err(PluginError::Runtime("plugin manager stopped".to_string()));
        };
        manager.host_settings_update(identity, entries).await
    }
    async fn archive_rules(
        &self,
        identity: &str,
        refresh: bool,
    ) -> Result<serde_json::Value, PluginError> {
        let Some(manager) = self.manager.upgrade() else {
            return Err(PluginError::Runtime("plugin manager stopped".to_string()));
        };
        manager.archive_rules_status(identity, refresh).await
    }

    fn close_view(&self, identity: &str, session: &str) -> Result<(), PluginError> {
        let Some(manager) = self.manager.upgrade() else {
            return Err(PluginError::Runtime("plugin manager stopped".to_string()));
        };
        manager.host_view_close(identity, session)
    }

    async fn navigate(
        &self,
        identity: &str,
        session: &str,
        target: serde_json::Value,
    ) -> Result<(), PluginError> {
        let Some(manager) = self.manager.upgrade() else {
            return Err(PluginError::Runtime("plugin manager stopped".to_string()));
        };
        manager.host_view_navigate(identity, session, target).await
    }

    async fn image_load(
        &self,
        identity: &str,
        session: &str,
        options: super::super::images::ImageLoadOptions,
    ) -> Result<String, PluginError> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| PluginError::Runtime("plugin manager stopped".to_string()))?;
        manager.host_image_load(identity, session, options).await
    }

    async fn image_release(
        &self,
        identity: &str,
        session: &str,
        src: &str,
    ) -> Result<(), PluginError> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| PluginError::Runtime("plugin manager stopped".to_string()))?;
        let sessions = lock(&manager.host.sessions);
        if sessions.get(session).map(|entry| entry.identity.as_str()) != Some(identity) {
            return Err(PluginError::Runtime("unknown view session".to_string()));
        }
        manager.host.images.release(identity, session, src)
    }
}

/// `flux.tasks.*` 的 manager 侧包装：审计 + `create` 限流，再转给 daemon 端口。
struct ScopedTaskPort {
    identity: String,
    inner: Arc<dyn PluginTaskPort>,
    audit: AuditLog,
    limiter: Arc<CreateLimiter>,
}

impl ScopedTaskPort {
    fn record(&self, action: &str, target: String, outcome: &str, detail: String) {
        self.audit.record(PluginAuditRow::new(
            &self.identity,
            "daemon",
            action,
            target,
            outcome,
            detail,
        ));
    }
}

#[async_trait::async_trait]
impl PluginTaskPort for ScopedTaskPort {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        let (action, target) = match method {
            "daemon.task.create" => {
                let url = params
                    .pointer("/request/url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !self.limiter.try_acquire(Instant::now()) {
                    self.record(
                        "tasks.create",
                        url,
                        "denied",
                        "tasks.create rate limited".to_string(),
                    );
                    return Err(PluginError::Runtime(
                        "tasks.create rate limited".to_string(),
                    ));
                }
                ("tasks.create", url)
            }
            "daemon.task.pause" | "daemon.task.resume" | "daemon.task.delete" => {
                let verb = method.rsplit('.').next().unwrap_or(method);
                let id = params
                    .get("taskId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                ("tasks.control", format!("{verb} {id}"))
            }
            _ => return self.inner.call(method, params).await,
        };
        let result = self.inner.call(method, params).await;
        match &result {
            Ok(_) => self.record(action, target, "ok", String::new()),
            Err(e) => self.record(action, target, "error", plain(e)),
        }
        result
    }
}

impl PluginManager {
    // ---------------------------------------------------------------------
    // 装配
    // ---------------------------------------------------------------------

    /// 注入 `flux.tasks.*` 的出口（daemon 装配时调用一次）。
    pub fn set_task_port(self: &Arc<Self>, port: Arc<dyn PluginTaskPort>) {
        if self.host.task_port.set(port).is_err() {
            log_info!("[plugin] 任务端口已设置，忽略重复设置");
        }
    }

    /// 启动全部 `main.background == true` 的已启用插件宿主；此后 `load_all`
    /// 对账也会补启动新启用的 background 插件。
    pub async fn start_background_hosts(self: &Arc<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        if self.host.background_tx.set(tx).is_ok() {
            let weak = Arc::downgrade(self);
            self.runtime.spawn_handle().spawn(async move {
                while let Some(identity) = rx.recv().await {
                    let Some(me) = weak.upgrade() else {
                        return;
                    };
                    if let Err(e) = me.ensure_host(&identity).await {
                        log_info!("[plugin] {identity} 后台宿主启动失败: {e}");
                    }
                }
            });
        }
        let snapshot = self.plugins.read().await.clone();
        for plugin in snapshot.iter() {
            let background = plugin.manifest.main.as_ref().is_some_and(|m| m.background);
            if plugin.enabled && background {
                let identity = plugin.manifest.identity.clone();
                if let Err(e) = self.ensure_host(&identity).await {
                    log_info!("[plugin] {identity} 后台宿主启动失败: {e}");
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // 公开接口
    // ---------------------------------------------------------------------

    /// 执行插件命令（`flux.commands.register` 注册的处理器）。
    pub async fn run_command(
        self: &Arc<Self>,
        identity: &str,
        id: &str,
        task_ids: Vec<String>,
    ) -> Result<(), PluginError> {
        let declared = {
            let snapshot = self.plugins.read().await.clone();
            snapshot
                .iter()
                .find(|p| p.manifest.identity == identity)
                .and_then(|p| p.manifest.contributes.as_ref())
                .is_some_and(|c| c.commands.iter().any(|command| command.id == id))
        };
        if !declared {
            return Err(PluginError::Runtime(format!("command not declared: {id}")));
        }
        self.host_call(
            identity,
            HostCall::Command {
                id: id.to_string(),
                task_ids,
            },
        )
        .await
        .map(|_| ())
    }

    /// 调用插件 `flux.rpc.handle` 注册的处理器（桌面视图调用，ctx 为 `{ caller: "desktop" }`）。
    pub async fn invoke(
        self: &Arc<Self>,
        identity: &str,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        self.invoke_value(identity, name, args, None)
            .await
            .map(|(value, _)| value)
    }

    /// 浏览器页面脚本调用插件处理器。插件须启用、已加载、声明 `pageScripts` 权限且无待授权项；
    /// 处理器须以 `{ page: true }` 注册，否则报 `rpc not exposed to page: <name>`。
    /// 返回处理器结果与本次调用期间插件经 `flux.tasks` 开始的任务。
    pub async fn invoke_from_page(
        self: &Arc<Self>,
        identity: &str,
        name: &str,
        args: serde_json::Value,
        page_url: &str,
    ) -> Result<(serde_json::Value, StartedTasks), PluginError> {
        let manifest = {
            let snapshot = self.plugins.read().await.clone();
            let plugin = snapshot
                .iter()
                .find(|p| p.manifest.identity == identity)
                .ok_or_else(|| PluginError::Runtime(format!("plugin not found: {identity}")))?;
            if !plugin.enabled {
                return Err(PluginError::Runtime(format!(
                    "plugin disabled: {identity} ({})",
                    plugin.disabled_reason.describe()
                )));
            }
            plugin.manifest.clone()
        };
        if manifest.main.is_none() {
            return Err(PluginError::Runtime(format!(
                "plugin has no main: {identity}"
            )));
        }
        if !manifest.has_permission(PERMISSION_PAGE_SCRIPTS)
            || manifest
                .contributes
                .as_ref()
                .is_none_or(|c| c.page_scripts.is_empty())
        {
            return Err(PluginError::Runtime(format!(
                "plugin has no pageScripts permission: {identity}"
            )));
        }
        let (pending, pending_hosts) = self.pending_for(&manifest).await;
        if !pending.is_empty() || !pending_hosts.is_empty() {
            return Err(PluginError::Runtime(format!(
                "plugin consent pending: {identity}"
            )));
        }
        self.invoke_value(identity, name, args, Some(page_url.to_string()))
            .await
    }

    async fn invoke_value(
        self: &Arc<Self>,
        identity: &str,
        name: &str,
        args: serde_json::Value,
        page_url: Option<String>,
    ) -> Result<(serde_json::Value, StartedTasks), PluginError> {
        match self
            .host_call(
                identity,
                HostCall::Invoke {
                    name: name.to_string(),
                    args,
                    page_url,
                },
            )
            .await?
        {
            HostReply::Value { value, started } => Ok((value, started)),
            _ => Ok((serde_json::Value::Null, StartedTasks::default())),
        }
    }

    /// 打开一个可移植视图会话，返回 `(session, 首帧原始树)`。
    pub async fn open_view(
        self: &Arc<Self>,
        identity: &str,
        view: &str,
        props: serde_json::Value,
        task_id: Option<String>,
        locale: &str,
    ) -> Result<(String, serde_json::Value), PluginError> {
        {
            let snapshot = self.plugins.read().await.clone();
            let plugin = snapshot
                .iter()
                .find(|p| p.manifest.identity == identity)
                .ok_or_else(|| PluginError::Runtime(format!("plugin not found: {identity}")))?;
            if !plugin.enabled {
                return Err(PluginError::Runtime(format!(
                    "plugin disabled: {identity} ({})",
                    plugin.disabled_reason.describe()
                )));
            }
            if plugin.manifest.main.is_none() {
                return Err(PluginError::Runtime(format!(
                    "plugin has no main: {identity}"
                )));
            }
            let portable = plugin
                .manifest
                .contributes
                .as_ref()
                .and_then(|c| c.views.iter().find(|v| v.id == view))
                .map(|v| v.is_portable());
            match portable {
                None => return Err(PluginError::Runtime(format!("view not declared: {view}"))),
                Some(false) => {
                    return Err(PluginError::Runtime(format!(
                        "view is desktop-only: {view}"
                    )));
                }
                Some(true) => {}
            }
        }
        self.ensure_host(identity).await?;

        let session = uuid::Uuid::new_v4().to_string();
        {
            let mut sessions = lock(&self.host.sessions);
            sessions.insert(
                session.clone(),
                SessionEntry {
                    identity: identity.to_string(),
                    last_keepalive: Instant::now(),
                    opened: false,
                    last_render: None,
                    pending: None,
                    flush_scheduled: false,
                    refresh_pending: false,
                    next_ticket: 0,
                    last_ticket: 0,
                },
            );
            self.host.images.open_session(identity, &session);
        }
        let call = HostCall::ViewOpen {
            session: session.clone(),
            view: view.to_string(),
            props,
            task_id,
            locale: locale.to_string(),
        };
        let outcome = match self.enqueue(identity, call) {
            Ok(enqueued) => self.finish_call(identity, enqueued).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(HostReply::Tree(tree)) => {
                self.mark_opened(&session);
                Ok((session, tree))
            }
            Ok(_) => {
                lock(&self.host.sessions).remove(&session);
                self.host.images.close_session(&session);
                Err(PluginError::InvalidOutput(
                    "view open returned no tree".to_string(),
                ))
            }
            Err(e) => {
                lock(&self.host.sessions).remove(&session);
                self.host.images.close_session(&session);
                Err(e)
            }
        }
    }

    /// 投递视图事件：入队后立即返回，处理结果经 `PluginViewRendered` /
    /// `PluginViewFailed` 事件送出。同一会话的事件按调用顺序处理。
    pub fn view_event(
        self: &Arc<Self>,
        session: &str,
        event: serde_json::Value,
    ) -> Result<(), PluginError> {
        let (identity, ticket, enqueued) = self.enqueue_session_call(
            session,
            HostCall::ViewEvent {
                session: session.to_string(),
                event,
            },
        )?;
        let me = self.clone();
        let session = session.to_string();
        self.runtime.spawn_handle().spawn(async move {
            me.finish_tree(identity, session, ticket, enqueued).await;
        });
        Ok(())
    }

    /// 关闭会话（客户端主动）。未知会话静默忽略。
    pub fn close_view(self: &Arc<Self>, session: &str) {
        self.close_view_with_reason(session, "closed");
    }

    /// 以给定 `reason` 结束会话并广播 `PluginViewClosed`。未知会话静默忽略。
    fn close_view_with_reason(self: &Arc<Self>, session: &str, reason: &str) {
        let removed = lock(&self.host.sessions).remove(session);
        let Some(entry) = removed else {
            return;
        };
        self.host.images.close_session(session);
        self.sink.emit(EngineEvent::PluginViewClosed {
            session: session.to_string(),
            reason: reason.to_string(),
        });
        self.notify_host_closed(&entry.identity, session);
    }

    /// 续期会话；超过 90 s 没有 keepalive 的会话会被节拍关闭。
    pub fn keepalive(&self, sessions: &[String]) {
        let now = Instant::now();
        let mut map = lock(&self.host.sessions);
        for id in sessions {
            if let Some(entry) = map.get_mut(id) {
                entry.last_keepalive = now;
            }
        }
    }

    // ---------------------------------------------------------------------
    // 宿主生命周期
    // ---------------------------------------------------------------------

    fn live_host(&self, identity: &str) -> Option<Arc<dyn PluginHost>> {
        lock(&self.host.hosts)
            .get(identity)
            .map(|entry| entry.host.clone())
    }

    fn activation_lock(&self, identity: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.host.activation)
            .entry(identity.to_string())
            .or_default()
            .clone()
    }

    /// 取得（必要时启动）插件宿主。
    async fn ensure_host(
        self: &Arc<Self>,
        identity: &str,
    ) -> Result<Arc<dyn PluginHost>, PluginError> {
        if let Some(host) = self.live_host(identity) {
            return Ok(host);
        }
        let activation = self.activation_lock(identity);
        let _serial = activation.lock().await;
        if let Some(host) = self.live_host(identity) {
            return Ok(host);
        }

        let snapshot = self.plugins.read().await.clone();
        let plugin = snapshot
            .iter()
            .find(|p| p.manifest.identity == identity)
            .ok_or_else(|| PluginError::Runtime(format!("plugin not found: {identity}")))?;
        if !plugin.enabled {
            return Err(PluginError::Runtime(format!(
                "plugin disabled: {identity} ({})",
                plugin.disabled_reason.describe()
            )));
        }
        let Some(main) = &plugin.manifest.main else {
            return Err(PluginError::Runtime(format!(
                "plugin has no main: {identity}"
            )));
        };
        let source = plugin
            .main_source()
            .await
            .ok_or_else(|| PluginError::Runtime(format!("读取 main 失败: {identity}")))?;
        let fingerprint = host_fingerprint(&plugin.manifest, &source);
        let values = load_setting_values_db(&self.db, identity).await;
        let settings_json = build_typed_settings_json(&plugin.manifest, &values);
        let context = HostContext {
            ffmpeg_permitted: plugin.manifest.has_permission(PERMISSION_FFMPEG),
            ffmpeg_root: None,
            ytdlp_permitted: plugin.manifest.has_permission(PERMISSION_YTDLP),
            auth_permitted: plugin.manifest.has_permission(PERMISSION_AUTH),
            private_network_allowed: plugin.dev,
            // 常驻宿主不绑定某次调用：按全局规则 + 全局模式选出口。
            egress: None,
        };
        let task_port: Option<Arc<dyn PluginTaskPort>> =
            if plugin.manifest.has_permission(PERMISSION_TASKS) {
                self.host.task_port.get().map(|inner| {
                    let limiter = lock(&self.host.limiters)
                        .entry(identity.to_string())
                        .or_insert_with(|| Arc::new(CreateLimiter::new()))
                        .clone();
                    Arc::new(ScopedTaskPort {
                        identity: identity.to_string(),
                        inner: inner.clone(),
                        audit: self.audit.clone(),
                        limiter,
                    }) as Arc<dyn PluginTaskPort>
                })
            } else {
                None
            };
        let events: Arc<dyn HostEvents> = Arc::new(HostEventsImpl {
            manager: Arc::downgrade(self),
        });
        let script = PluginScript {
            identity: identity.to_string(),
            source,
            entry_fn_hint: PluginEntryKind::Main,
            version: plugin.manifest.version.clone(),
            app_version: self.app_version.clone(),
        };
        let started = self
            .runtime
            .start_host(
                script,
                settings_json,
                self.bridge.clone(),
                context,
                task_port,
                events,
            )
            .await;
        let host = match started {
            Ok(host) => host,
            Err(e) => {
                self.note_failure(identity, &e).await;
                return Err(e);
            }
        };
        lock(&self.host.hosts).insert(
            identity.to_string(),
            HostEntry {
                host: host.clone(),
                fingerprint,
                background: main.background,
                inflight: Arc::new(AtomicUsize::new(0)),
                last_busy: Instant::now(),
            },
        );
        self.ensure_ticker();
        Ok(host)
    }

    /// 释放宿主：停 actor、取消定时器、结束其全部会话（`pluginUnloaded`）。
    pub(super) fn dispose_host(&self, identity: &str) {
        let removed = lock(&self.host.hosts).remove(identity);
        if let Some(entry) = removed {
            entry.host.dispose();
        }
        lock(&self.host.timers).retain(|(owner, _), handle| {
            let keep = owner != identity;
            if !keep {
                handle.abort();
            }
            keep
        });
        self.close_sessions_of(identity, "pluginUnloaded");
        self.host.images.close_plugin(identity);
    }

    /// 结束某插件的全部会话并广播 `PluginViewClosed`。
    fn close_sessions_of(&self, identity: &str, reason: &str) {
        let closed: Vec<String> = {
            let mut sessions = lock(&self.host.sessions);
            let ids: Vec<String> = sessions
                .iter()
                .filter(|(_, entry)| entry.identity == identity)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                sessions.remove(id);
            }
            ids
        };
        for session in closed {
            self.sink.emit(EngineEvent::PluginViewClosed {
                session,
                reason: reason.to_string(),
            });
        }
    }

    /// `load_all` 尾部对账：释放已不应存在的宿主；补启动新启用的 background 宿主。
    pub(super) async fn reconcile_hosts(&self) {
        let snapshot = self.plugins.read().await.clone();
        let running: Vec<(String, u64)> = lock(&self.host.hosts)
            .iter()
            .map(|(identity, entry)| (identity.clone(), entry.fingerprint))
            .collect();
        for (identity, fingerprint) in running {
            let wanted = match snapshot.iter().find(|p| p.manifest.identity == identity) {
                Some(plugin) if plugin.enabled && plugin.manifest.main.is_some() => {
                    let source = plugin.main_source().await.unwrap_or_default();
                    Some(host_fingerprint(&plugin.manifest, &source))
                }
                _ => None,
            };
            if wanted != Some(fingerprint) {
                self.dispose_host(&identity);
            }
        }

        let Some(tx) = self.host.background_tx.get() else {
            return;
        };
        for plugin in snapshot.iter() {
            let background = plugin.manifest.main.as_ref().is_some_and(|m| m.background);
            let identity = plugin.manifest.identity.clone();
            if plugin.enabled
                && background
                && self.live_host(&identity).is_none()
                && tx.send(identity.clone()).is_err()
            {
                log_info!("[plugin] {identity} 后台宿主启动队列已关闭");
            }
        }
    }

    // ---------------------------------------------------------------------
    // 调用管线
    // ---------------------------------------------------------------------

    /// 入队一次调用（同步完成入队，保证先后顺序）。
    fn enqueue(&self, identity: &str, call: HostCall) -> Result<Enqueued, PluginError> {
        let mut hosts = lock(&self.host.hosts);
        let entry = hosts
            .get_mut(identity)
            .ok_or_else(|| PluginError::Runtime(format!("plugin host not running: {identity}")))?;
        entry.inflight.fetch_add(1, Ordering::AcqRel);
        entry.last_busy = Instant::now();
        let guard = InflightGuard(entry.inflight.clone());
        let fut = entry.host.call(call);
        Ok(Enqueued { fut, guard })
    }

    /// 会话内的调用：分配票号并入队（锁序：sessions → hosts）。
    fn enqueue_session_call(
        &self,
        session: &str,
        call: HostCall,
    ) -> Result<(String, u64, Enqueued), PluginError> {
        let mut sessions = lock(&self.host.sessions);
        let entry = sessions
            .get_mut(session)
            .ok_or_else(|| PluginError::Runtime(format!("unknown session: {session}")))?;
        entry.next_ticket += 1;
        let ticket = entry.next_ticket;
        let identity = entry.identity.clone();
        let enqueued = self.enqueue(&identity, call)?;
        Ok((identity, ticket, enqueued))
    }

    /// 等待调用完成并做熔断记账：成功清零连续失败，超时 / OOM 累计并释放宿主。
    async fn finish_call(
        self: &Arc<Self>,
        identity: &str,
        enqueued: Enqueued,
    ) -> Result<HostReply, PluginError> {
        let Enqueued { fut, guard } = enqueued;
        let outcome = fut.await;
        drop(guard);
        if let Some(entry) = lock(&self.host.hosts).get_mut(identity) {
            entry.last_busy = Instant::now();
        }
        match outcome {
            Ok(reply) => {
                if let Some(streak) = self.streak_of(identity).await {
                    streak.store(0, Ordering::SeqCst);
                }
                Ok(reply)
            }
            Err(e) => {
                self.note_failure(identity, &e).await;
                Err(e)
            }
        }
    }

    async fn host_call(
        self: &Arc<Self>,
        identity: &str,
        call: HostCall,
    ) -> Result<HostReply, PluginError> {
        self.ensure_host(identity).await?;
        let enqueued = self.enqueue(identity, call)?;
        self.finish_call(identity, enqueued).await
    }

    async fn streak_of(&self, identity: &str) -> Option<Arc<std::sync::atomic::AtomicU32>> {
        let snapshot = self.plugins.read().await.clone();
        snapshot
            .iter()
            .find(|p| p.manifest.identity == identity)
            .map(|p| p.timeout_streak.clone())
    }

    /// 超时 / OOM：JS 状态不再可信，释放宿主并累计熔断计数。
    async fn note_failure(self: &Arc<Self>, identity: &str, error: &PluginError) {
        if !fatal(error) {
            return;
        }
        self.dispose_host(identity);
        let Some(streak) = self.streak_of(identity).await else {
            return;
        };
        let count = streak.fetch_add(1, Ordering::SeqCst) + 1;
        if count >= CIRCUIT_BREAKER_THRESHOLD {
            self.trip_circuit_breaker(identity).await;
        }
    }

    /// 视图调用的结果处理：成功 → 节流投递；失败 → `PluginViewFailed`。
    async fn finish_tree(
        self: &Arc<Self>,
        identity: String,
        session: String,
        ticket: u64,
        enqueued: Enqueued,
    ) {
        match self.finish_call(&identity, enqueued).await {
            Ok(HostReply::Tree(tree)) => self.deliver_tree(&session, ticket, tree),
            Ok(_) => {}
            Err(e) => {
                if lock(&self.host.sessions).contains_key(&session) {
                    self.sink.emit(EngineEvent::PluginViewFailed {
                        session,
                        message: plain(&e),
                    });
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // 渲染节流
    // ---------------------------------------------------------------------

    /// 投递一棵渲染结果：同一会话两次渲染至少间隔 100 ms，期间只保留最新一棵。
    fn deliver_tree(self: &Arc<Self>, session: &str, ticket: u64, tree: serde_json::Value) {
        let mut sessions = lock(&self.host.sessions);
        let Some(entry) = sessions.get_mut(session) else {
            return;
        };
        if ticket < entry.last_ticket {
            return;
        }
        entry.last_ticket = ticket;
        if !entry.opened {
            entry.pending = Some(tree);
            return;
        }
        let now = Instant::now();
        let due = entry
            .last_render
            .is_none_or(|at| now.duration_since(at) >= RENDER_MIN_INTERVAL);
        if due && !entry.flush_scheduled {
            entry.last_render = Some(now);
            self.sink.emit(EngineEvent::PluginViewRendered {
                session: session.to_string(),
                tree,
            });
            return;
        }
        entry.pending = Some(tree);
        if !entry.flush_scheduled {
            entry.flush_scheduled = true;
            let wait = entry
                .last_render
                .map_or(RENDER_MIN_INTERVAL, |at| {
                    RENDER_MIN_INTERVAL.saturating_sub(now.duration_since(at))
                })
                .max(Duration::from_millis(1));
            self.spawn_flush(session.to_string(), wait);
        }
    }

    fn spawn_flush(self: &Arc<Self>, session: String, wait: Duration) {
        let me = self.clone();
        self.runtime.spawn_handle().spawn(async move {
            tokio::time::sleep(wait).await;
            let mut sessions = lock(&me.host.sessions);
            let Some(entry) = sessions.get_mut(&session) else {
                return;
            };
            entry.flush_scheduled = false;
            if let Some(tree) = entry.pending.take() {
                entry.last_render = Some(Instant::now());
                me.sink
                    .emit(EngineEvent::PluginViewRendered { session, tree });
            }
        });
    }

    /// `open_view` 成功返回后：开闸，并安排在节流窗口后补发期间暂存的树。
    fn mark_opened(self: &Arc<Self>, session: &str) {
        let mut sessions = lock(&self.host.sessions);
        let Some(entry) = sessions.get_mut(session) else {
            return;
        };
        entry.opened = true;
        entry.last_render = Some(Instant::now());
        if entry.pending.is_some() && !entry.flush_scheduled {
            entry.flush_scheduled = true;
            self.spawn_flush(session.to_string(), RENDER_MIN_INTERVAL);
        }
    }

    /// `ctx.refresh()`：合并窗口内的重复请求，窗口结束后渲染一次。
    fn schedule_render(self: &Arc<Self>, session: &str) {
        let wait = {
            let mut sessions = lock(&self.host.sessions);
            let Some(entry) = sessions.get_mut(session) else {
                return;
            };
            if entry.refresh_pending {
                return;
            }
            entry.refresh_pending = true;
            entry.last_render.map_or(Duration::ZERO, |at| {
                RENDER_MIN_INTERVAL.saturating_sub(at.elapsed())
            })
        };
        let me = self.clone();
        let session = session.to_string();
        self.runtime.spawn_handle().spawn(async move {
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            me.run_refresh(session).await;
        });
    }

    async fn run_refresh(self: &Arc<Self>, session: String) {
        if let Some(entry) = lock(&self.host.sessions).get_mut(&session) {
            entry.refresh_pending = false;
        } else {
            return;
        }
        match self.enqueue_session_call(
            &session,
            HostCall::ViewRefresh {
                session: session.clone(),
            },
        ) {
            Ok((identity, ticket, enqueued)) => {
                self.finish_tree(identity, session, ticket, enqueued).await;
            }
            Err(e) => log_info!("[plugin] 视图刷新入队失败: {e}"),
        }
    }

    /// 通知宿主会话已关闭（尽力而为，结果不影响调用方）。
    fn notify_host_closed(self: &Arc<Self>, identity: &str, session: &str) {
        let Ok(enqueued) = self.enqueue(
            identity,
            HostCall::ViewClose {
                session: session.to_string(),
            },
        ) else {
            return;
        };
        let me = self.clone();
        let identity = identity.to_string();
        self.runtime.spawn_handle().spawn(async move {
            if let Err(e) = me.finish_call(&identity, enqueued).await {
                log_info!("[plugin] {identity} 会话关闭回调失败: {e}");
            }
        });
    }

    // ---------------------------------------------------------------------
    // 定时器
    // ---------------------------------------------------------------------

    fn schedule_timer(self: &Arc<Self>, identity: &str, id: u32, delay: Duration) {
        let me = self.clone();
        let owner = identity.to_string();
        let task = self.runtime.spawn_handle().spawn(async move {
            tokio::time::sleep(delay).await;
            me.fire_timer(owner, id).await;
        });
        let previous =
            lock(&self.host.timers).insert((identity.to_string(), id), task.abort_handle());
        if let Some(previous) = previous {
            previous.abort();
        }
    }

    async fn fire_timer(self: &Arc<Self>, identity: String, id: u32) {
        // 先入队（计入进行中），再摘除登记：避免「无定时器 + 无调用」的瞬时空档被误判空闲。
        let enqueued = self.enqueue(&identity, HostCall::Timer { id });
        lock(&self.host.timers).remove(&(identity.clone(), id));
        let Ok(enqueued) = enqueued else {
            return;
        };
        if let Err(e) = self.finish_call(&identity, enqueued).await {
            log_info!("[plugin] {identity} 定时器回调失败: {e}");
        }
    }

    // ---------------------------------------------------------------------
    // 设置
    // ---------------------------------------------------------------------

    /// 校验 `session` 属于 `identity`（`ctx.close/navigate` 共用）。`init` 期间（首帧返回前）也
    /// 放行：动作经事件送达客户端，客户端在 open 应答前收到的动作会先缓存、登记会话后再执行。
    fn check_view_session(&self, identity: &str, session: &str) -> Result<(), PluginError> {
        match lock(&self.host.sessions).get(session) {
            Some(entry) if entry.identity == identity => Ok(()),
            _ => Err(PluginError::Runtime(format!("unknown session: {session}"))),
        }
    }

    /// 会话是否仍存在（daemon 在 open 应答前据此清理插件已在 `init` 中关闭的会话）。
    #[must_use]
    pub fn has_view_session(&self, session: &str) -> bool {
        lock(&self.host.sessions).contains_key(session)
    }

    /// Read a sanitized PNG resource only within its owning live view session.
    #[must_use]
    pub fn view_image(&self, session: &str, id: &str) -> Option<Arc<[u8]>> {
        let sessions = lock(&self.host.sessions);
        sessions.get(session)?;
        self.host.images.read(session, id)
    }

    async fn host_image_load(
        &self,
        identity: &str,
        session: &str,
        mut options: super::super::images::ImageLoadOptions,
    ) -> Result<String, PluginError> {
        self.check_view_session(identity, session)?;
        options.validate()?;
        let snapshot = self.plugins.read().await.clone();
        let plugin = snapshot
            .iter()
            .find(|plugin| plugin.enabled && plugin.manifest.identity == identity)
            .ok_or_else(|| PluginError::Runtime("plugin is not enabled".to_string()))?;
        let hosts = plugin
            .manifest
            .main
            .as_ref()
            .map(|main| main.image_hosts.clone())
            .unwrap_or_default();
        let auth_allowed = plugin.manifest.has_permission(PERMISSION_AUTH);
        // Authentication may rotate without changing authRef; never reuse a previous fetch.
        if auth_allowed {
            options.cache = false;
        }
        let url = url::Url::parse(&options.url)
            .map_err(|error| PluginError::InvalidOutput(format!("invalid image URL: {error}")))?;
        if !url.host_str().is_some_and(|host| {
            let host = host.trim_matches(['[', ']']);
            hosts.iter().any(|allowed| allowed == host)
        }) {
            return Err(PluginError::Runtime(
                "image host is not declared in main.imageHosts".to_string(),
            ));
        }
        if let Some(src) = self.host.images.cached(identity, session, &options)? {
            return Ok(src);
        }
        let _permit = acquire_image_load(&IMAGE_LOADS).await?;
        self.check_view_session(identity, session)?;
        if let Some(src) = self.host.images.cached(identity, session, &options)? {
            return Ok(src);
        }
        let request = super::super::runtime::BridgeHttpRequest {
            url: options.url.clone(),
            headers: options.headers.clone(),
            auth_ref: options.auth_ref.clone(),
            auth_allowed,
            private_network_allowed: plugin.dev,
            ..Default::default()
        };
        let bytes = self.bridge.image_request(identity, request, hosts).await?;
        let bytes = super::super::images::thumbnail(bytes, &options).await?;
        self.check_view_session(identity, session)?;
        self.host.images.insert(identity, session, &options, bytes)
    }

    /// `ctx.close()`：原因固定为 `requested`（对应 `fluxdown_protocol::VIEW_CLOSE_REASON_REQUESTED`；
    /// 引擎不依赖 protocol crate，故在此保持同值）。
    fn host_view_close(self: &Arc<Self>, identity: &str, session: &str) -> Result<(), PluginError> {
        self.check_view_session(identity, session)?;
        self.close_view_with_reason(session, "requested");
        Ok(())
    }

    /// `ctx.navigate(target)`：把 target 解析为路由 id（对应 `fluxdown_protocol::plugin_route_id`
    /// 与 `NAV_BUILTIN_TARGETS`）并发出 `PluginViewNavigate`。
    async fn host_view_navigate(
        &self,
        identity: &str,
        session: &str,
        target: serde_json::Value,
    ) -> Result<(), PluginError> {
        self.check_view_session(identity, session)?;
        let route = match &target {
            serde_json::Value::String(s)
                if ["downloads", "rss", "webhooks"].contains(&s.as_str()) =>
            {
                s.clone()
            }
            serde_json::Value::Object(map) if map.len() == 1 => {
                let Some(view) = map.get("view").and_then(serde_json::Value::as_str) else {
                    return Err(PluginError::Runtime(format!(
                        "navigate: unknown target {target}"
                    )));
                };
                let snapshot = self.plugins.read().await.clone();
                let is_activity = snapshot
                    .iter()
                    .find(|p| p.manifest.identity == identity)
                    .and_then(|p| p.manifest.contributes.as_ref())
                    .is_some_and(|c| {
                        c.views
                            .iter()
                            .any(|v| v.id == view && v.location == ViewLocation::Activity)
                    });
                if !is_activity {
                    return Err(PluginError::Runtime(format!(
                        "navigate: view '{view}' is not an activity view of this plugin"
                    )));
                }
                format!("plugin:{identity}:{view}")
            }
            _ => {
                return Err(PluginError::Runtime(format!(
                    "navigate: unknown target {target}"
                )));
            }
        };
        self.sink.emit(EngineEvent::PluginViewNavigate {
            session: session.to_string(),
            target: route,
        });
        Ok(())
    }

    /// `flux.settings.update`：与 UI 路径同一校验，额外写审计。
    pub(super) async fn host_settings_update(
        &self,
        identity: &str,
        entries: Vec<(String, String)>,
    ) -> Result<(), PluginError> {
        let keys = entries
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let result = self.update_settings(identity, &entries).await;
        let (outcome, detail) = match &result {
            Ok(()) => ("ok", String::new()),
            Err(e) => ("error", plain(e)),
        };
        self.audit.record(PluginAuditRow::new(
            identity,
            "daemon",
            "settings.update",
            keys,
            outcome,
            detail,
        ));
        result
    }

    /// 设置被修改后，把最新值推给在运行的宿主（就地更新 `flux.settings`）。
    pub(super) async fn push_host_settings(&self, identity: &str) {
        if self.live_host(identity).is_none() {
            return;
        }
        let snapshot = self.plugins.read().await.clone();
        let Some(plugin) = snapshot.iter().find(|p| p.manifest.identity == identity) else {
            return;
        };
        let values = load_setting_values_db(&self.db, identity).await;
        let json = build_typed_settings_json(&plugin.manifest, &values);
        // 入队即完成投递；结果不可观察（Settings 处理器不会失败）。
        if let Err(e) = self.enqueue(identity, HostCall::Settings { json }) {
            log_info!("[plugin] {identity} 设置推送失败: {e}");
        }
    }

    // ---------------------------------------------------------------------
    // 事件投递
    // ---------------------------------------------------------------------

    /// `notify` 路径：把声明在 `main.events` 中的事件投给宿主（必要时激活）。
    pub(super) fn deliver_host_event(self: &Arc<Self>, identity: &str, event: PluginEvent) {
        let me = self.clone();
        let identity = identity.to_string();
        self.runtime.spawn_handle().spawn(async move {
            if let Err(e) = me.host_call(&identity, HostCall::Event(event)).await {
                log_info!("[plugin] {identity} 宿主事件投递失败: {e}");
            }
        });
    }

    // ---------------------------------------------------------------------
    // 节拍
    // ---------------------------------------------------------------------

    /// 启动节拍任务（若尚未运行）。任务在没有任何宿主时自行退出。
    fn ensure_ticker(self: &Arc<Self>) {
        if self.host.ticker.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        self.runtime.spawn_handle().spawn(async move {
            loop {
                tokio::time::sleep(TICK).await;
                let Some(me) = weak.upgrade() else {
                    return;
                };
                if !me.sweep_at(Instant::now()) {
                    return;
                }
            }
        });
    }

    /// 一次节拍：关闭过期会话、回收空闲宿主。返回是否仍有宿主（否则节拍任务退出）。
    pub(super) fn sweep_at(self: &Arc<Self>, now: Instant) -> bool {
        let expired: Vec<(String, String)> = {
            let mut sessions = lock(&self.host.sessions);
            let ids: Vec<String> = sessions
                .iter()
                .filter(|(_, entry)| now.duration_since(entry.last_keepalive) > SESSION_TTL)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| sessions.remove(&id).map(|entry| (id, entry.identity)))
                .collect()
        };
        for (session, identity) in expired {
            self.host.images.close_session(&session);
            self.sink.emit(EngineEvent::PluginViewClosed {
                session: session.clone(),
                reason: "expired".to_string(),
            });
            self.notify_host_closed(&identity, &session);
        }

        let busy_identities: std::collections::HashSet<String> = {
            let sessions = lock(&self.host.sessions);
            let timers = lock(&self.host.timers);
            sessions
                .values()
                .map(|entry| entry.identity.clone())
                .chain(timers.keys().map(|(identity, _)| identity.clone()))
                .collect()
        };
        let idle: Vec<String> = {
            let mut hosts = lock(&self.host.hosts);
            let mut idle = Vec::new();
            for (identity, entry) in hosts.iter_mut() {
                let busy = busy_identities.contains(identity)
                    || entry.inflight.load(Ordering::Acquire) > 0;
                if busy {
                    entry.last_busy = now;
                } else if !entry.background && now.duration_since(entry.last_busy) >= IDLE_RECLAIM {
                    idle.push(identity.clone());
                }
            }
            idle
        };
        for identity in idle {
            self.dispose_host(&identity);
        }

        let hosts = lock(&self.host.hosts);
        if hosts.is_empty() {
            // 在持锁状态下复位，与 `ensure_host` 的「插入 → ensure_ticker」互斥。
            self.host.ticker.store(false, Ordering::Release);
            return false;
        }
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn image_load_waits_for_capacity_instead_of_rejecting() {
        let semaphore = tokio::sync::Semaphore::new(1);
        let held = semaphore.acquire().await.unwrap();
        let waiting = acquire_image_load(&semaphore);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        drop(held);
        let permit = waiting.await.unwrap();
        assert_eq!(semaphore.available_permits(), 0);
        drop(permit);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn image_load_queue_timeout_reports_overload_without_leaking_capacity() {
        let semaphore = tokio::sync::Semaphore::new(1);
        let held = semaphore.acquire().await.unwrap();
        assert!(matches!(
            acquire_image_load(&semaphore).await,
            Err(PluginError::Overloaded)
        ));
        drop(held);
        assert_eq!(semaphore.available_permits(), 1);
        assert!(acquire_image_load(&semaphore).await.is_ok());
    }

    #[test]
    fn create_limiter_allows_twenty_per_window() {
        let limiter = CreateLimiter::new();
        let start = Instant::now();
        for _ in 0..CREATE_LIMIT {
            assert!(limiter.try_acquire(start));
        }
        assert!(!limiter.try_acquire(start));
        // 窗口滑过之后额度恢复。
        assert!(limiter.try_acquire(start + CREATE_WINDOW));
    }

    struct EchoPort;

    #[async_trait::async_trait]
    impl PluginTaskPort for EchoPort {
        async fn call(
            &self,
            _method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value, PluginError> {
            Ok(serde_json::Value::Null)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tasks_create_audit_target_is_request_url() {
        let db = crate::db::Db::connect("sqlite::memory:").await.unwrap();
        let audit = AuditLog::new(db.clone(), &tokio::runtime::Handle::current());
        let port = ScopedTaskPort {
            identity: "a@x".to_string(),
            inner: Arc::new(EchoPort),
            audit: audit.clone(),
            limiter: Arc::new(CreateLimiter::new()),
        };
        port.call(
            "daemon.task.create",
            serde_json::json!({ "request": { "url": "https://example.com/a.bin" } }),
        )
        .await
        .unwrap();
        audit.flush().await;
        let rows = db.list_plugin_audit(None, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "tasks.create");
        assert_eq!(rows[0].target, "https://example.com/a.bin");
    }
}
