//! `PluginManager` —— 插件装载 / 启停 / resolve / notify / 安装 / 设置。
//!
//! `Arc` 共享，注入 `DownloadManager`。`RwLock<Arc<Vec<LoadedPlugin>>>` 读多写少：
//! install/uninstall/toggle 写（整表原子替换），resolve/hook match 读。**不用 arc-swap**
//! （workspace 无此依赖且约束禁新增）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::RwLock;

use crate::db::Db;
use crate::events::{EngineEvent, EventSink};
use crate::logger::log_info;
use crate::rss::parser::{ParsedFeed, ParsedItem};
use crate::subscription::{
    SubscriptionFetchFuture, SubscriptionFetchRequest, SubscriptionProvider,
};

use super::call_egress::{CallEgress, EgressOutcome, EgressPlan, PlanTask, plan_call, route_name};
use super::manifest::{
    PERMISSION_AUTH, PERMISSION_FFMPEG, PERMISSION_YTDLP, PluginManifest, SettingField,
    SettingType, is_safe_relative_path, is_valid_identity,
};
use super::quickjs::HARD_TIMEOUT_CEILING;
use super::runtime::{
    AuthRequest, AuthResult, ExecutionBudget, HostContext, ManifestItem, PluginBridge,
    PluginEntryKind, PluginError, PluginEvent, PluginScript, ResolveManifest, ResolveRequest,
    ResolveResult, ScriptRuntime, SubscriptionRequest,
};

#[path = "archive_rules_manager.rs"]
mod archive_rules_manager;
#[path = "host_manager.rs"]
mod host_manager;
#[path = "interceptors.rs"]
mod interceptors;
#[path = "routing.rs"]
pub mod routing;
/// 连续超时/超内存达到该次数 → 自动熔断禁用。
const CIRCUIT_BREAKER_THRESHOLD: u32 = 3;

/// 默认 resolve 预算。
const DEFAULT_RESOLVE_BUDGET: ExecutionBudget = ExecutionBudget {
    timeout: Duration::from_secs(10),
    memory_limit_bytes: 64 * 1024 * 1024,
};
/// resolver 可经 manifest `memoryLimitMb` 声明的 QuickJS 堆硬顶。
pub const RESOLVE_MEMORY_CEILING: usize = 512 * 1024 * 1024;
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
/// auth 平面默认预算（M-2）：与 resolve 的 manifest.resolvers[0].timeoutMs 完全
/// 解耦——一个把 resolver 超时调到 500ms 的插件不该连带把登录脚本的墙钟预算
/// 也压到 500ms。可被 manifest `auth.timeoutMs` 下调，30s 硬顶。
const DEFAULT_AUTH_TIMEOUT: Duration = Duration::from_secs(30);

/// 禁用原因（PascalCase 序列化，全文一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    None,
    Manual,
    CircuitBreaker,
    Revoked,
    ConsentRequired,
}

impl DisabledReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DisabledReason::None => "None",
            DisabledReason::Manual => "Manual",
            DisabledReason::CircuitBreaker => "CircuitBreaker",
            DisabledReason::Revoked => "Revoked",
            DisabledReason::ConsentRequired => "ConsentRequired",
        }
    }
    /// 面向用户的禁用原因短语（接在「插件 X」之后）。
    pub(super) fn describe(self) -> &'static str {
        match self {
            DisabledReason::CircuitBreaker => "已被熔断自动禁用",
            DisabledReason::Revoked => "已被插件市场撤回",
            DisabledReason::ConsentRequired => "需要授予新的权限（插件页面中完成授权）",
            DisabledReason::Manual | DisabledReason::None => "已被禁用",
        }
    }
    /// 面向用户的恢复途径（接在 [`Self::describe`] 之后）。
    fn remedy(self) -> &'static str {
        match self {
            DisabledReason::Revoked => "升级插件后重试",
            DisabledReason::ConsentRequired => "完成授权后重试",
            DisabledReason::CircuitBreaker | DisabledReason::Manual | DisabledReason::None => {
                "重新启用插件后重试"
            }
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "Manual" => DisabledReason::Manual,
            "CircuitBreaker" => DisabledReason::CircuitBreaker,
            "Revoked" => DisabledReason::Revoked,
            "ConsentRequired" => DisabledReason::ConsentRequired,
            _ => DisabledReason::None,
        }
    }
}

/// 插件加载状态（PascalCase 序列化，全文一致；wire 契约固定为
/// `"Loaded"`/`"Failed"`，三端客户端按字面量比较，不可改动，见 671#4）。
/// 单点定义比照同文件 [`DisabledReason`] 的先例：生产端（[`PluginManager::list`]）
/// 统一从这里取字面量，杜绝散落的字符串字面量漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginLoadStatus {
    Loaded,
    Failed,
}

impl PluginLoadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loaded => "Loaded",
            Self::Failed => "Failed",
        }
    }
}

/// 安装前的启停相关 config 键快照（失败时原样恢复）。
struct InstallState {
    enabled: Option<String>,
    disabled_reason: Option<String>,
    revoked_version: Option<String>,
}

/// 读取已安装目录 manifest 的版本（决定安装后的启停规则，读不到即安装失败）。
async fn installed_manifest_version(dir: &Path) -> Result<String, PluginError> {
    let bytes = tokio::fs::read(dir.join("manifest.json"))
        .await
        .map_err(|e| PluginError::ManifestInvalid(format!("读取 manifest 失败: {e}")))?;
    PluginManifest::parse(&bytes).map(|manifest| manifest.version)
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
    /// interceptors 入口绝对路径（若声明）。
    intercept_entry: Option<PathBuf>,
    /// auth 入口绝对路径（若声明）。
    auth_entry: Option<PathBuf>,
    /// subscription provider 入口绝对路径（若声明）。
    subscription_entry: Option<PathBuf>,
    /// 非 dev 模式的缓存源码（加载时读入）。
    resolver_cache: Option<String>,
    hooks_cache: Option<String>,
    intercept_cache: Option<String>,
    auth_cache: Option<String>,
    subscription_cache: Option<String>,
    /// 归档入口绝对路径与非 dev 模式的源码缓存。
    archive_entry: Option<PathBuf>,
    archive_cache: Option<String>,
    /// `main` 入口绝对路径（若声明）。
    main_entry: Option<PathBuf>,
    /// 非 dev 模式的 `main` 源码缓存。
    main_cache: Option<String>,
    /// 视图图标 SVG 文本（view id → 内容），加载期读入并校验。
    view_icons: Vec<(String, String)>,
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
    async fn intercept_source(&self) -> Option<String> {
        match (&self.intercept_entry, self.dev) {
            (Some(p), true) => tokio::fs::read_to_string(p).await.ok(),
            (Some(_), false) => self.intercept_cache.clone(),
            (None, _) => None,
        }
    }
    async fn archive_source(&self) -> Option<String> {
        match (&self.archive_entry, self.dev) {
            (Some(path), true) => tokio::fs::read_to_string(path).await.ok(),
            (Some(_), false) => self.archive_cache.clone(),
            (None, _) => None,
        }
    }

    async fn auth_source(&self) -> Option<String> {
        match (&self.auth_entry, self.dev) {
            (Some(p), true) => tokio::fs::read_to_string(p).await.ok(),
            (Some(_), false) => self.auth_cache.clone(),
            (None, _) => None,
        }
    }

    async fn main_source(&self) -> Option<String> {
        match (&self.main_entry, self.dev) {
            (Some(p), true) => tokio::fs::read_to_string(p).await.ok(),
            (Some(_), false) => self.main_cache.clone(),
            (None, _) => None,
        }
    }

    async fn subscription_source(&self) -> Option<String> {
        match (&self.subscription_entry, self.dev) {
            (Some(p), true) => tokio::fs::read_to_string(p).await.ok(),
            (Some(_), false) => self.subscription_cache.clone(),
            (None, _) => None,
        }
    }
}

/// 动态插件订阅路由。provider 安装、启停或卸载后无需重建引擎，路由每次
/// 调用都向 `PluginManager` 读取当前快照。
pub struct PluginSubscriptionRouter {
    manager: Arc<PluginManager>,
}

impl SubscriptionProvider for PluginSubscriptionRouter {
    fn id(&self) -> &str {
        "*"
    }

    fn fetch(&self, request: SubscriptionFetchRequest) -> SubscriptionFetchFuture {
        let manager = self.manager.clone();
        let provider_id = request.provider_id.clone();
        Box::pin(async move {
            manager
                .fetch_subscription(&provider_id, request)
                .await
                .map_err(|e| e.to_string())
        })
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
    /// 是否声明平台登录入口。
    pub auth_supported: bool,
    /// manifest 声明的订阅 provider ID；仅启用插件会被前端作为可选订阅来源展示。
    pub subscription_provider_ids: Vec<String>,
    /// manifest resolvers[0].match.urls（glob，语义见 url_glob_match）。
    pub resolver_urls: Vec<String>,
    /// 加载状态：`Loaded` 表示已通过启动时的 manifest/源码加载，`Failed` 表示
    /// 目录仍存在但加载失败。与 `enabled` 分离：手动禁用的插件仍可能已加载。
    pub load_status: String,
    /// 加载失败的可读原因；加载成功时为空。
    pub load_error: String,
    /// manifest 声明的贡献点（视图 / 命令 / 嗅探规则）；加载失败的插件为空。
    pub contributions: super::manifest::ContributesDecl,
    /// 视图图标 SVG 文本（view id → 内容）；加载失败的插件为空。
    pub view_icons: Vec<(String, String)>,
    /// manifest 是否声明了 `main`（可移植视图由 main 提供）。
    pub has_main: bool,
    /// 由权限推导的风险等级。
    pub risk: super::manifest::PluginRisk,
    /// 所有 desktop 视图声明的网络主机（排序去重）。
    pub network_hosts: Vec<String>,
    /// 尚未授权的权限（manifest 请求 − `plugin.<id>.consent` 已授权）。
    pub consent_pending: Vec<String>,
    /// 尚未授权的网络主机。
    pub consent_pending_hosts: Vec<String>,
    /// 插件包修订号：包代码被替换（安装 / 升级 / dev 安装 / dev 重载）后变化，只做相等比较。
    pub package_revision: u64,
    /// 插件包绝对目录；仅已加载插件为 Some。
    pub package_dir: Option<String>,
}

/// 目录或 dev 配置仍存在、但没有进入运行时快照的插件。
///
/// 失败插件不能混入 `LoadedPlugin`，否则解析链路会误把它当作可运行插件；
/// 但管理页仍需要看到它，才能展示原因并允许用户卸载或修复。
#[derive(Debug, Clone)]
struct FailedPlugin {
    identity: String,
    /// 实际扫描到的插件目录。identity 可能来自非法 manifest，不能反向拼接路径。
    dir: Box<Path>,
    manifest: Option<Box<PluginManifest>>,
    dev_mode: bool,
    error: String,
}

/// 插件管理器。
pub struct PluginManager {
    runtime: Arc<dyn ScriptRuntime>,
    bridge: Arc<dyn PluginBridge>,
    plugins: RwLock<Arc<Vec<LoadedPlugin>>>,
    failed_plugins: RwLock<Arc<Vec<FailedPlugin>>>,
    db: Db,
    root: PathBuf,
    app_version: String,
    resolve_budget: ExecutionBudget,
    hook_budget: ExecutionBudget,
    sink: Arc<dyn EventSink>,
    /// 串行化 `load_all`：扫描 + 整表替换非原子，交错的两次重载会用旧快照
    /// 覆盖新的启停/熔断状态。
    load_lock: tokio::sync::Mutex<()>,
    audit: super::audit::AuditLog,
    /// 常驻宿主（`main`）的运行态：宿主表、视图会话、定时器、任务端口。
    host: host_manager::HostState,
    /// 多插件选择框按站点串行化（见 [`routing`]）。
    site_prompts: routing::SitePromptLocks,
    /// 插件包修订号（见 [`super::package_revision`]）。
    package_revisions: super::package_revision::PackageRevisions,
    /// Serializes publication so state snapshots and emitted events stay ordered.
    publish_lock: tokio::sync::Mutex<()>,
    /// Cache of validated archive rule packs; matching never performs I/O.
    archive_rule_packs:
        tokio::sync::RwLock<HashMap<String, Arc<super::archive_rules::VerifiedArchiveRules>>>,
    /// Poll diagnostics stay in memory; unchanged rule checks never persist state.
    archive_rule_status:
        tokio::sync::RwLock<HashMap<String, archive_rules_manager::ArchiveRuleCache>>,
    /// Per-plugin serialized signed archive-rule refreshes.
    archive_rule_locks: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Wakes the idle updater when a plugin's enabled rule feeds change.
    archive_rule_updates_notify: Arc<tokio::sync::Notify>,
    archive_rule_updates_started: AtomicBool,
    /// 全局出口来源（引擎装配时注入）：非任务调用（预解析 / 订阅 / 登录 / 宿主加速改写）
    /// 据此制定出口计划。未注入（单测里的 null bridge）时调用走 bridge 的全局出口、不换路。
    egress_source: std::sync::RwLock<Option<Arc<super::bridge::EngineBridge>>>,
}

/// 一次 resolve 调用的参数（前置校验通过后；每个出口步复用）。
struct PreparedResolve {
    streak: Arc<AtomicU32>,
    script: PluginScript,
    settings_json: String,
    budget: ExecutionBudget,
    host: HostContext,
    second_stage: bool,
}

/// 计划内所有路径都断在网络层：错误文案附上已尝试的出口，便于用户判断是网络问题。
/// 只改写插件运行时错误的文案；`Timeout` 等变体保持原样（调用方按变体分支）。
fn with_tried_routes(error: PluginError, failed: &[crate::auto_proxy::RoutePath]) -> PluginError {
    match error {
        PluginError::Runtime(message) if failed.len() > 1 => {
            let tried: Vec<&str> = failed
                .iter()
                .map(|route| route_name(Some(*route)))
                .collect();
            PluginError::Runtime(format!(
                "{message}（已依次尝试{}，均断在网络层）",
                tried.join(" → ")
            ))
        }
        other => other,
    }
}

impl PluginManager {
    /// 构造（不加载）；随后调用 [`Self::load_all`]。审计写入器由 manager 自建。
    pub fn new(
        runtime: Arc<dyn ScriptRuntime>,
        bridge: Arc<dyn PluginBridge>,
        db: Db,
        root: PathBuf,
        app_version: String,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        let audit = super::audit::AuditLog::new(db.clone(), &runtime.spawn_handle());
        Self::with_audit(runtime, bridge, db, root, app_version, sink, audit)
    }

    /// 与 [`Self::new`] 相同，但使用外部创建的审计写入器（引擎装配时同一个
    /// [`super::audit::AuditLog`] 同时喂给 `AuditingBridge`）。
    pub fn with_audit(
        runtime: Arc<dyn ScriptRuntime>,
        bridge: Arc<dyn PluginBridge>,
        db: Db,
        root: PathBuf,
        app_version: String,
        sink: Arc<dyn EventSink>,
        audit: super::audit::AuditLog,
    ) -> Self {
        Self {
            runtime,
            bridge,
            plugins: RwLock::new(Arc::new(Vec::new())),
            failed_plugins: RwLock::new(Arc::new(Vec::new())),
            db,
            root,
            app_version,
            resolve_budget: DEFAULT_RESOLVE_BUDGET,
            hook_budget: DEFAULT_HOOK_BUDGET,
            sink,
            load_lock: tokio::sync::Mutex::new(()),
            audit,
            host: host_manager::HostState::new(),
            site_prompts: routing::SitePromptLocks::default(),
            package_revisions: super::package_revision::PackageRevisions::new(),
            archive_rule_locks: tokio::sync::Mutex::new(HashMap::new()),
            archive_rule_packs: tokio::sync::RwLock::new(HashMap::new()),
            archive_rule_status: tokio::sync::RwLock::new(HashMap::new()),
            archive_rule_updates_notify: Arc::new(tokio::sync::Notify::new()),
            archive_rule_updates_started: AtomicBool::new(false),
            publish_lock: tokio::sync::Mutex::new(()),
            egress_source: std::sync::RwLock::new(None),
        }
    }

    /// 审计写入器（生命周期记录与宿主调用共用）。
    pub fn audit(&self) -> &super::audit::AuditLog {
        &self.audit
    }

    /// 广播当前插件状态（`EngineEvent::PluginsChanged`）。任何改变 `PluginDto`
    /// 可见字段或插件相关查询（设置、站点默认）的操作结束时调用。
    pub(crate) async fn publish_state(&self) {
        let _ordered = self.publish_lock.lock().await;
        let plugins = self.list().await;
        self.sink.emit(EngineEvent::PluginsChanged(plugins));
    }

    /// 插件生效代码被替换（安装 / 升级 / dev 重载成功）：递增包修订号，并把熔断计数
    /// 清零——新代码不继承旧代码的连续超时，而之后无关的 `load_all` 继续沿用同一计数。
    async fn mark_code_replaced(&self, identity: &str) {
        self.package_revisions.bump(identity);
        if let Some(plugin) = self
            .plugins
            .read()
            .await
            .iter()
            .find(|p| p.manifest.identity == identity)
        {
            plugin.timeout_streak.store(0, Ordering::SeqCst);
        }
    }

    /// 专用运行时 handle，供 off-actor worker spawn（禁止裸 tokio::spawn）。
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.spawn_handle()
    }
    /// Start a weak-manager updater; it sleeps without periodic work when no signed feeds are enabled.
    pub(crate) fn start_archive_rule_updates(self: &Arc<Self>) {
        if self
            .archive_rule_updates_started
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let weak = Arc::downgrade(self);
        let notify = Arc::clone(&self.archive_rule_updates_notify);
        self.runtime.spawn_handle().spawn(async move {
            loop {
                let Some(manager) = weak.upgrade() else {
                    return;
                };
                let identities: Vec<String> = manager
                    .plugins
                    .read()
                    .await
                    .iter()
                    .filter(|plugin| plugin.enabled)
                    .filter_map(|plugin| {
                        plugin
                            .manifest
                            .contributes
                            .as_ref()
                            .and_then(|contributes| contributes.archive_rules.as_ref())
                            .filter(|rules| rules.update_url.is_some())
                            .map(|_| plugin.manifest.identity.clone())
                    })
                    .collect();
                if identities.is_empty() {
                    drop(manager);
                    notify.notified().await;
                    continue;
                }
                for identity in identities {
                    let Some(manager) = weak.upgrade() else {
                        return;
                    };
                    if let Err(error) = manager.archive_rules_status(&identity, false).await {
                        log_info!("[plugin] archive rules update for {identity} failed: {error}");
                    }
                }
                let Some(manager) = weak.upgrade() else {
                    return;
                };
                drop(manager);
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(24 * 60 * 60)) => {},
                    _ = notify.notified() => {},
                }
            }
        });
    }

    /// 宿主并发上限变化时同步 resolve 并发容量（见 [`ScriptRuntime::set_resolve_capacity`]）。
    pub fn set_resolve_capacity(&self, max_concurrent: usize) {
        self.runtime.set_resolve_capacity(max_concurrent);
    }

    /// 扫描根目录 + `plugin.dev.*` 键，解析并加载全部插件。
    pub async fn load_all(&self) {
        let _serial = self.load_lock.lock().await;
        self.migrate_consent().await;
        let mut loaded: Vec<LoadedPlugin> = Vec::new();
        let mut failed: Vec<FailedPlugin> = Vec::new();

        // 1. 安装目录下的子目录。
        if let Ok(mut rd) = tokio::fs::read_dir(&self.root).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                let is_internal_dir = path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'));
                if path.is_dir() && !is_internal_dir {
                    match self.load_one(&path, false, None).await {
                        Ok(p) => loaded.push(p),
                        Err(p) => failed.push(p),
                    }
                }
            }
        }

        // 2. dev 插件（plugin.dev.<identity> = abs path）。
        if let Ok(entries) = self.db.list_config_with_prefix("plugin.dev.").await {
            for (_k, path_str) in entries {
                let path = PathBuf::from(&path_str);
                match self
                    .load_one(&path, true, Some(identity_from_dev_key(&_k)))
                    .await
                {
                    Ok(p) => loaded.push(p),
                    Err(p) => failed.push(p),
                }
            }
        }

        let mut deduped = dedup_plugins_by_identity(loaded, &mut failed);

        // 熔断计数跨 `load_all` 沿用：同版本、前后都启用时共用旧 Arc，否则无关插件的
        // 启停 / 设置变更都会把别人的连续超时清零。重新启用得到全新计数；代码被替换
        // （安装 / 升级 / dev 重载）时由 [`Self::mark_code_replaced`] 清零。
        {
            let previous = self.plugins.read().await.clone();
            for plugin in &mut deduped {
                if !plugin.enabled {
                    continue;
                }
                if let Some(old) = previous.iter().find(|old| {
                    old.enabled
                        && old.manifest.identity == plugin.manifest.identity
                        && old.manifest.version == plugin.manifest.version
                        && old.dev == plugin.dev
                }) {
                    plugin.timeout_streak = old.timeout_streak.clone();
                }
            }
        }

        *self.plugins.write().await = Arc::new(deduped);
        *self.failed_plugins.write().await = Arc::new(failed);
        self.archive_rule_status.write().await.clear();
        // A new declaration must be revalidated before any cached rules can match.
        self.archive_rule_packs.write().await.clear();
        // 宿主对账：被禁用 / 卸载 / manifest 或 main 源码变化的插件，其宿主立即释放。
        self.reconcile_hosts().await;
        self.initialize_archive_rule_packs().await;
        self.archive_rule_updates_notify.notify_one();
    }

    /// 加载单个插件目录。失败记 warn 并保留诊断信息（不阻塞其他插件）。
    async fn load_one(
        &self,
        dir: &Path,
        dev: bool,
        identity_hint: Option<&str>,
    ) -> Result<LoadedPlugin, FailedPlugin> {
        let manifest_path = dir.join("manifest.json");
        let bytes = match tokio::fs::read(&manifest_path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                let failure = FailedPlugin {
                    identity: failure_identity(dir, identity_hint),
                    dir: dir.to_path_buf().into_boxed_path(),
                    manifest: None,
                    dev_mode: dev,
                    error: format!("读取 manifest.json 失败: {error}"),
                };
                log_info!("[plugin] 跳过 {dir:?}: {}", failure.error);
                return Err(failure);
            }
        };
        let manifest = match PluginManifest::parse(&bytes) {
            Ok(manifest) => {
                if let Err(error) = manifest.validate() {
                    log_info!("[plugin] 跳过 {dir:?}: manifest 非法: {error}");
                    return Err(FailedPlugin {
                        identity: manifest.identity.clone(),
                        dir: dir.to_path_buf().into_boxed_path(),
                        manifest: Some(Box::new(manifest)),
                        dev_mode: dev,
                        error: error.to_string(),
                    });
                }
                manifest
            }
            Err(error) => {
                log_info!("[plugin] 跳过 {dir:?}: manifest 非法: {error}");
                return Err(FailedPlugin {
                    identity: failure_identity(dir, identity_hint),
                    dir: dir.to_path_buf().into_boxed_path(),
                    manifest: None,
                    dev_mode: dev,
                    error: error.to_string(),
                });
            }
        };

        // minAppVersion 门槛（app_version 未知或 manifest 未声明时从宽放行）。
        if !manifest.min_app_version.is_empty()
            && !self.app_version.is_empty()
            && !super::semver::satisfies_min(&self.app_version, &manifest.min_app_version)
        {
            let required_version = manifest.min_app_version.clone();
            log_info!(
                "[plugin] 跳过 {}: 需 App ≥ {}，当前 {}",
                manifest.identity,
                required_version,
                self.app_version
            );
            return Err(FailedPlugin {
                identity: manifest.identity.clone(),
                dir: dir.to_path_buf().into_boxed_path(),
                manifest: Some(Box::new(manifest)),
                dev_mode: dev,
                error: format!("需要 App ≥ {}，当前 {}", required_version, self.app_version),
            });
        }

        let resolver_entry = manifest.resolvers.first().map(|r| dir.join(&r.entry));
        let hooks_entry = manifest.hooks.as_ref().map(|h| dir.join(&h.entry));
        let intercept_entry = manifest.interceptors.as_ref().map(|i| dir.join(&i.entry));
        let archive_entry = manifest
            .contributes
            .as_ref()
            .and_then(|c| c.archive.as_ref())
            .map(|p| dir.join(p));
        let auth_entry = manifest.auth.as_ref().map(|a| dir.join(&a.entry));
        let subscription_entry = manifest.subscriptions.first().map(|s| dir.join(&s.entry));

        // main / desktop 入口 / sniffRules 只做存在性检查：main 由宿主按需编译，desktop 入口是
        // GPUI Shell 执行的 ESM，引擎从不编译或求值它。
        let mut present: HashSet<String> = HashSet::new();
        for file in manifest.package_files() {
            let is_file = tokio::fs::metadata(dir.join(&file))
                .await
                .map(|m| m.is_file())
                .unwrap_or(false);
            if is_file {
                present.insert(file);
            }
        }
        let mut file_check = manifest.check_package_files(|p| present.contains(p));
        if file_check.is_ok()
            && let Some(rules) = manifest
                .contributes
                .as_ref()
                .and_then(|c| c.sniff_rules.as_ref())
        {
            file_check = match tokio::fs::read(dir.join(rules)).await {
                Ok(bytes) => super::manifest::validate_sniff_pack(&bytes),
                Err(e) => Err(PluginError::ManifestInvalid(format!(
                    "sniffRules '{rules}' 读取失败: {e}"
                ))),
            };
        }
        if file_check.is_ok()
            && let Some(rules) = manifest
                .contributes
                .as_ref()
                .and_then(|contributes| contributes.archive_rules.as_ref())
        {
            file_check = match super::archive_rules::read_bounded(&dir.join(&rules.entry)).await {
                Ok(bytes) => manifest.validate_archive_rules_content(|path| {
                    (path == rules.entry).then(|| bytes.clone())
                }),
                Err(error) => Err(error),
            };
        }
        if file_check.is_ok()
            && let Some(contributes) = manifest.contributes.as_ref()
        {
            for script in &contributes.page_scripts {
                let mut contents: std::collections::HashMap<&str, Vec<u8>> =
                    std::collections::HashMap::new();
                for file in &script.js {
                    match tokio::fs::read(dir.join(file)).await {
                        Ok(bytes) => {
                            contents.insert(file.as_str(), bytes);
                        }
                        Err(e) => {
                            file_check = Err(PluginError::ManifestInvalid(format!(
                                "pageScript '{}' 读取 '{file}' 失败: {e}",
                                script.id
                            )));
                            break;
                        }
                    }
                }
                if file_check.is_ok() {
                    file_check =
                        super::manifest::concat_page_script(script, |p| contents.get(p).cloned())
                            .map(|_| ());
                }
                if file_check.is_err() {
                    break;
                }
            }
        }
        let mut view_icons = Vec::new();
        if file_check.is_ok()
            && let Some(contributes) = manifest.contributes.as_ref()
        {
            for view in &contributes.views {
                let Some(icon) = &view.icon else { continue };
                file_check = match tokio::fs::read(dir.join(icon)).await {
                    Ok(bytes) => super::manifest::validate_view_icon(&view.id, bytes)
                        .map(|svg| view_icons.push((view.id.clone(), svg))),
                    Err(e) => Err(PluginError::ManifestInvalid(format!(
                        "view '{}' icon '{icon}' 读取失败: {e}",
                        view.id
                    ))),
                };
                if file_check.is_err() {
                    break;
                }
            }
        }
        if let Err(e) = file_check {
            log_info!("[plugin] 跳过 {}: {e}", manifest.identity);
            return Err(FailedPlugin {
                identity: manifest.identity.clone(),
                dir: dir.to_path_buf().into_boxed_path(),
                manifest: Some(Box::new(manifest.clone())),
                dev_mode: dev,
                error: e.to_string(),
            });
        }

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

        // 入口脚本：dev 与非 dev 都在加载期读一遍——缺失或不可读直接判定加载失败并点名
        // 文件，而不是等到调用时才报「未提供 resolver」。非 dev 另存缓存；dev 每次调用重读。
        let fail = |error: String| {
            log_info!("[plugin] 跳过 {}: {error}", manifest.identity);
            FailedPlugin {
                identity: manifest.identity.clone(),
                dir: dir.to_path_buf().into_boxed_path(),
                manifest: Some(Box::new(manifest.clone())),
                dev_mode: dev,
                error,
            }
        };
        let resolver_source = read_entry(dir, manifest.resolvers.first().map(|r| r.entry.as_str()))
            .await
            .map_err(fail)?;
        let hooks_source = read_entry(dir, manifest.hooks.as_ref().map(|h| h.entry.as_str()))
            .await
            .map_err(fail)?;
        let auth_source = read_entry(dir, manifest.auth.as_ref().map(|a| a.entry.as_str()))
            .await
            .map_err(fail)?;
        let subscription_source = read_entry(
            dir,
            manifest.subscriptions.first().map(|s| s.entry.as_str()),
        )
        .await
        .map_err(fail)?;
        let main_source = read_entry(dir, manifest.main.as_ref().map(|m| m.entry.as_str()))
            .await
            .map_err(fail)?;
        let intercept_source = read_entry(
            dir,
            manifest.interceptors.as_ref().map(|i| i.entry.as_str()),
        )
        .await
        .map_err(fail)?;
        let archive_source = read_entry(
            dir,
            manifest
                .contributes
                .as_ref()
                .and_then(|c| c.archive.as_deref()),
        )
        .await
        .map_err(fail)?;
        if let Some(source) = archive_source.as_deref()
            && let Err(error) = self.runtime.check_compile(source)
        {
            return Err(fail(format!("archive 脚本编译失败: {error}")));
        }
        let (
            resolver_cache,
            hooks_cache,
            auth_cache,
            subscription_cache,
            intercept_cache,
            archive_cache,
        ) = if dev {
            (None, None, None, None, None, None)
        } else {
            (
                resolver_source,
                hooks_source,
                auth_source,
                subscription_source,
                intercept_source,
                archive_source,
            )
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
        let mut enabled = enabled_str.as_deref().map(|v| v == "true").unwrap_or(true);
        let mut disabled_reason = disabled_reason;
        // 授权判定：任一待授权项非空 → 视为禁用（不写 enabled 键，授权后直接生效）。
        let consent = self.read_consent(&identity).await;
        let (pending_perms, pending_hosts) = super::consent::pending(&manifest, consent.as_ref());
        if !pending_perms.is_empty() || !pending_hosts.is_empty() {
            enabled = false;
            disabled_reason = DisabledReason::ConsentRequired;
        }

        let main_entry = manifest.main.as_ref().map(|m| dir.join(&m.entry));
        let main_cache = if dev { None } else { main_source };

        Ok(LoadedPlugin {
            manifest,
            dir: dir.to_path_buf(),
            dev,
            enabled,
            disabled_reason,
            resolver_entry,
            hooks_entry,
            intercept_entry,
            archive_entry,
            auth_entry,
            subscription_entry,
            resolver_cache,
            hooks_cache,
            intercept_cache,
            archive_cache,
            auth_cache,
            subscription_cache,
            main_entry,
            main_cache,
            view_icons,
            timeout_streak: Arc::new(AtomicU32::new(0)),
        })
    }

    /// 惰性解析（非任务调用：新建下载预解析、宿主自身下载的加速改写、测试）。off-actor
    /// 调用（在插件专用 runtime 上）。出口按源链接与全局代理设定制定计划（见
    /// [`super::call_egress`]），Auto 下断在网络层时换路重跑。
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
        let plan = self.plan_for(&req.url, "").await;
        self.resolve_planned(identity, req, plan).await.0
    }

    /// 按给定出口计划解析（任务解析由下载管理器结合任务代理 / 备用链路制定计划）。
    /// 返回值附带计划执行结果：换过路的路径与产出结果的一步（下载据此钉定同一出口）。
    /// `plan` 为 `None` 时单次调用、走 bridge 的全局出口。
    pub async fn resolve_planned(
        &self,
        identity: &str,
        mut req: ResolveRequest,
        plan: Option<EgressPlan>,
    ) -> (Result<Option<ResolveResult>, PluginError>, EgressOutcome) {
        let prepared = match self.prepare_resolve(identity, &mut req).await {
            Ok(prepared) => prepared,
            Err(error) => return (Err(error), EgressOutcome::default()),
        };
        let PreparedResolve {
            streak,
            script,
            settings_json,
            budget,
            host,
            second_stage,
        } = prepared;
        let (result, outcome) = self
            .run_planned(plan, identity, true, |egress| {
                let req = req.clone();
                let settings_json = settings_json.clone();
                let host = HostContext {
                    egress: egress.clone(),
                    ..host.clone()
                };
                let script = &script;
                let streak = &streak;
                async move {
                    let result = self
                        .runtime
                        .invoke_resolve(
                            script,
                            req,
                            settings_json,
                            self.bridge.clone(),
                            budget,
                            host,
                        )
                        .await;
                    self.note_execution(identity, streak, result.as_ref().err(), egress.as_deref())
                        .await;
                    result
                }
            })
            .await;
        let result = result.and_then(|result| {
            if let Some(res) = &result {
                validate_resolve_output(res, second_stage)?;
            }
            Ok(result)
        });
        (result, outcome)
    }

    /// resolve 的前置校验与调用参数（插件存在 / 启用 / 有 resolver / 必填设置）。
    async fn prepare_resolve(
        &self,
        identity: &str,
        req: &mut ResolveRequest,
    ) -> Result<PreparedResolve, PluginError> {
        // 二段解析任务（任务组成员 / 插件订阅条目）的原始链接是分享页，「忽略插件重试」
        // 会被拒绝（见 `clear_task_resolver`），提示里不能再推荐它。
        let escape = if req.resolver_item.is_empty() {
            "，或选择「忽略插件重试」按原始链接下载"
        } else {
            ""
        };
        // 从快照克隆所需（避免跨 await 持锁）。
        let (manifest, streak, source, dev_ver, dev_mode) = {
            let snapshot = self.plugins.read().await.clone();
            let Some(p) = snapshot.iter().find(|p| p.manifest.identity == identity) else {
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} 已卸载或加载失败；重新安装插件后重试{escape}"
                )));
            };
            if !p.enabled {
                let reason = p.disabled_reason.describe();
                let remedy = p.disabled_reason.remedy();
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} {reason}；{remedy}{escape}"
                )));
            }
            let source = p.resolver_source().await;
            (
                p.manifest.clone(),
                p.timeout_streak.clone(),
                source,
                p.manifest.version.clone(),
                p.dev,
            )
        };
        let Some(source) = source else {
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 当前版本未提供 resolver（或源码读取失败）；升级或更换插件后重试{escape}"
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
        // 每次 resolve 都把同一插件/站点映射到稳定认证引用。插件无需把 Cookie
        // 放进自己的 KV，也无需在每次调用时重新登录。只对声明了 auth 权限的
        // 插件填充（M-5）：未授权插件本就会被 `bridge::http_request` 拒绝
        // 携带显式 authRef（见 `req.auth_allowed`），缺省注入只会让它们在
        // `ctx.authRef` 里看到一个自己永远用不了的值，与 runtime.rs 文档
        // 承诺的字段语义不符。
        if req.auth_ref.is_empty()
            && manifest.has_permission(PERMISSION_AUTH)
            && let Some(auth_ref) = crate::auth::default_auth_ref(identity, &req.url)
        {
            req.auth_ref = auth_ref;
        }
        // 二段防递归判定须在 req 被 invoke_resolve 消费前捕获。
        let second_stage = !req.resolver_item.is_empty();
        Ok(PreparedResolve {
            streak,
            script,
            settings_json,
            budget,
            host: HostContext {
                // resolve 平面授予 yt-dlp（直链提取的主战场）；ffmpeg 无产物牢笼故不授予。
                ytdlp_permitted: manifest.has_permission(PERMISSION_YTDLP),
                auth_permitted: manifest.has_permission(PERMISSION_AUTH),
                private_network_allowed: dev_mode,
                ..Default::default()
            },
            second_stage,
        })
    }

    /// 熔断计数：连续 Timeout/MemoryLimitExceeded 触发自动禁用。QuickJS 内存
    /// 超限在 JS 侧表现为「out of memory」异常，quickjs.rs 的 reclassify_oom
    /// 已把它归一为 `MemoryLimitExceeded`——OOM 与超时同样计入熔断。超时时仍有
    /// 网络请求在等（被墙 / 慢网络）不是脚本缺陷：既不计数也不清零。
    async fn note_execution(
        &self,
        identity: &str,
        streak: &AtomicU32,
        error: Option<&PluginError>,
        egress: Option<&CallEgress>,
    ) {
        match error {
            Some(PluginError::Timeout)
                if egress.is_some_and(|egress| egress.observer().network_caused_timeout()) =>
            {
                log_info!("[plugin:{identity}] 超时由网络造成（断网 / 仍在等待网络），不计入熔断");
            }
            Some(PluginError::Timeout | PluginError::MemoryLimitExceeded) => {
                let n = streak.fetch_add(1, Ordering::SeqCst) + 1;
                if n >= CIRCUIT_BREAKER_THRESHOLD {
                    self.trip_circuit_breaker(identity).await;
                }
            }
            _ => streak.store(0, Ordering::SeqCst),
        }
    }

    /// 注入全局出口来源（引擎装配时调用；见 [`Self::plan_for`]）。
    pub fn attach_egress_source(&self, bridge: Arc<super::bridge::EngineBridge>) {
        *self
            .egress_source
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bridge);
    }

    /// 非任务调用的出口计划：以 `subject_url` 为源链接、`proxy_url` 为调用自带代理（订阅级
    /// 代理；空串跟随全局）。未注入出口来源时 `None`（单次调用、bridge 全局出口）。
    async fn plan_for(&self, subject_url: &str, proxy_url: &str) -> Option<EgressPlan> {
        let bridge = self
            .egress_source
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let global = bridge.proxy_config();
        let subject = subject_url.to_string();
        let proxy_url = proxy_url.to_string();
        // System 检测 / Auto 系统候选检测可能起子进程：放进阻塞池。
        let planned = tokio::task::spawn_blocking(move || {
            plan_call(
                &global,
                &subject,
                PlanTask {
                    proxy_url: &proxy_url,
                    ..PlanTask::default()
                },
            )
        })
        .await;
        match planned {
            Ok(plan) => Some(plan),
            Err(error) => {
                crate::logger::report_error("plugin", "plan plugin egress", &error);
                None
            }
        }
    }

    /// 按出口计划执行一次插件调用：每步一个 [`CallEgress`]；Auto 步失败且 bridge 观测证实
    /// 断在 fallback 出口的网络层、整机网络可达时，记入 `route_health`（源链接 host）并
    /// 换下一步重跑。其余失败、非 Auto 步、计划耗尽即返回该次结果。`retry_timeouts`
    /// 为 `false` 时墙钟超时不重跑（有副作用的调用，见 [`NetObserver::transport_suspect`]）。
    ///
    /// [`NetObserver::transport_suspect`]: super::call_egress::NetObserver::transport_suspect
    pub(crate) async fn run_planned<T, F, Fut>(
        &self,
        plan: Option<EgressPlan>,
        identity: &str,
        retry_timeouts: bool,
        mut call: F,
    ) -> (Result<T, PluginError>, EgressOutcome)
    where
        F: FnMut(Option<Arc<CallEgress>>) -> Fut,
        Fut: Future<Output = Result<T, PluginError>>,
    {
        let Some(EgressPlan { steps, rules, host }) = plan else {
            return (call(None).await, EgressOutcome::default());
        };
        let mut outcome = EgressOutcome {
            host,
            ..EgressOutcome::default()
        };
        let mut steps = steps.into_iter().peekable();
        while let Some(step) = steps.next() {
            let egress = Arc::new(CallEgress::new(
                step.proxy.clone(),
                step.explicit,
                rules.clone(),
            ));
            let error = match call(Some(Arc::clone(&egress))).await {
                Ok(value) => {
                    outcome.used = Some(step);
                    return (Ok(value), outcome);
                }
                Err(error) => error,
            };
            let Some(route) = step.route else {
                return (Err(error), outcome);
            };
            if !egress.observer().transport_suspect(&error, retry_timeouts)
                || !crate::route_health::network_reachable()
            {
                return (Err(error), outcome);
            }
            if let Some(host) = &outcome.host {
                crate::route_health::record_path_failure(host, route, &self.db);
            }
            outcome.failed.push(route);
            match steps.peek() {
                Some(next) => log_info!(
                    "[plugin:{identity}] 经{}的请求断在网络层（{error}），改经{}重试",
                    route_name(Some(route)),
                    route_name(next.route)
                ),
                None => return (Err(with_tried_routes(error, &outcome.failed)), outcome),
            }
        }
        (call(None).await, outcome)
    }

    /// 执行插件平台登录入口。登录状态由插件通过 `flux.auth.save` 写入宿主，
    /// 本方法只负责驱动 begin/poll/cancel/logout/status 并把挑战返回给 UI。
    ///
    /// `logout` 是唯一在插件被禁用时也放行的 action（M-3）：禁用插件（手动
    /// 关闭或被熔断）仍可能留有 Cookie/Bearer 等登录态，用户必须能清掉它，
    /// 且不需要为此临时重新启用一个已知有问题的插件去跑它的 JS。这种情况下
    /// 完全不进 JS（禁用插件不该被驱动执行任意脚本），只由宿主兜底删除凭据。
    pub async fn authenticate(
        &self,
        identity: &str,
        mut req: AuthRequest,
    ) -> Result<AuthResult, PluginError> {
        if !matches!(
            req.action.as_str(),
            "begin" | "poll" | "cancel" | "logout" | "status"
        ) {
            return Err(PluginError::InvalidOutput(
                "auth action 必须是 begin/poll/cancel/logout/status".to_string(),
            ));
        }

        let (manifest, source, version, enabled, dev_mode) = {
            let snapshot = self.plugins.read().await.clone();
            let Some(plugin) = snapshot.iter().find(|p| p.manifest.identity == identity) else {
                return Err(PluginError::Runtime(format!("插件 {identity} 不存在")));
            };
            if !plugin.manifest.has_permission(PERMISSION_AUTH) {
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} 未声明 auth 权限"
                )));
            }
            let source = plugin.auth_source().await;
            (
                plugin.manifest.clone(),
                source,
                plugin.manifest.version.clone(),
                plugin.enabled,
                plugin.dev,
            )
        };

        if req.auth_ref.is_empty()
            && !req.site.trim().is_empty()
            && let Some(site) = crate::auth::normalize_site(&req.site)
        {
            req.auth_ref = format!("{identity}::{site}");
        }

        if !enabled {
            if req.action != "logout" {
                return Err(PluginError::Runtime(format!("插件 {identity} 未启用")));
            }
            if req.auth_ref.is_empty() {
                return Err(PluginError::InvalidOutput(
                    "logout 需要 authRef 或 site".to_string(),
                ));
            }
            self.bridge.auth_remove(identity, &req.auth_ref).await?;
            return Ok(AuthResult {
                status: "success".to_string(),
                auth_ref: Some(req.auth_ref),
                ..Default::default()
            });
        }
        let Some(source) = source else {
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 未提供 auth 入口"
            )));
        };

        let values = self.load_setting_values(identity).await;
        for field in &manifest.settings {
            if field.required && value_of(&values, field).is_none() {
                return Err(PluginError::MissingRequiredSetting(format!(
                    "插件 {identity} 需先配置「{}」",
                    field.title
                )));
            }
        }

        let script = PluginScript {
            identity: identity.to_string(),
            source,
            entry_fn_hint: PluginEntryKind::Auth,
            version,
            app_version: self.app_version.clone(),
        };
        let action = req.action.clone();
        let requested_auth_ref = req.auth_ref.clone();
        // 登录流程的出口以站点为源链接：与该站点的解析 / 下载同一套规则与 Auto 换路。
        let subject = crate::auth::normalize_site(&req.site)
            .map(|site| format!("{site}/"))
            .unwrap_or_default();
        let plan = self.plan_for(&subject, "").await;
        let settings_json = build_typed_settings_json(&manifest, &values);
        let budget = self.auth_budget_for(&manifest);
        let script = &script;
        let (invocation, _) = self
            .run_planned(plan, identity, false, |egress| {
                let req = req.clone();
                let settings_json = settings_json.clone();
                async move {
                    self.runtime
                        .invoke_auth(
                            script,
                            req,
                            settings_json,
                            self.bridge.clone(),
                            budget,
                            HostContext {
                                auth_permitted: true,
                                private_network_allowed: dev_mode,
                                egress,
                                ..Default::default()
                            },
                        )
                        .await
                }
            })
            .await;
        if action == "logout" && !requested_auth_ref.is_empty() {
            self.bridge
                .auth_remove(identity, &requested_auth_ref)
                .await?;
        }
        let mut result = invocation?;
        if result.status == "success" && result.auth_ref.is_none() && !requested_auth_ref.is_empty()
        {
            result.auth_ref = Some(requested_auth_ref);
        }
        Ok(result)
    }

    /// 构造动态插件订阅路由；插件安装、启停或卸载后无需重建该路由。
    pub fn subscription_router(self: &Arc<Self>) -> Arc<dyn SubscriptionProvider> {
        Arc::new(PluginSubscriptionRouter {
            manager: self.clone(),
        })
    }

    /// 调用一个插件订阅 provider，并把返回值规范化为引擎条目。
    pub async fn fetch_subscription(
        &self,
        provider_id: &str,
        request: SubscriptionFetchRequest,
    ) -> Result<ParsedFeed, PluginError> {
        let (manifest, streak, source, version, identity, dev_mode) = {
            let snapshot = self.plugins.read().await.clone();
            let Some(p) = snapshot
                .iter()
                .filter(|p| {
                    p.enabled
                        && p.manifest
                            .subscriptions
                            .first()
                            .is_some_and(|s| s.provider_id == provider_id)
                })
                .min_by(|a, b| a.manifest.identity.cmp(&b.manifest.identity))
            else {
                return Err(PluginError::Runtime(format!(
                    "订阅 provider {provider_id} 未安装或已禁用"
                )));
            };
            let source = p.subscription_source().await;
            (
                p.manifest.clone(),
                p.timeout_streak.clone(),
                source,
                p.manifest.version.clone(),
                p.manifest.identity.clone(),
                p.dev,
            )
        };
        let Some(source) = source else {
            return Err(PluginError::Runtime(format!(
                "订阅 provider {provider_id} 的脚本读取失败"
            )));
        };

        let values = self.load_setting_values(&identity).await;
        for field in &manifest.settings {
            if field.required && value_of(&values, field).is_none() {
                return Err(PluginError::MissingRequiredSetting(format!(
                    "插件 {identity} 需先配置「{}」",
                    field.title
                )));
            }
        }

        manifest
            .subscriptions
            .first()
            .ok_or_else(|| PluginError::Runtime("订阅 provider 声明缺失".to_string()))?;
        let budget = self.subscription_budget_for(&manifest);
        let script = PluginScript {
            identity: identity.clone(),
            source,
            entry_fn_hint: PluginEntryKind::Subscription,
            version,
            app_version: self.app_version.clone(),
        };
        // 订阅出口：订阅自带代理（与任务自带代理同语义）> 订阅链接命中的站点规则 > 全局模式
        // （Auto 断在网络层时换路重跑）。
        let plan = self.plan_for(&request.url, &request.proxy_url).await;
        let subscription = SubscriptionRequest {
            provider_id: provider_id.to_string(),
            source_id: request.source_id,
            url: request.url,
            provider_config: if request.provider_config.trim().is_empty() {
                "{}".to_string()
            } else {
                request.provider_config
            },
            cookies: request.cookies,
            user_agent: request.user_agent,
        };
        let settings_json = build_typed_settings_json(&manifest, &values);
        let host = HostContext {
            ytdlp_permitted: manifest.has_permission(PERMISSION_YTDLP),
            auth_permitted: manifest.has_permission(PERMISSION_AUTH),
            private_network_allowed: dev_mode,
            ..Default::default()
        };
        let (script, streak, identity_ref) = (&script, &streak, identity.as_str());
        let (result, _) = self
            .run_planned(plan, identity_ref, true, |egress| {
                let subscription = subscription.clone();
                let settings_json = settings_json.clone();
                let host = HostContext {
                    egress: egress.clone(),
                    ..host.clone()
                };
                async move {
                    let result = self
                        .runtime
                        .invoke_subscription(
                            script,
                            subscription,
                            settings_json,
                            self.bridge.clone(),
                            budget,
                            host,
                        )
                        .await;
                    self.note_execution(
                        identity_ref,
                        streak,
                        result.as_ref().err(),
                        egress.as_deref(),
                    )
                    .await;
                    result
                }
            })
            .await;

        let raw = result?;
        parse_subscription_output(&raw)
    }

    /// 通知平面：遍历声明该事件且 match 命中的启用插件，逐个在插件 runtime 上 spawn
    /// invoke_hook。**全部 fire-and-forget，本函数立即返回**。
    pub async fn notify(self: &Arc<Self>, event: PluginEvent) {
        let snapshot = self.plugins.read().await.clone();
        let ev_name = event.declared_name();
        let url = event.url().to_string();
        for idx in 0..snapshot.len() {
            let p = &snapshot[idx];
            if !p.enabled {
                continue;
            }
            // 常驻宿主：事件声明在 `main.events` 的插件，由宿主处理（必要时激活）。
            if p.manifest
                .main
                .as_ref()
                .is_some_and(|m| m.events.iter().any(|e| e == ev_name))
            {
                self.deliver_host_event(&p.manifest.identity, event.clone());
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
            let auth_permitted = p.manifest.has_permission(PERMISSION_AUTH);
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
                auth_permitted,
                private_network_allowed: p.dev,
                // 通知钩子 fire-and-forget、不重跑：按全局规则 + 全局模式选出口。
                egress: None,
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

    /// resolve 预算：manifest `timeoutMs` 可下调，30s 硬顶；`memoryLimitMb` 替换默认
    /// 堆上限，[`RESOLVE_MEMORY_CEILING`] 硬顶。
    fn resolve_budget_for(&self, manifest: &PluginManifest) -> ExecutionBudget {
        let resolver = manifest.resolvers.first();
        let timeout = resolver
            .and_then(|r| r.timeout_ms)
            .map(Duration::from_millis)
            .unwrap_or(self.resolve_budget.timeout)
            .min(HARD_TIMEOUT_CEILING);
        let memory_limit_bytes = resolver
            .and_then(|r| r.memory_limit_mb)
            .map(|mb| usize::try_from(mb).map_or(usize::MAX, |mb| mb.saturating_mul(1024 * 1024)))
            .unwrap_or(self.resolve_budget.memory_limit_bytes)
            .min(RESOLVE_MEMORY_CEILING);
        ExecutionBudget {
            timeout,
            memory_limit_bytes,
        }
    }

    /// auth 预算：manifest `auth.timeoutMs` 可下调，默认 [`DEFAULT_AUTH_TIMEOUT`]，
    /// 30s 硬顶。与 [`Self::resolve_budget_for`] 完全独立——不读 resolver 的
    /// timeoutMs（M-2）。
    fn auth_budget_for(&self, manifest: &PluginManifest) -> ExecutionBudget {
        let timeout = manifest
            .auth
            .as_ref()
            .and_then(|a| a.timeout_ms)
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_AUTH_TIMEOUT)
            .min(HARD_TIMEOUT_CEILING);
        ExecutionBudget {
            timeout,
            memory_limit_bytes: self.resolve_budget.memory_limit_bytes,
        }
    }

    /// subscription 预算：manifest 可下调默认 resolve 预算。
    fn subscription_budget_for(&self, manifest: &PluginManifest) -> ExecutionBudget {
        let timeout = manifest
            .subscriptions
            .first()
            .and_then(|s| s.timeout_ms)
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
        let outcome = super::install::install_from_zip_with_backup(&self.root, &bytes, |_| Ok(()))?;
        self.finish_install_outcome(outcome).await
    }

    /// 与 [`Self::install_from_zip`] 相同，但在 manifest 校验通过后、替换旧版本前
    /// 先运行 `check_manifest`（市场安装用它核对包内 manifest 与索引条目一致）。
    pub(crate) async fn install_from_zip_checked(
        &self,
        bytes: Vec<u8>,
        check_manifest: impl FnOnce(&super::manifest::PluginManifest) -> Result<(), PluginError>,
    ) -> Result<String, PluginError> {
        let outcome =
            super::install::install_from_zip_with_backup(&self.root, &bytes, check_manifest)?;
        self.finish_install_outcome(outcome).await
    }

    /// 从目录安装（不剥壳，path 须直接含 manifest.json）。
    pub async fn install_from_dir(&self, path: &Path) -> Result<String, PluginError> {
        let outcome = super::install::install_from_dir_with_backup(&self.root, path)?;
        self.finish_install_outcome(outcome).await
    }

    /// dev 安装（写 plugin.dev.<identity>=abs(path)，不拷贝）。
    pub async fn install_dev(&self, path: &Path) -> Result<String, PluginError> {
        let result = self.install_dev_inner(path).await;
        self.publish_state().await;
        result
    }

    async fn install_dev_inner(&self, path: &Path) -> Result<String, PluginError> {
        let manifest_path = path.join("manifest.json");
        let bytes = std::fs::read(&manifest_path)
            .map_err(|e| PluginError::ManifestInvalid(format!("读取 manifest 失败: {e}")))?;
        let manifest = PluginManifest::parse(&bytes)?;
        manifest.validate()?;
        let identity = manifest.identity.clone();
        let abs = std::fs::canonicalize(path)
            .map_err(|e| PluginError::ManifestInvalid(format!("解析路径失败: {e}")))?;
        let dev_key = format!("plugin.dev.{identity}");
        // 失败只回退本次写入的 dev 登记与启停状态；同 identity 的已安装目录与
        // `plugin.<id>.*` 设置/KV 不属于这次 dev 安装，绝不能清。
        let previous = self
            .db
            .get_config(&dev_key)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        let prior = self.capture_install_state(&identity).await?;
        // 启停规则先于 dev 登记落盘：dev 副本加载时已带着最终启停状态。
        let activated = match self
            .apply_install_enable_rule(&identity, &manifest.version, &prior)
            .await
        {
            Ok(()) => match self.db.set_config(&dev_key, &abs.to_string_lossy()).await {
                Ok(()) => self.activate_dev_install(&identity).await,
                Err(e) => Err(PluginError::Runtime(e.to_string())),
            },
            Err(error) => Err(error),
        };
        if let Err(error) = activated {
            let rollback = match previous {
                Some(old) => self.db.set_config(&dev_key, &old).await,
                None => self.db.delete_config(&dev_key).await,
            };
            if let Err(rollback_error) = rollback {
                crate::logger::report_error("plugin", "restore_dev_registration", &rollback_error);
            }
            self.restore_install_state(&identity, &prior).await;
            self.load_all().await;
            return Err(error);
        }
        self.mark_code_replaced(&identity).await;
        self.audit_event(&identity, "install", manifest.version.clone(), "dev");
        Ok(identity)
    }

    /// dev 安装的加载 + 校验：dev 副本必须真正加载成功（否则同 identity 的已安装
    /// 副本会被顶替/保留，而 dev 版未生效却报成功）。
    async fn activate_dev_install(&self, identity: &str) -> Result<(), PluginError> {
        self.load_all().await;
        if let Some(error) = self.dev_load_failure(identity).await {
            return Err(error);
        }
        self.validate_loaded(identity).await
    }

    /// 本 identity 的 dev 登记对应的加载失败（忽略被 dev 顶替的已安装副本那条
    /// identity 重复记录）。
    async fn dev_load_failure(&self, identity: &str) -> Option<PluginError> {
        self.failed_plugins
            .read()
            .await
            .iter()
            .find(|p| p.identity == identity && p.dev_mode)
            .map(|p| PluginError::LoadFailed(p.error.clone()))
    }

    /// 重新加载一个 dev 插件：重读 manifest 与全部入口源码并做与安装相同的
    /// compile / pattern 校验。
    ///
    /// dev 插件的 JS 入口每次调用都按最新源码重读，但 manifest（名称 / 版本 /
    /// 匹配规则 / 设置项 / 入口路径 / 权限）只在 [`Self::load_all`] 时解析，
    /// 改了它就必须显式重载。与 [`Self::install_dev`] 不同：校验失败**保留**
    /// dev 登记与启停状态（开发中的半成品不该被清掉），只把错误回报给调用方；
    /// 之前加载失败的插件修好后经此恢复。
    pub async fn reload_dev(&self, identity: &str) -> Result<(), PluginError> {
        let result = self.reload_dev_inner(identity).await;
        self.publish_state().await;
        result
    }

    async fn reload_dev_inner(&self, identity: &str) -> Result<(), PluginError> {
        let registered = self
            .db
            .get_config(&format!("plugin.dev.{identity}"))
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?
            .is_some();
        if !registered {
            return Err(PluginError::NotDevPlugin(identity.to_string()));
        }
        self.load_all().await;
        if let Some(error) = self.dev_load_failure(identity).await {
            return Err(error);
        }
        self.validate_loaded(identity).await?;
        // 仅成功的重载替换了生效代码，失败不递增修订号。
        self.mark_code_replaced(identity).await;
        Ok(())
    }

    async fn finish_install_outcome(
        &self,
        outcome: super::install::InstallOutcome,
    ) -> Result<String, PluginError> {
        let result = self.finish_install_outcome_inner(outcome).await;
        self.publish_state().await;
        result
    }

    async fn finish_install_outcome_inner(
        &self,
        outcome: super::install::InstallOutcome,
    ) -> Result<String, PluginError> {
        let identity = outcome.identity().to_string();
        let previous_version = self.installed_version(&identity).await;
        let prepared = match installed_manifest_version(&self.root.join(&identity)).await {
            Ok(version) => self
                .capture_install_state(&identity)
                .await
                .map(|prior| (version, prior)),
            Err(error) => Err(error),
        };
        let (new_version, prior) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.rollback_install_outcome(&outcome).await;
                return Err(error);
            }
        };
        if let Err(error) = self.finish_install(&identity, &new_version, &prior).await {
            // 升级失败恢复旧目录；新装失败只删本次新建的目录。设置/KV 与同 identity
            // 的 dev 登记不是这次安装写的，绝不能清；启停相关键恢复到安装前的值。
            self.restore_install_state(&identity, &prior).await;
            self.rollback_install_outcome(&outcome).await;
            return Err(error);
        }
        if let Err(error) = super::install::commit_install(&outcome) {
            self.restore_install_state(&identity, &prior).await;
            self.rollback_install_outcome(&outcome).await;
            return Err(error);
        }
        self.mark_code_replaced(&identity).await;
        let new_version = self.installed_version(&identity).await.unwrap_or_default();
        match previous_version {
            Some(previous) if previous != new_version => self.audit_event(
                &identity,
                "update",
                new_version.clone(),
                format!("{previous} -> {new_version}"),
            ),
            _ => self.audit_event(&identity, "install", new_version, ""),
        }
        Ok(identity)
    }

    /// 回滚安装目录并重载，使内存态回到安装前。失败的安装没有替换生效代码，修订号不变。
    async fn rollback_install_outcome(&self, outcome: &super::install::InstallOutcome) {
        if let Err(rollback_error) = super::install::rollback_install(outcome) {
            crate::logger::report_error("plugin", "rollback_install", &rollback_error);
        }
        self.load_all().await;
    }

    /// 读取安装前的启停相关 config 键，供安装失败时原样恢复。
    async fn capture_install_state(&self, identity: &str) -> Result<InstallState, PluginError> {
        let get = |suffix: &'static str| {
            let key = format!("plugin.{identity}.{suffix}");
            async move {
                self.db
                    .get_config(&key)
                    .await
                    .map_err(|e| PluginError::Runtime(e.to_string()))
            }
        };
        Ok(InstallState {
            enabled: get("enabled").await?,
            disabled_reason: get("disabled_reason").await?,
            revoked_version: get("revoked_version").await?,
        })
    }

    /// 把 [`Self::capture_install_state`] 抓到的值写回（缺失的键删除）。
    async fn restore_install_state(&self, identity: &str, prior: &InstallState) {
        for (suffix, value) in [
            ("enabled", &prior.enabled),
            ("disabled_reason", &prior.disabled_reason),
            ("revoked_version", &prior.revoked_version),
        ] {
            let key = format!("plugin.{identity}.{suffix}");
            let result = match value {
                Some(value) => self.db.set_config(&key, value).await,
                None => self.db.delete_config(&key).await,
            };
            if let Err(error) = result {
                crate::logger::report_error("plugin", "restore_install_state", &error);
            }
        }
    }

    /// 安装共同后续：enabled 规则（按新包版本，先于加载写入）+ 整表重载 + compile 校验。
    ///
    /// 启停状态在新代码加载之前就已落盘，因此不存在「新代码已加载、却带着旧的
    /// 熔断 / 撤回禁用状态」的中间态。校验失败由调用方恢复 `prior` 并回滚目录。
    async fn finish_install(
        &self,
        identity: &str,
        new_version: &str,
        prior: &InstallState,
    ) -> Result<(), PluginError> {
        self.apply_install_enable_rule(identity, new_version, prior)
            .await?;
        // 整表重载后做 compile / pattern 校验；失败由调用方回滚。
        self.load_all().await;
        self.validate_loaded(identity).await
    }

    /// 安装（含 dev 安装）的启停写入规则，必须在加载新代码之前调用：
    /// - 新装（无 enabled 键）或熔断 → enabled=1, reason=None（升级即解熔断）
    /// - 撤回且新版本不同于被撤回版本 → 同上并清除 revoked_version
    /// - Manual → 维持 disabled 不动（不覆盖用户主动关闭）
    async fn apply_install_enable_rule(
        &self,
        identity: &str,
        new_version: &str,
        prior: &InstallState,
    ) -> Result<(), PluginError> {
        let reason = prior
            .disabled_reason
            .as_deref()
            .map(DisabledReason::parse)
            .unwrap_or(DisabledReason::None);
        let revoked_stale = reason == DisabledReason::Revoked
            && prior.revoked_version.as_deref() != Some(new_version);
        if prior.enabled.is_none() || reason == DisabledReason::CircuitBreaker || revoked_stale {
            self.write_enabled(identity, true, DisabledReason::None)
                .await?;
            self.db
                .delete_config(&format!("plugin.{identity}.revoked_version"))
                .await
                .map_err(|e| PluginError::Runtime(e.to_string()))?;
        }
        Ok(())
    }

    /// 对当前快照中的插件做 compile / pattern 校验（不改任何状态）。
    async fn validate_loaded(&self, identity: &str) -> Result<(), PluginError> {
        let snapshot = self.plugins.read().await.clone();
        let Some(p) = snapshot.iter().find(|p| p.manifest.identity == identity) else {
            return Err(PluginError::ManifestInvalid(
                "安装后未能加载插件（校验失败或版本门槛不满足）".to_string(),
            ));
        };
        for src in [
            p.resolver_source().await,
            p.hooks_source().await,
            p.auth_source().await,
            p.subscription_source().await,
            p.intercept_source().await,
            p.archive_source().await,
        ]
        .into_iter()
        .flatten()
        {
            self.runtime.check_compile(&src)?;
        }
        for f in &p.manifest.settings {
            if let Some(pat) = &f.pattern
                && !self.runtime.regex_valid(pat)
            {
                return Err(PluginError::ManifestInvalid(format!(
                    "setting '{}': pattern 非法（JS RegExp 编译失败）",
                    f.key
                )));
            }
        }
        Ok(())
    }

    /// 卸载（用户主动）：删目录 + 清 config 键 + 清任务 resolver 绑定 + 清认证凭据。
    ///
    /// 清绑定 = 对受影响任务批量应用「忽略插件、按原始链接重跑」逃生舱；不清则
    /// 留下 orphaned 绑定，resume 走 fail-closed 报错（见 [`Self::resolve`]）。
    /// 凭据清理只挂在这里（用户主动卸载），不挂在 [`Self::purge`] 本身（M-4）。
    /// 安装失败的回滚不走 purge（只回退本次写入的目录/dev 登记），避免误删同
    /// identity 已有插件的目录、设置与凭据。
    pub async fn uninstall(&self, identity: &str) -> Result<(), PluginError> {
        let result = self.uninstall_inner(identity).await;
        self.publish_state().await;
        result
    }

    async fn uninstall_inner(&self, identity: &str) -> Result<(), PluginError> {
        let uninstalled_version = self.installed_version(identity).await.unwrap_or_default();
        self.db
            .clear_tasks_resolver(identity)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.purge(identity).await?;
        // purge 的 `load_all` 之后快照已不含该插件，路由不会再选中它；但第一次清理与
        // 快照替换之间创建的任务可能已绑定到它，这里补清一次使绑定闭合。
        // `resolver_choice_pending=1` 的任务仍被 `clear_tasks_resolver` 跳过：它们尚未
        // 绑定，等待用户从剩余候选里选择，而候选以当前快照为准，已卸载者不再出现。
        self.db
            .clear_tasks_resolver(identity)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        super::page_scripts::purge_identity(identity).await;
        super::sniff_pack::purge_identity(identity).await;
        if let Err(error) = self.forget_site_defaults_of(identity).await {
            // 与凭据清理同理：插件本体已卸载完成；残留记录选中它时会在下次匹配时失效重问。
            log_info!("[plugin] 整理插件 {identity} 的站点默认插件记录失败（已忽略）: {error:#}");
        }
        self.bridge.remove_plugin_workspace(identity).await;
        if let Err(e) = crate::auth::remove_plugin(&self.db, identity).await {
            // 只记日志不 `?`：目录/配置键已经清干净，卸载本身已经完成；凭据
            // 清理失败（如旧版整表损坏——已由 auth::read_legacy_table 兜底，
            // 这里只剩真正的 db I/O 错误）不该让用户看到卸载报错回滚假象。
            log_info!("[plugin] 清理插件 {identity} 认证凭据失败（已忽略）: {e:#}");
        }
        self.audit_event(identity, "uninstall", uninstalled_version, "");
        Ok(())
    }

    /// 删目录 + 清 `plugin.<identity>.` 前缀全部 config 键 + 重载。仅供
    /// [`Self::uninstall`] 使用。
    ///
    /// 安装目录扫描只认 manifest 的 identity、不认目录名，同一 identity 可能有多个
    /// 安装副本（`plugins/<identity>` 之外还有手动解压、目录名不同的副本）。这里删除
    /// 全部非 dev 副本；只删一个会让重载后剩下的副本重新出现、卸载却报成功。
    async fn purge(&self, identity: &str) -> Result<(), PluginError> {
        // dev 不删源，仅删配置键；dev 登记若恰好指向安装根目录内，扫描会把它当作
        // 安装副本一并删除（与被 dev 顶替的安装副本同一语义）。
        let dirs = self.installed_copies_of(identity).await;
        if dirs.is_empty() && !is_valid_identity(identity) {
            return Err(PluginError::ManifestInvalid(
                "插件 identity 非法，拒绝拼接卸载路径".to_string(),
            ));
        }
        for dir in &dirs {
            if let Err(error) = tokio::fs::remove_dir_all(dir).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                // 删除目录可能已部分成功，返回原错误前重建可执行快照。
                self.load_all().await;
                return Err(PluginError::Runtime(format!(
                    "删除插件目录 {} 失败: {error}",
                    dir.display()
                )));
            }
        }
        let result = self.purge_config(identity).await;
        // 配置枚举/删除失败也必须重载：安装目录已删除，不能继续消费旧 JS 缓存。
        // 重载只负责收尾，卸载仍返回配置清理的原始错误。
        self.load_all().await;
        result?;
        // 仍有同 identity 的副本（如另一条 dev 登记的 manifest 也声明了它）时如实报错，
        // 不让 UI 显示「已卸载」而列表里还留着它。
        let residual = self.installed_copies_of(identity).await;
        let dev_residual = self
            .plugins
            .read()
            .await
            .iter()
            .any(|p| p.dev && p.manifest.identity == identity)
            || self
                .failed_plugins
                .read()
                .await
                .iter()
                .any(|p| p.dev_mode && p.identity == identity);
        if !residual.is_empty() || dev_residual {
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 卸载后仍有残留副本，请检查插件目录与开发模式登记"
            )));
        }
        Ok(())
    }

    /// 该 identity 当前存在的全部安装副本目录（已加载 / 失败 / 被顶替的非 dev 副本，
    /// 外加约定位置 `plugins/<identity>`），去重。
    async fn installed_copies_of(&self, identity: &str) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut push = |dir: PathBuf| {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        };
        for plugin in self.plugins.read().await.iter() {
            if !plugin.dev && plugin.manifest.identity == identity {
                push(plugin.dir.clone());
            }
        }
        for plugin in self.failed_plugins.read().await.iter() {
            if !plugin.dev_mode && plugin.identity == identity {
                push(plugin.dir.to_path_buf());
            }
        }
        if is_valid_identity(identity) {
            let conventional = self.root.join(identity);
            if conventional.exists() {
                push(conventional);
            }
        }
        dirs
    }

    async fn purge_config(&self, identity: &str) -> Result<(), PluginError> {
        // config 键清理：identity 不合法时禁止前缀扫描（671#1）。失败插件的
        // identity 可能来自未通过 validate() 的 manifest，或干脆是扫到的目录
        // 名（见 `failure_identity`/`FailedPlugin::identity` 文档）——例如插件
        // 开发者常在插件根目录放一个无 manifest.json 的 "dev" 工作区目录，会
        // 被当成失败插件、identity 就是字面 "dev"；若仍按 `plugin.{identity}.`
        // 前缀扫描删除，会精确撞上 `load_all` 用来枚举 dev 插件注册的保留前缀
        // `plugin.dev.`，把全部 dev 模式插件注册一次性清空。identity 不合法时
        // 改走精确键删除：只清这个 identity 自己能推导出的固定几个键（有效
        // identity 的插件从不会有已知设置遗留在失败态——`update_settings` 只
        // 认 `self.plugins` 里的已加载插件，不合法 identity 不可能有 setting
        // 键要清）。
        if is_valid_identity(identity) {
            for prefix in [
                format!("plugin.{identity}."),
                format!("plugin.dev.{identity}"),
            ] {
                let entries = self
                    .db
                    .list_config_with_prefix(&prefix)
                    .await
                    .map_err(|e| PluginError::Runtime(e.to_string()))?;

                for (k, _) in entries {
                    // 认证凭据（`plugin.<id>.auth.<site>`）虽然共享这个前缀
                    // 命名空间，但只能由用户主动 uninstall 清（M-4）——
                    // purge 是安装回滚复用路径，不该连带删掉已登录成功的
                    // 凭据。
                    if crate::auth::is_sensitive_config_key(&k) {
                        continue;
                    }
                    self.db
                        .delete_config(&k)
                        .await
                        .map_err(|e| PluginError::Runtime(e.to_string()))?;
                }

                // plugin.dev.<id> 是精确键（无尾点），单独删。
                self.db
                    .delete_config(&prefix)
                    .await
                    .map_err(|e| PluginError::Runtime(e.to_string()))?;
            }
        } else {
            for key in [
                format!("plugin.{identity}.enabled"),
                format!("plugin.{identity}.disabled_reason"),
                format!("plugin.dev.{identity}"),
            ] {
                self.db
                    .delete_config(&key)
                    .await
                    .map_err(|e| PluginError::Runtime(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// 手动开关（写 enabled + disabled_reason）。
    pub async fn set_enabled(&self, identity: &str, enabled: bool) -> Result<(), PluginError> {
        if enabled {
            let pending = {
                let snapshot = self.plugins.read().await.clone();
                match snapshot.iter().find(|p| p.manifest.identity == identity) {
                    Some(p) => self.pending_for(&p.manifest).await,
                    None => (Vec::new(), Vec::new()),
                }
            };
            if !pending.0.is_empty() || !pending.1.is_empty() {
                return Err(PluginError::ManifestInvalid("consent required".to_string()));
            }
            let revoked = self
                .db
                .get_config(&format!("plugin.{identity}.revoked_version"))
                .await
                .map_err(|e| PluginError::Runtime(e.to_string()))?;
            if let Some(revoked) = revoked
                && self.installed_version(identity).await.as_deref() == Some(revoked.as_str())
            {
                return Err(PluginError::Runtime(
                    "该插件版本已被插件市场撤回，请升级后再启用".into(),
                ));
            }
        }
        let reason = if enabled {
            DisabledReason::None
        } else {
            DisabledReason::Manual
        };
        self.write_enabled(identity, enabled, reason).await?;
        self.load_all().await;
        self.audit_event(
            identity,
            if enabled { "enable" } else { "disable" },
            "",
            "manual",
        );
        self.publish_state().await;
        Ok(())
    }

    /// 当前已装版本：已加载插件优先，其次是带 manifest 的失败插件。
    async fn installed_version(&self, identity: &str) -> Option<String> {
        let loaded = self.plugins.read().await.clone();
        if let Some(p) = loaded.iter().find(|p| p.manifest.identity == identity) {
            return Some(p.manifest.version.clone());
        }
        let failed = self.failed_plugins.read().await.clone();
        failed
            .iter()
            .find(|p| p.identity == identity)
            .and_then(|p| p.manifest.as_ref().map(|m| m.version.clone()))
    }

    /// 按市场索引撤回已装插件：(identity, version) 命中即禁用（Revoked）并记录 revoked_version；
    /// 已记录但不再命中的只清除 revoked_version（保持禁用，由用户手动启用）。返回是否有变化。
    pub async fn apply_market_revocations(
        &self,
        revoked: &std::collections::HashSet<(String, String)>,
    ) -> Result<bool, PluginError> {
        let loaded = self.plugins.read().await.clone();
        let failed = self.failed_plugins.read().await.clone();
        let mut targets: Vec<(String, String)> = Vec::new();
        for p in loaded.iter().filter(|p| !p.dev) {
            targets.push((p.manifest.identity.clone(), p.manifest.version.clone()));
        }
        for p in failed.iter().filter(|p| !p.dev_mode) {
            if let Some(manifest) = &p.manifest {
                targets.push((p.identity.clone(), manifest.version.clone()));
            }
        }

        let mut newly_revoked: Vec<(String, String)> = Vec::new();
        let mut changed = false;
        for (identity, version) in targets {
            let key = format!("plugin.{identity}.revoked_version");
            let recorded = self
                .db
                .get_config(&key)
                .await
                .map_err(|e| PluginError::Runtime(e.to_string()))?;
            if revoked.contains(&(identity.clone(), version.clone())) {
                let (enabled, reason) = self.plugin_state(&identity).await;
                if !enabled
                    && reason == DisabledReason::Revoked
                    && recorded.as_deref() == Some(version.as_str())
                {
                    continue;
                }
                let values = std::collections::BTreeMap::from([
                    (format!("plugin.{identity}.enabled"), "false".to_string()),
                    (
                        format!("plugin.{identity}.disabled_reason"),
                        DisabledReason::Revoked.as_str().to_string(),
                    ),
                    (key, version.clone()),
                ]);
                self.db
                    .set_config_batch_atomic(&values)
                    .await
                    .map_err(|e| PluginError::Runtime(e.to_string()))?;
                newly_revoked.push((identity, version));
                changed = true;
            } else if recorded.is_some() {
                self.db
                    .delete_config(&key)
                    .await
                    .map_err(|e| PluginError::Runtime(e.to_string()))?;
                changed = true;
            }
        }

        if changed {
            self.load_all().await;
            self.publish_state().await;
        }
        for (identity, version) in newly_revoked {
            self.audit_event(&identity, "revoked", version, "market revocation");
            self.sink.emit(EngineEvent::PluginAutoDisabled {
                identity: identity.clone(),
                reason: DisabledReason::Revoked.as_str().to_string(),
            });
            log_info!("[plugin] {identity} 已被插件市场撤回，已自动禁用");
        }
        Ok(changed)
    }

    async fn write_enabled(
        &self,
        identity: &str,
        enabled: bool,
        reason: DisabledReason,
    ) -> Result<(), PluginError> {
        let values = std::collections::BTreeMap::from([
            (format!("plugin.{identity}.enabled"), enabled.to_string()),
            (
                format!("plugin.{identity}.disabled_reason"),
                reason.as_str().to_string(),
            ),
        ]);
        self.db
            .set_config_batch_atomic(&values)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))
    }

    /// 熔断：自动禁用 + emit 事件。
    async fn trip_circuit_breaker(&self, identity: &str) {
        if let Err(error) = self
            .write_enabled(identity, false, DisabledReason::CircuitBreaker)
            .await
        {
            crate::logger::report_error("plugin", "persist_circuit_breaker", &error);
            return;
        }
        self.load_all().await;
        self.publish_state().await;
        self.sink.emit(EngineEvent::PluginAutoDisabled {
            identity: identity.to_string(),
            reason: DisabledReason::CircuitBreaker.as_str().to_string(),
        });
        log_info!("[plugin] {identity} 连续超时，已自动熔断禁用");
        self.audit_event(identity, "breaker", "", "consecutive timeouts");
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
                return Err(PluginError::InvalidSetting {
                    key: key.clone(),
                    message: format!("未知设置项 '{key}'"),
                });
            };
            self.validate_value(field, value)?;
        }
        let values = entries
            .iter()
            .map(|(key, value)| (format!("plugin.{identity}.setting.{key}"), value.clone()))
            .collect();
        self.db
            .set_config_batch_atomic(&values)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.push_host_settings(identity).await;
        self.publish_state().await;
        Ok(())
    }

    /// 单设置项校验（类型/required/pattern/min-max/select/toggle）。
    fn validate_value(&self, field: &SettingField, value: &str) -> Result<(), PluginError> {
        let bad = |m: String| {
            Err(PluginError::InvalidSetting {
                key: field.key.clone(),
                message: m,
            })
        };
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
                    .ok_or_else(|| PluginError::InvalidSetting {
                        key: field.key.clone(),
                        message: format!("'{}' 不是有效数字", field.key),
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
        let failed_snapshot = self.failed_plugins.read().await.clone();
        let mut out = Vec::with_capacity(snapshot.len() + failed_snapshot.len());
        for p in snapshot.iter() {
            let values = self.load_setting_values(&p.manifest.identity).await;
            let loaded_pending = self.pending_for(&p.manifest).await;
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
                auth_supported: p.manifest.auth.is_some(),
                subscription_provider_ids: p
                    .manifest
                    .subscriptions
                    .iter()
                    .map(|subscription| subscription.provider_id.clone())
                    .collect(),
                resolver_urls: p
                    .manifest
                    .resolvers
                    .first()
                    .map(|r| r.match_decl.urls.clone())
                    .unwrap_or_default(),
                load_status: PluginLoadStatus::Loaded.as_str().to_string(),
                load_error: String::new(),
                contributions: p.manifest.contributes.clone().unwrap_or_default(),
                view_icons: p.view_icons.clone(),
                has_main: p.manifest.main.is_some(),
                risk: p.manifest.risk(),
                network_hosts: p.manifest.network_hosts(),
                consent_pending: loaded_pending.0,
                consent_pending_hosts: loaded_pending.1,
                package_revision: self.package_revisions.get(&p.manifest.identity),
                package_dir: Some(p.dir.to_string_lossy().into_owned()),
            });
        }
        for p in failed_snapshot.iter() {
            // enabled 强制 false（671#6）：失败插件从未真正跑起来过，
            // `plugin_state()` 的「无 enabled 键 = 新装默认启用」缺省语义是为
            // 已加载插件设计的，套在失败插件上会呈现「加载失败」徽章与
            // 「已启用」开关同框的矛盾态。disabled_reason 仍如实返回（用户
            // 手动禁用/熔断的历史原因，供 UI 展示）。
            let (_, disabled_reason) = self.plugin_state(&p.identity).await;
            let (
                name,
                version,
                description,
                homepage,
                settings,
                permissions,
                auth_supported,
                subscription_provider_ids,
            ) = match &p.manifest {
                Some(manifest) => (
                    manifest.name.clone(),
                    manifest.version.clone(),
                    manifest.description.clone(),
                    manifest.homepage.clone(),
                    manifest.settings.clone(),
                    manifest.permissions.clone(),
                    manifest.auth.is_some(),
                    manifest
                        .subscriptions
                        .iter()
                        .map(|subscription| subscription.provider_id.clone())
                        .collect(),
                ),
                None => (
                    p.identity.clone(),
                    String::new(),
                    String::new(),
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    false,
                    Vec::new(),
                ),
            };
            let values = self.load_setting_values(&p.identity).await;
            let failed_pending = match &p.manifest {
                Some(manifest) => self.pending_for(manifest).await,
                None => (Vec::new(), Vec::new()),
            };
            out.push(PluginInfo {
                identity: p.identity.clone(),
                name,
                version,
                description,
                homepage,
                enabled: false,
                dev_mode: p.dev_mode,
                disabled_reason: disabled_reason.as_str().to_string(),
                settings,
                settings_values: values.into_iter().collect(),
                permissions,
                auth_supported,
                subscription_provider_ids,
                resolver_urls: p
                    .manifest
                    .as_ref()
                    .and_then(|m| m.resolvers.first())
                    .map(|r| r.match_decl.urls.clone())
                    .unwrap_or_default(),
                load_status: PluginLoadStatus::Failed.as_str().to_string(),
                load_error: truncate_load_error(&p.error),
                contributions: super::manifest::ContributesDecl::default(),
                view_icons: Vec::new(),
                has_main: false,
                risk: p
                    .manifest
                    .as_ref()
                    .map(|m| m.risk())
                    .unwrap_or(super::manifest::PluginRisk::Standard),
                network_hosts: p
                    .manifest
                    .as_ref()
                    .map(|m| m.network_hosts())
                    .unwrap_or_default(),
                consent_pending: failed_pending.0,
                consent_pending_hosts: failed_pending.1,
                package_revision: self.package_revisions.get(&p.identity),
                package_dir: None,
            });
        }
        out.sort_by(|a, b| a.identity.cmp(&b.identity));
        out
    }

    async fn plugin_state(&self, identity: &str) -> (bool, DisabledReason) {
        let enabled = self
            .db
            .get_config(&format!("plugin.{identity}.enabled"))
            .await
            .ok()
            .flatten()
            .map(|value| value == "true")
            .unwrap_or(true);
        let reason = self
            .db
            .get_config(&format!("plugin.{identity}.disabled_reason"))
            .await
            .ok()
            .flatten()
            .as_deref()
            .map(DisabledReason::parse)
            .unwrap_or(DisabledReason::None);
        (enabled, reason)
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
    ///
    /// 二段解析任务（`resolver_item` 非空，任务组成员）的下载地址是分享页 / 清单页，
    /// 忽略插件直接下载只会把网页存成文件，故拒绝（[`PluginError::InvalidSetting`]，
    /// 宿主按参数错误展示）。
    pub async fn clear_task_resolver(&self, task_id: &str) -> Result<(), PluginError> {
        let item = self
            .db
            .get_task_resolver_item(task_id)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        if !item.is_empty() {
            return Err(PluginError::InvalidSetting {
                key: "taskId".to_owned(),
                message:
                    "该任务（任务组成员或插件订阅条目）必须由解析插件取得真实下载地址，无法忽略插件重试"
                        .to_owned(),
            });
        }
        self.db
            .set_task_resolver(task_id, "")
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))
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

/// 按 identity 去重（476#1）：安装目录扫描与 dev 键扫描互相独立，同一
/// identity 若既有已安装目录、又有 `plugin.dev.<identity>` 覆盖（或误留了两条
/// dev 注册），此前会原样塞进快照，让管理页显示两条同标识记录、且卸载其一
/// 时看起来「删不掉」（另一条仍在快照里）。这里保证同一 identity 只保留
/// 一条：dev 覆盖优先于安装目录（开发者主动登记的本地调试版本应生效）；
/// 被顶替的一份转入 `failed`，仍可在管理页看到并允许用户显式卸载清理磁盘/
/// 配置残留，不静默丢弃。纯函数，独立于 I/O 可测。
fn dedup_plugins_by_identity(
    loaded: Vec<LoadedPlugin>,
    failed: &mut Vec<FailedPlugin>,
) -> Vec<LoadedPlugin> {
    let mut identity_index: HashMap<String, usize> = HashMap::with_capacity(loaded.len());
    let mut deduped: Vec<LoadedPlugin> = Vec::with_capacity(loaded.len());
    for plugin in loaded {
        match identity_index.get(&plugin.manifest.identity).copied() {
            Some(idx) => {
                // dev 覆盖已安装目录：换入 dev 版本，被顶替的安装目录转入 failed。
                let displaced = if plugin.dev && !deduped[idx].dev {
                    std::mem::replace(&mut deduped[idx], plugin)
                } else {
                    plugin
                };
                failed.push(FailedPlugin {
                    identity: displaced.manifest.identity.clone(),
                    dir: displaced.dir.clone().into_boxed_path(),
                    manifest: Some(Box::new(displaced.manifest.clone())),
                    dev_mode: displaced.dev,
                    error: "identity 重复：与另一已加载插件冲突，可在此卸载清理残留目录/配置"
                        .to_string(),
                });
            }
            None => {
                identity_index.insert(plugin.manifest.identity.clone(), deduped.len());
                deduped.push(plugin);
            }
        }
    }
    deduped
}

fn identity_from_dev_key(key: &str) -> &str {
    key.strip_prefix("plugin.dev.").unwrap_or(key)
}

/// 读取 manifest 声明的入口脚本（相对插件目录）。未声明返回 `None`；缺失与不可读都
/// 以点名该文件的消息报告，供加载期判定失败。
async fn read_entry(dir: &Path, rel: Option<&str>) -> Result<Option<String>, String> {
    let Some(rel) = rel else {
        return Ok(None);
    };
    match tokio::fs::read_to_string(dir.join(rel)).await {
        Ok(source) => Ok(Some(source)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("包内缺少文件 '{rel}'"))
        }
        Err(error) => Err(format!("读取入口文件 '{rel}' 失败: {error}")),
    }
}

fn failure_identity(dir: &Path, identity_hint: Option<&str>) -> String {
    identity_hint
        .map(str::to_owned)
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// `FailedPlugin.error` 的可读渲染上限（671#9）：来源之一是
/// `manifest.validate()` 的错误文本，会原样插值插件可控的 manifest 字符串
/// （identity/version/icon 路径等，见 `manifest.rs` 的 `ManifestInvalid`
/// 消息），未经任何长度上限就会随每次插件列表刷新原样进入 `PluginInfo` /
/// REST `PluginDto` 并被客户端逐字渲染。截断只在这一处生效——
/// `list()` 是 `FailedPlugin.error` 唯一的对外读出口。
const MAX_LOAD_ERROR_LEN: usize = 1024;

fn truncate_load_error(error: &str) -> String {
    if error.len() <= MAX_LOAD_ERROR_LEN {
        return error.to_string();
    }
    let mut end = MAX_LOAD_ERROR_LEN;
    while end > 0 && !error.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = error[..end].to_string();
    truncated.push('…');
    truncated
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SubscriptionOutput {
    title: String,
    link: String,
    items: Vec<SubscriptionOutputItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SubscriptionOutputItem {
    guid: String,
    title: String,
    link: String,
    enclosure_url: String,
    /// 可选二段解析标识；非空时核心用条目 link 调 resolver，保留插件的精确规格。
    resolver_item: String,
    enclosure_length: i64,
    pub_date: i64,
}

/// 订阅输出校验：条目总数 ≤1000；单条非法（guid 空/超长、URL scheme 不在
/// 白名单、字段超长、长度为负）**跳过并记日志**而不是整批失败——订阅是无人
/// 值守链路，上游一条脏数据不该让整个源进入退避；但若条目非空且**全部**
/// 非法，视为插件输出系统性错误，返回 `InvalidOutput`。
fn parse_subscription_output(raw: &str) -> Result<ParsedFeed, PluginError> {
    let output: SubscriptionOutput = serde_json::from_str(raw)
        .map_err(|e| PluginError::InvalidOutput(format!("订阅返回值非法: {e}")))?;
    if output.items.len() > 1000 {
        return Err(PluginError::InvalidOutput(
            "订阅条目数量超过 1000".to_string(),
        ));
    }
    let total = output.items.len();
    let mut items = Vec::with_capacity(total);
    let mut first_reason: Option<String> = None;
    let mut rejected = 0usize;
    for item in output.items {
        match validate_subscription_item(item) {
            Ok(item) => items.push(item),
            Err(reason) => {
                rejected += 1;
                if first_reason.is_none() {
                    first_reason = Some(reason);
                }
            }
        }
    }
    if rejected > 0 {
        let reason = first_reason.unwrap_or_default();
        if items.is_empty() {
            return Err(PluginError::InvalidOutput(format!(
                "订阅条目全部非法（{rejected} 条），首条原因: {reason}"
            )));
        }
        crate::logger::log_error!(
            "[plugin] subscription output: skipped {} of {} items, first reason: {}",
            rejected,
            total,
            reason
        );
    }
    Ok(ParsedFeed {
        title: output.title,
        link: output.link,
        items,
    })
}

/// 单条订阅条目校验：guid 非空 ≤2048；title ≤1024；resolver_item ≤2048；
/// `enclosureUrl` / `link` 至少一个非空且都过 [`check_output_url`]（同 resolve
/// 平面：scheme 白名单 + ≤8KB）；`enclosureLength` 非负。
fn validate_subscription_item(item: SubscriptionOutputItem) -> Result<ParsedItem, String> {
    if item.guid.is_empty() || item.guid.len() > 2048 {
        return Err("guid 必须非空且不超过 2048 字节".to_string());
    }
    if item.title.len() > 1024 {
        return Err(format!("条目 {} 的 title 超过 1024 字节", item.guid));
    }
    if item.resolver_item.len() > 2048 {
        return Err(format!("条目 {} 的 resolverItem 超过 2048 字节", item.guid));
    }
    if item.link.is_empty() && item.enclosure_url.is_empty() {
        return Err(format!("条目 {} 缺少 link / enclosureUrl", item.guid));
    }
    if !item.resolver_item.is_empty() && item.link.is_empty() {
        return Err(format!(
            "条目 {} 含 resolverItem 时必须提供 link",
            item.guid
        ));
    }
    for (name, url) in [("link", &item.link), ("enclosureUrl", &item.enclosure_url)] {
        if !url.is_empty()
            && let Err(e) = check_output_url(url)
        {
            return Err(format!("条目 {} 的 {name} 非法: {e}", item.guid));
        }
    }
    if item.enclosure_length < 0 {
        return Err(format!("条目 {} 的 enclosureLength 不可为负数", item.guid));
    }
    Ok(ParsedItem {
        guid: item.guid,
        title: item.title,
        link: item.link,
        enclosure_url: item.enclosure_url,
        enclosure_type: String::new(),
        resolver_item: item.resolver_item,
        enclosure_length: item.enclosure_length,
        pub_date: item.pub_date,
    })
}

/// resolve 输出校验：url scheme ∈{http,https,ftp,magnet,ed2k}、长度 ≤8KB；
/// file_name 拒绝 `/ \ ..` 与控制字符；`manifest` 与 `url`/`variants`/
/// `audio_url` 互斥，且二段解析（`second_stage`，对应
/// [`ResolveRequest::resolver_item`] 非空）不可返回 `manifest`（防递归裂变，
/// D6/§7.6）。全 fail-closed：任一违规整体拒绝该次 resolve 结果。
fn validate_resolve_output(res: &ResolveResult, second_stage: bool) -> Result<(), PluginError> {
    if let Some(metadata) = &res.archive_metadata {
        validate_archive_metadata(metadata)?;
    }
    if let Some(manifest) = &res.manifest {
        if second_stage {
            return Err(PluginError::InvalidOutput(
                "二段解析（resolverItem 非空）不可返回 manifest：防递归裂变".to_string(),
            ));
        }
        if !res.url.is_empty() || !res.variants.is_empty() || res.audio_url.is_some() {
            return Err(PluginError::InvalidOutput(
                "manifest 与 url/variants/audioUrl 互斥".to_string(),
            ));
        }
        return validate_manifest(manifest);
    }
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

/// [`ResolveResult::manifest`] 校验：items 1..=1000；item.id 非空 ≤200；
/// item.name 非空且过 [`check_file_name`]；path 空或
/// [`is_safe_relative_path`]，按 `/` 计深度 ≤8，`path+"/"+name` 总长 ≤180
/// 字符（给 `save_dir` 前缀留余量，规避 Windows `MAX_PATH` 260 限制）；
/// kind ∈ {"", "file"}；per-item variants ≤50，variant id/label 非空。
/// fail-closed：任一条目违规拒绝整个 manifest（不做部分放行）。
fn validate_archive_metadata(metadata: &crate::model::ArchiveMetadata) -> Result<(), PluginError> {
    if !matches!(metadata.media_kind.as_str(), "" | "series" | "post") {
        return Err(PluginError::InvalidOutput(
            "archiveMetadata.mediaKind must be empty, series, or post".to_string(),
        ));
    }
    for (field, value) in [
        ("platform", &metadata.platform),
        ("creatorId", &metadata.creator_id),
        ("creatorName", &metadata.creator_name),
        ("seriesId", &metadata.series_id),
        ("seriesTitle", &metadata.series_title),
        ("collectionId", &metadata.collection_id),
        ("collectionTitle", &metadata.collection_title),
    ] {
        if value.len() > 180 || value.chars().any(char::is_control) {
            return Err(PluginError::InvalidOutput(format!(
                "archiveMetadata.{field} exceeds limit or contains controls"
            )));
        }
    }
    Ok(())
}

fn validate_manifest(m: &ResolveManifest) -> Result<(), PluginError> {
    if let Some(metadata) = &m.archive_metadata {
        validate_archive_metadata(metadata)?;
    }
    if m.items.is_empty() || m.items.len() > 1000 {
        return Err(PluginError::InvalidOutput(format!(
            "manifest items 数量非法: {}（须 1..=1000）",
            m.items.len()
        )));
    }
    for item in &m.items {
        validate_manifest_item(item)?;
    }
    Ok(())
}

fn validate_manifest_item(item: &ManifestItem) -> Result<(), PluginError> {
    if item.id.is_empty() || item.id.chars().count() > 200 {
        return Err(PluginError::InvalidOutput(format!(
            "manifest item id 非法: '{}'",
            item.id
        )));
    }
    if item.name.is_empty() {
        return Err(PluginError::InvalidOutput(
            "manifest item name 不可为空".to_string(),
        ));
    }
    check_file_name(&item.name)?;
    if !item.path.is_empty() && !is_safe_relative_path(&item.path) {
        return Err(PluginError::InvalidOutput(format!(
            "manifest item path 非法: '{}'",
            item.path
        )));
    }
    let depth = if item.path.is_empty() {
        0
    } else {
        item.path.split('/').count()
    };
    if depth > 8 {
        return Err(PluginError::InvalidOutput(format!(
            "manifest item path 深度超限: {depth} > 8"
        )));
    }
    let full_len = if item.path.is_empty() {
        item.name.chars().count()
    } else {
        item.path.chars().count() + 1 + item.name.chars().count()
    };
    if full_len > 180 {
        return Err(PluginError::InvalidOutput(format!(
            "manifest item 落盘路径过长: {full_len} > 180"
        )));
    }
    if !item.kind.is_empty() && item.kind != "file" {
        return Err(PluginError::InvalidOutput(format!(
            "manifest item kind 非法: '{}'",
            item.kind
        )));
    }
    if item.variants.len() > 50 {
        return Err(PluginError::InvalidOutput(format!(
            "manifest item variants 过多: {} > 50",
            item.variants.len()
        )));
    }
    for v in &item.variants {
        if v.id.is_empty() || v.label.is_empty() {
            return Err(PluginError::InvalidOutput(
                "manifest variant id/label 不可为空".to_string(),
            ));
        }
    }
    Ok(())
}

impl PluginManager {
    /// 生命周期审计（origin 恒为 `daemon`，outcome 恒为 `ok`）。
    fn audit_event(
        &self,
        identity: &str,
        action: &str,
        target: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.audit.record(super::audit::PluginAuditRow::new(
            identity, "daemon", action, target, "ok", detail,
        ));
    }

    /// 读取授权记录；缺失或损坏均为 `None`（损坏不得放行）。
    async fn read_consent(&self, identity: &str) -> Option<super::consent::ConsentRecord> {
        match self
            .db
            .get_config(&super::consent::consent_key(identity))
            .await
        {
            Ok(Some(raw)) => super::consent::ConsentRecord::parse(&raw),
            Ok(None) => None,
            Err(error) => {
                crate::logger::report_error("plugin", "read_consent", &error);
                None
            }
        }
    }

    /// 某 manifest 当前待授权的 `(权限, 网络主机)`。
    async fn pending_for(&self, manifest: &PluginManifest) -> (Vec<String>, Vec<String>) {
        let consent = self.read_consent(&manifest.identity).await;
        super::consent::pending(manifest, consent.as_ref())
    }

    /// 一次性迁移：升级前已安装的插件视为已授权其当前请求的全部能力（`source:"migration"`）。
    /// 之后安装的插件不会自动获得授权。
    async fn migrate_consent(&self) {
        use super::consent::{CONSENT_MIGRATED_KEY, ConsentRecord, consent_key};
        match self.db.get_config(CONSENT_MIGRATED_KEY).await {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                crate::logger::report_error("plugin", "consent_migration_flag", &error);
                return;
            }
        }
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Ok(mut rd) = tokio::fs::read_dir(&self.root).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                let internal = path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'));
                if path.is_dir() && !internal {
                    dirs.push(path);
                }
            }
        }
        if let Ok(entries) = self.db.list_config_with_prefix("plugin.dev.").await {
            dirs.extend(entries.into_iter().map(|(_, p)| PathBuf::from(p)));
        }
        let mut values = std::collections::BTreeMap::new();
        let mut migrated: Vec<(String, ConsentRecord)> = Vec::new();
        for dir in dirs {
            let Ok(bytes) = tokio::fs::read(dir.join("manifest.json")).await else {
                continue;
            };
            let Ok(manifest) = PluginManifest::parse(&bytes) else {
                continue;
            };
            if manifest.validate().is_err() {
                continue;
            }
            let identity = manifest.identity.clone();
            let key = consent_key(&identity);
            if values.contains_key(&key) {
                continue;
            }
            match self.db.get_config(&key).await {
                Ok(None) => {}
                Ok(Some(_)) => continue,
                Err(error) => {
                    crate::logger::report_error("plugin", "consent_migration_read", &error);
                    return;
                }
            }
            let record = ConsentRecord::for_manifest(&manifest, "migration");
            match record.to_json() {
                Ok(json) => {
                    values.insert(key, json);
                    migrated.push((identity, record));
                }
                Err(error) => {
                    crate::logger::report_error("plugin", "consent_migration_encode", &error);
                    return;
                }
            }
        }
        values.insert(CONSENT_MIGRATED_KEY.to_string(), "1".to_string());
        if let Err(error) = self.db.set_config_batch_atomic(&values).await {
            // 不写完成标记，下次加载重试。
            crate::logger::report_error("plugin", "consent_migration_write", &error);
            return;
        }
        for (identity, record) in migrated {
            self.audit_event(
                &identity,
                "consent.migrated",
                record.version.clone(),
                format!(
                    "permissions={} networkHosts={}",
                    record.permissions.join(","),
                    record.network_hosts.join(",")
                ),
            );
        }
    }

    /// 用户授权：`permissions` / `network_hosts` 必须与 manifest 当前请求的集合完全相等。
    pub async fn grant_consent(
        &self,
        identity: &str,
        permissions: &[String],
        network_hosts: &[String],
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
        if !super::consent::same_set(
            permissions,
            &super::consent::requested_permissions(&manifest),
        ) || !super::consent::same_set(network_hosts, &manifest.network_hosts())
        {
            return Err(PluginError::ManifestInvalid(
                "consent does not match requested capabilities".to_string(),
            ));
        }
        let record = super::consent::ConsentRecord::for_manifest(&manifest, "user");
        let json = record
            .to_json()
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.db
            .set_config(&super::consent::consent_key(identity), &json)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.audit_event(
            identity,
            "consent.granted",
            record.version.clone(),
            format!(
                "permissions={} networkHosts={}",
                record.permissions.join(","),
                record.network_hosts.join(",")
            ),
        );
        self.load_all().await;
        self.publish_state().await;
        Ok(())
    }
}

/// `daemon.plugin.audit.record` 单次上限。
const AUDIT_RECORD_MAX_ENTRIES: usize = 100;

/// 客户端上报的一条桌面审计记录（未校验）。
pub struct DesktopAuditEntry {
    pub identity: String,
    pub action: String,
    pub target: String,
    pub outcome: String,
    pub detail: String,
}

impl PluginManager {
    /// 分页读取审计（id 降序）；返回的游标非空表示还有更早的记录。
    pub async fn audit_list(
        &self,
        identity: Option<&str>,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<(Vec<super::audit::PluginAuditRow>, Option<i64>), PluginError> {
        let limit = limit.clamp(1, 1000);
        // 先落盘缓冲中的记录，保证读到最新。
        self.audit.flush().await;
        let mut rows = self
            .db
            .list_plugin_audit(identity, before_id, limit + 1)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        let next = if rows.len() > limit as usize {
            rows.truncate(limit as usize);
            rows.last().map(|row| row.id)
        } else {
            None
        };
        Ok((rows, next))
    }

    /// 清空审计（`identity` 为空 = 全部），并记一条 `audit.clear`（全部时记在 `*`）。
    pub async fn audit_clear(&self, identity: Option<&str>) -> Result<(), PluginError> {
        self.audit.flush().await;
        self.db
            .clear_plugin_audit(identity)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.audit_event(identity.unwrap_or("*"), "audit.clear", "", "");
        Ok(())
    }

    /// 接收客户端上报的桌面脚本审计：整批校验通过才入队；`origin` 强制为 `desktop`。
    pub async fn audit_record(&self, entries: Vec<DesktopAuditEntry>) -> Result<(), PluginError> {
        if entries.len() > AUDIT_RECORD_MAX_ENTRIES {
            return Err(PluginError::Runtime(format!(
                "entries exceeds {AUDIT_RECORD_MAX_ENTRIES}"
            )));
        }
        let mut known: HashSet<String> = HashSet::new();
        for entry in &entries {
            if !entry.action.starts_with("desktop.") {
                return Err(PluginError::Runtime(format!(
                    "action '{}' must start with 'desktop.'",
                    entry.action
                )));
            }
            if !matches!(entry.outcome.as_str(), "ok" | "denied" | "error") {
                return Err(PluginError::Runtime(format!(
                    "outcome '{}' must be ok|denied|error",
                    entry.outcome
                )));
            }
            if !known.contains(&entry.identity) {
                if self.installed_version(&entry.identity).await.is_none() {
                    return Err(PluginError::Runtime(format!(
                        "plugin '{}' is not installed",
                        entry.identity
                    )));
                }
                known.insert(entry.identity.clone());
            }
        }
        for entry in entries {
            self.audit.record(super::audit::PluginAuditRow::new(
                &entry.identity,
                "desktop",
                &entry.action,
                entry.target,
                &entry.outcome,
                entry.detail,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        FailedPlugin, LoadedPlugin, dedup_plugins_by_identity, parse_subscription_output,
        validate_resolve_output,
    };
    use crate::plugin::PluginManifest;
    use crate::plugin::{
        ManifestItem, ManifestVariant, PluginError, ResolveManifest, ResolveResult, ResolveVariant,
    };
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    fn test_manifest(identity: &str) -> PluginManifest {
        PluginManifest {
            identity: identity.to_string(),
            name: identity.to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            homepage: String::new(),
            icon: String::new(),
            min_app_version: String::new(),
            resolvers: Vec::new(),
            subscriptions: Vec::new(),
            hooks: None,
            interceptors: None,
            auth: None,
            main: None,
            contributes: None,
            settings: Vec::new(),
            permissions: Vec::new(),
        }
    }

    fn test_loaded_plugin(identity: &str, dev: bool, dir: &str) -> LoadedPlugin {
        LoadedPlugin {
            manifest: test_manifest(identity),
            dir: PathBuf::from(dir),
            dev,
            enabled: true,
            disabled_reason: super::DisabledReason::None,
            resolver_entry: None,
            hooks_entry: None,
            intercept_entry: None,
            archive_entry: None,
            auth_entry: None,
            subscription_entry: None,
            resolver_cache: None,
            hooks_cache: None,
            intercept_cache: None,
            archive_cache: None,
            auth_cache: None,
            subscription_cache: None,
            main_entry: None,
            main_cache: None,
            view_icons: Vec::new(),
            timeout_streak: Arc::new(AtomicU32::new(0)),
        }
    }

    #[tokio::test]
    async fn task_interceptors_follow_identity_order_not_load_order() {
        let (manager, _sink, root) = revoke_fixture().await;
        let manager = Arc::new(manager);
        let mut plugins = Vec::new();
        for identity in ["z@last", "a@first"] {
            let mut plugin = test_loaded_plugin(identity, false, "/unused");
            plugin.manifest.permissions = vec!["intercept".to_string()];
            plugin.manifest.interceptors = Some(
                serde_json::from_value(serde_json::json!({
                    "entry": "intercept.js",
                    "stages": ["prepare"],
                    "match": { "urls": ["https://example.com/*"] }
                }))
                .unwrap(),
            );
            plugins.push(plugin);
        }
        *manager.plugins.write().await = Arc::new(plugins);
        let interceptor = manager
            .task_interceptor("task", "https://example.com/file.bin", "")
            .await;
        let identities: Vec<_> = interceptor
            .entries()
            .iter()
            .map(|entry| entry.identity.as_str())
            .collect();
        assert_eq!(identities, ["a@first", "z@last"]);
        drop(interceptor);
        drop(manager);
        std::fs::remove_dir_all(root).unwrap();
    }

    // 476#1：同 identity 的安装目录 + dev 注册只保留一条（dev 覆盖安装目录），
    // 被顶替的一份进入 failed 列表而不是静默丢弃或重复出现在快照里。
    #[test]
    fn dedup_plugins_by_identity_prefers_dev_over_installed() {
        let installed = test_loaded_plugin("acme@demo", false, "/root/acme@demo");
        let dev = test_loaded_plugin("acme@demo", true, "/home/dev/demo");
        let mut failed: Vec<FailedPlugin> = Vec::new();

        let deduped = dedup_plugins_by_identity(vec![installed, dev], &mut failed);

        assert_eq!(deduped.len(), 1, "only one entry per identity may survive");
        assert!(
            deduped[0].dev,
            "dev registration must win over installed dir"
        );
        assert_eq!(
            failed.len(),
            1,
            "displaced duplicate must be recorded, not dropped"
        );
        assert!(
            !failed[0].dev_mode,
            "the displaced entry is the installed-dir copy"
        );
        assert_eq!(failed[0].identity, "acme@demo");
    }

    // 两个不同 identity 必须都保留、互不影响。
    #[test]
    fn dedup_plugins_by_identity_keeps_distinct_identities() {
        let a = test_loaded_plugin("acme@demo", false, "/root/acme@demo");
        let b = test_loaded_plugin("other@demo", false, "/root/other@demo");
        let mut failed: Vec<FailedPlugin> = Vec::new();

        let deduped = dedup_plugins_by_identity(vec![a, b], &mut failed);

        assert_eq!(deduped.len(), 2);
        assert!(failed.is_empty());
    }

    fn variant(label: &str, url: &str) -> ResolveVariant {
        ResolveVariant {
            label: label.into(),
            url: url.into(),
            ..Default::default()
        }
    }

    fn item(id: &str, name: &str, path: &str) -> ManifestItem {
        ManifestItem {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            ..Default::default()
        }
    }

    /// 单条非法条目跳过、合法条目保留；URL scheme 白名单与 resolve 平面一致。
    #[test]
    fn subscription_output_skips_invalid_items_but_keeps_valid_ones() {
        let raw = r#"{"title":"T","items":[
            {"guid":"ok","link":"https://a.test/1","enclosureUrl":"https://a.test/1.torrent"},
            {"guid":"","link":"https://a.test/2"},
            {"guid":"bad-scheme","link":"javascript:alert(1)"},
            {"guid":"no-url"},
            {"guid":"neg","link":"https://a.test/3","enclosureLength":-1}
        ]}"#;
        let feed = parse_subscription_output(raw).expect("partial output is accepted");
        assert_eq!(feed.items.len(), 1);
        assert_eq!(feed.items[0].guid, "ok");
    }

    /// 条目非空但全部非法：视为插件输出系统性错误，整轮失败。
    #[test]
    fn subscription_output_fails_when_every_item_is_invalid() {
        let raw = r#"{"items":[{"guid":"","link":"https://a.test/1"},{"guid":"x"}]}"#;
        assert!(matches!(
            parse_subscription_output(raw),
            Err(PluginError::InvalidOutput(_))
        ));
        // 空 feed（无条目）是合法的：新源尚无内容不算失败。
        assert!(parse_subscription_output(r#"{"items":[]}"#).is_ok());
    }

    #[test]
    fn subscription_output_requires_link_for_resolver_item() {
        let raw = r#"{"items":[{
            "guid":"resolver-without-link",
            "resolverItem":"episode@1080p",
            "enclosureUrl":"https://cdn.example/episode.torrent"
        }]}"#;
        assert!(matches!(
            parse_subscription_output(raw),
            Err(PluginError::InvalidOutput(_))
        ));
    }

    /// 有 variants 时顶层 url 允许为空（选中变体后覆盖）。
    #[test]
    fn variants_allow_empty_top_level_url() {
        let res = ResolveResult {
            variants: vec![variant("1080p", "https://v.example.com/a")],
            ..Default::default()
        };
        assert!(validate_resolve_output(&res, false).is_ok());
    }

    /// 无 variants 且顶层 url 为空 → 拒（原有单直链语义不放松）。
    #[test]
    fn empty_url_without_variants_rejected() {
        assert!(validate_resolve_output(&ResolveResult::default(), false).is_err());
    }

    /// 变体 label 为空 / url scheme 非法 → 拒。
    #[test]
    fn invalid_variant_rejected() {
        let empty_label = ResolveResult {
            variants: vec![variant("", "https://v.example.com/a")],
            ..Default::default()
        };
        assert!(validate_resolve_output(&empty_label, false).is_err());
        let bad_scheme = ResolveResult {
            variants: vec![variant("x", "javascript:alert(1)")],
            ..Default::default()
        };
        assert!(validate_resolve_output(&bad_scheme, false).is_err());
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
        assert!(validate_resolve_output(&res, false).is_err());
    }

    /// manifest 合法清单（1 段路径 + 无规格）→ 通过。
    #[test]
    fn valid_manifest_passes() {
        let res = ResolveResult {
            manifest: Some(ResolveManifest {
                name: "share".into(),
                items: vec![item("f1", "a.mp4", "sub")],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_resolve_output(&res, false).is_ok());
    }
    #[test]
    fn archive_metadata_is_validated_on_resolve_and_manifest() {
        let valid = crate::model::ArchiveMetadata {
            media_kind: "series".into(),
            series_title: "Series".into(),
            ..Default::default()
        };
        let direct = ResolveResult {
            archive_metadata: Some(valid.clone()),
            url: "https://cdn.example/file.mp4".into(),
            ..Default::default()
        };
        assert!(validate_resolve_output(&direct, false).is_ok());
        let invalid = crate::model::ArchiveMetadata {
            media_kind: "unknown".into(),
            ..Default::default()
        };
        assert!(
            validate_resolve_output(
                &ResolveResult {
                    archive_metadata: Some(invalid),
                    url: "https://cdn.example/file.mp4".into(),
                    ..Default::default()
                },
                false
            )
            .is_err()
        );
        assert!(
            validate_resolve_output(
                &ResolveResult {
                    manifest: Some(ResolveManifest {
                        archive_metadata: Some(valid),
                        items: vec![item("id", "file.mp4", "")],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                false
            )
            .is_ok()
        );
    }

    /// manifest 与 url/variants/audioUrl 互斥 → 各自拒绝。
    #[test]
    fn manifest_mutually_exclusive_with_url_fields() {
        let with_url = ResolveResult {
            url: "https://x.example.com/a".into(),
            manifest: Some(ResolveManifest {
                items: vec![item("f1", "a.mp4", "")],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_resolve_output(&with_url, false).is_err());

        let with_variants = ResolveResult {
            variants: vec![variant("x", "https://x.example.com/a")],
            manifest: Some(ResolveManifest {
                items: vec![item("f1", "a.mp4", "")],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_resolve_output(&with_variants, false).is_err());

        let with_audio = ResolveResult {
            audio_url: Some("https://x.example.com/a.m4a".into()),
            manifest: Some(ResolveManifest {
                items: vec![item("f1", "a.mp4", "")],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_resolve_output(&with_audio, false).is_err());
    }

    /// 二段解析（`resolverItem` 非空，`second_stage=true`）返回 manifest → 拒
    /// （防递归裂变，D6/§7.6）。
    #[test]
    fn second_stage_manifest_rejected() {
        let res = ResolveResult {
            manifest: Some(ResolveManifest {
                items: vec![item("f1", "a.mp4", "")],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_resolve_output(&res, true).is_err());
    }

    /// items 为空 / 超过 1000 → 拒。
    #[test]
    fn manifest_item_count_out_of_range_rejected() {
        let empty = ResolveResult {
            manifest: Some(ResolveManifest::default()),
            ..Default::default()
        };
        assert!(validate_resolve_output(&empty, false).is_err());

        let too_many = ResolveResult {
            manifest: Some(ResolveManifest {
                items: (0..1001)
                    .map(|i| item(&format!("f{i}"), "a.mp4", ""))
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate_resolve_output(&too_many, false).is_err());
    }

    /// item id/name 为空、path 不安全（含 `..`）、path 深度超 8、
    /// `path+name` 总长超 180、kind 非法、variant id 为空 → 各自拒绝。
    #[test]
    fn manifest_item_field_violations_rejected() {
        let cases: Vec<ResolveManifest> = vec![
            ResolveManifest {
                items: vec![item("", "a.mp4", "")],
                ..Default::default()
            },
            ResolveManifest {
                items: vec![item("f1", "", "")],
                ..Default::default()
            },
            ResolveManifest {
                items: vec![item("f1", "a.mp4", "../escape")],
                ..Default::default()
            },
            ResolveManifest {
                items: vec![item("f1", "a.mp4", "a/b/c/d/e/f/g/h/i")],
                ..Default::default()
            },
            ResolveManifest {
                items: vec![item("f1", &"a".repeat(200), "")],
                ..Default::default()
            },
            ResolveManifest {
                items: vec![ManifestItem {
                    kind: "folder".into(),
                    ..item("f1", "a.mp4", "")
                }],
                ..Default::default()
            },
            ResolveManifest {
                items: vec![ManifestItem {
                    variants: vec![ManifestVariant {
                        id: String::new(),
                        label: "1080p".into(),
                        size: None,
                    }],
                    ..item("f1", "a.mp4", "")
                }],
                ..Default::default()
            },
        ];
        for m in cases {
            let res = ResolveResult {
                manifest: Some(m),
                ..Default::default()
            };
            assert!(validate_resolve_output(&res, false).is_err());
        }
    }

    /// item.variants 数量 > 50 → 拒。
    #[test]
    fn manifest_item_too_many_variants_rejected() {
        let m = ResolveManifest {
            items: vec![ManifestItem {
                variants: (0..51)
                    .map(|i| ManifestVariant {
                        id: format!("v{i}"),
                        label: format!("v{i}"),
                        size: None,
                    })
                    .collect(),
                ..item("f1", "a.mp4", "")
            }],
            ..Default::default()
        };
        let res = ResolveResult {
            manifest: Some(m),
            ..Default::default()
        };
        assert!(validate_resolve_output(&res, false).is_err());
    }

    // ---- 市场撤回（Revoked）----

    use super::PluginInfo;
    use super::PluginManager;
    use std::sync::atomic::Ordering;

    async fn streak_arc(manager: &PluginManager, identity: &str) -> Arc<AtomicU32> {
        manager
            .plugins
            .read()
            .await
            .iter()
            .find(|p| p.manifest.identity == identity)
            .unwrap()
            .timeout_streak
            .clone()
    }
    use crate::events::{EngineEvent, EventSink};
    use crate::plugin::quickjs::QuickJsScriptRuntime;
    use crate::plugin::runtime::{
        BridgeHttpRequest, BridgeHttpResponse, PluginBridge, PluginLogLevel,
    };
    use std::collections::HashSet;

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

    #[derive(Default)]
    struct CaptureSink(std::sync::Mutex<Vec<EngineEvent>>);

    impl EventSink for CaptureSink {
        fn emit(&self, event: EngineEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    const REVOKE_ID: &str = "acme@demo";

    fn write_plugin(root: &std::path::Path, dir: &str, identity: &str, version: &str) {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(r#"{{"identity":"{identity}","name":"N","version":"{version}"}}"#),
        )
        .unwrap();
    }

    async fn revoke_fixture() -> (PluginManager, Arc<CaptureSink>, PathBuf) {
        let root = std::env::temp_dir().join(format!("fluxdown-revoke-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        write_plugin(&root, REVOKE_ID, REVOKE_ID, "1.0.0");
        let db = crate::db::Db::connect("sqlite::memory:").await.unwrap();
        let sink = Arc::new(CaptureSink::default());
        let manager = PluginManager::new(
            Arc::new(QuickJsScriptRuntime::new(1).unwrap()),
            Arc::new(NullBridge),
            db,
            root.clone(),
            "1.0.0".to_string(),
            sink.clone(),
        );
        manager.load_all().await;
        (manager, sink, root)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signed_archive_rules_seed_is_loaded_for_matching() {
        let identity = "acme@archive-rules";
        let root =
            std::env::temp_dir().join(format!("fluxdown-archive-rules-{}", uuid::Uuid::new_v4()));
        let plugin_dir = root.join(identity);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let payload = "{\"format\":\"fluxdown-archive-rules\",\"schemaVersion\":1,\"sequence\":1,\"rules\":[{\"id\":\"first\",\"folderName\":\"Series\",\"match\":{\"extensions\":[\"mkv\"]}}]}";
        let signature = "887d638fd517b304863762f7bb28d77366d47ff9631e76a0ced3c8d7191edb4664863ac908417e4880cbff89df6cee94cbad21afe9429573694530d6920db402";
        let public_key = "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8";
        let envelope = format!(
            "{{\"payload\":{},\"signature\":\"{}\"}}",
            serde_json::to_string(payload).unwrap(),
            signature
        );
        std::fs::write(plugin_dir.join("archive-rules.json"), envelope).unwrap();

        let db = crate::db::Db::connect("sqlite::memory:").await.unwrap();
        let manager = PluginManager::new(
            Arc::new(QuickJsScriptRuntime::new(1).unwrap()),
            Arc::new(NullBridge),
            db,
            root.clone(),
            "1.0.0".to_string(),
            Arc::new(CaptureSink::default()),
        );
        let mut plugin = test_loaded_plugin(identity, false, &plugin_dir.to_string_lossy());
        plugin.manifest.permissions.push("archive".to_string());
        plugin.manifest.contributes = Some(crate::plugin::manifest::ContributesDecl {
            archive_rules: Some(crate::plugin::manifest::ArchiveRulesDecl {
                entry: "archive-rules.json".to_string(),
                public_key: public_key.to_string(),
                update_url: None,
            }),
            ..Default::default()
        });
        *manager.plugins.write().await = Arc::new(vec![plugin]);

        manager.archive_rules_status(identity, false).await.unwrap();
        let matched = manager
            .archive_rule_match(
                identity,
                &crate::model::ArchiveContext {
                    file_name: "episode.MKV".to_string(),
                    ..Default::default()
                },
            )
            .await;
        assert_eq!(
            matched,
            Some(("Series".to_string(), "first".to_string(), 1))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn signed_rule_pack(key: &ed25519_dalek::SigningKey, sequence: u64, id: &str) -> Vec<u8> {
        use ed25519_dalek::Signer;

        let payload = serde_json::json!({
            "format": "fluxdown-archive-rules",
            "schemaVersion": 1,
            "sequence": sequence,
            "rules": [{
                "id": id,
                "folderName": format!("Folder-{id}"),
                "match": {"extensions": ["mkv"]}
            }]
        })
        .to_string();
        let signature = key.sign(payload.as_bytes());
        serde_json::json!({
            "payload": payload,
            "signature": hex::encode(signature.to_bytes())
        })
        .to_string()
        .into_bytes()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn archive_rule_public_key_rotation_resets_trust_watermark() {
        use ed25519_dalek::SigningKey;

        let identity = "acme@archive-rotate-key";
        let root = std::env::temp_dir().join(format!(
            "fluxdown-archive-rotate-key-{}",
            uuid::Uuid::new_v4()
        ));
        let plugin_dir = root.join(identity);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let old_key = SigningKey::from_bytes(&[11; 32]);
        let new_key = SigningKey::from_bytes(&[13; 32]);
        let old_public = hex::encode(old_key.verifying_key().to_bytes());
        let new_public = hex::encode(new_key.verifying_key().to_bytes());
        std::fs::write(
            plugin_dir.join("archive-rules.json"),
            signed_rule_pack(&old_key, 9, "old"),
        )
        .unwrap();

        let db = crate::db::Db::connect("sqlite::memory:").await.unwrap();
        let manager = PluginManager::new(
            Arc::new(QuickJsScriptRuntime::new(1).unwrap()),
            Arc::new(NullBridge),
            db,
            root.clone(),
            "1.0.0".to_string(),
            Arc::new(CaptureSink::default()),
        );
        let mut plugin = test_loaded_plugin(identity, false, &plugin_dir.to_string_lossy());
        plugin.manifest.permissions.push("archive".to_string());
        plugin.manifest.contributes = Some(crate::plugin::manifest::ContributesDecl {
            archive_rules: Some(crate::plugin::manifest::ArchiveRulesDecl {
                entry: "archive-rules.json".to_string(),
                public_key: old_public,
                update_url: None,
            }),
            ..Default::default()
        });
        *manager.plugins.write().await = Arc::new(vec![plugin]);
        let bootstrapped = manager.archive_rules_status(identity, false).await.unwrap();
        assert_eq!(bootstrapped["sequence"], 9);

        std::fs::write(
            plugin_dir.join("archive-rules.json"),
            signed_rule_pack(&new_key, 1, "rotated"),
        )
        .unwrap();
        let mut plugin = test_loaded_plugin(identity, false, &plugin_dir.to_string_lossy());
        plugin.manifest.permissions.push("archive".to_string());
        plugin.manifest.contributes = Some(crate::plugin::manifest::ContributesDecl {
            archive_rules: Some(crate::plugin::manifest::ArchiveRulesDecl {
                entry: "archive-rules.json".to_string(),
                public_key: new_public.clone(),
                update_url: None,
            }),
            ..Default::default()
        });
        *manager.plugins.write().await = Arc::new(vec![plugin]);
        manager.archive_rule_packs.write().await.remove(identity);
        manager.archive_rule_status.write().await.remove(identity);

        let rotated = manager.archive_rules_status(identity, false).await.unwrap();
        assert_eq!(rotated["sequence"], 1);
        assert_eq!(rotated["publicKey"], new_public);
        let matched = manager
            .archive_rule_match(
                identity,
                &crate::model::ArchiveContext {
                    file_name: "episode.mkv".to_string(),
                    ..Default::default()
                },
            )
            .await;
        assert_eq!(
            matched,
            Some(("Folder-rotated".to_string(), "rotated".to_string(), 1))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn archive_rule_refresh_persists_only_verified_promotions() {
        use ed25519_dalek::SigningKey;

        let identity = "acme@archive-refresh";
        let root =
            std::env::temp_dir().join(format!("fluxdown-archive-refresh-{}", uuid::Uuid::new_v4()));
        let plugin_dir = root.join(identity);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let public_key = hex::encode(signing_key.verifying_key().to_bytes());
        let seed = signed_rule_pack(&signing_key, 1, "seed");
        std::fs::write(plugin_dir.join("archive-rules.json"), &seed).unwrap();

        let db = crate::db::Db::connect("sqlite::memory:").await.unwrap();
        let manager = PluginManager::new(
            Arc::new(QuickJsScriptRuntime::new(1).unwrap()),
            Arc::new(NullBridge),
            db,
            root.clone(),
            "1.0.0".to_string(),
            Arc::new(CaptureSink::default()),
        );
        let mut plugin = test_loaded_plugin(identity, false, &plugin_dir.to_string_lossy());
        plugin.manifest.permissions.push("archive".to_string());
        let declaration = crate::plugin::manifest::ArchiveRulesDecl {
            entry: "archive-rules.json".to_string(),
            public_key: public_key.clone(),
            update_url: None,
        };
        plugin.manifest.contributes = Some(crate::plugin::manifest::ContributesDecl {
            archive_rules: Some(declaration.clone()),
            ..Default::default()
        });
        *manager.plugins.write().await = Arc::new(vec![plugin]);
        manager.archive_rules_status(identity, false).await.unwrap();
        // Reject even byte-identical writes: raw-value equality alone cannot prove idle silence.
        for sql in [
            "CREATE TRIGGER reject_archive_poll_insert BEFORE INSERT ON config \
             BEGIN SELECT RAISE(ABORT, 'idle archive refresh attempted a config write'); END",
            "CREATE TRIGGER reject_archive_poll_update BEFORE UPDATE ON config \
             BEGIN SELECT RAISE(ABORT, 'idle archive refresh attempted a config write'); END",
            "CREATE TRIGGER reject_archive_poll_delete BEFORE DELETE ON config \
             BEGIN SELECT RAISE(ABORT, 'idle archive refresh attempted a config write'); END",
        ] {
            sqlx::query(sql).execute(manager.db.pool()).await.unwrap();
        }

        let cache_key = format!("plugin.{identity}.archive_rules.cache");
        let watermark_key = format!("plugin.{identity}.archive_rules.watermark");
        let original_cache = manager.db.get_config(&cache_key).await.unwrap().unwrap();
        let original_watermark = manager
            .db
            .get_config(&watermark_key)
            .await
            .unwrap()
            .unwrap();
        let mut cache = manager
            .archive_rule_status
            .read()
            .await
            .get(identity)
            .unwrap()
            .clone();
        let url = "https://updates.example.invalid/archive-rules.json";

        manager
            .process_archive_rule_update(
                identity,
                &declaration,
                url,
                &mut cache,
                Err(PluginError::Runtime("offline".to_string())),
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            manager.db.get_config(&cache_key).await.unwrap().as_deref(),
            Some(original_cache.as_str())
        );
        assert_eq!(
            manager
                .db
                .get_config(&watermark_key)
                .await
                .unwrap()
                .as_deref(),
            Some(original_watermark.as_str())
        );

        manager
            .process_archive_rule_update(
                identity,
                &declaration,
                url,
                &mut cache,
                Ok(b"{invalid signature}".to_vec()),
                15,
            )
            .await
            .unwrap();
        assert_eq!(
            manager.db.get_config(&cache_key).await.unwrap().as_deref(),
            Some(original_cache.as_str())
        );
        assert_eq!(
            manager
                .db
                .get_config(&watermark_key)
                .await
                .unwrap()
                .as_deref(),
            Some(original_watermark.as_str())
        );

        manager
            .process_archive_rule_update(
                identity,
                &declaration,
                url,
                &mut cache,
                Ok(seed.clone()),
                20,
            )
            .await
            .unwrap();
        assert_eq!(cache.last_error, None);
        assert_eq!(
            manager.db.get_config(&cache_key).await.unwrap().as_deref(),
            Some(original_cache.as_str())
        );
        assert_eq!(
            manager
                .db
                .get_config(&watermark_key)
                .await
                .unwrap()
                .as_deref(),
            Some(original_watermark.as_str())
        );

        manager
            .process_archive_rule_update(
                identity,
                &declaration,
                url,
                &mut cache,
                Ok(signed_rule_pack(&signing_key, 1, "equivocation")),
                30,
            )
            .await
            .unwrap();
        assert_eq!(
            manager.db.get_config(&cache_key).await.unwrap().as_deref(),
            Some(original_cache.as_str())
        );
        assert_eq!(
            manager
                .db
                .get_config(&watermark_key)
                .await
                .unwrap()
                .as_deref(),
            Some(original_watermark.as_str())
        );

        for sql in [
            "DROP TRIGGER reject_archive_poll_insert",
            "DROP TRIGGER reject_archive_poll_update",
            "DROP TRIGGER reject_archive_poll_delete",
        ] {
            sqlx::query(sql).execute(manager.db.pool()).await.unwrap();
        }

        let promoted = signed_rule_pack(&signing_key, 2, "promoted");
        let verified = manager
            .process_archive_rule_update(identity, &declaration, url, &mut cache, Ok(promoted), 40)
            .await
            .unwrap()
            .expect("higher sequence must be promoted");
        assert_eq!(cache.sequence, 2);
        assert_eq!(cache.last_error, None);
        let persisted_cache_raw = manager.db.get_config(&cache_key).await.unwrap().unwrap();
        let persisted_cache: serde_json::Value =
            serde_json::from_str(&persisted_cache_raw).unwrap();
        assert_eq!(persisted_cache["sequence"], 2);
        assert!(persisted_cache["lastCheckedAt"].is_null());
        assert!(persisted_cache["lastError"].is_null());
        let persisted_watermark: serde_json::Value = serde_json::from_str(
            &manager
                .db
                .get_config(&watermark_key)
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(persisted_watermark["sequence"], 2);
        assert_eq!(persisted_watermark["digest"], cache.digest);
        manager
            .archive_rule_packs
            .write()
            .await
            .insert(identity.to_string(), Arc::new(verified));
        manager
            .archive_rule_status
            .write()
            .await
            .insert(identity.to_string(), cache);
        manager
            .db
            .set_config(&cache_key, "malformed")
            .await
            .unwrap();
        let status = manager.archive_rules_status(identity, false).await.unwrap();
        assert_eq!(status["sequence"], 2);
        manager
            .db
            .set_config(&cache_key, &persisted_cache_raw)
            .await
            .unwrap();
        std::fs::write(
            plugin_dir.join("archive-rules.json"),
            signed_rule_pack(&signing_key, 3, "bundled-upgrade"),
        )
        .unwrap();
        manager.archive_rule_packs.write().await.remove(identity);
        manager.archive_rule_status.write().await.remove(identity);
        let upgraded = manager.archive_rules_status(identity, false).await.unwrap();
        assert_eq!(upgraded["sequence"], 3);
        let upgraded_watermark: serde_json::Value = serde_json::from_str(
            &manager
                .db
                .get_config(&watermark_key)
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(upgraded_watermark["sequence"], 3);
        std::fs::write(
            plugin_dir.join("archive-rules.json"),
            b"invalid replacement",
        )
        .unwrap();
        manager.archive_rule_status.write().await.remove(identity);
        manager.initialize_archive_rule_packs().await;
        assert!(
            manager
                .archive_rule_match(
                    identity,
                    &crate::model::ArchiveContext {
                        file_name: "video.mkv".to_owned(),
                        ..Default::default()
                    },
                )
                .await
                .is_none(),
            "a failed declaration reload must not keep matching the old rule pack"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn revoked_set(identity: &str, version: &str) -> HashSet<(String, String)> {
        HashSet::from([(identity.to_string(), version.to_string())])
    }

    async fn state_of(manager: &PluginManager) -> (bool, String) {
        let info = manager
            .list()
            .await
            .into_iter()
            .find(|p| p.identity == REVOKE_ID)
            .unwrap();
        (info.enabled, info.disabled_reason)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn market_revocation_disables_plugin_and_emits_event() {
        let (manager, sink, _root) = revoke_fixture().await;
        assert_eq!(state_of(&manager).await, (true, "None".to_string()));

        let changed = manager
            .apply_market_revocations(&revoked_set(REVOKE_ID, "1.0.0"))
            .await
            .unwrap();
        assert!(changed);
        assert_eq!(state_of(&manager).await, (false, "Revoked".to_string()));
        assert!(sink.0.lock().unwrap().iter().any(|event| matches!(
            event,
            EngineEvent::PluginAutoDisabled { identity, reason }
                if identity == REVOKE_ID && reason == "Revoked"
        )));

        // 幂等：重复应用不再变化、不再发事件（首次：PluginsChanged + PluginAutoDisabled）。
        let again = manager
            .apply_market_revocations(&revoked_set(REVOKE_ID, "1.0.0"))
            .await
            .unwrap();
        assert!(!again);
        assert_eq!(sink.0.lock().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn revoked_version_cannot_be_enabled() {
        let (manager, _sink, _root) = revoke_fixture().await;
        manager
            .apply_market_revocations(&revoked_set(REVOKE_ID, "1.0.0"))
            .await
            .unwrap();
        let error = manager.set_enabled(REVOKE_ID, true).await.unwrap_err();
        assert!(error.to_string().contains("已被插件市场撤回"));
        assert_eq!(state_of(&manager).await, (false, "Revoked".to_string()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn installing_newer_version_reenables_and_clears_marker() {
        let (manager, _sink, root) = revoke_fixture().await;
        manager
            .apply_market_revocations(&revoked_set(REVOKE_ID, "1.0.0"))
            .await
            .unwrap();

        write_plugin(&root, REVOKE_ID, REVOKE_ID, "1.1.0");
        let prior = manager.capture_install_state(REVOKE_ID).await.unwrap();
        manager
            .finish_install(REVOKE_ID, "1.1.0", &prior)
            .await
            .unwrap();

        assert_eq!(state_of(&manager).await, (true, "None".to_string()));
        let marker = manager
            .db
            .get_config(&format!("plugin.{REVOKE_ID}.revoked_version"))
            .await
            .unwrap();
        assert!(marker.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lifted_revocation_clears_marker_but_stays_disabled() {
        let (manager, _sink, _root) = revoke_fixture().await;
        manager
            .apply_market_revocations(&revoked_set(REVOKE_ID, "1.0.0"))
            .await
            .unwrap();

        let changed = manager
            .apply_market_revocations(&HashSet::new())
            .await
            .unwrap();
        assert!(changed);
        assert_eq!(state_of(&manager).await, (false, "Revoked".to_string()));
        manager.set_enabled(REVOKE_ID, true).await.unwrap();
        assert_eq!(state_of(&manager).await, (true, "None".to_string()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dev_plugins_are_not_revoked() {
        let (manager, sink, root) = revoke_fixture().await;
        // 把已装目录换成 dev 登记：dev 覆盖同名安装目录。
        let dev_dir = root.join(".dev-src");
        write_plugin(&root, ".dev-src", REVOKE_ID, "1.0.0");
        manager
            .db
            .set_config(
                &format!("plugin.dev.{REVOKE_ID}"),
                &dev_dir.to_string_lossy(),
            )
            .await
            .unwrap();
        std::fs::remove_dir_all(root.join(REVOKE_ID)).unwrap();
        manager.load_all().await;

        let changed = manager
            .apply_market_revocations(&revoked_set(REVOKE_ID, "1.0.0"))
            .await
            .unwrap();
        assert!(!changed);
        assert_eq!(state_of(&manager).await, (true, "None".to_string()));
        assert!(sink.0.lock().unwrap().is_empty());
    }

    fn plugins_changed(sink: &CaptureSink) -> Vec<Vec<PluginInfo>> {
        sink.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                EngineEvent::PluginsChanged(list) => Some(list.clone()),
                _ => None,
            })
            .collect()
    }

    fn find_plugin<'a>(list: &'a [PluginInfo], identity: &str) -> &'a PluginInfo {
        list.iter().find(|p| p.identity == identity).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn circuit_breaker_trip_publishes_disabled_state_before_notice() {
        let (manager, sink, _root) = revoke_fixture().await;
        manager.trip_circuit_breaker(REVOKE_ID).await;

        let events = sink.0.lock().unwrap();
        let changed_at = events
            .iter()
            .position(|event| matches!(event, EngineEvent::PluginsChanged(_)))
            .expect("PluginsChanged emitted");
        let notice_at = events
            .iter()
            .position(|event| matches!(event, EngineEvent::PluginAutoDisabled { .. }))
            .expect("PluginAutoDisabled emitted");
        assert!(changed_at < notice_at);
        let EngineEvent::PluginsChanged(list) = &events[changed_at] else {
            unreachable!();
        };
        let info = find_plugin(list, REVOKE_ID);
        assert!(!info.enabled);
        assert_eq!(info.disabled_reason, "CircuitBreaker");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_settings_update_publishes_new_values() {
        let (manager, sink, root) = revoke_fixture().await;
        std::fs::write(
            root.join(REVOKE_ID).join("manifest.json"),
            format!(
                r#"{{"identity":"{REVOKE_ID}","name":"N","version":"1.0.0",
                    "settings":[{{"key":"q","title":"Q","type":"string"}}]}}"#
            ),
        )
        .unwrap();
        manager.load_all().await;

        manager
            .host_settings_update(REVOKE_ID, vec![("q".to_string(), "hello".to_string())])
            .await
            .unwrap();

        let lists = plugins_changed(&sink);
        let last = lists.last().expect("PluginsChanged emitted");
        let info = find_plugin(last, REVOKE_ID);
        assert!(info.settings.iter().any(|field| field.key == "q"));
        assert!(
            info.settings_values
                .iter()
                .any(|(key, value)| key == "q" && value == "hello")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn upgrade_publishes_bumped_package_revision() {
        let (manager, sink, root) = revoke_fixture().await;
        let before = find_plugin(&manager.list().await, REVOKE_ID).package_revision;

        let src = root.join(".upgrade-src");
        write_plugin(&root, ".upgrade-src", REVOKE_ID, "1.1.0");
        manager.install_from_dir(&src).await.unwrap();

        let lists = plugins_changed(&sink);
        let last = lists.last().expect("PluginsChanged emitted");
        let info = find_plugin(last, REVOKE_ID);
        assert_eq!(info.version, "1.1.0");
        assert!(info.package_revision > before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_streak_survives_unrelated_reloads_and_resets_on_reenable() {
        const OTHER: &str = "other@demo";
        let (manager, _sink, root) = revoke_fixture().await;
        write_plugin(&root, OTHER, OTHER, "1.0.0");
        manager.load_all().await;

        let streak = streak_arc(&manager, REVOKE_ID).await;
        streak.store(2, Ordering::SeqCst);

        manager.set_enabled(OTHER, false).await.unwrap();
        manager.set_enabled(OTHER, true).await.unwrap();
        manager.load_all().await;
        assert_eq!(
            streak_arc(&manager, REVOKE_ID).await.load(Ordering::SeqCst),
            2
        );

        manager.set_enabled(REVOKE_ID, false).await.unwrap();
        manager.set_enabled(REVOKE_ID, true).await.unwrap();
        assert_eq!(
            streak_arc(&manager, REVOKE_ID).await.load(Ordering::SeqCst),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_streak_resets_on_code_replacement_then_survives_unrelated_reloads() {
        const OTHER: &str = "other@demo";
        let (manager, _sink, root) = revoke_fixture().await;
        write_plugin(&root, OTHER, OTHER, "1.0.0");
        manager.load_all().await;
        streak_arc(&manager, REVOKE_ID)
            .await
            .store(2, Ordering::SeqCst);

        // 同版本重装 = 代码被替换：新代码不继承旧计数。
        let src = root.join(".reinstall-src");
        write_plugin(&root, ".reinstall-src", REVOKE_ID, "1.0.0");
        manager.install_from_dir(&src).await.unwrap();
        let streak = streak_arc(&manager, REVOKE_ID).await;
        assert_eq!(streak.load(Ordering::SeqCst), 0);

        // 替换之后，无关插件的启停不再清零。
        streak.store(1, Ordering::SeqCst);
        manager.set_enabled(OTHER, false).await.unwrap();
        manager.set_enabled(OTHER, true).await.unwrap();
        assert_eq!(
            streak_arc(&manager, REVOKE_ID).await.load(Ordering::SeqCst),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_upgrade_restores_breaker_state() {
        let (manager, _sink, root) = revoke_fixture().await;
        manager.trip_circuit_breaker(REVOKE_ID).await;

        // 新版本声明了不存在的 resolver 入口 → 加载失败 → 安装回滚。
        let src = root.join(".bad-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("manifest.json"),
            format!(
                r#"{{"identity":"{REVOKE_ID}","name":"N","version":"2.0.0",
                    "resolvers":[{{"match":{{"urls":["*://x.com/*"]}},"entry":"missing.js"}}]}}"#
            ),
        )
        .unwrap();
        assert!(manager.install_from_dir(&src).await.is_err());

        assert_eq!(
            state_of(&manager).await,
            (false, "CircuitBreaker".to_string())
        );
        let list = manager.list().await;
        assert_eq!(find_plugin(&list, REVOKE_ID).version, "1.0.0");
    }
}
