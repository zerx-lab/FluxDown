//! 常驻插件宿主的 QuickJS 实现（`main` 入口）。**rquickjs 类型只允许出现在本文件与
//! [`super::quickjs`]**。
//!
//! 与 `quickjs.rs` 的「每次调用全新运行时」相反：一个宿主 = 一个 actor 任务，独占一份
//! `AsyncRuntime` + `AsyncContext`，`main` 源码只在启动时执行一次，注册的视图 / 命令 /
//! RPC / 事件 / 定时器都留在 JS 全局里；之后所有调用经 `mpsc` 串行进入
//! `globalThis.__fluxHost.dispatch(kind, json)`。
//!
//! 预算：内存 64 MiB；每次调用的**中断（CPU）预算** 2 s（挂起在宿主 await 上的时间会
//! 补回预算，同 `quickjs.rs` 的 ffmpeg/yt-dlp 处理）；每次调用的**墙钟**上限 30 s。
//! 超时 / OOM 之后 JS 状态不再可信，actor 随即退出，由 manager 熔断计数并丢弃句柄。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rquickjs::prelude::Async;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Ctx, Function, Object, Promise};
use tokio::sync::{mpsc, oneshot, watch};

use super::quickjs::{FLUX_PRELUDE, info_json, inject_bridge, set_json_global};
use super::runtime::{
    HostCall, HostContext, HostEvents, HostFuture, HostReply, PluginBridge, PluginError,
    PluginHost, PluginScript, PluginTaskPort, StartedTasks, WallPause,
};

/// 宿主 JS 内存上限。
const HOST_MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// 每次调用的中断（CPU）预算。
const HOST_CPU_BUDGET: Duration = Duration::from_secs(2);
/// 每次调用的墙钟上限。
const HOST_WALL_TIMEOUT: Duration = Duration::from_secs(30);
/// 中断截止的「未武装」值：取 `u64::MAX / 2`，使挂起补偿的 `fetch_add` 永不回绕。
const DISARMED_NS: u64 = u64::MAX / 2;
/// `flux.tasks.*` 允许的方法里，算作「开始任务」的两个。
const TASK_CREATE: &str = "daemon.task.create";
const TASK_RESUME: &str = "daemon.task.resume";

/// 当前页面脚本调用开始的任务。actor 串行执行调用：页面调用开始时置 `Some`、结束时取走，
/// 其余调用期间为 `None`。调用返回后仍在运行的悬空 Promise 不归属任何页面调用（`None` 时不记）。
type StartScope = Arc<Mutex<Option<StartedTasks>>>;

fn scope_lock(scope: &StartScope) -> std::sync::MutexGuard<'_, Option<StartedTasks>> {
    scope.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 中断预算状态：handler 读、actor / 桥接函数写。
struct Budget {
    base: Instant,
    deadline_ns: Arc<AtomicU64>,
}

impl Budget {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            deadline_ns: Arc::new(AtomicU64::new(DISARMED_NS)),
        }
    }

    /// 开始一次调用：截止 = 此刻 + CPU 预算。
    fn arm(&self) {
        let now = self.base.elapsed().as_nanos() as u64;
        self.deadline_ns.store(
            now.saturating_add(HOST_CPU_BUDGET.as_nanos() as u64),
            Ordering::Release,
        );
    }

    fn disarm(&self) {
        self.deadline_ns.store(DISARMED_NS, Ordering::Release);
    }

    fn handler(&self) -> Box<dyn FnMut() -> bool + Send + 'static> {
        let base = self.base;
        let dl = self.deadline_ns.clone();
        Box::new(move || base.elapsed().as_nanos() as u64 >= dl.load(Ordering::Acquire))
    }
}

/// 一次排队的调用。
struct Message {
    call: HostCall,
    reply: oneshot::Sender<Result<HostReply, PluginError>>,
}

/// 句柄：入队发生在 `call` 返回之前（保序），`dispose` 立即通知 actor 退出。
struct QuickJsHost {
    tx: mpsc::UnboundedSender<Message>,
    shutdown: watch::Sender<bool>,
}

fn stopped() -> PluginError {
    PluginError::Runtime("plugin host stopped".to_string())
}

impl PluginHost for QuickJsHost {
    fn call(&self, call: HostCall) -> HostFuture {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Message { call, reply }).is_err() {
            return Box::pin(async { Err(stopped()) });
        }
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err(stopped())) })
    }

    fn dispose(&self) {
        self.shutdown.send_replace(true);
    }
}

impl Drop for QuickJsHost {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}

/// 启动宿主：建运行时、注入桥接、执行门面与 `main` 源码，再 spawn actor。
pub(super) async fn start(
    handle: tokio::runtime::Handle,
    plugin: PluginScript,
    settings_json: String,
    bridge: Arc<dyn PluginBridge>,
    host: HostContext,
    task_port: Option<Arc<dyn PluginTaskPort>>,
    events: Arc<dyn HostEvents>,
) -> Result<Arc<dyn PluginHost>, PluginError> {
    let rt = AsyncRuntime::new().map_err(|e| PluginError::Runtime(e.to_string()))?;
    rt.set_memory_limit(HOST_MEMORY_LIMIT).await;
    let budget = Arc::new(Budget::new());
    rt.set_interrupt_handler(Some(budget.handler())).await;
    let ctx = AsyncContext::full(&rt)
        .await
        .map_err(|e| PluginError::Runtime(e.to_string()))?;

    let identity = plugin.identity.clone();
    let info = info_json(&plugin.identity, &plugin.version, &plugin.app_version);
    let source = plugin.source;
    let interrupt_ns = budget.deadline_ns.clone();
    let wall_pause = Arc::new(WallPause::default());
    let scope: StartScope = Arc::default();
    let bridge_scope = scope.clone();
    budget.arm();
    let init = ctx.async_with(async move |ctx| -> Result<(), PluginError> {
        inject_bridge(
            &ctx,
            &bridge,
            &identity,
            None,
            None,
            host.ffmpeg_permitted,
            None,
            host.ytdlp_permitted,
            host.auth_permitted,
            host.private_network_allowed,
            host.egress.clone(),
            interrupt_ns.clone(),
            wall_pause,
        )
        .map_err(|e| PluginError::Runtime(format!("注入 flux 失败: {e}")))?;
        inject_host_bridge(
            &ctx,
            &identity,
            &events,
            task_port,
            &interrupt_ns,
            bridge_scope,
        )
        .map_err(|e| PluginError::Runtime(format!("注入宿主桥接失败: {e}")))?;
        set_json_global(&ctx, "__flux_settings_json", &settings_json)?;
        set_json_global(&ctx, "__flux_info", &info)?;
        ctx.eval::<rquickjs::Value, _>(FLUX_PRELUDE)
            .catch(&ctx)
            .map_err(|e| PluginError::Runtime(format!("flux 门面初始化失败: {e}")))?;
        ctx.eval::<rquickjs::Value, _>(HOST_PRELUDE)
            .catch(&ctx)
            .map_err(|e| PluginError::Runtime(format!("宿主门面初始化失败: {e}")))?;
        ctx.eval::<rquickjs::Value, _>(source.as_bytes())
            .catch(&ctx)
            .map_err(|e| PluginError::CompileFailed(e.to_string()))?;
        Ok(())
    });
    let started = tokio::time::timeout(HOST_WALL_TIMEOUT, init).await;
    budget.disarm();
    match started {
        Err(_) => return Err(PluginError::Timeout),
        Ok(result) => result.map_err(reclassify)?,
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown, shutdown_rx) = watch::channel(false);
    handle.spawn(actor_main(rt, ctx, budget, scope, rx, shutdown_rx));
    Ok(Arc::new(QuickJsHost { tx, shutdown }))
}

/// actor：串行处理调用，直到句柄 dispose / drop 或出现致命错误。
async fn actor_main(
    rt: AsyncRuntime,
    ctx: AsyncContext,
    budget: Arc<Budget>,
    scope: StartScope,
    mut rx: mpsc::UnboundedReceiver<Message>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let message = tokio::select! {
            biased;
            _ = shutdown.changed() => None,
            message = rx.recv() => message,
        };
        let Some(Message { call, reply }) = message else {
            break;
        };
        let outcome = tokio::select! {
            biased;
            _ = shutdown.changed() => {
                if reply.send(Err(stopped())).is_err() {
                    // 调用方已放弃等待。
                }
                break;
            }
            outcome = run_call(&ctx, &budget, &scope, call) => outcome,
        };
        let fatal = matches!(
            outcome,
            Err(PluginError::Timeout | PluginError::MemoryLimitExceeded)
        );
        if reply.send(outcome).is_err() {
            // 调用方已放弃等待；结果无人消费。
        }
        if fatal {
            break;
        }
    }
    // 先关闭接收端：排队中的调用立即以 `stopped` 结束，而不是等到运行时析构。
    rx.close();
    drop(ctx);
    drop(rt);
}

/// 把 [`HostCall`] 编码为 `(kind, json)`。
fn encode(call: &HostCall) -> Result<(&'static str, String), PluginError> {
    let (kind, payload) = match call {
        HostCall::ViewOpen {
            session,
            view,
            props,
            task_id,
            locale,
        } => (
            "viewOpen",
            serde_json::json!({
                "session": session,
                "view": view,
                "props": props,
                "taskId": task_id,
                "locale": locale,
            }),
        ),
        HostCall::ViewEvent { session, event } => (
            "viewEvent",
            serde_json::json!({ "session": session, "event": event }),
        ),
        HostCall::ViewRefresh { session } => {
            ("viewRefresh", serde_json::json!({ "session": session }))
        }
        HostCall::ViewClose { session } => ("viewClose", serde_json::json!({ "session": session })),
        HostCall::Command { id, task_ids } => (
            "command",
            serde_json::json!({ "id": id, "taskIds": task_ids }),
        ),
        HostCall::Invoke {
            name,
            args,
            page_url,
        } => (
            "invoke",
            serde_json::json!({ "name": name, "args": args, "pageUrl": page_url }),
        ),
        HostCall::Event(event) => (
            "event",
            serde_json::json!({ "hook": event.hook_fn_name(), "payload": event }),
        ),
        HostCall::Timer { id } => ("timer", serde_json::json!({ "id": id })),
        HostCall::Settings { json } => {
            let settings: serde_json::Value = serde_json::from_str(json)
                .map_err(|e| PluginError::InvalidOutput(format!("设置 JSON 非法: {e}")))?;
            ("settings", serde_json::json!({ "settings": settings }))
        }
    };
    Ok((kind, payload.to_string()))
}

/// 按调用种类把 JS 返回值映射为 [`HostReply`]。
fn decode(kind: &str, value: serde_json::Value, started: StartedTasks) -> HostReply {
    match kind {
        "viewOpen" | "viewEvent" | "viewRefresh" => HostReply::Tree(value),
        "invoke" => HostReply::Value { value, started },
        _ => HostReply::None,
    }
}

/// 执行一次调用（含预算与墙钟超时）。
async fn run_call(
    ctx: &AsyncContext,
    budget: &Budget,
    scope: &StartScope,
    call: HostCall,
) -> Result<HostReply, PluginError> {
    let (kind, payload) = encode(&call)?;
    let page_call = matches!(
        call,
        HostCall::Invoke {
            page_url: Some(_),
            ..
        }
    );
    *scope_lock(scope) = page_call.then(StartedTasks::default);
    budget.arm();
    let exec = ctx.async_with(async move |ctx| -> Result<String, PluginError> {
        let host_obj: Object = ctx
            .globals()
            .get("__fluxHost")
            .map_err(|e| PluginError::Runtime(format!("缺少 __fluxHost: {e}")))?;
        let dispatch: Function = host_obj
            .get("dispatch")
            .map_err(|e| PluginError::Runtime(format!("缺少 __fluxHost.dispatch: {e}")))?;
        let promise: Promise = dispatch
            .call((kind, payload))
            .catch(&ctx)
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        promise
            .into_future::<String>()
            .await
            .catch(&ctx)
            .map_err(|e| PluginError::Runtime(e.to_string()))
    });
    let raced = tokio::time::timeout(HOST_WALL_TIMEOUT, exec).await;
    budget.disarm();
    // 无论成败都结束本次归属：之后的 `flux.tasks.*` 不再记到这次页面调用上。
    let started = scope_lock(scope).take().unwrap_or_default();
    let raw = match raced {
        Err(_) => return Err(PluginError::Timeout),
        Ok(result) => result.map_err(reclassify)?,
    };
    let envelope: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| PluginError::InvalidOutput(format!("宿主返回值非法: {e}")))?;
    if envelope.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        let value = envelope
            .get("v")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return Ok(decode(kind, value, started));
    }
    let message = envelope
        .get("e")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown error");
    Err(reclassify(PluginError::Runtime(message.to_string())))
}

/// 把 QuickJS 的 OOM / 中断异常归一为可熔断的错误类型（其余原样）。
fn reclassify(error: PluginError) -> PluginError {
    let message = match &error {
        PluginError::Runtime(m) | PluginError::CompileFailed(m) => m.as_str(),
        _ => return error,
    };
    if message.contains("out of memory") || message.contains("Allocation failed") {
        PluginError::MemoryLimitExceeded
    } else if message.contains("interrupted") {
        PluginError::Timeout
    } else {
        error
    }
}

/// 面向 JS 的错误文本：`Runtime` 只给原因（不带本地化前缀），其余用 `Display`。
fn plain_error(error: &PluginError) -> String {
    match error {
        PluginError::Runtime(message) => message.clone(),
        other => other.to_string(),
    }
}

/// 在 `fut` 挂起期间不消耗 CPU 预算：完成后把挂起时长补回中断截止。
async fn credited<T>(deadline_ns: &AtomicU64, fut: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let out = fut.await;
    deadline_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Release);
    out
}

/// 注入宿主专用的 `__flux_*` 桥接函数。
fn inject_host_bridge(
    ctx: &Ctx<'_>,
    identity: &str,
    events: &Arc<dyn HostEvents>,
    task_port: Option<Arc<dyn PluginTaskPort>>,
    interrupt_ns: &Arc<AtomicU64>,
    scope: StartScope,
) -> Result<(), rquickjs::Error> {
    let globals = ctx.globals();

    {
        let ev = events.clone();
        let pid = identity.to_string();
        let dl = interrupt_ns.clone();
        let f = Function::new(
            ctx.clone(),
            Async(move |session: String, options_json: String| {
                let ev = ev.clone();
                let pid = pid.clone();
                let dl = dl.clone();
                async move {
                    let result = match serde_json::from_str(&options_json) {
                        Ok(options) => credited(&dl, ev.image_load(&pid, &session, options)).await,
                        Err(error) => Err(PluginError::InvalidOutput(format!(
                            "invalid image options: {error}"
                        ))),
                    };
                    let payload = match result {
                        Ok(src) => serde_json::json!({ "value": src }),
                        Err(error) => serde_json::json!({ "__fluxError": plain_error(&error) }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_image_load")?;
        globals.set("__flux_image_load", f)?;
    }
    // __flux_view_refresh(session) -> ()
    {
        let ev = events.clone();
        let f = Function::new(ctx.clone(), move |session: String| {
            ev.request_render(&session);
        })?
        .with_name("__flux_view_refresh")?;
        globals.set("__flux_view_refresh", f)?;
    }

    // __flux_toast(level, text) -> ()
    {
        let ev = events.clone();
        let pid = identity.to_string();
        let f = Function::new(ctx.clone(), move |level: String, text: String| {
            ev.toast(&pid, &level, &text);
        })?
        .with_name("__flux_toast")?;
        globals.set("__flux_toast", f)?;
    }

    // __flux_view_close(session) -> String(JSON 信封)；JS 侧遇 __fluxError 抛出
    {
        let ev = events.clone();
        let pid = identity.to_string();
        let f = Function::new(ctx.clone(), move |session: String| -> String {
            match ev.close_view(&pid, &session) {
                Ok(()) => serde_json::json!({ "value": null }),
                Err(e) => serde_json::json!({ "__fluxError": plain_error(&e) }),
            }
            .to_string()
        })?
        .with_name("__flux_view_close")?;
        globals.set("__flux_view_close", f)?;
    }

    // __flux_view_navigate(session, targetJson) -> Promise<String(JSON)>
    {
        let ev = events.clone();
        let pid = identity.to_string();
        let dl = interrupt_ns.clone();
        let f = Function::new(
            ctx.clone(),
            Async(move |session: String, target_json: String| {
                let ev = ev.clone();
                let pid = pid.clone();
                let dl = dl.clone();
                async move {
                    let payload = match serde_json::from_str::<serde_json::Value>(&target_json) {
                        Ok(target) => {
                            match credited(&dl, ev.navigate(&pid, &session, target)).await {
                                Ok(()) => serde_json::json!({ "value": null }),
                                Err(e) => serde_json::json!({ "__fluxError": plain_error(&e) }),
                            }
                        }
                        Err(e) => serde_json::json!({
                            "__fluxError": format!("navigate: unknown target ({e})")
                        }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_view_navigate")?;
        globals.set("__flux_view_navigate", f)?;
    }

    // __flux_timer_set(id, delayMs) / __flux_timer_clear(id)
    {
        let ev = events.clone();
        let pid = identity.to_string();
        let f = Function::new(ctx.clone(), move |id: u32, delay_ms: f64| {
            let millis = if delay_ms.is_finite() && delay_ms > 0.0 {
                delay_ms as u64
            } else {
                0
            };
            ev.timer(&pid, id, Duration::from_millis(millis));
        })?
        .with_name("__flux_timer_set")?;
        globals.set("__flux_timer_set", f)?;

        let ev = events.clone();
        let pid = identity.to_string();
        let f = Function::new(ctx.clone(), move |id: u32| {
            ev.timer_clear(&pid, id);
        })?
        .with_name("__flux_timer_clear")?;
        globals.set("__flux_timer_clear", f)?;
    }

    // __flux_settings_update(entriesJson) -> Promise<String(JSON)>
    {
        let ev = events.clone();
        let pid = identity.to_string();
        let dl = interrupt_ns.clone();
        let f = Function::new(
            ctx.clone(),
            Async(move |entries_json: String| {
                let ev = ev.clone();
                let pid = pid.clone();
                let dl = dl.clone();
                async move {
                    let payload =
                        match serde_json::from_str::<BTreeMap<String, String>>(&entries_json) {
                            Ok(entries) => {
                                let entries: Vec<(String, String)> = entries.into_iter().collect();
                                match credited(&dl, ev.settings_update(&pid, entries)).await {
                                    Ok(()) => serde_json::json!({ "value": null }),
                                    Err(e) => serde_json::json!({ "__fluxError": plain_error(&e) }),
                                }
                            }
                            Err(e) => serde_json::json!({
                                "__fluxError": format!("settings.update 参数非法: {e}")
                            }),
                        };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_settings_update")?;
        globals.set("__flux_settings_update", f)?;
    }
    // __flux_archive_rules(refresh) -> Promise<string(JSON)>
    {
        let ev = events.clone();
        let pid = identity.to_string();
        let dl = interrupt_ns.clone();
        let f = Function::new(
            ctx.clone(),
            Async(move |refresh: bool| {
                let ev = ev.clone();
                let pid = pid.clone();
                let dl = dl.clone();
                async move {
                    let payload = match credited(&dl, ev.archive_rules(&pid, refresh)).await {
                        Ok(value) => serde_json::json!({ "value": value }),
                        Err(error) => serde_json::json!({ "__fluxError": plain_error(&error) }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_archive_rules")?;
        globals.set("__flux_archive_rules", f)?;
    }

    // __flux_task_port(method, paramsJson) -> Promise<String(JSON)>（仅 tasks 权限）
    if let Some(port) = task_port {
        let dl = interrupt_ns.clone();
        let f = Function::new(
            ctx.clone(),
            Async(move |method: String, params_json: String| {
                let port = port.clone();
                let dl = dl.clone();
                let scope = scope.clone();
                async move {
                    let payload = match serde_json::from_str::<serde_json::Value>(&params_json) {
                        Ok(params) => {
                            let resumed = (method == TASK_RESUME)
                                .then(|| task_id_param(&params))
                                .flatten();
                            match credited(&dl, port.call(&method, params)).await {
                                Ok(value) => {
                                    record_started(&scope, &method, resumed, &value);
                                    serde_json::json!({ "value": value })
                                }
                                Err(e) => serde_json::json!({ "__fluxError": plain_error(&e) }),
                            }
                        }
                        Err(e) => serde_json::json!({
                            "__fluxError": format!("任务参数非法: {e}")
                        }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_task_port")?;
        globals.set("__flux_task_port", f)?;
    }

    Ok(())
}

/// `daemon.task.resume` 的任务 ID（daemon 同时接受 `taskId` 与 `id`）。
fn task_id_param(params: &serde_json::Value) -> Option<String> {
    params
        .get("taskId")
        .or_else(|| params.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// 页面调用期间成功的 `create` / `resume` 记入当前归属；非页面调用（`None`）不记。
fn record_started(
    scope: &StartScope,
    method: &str,
    resumed: Option<String>,
    value: &serde_json::Value,
) {
    let mut scope = scope_lock(scope);
    let Some(started) = scope.as_mut() else {
        return;
    };
    if method == TASK_CREATE {
        if let Some(task_id) = value.get("taskId").and_then(serde_json::Value::as_str) {
            started.created.push(task_id.to_owned());
        }
    } else if let Some(task_id) = resumed {
        started.resumed.push(task_id);
    }
}

/// 宿主门面：在 `flux` 上挂 `ui` / `commands` / `rpc` / `events` / `timers`（及授权后的
/// `tasks`、`settings.update`），并定义分发入口 `globalThis.__fluxHost.dispatch`。
///
/// `dispatch(kind, json)` 恒返回 `Promise<string>`，内容为 `{ok:true,v}` 或
/// `{ok:false,e}` 信封——错误文本原样回到 Rust，不经过异常栈格式化。
const HOST_PRELUDE: &str = r#"
(function () {
  const MAX_TIMERS = 64;
  const views = new Map();
  const sessions = new Map();
  const commands = new Map();
  const rpcs = new Map();
  const listeners = new Map();
  const timers = new Map();
  let nextTimerId = 1;
  let clampLogged = false;
  const flux = globalThis.flux;

  const errMsg = (e) => (e && e.message !== undefined ? String(e.message) : String(e));
  const skip = (c) => c === null || c === undefined || c === false || c === true;
  const flatten = (list) => {
    const out = [];
    const walk = (c) => {
      if (Array.isArray(c)) { c.forEach(walk); return; }
      if (skip(c)) return;
      if (c.__fragment === true) { c.c.forEach(walk); return; }
      out.push(c);
    };
    list.forEach(walk);
    return out;
  };
  const Fragment = (props) => ({ __fragment: true, c: flatten([props && props.children]) });
  const h = (kind, props, ...children) => {
    const p = props ? Object.assign({}, props) : {};
    if (typeof kind === 'function') {
      p.children = children;
      return kind(p);
    }
    delete p.children;
    return { k: String(kind), p, c: flatten(children) };
  };
  const TONES = ['info', 'success', 'warning', 'error'];

  const ui = {
    view(id, def) {
      if (typeof id !== 'string' || id === '') throw new Error('view id must be a non-empty string');
      if (!def || typeof def.render !== 'function') throw new Error('view "' + id + '" needs render()');
      views.set(id, def);
    },
    h,
    Fragment,
    toast(level, text) {
      __flux_toast(TONES.includes(level) ? level : 'info', String(text));
    },
  };

  const clampInterval = (ms) => {
    const min = sessions.size > 0 ? 1000 : 60000;
    if (ms < min) {
      if (!clampLogged) {
        clampLogged = true;
        __flux_log('info', 'setInterval interval clamped to ' + min + 'ms');
      }
      return min;
    }
    return ms;
  };
  const arm = (id, t) => {
    __flux_timer_set(id, t.interval ? clampInterval(t.ms) : Math.max(0, t.ms));
  };
  const addTimer = (fn, ms, interval) => {
    if (typeof fn !== 'function') throw new TypeError('timer callback must be a function');
    if (timers.size >= MAX_TIMERS) throw new Error('too many timers (max ' + MAX_TIMERS + ')');
    const id = nextTimerId++;
    const t = { fn, ms: Number(ms) || 0, interval };
    timers.set(id, t);
    arm(id, t);
    return id;
  };

  flux.ui = ui;
  flux.images = Object.freeze({
    load(options) {
      const { session, ...request } = options || {};
      if (typeof session !== 'string' || !sessions.has(session)) {
        return Promise.reject(new Error('images.load requires an active view session'));
      }
      return __flux_image_load(session, JSON.stringify(request)).then((raw) => {
        const r = JSON.parse(raw);
        if (r.__fluxError) throw new Error(r.__fluxError);
        return r.value;
      });
    },
  });
  flux.commands = {
    register(id, fn) {
      if (typeof id !== 'string' || typeof fn !== 'function') throw new TypeError('commands.register(id, fn)');
      commands.set(id, fn);
    },
  };
  flux.rpc = {
    handle(name, fn, opts) {
      if (typeof name !== 'string' || typeof fn !== 'function') throw new TypeError('rpc.handle(name, fn, opts?)');
      if (opts !== undefined && (opts === null || typeof opts !== 'object' || Array.isArray(opts))) {
        throw new TypeError('rpc.handle opts must be an object');
      }
      if (opts !== undefined && opts.page !== undefined && typeof opts.page !== 'boolean') {
        throw new TypeError('rpc.handle opts.page must be a boolean');
      }
      rpcs.set(name, { fn, page: !!(opts && opts.page) });
    },
  };
  flux.events = {
    on(name, fn) {
      if (typeof name !== 'string' || typeof fn !== 'function') throw new TypeError('events.on(name, fn)');
      if (!listeners.has(name)) listeners.set(name, []);
      listeners.get(name).push(fn);
    },
  };
  flux.timers = {
    setTimeout: (fn, ms) => addTimer(fn, ms, false),
    setInterval: (fn, ms) => addTimer(fn, ms, true),
    clear(id) {
      if (timers.delete(id)) __flux_timer_clear(id);
    },
  };
  Object.defineProperty(flux.settings, 'update', {
    enumerable: false,
    configurable: true,
    value: (entries) => __flux_settings_update(JSON.stringify(entries || {})).then((s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
    }),
  });
  const archiveRules = Object.freeze({
    status: () => __flux_archive_rules(false).then((s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    }),
    update: () => __flux_archive_rules(true).then((s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    }),
  });
  Object.defineProperty(flux, "archiveRules", { value: archiveRules });
  if (typeof __flux_task_port === 'function') {
    const call = (method, params) => __flux_task_port(method, JSON.stringify(params || {})).then((s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    });
    flux.tasks = {
      // 任务数超过 5000 时无参调用会被拒绝：传 `{ cursor, limit }`（cursor = 上一页最后一个 taskId）分页。
      list: (params) => call('daemon.task.list', params && typeof params === 'object' ? params : {}),
      get: (id) => call('daemon.task.get', { taskId: String(id) }),
      create: (params) => call('daemon.task.create', params),
      pause: (id) => call('daemon.task.pause', { taskId: String(id) }),
      resume: (id) => call('daemon.task.resume', { taskId: String(id) }),
      remove: (id, deleteFiles = false) =>
        call('daemon.task.delete', { taskId: String(id), deleteFiles: !!deleteFiles }),
    };
  }

  // close / navigate 在 init 内也可调用：动作随会话一起送达客户端（客户端在 open 应答前到达的
  // 动作会先缓存，登记会话后再执行）。close 幂等：会话已请求关闭后再次调用不做任何事。
  const makeCtx = (s, session, props, taskId, locale) => ({
    session,
    props: props || {},
    taskId: taskId === undefined ? null : taskId,
    locale: locale || '',
    images: Object.freeze({
      load: (options) => flux.images.load(Object.assign({}, options, { session })),
    }),
    refresh() { __flux_view_refresh(session); },
    close() {
      if (s.closing) return;
      const r = JSON.parse(__flux_view_close(session));
      if (r.__fluxError) throw new Error(r.__fluxError);
      s.closing = true;
    },
    navigate(target) {
      if (s.closing) return Promise.reject(new Error('navigate: the view has been closed; call navigate() before close()'));
      return __flux_view_navigate(session, JSON.stringify(target === undefined ? null : target)).then((raw) => {
        const r = JSON.parse(raw);
        if (r.__fluxError) throw new Error(r.__fluxError);
      });
    },
  });
  const renderSession = async (s) => {
    const tree = await s.def.render(s.state, s.ctx);
    if (tree === undefined || tree === null) throw new Error('render() returned no node');
    return tree;
  };
  const sessionOf = (id) => {
    const s = sessions.get(id);
    if (!s) throw new Error('unknown session: ' + id);
    return s;
  };

  const handlers = {
    async viewOpen(a) {
      const def = views.get(a.view);
      if (!def) throw new Error('view not registered: ' + a.view);
      const s = { def, closing: false, ctx: null, state: {} };
      s.ctx = makeCtx(s, a.session, a.props, a.taskId, a.locale);
      sessions.set(a.session, s);
      try {
        if (typeof def.init === 'function') {
          const st = await def.init(s.ctx);
          if (st !== undefined) s.state = st;
        }
        return await renderSession(s);
      } catch (e) {
        sessions.delete(a.session);
        throw e;
      }
    },
    async viewEvent(a) {
      const s = sessionOf(a.session);
      if (typeof s.def.event === 'function') {
        const next = await s.def.event(s.state, a.event, s.ctx);
        if (next !== undefined) s.state = next;
      }
      return await renderSession(s);
    },
    async viewRefresh(a) {
      return await renderSession(sessionOf(a.session));
    },
    async viewClose(a) {
      const s = sessions.get(a.session);
      if (!s) return null;
      sessions.delete(a.session);
      if (typeof s.def.dispose === 'function') await s.def.dispose(s.state);
      return null;
    },
    async command(a) {
      const fn = commands.get(a.id);
      if (!fn) throw new Error('command not registered: ' + a.id);
      await fn({ taskIds: a.taskIds || [] });
      return null;
    },
    async invoke(a) {
      const entry = rpcs.get(a.name);
      if (!entry) throw new Error('rpc not registered: ' + a.name);
      let ctx;
      if (typeof a.pageUrl === 'string') {
        if (!entry.page) throw new Error('rpc not exposed to page: ' + a.name);
        ctx = { caller: 'page', pageUrl: a.pageUrl };
      } else {
        ctx = { caller: 'desktop' };
      }
      const r = await entry.fn(a.args, ctx);
      return r === undefined ? null : r;
    },
    async event(a) {
      for (const fn of (listeners.get(a.hook) || []).slice()) {
        try {
          await fn(a.payload);
        } catch (e) {
          __flux_log('warn', 'event ' + a.hook + ' handler failed: ' + errMsg(e));
        }
      }
      for (const id of sessions.keys()) __flux_view_refresh(id);
      return null;
    },
    async timer(a) {
      const t = timers.get(a.id);
      if (!t) return null;
      if (t.interval) arm(a.id, t);
      else timers.delete(a.id);
      await t.fn();
      return null;
    },
    async settings(a) {
      for (const key of Object.keys(flux.settings)) delete flux.settings[key];
      Object.assign(flux.settings, a.settings);
      return null;
    },
  };

  globalThis.__fluxHost = {
    dispatch: async (kind, json) => {
      try {
        const handler = handlers[kind];
        if (!handler) throw new Error('unknown host call: ' + kind);
        const v = await handler(JSON.parse(json));
        return JSON.stringify({ ok: true, v: v === undefined ? null : v });
      } catch (e) {
        return JSON.stringify({ ok: false, e: errMsg(e) });
      }
    },
  };
})();
"#;
