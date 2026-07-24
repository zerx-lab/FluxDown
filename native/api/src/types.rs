//! Wire 数据类型 —— HTTP API 的对外 JSON 契约。
//!
//! 独立于 `fluxdown_engine::model` 定义：API 一经发布（`/api/v1`）即为对外稳定
//! 契约，引擎内部模型重构不得破坏线上 JSON 格式。两者通过 `From` 转换衔接
//! （与 hub 侧 `signal_bridge` 对 rinf 信号做的事完全对称）。
//!
//! 字段命名统一 camelCase（与浏览器扩展协议、Gopeed API 风格一致）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// 外部下载请求（浏览器扩展 NMH / 油猴脚本接管 / aria2 兼容层共用）
// ---------------------------------------------------------------------------

/// 浏览器原始请求体（form POST / XHR raw body 等）。
///
/// 当用户在 form-submit 触发的下载中点击下载按钮时，浏览器实际发起的是
/// POST 请求并携带表单数据；扩展通过 `webRequest.onBeforeRequest` 抓到 method
/// 与 body 后透传到此字段。宿主端按 `kind` 重建请求体。
///
/// 协议字段：
/// - `formData`：来自 `requestBody.formData`，宿主用 `reqwest::form()` 编码为
///   `application/x-www-form-urlencoded`
/// - `urlencoded`：扩展端已序列化好的 url-encoded 字符串（直接作为 body 发送）
/// - `raw`：base64 编码的二进制 body（XHR / fetch 直接发送 ArrayBuffer 的场景）
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RequestBody {
    FormData {
        fields: HashMap<String, Vec<String>>,
    },
    Urlencoded {
        raw: String,
    },
    Raw {
        #[serde(rename = "bytesB64")]
        bytes_b64: String,
        #[serde(rename = "contentType", default)]
        content_type: Option<String>,
    },
}

impl From<RequestBody> for fluxdown_engine::downloader::CapturedRequestBody {
    /// wire 形态 → 引擎传输无关形态（与 hub 侧 NMH 的同名转换语义一致）。
    ///
    /// # Examples
    ///
    /// ```
    /// use fluxdown_api::types::RequestBody;
    /// use fluxdown_engine::downloader::CapturedRequestBody;
    ///
    /// let wire = RequestBody::Urlencoded { raw: "k=v".to_string() };
    /// let captured = CapturedRequestBody::from(wire);
    /// assert!(matches!(captured, CapturedRequestBody::Urlencoded { raw } if raw == "k=v"));
    /// ```
    fn from(body: RequestBody) -> Self {
        match body {
            RequestBody::FormData { fields } => Self::FormData { fields },
            RequestBody::Urlencoded { raw } => Self::Urlencoded { raw },
            RequestBody::Raw {
                bytes_b64,
                content_type,
            } => Self::Raw {
                bytes_b64,
                content_type,
            },
        }
    }
}

/// 外部下载请求载荷（浏览器扩展 / 油猴脚本 / aria2 兼容层）。
///
/// 由宿主的「外部下载」通道消费：缓存请求事务 → 弹出快速下载确认框 →
/// 用户确认后创建任务。与管理 API 的 [`CreateTaskRequest`]（直接建任务、
/// 无确认框）语义不同。
///
/// # Examples
///
/// ```
/// use fluxdown_api::types::DownloadRequest;
///
/// let req: DownloadRequest =
///     serde_json::from_str(r#"{"url":"https://example.com/f.zip"}"#).unwrap();
/// assert_eq!(req.url, "https://example.com/f.zip");
/// assert!(req.filename.is_empty());
/// ```
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DownloadRequest {
    pub url: String,
    #[serde(default)]
    pub filename: String,
    /// 保存目录（aria2 `dir` 选项 / 接管请求 `saveDir` 字段）。
    /// 空 = 由宿主按分类匹配 / 默认目录决定。
    #[serde(rename = "saveDir")]
    #[serde(default)]
    pub save_dir: String,
    #[serde(default)]
    pub referrer: String,
    #[serde(default)]
    pub cookies: String,
    /// 浏览器请求中捕获的额外 HTTP 头（如 Authorization）。
    /// 由下载引擎在发起请求时附加到请求头中。
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// 文件大小提示（字节）。
    ///   - `>0` = 已知大小，跳过 probe
    ///   - `-1` = 大小未知但确认是下载资源（webRequest 嗅探），跳过 probe
    ///   - `0` / `None` = 正常 probe
    #[serde(rename = "fileSize")]
    #[serde(default)]
    pub file_size: Option<i64>,
    #[serde(rename = "mimeType")]
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 浏览器原始请求方法（"GET" / "POST" / ...）。
    /// 缺省 = "GET"。POST/PUT/PATCH 类请求由 `body` 携带请求体。
    #[serde(default)]
    pub method: Option<String>,
    /// 浏览器原始请求体（仅在非 GET 时有意义）。
    #[serde(default)]
    pub body: Option<RequestBody>,
    /// 音频轨 URL（可选，通用「视频轨+音频轨」离散下载对语义，按 MIME
    /// video/* vs audio/* 分轨判定，非站点专用协议字段）。
    /// 非空 = 这是一对轨道，引擎分别下载两路后用 ffmpeg mux 合并；
    /// 空/缺省 = 普通单 URL 下载。
    #[serde(rename = "audioUrl", default)]
    pub audio_url: Option<String>,
}

// ---------------------------------------------------------------------------
// 管理 API（/api/v1）资源类型
// ---------------------------------------------------------------------------

/// 任务信息（`GET /api/v1/tasks`、`GET /api/v1/tasks/{id}` 响应）。
///
/// # Examples
///
/// ```
/// use fluxdown_api::types::TaskDto;
/// use fluxdown_engine::model::TaskInfo;
///
/// let info = TaskInfo {
///     task_id: "t1".to_string(),
///     url: "https://example.com/f.zip".to_string(),
///     file_name: "f.zip".to_string(),
///     save_dir: "/tmp".to_string(),
///     status: 1,
///     downloaded_bytes: 10,
///     total_bytes: 100,
///     error_message: String::new(),
///     created_at: "1700000000".to_string(),
///     proxy_url: String::new(),
///     queue_id: String::new(),
///     checksum: String::new(),
///     ignore_tls_errors: false,
///     file_missing: false,
///     completed_at: String::new(),
///     segments: 0,
///     queue_order: 0,
///     referrer: String::new(),
/// };
/// let dto = TaskDto::from(info);
/// assert_eq!(dto.task_id, "t1");
/// let json = serde_json::to_string(&dto).unwrap();
/// assert!(json.contains("\"taskId\":\"t1\""));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskDto {
    pub task_id: String,
    pub url: String,
    pub file_name: String,
    pub save_dir: String,
    /// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
    pub status: i32,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub error_message: String,
    /// Unix 秒级时间戳（字符串）。
    pub created_at: String,
    /// 单任务代理 URL（空 = 使用全局代理）。
    pub proxy_url: String,
    /// 命名队列 ID（空 = 默认队列）。
    pub queue_id: String,
    /// Checksum spec，格式 `algo=hexhash`（空 = 跳过校验）。
    pub checksum: String,
    /// 是否显式忽略 HTTPS 证书错误。默认 false（严格验证）。
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// 文件跟踪：completed 任务的目标文件是否已丢失（被删除/移动）。默认 false。
    #[serde(default)]
    pub file_missing: bool,
    /// 任务结束时间，Unix 秒级时间戳（空 = 尚未完成）。
    /// 记录下载真正完成（status→3）的时刻，不含插件 hook 后处理耗时。
    #[serde(default)]
    pub completed_at: String,
    /// Source page URL captured by the browser extension (empty = none).
    #[serde(default)]
    pub referrer: String,
}

impl From<fluxdown_engine::model::TaskInfo> for TaskDto {
    fn from(t: fluxdown_engine::model::TaskInfo) -> Self {
        Self {
            task_id: t.task_id,
            url: t.url,
            file_name: t.file_name,
            save_dir: t.save_dir,
            status: t.status,
            downloaded_bytes: t.downloaded_bytes,
            total_bytes: t.total_bytes,
            error_message: t.error_message,
            created_at: t.created_at,
            proxy_url: t.proxy_url,
            queue_id: t.queue_id,
            checksum: t.checksum,
            ignore_tls_errors: t.ignore_tls_errors,
            file_missing: t.file_missing,
            completed_at: t.completed_at,
            referrer: t.referrer,
        }
    }
}

/// 命名队列信息（`GET /api/v1/queues` 响应）。
///
/// # Examples
///
/// ```
/// use fluxdown_api::types::QueueDto;
/// use fluxdown_engine::model::QueueInfo;
///
/// let q = QueueInfo {
///     queue_id: "q1".to_string(),
///     name: "工作".to_string(),
///     speed_limit_kbps: 0,
///     max_concurrent: 3,
///     default_save_dir: String::new(),
///     position: 0,
///     default_segments: 0,
///     default_user_agent: String::new(),
///     is_running: true,
///     schedule_enabled: false,
///     schedule_start: String::new(),
///     schedule_stop: String::new(),
///     schedule_days: 127,
/// };
/// let dto = QueueDto::from(q);
/// assert_eq!(dto.queue_id, "q1");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueDto {
    pub queue_id: String,
    pub name: String,
    /// 队列限速（KB/s），0 = 不限速。
    pub speed_limit_kbps: i64,
    /// 队列并发上限，0 = 跟随全局。
    pub max_concurrent: i32,
    pub default_save_dir: String,
    pub position: i32,
    pub default_segments: i32,
    pub default_user_agent: String,
    /// 队列运行状态：停止的队列不自动启动其中任务。
    #[serde(default = "default_true")]
    pub is_running: bool,
    /// 每日定时计划是否启用。
    #[serde(default)]
    pub schedule_enabled: bool,
    /// 每日定时启动时间 `HH:MM`（空 = 不定时启动）。
    #[serde(default)]
    pub schedule_start: String,
    /// 每日定时停止时间 `HH:MM`（空 = 不定时停止）。
    #[serde(default)]
    pub schedule_stop: String,
    /// 定时生效星期位掩码：bit0=周一 … bit6=周日；127 = 每天。
    #[serde(default = "default_schedule_days")]
    pub schedule_days: i32,
}

impl From<fluxdown_engine::model::QueueInfo> for QueueDto {
    fn from(q: fluxdown_engine::model::QueueInfo) -> Self {
        Self {
            queue_id: q.queue_id,
            name: q.name,
            speed_limit_kbps: q.speed_limit_kbps,
            max_concurrent: q.max_concurrent,
            default_save_dir: q.default_save_dir,
            position: q.position,
            default_segments: q.default_segments,
            default_user_agent: q.default_user_agent,
            is_running: q.is_running,
            schedule_enabled: q.schedule_enabled,
            schedule_start: q.schedule_start,
            schedule_stop: q.schedule_stop,
            schedule_days: q.schedule_days,
        }
    }
}

/// 创建任务请求（`POST /api/v1/tasks`）。
///
/// 与外部下载请求 [`DownloadRequest`] 不同：本请求**直接创建任务**，
/// 不经过快速下载确认弹框（管理 API 的调用方是受信任的自动化客户端）。
///
/// # Examples
///
/// ```
/// use fluxdown_api::types::CreateTaskRequest;
///
/// let req: CreateTaskRequest =
///     serde_json::from_str(r#"{"url":"https://example.com/f.zip","segments":8}"#).unwrap();
/// assert_eq!(req.url, "https://example.com/f.zip");
/// assert_eq!(req.segments, 8);
/// assert!(req.save_dir.is_empty()); // 空 = 使用全局默认保存目录
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskRequest {
    pub url: String,
    /// 空 = 从 URL / Content-Disposition 推断。
    #[serde(default)]
    pub file_name: String,
    /// 空 = 使用全局默认保存目录。
    #[serde(default)]
    pub save_dir: String,
    /// 0 = 由 segment_advisor 按文件大小动态决定。
    #[serde(default)]
    pub segments: i32,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub referrer: String,
    /// 单任务代理 URL（空 = 使用全局代理）。
    #[serde(default)]
    pub proxy_url: String,
    /// 空 = 使用全局 User-Agent。
    #[serde(default)]
    pub user_agent: String,
    /// 命名队列 ID（空 = 默认队列）。
    #[serde(default)]
    pub queue_id: String,
    /// Checksum spec，格式 `algo=hexhash`（空 = 跳过校验）。
    #[serde(default)]
    pub checksum: String,
    /// 忽略 HTTPS 证书错误。缺省 false（严格验证）。
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// 附加 HTTP 请求头。
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// BT 种子文件字节（base64 编码，aria2 `addTorrent` 兼容入口）。
    /// 非空时按种子任务创建，`url` 允许为空占位。
    #[serde(default)]
    pub torrent_b64: Option<String>,
    /// 浏览器原始 HTTP method（`"GET"`/`"POST"`/…）。缺省 = GET。
    /// form-POST 触发的下载必须携带，否则引擎用 GET 重发会拿到错误内容。
    #[serde(default)]
    pub method: Option<String>,
    /// 浏览器原始请求体（仅非 GET 时有意义）。
    #[serde(default)]
    pub body: Option<RequestBody>,
    /// 音频轨 URL（「视频轨+音频轨」离散下载对语义）。
    /// 非空 = 引擎分别下载两路后 mux 合并；空/缺省 = 普通单 URL 下载。
    #[serde(default)]
    pub audio_url: Option<String>,
    /// 稍后下载：true = 建任务后不启动（paused 落库），待「启动队列」
    /// 按序恢复或用户手动恢复。缺省 false = 立即开始。
    #[serde(default)]
    pub start_paused: bool,
}

fn default_true() -> bool {
    true
}

fn default_schedule_days() -> i32 {
    127
}

/// 创建任务响应（`POST /api/v1/tasks`）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedTask {
    pub task_id: String,
}

/// 应用信息（`GET /api/v1/info` 响应）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiInfo {
    pub name: String,
    pub version: String,
}

/// 通用结果响应（接管端点应答 / 各端点错误响应统一格式）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ResultMessage {
    pub success: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// 插件系统 DTO（camelCase；双向 serde + ToSchema）
// ---------------------------------------------------------------------------

/// select 控件选项。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SettingOptionDto {
    pub value: String,
    pub label: String,
}

/// 声明式设置项（镜像 `engine::plugin::SettingField`，api 本地定义）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingFieldDto {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// `string` / `number` / `boolean`。
    #[serde(rename = "type")]
    pub setting_type: String,
    /// `text`/`password`/`textarea`/`select`/`toggle`/`number`/`folder`。
    pub widget: String,
    #[serde(default)]
    pub options: Vec<SettingOptionDto>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub pattern: Option<String>,
    /// 辅助脚本（非空时 UI 在字段旁渲染复制按钮，仅复制文本、绝不执行）。
    #[serde(default)]
    pub helper_script: Option<String>,
    /// 辅助脚本按钮文案（空则用默认文案）。
    #[serde(default)]
    pub helper_label: Option<String>,
}

/// 已安装插件视图（列表/设置表单）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginDto {
    pub identity: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    pub enabled: bool,
    pub dev_mode: bool,
    /// `None` / `Manual` / `CircuitBreaker`。
    pub disabled_reason: String,
    pub settings: Vec<SettingFieldDto>,
    /// 当前设置值（key → value 字符串）。
    pub settings_values: HashMap<String, String>,
    /// manifest 声明的能力权限（如 `["ffmpeg"]`，供 UI 展示授权徽章）。
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// 安装 dev 插件请求体。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallPluginDevRequest {
    pub dir_path: String,
}

/// 设置插件启用状态请求体。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetPluginEnabledRequest {
    pub enabled: bool,
}

/// 安装成功返回体。
///
/// `missing_components` 列出插件声明权限所需、但尚未安装的基础组件
/// （如 `"ffmpeg"`/`"ytdlp"`，依赖表见引擎 `plugin::dependencies`）——
/// 提醒式而非阻断式：安装本身已成功，客户端应提示用户前往组件设置安装依赖。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub identity: String,
    #[serde(default)]
    pub missing_components: Vec<String>,
}

#[cfg(feature = "plugins")]
impl From<fluxdown_engine::plugin::SettingField> for SettingFieldDto {
    fn from(f: fluxdown_engine::plugin::SettingField) -> Self {
        use fluxdown_engine::plugin::{SettingType, SettingWidget};
        let setting_type = match f.ty {
            SettingType::String => "string",
            SettingType::Number => "number",
            SettingType::Boolean => "boolean",
        }
        .to_string();
        let widget = match f.effective_widget() {
            SettingWidget::Text => "text",
            SettingWidget::Password => "password",
            SettingWidget::Textarea => "textarea",
            SettingWidget::Select => "select",
            SettingWidget::Toggle => "toggle",
            SettingWidget::Number => "number",
            SettingWidget::Folder => "folder",
        }
        .to_string();
        Self {
            key: f.key,
            title: f.title,
            description: f.description,
            setting_type,
            widget,
            options: f
                .options
                .into_iter()
                .map(|o| SettingOptionDto {
                    value: o.value,
                    label: o.label,
                })
                .collect(),
            default: f.default,
            required: f.required,
            min: f.min,
            max: f.max,
            helper_script: f.helper_script,
            helper_label: f.helper_label,
            pattern: f.pattern,
        }
    }
}

#[cfg(feature = "plugins")]
impl From<fluxdown_engine::plugin::PluginInfo> for PluginDto {
    fn from(p: fluxdown_engine::plugin::PluginInfo) -> Self {
        Self {
            identity: p.identity,
            name: p.name,
            version: p.version,
            description: p.description,
            homepage: p.homepage,
            enabled: p.enabled,
            dev_mode: p.dev_mode,
            disabled_reason: p.disabled_reason,
            settings: p.settings.into_iter().map(SettingFieldDto::from).collect(),
            settings_values: p.settings_values.into_iter().collect(),
            permissions: p.permissions,
        }
    }
}

/// 市场索引条目视图（去中心化插件市场浏览/安装）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarketEntryDto {
    pub plugin_id: String,
    pub version: String,
    pub sequence: u64,
    pub content_hash: String,
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub mirrors: Vec<String>,
    #[serde(default)]
    pub publish_time: String,
    #[serde(default)]
    pub yanked: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// manifest 声明的能力权限（如 `["ffmpeg"]`，供安装前展示授权）。
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// 从市场安装请求体。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarketInstallRequest {
    pub plugin_id: String,
}

#[cfg(feature = "plugins")]
impl From<fluxdown_engine::plugin::MarketEntry> for MarketEntryDto {
    fn from(e: fluxdown_engine::plugin::MarketEntry) -> Self {
        Self {
            plugin_id: e.plugin_id,
            version: e.version,
            sequence: e.sequence,
            content_hash: e.content_hash,
            min_app_version: e.min_app_version,
            name: e.name,
            description: e.description,
            author: e.author,
            homepage: e.homepage,
            mirrors: e.mirrors,
            publish_time: e.publish_time,
            yanked: e.yanked,
            tags: e.tags,
            permissions: e.permissions,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    struct DeserCase {
        name: &'static str,
        json: &'static str,
        check: fn(&DownloadRequest),
    }

    /// 迁移自旧 `native/hub/src/native_messaging.rs` 的 `DownloadRequest` 反序列化
    /// 测试套件：浏览器扩展 / 油猴脚本发来的 wire JSON 必须精确映射到字段。
    #[test]
    fn download_request_deserializes_wire_fields() {
        let cases = [
            DeserCase {
                name: "full payload with headers",
                json: r#"{
                    "url": "https://example.com/file.zip",
                    "filename": "file.zip",
                    "referrer": "https://example.com/",
                    "cookies": "session=abc123",
                    "headers": {"Authorization": "Bearer token123", "X-Custom": "value"},
                    "fileSize": 1024,
                    "mimeType": "application/zip"
                }"#,
                check: |req| {
                    assert_eq!(req.url, "https://example.com/file.zip");
                    assert_eq!(req.filename, "file.zip");
                    assert_eq!(req.referrer, "https://example.com/");
                    assert_eq!(req.cookies, "session=abc123");
                    let headers = req.headers.as_ref().unwrap();
                    assert_eq!(headers.get("Authorization").unwrap(), "Bearer token123");
                    assert_eq!(headers.get("X-Custom").unwrap(), "value");
                    assert_eq!(req.file_size, Some(1024));
                    assert_eq!(req.mime_type.as_deref(), Some("application/zip"));
                },
            },
            DeserCase {
                name: "minimal payload omits optional fields",
                json: r#"{"url": "https://example.com/file.zip"}"#,
                check: |req| {
                    assert!(req.headers.is_none());
                    assert_eq!(req.cookies, "");
                    assert_eq!(req.referrer, "");
                    assert_eq!(req.file_size, None);
                },
            },
            DeserCase {
                name: "empty headers object deserializes to Some(empty map)",
                json: r#"{"url": "https://example.com/file.zip", "headers": {}}"#,
                check: |req| {
                    assert!(req.headers.as_ref().unwrap().is_empty());
                },
            },
            DeserCase {
                name: "fileSize -1 marks skip-probe hint",
                json: r#"{"url": "https://x/y", "cookies": "session=abc", "fileSize": -1}"#,
                check: |req| {
                    assert_eq!(req.file_size, Some(-1));
                    assert_eq!(req.cookies, "session=abc");
                },
            },
            DeserCase {
                name: "embedded newline in url survives round trip (batch join format)",
                json: r#"{"url": "https://a.com/1.zip\nhttps://b.com/2.zip"}"#,
                check: |req| {
                    let urls: Vec<&str> = req.url.split('\n').collect();
                    assert_eq!(urls, ["https://a.com/1.zip", "https://b.com/2.zip"]);
                },
            },
        ];

        for case in cases {
            let req: DownloadRequest = serde_json::from_str(case.json)
                .unwrap_or_else(|e| panic!("case `{}` failed to parse: {e}", case.name));
            (case.check)(&req);
        }
    }

    /// 扩展/接管入口透传的浏览器请求事务字段：`method`/`body`/`audioUrl`
    /// 必须按 camelCase wire 名精确落到 [`CreateTaskRequest`]，且缺省安全。
    #[test]
    fn create_task_request_deserializes_browser_transaction_fields() {
        let req: CreateTaskRequest = serde_json::from_str(
            r#"{
                "url": "https://example.com/dl",
                "method": "POST",
                "body": {"kind": "raw", "bytesB64": "aGk=", "contentType": "text/plain"},
                "audioUrl": "https://example.com/audio.m4s"
            }"#,
        )
        .unwrap();
        assert_eq!(req.method.as_deref(), Some("POST"));
        assert_eq!(
            req.audio_url.as_deref(),
            Some("https://example.com/audio.m4s")
        );
        match req.body.as_ref().unwrap() {
            RequestBody::Raw {
                bytes_b64,
                content_type,
            } => {
                assert_eq!(bytes_b64, "aGk=");
                assert_eq!(content_type.as_deref(), Some("text/plain"));
            }
            other => panic!("expected Raw body, got {other:?}"),
        }

        // 缺省：旧客户端（CLI / aria2 shim）不带这三个字段，必须解析为 None。
        let minimal: CreateTaskRequest =
            serde_json::from_str(r#"{"url": "https://example.com/f.zip"}"#).unwrap();
        assert!(minimal.method.is_none());
        assert!(minimal.body.is_none());
        assert!(minimal.audio_url.is_none());

        // formData 形态 → 引擎 CapturedRequestBody 转换保真。
        let form: RequestBody = serde_json::from_str(
            r#"{"kind": "formData", "fields": {"autodl": ["2"], "updates": ["1"]}}"#,
        )
        .unwrap();
        match fluxdown_engine::downloader::CapturedRequestBody::from(form) {
            fluxdown_engine::downloader::CapturedRequestBody::FormData { fields } => {
                assert_eq!(fields.get("autodl").unwrap(), &vec!["2".to_string()]);
                assert_eq!(fields.get("updates").unwrap(), &vec!["1".to_string()]);
            }
            other => panic!("expected FormData, got {other:?}"),
        }
    }

    #[test]
    fn task_dto_serializes_camel_case_with_correct_values() {
        let dto = TaskDto {
            task_id: "t1".to_string(),
            url: "https://example.com/f.zip".to_string(),
            file_name: "f.zip".to_string(),
            save_dir: "/tmp".to_string(),
            status: 1,
            downloaded_bytes: 10,
            total_bytes: 100,
            error_message: String::new(),
            created_at: "1700000000".to_string(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            referrer: String::new(),
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v["taskId"], "t1");
        assert_eq!(v["url"], "https://example.com/f.zip");
        assert_eq!(v["fileName"], "f.zip");
        assert_eq!(v["saveDir"], "/tmp");
        assert_eq!(v["status"], 1);
        assert_eq!(v["downloadedBytes"], 10);
        assert_eq!(v["totalBytes"], 100);
        assert_eq!(v["errorMessage"], "");
        assert_eq!(v["createdAt"], "1700000000");
        assert_eq!(v["proxyUrl"], "");
        assert_eq!(v["queueId"], "");
        assert_eq!(v["checksum"], "");
        assert_eq!(v["ignoreTlsErrors"], false);
        // 蛇形字段名不应残留（防止漏掉 rename_all）。
        assert!(v.get("task_id").is_none());
        assert!(v.get("file_name").is_none());
    }

    #[test]
    fn queue_dto_serializes_camel_case_with_correct_values() {
        let dto = QueueDto {
            queue_id: "q1".to_string(),
            name: "工作".to_string(),
            speed_limit_kbps: 512,
            max_concurrent: 3,
            default_save_dir: "/tmp".to_string(),
            position: 0,
            default_segments: 4,
            default_user_agent: "UA/1".to_string(),
            is_running: true,
            schedule_enabled: false,
            schedule_start: String::new(),
            schedule_stop: String::new(),
            schedule_days: 127,
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v["queueId"], "q1");
        assert_eq!(v["name"], "工作");
        assert_eq!(v["speedLimitKbps"], 512);
        assert_eq!(v["maxConcurrent"], 3);
        assert_eq!(v["defaultSaveDir"], "/tmp");
        assert_eq!(v["position"], 0);
        assert_eq!(v["defaultSegments"], 4);
        assert_eq!(v["defaultUserAgent"], "UA/1");
        assert!(v.get("queue_id").is_none());
        assert!(v.get("speed_limit_kbps").is_none());
    }
}
