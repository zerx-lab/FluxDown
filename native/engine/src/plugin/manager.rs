//! `PluginManager` —— 插件装载 / 启停 / resolve / notify / 安装 / 设置。
//!
//! `Arc` 共享，注入 `DownloadManager`。`RwLock<Arc<Vec<LoadedPlugin>>>` 读多写少：
//! install/uninstall/toggle 写（整表原子替换），resolve/hook match 读。**不用 arc-swap**
//! （workspace 无此依赖且约束禁新增）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::RwLock;

use crate::db::Db;
use crate::events::{EngineEvent, EventSink};
use crate::logger::log_info;

use super::manifest::{
    PERMISSION_FFMPEG, PERMISSION_YTDLP, PluginManifest, SettingField, SettingType,
};
use super::quickjs::HARD_TIMEOUT_CEILING;
use super::runtime::{
    ExecutionBudget, HostContext, PluginBridge, PluginEntryKind, PluginError, PluginEvent,
    PluginScript, ResolveRequest, ResolveResult, ScriptRuntime,
};

/// 连续超时/超内存达到该次数 → 自动熔断禁用。
const CIRCUIT_BREAKER_THRESHOLD: u32 = 3;

/// 默认 resolve 预算。
const DEFAULT_RESOLVE_BUDGET: ExecutionBudget = ExecutionBudget {
    timeout: Duration::from_secs(10),
    memory_limit_bytes: 64 * 1024 * 1024,
};
/// 默认 hook 预算。
const DEFAULT_HOOK_BUDGET: ExecutionBudget = ExecutionBudget {
    timeout: Duration::from_secs(5),
    memory_limit_bytes: 32 * 1024 * 1024,
};
/// 外部工具（ffmpeg / yt-dlp）授权插件的 hook 墙钟预算：容纳长时子进程（转码 /
/// 下载可达分钟级）。CPU/中断预算仍是 30s（见 [`super::quickjs`]，`await` 不烧
/// CPU、不计入中断顶），内存与普通 hook 一致。
const EXTERNAL_TOOL_HOOK_BUDGET: ExecutionBudget = ExecutionBudget {
    timeout: Duration::from_secs(1830),
    memory_limit_bytes: 32 * 1024 * 1024,
};

/// 禁用原因（PascalCase 序列化，全文一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    None,
    Manual,
    CircuitBreaker,
}

impl DisabledReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DisabledReason::None => "None",
            DisabledReason::Manual => "Manual",
            DisabledReason::CircuitBreaker => "CircuitBreaker",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "Manual" => DisabledReason::Manual,
            "CircuitBreaker" => DisabledReason::CircuitBreaker,
            _ => DisabledReason::None,
        }
    }
}

/// 已加载插件的运行态。
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    /// 插件目录（安装模式）或 dev 目录。
    pub dir: PathBuf,
    /// devMode：每次调用按最新源码重读（不缓存）。
    pub dev: bool,
    pub enabled: bool,
    pub disabled_reason: DisabledReason,
    /// resolver 入口绝对路径（若声明）。
    resolver_entry: Option<PathBuf>,
    /// hooks 入口绝对路径（若声明）。
    hooks_entry: Option<PathBuf>,
    /// 非 dev 模式的缓存源码（加载时读入）。
    resolver_cache: Option<String>,
    hooks_cache: Option<String>,
    /// 熔断计数（连续 Timeout/MemoryLimit）。
    timeout_streak: Arc<AtomicU32>,
}

impl LoadedPlugin {
    async fn resolver_source(&self) -> Option<String> {
        match (&self.resolver_entry, self.dev) {
            (Some(p), true) => tokio::fs::read_to_string(p).await.ok(),
            (Some(_), false) => self.resolver_cache.clone(),
            (None, _) => None,
        }
    }
    async fn hooks_source(&self) -> Option<String> {
        match (&self.hooks_entry, self.dev) {
            (Some(p), true) => tokio::fs::read_to_string(p).await.ok(),
            (Some(_), false) => self.hooks_cache.clone(),
            (None, _) => None,
        }
    }
}

/// 传给 UI 的插件视图。
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub identity: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: String,
    pub enabled: bool,
    pub dev_mode: bool,
    pub disabled_reason: String,
    pub settings: Vec<SettingField>,
    /// 当前设置值（key → value 字符串）。
    pub settings_values: Vec<(String, String)>,
    /// manifest 声明的能力权限（供 UI 展示授权，如 `["ffmpeg"]`）。
    pub permissions: Vec<String>,
}

/// 安装来源判别（供 actor 分发规则表）。
pub enum InstallSource {
    Zip(Vec<u8>),
    Dir(PathBuf),
    Dev(PathBuf),
}

/// 插件管理器。
pub struct PluginManager {
    runtime: Arc<dyn ScriptRuntime>,
    bridge: Arc<dyn PluginBridge>,
    plugins: RwLock<Arc<Vec<LoadedPlugin>>>,
    db: Db,
    root: PathBuf,
    app_version: String,
    resolve_budget: ExecutionBudget,
    hook_budget: ExecutionBudget,
    sink: Arc<dyn EventSink>,
}

impl PluginManager {
    /// 构造（不加载）；随后调用 [`Self::load_all`]。
    pub fn new(
        runtime: Arc<dyn ScriptRuntime>,
        bridge: Arc<dyn PluginBridge>,
        db: Db,
        root: PathBuf,
        app_version: String,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            runtime,
            bridge,
            plugins: RwLock::new(Arc::new(Vec::new())),
            db,
            root,
            app_version,
            resolve_budget: DEFAULT_RESOLVE_BUDGET,
            hook_budget: DEFAULT_HOOK_BUDGET,
            sink,
        }
    }

    /// 专用运行时 handle，供 off-actor worker spawn（禁止裸 tokio::spawn）。
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.spawn_handle()
    }

    /// 扫描根目录 + `plugin.dev.*` 键，解析并加载全部插件。
    pub async fn load_all(&self) {
        let mut loaded: Vec<LoadedPlugin> = Vec::new();

        // 1. 安装目录下的子目录。
        if let Ok(mut rd) = tokio::fs::read_dir(&self.root).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                if path.is_dir()
                    && let Some(p) = self.load_one(&path, false).await
                {
                    loaded.push(p);
                }
            }
        }

        // 2. dev 插件（plugin.dev.<identity> = abs path）。
        if let Ok(entries) = self.db.list_config_with_prefix("plugin.dev.").await {
            for (_k, path_str) in entries {
                let path = PathBuf::from(&path_str);
                if path.is_dir()
                    && let Some(p) = self.load_one(&path, true).await
                {
                    loaded.push(p);
                }
            }
        }

        *self.plugins.write().await = Arc::new(loaded);
    }

    /// 加载单个插件目录。失败记 warn 返回 None（不阻塞其他插件）。
    async fn load_one(&self, dir: &Path, dev: bool) -> Option<LoadedPlugin> {
        let manifest_path = dir.join("manifest.json");
        let bytes = tokio::fs::read(&manifest_path).await.ok()?;
        let manifest = match PluginManifest::parse(&bytes).and_then(|m| {
            m.validate()?;
            Ok(m)
        }) {
            Ok(m) => m,
            Err(e) => {
                log_info!("[plugin] 跳过 {dir:?}: manifest 非法: {e}");
                return None;
            }
        };

        // minAppVersion 门槛（app_version 未知或 manifest 未声明时从宽放行）。
        if !manifest.min_app_version.is_empty()
            && !self.app_version.is_empty()
            && !super::semver::satisfies_min(&self.app_version, &manifest.min_app_version)
        {
            log_info!(
                "[plugin] 跳过 {}: 需 App ≥ {}，当前 {}",
                manifest.identity,
                manifest.min_app_version,
                self.app_version
            );
            return None;
        }

        let resolver_entry = manifest.resolvers.first().map(|r| dir.join(&r.entry));
        let hooks_entry = manifest.hooks.as_ref().map(|h| dir.join(&h.entry));

        // 死订阅检查：同时声明 resolver 与订阅 onMetaProbed → warn（带 resolver 的
        // 任务跳过 probe，onMetaProbed 不会触发）。
        if !manifest.resolvers.is_empty()
            && let Some(h) = &manifest.hooks
            && h.events.iter().any(|e| e == "onMetaProbed")
        {
            log_info!(
                "[plugin] {} 同时声明 resolver 与 onMetaProbed；带 resolver 的任务跳过探测，该钩子不会触发",
                manifest.identity
            );
        }

        // 非 dev：加载时读入源码缓存。
        let (resolver_cache, hooks_cache) = if dev {
            (None, None)
        } else {
            let rc = match &resolver_entry {
                Some(p) => match tokio::fs::read_to_string(p).await {
                    Ok(s) => Some(s),
                    Err(e) => {
                        log_info!(
                            "[plugin] 跳过 {}: 读取 resolver 失败: {e}",
                            manifest.identity
                        );
                        return None;
                    }
                },
                None => None,
            };
            let hc = match &hooks_entry {
                Some(p) => match tokio::fs::read_to_string(p).await {
                    Ok(s) => Some(s),
                    Err(e) => {
                        log_info!("[plugin] 跳过 {}: 读取 hooks 失败: {e}", manifest.identity);
                        return None;
                    }
                },
                None => None,
            };
            (rc, hc)
        };

        let identity = manifest.identity.clone();
        let enabled_str = self
            .db
            .get_config(&format!("plugin.{identity}.enabled"))
            .await
            .ok()
            .flatten();
        let reason_str = self
            .db
            .get_config(&format!("plugin.{identity}.disabled_reason"))
            .await
            .ok()
            .flatten();
        let disabled_reason = reason_str
            .as_deref()
            .map(DisabledReason::parse)
            .unwrap_or(DisabledReason::None);
        // 无 enabled 键 = 新装默认启用。
        let enabled = enabled_str.as_deref().map(|v| v == "true").unwrap_or(true);

        Some(LoadedPlugin {
            manifest,
            dir: dir.to_path_buf(),
            dev,
            enabled,
            disabled_reason,
            resolver_entry,
            hooks_entry,
            resolver_cache,
            hooks_cache,
            timeout_streak: Arc::new(AtomicU32::new(0)),
        })
    }

    /// 纯 Rust glob 首匹配（按 identity 字典序稳定排序）。返回命中插件 identity。
    pub async fn match_resolver(&self, url: &str) -> Option<String> {
        let snapshot = self.plugins.read().await.clone();
        let mut candidates: Vec<&LoadedPlugin> = snapshot
            .iter()
            .filter(|p| p.enabled && !p.manifest.resolvers.is_empty())
            .collect();
        candidates.sort_by(|a, b| a.manifest.identity.cmp(&b.manifest.identity));
        for p in candidates {
            for pat in p.manifest.resolver_urls() {
                if super::manifest::url_glob_match(pat, url) {
                    return Some(p.manifest.identity.clone());
                }
            }
        }
        None
    }

    /// 惰性解析。off-actor worker 调用（在插件专用 runtime 上）。
    ///
    /// **fail-closed**：任务带 resolver 绑定但插件已卸载/被禁用/当前版本无
    /// resolver 时返回 `Err`——绝不放行原始页面 URL（那会把网页 HTML 当媒体文件
    /// 存盘）。用户经「忽略插件重试」逃生舱显式清绑定后方可按原始链接下载；
    /// 正常卸载路径由 [`Self::uninstall`] 批量清绑定，不会走到这里。
    pub async fn resolve(
        &self,
        identity: &str,
        req: ResolveRequest,
    ) -> Result<Option<ResolveResult>, PluginError> {
        // 从快照克隆所需（避免跨 await 持锁）。
        let (manifest, streak, source, dev_ver) = {
            let snapshot = self.plugins.read().await.clone();
            let Some(p) = snapshot.iter().find(|p| p.manifest.identity == identity) else {
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} 已卸载或加载失败；可在任务菜单选择「忽略插件重试」按原始链接下载"
                )));
            };
            if !p.enabled {
                let reason = match p.disabled_reason {
                    DisabledReason::CircuitBreaker => "已被熔断自动禁用",
                    _ => "已被禁用",
                };
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} {reason}；重新启用插件，或选择「忽略插件重试」按原始链接下载"
                )));
            }
            let source = p.resolver_source().await;
            (
                p.manifest.clone(),
                p.timeout_streak.clone(),
                source,
                p.manifest.version.clone(),
            )
        };
        let Some(source) = source else {
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 当前版本未提供 resolver（或源码读取失败）；可选择「忽略插件重试」按原始链接下载"
            )));
        };

        // required 运行时校验。
        let values = self.load_setting_values(identity).await;
        for f in &manifest.settings {
            if f.required && value_of(&values, f).is_none() {
                return Err(PluginError::MissingRequiredSetting(format!(
                    "插件 {identity} 需先配置「{}」",
                    f.title
                )));
            }
        }

        let settings_json = build_typed_settings_json(&manifest, &values);
        let budget = self.resolve_budget_for(&manifest);
        let script = PluginScript {
            identity: identity.to_string(),
            source,
            entry_fn_hint: PluginEntryKind::Resolve,
            version: dev_ver,
            app_version: self.app_version.clone(),
        };

        let result = self
            .runtime
            .invoke_resolve(
                &script,
                req,
                settings_json,
                self.bridge.clone(),
                budget,
                HostContext {
                    // resolve 平面授予 yt-dlp（直链提取的主战场）；ffmpeg 无产物牢笼故不授予。
                    ytdlp_permitted: manifest.has_permission(PERMISSION_YTDLP),
                    ..Default::default()
                },
            )
            .await;

        // 熔断计数：连续 Timeout/MemoryLimitExceeded 触发自动禁用。QuickJS 内存
        // 超限在 JS 侧表现为「out of memory」异常，quickjs.rs 的 reclassify_oom
        // 已把它归一为 `MemoryLimitExceeded`——OOM 与超时同样计入熔断。
        match &result {
            Err(PluginError::Timeout) | Err(PluginError::MemoryLimitExceeded) => {
                let n = streak.fetch_add(1, Ordering::SeqCst) + 1;
                if n >= CIRCUIT_BREAKER_THRESHOLD {
                    self.trip_circuit_breaker(identity).await;
                }
            }
            _ => {
                streak.store(0, Ordering::SeqCst);
            }
        }

        let result = result?;
        if let Some(res) = &result {
            validate_resolve_output(res)?;
        }
        Ok(result)
    }

    /// 通知平面：遍历声明该事件且 match 命中的启用插件，逐个在插件 runtime 上 spawn
    /// invoke_hook。**全部 fire-and-forget，本函数立即返回**。
    pub async fn notify(&self, event: PluginEvent) {
        let snapshot = self.plugins.read().await.clone();
        let ev_name = event.declared_name();
        let url = event.url().to_string();
        for idx in 0..snapshot.len() {
            let p = &snapshot[idx];
            if !p.enabled {
                continue;
            }
            let Some(hooks) = &p.manifest.hooks else {
                continue;
            };
            if !hooks.events.iter().any(|e| e == ev_name) {
                continue;
            }
            // match.urls（缺省 = 全匹配）。
            let matches = match &hooks.match_decl {
                Some(m) => m
                    .urls
                    .iter()
                    .any(|pat| super::manifest::url_glob_match(pat, &url)),
                None => true,
            };
            if !matches {
                continue;
            }

            let runtime = self.runtime.clone();
            let bridge = self.bridge.clone();
            let db = self.db.clone();
            let manifest = p.manifest.clone();
            let identity = p.manifest.identity.clone();
            let version = p.manifest.version.clone();
            let app_version = self.app_version.clone();
            // ffmpeg 门：授权 + 有产物文件（onDone 才有）→ 注入 flux.ffmpeg 并抬升
            // hook 墙钟预算；牢笼根 = 产物所在目录。其余事件/未授权 → 无 ffmpeg。
            let ffmpeg_permitted = p.manifest.has_permission(PERMISSION_FFMPEG);
            let ytdlp_permitted = p.manifest.has_permission(PERMISSION_YTDLP);
            let ffmpeg_root = match &event {
                PluginEvent::Done { file_path, .. } => {
                    Path::new(file_path).parent().map(Path::to_path_buf)
                }
                _ => None,
            };
            let host = HostContext {
                ffmpeg_permitted,
                ffmpeg_root,
                ytdlp_permitted,
            };
            // 授权外部工具（ffmpeg 有产物牢笼 / yt-dlp 任意上下文）→ 抬升墙钟预算。
            let budget = if (ffmpeg_permitted && host.ffmpeg_root.is_some()) || ytdlp_permitted {
                EXTERNAL_TOOL_HOOK_BUDGET
            } else {
                self.hook_budget
            };
            let event = event.clone();
            // onDone 活动指示：带产物钩子可能长时（ffmpeg 转码），旁路上报
            // 开始/结束供 UI 显示「插件处理中」，不触碰任务状态机。
            let activity_task_id = match &event {
                PluginEvent::Done { task_id, .. } => Some(task_id.clone()),
                _ => None,
            };
            let activity_plugin_id = p.manifest.identity.clone();
            let sink = self.sink.clone();
            let handle = self.runtime.spawn_handle();
            let dev = p.dev;
            let hooks_entry = p.hooks_entry.clone();
            let hooks_cache = p.hooks_cache.clone();

            handle.spawn(async move {
                if let Some(tid) = &activity_task_id {
                    sink.emit(EngineEvent::PluginHookActivity {
                        task_id: tid.clone(),
                        plugin_id: activity_plugin_id.clone(),
                        running: true,
                    });
                }
                let source = match (hooks_entry, dev) {
                    (Some(path), true) => tokio::fs::read_to_string(&path).await.ok(),
                    (Some(_), false) => hooks_cache,
                    (None, _) => None,
                };
                if let Some(source) = source {
                    let values = load_setting_values_db(&db, &identity).await;
                    let settings_json = build_typed_settings_json(&manifest, &values);
                    let script = PluginScript {
                        identity,
                        source,
                        entry_fn_hint: PluginEntryKind::Hook,
                        version,
                        app_version,
                    };
                    runtime
                        .invoke_hook(&script, event, settings_json, bridge, budget, host)
                        .await;
                }
                if let Some(tid) = &activity_task_id {
                    sink.emit(EngineEvent::PluginHookActivity {
                        task_id: tid.clone(),
                        plugin_id: activity_plugin_id,
                        running: false,
                    });
                }
            });
        }
    }

    /// resolve 预算：manifest timeoutMs 可下调，30s 硬顶。
    fn resolve_budget_for(&self, manifest: &PluginManifest) -> ExecutionBudget {
        let timeout = manifest
            .resolvers
            .first()
            .and_then(|r| r.timeout_ms)
            .map(Duration::from_millis)
            .unwrap_or(self.resolve_budget.timeout)
            .min(HARD_TIMEOUT_CEILING);
        ExecutionBudget {
            timeout,
            memory_limit_bytes: self.resolve_budget.memory_limit_bytes,
        }
    }

    // ---------------------------------------------------------------------
    // 安装 / 卸载 / 启停 / 设置
    // ---------------------------------------------------------------------

    /// 从 zip 字节安装。
    pub async fn install_from_zip(&self, bytes: Vec<u8>) -> Result<String, PluginError> {
        let identity = super::install::install_from_zip(&self.root, &bytes)?;
        self.finish_install(&identity).await?;
        Ok(identity)
    }

    /// 从目录安装（不剥壳，path 须直接含 manifest.json）。
    pub async fn install_from_dir(&self, path: &Path) -> Result<String, PluginError> {
        let identity = super::install::install_from_dir(&self.root, path)?;
        self.finish_install(&identity).await?;
        Ok(identity)
    }

    /// dev 安装（写 plugin.dev.<identity>=abs(path)，不拷贝）。
    pub async fn install_dev(&self, path: &Path) -> Result<String, PluginError> {
        let manifest_path = path.join("manifest.json");
        let bytes = std::fs::read(&manifest_path)
            .map_err(|e| PluginError::ManifestInvalid(format!("读取 manifest 失败: {e}")))?;
        let manifest = PluginManifest::parse(&bytes)?;
        manifest.validate()?;
        let identity = manifest.identity.clone();
        let abs = std::fs::canonicalize(path)
            .map_err(|e| PluginError::ManifestInvalid(format!("解析路径失败: {e}")))?;
        self.db
            .set_config(&format!("plugin.dev.{identity}"), &abs.to_string_lossy())
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.finish_install(&identity).await?;
        Ok(identity)
    }

    /// 安装共同后续：compile 校验 + enabled 规则 + 整表重载。
    async fn finish_install(&self, identity: &str) -> Result<(), PluginError> {
        // 读回 disabled_reason 判断 enabled 规则。
        let reason = self
            .db
            .get_config(&format!("plugin.{identity}.disabled_reason"))
            .await
            .ok()
            .flatten()
            .as_deref()
            .map(DisabledReason::parse)
            .unwrap_or(DisabledReason::None);
        let has_enabled_key = self
            .db
            .get_config(&format!("plugin.{identity}.enabled"))
            .await
            .ok()
            .flatten()
            .is_some();

        // compile / pattern 校验各 entry：先临时重载以拿到源码，再校验。
        // 任一校验失败 → **回滚**（uninstall：删目录 + 清 config + 重载），
        // 避免残留一个「已装但校验失败」且默认启用的插件（reviewer finding 5）。
        self.load_all().await;
        let validation: Result<(), PluginError> = {
            let snapshot = self.plugins.read().await.clone();
            match snapshot.iter().find(|p| p.manifest.identity == identity) {
                Some(p) => {
                    let mut r = Ok(());
                    if let Some(src) = p.resolver_source().await {
                        r = self.runtime.check_compile(&src);
                    }
                    if r.is_ok()
                        && let Some(src) = p.hooks_source().await
                    {
                        r = self.runtime.check_compile(&src);
                    }
                    if r.is_ok() {
                        for f in &p.manifest.settings {
                            if let Some(pat) = &f.pattern
                                && !self.runtime.regex_valid(pat)
                            {
                                r = Err(PluginError::ManifestInvalid(format!(
                                    "setting '{}': pattern 非法（JS RegExp 编译失败）",
                                    f.key
                                )));
                                break;
                            }
                        }
                    }
                    r
                }
                None => Err(PluginError::ManifestInvalid(
                    "安装后未能加载插件（校验失败或版本门槛不满足）".to_string(),
                )),
            }
        };
        if let Err(e) = validation {
            // 回滚用 purge（不清任务绑定）：失败的升级不该改变存量任务语义——
            // 绑定保留，resume 走 fail-closed 报错，用户重装插件即恢复。
            let _ = self.purge(identity).await;
            return Err(e);
        }

        // enabled 写入规则：
        // - 新装（无 enabled 键）或熔断 → enabled=1, reason=None（升级即解熔断）
        // - Manual → 维持 disabled 不动（不覆盖用户主动关闭）
        if !has_enabled_key || reason == DisabledReason::CircuitBreaker {
            self.write_enabled(identity, true, DisabledReason::None)
                .await;
        }
        // 最终重载让内存态与 config 一致。
        self.load_all().await;
        Ok(())
    }

    /// 卸载（用户主动）：删目录 + 清 config 键 + 清任务 resolver 绑定。
    ///
    /// 清绑定 = 对受影响任务批量应用「忽略插件、按原始链接重跑」逃生舱；不清则
    /// 留下 orphaned 绑定，resume 走 fail-closed 报错（见 [`Self::resolve`]）。
    pub async fn uninstall(&self, identity: &str) -> Result<(), PluginError> {
        let _ = self.db.clear_tasks_resolver(identity).await;
        self.purge(identity).await
    }

    /// 删目录 + 清 `plugin.<identity>.` 前缀全部 config 键 + 重载。
    /// 安装回滚复用（与 [`Self::uninstall`] 的差别：**不**清任务绑定）。
    async fn purge(&self, identity: &str) -> Result<(), PluginError> {
        // 删安装目录（dev 不删源，仅删配置键）。
        let dir = self.root.join(identity);
        if dir.exists() {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        for prefix in [
            format!("plugin.{identity}."),
            format!("plugin.dev.{identity}"),
        ] {
            if let Ok(entries) = self.db.list_config_with_prefix(&prefix).await {
                for (k, _) in entries {
                    let _ = self.db.delete_config(&k).await;
                }
            }
            // plugin.dev.<id> 是精确键（无尾点），单独删。
            let _ = self.db.delete_config(&prefix).await;
        }
        self.load_all().await;
        Ok(())
    }

    /// 手动开关（写 enabled + disabled_reason）。
    pub async fn set_enabled(&self, identity: &str, enabled: bool) -> Result<(), PluginError> {
        let reason = if enabled {
            DisabledReason::None
        } else {
            DisabledReason::Manual
        };
        self.write_enabled(identity, enabled, reason).await;
        self.load_all().await;
        Ok(())
    }

    async fn write_enabled(&self, identity: &str, enabled: bool, reason: DisabledReason) {
        let _ = self
            .db
            .set_config(
                &format!("plugin.{identity}.enabled"),
                if enabled { "true" } else { "false" },
            )
            .await;
        let _ = self
            .db
            .set_config(
                &format!("plugin.{identity}.disabled_reason"),
                reason.as_str(),
            )
            .await;
    }

    /// 熔断：自动禁用 + emit 事件。
    async fn trip_circuit_breaker(&self, identity: &str) {
        self.write_enabled(identity, false, DisabledReason::CircuitBreaker)
            .await;
        self.load_all().await;
        self.sink.emit(EngineEvent::PluginAutoDisabled {
            identity: identity.to_string(),
            reason: DisabledReason::CircuitBreaker.as_str().to_string(),
        });
        log_info!("[plugin] {identity} 连续超时，已自动熔断禁用");
    }

    /// 批量设置，all-or-nothing。
    pub async fn update_settings(
        &self,
        identity: &str,
        entries: &[(String, String)],
    ) -> Result<(), PluginError> {
        let manifest = {
            let snapshot = self.plugins.read().await.clone();
            snapshot
                .iter()
                .find(|p| p.manifest.identity == identity)
                .map(|p| p.manifest.clone())
        };
        let Some(manifest) = manifest else {
            return Err(PluginError::Runtime(format!("插件 {identity} 未找到")));
        };
        // 全量校验（任一失败即整体拒绝，不写任何键）。
        for (key, value) in entries {
            let field = manifest.settings.iter().find(|f| &f.key == key);
            let Some(field) = field else {
                return Err(PluginError::InvalidOutput(format!("未知设置项 '{key}'")));
            };
            self.validate_value(field, value)?;
        }
        // 全通过再逐个写。
        for (key, value) in entries {
            let _ = self
                .db
                .set_config(&format!("plugin.{identity}.setting.{key}"), value)
                .await;
        }
        Ok(())
    }

    /// 单设置项校验（类型/required/pattern/min-max/select/toggle）。
    fn validate_value(&self, field: &SettingField, value: &str) -> Result<(), PluginError> {
        let bad = |m: String| Err(PluginError::InvalidOutput(m));
        match field.ty {
            SettingType::Boolean => {
                if value != "true" && value != "false" {
                    return bad(format!("'{}' 必须为 true/false", field.key));
                }
            }
            SettingType::Number => {
                let v = value
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| {
                        PluginError::InvalidOutput(format!("'{}' 不是有效数字", field.key))
                    })?;
                if let Some(lo) = field.min
                    && v < lo
                {
                    return bad(format!("'{}' 小于下限 {lo}", field.key));
                }
                if let Some(hi) = field.max
                    && v > hi
                {
                    return bad(format!("'{}' 大于上限 {hi}", field.key));
                }
            }
            SettingType::String => {
                // select：成员必须 ∈ options。
                if field.effective_widget() == super::manifest::SettingWidget::Select
                    && !field.options.iter().any(|o| o.value == value)
                {
                    return bad(format!("'{}' 不是合法选项", field.key));
                }
                // pattern。
                if let Some(pat) = &field.pattern
                    && !self.runtime.regex_test(pat, value)
                {
                    return bad(format!("'{}' 不匹配 pattern", field.key));
                }
            }
        }
        Ok(())
    }

    /// 列出全部插件（供 UI）。
    pub async fn list(&self) -> Vec<PluginInfo> {
        let snapshot = self.plugins.read().await.clone();
        let mut out = Vec::with_capacity(snapshot.len());
        for p in snapshot.iter() {
            let values = self.load_setting_values(&p.manifest.identity).await;
            out.push(PluginInfo {
                identity: p.manifest.identity.clone(),
                name: p.manifest.name.clone(),
                version: p.manifest.version.clone(),
                description: p.manifest.description.clone(),
                homepage: p.manifest.homepage.clone(),
                enabled: p.enabled,
                dev_mode: p.dev,
                disabled_reason: p.disabled_reason.as_str().to_string(),
                settings: p.manifest.settings.clone(),
                settings_values: values.into_iter().collect(),
                permissions: p.manifest.permissions.clone(),
            });
        }
        out
    }

    /// 按 identity 查插件 manifest 声明的能力权限（供安装后依赖提醒）。
    ///
    /// 插件不存在时返回空（调用方视为无依赖）。
    pub async fn permissions_of(&self, identity: &str) -> Vec<String> {
        let snapshot = self.plugins.read().await.clone();
        snapshot
            .iter()
            .find(|p| p.manifest.identity == identity)
            .map(|p| p.manifest.permissions.clone())
            .unwrap_or_default()
    }

    /// 逃生舱窄接口：清该任务 resolver_plugin_id（不改插件全局状态）。
    pub async fn clear_task_resolver(&self, task_id: &str) {
        let _ = self.db.set_task_resolver(task_id, "").await;
    }

    /// 读取某插件的设置值（config `plugin.<id>.setting.*`）。
    async fn load_setting_values(&self, identity: &str) -> HashMap<String, String> {
        load_setting_values_db(&self.db, identity).await
    }
}

/// 读取设置值（自由函数，供 notify spawn 任务复用，无需 &self）。
async fn load_setting_values_db(db: &Db, identity: &str) -> HashMap<String, String> {
    let prefix = format!("plugin.{identity}.setting.");
    let mut map = HashMap::new();
    if let Ok(entries) = db.list_config_with_prefix(&prefix).await {
        for (k, v) in entries {
            if let Some(key) = k.strip_prefix(&prefix) {
                map.insert(key.to_string(), v);
            }
        }
    }
    map
}

/// 取字段生效值：config 存值 → manifest default → None。
fn value_of(values: &HashMap<String, String>, field: &SettingField) -> Option<String> {
    values
        .get(&field.key)
        .cloned()
        .or_else(|| field.default.clone())
}

/// 构建类型化设置 JSON 对象（string→JS string、number→JS number、boolean→JS bool）。
fn build_typed_settings_json(
    manifest: &PluginManifest,
    values: &HashMap<String, String>,
) -> String {
    let mut obj = serde_json::Map::new();
    for f in &manifest.settings {
        let raw = value_of(values, f);
        let jv = match f.ty {
            SettingType::Boolean => serde_json::Value::Bool(raw.as_deref() == Some("true")),
            SettingType::Number => match raw.as_deref().and_then(|s| s.parse::<f64>().ok()) {
                Some(n) => serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                None => serde_json::Value::Null,
            },
            SettingType::String => match raw {
                Some(s) => serde_json::Value::String(s),
                None => serde_json::Value::Null,
            },
        };
        obj.insert(f.key.clone(), jv);
    }
    serde_json::Value::Object(obj).to_string()
}

/// resolve 输出校验：url scheme ∈{http,https,ftp,magnet,ed2k}、长度 ≤8KB；
/// file_name 拒绝 `/ \ ..` 与控制字符。
fn validate_resolve_output(res: &ResolveResult) -> Result<(), PluginError> {
    // 变体存在时顶层 url 允许为空（选中变体后覆盖）；非空时仍须合法。
    if res.variants.is_empty() || !res.url.is_empty() {
        check_output_url(&res.url)?;
    }
    if res.variants.len() > 50 {
        return Err(PluginError::InvalidOutput(format!(
            "variants 过多: {} > 50",
            res.variants.len()
        )));
    }
    for v in &res.variants {
        if v.label.is_empty() || v.label.chars().count() > 200 {
            return Err(PluginError::InvalidOutput(
                "variant label 须非空且 ≤200 字符".to_string(),
            ));
        }
        check_output_url(&v.url)?;
        if let Some(a) = &v.audio_url
            && !a.is_empty()
        {
            check_output_url(a)?;
        }
        if let Some(name) = &v.file_name {
            check_file_name(name)?;
        }
    }
    if let Some(a) = &res.audio_url
        && !a.is_empty()
    {
        check_output_url(a)?;
    }
    if let Some(name) = &res.file_name {
        check_file_name(name)?;
    }
    Ok(())
}

fn check_file_name(name: &str) -> Result<(), PluginError> {
    if name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.chars().any(|c| c.is_control())
    {
        return Err(PluginError::InvalidOutput(format!(
            "file_name 非法: {name}"
        )));
    }
    Ok(())
}

fn check_output_url(url: &str) -> Result<(), PluginError> {
    if url.len() > 8 * 1024 {
        return Err(PluginError::InvalidOutput("url 超过 8KB".to_string()));
    }
    let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(
        scheme.as_str(),
        "http" | "https" | "ftp" | "magnet" | "ed2k"
    ) {
        return Err(PluginError::InvalidOutput(format!(
            "url scheme 不允许: {scheme}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::validate_resolve_output;
    use crate::plugin::{ResolveResult, ResolveVariant};

    fn variant(label: &str, url: &str) -> ResolveVariant {
        ResolveVariant {
            label: label.into(),
            url: url.into(),
            ..Default::default()
        }
    }

    /// 有 variants 时顶层 url 允许为空（选中变体后覆盖）。
    #[test]
    fn variants_allow_empty_top_level_url() {
        let res = ResolveResult {
            variants: vec![variant("1080p", "https://v.example.com/a")],
            ..Default::default()
        };
        assert!(validate_resolve_output(&res).is_ok());
    }

    /// 无 variants 且顶层 url 为空 → 拒（原有单直链语义不放松）。
    #[test]
    fn empty_url_without_variants_rejected() {
        assert!(validate_resolve_output(&ResolveResult::default()).is_err());
    }

    /// 变体 label 为空 / url scheme 非法 → 拒。
    #[test]
    fn invalid_variant_rejected() {
        let empty_label = ResolveResult {
            variants: vec![variant("", "https://v.example.com/a")],
            ..Default::default()
        };
        assert!(validate_resolve_output(&empty_label).is_err());
        let bad_scheme = ResolveResult {
            variants: vec![variant("x", "javascript:alert(1)")],
            ..Default::default()
        };
        assert!(validate_resolve_output(&bad_scheme).is_err());
    }

    /// 变体数量 > 50 → 拒。
    #[test]
    fn too_many_variants_rejected() {
        let res = ResolveResult {
            variants: (0..51)
                .map(|i| variant(&format!("v{i}"), "https://v.example.com/a"))
                .collect(),
            ..Default::default()
        };
        assert!(validate_resolve_output(&res).is_err());
    }
}
