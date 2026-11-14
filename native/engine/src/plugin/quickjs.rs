//! v1 唯一 `ScriptRuntime` 实现 —— `QuickJsScriptRuntime`。**rquickjs 类型只存在于
//! 本文件**（抽象层 [`super::runtime`] 禁止 rquickjs 类型，未来可换 deno_core）。
//!
//! 设计要点（spike `examples/plugin_spike.rs` 已在 Windows MSVC 实测坐实）：
//! - 专用 `tokio::runtime::Runtime`（`min(4,cpu)` 线程，仿 BT 先例），与主 actor
//!   （current_thread）、BT runtime 物理隔离。
//! - 两个独立信号量（resolve/hook 拆池，两平面正交）。
//! - **每次调用全新 `AsyncRuntime`+`AsyncContext`**（QuickJS 初始化亚毫秒级，无跨调用
//!   状态泄漏；全局锁问题因不共享 Runtime 而消失）。
//! - `set_memory_limit` + `set_interrupt_handler`（deadline 闭包，最佳努力）+ 外层
//!   `tokio::time::timeout`（不依赖 QuickJS 检查点，覆盖 await 挂起）。
//! - 脚本以 **classic script** 加载（`Context::eval`，非 ESM），入口函数挂 `globalThis`。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Context, Ctx, Function, Promise,
    Runtime, prelude::Async,
};
use tokio::sync::Semaphore;

use super::runtime::{
    ArchiveRequest, ArchiveResult, AuthRequest, AuthResult, ExecutionBudget, FfmpegSpec,
    HostContext, HostEvents, PluginBridge, PluginEntryKind, PluginError, PluginEvent, PluginHost,
    PluginLogLevel, PluginScript, PluginTaskPort, ResolveRequest, ResolveResult, ScriptRuntime,
    SubscriptionRequest, YtdlpSpec,
};

/// 硬顶（**CPU/中断**预算）：单次调用的 JS 字节码执行不得跨过该墙——interrupt
/// handler 据此掐死 `while(true)` 类忙循环。挂起（`await` 外部 future，如 ffmpeg
/// 子进程）不烧 CPU、不触发 interrupt，故不计入此顶（见 [`Self::run_script`]）。
pub const HARD_TIMEOUT_CEILING: Duration = Duration::from_secs(30);
/// 硬顶（**墙钟**预算）：单次调用的总墙钟时长上限（外层 `tokio::time::timeout`）。
/// 覆盖长时 `await`（ffmpeg 转码可达分钟级），远大于中断顶。
pub const HARD_WALL_CEILING: Duration = Duration::from_secs(1830);
/// resolve/auth 信号量 acquire 超时（超时 → `Overloaded`，fail-closed）。
const RESOLVE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);
/// auth 平面并发上限（M-2）：独立于 resolve 信号量，容量刻意开小——登录/二维码
/// 轮询是低频交互式调用，不该也不需要和 resolve 抢同一组 permit（否则高频轮询
/// 会把正常下载任务的 resolve 挤到 `Overloaded`）。
const AUTH_CONCURRENCY: usize = 2;
/// 归档脚本的独立并发上限，避免不可信脚本耗尽其他调用平面的运行时资源。
const ARCHIVE_CONCURRENCY: usize = 2;
/// resolve 返回 null/undefined 的哨兵。
const NULL_SENTINEL: &str = "__FLUX_NULL__";
/// storage.get 无值的哨兵。
const STORAGE_NONE: &str = "__FLUX_NONE__";
/// 同步 eval（check_compile / regex）的内存上限——挡恶意 manifest/pattern 在 actor
/// 线程上 OOM/ReDoS。
const SYNC_EVAL_MEMORY: usize = 32 * 1024 * 1024;
/// 同步 eval 的中断截止（interrupt handler 按字节码计数）——挡 while(true) 死循环 /
/// 灾难性回溯 RegExp 冻结 actor。
const SYNC_EVAL_TIMEOUT: Duration = Duration::from_secs(2);

/// v1 QuickJS 运行时实现。
pub struct QuickJsScriptRuntime {
    /// 专用 multi_thread runtime。`Option` 以便在 [`Drop`] 中 `shutdown_background`
    /// 取出——直接 drop 一个 multi_thread Runtime 若发生在异步上下文中会 panic
    /// （"Cannot drop a runtime in a context where blocking is not allowed"）。
    runtime: Option<tokio::runtime::Runtime>,
    /// runtime 的 handle（cheap clone，供 `spawn_handle` 恒可用，与 runtime 生命周期同步）。
    handle: tokio::runtime::Handle,
    /// resolve/subscription 共用信号量：容量 `max(max_concurrent, workers)`，随宿主并发上限
    /// 经 [`ScriptRuntime::set_resolve_capacity`] 同步调整。
    resolve_sema: Arc<Semaphore>,
    /// `resolve_sema` 当前的目标容量（调整时据此算增减量）。
    resolve_cap: std::sync::Mutex<usize>,
    /// 专用 runtime 的 worker 数（resolve 容量下界）。
    workers: usize,
    /// hook 平面信号量：容量 = workers；`try_acquire` 失败即丢。
    hook_sema: Arc<Semaphore>,
    /// 外部工具（ffmpeg/yt-dlp）授权插件的钩子平面：钩子可跑到 30 分钟级，不能与
    /// 短通知共用 `hook_sema`（会把后续钩子挤丢）。容量 = workers；满时有界排队。
    ext_hook_sema: Arc<Semaphore>,
    /// `ext_hook_sema` 的排队中数量（防无界堆积）。
    ext_hook_waiters: Arc<AtomicUsize>,
    /// auth 平面独立信号量（M-2）：与 resolve/subscription 物理隔离，登录轮询
    /// 不会挤占正常下载任务的 resolve permit，反之亦然。
    auth_sema: Arc<Semaphore>,
    /// 归档脚本使用独立信号量，限制不可信脚本运行时的并发数。
    archive_sema: Arc<Semaphore>,
}

impl Drop for QuickJsScriptRuntime {
    fn drop(&mut self) {
        // 非阻塞关停：不等待 in-flight 脚本（进程/引擎退出时 pending resolve 放弃即可），
        // 避免在异步上下文中 drop multi_thread Runtime 触发 panic。
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

impl QuickJsScriptRuntime {
    /// 构造专用运行时。`max_concurrent_at_startup` 用于 resolve 信号量容量下界。
    pub fn new(max_concurrent_at_startup: usize) -> std::io::Result<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let workers = std::cmp::min(4, cpus.max(1));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name("plugin-runtime")
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        let resolve_cap = max_concurrent_at_startup.max(workers).max(1);
        Ok(Self {
            runtime: Some(runtime),
            handle,
            resolve_sema: Arc::new(Semaphore::new(resolve_cap)),
            resolve_cap: std::sync::Mutex::new(resolve_cap),
            workers,
            hook_sema: Arc::new(Semaphore::new(workers.max(1))),
            ext_hook_sema: Arc::new(Semaphore::new(workers.max(1))),
            ext_hook_waiters: Arc::new(AtomicUsize::new(0)),
            auth_sema: Arc::new(Semaphore::new(AUTH_CONCURRENCY)),
            archive_sema: Arc::new(Semaphore::new(ARCHIVE_CONCURRENCY)),
        })
    }

    /// clamp budget.timeout 到硬顶。
    fn clamp(budget: ExecutionBudget) -> ExecutionBudget {
        ExecutionBudget {
            // 墙钟顶：容纳长时 await（ffmpeg）。CPU/中断顶另在 run_script 单独裁剪。
            timeout: budget.timeout.min(HARD_WALL_CEILING),
            memory_limit_bytes: budget.memory_limit_bytes,
        }
    }

    /// 同步跑一段返回 boolean 的 JS（带内存/中断预算，任何错误 → false）。
    fn eval_bool(src: &str) -> bool {
        let Ok(rt) = Runtime::new() else {
            return false;
        };
        rt.set_memory_limit(SYNC_EVAL_MEMORY);
        let deadline = Instant::now() + SYNC_EVAL_TIMEOUT;
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let Ok(ctx) = Context::full(&rt) else {
            return false;
        };
        ctx.with(|ctx| ctx.eval::<bool, _>(src).catch(&ctx).unwrap_or(false))
    }

    /// 执行一段脚本并调用入口函数，返回 JS 侧字符串结果（resolve 用；hook 返回空串）。
    ///
    /// `entry` = 入口全局函数名；`arg_json` = 传给入口的参数 JSON；`retry_task_id`
    /// = Some(task_id) 时 `flux.task.requestRetry` 生效（onError 专用）；
    /// `artifact_task_id` = Some(task_id) 时 `flux.task.recordArtifact` 生效
    /// （onDone 专用）；均为 None 时对应门面调用被 warn 忽略/拒绝。
    #[allow(clippy::too_many_arguments)]
    async fn run_script(
        &self,
        source: String,
        entry: &'static str,
        is_resolve: bool,
        arg_json: String,
        settings_json: String,
        info_json: String,
        retry_task_id: Option<String>,
        artifact_task_id: Option<String>,
        bridge: Option<Arc<dyn PluginBridge>>,
        plugin_id: String,
        budget: ExecutionBudget,
        host: HostContext,
    ) -> Result<String, PluginError> {
        let budget = Self::clamp(budget);

        let rt = AsyncRuntime::new().map_err(|e| PluginError::Runtime(e.to_string()))?;
        rt.set_memory_limit(budget.memory_limit_bytes).await;
        // 中断（CPU）预算与墙钟预算解耦：interrupt handler 掐忙循环，用相对基准 +
        // 可累加的纳秒截止；`flux.ffmpeg.run` 完成后把挂起时长补进截止（见
        // inject_bridge），使长时子进程返回后 JS 仍保有中断预算、不被立刻中断。
        let base = Instant::now();
        let interrupt_ns = Arc::new(AtomicU64::new(
            budget.timeout.min(HARD_TIMEOUT_CEILING).as_nanos() as u64,
        ));
        let dl = interrupt_ns.clone();
        rt.set_interrupt_handler(Some(Box::new(move || {
            base.elapsed().as_nanos() as u64 >= dl.load(Ordering::Acquire)
        })))
        .await;
        let ctx = AsyncContext::full(&rt)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;

        let entry_owned = entry.to_string();
        let ffmpeg_permitted = host.ffmpeg_permitted;
        let ffmpeg_root = host.ffmpeg_root.clone();
        let ytdlp_permitted = host.ytdlp_permitted;
        let auth_permitted = host.auth_permitted;
        let private_network_allowed = host.private_network_allowed;
        let egress = host.egress.clone();
        let interrupt_ns_bridge = interrupt_ns.clone();
        // 脚本墙钟里扣除的排队时间（yt-dlp 并发槽等待）。
        let wall_pause = Arc::new(super::runtime::WallPause::default());
        let wall_pause_bridge = wall_pause.clone();
        let exec = ctx.async_with(async move |ctx| -> Result<String, PluginError> {
            if entry_owned == "archive" {
                // 归档回调只拿到日志：不注入任何网络 / 存储 / 任务 / 进程桥。
                if let Some(bridge) = &bridge {
                    inject_log(&ctx, bridge, &plugin_id)
                        .map_err(|e| PluginError::Runtime(format!("注入 flux 失败: {e}")))?;
                }
            } else if let Some(bridge) = &bridge {
                inject_bridge(
                    &ctx,
                    bridge,
                    &plugin_id,
                    retry_task_id,
                    artifact_task_id,
                    ffmpeg_permitted,
                    ffmpeg_root,
                    ytdlp_permitted,
                    auth_permitted,
                    private_network_allowed,
                    egress,
                    interrupt_ns_bridge,
                    wall_pause_bridge,
                )
                .map_err(|e| PluginError::Runtime(format!("注入 flux 失败: {e}")))?;
            }
            set_json_global(&ctx, "__flux_settings_json", &settings_json)?;
            // 归档回调不下发插件 / App 元数据。
            if entry_owned != "archive" {
                set_json_global(&ctx, "__flux_info", &info_json)?;
            }
            set_json_global(&ctx, "__flux_arg", &arg_json)?;

            let prelude = if entry_owned == "archive" {
                ARCHIVE_PRELUDE
            } else {
                FLUX_PRELUDE
            };
            ctx.eval::<rquickjs::Value, _>(prelude)
                .catch(&ctx)
                .map_err(|e| PluginError::Runtime(format!("flux 门面初始化失败: {e}")))?;
            ctx.eval::<rquickjs::Value, _>(source.as_bytes())
                .catch(&ctx)
                .map_err(|e| caught_error(e, PluginError::CompileFailed))?;

            // 调入口 → Promise<String>。
            let wrapper = build_entry_wrapper(&entry_owned, is_resolve);
            let promise: Promise = ctx
                .eval(wrapper.as_bytes())
                .catch(&ctx)
                .map_err(|e| caught_error(e, PluginError::Runtime))?;
            // `.catch(&ctx)`：promise 拒绝时取真实 JS 异常消息（否则 rquickjs 只给
            // 泛化的 "exception generated by quickjs"，用户排错与 OOM 归一都失真）。
            let out: String = promise
                .into_future()
                .await
                .catch(&ctx)
                .map_err(|e| caught_error(e, PluginError::Runtime))?;
            Ok(out)
        });

        let started = Instant::now();
        let mut exec = Box::pin(exec);
        let outcome = loop {
            let deadline = started + budget.timeout + wall_pause.total();
            match tokio::time::timeout_at(deadline.into(), &mut exec).await {
                Ok(r) => break Some(r),
                Err(_) => {
                    // 到点时若期间累计了新的排队等待，截止顺延；否则才是真超时。
                    // 顺延只在仍有等待进行时发生，且每轮至少睡 20ms，避免空转。
                    if Instant::now() >= started + budget.timeout + wall_pause.total() {
                        break None;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        drop(exec);
        match outcome {
            Some(r) => {
                // 尽力回收 job 队列（不阻塞主流程）。
                rt.idle().await;
                r.map_err(reclassify_oom)
            }
            None => Err(PluginError::Timeout),
        }
    }
}

#[async_trait::async_trait]
impl ScriptRuntime for QuickJsScriptRuntime {
    fn check_compile(&self, source: &str) -> Result<(), PluginError> {
        let rt = Runtime::new().map_err(|e| PluginError::CompileFailed(e.to_string()))?;
        // 预算：classic eval 会执行顶层代码，须挡恶意插件在 actor 线程上 OOM/死循环。
        rt.set_memory_limit(SYNC_EVAL_MEMORY);
        let deadline = Instant::now() + SYNC_EVAL_TIMEOUT;
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let ctx = Context::full(&rt).map_err(|e| PluginError::CompileFailed(e.to_string()))?;
        ctx.with(|ctx| {
            ctx.eval::<rquickjs::Value, _>(source)
                .catch(&ctx)
                .map(|_| ())
                .map_err(|e| PluginError::CompileFailed(e.to_string()))
        })
    }

    fn regex_valid(&self, pattern: &str) -> bool {
        let lit = serde_json::to_string(pattern).unwrap_or_else(|_| "\"\"".to_string());
        Self::eval_bool(&format!(
            "(function(){{ try {{ new RegExp({lit}); return true; }} catch(e) {{ return false; }} }})()"
        ))
    }

    fn regex_test(&self, pattern: &str, value: &str) -> bool {
        let plit = serde_json::to_string(pattern).unwrap_or_else(|_| "\"\"".to_string());
        let vlit = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
        Self::eval_bool(&format!(
            "(function(){{ try {{ return new RegExp({plit}).test({vlit}); }} catch(e) {{ return false; }} }})()"
        ))
    }

    fn set_resolve_capacity(&self, max_concurrent: usize) {
        let target = max_concurrent.max(self.workers).max(1);
        let mut current = self
            .resolve_cap
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if target > *current {
            self.resolve_sema.add_permits(target - *current);
        } else if target < *current {
            let shrink = *current - target;
            // 空闲 permit 立即回收；仍被在途 resolve 占用的部分，由后台任务在其归还后
            // 逐个吞掉，容量最终收敛到目标值而不打断在途任务。
            let forgotten = self.resolve_sema.forget_permits(shrink);
            let debt = shrink - forgotten;
            if debt > 0 {
                let sema = self.resolve_sema.clone();
                self.handle.spawn(async move {
                    for _ in 0..debt {
                        match sema.clone().acquire_owned().await {
                            Ok(permit) => permit.forget(),
                            Err(_) => break,
                        }
                    }
                });
            }
        }
        *current = target;
    }

    async fn invoke_resolve(
        &self,
        plugin: &PluginScript,
        req: ResolveRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    ) -> Result<Option<ResolveResult>, PluginError> {
        // resolve 平面：acquire 3s 超时 → Overloaded（fail-closed）。
        let permit = tokio::time::timeout(
            RESOLVE_ACQUIRE_TIMEOUT,
            self.resolve_sema.clone().acquire_owned(),
        )
        .await
        .map_err(|_| PluginError::Overloaded)?
        .map_err(|_| PluginError::Overloaded)?;
        let _permit = permit;

        let arg_json =
            serde_json::to_string(&req).map_err(|e| PluginError::Runtime(e.to_string()))?;
        let info_json = info_json(&plugin.identity, &plugin.version, &plugin.app_version);

        let raw = self
            .run_script(
                plugin.source.clone(),
                "resolve",
                true,
                arg_json,
                settings_json,
                info_json,
                None,
                None,
                Some(bridge),
                plugin.identity.clone(),
                budget,
                host,
            )
            .await?;

        if raw == NULL_SENTINEL {
            return Ok(None);
        }
        let result: ResolveResult = serde_json::from_str(&raw)
            .map_err(|e| PluginError::InvalidOutput(format!("resolve 返回值非法: {e}")))?;
        Ok(Some(result))
    }

    async fn invoke_archive(
        &self,
        plugin: &PluginScript,
        req: ArchiveRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
    ) -> Result<Option<ArchiveResult>, PluginError> {
        let _permit = tokio::time::timeout(
            RESOLVE_ACQUIRE_TIMEOUT,
            self.archive_sema.clone().acquire_owned(),
        )
        .await
        .map_err(|_| PluginError::Overloaded)?
        .map_err(|_| PluginError::Overloaded)?;
        let arg_json =
            serde_json::to_string(&req).map_err(|e| PluginError::Runtime(e.to_string()))?;
        let result = self
            .run_script(
                plugin.source.clone(),
                "archive",
                false,
                arg_json,
                settings_json,
                // 归档回调不注入 `__flux_info`。
                String::new(),
                None,
                None,
                Some(bridge),
                plugin.identity.clone(),
                budget,
                HostContext::default(),
            )
            .await?;
        if result == NULL_SENTINEL {
            return Ok(None);
        }
        serde_json::from_str(&result)
            .map(Some)
            .map_err(|e| PluginError::InvalidOutput(format!("archive 返回值非法: {e}")))
    }

    #[allow(clippy::too_many_arguments)]
    async fn invoke_intercept(
        &self,
        plugin: &PluginScript,
        arg_json: String,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
        long_running: bool,
        artifact_task_id: Option<String>,
    ) -> Result<Option<String>, PluginError> {
        let _permit = if long_running {
            self.ext_hook_sema
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| PluginError::Overloaded)?
        } else {
            tokio::time::timeout(
                RESOLVE_ACQUIRE_TIMEOUT,
                self.resolve_sema.clone().acquire_owned(),
            )
            .await
            .map_err(|_| PluginError::Overloaded)?
            .map_err(|_| PluginError::Overloaded)?
        };

        let info_json = info_json(&plugin.identity, &plugin.version, &plugin.app_version);
        let raw = self
            .run_script(
                plugin.source.clone(),
                "intercept",
                true,
                arg_json,
                settings_json,
                info_json,
                None,
                artifact_task_id,
                Some(bridge),
                plugin.identity.clone(),
                budget,
                host,
            )
            .await?;
        if raw == NULL_SENTINEL {
            return Ok(None);
        }
        Ok(Some(raw))
    }

    async fn invoke_auth(
        &self,
        plugin: &PluginScript,
        req: AuthRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    ) -> Result<AuthResult, PluginError> {
        // auth 平面独立信号量（M-2）：不与 resolve/subscription 共用容量，登录
        // 轮询挤不掉正常下载任务的 resolve permit。
        let permit = tokio::time::timeout(
            RESOLVE_ACQUIRE_TIMEOUT,
            self.auth_sema.clone().acquire_owned(),
        )
        .await
        .map_err(|_| PluginError::Overloaded)?
        .map_err(|_| PluginError::Overloaded)?;
        let _permit = permit;

        let arg_json =
            serde_json::to_string(&req).map_err(|e| PluginError::Runtime(e.to_string()))?;
        let info_json = info_json(&plugin.identity, &plugin.version, &plugin.app_version);
        let raw = self
            .run_script(
                plugin.source.clone(),
                "authenticate",
                true,
                arg_json,
                settings_json,
                info_json,
                None,
                None,
                Some(bridge),
                plugin.identity.clone(),
                budget,
                host,
            )
            .await?;
        let result: AuthResult = serde_json::from_str(&raw)
            .map_err(|e| PluginError::InvalidOutput(format!("authenticate 返回值非法: {e}")))?;
        if !matches!(result.status.as_str(), "pending" | "success" | "error") {
            return Err(PluginError::InvalidOutput(
                "authenticate.status 必须是 pending/success/error".to_string(),
            ));
        }
        Ok(result)
    }

    async fn invoke_subscription(
        &self,
        plugin: &PluginScript,
        req: SubscriptionRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    ) -> Result<String, PluginError> {
        let permit = tokio::time::timeout(
            RESOLVE_ACQUIRE_TIMEOUT,
            self.resolve_sema.clone().acquire_owned(),
        )
        .await
        .map_err(|_| PluginError::Overloaded)?
        .map_err(|_| PluginError::Overloaded)?;
        let _permit = permit;

        let arg_json =
            serde_json::to_string(&req).map_err(|e| PluginError::Runtime(e.to_string()))?;
        let info_json = info_json(&plugin.identity, &plugin.version, &plugin.app_version);
        self.run_script(
            plugin.source.clone(),
            "subscribe",
            true,
            arg_json,
            settings_json,
            info_json,
            None,
            None,
            Some(bridge),
            plugin.identity.clone(),
            budget,
            host,
        )
        .await
    }

    async fn invoke_hook(
        &self,
        plugin: &PluginScript,
        event: PluginEvent,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    ) {
        debug_assert_eq!(plugin.entry_fn_hint, PluginEntryKind::Hook);
        // 短通知平面：try_acquire，失败即丢（带 warn 日志）。外部工具钩子（ffmpeg /
        // yt-dlp 授权，可运行数十分钟）走独立信号量并有界排队，避免长转码占满
        // 通用平面把其它钩子丢光，也避免自己被随机丢弃。
        const MAX_EXT_HOOK_WAITERS: usize = 64;
        let _permit = if host.ffmpeg_permitted || host.ytdlp_permitted {
            match self.ext_hook_sema.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    if self.ext_hook_waiters.fetch_add(1, Ordering::AcqRel) >= MAX_EXT_HOOK_WAITERS
                    {
                        self.ext_hook_waiters.fetch_sub(1, Ordering::AcqRel);
                        bridge.log(
                            &plugin.identity,
                            PluginLogLevel::Warn,
                            "外部工具钩子排队已满，本次钩子被丢弃",
                        );
                        return;
                    }
                    let got = self.ext_hook_sema.clone().acquire_owned().await;
                    self.ext_hook_waiters.fetch_sub(1, Ordering::AcqRel);
                    match got {
                        Ok(p) => p,
                        Err(_) => return,
                    }
                }
            }
        } else {
            match self.hook_sema.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    bridge.log(
                        &plugin.identity,
                        PluginLogLevel::Warn,
                        "钩子并发已满，本次钩子被丢弃",
                    );
                    return;
                }
            }
        };

        let arg_json = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(_) => return,
        };
        let entry = event.hook_fn_name();
        let retry_task_id = match &event {
            PluginEvent::Error { task_id, .. } => Some(task_id.clone()),
            _ => None,
        };
        // onDone 专用：flux.task.recordArtifact 登记衍生产物。
        let artifact_task_id = match &event {
            PluginEvent::Done { task_id, .. } => Some(task_id.clone()),
            _ => None,
        };
        let info_json = info_json(&plugin.identity, &plugin.version, &plugin.app_version);
        let identity = plugin.identity.clone();
        let log_bridge = bridge.clone();

        if let Err(e) = self
            .run_script(
                plugin.source.clone(),
                entry,
                false,
                arg_json,
                settings_json,
                info_json,
                retry_task_id,
                artifact_task_id,
                Some(bridge),
                identity.clone(),
                budget,
                host,
            )
            .await
        {
            // fire-and-forget：仅记日志。
            log_bridge.log(
                &identity,
                PluginLogLevel::Warn,
                &format!("hook {entry} 执行失败（已忽略）: {e}"),
            );
        }
    }

    async fn start_host(
        &self,
        plugin: PluginScript,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        host: HostContext,
        task_port: Option<Arc<dyn PluginTaskPort>>,
        events: Arc<dyn HostEvents>,
    ) -> Result<Arc<dyn PluginHost>, PluginError> {
        debug_assert_eq!(plugin.entry_fn_hint, PluginEntryKind::Main);
        super::quickjs_host::start(
            self.handle.clone(),
            plugin,
            settings_json,
            bridge,
            host,
            task_port,
            events,
        )
        .await
    }
    fn spawn_handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }
}

/// QuickJS 内存超限不会以独立错误类型浮出：`set_memory_limit` 命中后 JS 侧抛
/// 「out of memory」异常（rquickjs 侧偶见 allocation 失败），到达本层已是
/// [`PluginError::Runtime`]/[`PluginError::CompileFailed`] 里的字符串。归一为
/// [`PluginError::MemoryLimitExceeded`]，让 manager 的熔断器能统计连续 OOM
/// （否则反复 OOM 的插件永不自动熔断，只有 Timeout 计数）。
fn reclassify_oom(e: PluginError) -> PluginError {
    let msg = match &e {
        PluginError::Runtime(m) | PluginError::CompileFailed(m) => m.as_str(),
        _ => return e,
    };
    if msg.contains("out of memory") || msg.contains("Allocation failed") {
        PluginError::MemoryLimitExceeded
    } else {
        e
    }
}

/// 把捕获到的 JS 异常转成 [`PluginError`]，并识别字符串层认不出的 OOM 哨兵：内存
/// 紧贴上限时 QuickJS 连 `InternalError("out of memory")` 都分配不出，改抛 `null`
/// （quickjs.c `JS_ThrowError2`：「out of memory: throw JS_NULL to avoid recursing」），
/// 到字符串层只剩 `Null"null"`。命中与否取决于失败那次分配离上限多近（随分配器与
/// 堆布局变化），所以必须在仍持有异常值时判定。插件主动 `throw null` 同样会被计作
/// OOM：QuickJS 自己把 `null` 用作 OOM 哨兵，值层面无从区分。
fn caught_error(e: CaughtError<'_>, wrap: fn(String) -> PluginError) -> PluginError {
    match e {
        CaughtError::Value(v) if v.is_null() => PluginError::MemoryLimitExceeded,
        e => wrap(e.to_string()),
    }
}

/// 构建 `flux.info` 的 JSON。
pub(super) fn info_json(identity: &str, version: &str, app_version: &str) -> String {
    serde_json::json!({
        "identity": identity,
        "version": version,
        "appVersion": app_version,
    })
    .to_string()
}

/// 把 JSON 字符串 parse 为 JS 值并挂到全局。
pub(super) fn set_json_global(ctx: &Ctx<'_>, name: &str, json: &str) -> Result<(), PluginError> {
    let v = ctx
        .json_parse(json.as_bytes().to_vec())
        .catch(ctx)
        .map_err(|e| PluginError::Runtime(format!("解析 {name} 失败: {e}")))?;
    ctx.globals()
        .set(name, v)
        .map_err(|e| PluginError::Runtime(format!("设置 {name} 失败: {e}")))
}

/// 构建调用入口函数并 stringify 结果的 wrapper。
fn build_entry_wrapper(entry: &str, is_resolve: bool) -> String {
    let entry_lit = serde_json::to_string(entry).unwrap_or_else(|_| "\"resolve\"".to_string());
    if entry == "archive" || is_resolve {
        format!(
            "(async () => {{ const __fn = globalThis[{entry_lit}]; \
             if (typeof __fn !== 'function') throw new Error('入口 '+{entry_lit}+' 未定义'); \
             const __r = await __fn(__flux_arg); \
             return (__r === null || __r === undefined) ? '{NULL_SENTINEL}' : JSON.stringify(__r); }})()"
        )
    } else {
        format!(
            "(async () => {{ const __fn = globalThis[{entry_lit}]; \
             if (typeof __fn === 'function') {{ await __fn(__flux_arg); }} \
             return ''; }})()"
        )
    }
}

/// 注入同步 `__flux_log(level, msg)`：写插件日志，是归档回调唯一可用的宿主函数。
pub(super) fn inject_log(
    ctx: &Ctx<'_>,
    bridge: &Arc<dyn PluginBridge>,
    plugin_id: &str,
) -> Result<(), rquickjs::Error> {
    let b = bridge.clone();
    let pid = plugin_id.to_string();
    let f = Function::new(ctx.clone(), move |level: String, msg: String| {
        let lvl = match level.as_str() {
            "warn" => PluginLogLevel::Warn,
            "error" => PluginLogLevel::Error,
            _ => PluginLogLevel::Info,
        };
        b.log(&pid, lvl, &msg);
    })?
    .with_name("__flux_log")?;
    ctx.globals().set("__flux_log", f)
}

/// 注入低层 `__flux_*` 桥接函数（异步 fetch/storage、同步 log/requestRetry；
/// 授权时另注入异步 `__flux_ffmpeg_*`）。`interrupt_ns` 供 ffmpeg 调用把子进程
/// 挂起时长补进中断预算。
#[allow(clippy::too_many_arguments)]
pub(super) fn inject_bridge(
    ctx: &Ctx<'_>,
    bridge: &Arc<dyn PluginBridge>,
    plugin_id: &str,
    retry_task_id: Option<String>,
    artifact_task_id: Option<String>,
    ffmpeg_permitted: bool,
    ffmpeg_root: Option<PathBuf>,
    ytdlp_permitted: bool,
    auth_permitted: bool,
    private_network_allowed: bool,
    egress: Option<Arc<super::call_egress::CallEgress>>,
    interrupt_ns: Arc<AtomicU64>,
    wall_pause: Arc<super::runtime::WallPause>,
) -> Result<(), rquickjs::Error> {
    let globals = ctx.globals();

    // __flux_fetch(optsJson) -> Promise<String(JSON)>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let fetch_egress = egress.clone();
        let dl = interrupt_ns.clone();
        let f = Function::new(
            ctx.clone(),
            Async(move |opts: String| {
                let b = b.clone();
                let pid = pid.clone();
                let dl = dl.clone();
                let fetch_egress = fetch_egress.clone();
                async move {
                    let req: super::runtime::BridgeHttpRequest =
                        serde_json::from_str(&opts).unwrap_or_default();
                    let mut req = req;
                    req.auth_allowed = auth_permitted;
                    req.private_network_allowed = private_network_allowed;
                    req.egress = fetch_egress;
                    // 网络等待不烧 CPU：挂起时长补回中断预算（常驻宿主的 2 s CPU 预算依赖它）。
                    let started = Instant::now();
                    let outcome = b.http_request(&pid, req).await;
                    dl.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Release);
                    let payload = match outcome {
                        Ok(resp) => serde_json::json!({
                            "value": {
                                "status": resp.status,
                                "headers": resp.headers,
                                "body": resp.body,
                                "truncated": resp.truncated,
                            }
                        }),
                        Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_fetch")?;
        globals.set("__flux_fetch", f)?;
    }

    // 通用认证 API 仅在 manifest 声明 auth 权限时注入。
    if auth_permitted {
        // __flux_auth_get(authRef) -> Promise<String(JSON)>
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |auth_ref: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let payload = match b.auth_get(&pid, &auth_ref).await {
                        Ok(profile) => serde_json::json!({ "value": profile }),
                        Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_auth_get")?;
        globals.set("__flux_auth_get", f)?;

        // __flux_auth_save(profileJson) -> Promise<String(JSON)>
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |profile_json: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let payload =
                        match serde_json::from_str::<crate::auth::AuthProfile>(&profile_json) {
                            Ok(profile) => match b.auth_save(&pid, profile).await {
                                Ok(auth_ref) => serde_json::json!({ "value": auth_ref }),
                                Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                            },
                            Err(e) => serde_json::json!({
                                "__fluxError": format!("认证档案非法: {e}")
                            }),
                        };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_auth_save")?;
        globals.set("__flux_auth_save", f)?;

        // __flux_auth_remove(authRef) -> Promise<String(JSON)>
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |auth_ref: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let payload = match b.auth_remove(&pid, &auth_ref).await {
                        Ok(()) => serde_json::json!({ "value": true }),
                        Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_auth_remove")?;
        globals.set("__flux_auth_remove", f)?;
    }

    // __flux_storage_get(key) -> Promise<String>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |key: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let v = b
                        .storage_get(&pid, &key)
                        .await
                        .unwrap_or_else(|| STORAGE_NONE.to_string());
                    Ok::<String, rquickjs::Error>(v)
                }
            }),
        )?
        .with_name("__flux_storage_get")?;
        globals.set("__flux_storage_get", f)?;
    }

    // __flux_storage_set(key, value) -> Promise<String("" 或错误消息)>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |key: String, value: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let msg = match b.storage_set(&pid, &key, value).await {
                        Ok(()) => String::new(),
                        Err(e) => e.to_string(),
                    };
                    Ok::<String, rquickjs::Error>(msg)
                }
            }),
        )?
        .with_name("__flux_storage_set")?;
        globals.set("__flux_storage_set", f)?;
    }

    // __flux_fs_write(name, content) -> Promise<String("" 或错误消息)>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |name: String, content: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let msg = match b.fs_write(&pid, &name, content).await {
                        Ok(()) => String::new(),
                        Err(e) => e.to_string(),
                    };
                    Ok::<String, rquickjs::Error>(msg)
                }
            }),
        )?
        .with_name("__flux_fs_write")?;
        globals.set("__flux_fs_write", f)?;
    }

    // __flux_fs_read(name) -> Promise<String>（不存在返回 STORAGE_NONE 哨兵）
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |name: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let v = b
                        .fs_read(&pid, &name)
                        .await
                        .unwrap_or_else(|| STORAGE_NONE.to_string());
                    Ok::<String, rquickjs::Error>(v)
                }
            }),
        )?
        .with_name("__flux_fs_read")?;
        globals.set("__flux_fs_read", f)?;
    }

    // __flux_fs_remove(name) -> Promise<String("" 或错误消息)>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |name: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let msg = match b.fs_remove(&pid, &name).await {
                        Ok(()) => String::new(),
                        Err(e) => e.to_string(),
                    };
                    Ok::<String, rquickjs::Error>(msg)
                }
            }),
        )?
        .with_name("__flux_fs_remove")?;
        globals.set("__flux_fs_remove", f)?;
    }

    // __flux_fs_list() -> Promise<String(JSON array)>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move || {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let names = b.fs_list(&pid).await;
                    Ok::<String, rquickjs::Error>(
                        serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string()),
                    )
                }
            }),
        )?
        .with_name("__flux_fs_list")?;
        globals.set("__flux_fs_list", f)?;
    }

    inject_log(ctx, bridge, plugin_id)?;

    // __flux_request_retry(delayStr) -> ()（同步；仅 onError 生效）
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(ctx.clone(), move |delay: String| {
            let delay_ms = delay.parse::<u64>().unwrap_or(0);
            match &retry_task_id {
                Some(tid) => b.request_retry(&pid, tid, delay_ms),
                None => b.log(
                    &pid,
                    PluginLogLevel::Warn,
                    "flux.task.requestRetry 仅 onError 钩子可用，已忽略",
                ),
            }
        })?
        .with_name("__flux_request_retry")?;
        globals.set("__flux_request_retry", f)?;
    }

    // __flux_record_artifact(name) -> Promise<String("" 或错误消息)>（仅 onDone 生效）
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |name: String| {
                let b = b.clone();
                let pid = pid.clone();
                let tid = artifact_task_id.clone();
                async move {
                    let msg = match &tid {
                        Some(tid) => match b.record_artifact(&pid, tid, &name).await {
                            Ok(()) => String::new(),
                            Err(e) => e.to_string(),
                        },
                        None => "flux.task.recordArtifact 仅 onDone 钩子可用".to_string(),
                    };
                    Ok::<String, rquickjs::Error>(msg)
                }
            }),
        )?
        .with_name("__flux_record_artifact")?;
        globals.set("__flux_record_artifact", f)?;
    }

    // __flux_ffmpeg_*：仅在 manifest 授予 "ffmpeg" 权限时注入（否则 flux.ffmpeg
    // 门面不存在）。二者均返回 Promise<String(JSON)>：{value|__fluxError}。
    if ffmpeg_permitted {
        // __flux_ffmpeg_available() -> Promise<String(JSON)>
        {
            let b = bridge.clone();
            let f = Function::new(
                ctx.clone(),
                Async(move || {
                    let b = b.clone();
                    async move {
                        let payload = match b.ffmpeg_available().await {
                            Some(a) => serde_json::json!({ "value": {
                                "available": a.available,
                                "version": a.version,
                                "source": a.source,
                            }}),
                            None => serde_json::json!({ "value": {
                                "available": false, "version": "", "source": "none",
                            }}),
                        };
                        Ok::<String, rquickjs::Error>(payload.to_string())
                    }
                }),
            )?
            .with_name("__flux_ffmpeg_available")?;
            globals.set("__flux_ffmpeg_available", f)?;
        }

        // __flux_ffmpeg_run(optsJson) -> Promise<String(JSON)>
        {
            let b = bridge.clone();
            let pid = plugin_id.to_string();
            let root = ffmpeg_root.clone();
            let dl = interrupt_ns.clone();
            let f = Function::new(
                ctx.clone(),
                Async(move |opts: String| {
                    let b = b.clone();
                    let pid = pid.clone();
                    let root = root.clone();
                    let dl = dl.clone();
                    async move {
                        let Some(root) = root else {
                            return Ok::<String, rquickjs::Error>(
                                serde_json::json!({ "__fluxError":
                                    "flux.ffmpeg.run 仅在有产物文件的钩子（如 onDone）中可用" })
                                .to_string(),
                            );
                        };
                        let spec: FfmpegSpec = serde_json::from_str(&opts).unwrap_or_default();
                        // 挂起时长补进中断预算：长时子进程返回后 JS 仍保有 CPU 预算。
                        let started = Instant::now();
                        let out = b.run_ffmpeg(&pid, root, spec).await;
                        dl.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Release);
                        let payload = match out {
                            Ok(o) => serde_json::json!({ "value": {
                                "code": o.code,
                                "stdout": o.stdout,
                                "stderr": o.stderr,
                                "timedOut": o.timed_out,
                                "truncatedStdout": o.truncated_stdout,
                                "truncatedStderr": o.truncated_stderr,
                            }}),
                            Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                        };
                        Ok::<String, rquickjs::Error>(payload.to_string())
                    }
                }),
            )?
            .with_name("__flux_ffmpeg_run")?;
            globals.set("__flux_ffmpeg_run", f)?;
        }

        // __flux_ffprobe_run(optsJson) -> Promise<String(JSON)>（同 ffmpeg 权限门/牢笼）
        {
            let b = bridge.clone();
            let pid = plugin_id.to_string();
            let root = ffmpeg_root.clone();
            let dl = interrupt_ns.clone();
            let f = Function::new(
                ctx.clone(),
                Async(move |opts: String| {
                    let b = b.clone();
                    let pid = pid.clone();
                    let root = root.clone();
                    let dl = dl.clone();
                    async move {
                        let Some(root) = root else {
                            return Ok::<String, rquickjs::Error>(
                                serde_json::json!({ "__fluxError":
                                    "flux.ffprobe.run 仅在有产物文件的钩子（如 onDone）中可用" })
                                .to_string(),
                            );
                        };
                        let spec: FfmpegSpec = serde_json::from_str(&opts).unwrap_or_default();
                        let started = Instant::now();
                        let out = b.run_ffprobe(&pid, root, spec).await;
                        dl.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Release);
                        let payload = match out {
                            Ok(o) => serde_json::json!({ "value": {
                                "code": o.code,
                                "stdout": o.stdout,
                                "stderr": o.stderr,
                                "timedOut": o.timed_out,
                                "truncatedStdout": o.truncated_stdout,
                                "truncatedStderr": o.truncated_stderr,
                            }}),
                            Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                        };
                        Ok::<String, rquickjs::Error>(payload.to_string())
                    }
                }),
            )?
            .with_name("__flux_ffprobe_run")?;
            globals.set("__flux_ffprobe_run", f)?;
        }
    }

    // __flux_ytdlp_*：仅在 manifest 授予 "ytdlp" 权限时注入（否则 flux.ytdlp 门面
    // 不存在）。牢笼由 bridge 自持（每插件 scratch 目录），故 run 无 root 前置检查。
    if ytdlp_permitted {
        // __flux_ytdlp_available() -> Promise<String(JSON)>
        {
            let b = bridge.clone();
            let f = Function::new(
                ctx.clone(),
                Async(move || {
                    let b = b.clone();
                    async move {
                        let payload = match b.ytdlp_available().await {
                            Some(a) => serde_json::json!({ "value": {
                                "available": a.available,
                                "version": a.version,
                                "source": a.source,
                            }}),
                            None => serde_json::json!({ "value": {
                                "available": false, "version": "", "source": "none",
                            }}),
                        };
                        Ok::<String, rquickjs::Error>(payload.to_string())
                    }
                }),
            )?
            .with_name("__flux_ytdlp_available")?;
            globals.set("__flux_ytdlp_available", f)?;
        }

        // __flux_ytdlp_run(optsJson) -> Promise<String(JSON)>
        {
            let b = bridge.clone();
            let pid = plugin_id.to_string();
            let dl = interrupt_ns.clone();
            let wp = wall_pause.clone();
            let ytdlp_egress = egress.clone();
            let f = Function::new(
                ctx.clone(),
                Async(move |opts: String| {
                    let b = b.clone();
                    let pid = pid.clone();
                    let dl = dl.clone();
                    let wp = wp.clone();
                    let ytdlp_egress = ytdlp_egress.clone();
                    async move {
                        let mut spec: YtdlpSpec = serde_json::from_str(&opts).unwrap_or_default();
                        spec.wall_pause = Some(wp);
                        spec.egress = ytdlp_egress;
                        // 挂起时长补进中断预算：长时子进程返回后 JS 仍保有 CPU 预算。
                        let started = Instant::now();
                        let out = b.run_ytdlp(&pid, spec).await;
                        dl.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Release);
                        let payload = match out {
                            Ok(o) => serde_json::json!({ "value": {
                                "code": o.code,
                                "stdout": o.stdout,
                                "stderr": o.stderr,
                                "timedOut": o.timed_out,
                                "truncatedStdout": o.truncated_stdout,
                                "truncatedStderr": o.truncated_stderr,
                            }}),
                            Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                        };
                        Ok::<String, rquickjs::Error>(payload.to_string())
                    }
                }),
            )?
            .with_name("__flux_ytdlp_run")?;
            globals.set("__flux_ytdlp_run", f)?;
        }
    }

    Ok(())
}

/// flux/console 门面（引用上面注入的 `__flux_*` 与 `__flux_settings_json`/`__flux_info`）。
pub(super) const FLUX_PRELUDE: &str = r#"
(function () {
  const __args2str = (a) => a.map(x => {
    try { return typeof x === 'string' ? x : JSON.stringify(x); } catch (e) { return String(x); }
  }).join(' ');
  globalThis.flux = {
    fetch: (opts) => __flux_fetch(JSON.stringify(opts || {})).then((s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    }),
    // flux.auth：仅 manifest 授予 auth 权限时注入。凭据由宿主持久化，
    // flux.fetch 会自动按 authRef 或当前插件/站点复用。
    ...(typeof __flux_auth_get === 'function' ? {
      auth: {
        get: (authRef) => __flux_auth_get(String(authRef || '')).then((s) => {
          const r = JSON.parse(s);
          if (r.__fluxError) throw new Error(r.__fluxError);
          return r.value;
        }),
        save: (profile) => __flux_auth_save(JSON.stringify(profile || {})).then((s) => {
          const r = JSON.parse(s);
          if (r.__fluxError) throw new Error(r.__fluxError);
          return r.value;
        }),
        remove: (authRef) => __flux_auth_remove(String(authRef || '')).then((s) => {
          const r = JSON.parse(s);
          if (r.__fluxError) throw new Error(r.__fluxError);
          return r.value;
        }),
      },
    } : {}),
    storage: {
      get: (key) => __flux_storage_get(String(key)).then((s) => s === '__FLUX_NONE__' ? null : s),
      set: (key, value) => __flux_storage_set(String(key), String(value)).then((s) => {
        if (s) throw new Error(s);
      }),
    },
    // flux.fs：牢笼内（每插件工作区，与 flux.ytdlp 的 cwd 同根）通用临时文件读写，
    // 供为受管工具物化输入文件（cookie/config/字幕…）。始终可用，无需权限。
    fs: {
      writeFile: (name, content) =>
        __flux_fs_write(String(name), String(content)).then((s) => {
          if (s) throw new Error(s);
        }),
      readFile: (name) =>
        __flux_fs_read(String(name)).then((s) => (s === '__FLUX_NONE__' ? null : s)),
      remove: (name) => __flux_fs_remove(String(name)).then((s) => {
        if (s) throw new Error(s);
      }),
      list: () => __flux_fs_list().then((s) => JSON.parse(s)),
    },
    settings: __flux_settings_json,
    info: __flux_info,
    logger: {
      info: (...a) => __flux_log('info', __args2str(a)),
      warn: (...a) => __flux_log('warn', __args2str(a)),
      error: (...a) => __flux_log('error', __args2str(a)),
    },
    task: {
      requestRetry: (opts) => __flux_request_retry(String((opts && opts.delayMs) || 0)),
      recordArtifact: (name) => __flux_record_artifact(String(name)).then((s) => {
        if (s) throw new Error(s);
      }),
    },
  };
  // flux.ffmpeg：仅当宿主注入了 __flux_ffmpeg_run（即 manifest 授予 ffmpeg 权限）
  // 时可用；否则 flux.ffmpeg 为 undefined，插件应先 `if (flux.ffmpeg)` 判定。
  if (typeof __flux_ffmpeg_run === 'function') {
    const __ffunwrap = (s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    };
    globalThis.flux.ffmpeg = {
      available: () => __flux_ffmpeg_available().then(__ffunwrap),
      run: (opts) => __flux_ffmpeg_run(JSON.stringify(opts || {})).then(__ffunwrap),
    };
  }
  // flux.ffprobe：与 flux.ffmpeg 同由 ffmpeg 权限门 + 同牢笼门控（onDone 才有牢笼）。
  if (typeof __flux_ffprobe_run === 'function') {
    const __fpunwrap = (s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    };
    globalThis.flux.ffprobe = {
      run: (opts) => __flux_ffprobe_run(JSON.stringify(opts || {})).then(__fpunwrap),
    };
  }
  // flux.ytdlp：仅当宿主注入了 __flux_ytdlp_run（即 manifest 授予 ytdlp 权限）
  // 时可用；否则 flux.ytdlp 为 undefined，插件应先 `if (flux.ytdlp)` 判定。
  if (typeof __flux_ytdlp_run === 'function') {
    const __ytunwrap = (s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    };
    globalThis.flux.ytdlp = {
      available: () => __flux_ytdlp_available().then(__ytunwrap),
      run: (opts) => __flux_ytdlp_run(JSON.stringify(opts || {})).then(__ytunwrap),
    };
  }
  globalThis.console = {
    log: (...a) => __flux_log('info', __args2str(a)),
    info: (...a) => __flux_log('info', __args2str(a)),
    warn: (...a) => __flux_log('warn', __args2str(a)),
    error: (...a) => __flux_log('error', __args2str(a)),
    debug: (...a) => __flux_log('info', __args2str(a)),
  };
})();
"#;

/// 归档回调门面：只有类型化设置与写插件日志的 `flux.logger` / `console`（`__flux_log`
/// 由 [`inject_log`] 注入），没有网络、存储、文件、任务或进程能力。
pub(super) const ARCHIVE_PRELUDE: &str = r#"
(function () {
  const __args2str = (a) => a.map(x => {
    try { return typeof x === 'string' ? x : JSON.stringify(x); } catch (e) { return String(x); }
  }).join(' ');
  const __log = (level) => (...a) => __flux_log(level, __args2str(a));
  globalThis.flux = {
    settings: __flux_settings_json,
    logger: { info: __log('info'), warn: __log('warn'), error: __log('error') },
  };
  globalThis.console = {
    log: __log('info'),
    info: __log('info'),
    warn: __log('warn'),
    error: __log('error'),
    debug: __log('info'),
  };
})();
"#;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::QuickJsScriptRuntime;
    use crate::plugin::runtime::{
        ArchiveRequest, AuthRequest, BridgeHttpRequest, BridgeHttpResponse, ExecutionBudget,
        HostContext, PluginBridge, PluginEntryKind, PluginError, PluginLogLevel, PluginScript,
        ResolveRequest, ResolveResult, ScriptRuntime, SubscriptionRequest,
    };

    /// 测试桩：全空实现（OOM 测试不触网/不落盘）。
    struct NullBridge;

    #[async_trait::async_trait]
    impl PluginBridge for NullBridge {
        async fn http_request(
            &self,
            _plugin_id: &str,
            _req: BridgeHttpRequest,
        ) -> Result<BridgeHttpResponse, PluginError> {
            Err(PluginError::Runtime("no network in test".to_string()))
        }
        async fn storage_get(&self, _plugin_id: &str, _key: &str) -> Option<String> {
            None
        }
        async fn storage_set(
            &self,
            _plugin_id: &str,
            _key: &str,
            _value: String,
        ) -> Result<(), PluginError> {
            Ok(())
        }
        fn log(&self, _plugin_id: &str, _level: PluginLogLevel, _message: &str) {}
        fn request_retry(&self, _plugin_id: &str, _task_id: &str, _delay_ms: u64) {}
    }

    /// 回归：QuickJS 内存超限必须归一为 `MemoryLimitExceeded`（而非 `Runtime`），
    /// 否则连续 OOM 的插件不计入熔断计数、永不自动禁用。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oom_is_reclassified_as_memory_limit_exceeded() {
        let rt = QuickJsScriptRuntime::new(1).expect("runtime");
        let script = PluginScript {
            identity: "test@oom".to_string(),
            source: "globalThis.resolve = async () => { const a = []; \
                     for (;;) { a.push(new Array(65536).fill(1)); } };"
                .to_string(),
            entry_fn_hint: PluginEntryKind::Resolve,
            version: "1.0.0".to_string(),
            app_version: "0.0.0".to_string(),
        };
        let req = ResolveRequest {
            task_id: "t1".to_string(),
            url: "http://example.com/".to_string(),
            auth_ref: String::new(),
            cookies: String::new(),
            referrer: String::new(),
            user_agent: String::new(),
            extra_headers: Default::default(),
            resolver_item: String::new(),
        };
        let budget = ExecutionBudget {
            timeout: Duration::from_secs(10),
            memory_limit_bytes: 16 * 1024 * 1024,
        };
        let err = rt
            .invoke_resolve(
                &script,
                req,
                "{}".to_string(),
                Arc::new(NullBridge),
                budget,
                HostContext::default(),
            )
            .await
            .expect_err("unbounded allocation must fail");
        assert!(
            matches!(err, PluginError::MemoryLimitExceeded),
            "expected MemoryLimitExceeded, got: {err:?}"
        );
    }

    /// 回归：内存紧贴上限时 QuickJS 改抛 `null`（OOM 错误对象本身分配失败），
    /// 它也必须归为 `MemoryLimitExceeded`。分配器 / 堆布局决定上一条用例走哪条
    /// 路径（Linux 共享进程跑 `cargo test` 时会命中），此处直接确定性覆盖该哨兵。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn null_exception_sentinel_counts_as_memory_limit_exceeded() {
        let rt = QuickJsScriptRuntime::new(1).expect("runtime");
        let script = PluginScript {
            identity: "test@oom-null".to_string(),
            source: "globalThis.resolve = async () => { throw null; };".to_string(),
            entry_fn_hint: PluginEntryKind::Resolve,
            version: "1.0.0".to_string(),
            app_version: "0.0.0".to_string(),
        };
        let req = ResolveRequest {
            task_id: "t1".to_string(),
            url: "http://example.com/".to_string(),
            auth_ref: String::new(),
            cookies: String::new(),
            referrer: String::new(),
            user_agent: String::new(),
            extra_headers: Default::default(),
            resolver_item: String::new(),
        };
        let err = rt
            .invoke_resolve(
                &script,
                req,
                "{}".to_string(),
                Arc::new(NullBridge),
                ExecutionBudget {
                    timeout: Duration::from_secs(10),
                    memory_limit_bytes: 16 * 1024 * 1024,
                },
                HostContext::default(),
            )
            .await
            .expect_err("null rejection must fail");
        assert!(
            matches!(err, PluginError::MemoryLimitExceeded),
            "expected MemoryLimitExceeded, got: {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscription_entry_returns_normalized_json() {
        let rt = QuickJsScriptRuntime::new(1).expect("runtime");
        let script = PluginScript {
            identity: "test@subscription".to_string(),
            source: r#"
                globalThis.subscribe = async (ctx) => ({
                  title: "Demo feed",
                  link: ctx.url,
                  items: [{
                    guid: ctx.providerConfig,
                    title: "Episode 1",
                    link: ctx.url,
                    enclosureUrl: "https://cdn.example/episode-1.mp4",
                    enclosureLength: 42,
                    pubDate: 1710000000
                  }]
                });
            "#
            .to_string(),
            entry_fn_hint: PluginEntryKind::Subscription,
            version: "1.0.0".to_string(),
            app_version: "0.0.0".to_string(),
        };
        let raw = rt
            .invoke_subscription(
                &script,
                SubscriptionRequest {
                    provider_id: "demo-json".to_string(),
                    source_id: "source-1".to_string(),
                    url: "https://example.com/feed".to_string(),
                    provider_config: "item-1".to_string(),
                    cookies: String::new(),
                    user_agent: "FluxDown/test".to_string(),
                },
                "{}".to_string(),
                Arc::new(NullBridge),
                ExecutionBudget {
                    timeout: Duration::from_secs(5),
                    memory_limit_bytes: 16 * 1024 * 1024,
                },
                HostContext::default(),
            )
            .await
            .expect("subscription entry");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(value["title"], "Demo feed");
        assert_eq!(value["items"][0]["guid"], "item-1");
        assert_eq!(value["items"][0]["enclosureLength"], 42);
    }

    /// 用给定 host 上下文跑一段 resolver，返回其 `url` 字段（测试探针）。
    async fn probe_resolve(source: &str, host: HostContext) -> ResolveResult {
        let rt = QuickJsScriptRuntime::new(1).expect("runtime");
        let script = PluginScript {
            identity: "test@ff".to_string(),
            source: source.to_string(),
            entry_fn_hint: PluginEntryKind::Resolve,
            version: "1.0.0".to_string(),
            app_version: "0.0.0".to_string(),
        };
        let req = ResolveRequest {
            task_id: "t".to_string(),
            url: "http://x/".to_string(),
            auth_ref: String::new(),
            cookies: String::new(),
            referrer: String::new(),
            user_agent: String::new(),
            extra_headers: Default::default(),
            resolver_item: String::new(),
        };
        let budget = ExecutionBudget {
            timeout: Duration::from_secs(10),
            memory_limit_bytes: 32 * 1024 * 1024,
        };
        rt.invoke_resolve(
            &script,
            req,
            "{}".to_string(),
            Arc::new(NullBridge),
            budget,
            host,
        )
        .await
        .expect("resolve ok")
        .expect("non-null")
    }

    /// 无 ffmpeg 权限 → `flux.ffmpeg` 门面不注入（undefined）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ffmpeg_facade_absent_without_permission() {
        let r = probe_resolve(
            "globalThis.resolve = async () => ({ url: String(typeof flux.ffmpeg) });",
            HostContext::default(),
        )
        .await;
        assert_eq!(r.url, "undefined");
    }

    /// 授予权限 → 门面注入（object）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ffmpeg_facade_present_with_permission() {
        let r = probe_resolve(
            "globalThis.resolve = async () => ({ url: String(typeof flux.ffmpeg) });",
            HostContext {
                ffmpeg_permitted: true,
                ffmpeg_root: None,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(r.url, "object");
    }

    /// 无 auth 权限时，插件不能接触宿主认证存储。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_facade_absent_without_permission() {
        let r = probe_resolve(
            "globalThis.resolve = async () => ({ url: String(typeof flux.auth) });",
            HostContext::default(),
        )
        .await;
        assert_eq!(r.url, "undefined");
    }

    /// 授予 auth 权限后，插件可使用 get/save/remove 认证生命周期 API。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_facade_present_with_permission() {
        let r = probe_resolve(
            "globalThis.resolve = async () => ({ url: (typeof flux.auth) + ':' + \
             (typeof flux.auth.get) + ':' + (typeof flux.auth.save) + ':' + \
             (typeof flux.auth.remove) });",
            HostContext {
                auth_permitted: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(r.url, "object:function:function:function");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_entry_returns_qr_challenge() {
        let rt = QuickJsScriptRuntime::new(1).expect("runtime");
        let script = PluginScript {
            identity: "test@auth".to_string(),
            source: "globalThis.authenticate = async (ctx) => ({ \
                     status: 'pending', sessionId: ctx.sessionId, challenge: 'qr-data', \
                     challengeType: 'text' });"
                .to_string(),
            entry_fn_hint: PluginEntryKind::Auth,
            version: "1.0.0".to_string(),
            app_version: "0.0.0".to_string(),
        };
        let result = rt
            .invoke_auth(
                &script,
                AuthRequest {
                    action: "begin".to_string(),
                    site: "example.com".to_string(),
                    auth_ref: "test@auth::example.com".to_string(),
                    session_id: "s1".to_string(),
                    input: String::new(),
                },
                "{}".to_string(),
                Arc::new(NullBridge),
                ExecutionBudget {
                    timeout: Duration::from_secs(10),
                    memory_limit_bytes: 32 * 1024 * 1024,
                },
                HostContext {
                    auth_permitted: true,
                    ..Default::default()
                },
            )
            .await
            .expect("auth entry should succeed");
        assert_eq!(result.status, "pending");
        assert_eq!(result.session_id, "s1");
        assert_eq!(result.challenge.as_deref(), Some("qr-data"));
    }

    /// 授权但无牢笼根（如 resolve 平面）→ run() 抛错、不触达 bridge。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ffmpeg_run_rejected_without_jail_root() {
        let r = probe_resolve(
            "globalThis.resolve = async () => { try { \
             await flux.ffmpeg.run({ args: ['-version'] }); return { url: 'nothrow' }; \
             } catch (e) { return { url: 'threw:' + e.message }; } };",
            HostContext {
                ffmpeg_permitted: true,
                ffmpeg_root: None,
                ..Default::default()
            },
        )
        .await;
        assert!(r.url.starts_with("threw:"), "expected throw, got {}", r.url);
        assert!(
            r.url.contains("onDone"),
            "error should mention onDone: {}",
            r.url
        );
    }

    /// resolve 平面无产物任务上下文 → recordArtifact 抛错、不触达 bridge。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn record_artifact_rejected_outside_done_hook() {
        let r = probe_resolve(
            "globalThis.resolve = async () => { try { \
             await flux.task.recordArtifact('a.mp4'); return { url: 'nothrow' }; \
             } catch (e) { return { url: 'threw:' + e.message }; } };",
            HostContext::default(),
        )
        .await;
        assert!(r.url.starts_with("threw:"), "expected throw, got {}", r.url);
        assert!(
            r.url.contains("onDone"),
            "error should mention onDone: {}",
            r.url
        );
    }

    /// flux.fs 始终注入（无需权限），且四个方法形状正确。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs_facade_always_present() {
        let r = probe_resolve(
            "globalThis.resolve = async () => ({ url: \
             (typeof flux.fs) + ':' + (typeof flux.fs.writeFile) + ':' + \
             (typeof flux.fs.readFile) + ':' + (typeof flux.fs.remove) + ':' + \
             (typeof flux.fs.list) });",
            HostContext::default(),
        )
        .await;
        assert_eq!(r.url, "object:function:function:function:function");
    }

    /// 回归：宿主运行期放大/缩小并发上限，resolve 信号量容量随之同步
    /// （否则放大后超出启动容量的解析任务只能 3s 后 `Overloaded`）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_capacity_follows_host_concurrency() {
        let rt = QuickJsScriptRuntime::new(1).expect("runtime");
        let base = rt.resolve_sema.available_permits();
        let raised = base + 2;
        rt.set_resolve_capacity(raised);
        assert_eq!(rt.resolve_sema.available_permits(), raised);

        // 全部 permit 在途时缩容：无空闲可回收，欠下的部分在归还后被吞掉。
        let held = rt
            .resolve_sema
            .clone()
            .acquire_many_owned(raised as u32)
            .await
            .expect("permits");
        rt.set_resolve_capacity(base);
        drop(held);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(rt.resolve_sema.available_permits(), base);
    }

    /// 测试桩：只记录日志，其余同 [`NullBridge`]。
    #[derive(Default)]
    struct LogBridge(std::sync::Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl PluginBridge for LogBridge {
        async fn http_request(
            &self,
            _plugin_id: &str,
            _req: BridgeHttpRequest,
        ) -> Result<BridgeHttpResponse, PluginError> {
            Err(PluginError::Runtime("no network in test".to_string()))
        }
        async fn storage_get(&self, _plugin_id: &str, _key: &str) -> Option<String> {
            None
        }
        async fn storage_set(
            &self,
            _plugin_id: &str,
            _key: &str,
            _value: String,
        ) -> Result<(), PluginError> {
            Ok(())
        }
        fn log(&self, plugin_id: &str, level: PluginLogLevel, message: &str) {
            let level = match level {
                PluginLogLevel::Warn => "warn",
                PluginLogLevel::Error => "error",
                PluginLogLevel::Info => "info",
            };
            self.0
                .lock()
                .unwrap()
                .push(format!("{plugin_id} {level} {message}"));
        }
        fn request_retry(&self, _plugin_id: &str, _task_id: &str, _delay_ms: u64) {}
    }

    /// 归档回调可异步、可读设置、可写日志（`console` 与 `flux.logger`），但拿不到任何
    /// 其他宿主能力与插件元数据。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn archive_is_async_and_exposes_only_settings_and_logging() {
        let rt = QuickJsScriptRuntime::new(1).expect("runtime");
        let script = PluginScript {
            identity: "test@archive".to_string(),
            source: "globalThis.archive = async ctx => { \
                     console.log('name', ctx.fileName); flux.logger.warn({ n: 1 }); \
                     const absent = ['fetch', 'storage', 'info', 'task', 'fs', 'ffmpeg', \
                     'ytdlp', 'auth', 'rpc', 'ui'].filter(k => flux[k] !== undefined); \
                     if (ctx.headerHex !== undefined) throw new Error('raw file bytes exposed'); \
                     return { folderName: absent.length + ':' + typeof __flux_fetch + ':' \
                     + flux.settings.label + ':' + ctx.fileName }; };"
                .to_string(),
            entry_fn_hint: PluginEntryKind::Archive,
            version: "1.0.0".to_string(),
            app_version: "0.0.0".to_string(),
        };
        let bridge = Arc::new(LogBridge::default());
        let result = rt
            .invoke_archive(
                &script,
                ArchiveRequest {
                    file_name: "episode.mkv".to_string(),
                    metadata: None,
                    is_group: false,
                    header_hex: Some("0123456789abcdef".to_string()),
                    ..Default::default()
                },
                "{\"label\":\"Anime\"}".to_string(),
                bridge.clone(),
                ExecutionBudget {
                    timeout: Duration::from_secs(2),
                    memory_limit_bytes: 16 * 1024 * 1024,
                },
            )
            .await
            .expect("archive invocation");
        assert_eq!(
            result.expect("archive result").folder_name,
            "0:undefined:Anime:episode.mkv"
        );
        assert_eq!(
            *bridge.0.lock().unwrap(),
            vec![
                "test@archive info name episode.mkv".to_string(),
                "test@archive warn {\"n\":1}".to_string(),
            ]
        );
    }
}
