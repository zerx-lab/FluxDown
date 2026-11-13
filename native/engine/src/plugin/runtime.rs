//! 脚本运行时抽象层。**本文件禁止出现任何 rquickjs 类型**——v1 由
//! [`super::quickjs`] 实现；未来 deno_core 实现同一 trait。
//!
//! dyn 兼容论证同 `selection.rs`：`Engine` 以 `Arc<dyn>` 存字段跨任务共享。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 单次脚本调用的资源预算。`timeout` 由外层 `tokio::time::timeout` 强制（不依赖
/// QuickJS 检查点），`memory_limit_bytes` 交给运行时的内存限制器。
#[derive(Debug, Clone, Copy)]
pub struct ExecutionBudget {
    pub timeout: Duration,
    pub memory_limit_bytes: usize,
}

/// 已加载脚本的最小执行单元 —— 只承载 identity/源码/入口种类，与具体运行时无关。
#[derive(Debug, Clone)]
pub struct PluginScript {
    pub identity: String,
    pub source: String,
    pub entry_fn_hint: PluginEntryKind,
    /// 插件自身版本（供 `flux.info.version`）。
    pub version: String,
    /// 宿主 App 版本（供 `flux.info.appVersion`）。
    pub app_version: String,
}

/// 入口函数种类，决定运行时从 `globalThis` 取哪个函数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginEntryKind {
    /// resolver 入口：`globalThis.resolve`。
    Resolve,
    /// hook 入口：`globalThis.onStart/onError/onDone/onMetaProbed`（由 event 决定）。
    Hook,
    /// auth 入口：`globalThis.authenticate`。
    Auth,
    /// 订阅 provider 入口：`globalThis.subscribe`。
    Subscription,
    /// 常驻宿主入口：manifest `main.entry`，顶层脚本注册视图/命令/RPC。
    Main,
    /// 拦截入口：`globalThis.intercept`。
    Intercept,
    /// Pure archive callback: `globalThis.archive({fileName})`.
    Archive,
}

// ---------------------------------------------------------------------------
// 跨 JS 边界结构：统一 #[serde(rename_all="camelCase")]，JS 侧字段名即 camelCase。
// ---------------------------------------------------------------------------

/// 传入 `resolve(ctx)` 的请求上下文。`url` 恒为 source_url（原始任务 URL）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveRequest {
    pub task_id: String,
    pub url: String,
    /// 当前插件对该站点的默认认证引用；只有声明了 `auth` 权限的插件才会被
    /// 填充（M-5），未授权插件恒为空串。插件通常直接使用 `flux.fetch`，
    /// bridge 会自动按该引用查找并复用凭据。
    #[serde(default)]
    pub auth_ref: String,
    pub cookies: String,
    pub referrer: String,
    pub user_agent: String,
    pub extra_headers: HashMap<String, String>,
    /// 二段解析标识（不透明字符串，对应清单条目的 `id` 或 `id@variantId`）。
    /// 初段解析（含前置预解析）恒为空；非空即二段——`PluginManager::resolve`
    /// 据此拒绝嵌套 manifest（防裂变递归，见 [`ResolveResult::manifest`]）并让
    /// 变体收敛静默取默认（不为 N 个子任务弹 N 个选择框）。引擎不解释具体格式，
    /// 由发起方（前端固定选择/引擎自动裂变）与插件约定（D5 契约）。
    pub resolver_item: String,
}

pub type ArchiveRequest = crate::model::ArchiveContext;

/// Archive callback output; directory components are validated by the host.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchiveResult {
    /// Relative path components below the task's authorized default directory.
    pub directories: Vec<String>,
    /// Legacy single-component output from existing plugins.
    pub folder_name: String,
}

/// 一次平台登录动作。`action` 为 `begin`、`poll`、`cancel`、`logout` 或 `status`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthRequest {
    pub action: String,
    pub site: String,
    pub auth_ref: String,
    pub session_id: String,
    pub input: String,
}

/// 插件登录入口返回的交互状态。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AuthResult {
    /// `pending` / `success` / `error`。
    pub status: String,
    pub session_id: String,
    /// 二维码内容、data URL 或平台要求展示给用户的挑战数据。
    pub challenge: Option<String>,
    pub challenge_type: Option<String>,
    pub message: String,
    pub auth_ref: Option<String>,
}

/// 传入插件订阅 provider 的请求上下文。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionRequest {
    /// 当前 provider ID。
    pub provider_id: String,
    /// 公共订阅记录 ID。
    pub source_id: String,
    /// 订阅记录中的地址。
    pub url: String,
    /// provider 专属不透明配置 JSON 字符串。
    pub provider_config: String,
    /// 订阅级 Cookie。
    pub cookies: String,
    /// 已解析的 User-Agent。
    pub user_agent: String,
}

/// `resolve(ctx)` 的返回值。返回 `null`/`undefined` 表示放行不改写（映射为
/// `Ok(None)`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResolveResult {
    /// 改写后的直链。
    pub url: String,
    /// 可选音频直链（用于 DASH 音视频分离场景）。
    pub audio_url: Option<String>,
    pub file_name: Option<String>,
    pub total_bytes: Option<i64>,
    pub extra_headers: Option<HashMap<String, String>>,
    /// 直链为一次性/防探测签名 URL 时置 true → 跳过 probe（牺牲 If-Range）；
    /// 默认 false → 正常 probe 取 ETag，保 resume 一致性。
    pub ephemeral: bool,
    /// 插件担保该直链所在服务支持 HTTP Range（如 googlevideo）。与 `ephemeral`
    /// 正交：`ephemeral` 表达"probe 会作废直链"，本字段表达"Range 请求安全"。
    /// 置 true 时引擎跳过 probe 的同时仍按已验证 Range 规划多段并发下载，
    /// 不落入配额型端点（fnOS）的保守单流启动。默认 false（保守）。
    pub range_supported: bool,
    /// 可选变体列表（画质/格式）。非空时宿主经 `HostSelection::select_resolve_variant`
    /// 让用户选择，选中变体的非空字段覆盖本结构的 url/audio_url/file_name/
    /// total_bytes（在 resolve worker 内收敛，回流 actor 前完成）。空 = 单一直链
    /// 旧语义，零破坏。变体存在时顶层 `url` 允许为空。
    pub variants: Vec<ResolveVariant>,
    /// 默认变体索引（超时/免打扰/headless 回退用），通常由插件按自身"画质偏好"
    /// 设置指向对应变体。越界按 0 处理。默认 0。
    pub default_variant_index: i32,
    /// 多文件清单（网盘文件夹/多规格资源）。与 `url`/`variants`/`audio_url`
    /// **互斥**（`manifest.is_some()` 时后三者须为空，`validate_resolve_output`
    /// fail-closed 强制）。仅初段解析（[`ResolveRequest::resolver_item`] 为空）
    /// 可返回；二段返回会被拒绝（防递归裂变）。`None` = 单直链旧语义。
    pub manifest: Option<ResolveManifest>,
    /// Validated descriptive source metadata for classification.
    pub archive_metadata: Option<crate::model::ArchiveMetadata>,

    /// 请求改写补丁（与拦截器 `prepare` 返回值同形），每次运行都重新应用，
    /// 失败即任务失败。`extraHeaders` 作为整表替换并入同一补丁。
    pub request: Option<crate::intercept::RequestPatch>,
}

/// [`ResolveResult::manifest`] —— 插件返回的多文件清单（网盘文件夹/多规格
/// 资源）。扁平 `items[] + path` 表达（同构 [`crate::model::BtFileEntry`] 的
/// 相对路径惯例），不做递归树；`path` 深度与「`path`+`name`」总长受契约层硬
/// 限制（校验实现见 `super::manager` 的 `validate_resolve_output`，设计详见
/// 设计文档 §4.2）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResolveManifest {
    /// 文件夹名/组名（gopeed `Res.Name` 同义）。空 = 由宿主用母任务文件名/
    /// 首个条目名兜底（引擎自动裂变路径的语义，见 `on_resolve_ready`）。
    pub name: String,
    /// Root-level source classification hints shared by all manifest items.
    pub archive_metadata: Option<crate::model::ArchiveMetadata>,

    /// 清单条目，1..=1000。
    pub items: Vec<ManifestItem>,
}

/// [`ResolveManifest::items`] 的单个条目。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ManifestItem {
    /// 插件自定义标识，回传 [`ResolveRequest::resolver_item`] 用（不透明字符串，
    /// 引擎不解释语义）。非空，≤200 字符。
    pub id: String,
    /// 文件名。非空，须通过与 [`ResolveResult::file_name`] 相同的合法性检查
    /// （禁 `/`、`\`、`..`、控制字符）。
    pub name: String,
    /// 相对组根目录的子路径（空字符串 = 根）。须过
    /// [`super::manifest::is_safe_relative_path`]；按 `/` 计深度 ≤8 级；
    /// `path + "/" + name` 总长 ≤180 字符（给 `save_dir` 前缀留余量，规避
    /// Windows `MAX_PATH` 260 限制）。
    pub path: String,
    /// 已知大小（字节）。`None`/负数按未知（0）处理。
    pub size: Option<i64>,
    /// 条目种类。合法值 `""`/`"file"`（预留 `"folder"` 懒展开，v1 不实现，
    /// 其余值一律拒绝）。
    pub kind: String,
    /// 可选规格（画质/格式）。空 = 无规格选择，`resolver_item` 直接取 `id`；
    /// 非空时取 `id@variants[0].id`（引擎自动全选场景取首个规格——初段清单
    /// 无 default 语义）。≤50 个。
    pub variants: Vec<ManifestVariant>,
}

/// [`ManifestItem::variants`] 的单个规格。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ManifestVariant {
    /// 插件自定义标识，拼进 `resolver_item`（`<itemId>@<variantId>`）。非空。
    pub id: String,
    /// 展示标签。非空。
    pub label: String,
    /// 已知大小（字节）。`None`/负数按未知（0）处理。
    pub size: Option<i64>,
}

/// [`ResolveResult::variants`] 的单个变体。`label` 必填（展示用）；url 为该变体
/// 直链；其余字段缺省时不覆盖顶层值。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResolveVariant {
    /// 展示标签（如 `"1080p MP4"`）。必填非空。
    pub label: String,
    /// 该变体的视频/主直链。必填非空。
    pub url: String,
    /// 可选音频直链（DASH 音视频分离）。
    pub audio_url: Option<String>,
    /// 可选文件名（覆盖顶层 file_name）。
    pub file_name: Option<String>,
    /// 可选总字节数（覆盖顶层 total_bytes）。
    pub total_bytes: Option<i64>,
    /// 码率（bps），未知为 0（仅展示排序用）。
    pub bandwidth: i64,
    /// 视频宽度（像素），未知为 0。
    pub width: i64,
    /// 视频高度（像素），未知为 0。
    pub height: i64,
    /// 容器格式（如 `"mp4"`），可为空。
    pub container: String,
}

/// 通知事件。每个变体都带 `url`（= source_url），供 `notify()` 的 `match.urls` 过滤。
#[derive(Debug, Clone, Serialize)]
// `rename_all` 只重命名变体名；变体内字段须 `rename_all_fields` 才 camelCase 化
// （否则 JS 侧 ctx.task_id 而非 ctx.taskId，与 ResolveRequest 结构体不一致）。
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "event"
)]
pub enum PluginEvent {
    Start {
        task_id: String,
        url: String,
    },
    Error {
        task_id: String,
        url: String,
        message: String,
    },
    Done {
        task_id: String,
        url: String,
        file_path: String,
        /// 轨对任务 mux 失败降级时，独立音频 sidecar 的绝对路径
        /// （`<stem>.audio.m4a`）；单文件产物（含 mux 成功）为 `None`。
        audio_path: Option<String>,
        /// 轨对任务（视频+音频离散轨）是否成功 mux 为单文件；非轨对任务恒 `false`。
        muxed: bool,
    },
    MetaProbed {
        task_id: String,
        url: String,
        file_name: String,
        total_bytes: i64,
    },
    /// 用户关闭「选择插件」/ 变体选择对话框而取消（任务被暂停）终态通知
    /// （593#1）。与 onDone/onError 互斥：只在 generation 未被 on_task_done
    /// 接管的路径上发出，同一任务不会重复触发终态钩子。仅用于通知插件释放已转存
    /// 的临时远程资源，不影响任务状态机、不支持重试。
    Cancel {
        task_id: String,
        url: String,
        reason: CancelReason,
    },
}

/// `onCancel` 载荷的取消原因；每个变体都对应一条真实的引擎发射路径。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// 用户关闭了「选择插件」对话框（任务被暂停）。
    UserCancelled,
    /// 用户关闭了 resolver 多变体选择框。
    VariantSelectionCancelled,
}

impl PluginEvent {
    /// 事件类型对应的 JS 全局函数名。
    pub fn hook_fn_name(&self) -> &'static str {
        match self {
            PluginEvent::Start { .. } => "onStart",
            PluginEvent::Error { .. } => "onError",
            PluginEvent::Done { .. } => "onDone",
            PluginEvent::MetaProbed { .. } => "onMetaProbed",
            PluginEvent::Cancel { .. } => "onCancel",
        }
    }

    /// 事件在 manifest `events` 声明中的名字（与 `hook_fn_name` 相同）。
    pub fn declared_name(&self) -> &'static str {
        self.hook_fn_name()
    }

    /// 事件的 source_url，供 match.urls 过滤。
    pub fn url(&self) -> &str {
        match self {
            PluginEvent::Start { url, .. }
            | PluginEvent::Error { url, .. }
            | PluginEvent::Done { url, .. }
            | PluginEvent::MetaProbed { url, .. }
            | PluginEvent::Cancel { url, .. } => url,
        }
    }
}

/// 插件日志级别，转发到文件日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginLogLevel {
    Info,
    Warn,
    Error,
}

/// 插件系统错误。`Overloaded`/`Timeout`/`MemoryLimitExceeded` 是 fail-closed 触发点。
#[derive(thiserror::Error, Debug)]
pub enum PluginError {
    #[error("manifest 非法: {0}")]
    ManifestInvalid(String),
    #[error("脚本编译失败: {0}")]
    CompileFailed(String),
    #[error("脚本执行超时")]
    Timeout,
    #[error("脚本超出内存上限")]
    MemoryLimitExceeded,
    #[error("插件运行时过载（并发已满）")]
    Overloaded,
    #[error("插件输出非法: {0}")]
    InvalidOutput(String),
    #[error("缺少必填设置项: {0}")]
    MissingRequiredSetting(String),
    #[error("插件运行时错误: {0}")]
    Runtime(String),
    /// 插件目录 / dev 登记存在但加载失败；内容即加载诊断原文。
    #[error("插件加载失败: {0}")]
    LoadFailed(String),
    /// 指定标识没有 dev 登记（只有 dev 插件支持重新加载）。
    #[error("不是开发模式插件: {0}")]
    NotDevPlugin(String),
    /// 某个设置项的值不合法（类型/范围/选项/pattern）或设置键不存在；`key` 供宿主把
    /// 错误归因到具体字段，`message` 是面向用户的原因。
    #[error("{message}")]
    InvalidSetting { key: String, message: String },
    /// 公开渠道的安装撞上了私有商店来源的同标识插件。
    #[error("插件 {0} 由 FluxCloud 私有商店安装，公开渠道不能覆盖它")]
    PrivateSource(String),
}

// ---------------------------------------------------------------------------
// bridge 侧 HTTP 结构：JS 侧 flux.fetch(opts) 的 opts / 返回值字段名 == camelCase。
// ---------------------------------------------------------------------------

/// `flux.fetch(opts)` 的请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BridgeHttpRequest {
    /// 默认 GET。
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    /// 显式认证引用；为空时 bridge 按插件 ID + 请求站点推导默认引用。
    #[serde(default)]
    pub auth_ref: Option<String>,
    /// 由运行时注入，插件脚本不能自行打开认证能力。
    #[serde(skip)]
    pub auth_allowed: bool,
    /// 由运行时注入（仅开发模式插件为 true），插件脚本不能自行打开：
    /// 允许访问环回 / 私网等非公网地址，便于连接本机服务联调。
    #[serde(skip)]
    pub private_network_allowed: bool,
    /// 由运行时注入：本次插件调用的出口（见 [`super::call_egress`]）。`None` = 无调用级
    /// 出口（常驻宿主、通知钩子等），bridge 按全局规则 + 全局模式选出口。
    #[serde(skip)]
    pub egress: Option<Arc<super::call_egress::CallEgress>>,
}

impl Default for BridgeHttpRequest {
    fn default() -> Self {
        Self {
            method: "GET".to_string(),
            url: String::new(),
            headers: HashMap::new(),
            body: None,
            auth_ref: None,
            auth_allowed: false,
            private_network_allowed: false,
            egress: None,
        }
    }
}

/// `flux.fetch(opts)` 的返回值。`body` 为文本；二进制场景 v1 不支持。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeHttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// ffmpeg bridge：单一 near-raw argv 面（flux.ffmpeg.run / .available）。
// ---------------------------------------------------------------------------

/// `flux.ffmpeg.run(spec)` 的请求。**近乎全量 ffmpeg CLI**：`args` 直传给 ffmpeg
/// 二进制（不含程序名），仅经 bridge 侧「封网 + 封越牢路径」校验（见
/// [`super::bridge`]）。文件引用一律用**相对路径**（相对 cwd = 牢笼根/`subdir`），
/// 绝对路径 / `..` / URL scheme 会被拒。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FfmpegSpec {
    /// ffmpeg 参数数组（不含二进制名与自动注入的 `-nostdin`）。
    pub args: Vec<String>,
    /// 牢笼根下的工作子目录（可空；须为安全相对路径）。缺省时 cwd = 牢笼根本身。
    pub subdir: Option<String>,
    /// 本次调用超时（毫秒）。缺省取 bridge 默认值，并被 bridge 上限裁剪。
    pub timeout_ms: Option<u64>,
}

/// `flux.ffmpeg.run(spec)` 的返回值。`stdout`/`stderr` 均按 bridge 上限截断。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegOutcome {
    /// 进程退出码（被信号杀死或无码时为 -1）。
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// 命中超时被强杀。
    pub timed_out: bool,
    pub truncated_stdout: bool,
    pub truncated_stderr: bool,
}

/// `flux.ffmpeg.available()` 的返回值（探测生效 ffmpeg 路径）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegAvailability {
    pub available: bool,
    /// `ffmpeg -version` 探测到的版本串（不可用时为空）。
    pub version: String,
    /// 路径来源：`"manual"`/`"managed"`/`"system"`/`"none"`。
    pub source: String,
}

// ---------------------------------------------------------------------------
// yt-dlp bridge：单一 near-raw argv 面（flux.ytdlp.run / .available）。
// ---------------------------------------------------------------------------

/// `flux.ytdlp.run(spec)` 的请求。**近乎全量 yt-dlp CLI**：`args` 直传给 yt-dlp
/// 二进制（不含程序名），仅经 bridge 侧校验（见 [`super::bridge`]）。与 ffmpeg
/// 不同，yt-dlp 是网络工具——**URL 参数允许**；但越牢文件路径、以及会执行外部
/// 程序 / 加载任意配置或插件的开关（`--exec`/`--downloader`/`--config-location`/
/// `--plugin-dirs`/`--ffmpeg-location`/`--batch-file` 等）一律被拒。文件引用一律
/// 用**相对路径**（相对 cwd = 牢笼根/`subdir`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct YtdlpSpec {
    /// yt-dlp 参数数组（不含二进制名与宿主自动前置的 `--ignore-config` /
    /// `--ffmpeg-location`；后者指向宿主解析到的可信 ffmpeg，插件自带的同名开关被拒）。
    pub args: Vec<String>,
    /// 牢笼根下的工作子目录（可空；须为安全相对路径）。缺省时 cwd = 牢笼根本身。
    pub subdir: Option<String>,
    /// 本次调用超时（毫秒）。缺省取 bridge 默认值，并被 bridge 上限裁剪。
    pub timeout_ms: Option<u64>,
    /// 宿主在 JS 边界注入（不来自插件 JSON）：排队等待 yt-dlp 并发槽的时间由此
    /// 从脚本墙钟里扣除，避免排队被误判为插件自身超时。
    #[serde(skip)]
    pub wall_pause: Option<Arc<WallPause>>,
    /// 宿主在 JS 边界注入：本次插件调用的出口（见 [`BridgeHttpRequest::egress`]）。
    #[serde(skip)]
    pub egress: Option<Arc<super::call_egress::CallEgress>>,
}

/// 累计「不应计入脚本墙钟」的排队等待时长（含进行中的等待）。
#[derive(Debug, Default)]
pub struct WallPause {
    state: std::sync::Mutex<WallPauseState>,
}

#[derive(Debug, Default)]
struct WallPauseState {
    done: Duration,
    waiting: usize,
    since: Option<std::time::Instant>,
}

/// 一次排队等待的 RAII 守卫：drop 时结算（覆盖 future 被取消）。
pub struct WallPauseGuard(Arc<WallPause>);

impl WallPause {
    pub fn wait(self: &Arc<Self>) -> WallPauseGuard {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.waiting == 0 {
            s.since = Some(std::time::Instant::now());
        }
        s.waiting += 1;
        WallPauseGuard(self.clone())
    }

    /// 已结算 + 进行中的等待总时长。
    pub fn total(&self) -> Duration {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.done + s.since.map(|t| t.elapsed()).unwrap_or_default()
    }
}

impl Drop for WallPauseGuard {
    fn drop(&mut self) {
        let mut s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        s.waiting = s.waiting.saturating_sub(1);
        if s.waiting == 0
            && let Some(t) = s.since.take()
        {
            s.done += t.elapsed();
        }
    }
}

/// `flux.ytdlp.run(spec)` 的返回值。`stdout`/`stderr` 均按 bridge 上限截断。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YtdlpOutcome {
    /// 进程退出码（被信号杀死或无码时为 -1）。
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// 命中超时被强杀。
    pub timed_out: bool,
    pub truncated_stdout: bool,
    pub truncated_stderr: bool,
}

/// `flux.ytdlp.available()` 的返回值（探测生效 yt-dlp 路径）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YtdlpAvailability {
    pub available: bool,
    /// `yt-dlp --version` 探测到的版本串（不可用时为空）。
    pub version: String,
    /// 路径来源：`"manual"`/`"managed"`/`"system"`/`"none"`。
    pub source: String,
}

/// 单次插件调用的**宿主侧上下文**（不跨 JS 边界，插件不可设置）。承载 ffmpeg
/// 能力门 + 文件访问牢笼根，由 manager 按调用上下文（事件/resolve）注入。
#[derive(Debug, Clone, Default)]
pub struct HostContext {
    /// manifest `permissions` 是否含 `"ffmpeg"`——决定是否注入 `flux.ffmpeg` 门面。
    pub ffmpeg_permitted: bool,
    /// ffmpeg 允许读写的牢笼根（通常为任务 `save_dir`）。`None` = 无牢笼
    /// （resolve / 无产物事件）→ `flux.ffmpeg.run` 一律被拒。
    pub ffmpeg_root: Option<PathBuf>,
    /// manifest `permissions` 是否含 `"ytdlp"`——决定是否注入 `flux.ytdlp` 门面。
    /// yt-dlp 的文件牢笼由 bridge 自持（每插件 scratch 目录），故此处只需授权门，
    /// 无需牢笼根：resolve 与全部 hook 上下文下授权即可用（区别于 ffmpeg）。
    pub ytdlp_permitted: bool,
    /// manifest `permissions` 是否含 `"auth"`——决定是否允许插件读取/保存
    /// 通用认证凭据以及让 `flux.fetch` 自动注入凭据。
    pub auth_permitted: bool,
    /// 插件是否为开发模式（「从目录安装」）。仅此类插件的 `flux.fetch` 可访问
    /// 非公网地址（环回 / 私网）；正式安装的插件恒为 false。
    pub private_network_allowed: bool,
    /// 本次调用的出口（见 [`super::call_egress`]）；`None` = 全局出口。
    pub egress: Option<Arc<super::call_egress::CallEgress>>,
}

// ---------------------------------------------------------------------------
// 常驻宿主（`main`）：可移植视图 / 命令 / RPC / 事件 / 定时器。
// ---------------------------------------------------------------------------

/// `flux.tasks.*` 的出口：把任务操作转给 daemon 服务（方法白名单由实现方负责）。
/// `params` / 返回值均为 daemon wire 形状。
#[async_trait::async_trait]
pub trait PluginTaskPort: Send + Sync {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError>;
}

/// 一次页面脚本调用（[`HostCall::Invoke`] 且 `page_url` 为 `Some`）执行期间，插件经
/// `flux.tasks.create` / `flux.tasks.resume` 成功开始的任务。宿主据此把用户在浏览器里的
/// 操作登记为「用户开始」（进度 / 完成窗口）；其余调用来源（视图、命令、事件、定时器）恒为空。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartedTasks {
    /// `create` 返回的新任务 ID，按调用顺序。
    pub created: Vec<String>,
    /// `resume` 成功的已有任务 ID，按调用顺序。
    pub resumed: Vec<String>,
}

/// 投递给常驻宿主的一次调用。
#[derive(Debug, Clone)]
pub enum HostCall {
    ViewOpen {
        session: String,
        view: String,
        props: serde_json::Value,
        task_id: Option<String>,
        locale: String,
    },
    /// `event`：`{node, type, value?, key?, action?}`。
    ViewEvent {
        session: String,
        event: serde_json::Value,
    },
    ViewRefresh {
        session: String,
    },
    ViewClose {
        session: String,
    },
    Command {
        id: String,
        task_ids: Vec<String>,
    },
    /// `page_url` 为 `Some` 表示来自浏览器页面脚本（只命中 `{ page: true }` 处理器，
    /// ctx 为 `{ caller: "page", pageUrl }`）；`None` 表示桌面视图（ctx 为 `{ caller: "desktop" }`）。
    Invoke {
        name: String,
        args: serde_json::Value,
        page_url: Option<String>,
    },
    Event(PluginEvent),
    Timer {
        id: u32,
    },
    /// 设置值变化：`json` 为类型化设置对象，宿主就地更新 `flux.settings`。
    Settings {
        json: String,
    },
}

/// 宿主调用的返回值。
#[derive(Debug, Clone)]
pub enum HostReply {
    /// 视图树（未规范化的原始 JSON）。
    Tree(serde_json::Value),
    /// 命令 / RPC 的返回值；`started` 只有页面脚本调用可能非空。
    Value {
        value: serde_json::Value,
        started: StartedTasks,
    },
    None,
}

/// 宿主调用的 future。入队在 [`PluginHost::call`] 返回之前同步完成，因此调用方
/// 先后调用 `call` 的顺序就是宿主的处理顺序。
pub type HostFuture =
    std::pin::Pin<Box<dyn Future<Output = Result<HostReply, PluginError>> + Send>>;

/// 已启动的常驻宿主（一个插件一个，独占一份 JS 运行时与上下文，调用串行执行）。
pub trait PluginHost: Send + Sync {
    /// 入队一次调用；返回的 future 在处理完成时就绪。
    fn call(&self, call: HostCall) -> HostFuture;
    /// 停止宿主并释放 JS 运行时；排队中与进行中的调用以 `Runtime` 错误结束。
    fn dispose(&self);
}

/// 宿主回调（JS → Rust 的异步旁路）。
#[async_trait::async_trait]
pub trait HostEvents: Send + Sync {
    /// `ctx.refresh()`：请求重新渲染某个会话。
    fn request_render(&self, session: &str);
    /// `flux.ui.toast`。
    fn toast(&self, identity: &str, level: &str, text: &str);
    /// `ctx.close()`：以 `requested` 原因结束调用方自己的会话。
    fn close_view(&self, identity: &str, session: &str) -> Result<(), PluginError>;
    /// `ctx.navigate(target)`：校验目标后向客户端发出跳转请求。
    async fn navigate(
        &self,
        identity: &str,
        session: &str,
        target: serde_json::Value,
    ) -> Result<(), PluginError>;
    /// 安排一个一次性定时器：`delay` 后向宿主投递 [`HostCall::Timer`]。
    fn timer(&self, identity: &str, id: u32, delay: Duration);
    /// 取消尚未触发的定时器。
    fn timer_clear(&self, identity: &str, id: u32);
    /// `flux.settings.update()`，默认实现明确拒绝未配置的宿主。
    async fn settings_update(
        &self,
        _identity: &str,
        _entries: Vec<(String, String)>,
    ) -> Result<(), PluginError> {
        Err(PluginError::Runtime(
            "plugin settings update is unsupported".to_string(),
        ))
    }
    /// Retrieve or refresh this plugin's signed archive rule pack.
    async fn archive_rules(
        &self,
        _identity: &str,
        _refresh: bool,
    ) -> Result<serde_json::Value, PluginError> {
        Err(PluginError::Runtime(
            "archive rule management is unsupported".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// trait
// ---------------------------------------------------------------------------

/// 脚本运行时抽象。v1 由 quickjs 实现；未来 deno_core 实现同一 trait。
#[async_trait::async_trait]
pub trait ScriptRuntime: Send + Sync {
    /// 编译期检查源码语法（不执行副作用）。
    fn check_compile(&self, source: &str) -> Result<(), PluginError>;

    /// 用 JS `RegExp` 校验 pattern 语法是否合法（能否 `new RegExp(pattern)`）。
    fn regex_valid(&self, pattern: &str) -> bool;

    /// 用 JS `RegExp` 测试 `value` 是否匹配 `pattern`；pattern 非法时返回 false。
    fn regex_test(&self, pattern: &str, value: &str) -> bool;

    /// 宿主并发上限变化时同步 resolve/subscription 的并发容量（实际容量
    /// `max(max_concurrent, 内部 worker 数)`）。上限放大后，新增并发槽里的插件
    /// 解析任务才不会因拿不到 permit 而 `Overloaded`。
    fn set_resolve_capacity(&self, max_concurrent: usize);

    /// 调用 `globalThis.resolve(ctx)`。返回 `Ok(None)` = 放行不改写。
    /// `settings_json` 为 manager 预构建的**类型化**只读设置 JSON 对象字符串
    /// （string→JS string、number→JS number、boolean→JS boolean），注入为
    /// `flux.settings`。
    async fn invoke_resolve(
        &self,
        plugin: &PluginScript,
        req: ResolveRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    ) -> Result<Option<ResolveResult>, PluginError>;

    /// 调用隔离的 `globalThis.archive(ctx)`：只注入 `bridge` 的日志能力
    /// （`flux.logger` / `console`），不提供任何网络、存储、文件、任务或进程接口。
    async fn invoke_archive(
        &self,
        plugin: &PluginScript,
        req: ArchiveRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
    ) -> Result<Option<ArchiveResult>, PluginError>;

    /// 调用 `globalThis.intercept(ctx)`（下载拦截平面）。`Ok(None)` = 不改动；
    /// `Ok(Some(raw))` 为插件返回值的 JSON 文本。`long_running` 走外部工具信号量
    /// （finalize 阶段可能运行 ffmpeg / yt-dlp 数十分钟）。
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
    ) -> Result<Option<String>, PluginError>;

    /// 调用 `globalThis.authenticate(ctx)`，返回一次登录交互状态。
    async fn invoke_auth(
        &self,
        plugin: &PluginScript,
        req: AuthRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    ) -> Result<AuthResult, PluginError>;

    /// 调用 `globalThis.subscribe(ctx)`，返回插件规范化后的 JSON 字符串。
    async fn invoke_subscription(
        &self,
        plugin: &PluginScript,
        req: SubscriptionRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    ) -> Result<String, PluginError>;

    /// 通知钩子；**全部事件（含 Error）统一 fire-and-forget，实现方吞掉一切错误
    /// （仅日志），无返回值**。重试意图由脚本经 [`PluginBridge::request_retry`]
    /// 命令式发起，不走返回值通道。`settings_json` 同 [`Self::invoke_resolve`]。
    async fn invoke_hook(
        &self,
        plugin: &PluginScript,
        event: PluginEvent,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
        host: HostContext,
    );

    /// 启动一个常驻宿主：在插件 runtime 上 spawn 一个 actor，独占一份 JS 运行时，
    /// 依次执行 flux 门面、宿主门面与 `main` 源码，之后经返回的句柄串行接收调用。
    /// `task_port` 仅在 manifest 授予 `tasks` 权限时为 `Some`（决定是否注入
    /// `flux.tasks`）。
    #[allow(clippy::too_many_arguments)]
    async fn start_host(
        &self,
        plugin: PluginScript,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        host: HostContext,
        task_port: Option<Arc<dyn PluginTaskPort>>,
        events: Arc<dyn HostEvents>,
    ) -> Result<Arc<dyn PluginHost>, PluginError>;

    /// 供 off-actor worker `spawn` 用的 tokio `Handle`（专用 multi_thread runtime）。
    /// **禁止裸 `tokio::spawn`**——那可能把 resolve future 落到宿主的
    /// current_thread runtime 唯一线程上、冻结全命令面。
    fn spawn_handle(&self) -> tokio::runtime::Handle;
}

/// 宿主向脚本暴露的能力桥。`flux.settings` 不在此——它由 manager 从 manifest+config
/// 构建为类型化 JSON 后经 invoke 方法传入（bridge 无 manifest 语义，无法做类型化）。
#[async_trait::async_trait]
pub trait PluginBridge: Send + Sync {
    /// `flux.fetch`：经守卫 Client 发 HTTP 请求（防 SSRF）。
    async fn http_request(
        &self,
        plugin_id: &str,
        req: BridgeHttpRequest,
    ) -> Result<BridgeHttpResponse, PluginError>;

    /// 读取插件自己的认证档案。默认 bridge 不提供认证能力。
    async fn auth_get(
        &self,
        _plugin_id: &str,
        _auth_ref: &str,
    ) -> Result<Option<crate::auth::AuthProfile>, PluginError> {
        Ok(None)
    }

    /// 写入或替换插件自己的认证档案，返回稳定 authRef。
    async fn auth_save(
        &self,
        _plugin_id: &str,
        _profile: crate::auth::AuthProfile,
    ) -> Result<String, PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持认证存储".to_string()))
    }

    /// 删除插件自己的认证档案。
    async fn auth_remove(&self, _plugin_id: &str, _auth_ref: &str) -> Result<(), PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持认证存储".to_string()))
    }

    /// `flux.storage.get`。
    async fn storage_get(&self, plugin_id: &str, key: &str) -> Option<String>;

    /// `flux.storage.set`。值 ≤64KB、单插件 ≤100 键。
    async fn storage_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: String,
    ) -> Result<(), PluginError>;

    /// `flux.logger.*` / `console.*`。
    fn log(&self, plugin_id: &str, level: PluginLogLevel, message: &str);

    /// 命令式重试请求（onError 钩子专用，复刻 gopeed `ctx.task.continue()`）：
    /// 内部经 `plugin_retry_tx` 通道发起延迟 resume；限流在 actor 侧
    /// （`max_auto_retries`）；不阻塞、不返回决策。`plugin_id` 供审计归属。
    fn request_retry(&self, plugin_id: &str, task_id: &str, delay_ms: u64);

    /// `flux.task.recordArtifact(name)`：登记任务的衍生产物文件名（同
    /// `save_dir` 下的相对文件名，如转码产物 `<stem>.mp4`）。onDone 钩子专用；
    /// 登记后「删除任务并删除文件」会连同产物一并删除，保证单一任务的所有
    /// 文件成组管理。默认实现拒绝（无持久化能力的 bridge）。
    async fn record_artifact(
        &self,
        _plugin_id: &str,
        _task_id: &str,
        _file_name: &str,
    ) -> Result<(), PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持产物登记".to_string()))
    }

    /// `flux.fs.writeFile(name, content)`：把文本写入插件工作区（每插件 scratch
    /// 目录，与 `flux.ytdlp` 的 cwd 同根）内的**扁平文件名** `name`。供插件为受管
    /// 工具（yt-dlp/ffmpeg…）物化输入文件（cookie/config/字幕…），以相对名喂给
    /// 工具——取代按需给每个工具 spec 加类型化字段的做法。牢笼内限定 + 单文件/
    /// 总量/文件数上限；默认实现拒绝。
    async fn fs_write(
        &self,
        _plugin_id: &str,
        _name: &str,
        _content: String,
    ) -> Result<(), PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持 flux.fs".to_string()))
    }

    /// `flux.fs.readFile(name)`：读回工作区内文件文本（不存在返回 `None`）。
    /// 默认返回 `None`。
    async fn fs_read(&self, _plugin_id: &str, _name: &str) -> Option<String> {
        None
    }

    /// `flux.fs.remove(name)`：删除工作区内文件（不存在视为成功）。默认拒绝。
    async fn fs_remove(&self, _plugin_id: &str, _name: &str) -> Result<(), PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持 flux.fs".to_string()))
    }

    /// `flux.fs.list()`：列出工作区内顶层文件名（不含子目录 / yt-dlp `.cache`）。
    /// 默认返回空。
    async fn fs_list(&self, _plugin_id: &str) -> Vec<String> {
        Vec::new()
    }

    /// `flux.ffmpeg.available()`：探测生效 ffmpeg（manual→managed→system）。
    /// 只读、不触网、不落盘。默认实现返回 `None`（无 ffmpeg 支持的 bridge）。
    async fn ffmpeg_available(&self) -> Option<FfmpegAvailability> {
        None
    }

    /// `flux.ffmpeg.run(spec)`：在 `jail_root` 牢笼内执行 ffmpeg。
    ///
    /// `jail_root` 由宿主按调用上下文注入（见 [`HostContext::ffmpeg_root`]），
    /// **插件无法设置**；实现方须把一切文件访问约束在 `jail_root` 内、并封死
    /// 网络协议（见 [`super::bridge`] 的校验器）。默认实现拒绝调用。
    async fn run_ffmpeg(
        &self,
        _plugin_id: &str,
        _jail_root: PathBuf,
        _spec: FfmpegSpec,
    ) -> Result<FfmpegOutcome, PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持 ffmpeg".to_string()))
    }

    /// `flux.ffprobe.run(spec)`：在 `jail_root` 牢笼内执行 ffprobe（随 ffmpeg 组件
    /// 一并安装），用于结构化探测（`-print_format json -show_format -show_streams`）。
    /// 与 [`Self::run_ffmpeg`] 同权限门（`ffmpeg`）、同牢笼、同校验；默认实现拒绝。
    async fn run_ffprobe(
        &self,
        _plugin_id: &str,
        _jail_root: PathBuf,
        _spec: FfmpegSpec,
    ) -> Result<FfmpegOutcome, PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持 ffprobe".to_string()))
    }

    /// `flux.ytdlp.available()`：探测生效 yt-dlp（manual→managed→system）。
    /// 只读、不触网、不落盘。默认实现返回 `None`（无 yt-dlp 支持的 bridge）。
    async fn ytdlp_available(&self) -> Option<YtdlpAvailability> {
        None
    }

    /// `flux.ytdlp.run(spec)`：在 bridge 自持的每插件 scratch 牢笼内执行 yt-dlp。
    ///
    /// 实现方须把一切文件访问约束在牢笼内（cwd = 牢笼根/`subdir`，拒绝绝对路径
    /// 与 `..`），并拒绝会执行外部程序 / 加载任意配置或插件的开关（见
    /// [`super::bridge`] 的校验器）；**网络放行**（yt-dlp 的本职）。默认实现拒绝。
    async fn run_ytdlp(
        &self,
        _plugin_id: &str,
        _spec: YtdlpSpec,
    ) -> Result<YtdlpOutcome, PluginError> {
        Err(PluginError::Runtime("此 bridge 不支持 yt-dlp".to_string()))
    }

    /// 卸载插件时清理其 bridge 自持的 scratch 工作区（flux.fs / yt-dlp cwd）。
    /// 默认无操作（无工作区的 bridge）。
    async fn remove_plugin_workspace(&self, _plugin_id: &str) {}
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{PluginEvent, ResolveRequest};
    use std::collections::HashMap;

    #[test]
    fn plugin_event_fields_are_camel_case() {
        // 通知事件跨 JS 边界的字段名必须 camelCase（与 hooks.js 的 ctx.taskId 等一致）。
        let done = PluginEvent::Done {
            task_id: "t1".into(),
            url: "http://x/".into(),
            file_path: "/tmp/a.bin".into(),
            audio_path: Some("/tmp/a.audio.m4a".into()),
            muxed: false,
        };
        let v: serde_json::Value = serde_json::to_value(&done).expect("serialize");
        assert_eq!(v["taskId"], "t1");
        assert_eq!(v["filePath"], "/tmp/a.bin");
        assert_eq!(v["audioPath"], "/tmp/a.audio.m4a");
        assert_eq!(v["muxed"], false);
        assert!(v.get("audio_path").is_none());
        assert!(
            v.get("task_id").is_none(),
            "must not emit snake_case task_id"
        );
        assert!(v.get("file_path").is_none());

        let meta = PluginEvent::MetaProbed {
            task_id: "t2".into(),
            url: "http://y/".into(),
            file_name: "b.mp4".into(),
            total_bytes: 42,
        };
        let mv: serde_json::Value = serde_json::to_value(&meta).expect("serialize");
        assert_eq!(mv["fileName"], "b.mp4");
        assert_eq!(mv["totalBytes"], 42);
    }

    #[test]
    fn resolve_request_fields_are_camel_case() {
        let req = ResolveRequest {
            task_id: "t".into(),
            url: "u".into(),
            auth_ref: String::new(),
            cookies: String::new(),
            referrer: String::new(),
            user_agent: "UA".into(),
            extra_headers: HashMap::new(),
            resolver_item: String::new(),
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["taskId"], "t");
        assert_eq!(v["userAgent"], "UA");
        assert!(v.get("extra_headers").is_none());
        assert!(v.get("extraHeaders").is_some());
    }
}
