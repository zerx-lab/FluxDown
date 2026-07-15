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
    pub cookies: String,
    pub referrer: String,
    pub user_agent: String,
    pub extra_headers: HashMap<String, String>,
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
}

impl PluginEvent {
    /// 事件类型对应的 JS 全局函数名。
    pub fn hook_fn_name(&self) -> &'static str {
        match self {
            PluginEvent::Start { .. } => "onStart",
            PluginEvent::Error { .. } => "onError",
            PluginEvent::Done { .. } => "onDone",
            PluginEvent::MetaProbed { .. } => "onMetaProbed",
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
            | PluginEvent::MetaProbed { url, .. } => url,
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
}

impl Default for BridgeHttpRequest {
    fn default() -> Self {
        Self {
            method: "GET".to_string(),
            url: String::new(),
            headers: HashMap::new(),
            body: None,
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

    /// 供 off-actor worker `spawn` 用的 tokio `Handle`（专用 multi_thread runtime）。
    /// **禁止裸 `tokio::spawn`**——那会把 resolve future 落到 hub 的 current_thread
    /// 唯一线程上、冻结全命令面。
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
    /// （`max_auto_retries`）；不阻塞、不返回决策。
    fn request_retry(&self, task_id: &str, delay_ms: u64);

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
            cookies: String::new(),
            referrer: String::new(),
            user_agent: "UA".into(),
            extra_headers: HashMap::new(),
        };
        let v: serde_json::Value = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["taskId"], "t");
        assert_eq!(v["userAgent"], "UA");
        assert!(v.get("extra_headers").is_none());
        assert!(v.get("extraHeaders").is_some());
    }
}
