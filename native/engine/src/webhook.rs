//! 任务事件 Webhook 推送（免费自托管，BYOE = bring-your-own-endpoint）。
//!
//! 设计见 `docs/webhook-notification-design.md`。本模块只实现**免费层**：
//! 用户自带端点（多端点 CRUD + 服务预设模板 + 事件订阅过滤 + HMAC 签名 +
//! 投递日志）；付费托管 Relay 不在本模块范围内。
//!
//! 三条不变式（继承插件通知平面纪律）：
//!
//! - **fire-and-forget**：投递失败绝不影响任务状态机，最多记日志；
//! - **off-actor**：[`WebhookDispatcher::emit`] 是同步函数，只做「筛选端点 +
//!   `tokio::spawn`」，全部网络 IO 在派生任务里跑，actor 上零阻塞；
//! - **可失败**：重试耗尽即放弃（无死信队列），失败可见于投递日志。
//!
//! 出站纪律：全局并发 4，**同端点串行**（保序 + 天然限流）；单次超时 10s；
//! 网络错误与 5xx 重试 3 次（2s/4s/8s 指数退避），**4xx 不重试**——4xx 是
//! 配置错误，重试只会刷日志。
//!
//! SSRF 立场与插件 bridge **相反**（记录在案）：插件 bridge 的出口守卫防的是
//! 不可信脚本，而 webhook URL 是用户显式配置的输入，打局域网里的 Home
//! Assistant / NAS / 本机 ntfy 是核心场景。因此只拒非 http(s) scheme；
//! `http://` 明文默认拒绝，需端点级 `allowHttp` 显式开启（防误配把签名密钥
//! 泄漏进明文链路）。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::{Duration, Instant};

use hmac::{Mac, SimpleHmac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::{Notify, Semaphore};

use crate::db::{Db, WebhookDeliveryRow};
use crate::events::{EngineEvent, EventSink};
use crate::logger::log_info;
use crate::proxy_config::ProxyConfig;

/// config 表键：端点列表（JSON 数组，元素为 [`EndpointSpec`]）。
pub const CONFIG_KEY_ENDPOINTS: &str = "webhook.endpoints";

/// 投递日志容量。**落盘**（`webhook_deliveries` 表），内存里同样持有这么多
/// 条作为读路径；重启时从库里回灌。
///
/// 不做无上限：单条最多两段 2000 字的正文，1000 条 ≈ 4MB 已经够一个人翻很
/// 久了，再多就是在主库里养一份没人看的审计日志。超出的按时间从旧到新丢，
/// 用户点「清空」才会全没。
pub const MAX_DELIVERY_LOG: usize = 1000;

/// 每次推给宿主的增量条数上限。
///
/// 宿主按 `deliveryId` 合并进自己那份列表，所以这只是「一次能追上多少」的
/// 窗口；500ms 内新增超过这个数的场景（千级批量任务），下一轮继续追。
const WIRE_DELTA_LIMIT: usize = 100;

/// 单次请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 首次 + 3 次重试（对齐引擎 `MAX_RETRIES=3` 惯例）。
const MAX_ATTEMPTS: u32 = 4;
/// 指数退避基数（秒）：2s / 4s / 8s。
const RETRY_BASE_SECS: u64 = 2;
/// 出站全局并发上限。
const MAX_CONCURRENT_DELIVERIES: usize = 4;

/// 单个端点待投递队列的上限。端点离线时队列只会积压，满了丢**最旧**的一条
/// （过期通知价值最低），丢弃以一条汇总记录的形式出现在投递日志里。
const MAX_QUEUED_JOBS_PER_ENDPOINT: usize = 256;

/// 遵守 `Retry-After` 时的等待上限。
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// 日志里请求/响应体的截断长度（字符）。
const MAX_LOG_BODY: usize = 2000;
/// 日志变化推给宿主的最小间隔。千级批量任务完成时每条投递都推一份 100 条
/// 快照会把 UI 通道压垮；日志面板是诊断面，半秒延迟无感。节流窗口内的最后
/// 一次变化由尾随推送补上，不会停在旧状态。
const EMIT_MIN_INTERVAL: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// 事件
// ---------------------------------------------------------------------------

/// v1 事件集。wire 名一经发布即为契约，不可改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebhookEventKind {
    /// 任务创建成功（含外部请求确认后）。
    TaskCreated,
    /// 任务首次进入 downloading。
    TaskStarted,
    /// 任务进入终态 completed。
    TaskCompleted,
    /// 自动重试耗尽后仍为 error（重试期间不发）。
    TaskFailed,
    /// 用户显式暂停单任务（全局暂停/退出保存不触发，防通知风暴）。
    TaskPaused,
    /// 某队列由空闲转为有活跃 / 待启动任务（启动恢复不触发）。
    QueueStarted,
    /// 某队列活跃 + 待启动任务清零。
    QueueDrained,
}

impl WebhookEventKind {
    /// 全部事件，顺序即 UI 展示顺序。
    pub const ALL: [WebhookEventKind; 7] = [
        WebhookEventKind::TaskCreated,
        WebhookEventKind::TaskStarted,
        WebhookEventKind::TaskCompleted,
        WebhookEventKind::TaskFailed,
        WebhookEventKind::TaskPaused,
        WebhookEventKind::QueueStarted,
        WebhookEventKind::QueueDrained,
    ];

    /// wire 名（payload `event` 字段与 `X-FluxDown-Event` 头）。
    pub fn wire(self) -> &'static str {
        match self {
            WebhookEventKind::TaskCreated => "task.created",
            WebhookEventKind::TaskStarted => "task.started",
            WebhookEventKind::TaskCompleted => "task.completed",
            WebhookEventKind::TaskFailed => "task.failed",
            WebhookEventKind::TaskPaused => "task.paused",
            WebhookEventKind::QueueStarted => "queue.started",
            WebhookEventKind::QueueDrained => "queue.drained",
        }
    }

    /// wire 名反解；未知名返回 `None`（订阅列表里的未知项被忽略）。
    pub fn from_wire(s: &str) -> Option<WebhookEventKind> {
        WebhookEventKind::ALL.into_iter().find(|k| k.wire() == s)
    }

    /// `{event.title}` 的默认取值。
    ///
    /// 引擎不做 i18n（无 locale 概念），默认模板一律英文；需要本地化文案的
    /// 用户填 `bodyTemplate`（UI 提供变量插入与实时预览）。
    fn title(self) -> &'static str {
        match self {
            WebhookEventKind::TaskCreated => "Download created",
            WebhookEventKind::TaskStarted => "Download started",
            WebhookEventKind::TaskCompleted => "Download completed",
            WebhookEventKind::TaskFailed => "Download failed",
            WebhookEventKind::TaskPaused => "Download paused",
            WebhookEventKind::QueueStarted => "Queue started",
            WebhookEventKind::QueueDrained => "Queue drained",
        }
    }
}

/// 事件里的任务快照。字段名对齐 `fluxdown_protocol::daemon::TaskDto`。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookTask {
    pub id: String,
    pub file_name: String,
    pub url: String,
    pub save_dir: String,
    pub total_bytes: i64,
    pub status: i32,
    pub error_message: String,
}

/// 一条待投递的语义事件。
#[derive(Debug, Clone)]
pub struct WebhookEvent {
    pub kind: WebhookEventKind,
    /// `queue.drained` 无任务，其余事件必有。
    pub task: Option<WebhookTask>,
    pub queue_id: String,
    pub queue_name: String,
}

impl WebhookEvent {
    /// 任务类事件。
    pub fn task(
        kind: WebhookEventKind,
        task: WebhookTask,
        queue_id: String,
        queue_name: String,
    ) -> Self {
        Self {
            kind,
            task: Some(task),
            queue_id,
            queue_name,
        }
    }

    /// 队列开始事件（队列由空闲转为有活跃/待启动任务）。
    pub fn queue_started(queue_id: String, queue_name: String) -> Self {
        Self {
            kind: WebhookEventKind::QueueStarted,
            task: None,
            queue_id,
            queue_name,
        }
    }

    /// 队列清空事件。
    pub fn queue_drained(queue_id: String, queue_name: String) -> Self {
        Self {
            kind: WebhookEventKind::QueueDrained,
            task: None,
            queue_id,
            queue_name,
        }
    }

    /// 「发送测试 / 模拟一次投递」用的样例事件——用户配完端点无需真下载一个
    /// 文件即可端到端验证。
    pub fn sample() -> Self {
        Self::task(
            WebhookEventKind::TaskCompleted,
            WebhookTask {
                id: "00000000-0000-4000-8000-000000000000".to_string(),
                file_name: "ubuntu-24.04.2-desktop-amd64.iso".to_string(),
                url: "https://releases.ubuntu.com/24.04/ubuntu-24.04.2-desktop-amd64.iso"
                    .to_string(),
                save_dir: "/downloads".to_string(),
                total_bytes: 6_442_450_944,
                status: 3,
                error_message: String::new(),
            },
            "main".to_string(),
            "Main".to_string(),
        )
    }
}

/// 云端推送用的语义事件载荷（[`crate::events::EngineEvent::TaskNotice`]）。
///
/// 与 [`WebhookEvent`] 同源同语义，额外带一次性 `delivery_id`（云端幂等键）与毫秒时间戳。
#[derive(Debug, Clone)]
pub struct TaskNotice {
    /// UUID v4，云端按 `(user, deliveryId)` 去重。
    pub delivery_id: String,
    /// 事件 wire 名（同 [`WebhookEventKind::wire`]）。
    pub event: String,
    /// Unix 毫秒。
    pub timestamp_ms: i64,
    pub queue_id: String,
    pub queue_name: String,
    /// `queue.drained` 无任务，其余事件必有。
    pub task: Option<WebhookTask>,
}

impl TaskNotice {
    /// 由 webhook 事件派生；生成新的投递 ID 与当前时间戳。
    pub fn from_event(event: &WebhookEvent) -> Self {
        Self {
            delivery_id: uuid::Uuid::new_v4().to_string(),
            event: event.kind.wire().to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            queue_id: event.queue_id.clone(),
            queue_name: event.queue_name.clone(),
            task: event.task.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// 端点
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

/// 单个 webhook 端点配置。持久化为 `webhook.endpoints` 里的一个 JSON 元素。
///
/// 全字段 `default`：老配置缺字段不会让整份配置解析失败；类型不符的字段同样回退
/// 默认值（见 [`lenient`]），一个手改坏的字段不会让整条端点失效。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EndpointSpec {
    #[serde(deserialize_with = "lenient::string")]
    pub id: String,
    #[serde(deserialize_with = "lenient::string")]
    pub name: String,
    /// 见 [`Preset`]；未知值按 [`Preset::Custom`] 处理。
    #[serde(deserialize_with = "lenient::string")]
    pub preset: String,
    #[serde(deserialize_with = "lenient::string")]
    pub url: String,
    #[serde(default = "default_true", deserialize_with = "lenient::bool_or_true")]
    pub enabled: bool,
    /// 订阅的事件 wire 名；空 = 不投递任何事件。
    #[serde(deserialize_with = "lenient::strings")]
    pub events: Vec<String>,
    /// 队列过滤：空 = 全部队列。
    #[serde(deserialize_with = "lenient::string")]
    pub queue_id: String,
    /// 自定义请求头（可覆盖 `Content-Type`，承载各服务 token）。
    #[serde(deserialize_with = "lenient::string_map")]
    pub headers: BTreeMap<String, String>,
    /// 自定义 body 模板；空 = 用预设默认模板。
    #[serde(deserialize_with = "lenient::string")]
    pub body_template: String,
    /// 非空则开启 HMAC-SHA256 签名。
    #[serde(deserialize_with = "lenient::string")]
    pub sign_secret: String,
    /// 允许 `http://` 明文（仅建议局域网设备）。
    #[serde(deserialize_with = "lenient::bool_or_false")]
    pub allow_http: bool,
    /// 经全局代理发送（默认直连——局域网端点走代理必失败）。
    #[serde(deserialize_with = "lenient::bool_or_false")]
    pub use_proxy: bool,
}

/// 端点字段的宽松反序列化：类型不符回退默认值，数组 / 映射里的非字符串项丢弃。
/// 规则与 GPUI `sections::webhook::EndpointSpec`、Web `parseEndpoint` 逐字段一致，
/// 三端对同一份配置看到同一组端点。
mod lenient {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    pub(super) fn string<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
        Ok(match Value::deserialize(de)? {
            Value::String(value) => value,
            _ => String::new(),
        })
    }

    pub(super) fn bool_or_true<'de, D: Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
        Ok(Value::deserialize(de)?.as_bool().unwrap_or(true))
    }

    pub(super) fn bool_or_false<'de, D: Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
        Ok(Value::deserialize(de)?.as_bool().unwrap_or(false))
    }

    pub(super) fn strings<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
        Ok(match Value::deserialize(de)? {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::String(value) => Some(value),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        })
    }

    pub(super) fn string_map<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<BTreeMap<String, String>, D::Error> {
        Ok(match Value::deserialize(de)? {
            Value::Object(map) => map
                .into_iter()
                .filter_map(|(key, value)| match value {
                    Value::String(value) => Some((key, value)),
                    _ => None,
                })
                .collect(),
            _ => BTreeMap::new(),
        })
    }
}

impl EndpointSpec {
    fn preset(&self) -> Preset {
        Preset::from_wire(&self.preset)
    }

    fn subscribes(&self, kind: WebhookEventKind) -> bool {
        let wire = kind.wire();
        self.events.iter().any(|e| e == wire)
    }

    fn matches_queue(&self, queue_id: &str) -> bool {
        self.queue_id.is_empty() || self.queue_id == queue_id
    }

    /// 展示名：留空时回退预设标签，再回退 URL。
    fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        let label = self.preset().label();
        if label.is_empty() {
            self.url.clone()
        } else {
            label.to_string()
        }
    }
}

/// 端点配置校验。返回稳定英文 wire 消息，展示层按 locale 映射。
pub fn validate_endpoint(spec: &EndpointSpec) -> Result<(), String> {
    let url = spec.url.trim();
    if url.is_empty() {
        return Err("webhook url is required".to_string());
    }
    let parsed = url::Url::parse(url).map_err(|_| "webhook url is invalid".to_string())?;
    match parsed.scheme() {
        "https" => {}
        "http" => {
            if !spec.allow_http {
                return Err("plaintext http requires allowHttp".to_string());
            }
        }
        _ => return Err("webhook url must use http or https".to_string()),
    }
    if parsed.host_str().unwrap_or("").is_empty() {
        return Err("webhook url is invalid".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 服务预设
// ---------------------------------------------------------------------------

/// v1 预设矩阵。`Custom` 发 §3.2 信封原文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Ntfy,
    Gotify,
    Bark,
    ServerChan,
    Wecom,
    Feishu,
    Dingtalk,
    Telegram,
    Discord,
    Slack,
    Custom,
}

/// 预设元数据快照——UI 的服务预设网格与「实时载荷预览」的单一事实源，
/// 客户端只做 `{var}` 字符串替换，不复制模板内容。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetInfo {
    pub id: &'static str,
    pub label: &'static str,
    /// URL 输入框占位符。
    pub url_placeholder: &'static str,
    /// 默认 body 模板；`custom` 为空串（走信封）。
    pub default_template: &'static str,
    pub content_type: &'static str,
}

impl Preset {
    /// 全部预设，顺序即 UI 网格顺序。
    pub const ALL: [Preset; 11] = [
        Preset::Ntfy,
        Preset::Gotify,
        Preset::Bark,
        Preset::ServerChan,
        Preset::Wecom,
        Preset::Feishu,
        Preset::Dingtalk,
        Preset::Telegram,
        Preset::Discord,
        Preset::Slack,
        Preset::Custom,
    ];

    pub fn wire(self) -> &'static str {
        match self {
            Preset::Ntfy => "ntfy",
            Preset::Gotify => "gotify",
            Preset::Bark => "bark",
            Preset::ServerChan => "serverchan",
            Preset::Wecom => "wecom",
            Preset::Feishu => "feishu",
            Preset::Dingtalk => "dingtalk",
            Preset::Telegram => "telegram",
            Preset::Discord => "discord",
            Preset::Slack => "slack",
            Preset::Custom => "custom",
        }
    }

    /// 未知 wire 名一律降级为 [`Preset::Custom`]（发信封，永不静默丢事件）。
    pub fn from_wire(s: &str) -> Preset {
        Preset::ALL
            .into_iter()
            .find(|p| p.wire() == s)
            .unwrap_or(Preset::Custom)
    }

    fn label(self) -> &'static str {
        match self {
            Preset::Ntfy => "ntfy",
            Preset::Gotify => "Gotify",
            Preset::Bark => "Bark",
            Preset::ServerChan => "Server酱",
            Preset::Wecom => "企业微信",
            Preset::Feishu => "飞书",
            Preset::Dingtalk => "钉钉",
            Preset::Telegram => "Telegram",
            Preset::Discord => "Discord",
            Preset::Slack => "Slack",
            Preset::Custom => "Custom",
        }
    }

    /// 全部预设统一 JSON 请求体。Server酱官方 SDK（Go/JS/Python）即
    /// `application/json`；其 form 解析（尤其 Server酱³ push.ft07.com）观测到
    /// 不做百分号解码，form 载荷里的 URL/空格会原样显示成 `%2F` / `+`。
    fn content_type(self) -> &'static str {
        "application/json"
    }

    /// 预设默认 body 模板。`Custom` 返回空串——它走 §3.2 信封，无模板。
    fn default_template(self) -> &'static str {
        match self {
            // ntfy 的 JSON 发布必须打到服务根，topic 走 body（见 effective_url）。
            Preset::Ntfy => concat!(
                r#"{"topic":"{ntfy.topic}","title":"{event.title}","#,
                r#""message":"{event.summary}","tags":["fluxdown"]}"#
            ),
            Preset::Gotify => {
                r#"{"title":"{event.title}","message":"{event.summary}","priority":5}"#
            }
            Preset::Bark => {
                r#"{"title":"{event.title}","body":"{event.summary}","group":"FluxDown"}"#
            }
            Preset::ServerChan => r#"{"title":"{event.title}","desp":"{event.summary}"}"#,
            Preset::Wecom => {
                r#"{"msgtype":"markdown","markdown":{"content":"**{event.title}**\n{event.summary}"}}"#
            }
            Preset::Feishu => {
                r#"{"msg_type":"text","content":{"text":"{event.title}\n{event.summary}"}}"#
            }
            // 标题含「FluxDown」以通过钉钉机器人的「自定义关键词」安全设置。
            Preset::Dingtalk => concat!(
                r#"{"msgtype":"markdown","markdown":{"title":"FluxDown · {event.title}","#,
                r#""text":"**FluxDown · {event.title}**\n\n{event.summary}"}}"#
            ),
            // chat_id 走 URL query（?chat_id=…），body 只带文本。
            Preset::Telegram => r#"{"text":"{event.title}\n{event.summary}"}"#,
            Preset::Discord => r#"{"content":"**{event.title}**\n{event.summary}"}"#,
            Preset::Slack => r#"{"text":"*{event.title}*\n{event.summary}"}"#,
            Preset::Custom => "",
        }
    }

    fn url_placeholder(self) -> &'static str {
        match self {
            Preset::Ntfy => "https://ntfy.sh/<topic>",
            Preset::Gotify => "https://gotify.example.com/message?token=<token>",
            Preset::Bark => "https://api.day.app/<key>",
            Preset::ServerChan => "https://sctapi.ftqq.com/<SendKey>.send",
            Preset::Wecom => "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=<key>",
            Preset::Feishu => "https://open.feishu.cn/open-apis/bot/v2/hook/<id>",
            Preset::Dingtalk => "https://oapi.dingtalk.com/robot/send?access_token=<token>",
            Preset::Telegram => "https://api.telegram.org/bot<token>/sendMessage?chat_id=<id>",
            Preset::Discord => "https://discord.com/api/webhooks/...",
            Preset::Slack => "https://hooks.slack.com/services/...",
            Preset::Custom => "https://example.com/hook",
        }
    }

    /// 企业微信 / 飞书 / 钉钉的失败也回 HTTP 200，错误在响应体里：wecom / dingtalk 看
    /// `errcode`，飞书看 `code`（旧版为 `StatusCode`）。非零即业务失败，返回带错误码与
    /// 平台 `errmsg` / `msg` 的文本。响应体不是 JSON 或没有这些字段一律按成功处理
    /// （不猜测未知形态）；其余预设恒返回 `None`。
    fn business_error(self, body: &str) -> Option<String> {
        let (code_keys, msg_keys): (&[&str], &[&str]) = match self {
            Preset::Wecom | Preset::Dingtalk => (&["errcode"], &["errmsg"]),
            Preset::Feishu => (&["code", "StatusCode"], &["msg", "StatusMessage"]),
            _ => return None,
        };
        let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
        let code = code_keys
            .iter()
            .find_map(|key| value.get(*key).and_then(serde_json::Value::as_i64))?;
        if code == 0 {
            return None;
        }
        let message = msg_keys
            .iter()
            .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
            .unwrap_or_default();
        Some(format!("{} error {code}: {message}", self.label()))
    }

    fn info(self) -> PresetInfo {
        PresetInfo {
            id: self.wire(),
            label: self.label(),
            url_placeholder: self.url_placeholder(),
            default_template: self.default_template(),
            content_type: self.content_type(),
        }
    }
}

/// 预设目录快照（宿主转发给 UI）。
pub fn preset_catalog() -> Vec<PresetInfo> {
    Preset::ALL.into_iter().map(Preset::info).collect()
}

/// ntfy 的 JSON 发布端点是**服务根**，topic 在 body 里。用户填的是
/// `https://host/<topic>`（ntfy 官方分享的形态），这里剥掉最后一段路径。
///
/// 返回 `(投递 URL, topic)`。无路径段时 topic 为空，原样投递（由服务端报错，
/// 保存时 UI 已给出提示）。
fn ntfy_endpoint(url: &str) -> (String, String) {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return (url.to_string(), String::new());
    };
    let topic = parsed
        .path_segments()
        .and_then(|mut s| s.rfind(|seg: &&str| !seg.is_empty()))
        .unwrap_or("")
        .to_string();
    if topic.is_empty() {
        return (url.to_string(), String::new());
    }
    let mut segments: Vec<String> = parsed
        .path_segments()
        .map(|s| {
            s.filter(|seg| !seg.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    segments.pop();
    // 保留尾斜杠：反代在子路径下挂 ntfy 时（/ntfy/），少一个斜杠就是 301/404。
    let path = if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", segments.join("/"))
    };
    parsed.set_path(&path);
    parsed.set_query(None);
    (parsed.to_string(), topic)
}

// ---------------------------------------------------------------------------
// 模板渲染
// ---------------------------------------------------------------------------

/// 变量表。**纯字符串替换，无条件/循环/表达式**——杜绝模板注入面，也不引
/// 模板引擎依赖。想要逻辑的用户去写插件（onDone 钩子可编程）。
type Vars = HashMap<&'static str, String>;

/// 可用占位符清单（UI 的「点击插入变量」芯片单一事实源）。
pub const TEMPLATE_VARIABLES: &[&str] = &[
    "{event}",
    "{event.title}",
    "{event.summary}",
    "{timestamp}",
    "{instance.app}",
    "{instance.version}",
    "{instance.host}",
    "{task.id}",
    "{task.fileName}",
    "{task.url}",
    "{task.saveDir}",
    "{task.totalBytes}",
    "{task.totalBytesHuman}",
    "{task.status}",
    "{task.errorMessage}",
    "{queue.id}",
    "{queue.name}",
];

/// 人类可读字节数（1024 进制，与 UI 侧一致）。
pub fn format_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return "0 B".to_string();
    }
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn json_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 占位符替换。变量值一律按 JSON 字符串上下文转义。
///
/// 一个占位符是**不含嵌套 `{` 的** `{…}` 段；不在变量表里的段原样保留。
/// 两条规则合起来保证 JSON 字面量安然无恙：`{"text":"{event.title}"}` 的外层
/// `{` 因为内部还有 `{` 而被判定为普通字符，内层 `{event.title}` 才是占位符。
fn render_template(template: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(template.len() + 64);
    let bytes = template.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'{' {
                i += 1;
            }
            out.push_str(&template[start..i]);
            continue;
        }
        // 找本段的收尾 `}`；中途遇到另一个 `{` 说明当前这个不是占位符开头。
        let mut j = i + 1;
        while j < bytes.len() && bytes[j] != b'}' && bytes[j] != b'{' {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] == b'{' {
            out.push('{');
            i += 1;
            continue;
        }
        let end = j + 1;
        let key = &template[i..end];
        match vars.get(key) {
            Some(value) => out.push_str(&json_escape(value)),
            None => out.push_str(key),
        }
        i = end;
    }
    out
}

// ---------------------------------------------------------------------------
// 签名
// ---------------------------------------------------------------------------

/// Stripe 式签名头值：`t=<unix>,v1=hex(hmac_sha256(secret, "<t>.<body>"))`。
/// 时间戳参与签名 → 接收端可拒绝重放。
fn sign_header(secret: &str, unix_secs: i64, body: &str) -> String {
    let payload = format!("{unix_secs}.{body}");
    let Ok(mut mac) = <SimpleHmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(payload.as_bytes());
    let digest = hex::encode(mac.finalize().into_bytes());
    format!("t={unix_secs},v1={digest}")
}

// ---------------------------------------------------------------------------
// 投递记录
// ---------------------------------------------------------------------------

/// 一条投递记录（内存环形缓冲，不落盘）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookDelivery {
    pub delivery_id: String,
    /// Unix 毫秒。
    pub timestamp_ms: i64,
    /// 事件 wire 名。
    pub event: String,
    pub endpoint_id: String,
    pub endpoint_name: String,
    /// 实际投递 URL（ntfy 已剥 topic）。
    pub url: String,
    /// 请求头摘录，每行 `K: V`；鉴权类值已掩码。
    pub request_headers: String,
    pub request_body: String,
    /// HTTP 状态码；0 = 未拿到响应（网络错误/超时）。
    pub status_code: i32,
    pub response_body: String,
    pub latency_ms: i64,
    /// 实际尝试次数（含首次）。
    pub attempts: i32,
    pub success: bool,
    /// 失败原因（成功时为空）。
    pub error: String,
}

impl From<WebhookDeliveryRow> for WebhookDelivery {
    fn from(r: WebhookDeliveryRow) -> Self {
        Self {
            delivery_id: r.delivery_id,
            timestamp_ms: r.timestamp_ms,
            event: r.event,
            endpoint_id: r.endpoint_id,
            endpoint_name: r.endpoint_name,
            url: r.url,
            request_headers: r.request_headers,
            request_body: r.request_body,
            status_code: r.status_code,
            response_body: r.response_body,
            latency_ms: r.latency_ms,
            attempts: r.attempts,
            success: r.success,
            error: r.error,
        }
    }
}

impl From<&WebhookDelivery> for WebhookDeliveryRow {
    fn from(d: &WebhookDelivery) -> Self {
        Self {
            delivery_id: d.delivery_id.clone(),
            timestamp_ms: d.timestamp_ms,
            event: d.event.clone(),
            endpoint_id: d.endpoint_id.clone(),
            endpoint_name: d.endpoint_name.clone(),
            url: d.url.clone(),
            request_headers: d.request_headers.clone(),
            request_body: d.request_body.clone(),
            status_code: d.status_code,
            response_body: d.response_body.clone(),
            latency_ms: d.latency_ms,
            attempts: d.attempts,
            success: d.success,
            error: d.error.clone(),
        }
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}

/// 鉴权类请求头值掩码——投递日志会被导出/截图，不该把 token 明文带出去。
fn mask_header_value(name: &str, value: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let sensitive = lower.contains("auth")
        || lower.contains("token")
        || lower.contains("key")
        || lower.contains("secret")
        || lower.contains("signature");
    if !sensitive {
        return value.to_string();
    }
    let visible: String = value.chars().take(8).collect();
    if value.chars().count() <= 8 {
        visible
    } else {
        format!("{visible}…")
    }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceInfo {
    app: &'static str,
    version: &'static str,
    host: String,
}

fn detect_host() -> String {
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(key)
            && !v.trim().is_empty()
        {
            return v.trim().to_string();
        }
    }
    if let Ok(v) = std::fs::read_to_string("/etc/hostname")
        && !v.trim().is_empty()
    {
        return v.trim().to_string();
    }
    "fluxdown".to_string()
}

struct Clients {
    direct: Client,
    /// 全局代理 client；代理模式为「无」或构建失败时与 `direct` 相同。
    proxied: Client,
}

/// 一条排队中的投递任务。
struct Job {
    spec: EndpointSpec,
    event: Arc<WebhookEvent>,
}

/// 队列满时被丢弃的投递的汇总（由 worker 下次取活前写进投递日志）。
struct DroppedJobs {
    count: u32,
    /// 最近一条被丢弃投递所属的端点与事件。
    spec: EndpointSpec,
    event: &'static str,
}

#[derive(Default)]
struct QueueState {
    jobs: VecDeque<Job>,
    dropped: Option<DroppedJobs>,
    closed: bool,
}

/// worker 下一步该做的事。
enum QueueStep {
    Deliver(Job),
    ReportDropped(DroppedJobs),
    Closed,
}

/// 单个端点的有界 FIFO 投递队列。
///
/// 入队在 `emit` 里同步完成，**同端点的顺序在入队那一刻就定死了**；队列满时丢
/// 最旧的一条并累计到 [`DroppedJobs`]，`emit` 永远不阻塞。
struct EndpointQueue {
    state: StdMutex<QueueState>,
    wake: Notify,
}

impl EndpointQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: StdMutex::new(QueueState::default()),
            wake: Notify::new(),
        })
    }

    fn push(&self, job: Job) {
        if let Ok(mut guard) = self.state.lock() {
            let state = &mut *guard;
            if state.jobs.len() >= MAX_QUEUED_JOBS_PER_ENDPOINT
                && let Some(old) = state.jobs.pop_front()
            {
                let event = old.event.kind.wire();
                match &mut state.dropped {
                    Some(dropped) => {
                        dropped.count = dropped.count.saturating_add(1);
                        dropped.spec = old.spec;
                        dropped.event = event;
                    }
                    None => {
                        state.dropped = Some(DroppedJobs {
                            count: 1,
                            spec: old.spec,
                            event,
                        });
                    }
                }
            }
            state.jobs.push_back(job);
        }
        self.wake.notify_one();
    }

    /// 关闭队列（dispatcher 释放时），唤醒 worker 让其退出。
    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
        self.wake.notify_one();
    }

    /// 取下一步；队列为空且未关闭时返回 `None`（worker 应等待唤醒）。
    /// 先汇报丢弃，再投递，保证日志里的丢弃记录不会被一直忙碌的 worker 饿住。
    fn take(&self) -> Option<QueueStep> {
        let Ok(mut state) = self.state.lock() else {
            return Some(QueueStep::Closed);
        };
        if state.closed {
            return Some(QueueStep::Closed);
        }
        if let Some(dropped) = state.dropped.take() {
            return Some(QueueStep::ReportDropped(dropped));
        }
        state.jobs.pop_front().map(QueueStep::Deliver)
    }
}

struct Inner {
    endpoints: StdRwLock<Vec<EndpointSpec>>,
    log: StdMutex<VecDeque<WebhookDelivery>>,
    clients: StdRwLock<Arc<Clients>>,
    /// 同端点串行队列：`endpoint_id → 该端点的有界队列`。
    ///
    /// **顺序在 `emit` 里同步入队的那一刻就定死了**，与后续调度无关。
    /// 早先的实现是「每条事件 spawn 一个任务 + 端点级 async Mutex」——在
    /// 多线程 runtime（headless server 的 `#[tokio::main]`）上，两个任务抢锁
    /// 的先后是真随机的，实测出现过 `queue.drained` 抢在 `task.completed`
    /// 前面送达。串行 ≠ 保序，这里必须是队列。
    workers: StdMutex<HashMap<String, Arc<EndpointQueue>>>,
    sema: Arc<Semaphore>,
    instance: InstanceInfo,
    /// 有无启用端点的快速判据，避免每次事件都拿读锁。
    any_enabled: AtomicBool,
    /// 宿主事件出口，用于把日志变化推给打开着的 UI。构造时无（`Engine::new`
    /// 里 dispatcher 先于 sink 就位），由 `set_sink` 补。
    sink: StdRwLock<Option<Arc<dyn EventSink>>>,
    /// 上次推送时刻，配合 [`EMIT_MIN_INTERVAL`] 节流。
    last_emit: StdMutex<Option<Instant>>,
    /// 节流窗口内是否已排了一次尾随推送（保证最后一条投递不被吃掉）。
    emit_pending: AtomicBool,
    /// 落盘出口。与 `sink` 同理，构造时无，由 `set_db` 补。
    db: StdRwLock<Option<Db>>,
}

/// 任务事件 → HTTP 投递。挂在 `DownloadManager` 上，随引擎构造。
pub struct WebhookDispatcher {
    inner: Arc<Inner>,
}

impl WebhookDispatcher {
    /// `proxy_config` 是引擎全局代理；仅 `useProxy` 端点走它，其余直连。
    ///
    /// client 构建失败（例如 SOCKS URL 非法）时退化为 `Client::new()`——
    /// webhook 绝不能因为代理配置问题让引擎构造失败。
    pub fn new(proxy_config: &ProxyConfig) -> Self {
        let inner = Inner {
            endpoints: StdRwLock::new(Vec::new()),
            log: StdMutex::new(VecDeque::with_capacity(MAX_DELIVERY_LOG)),
            clients: StdRwLock::new(Arc::new(build_clients(proxy_config))),
            workers: StdMutex::new(HashMap::new()),
            sema: Arc::new(Semaphore::new(MAX_CONCURRENT_DELIVERIES)),
            instance: InstanceInfo {
                app: "fluxdown",
                version: env!("FLUXDOWN_APP_VERSION"),
                host: detect_host(),
            },
            any_enabled: AtomicBool::new(false),
            sink: StdRwLock::new(None),
            last_emit: StdMutex::new(None),
            emit_pending: AtomicBool::new(false),
            db: StdRwLock::new(None),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// 注入宿主事件出口。`DownloadManager::new` 在 sink 就位后调用一次。
    ///
    /// 不设 sink 时 dispatcher 完全静默（单测即如此），只有拉快照能看到日志。
    pub fn set_sink(&self, sink: Arc<dyn EventSink>) {
        if let Ok(mut slot) = self.inner.sink.write() {
            *slot = Some(sink);
        }
    }

    /// 注入落盘出口并从库里回灌历史日志（`Engine::new` 启动时调一次）。
    ///
    /// 不设 db 时日志只活在内存里（单测即如此）。
    pub async fn attach_db(&self, db: Db) {
        match db.load_webhook_deliveries(MAX_DELIVERY_LOG as i64).await {
            Ok(rows) => {
                if let Ok(mut log) = self.inner.log.lock() {
                    // 库里是新→旧，环形缓冲里是旧→新（`deliveries()` 再倒回去）。
                    log.clear();
                    for row in rows.into_iter().rev() {
                        log.push_back(WebhookDelivery::from(row));
                    }
                    log_info!("[webhook] restored {} delivery record(s)", log.len());
                }
            }
            Err(e) => log_info!("[webhook] delivery log restore failed: {e}"),
        }
        if let Ok(mut slot) = self.inner.db.write() {
            *slot = Some(db);
        }
    }

    /// 全局代理变更时重建代理 client（`set_proxy_config` 调用）。
    pub fn set_proxy_config(&self, proxy_config: &ProxyConfig) {
        let next = Arc::new(build_clients(proxy_config));
        if let Ok(mut slot) = self.inner.clients.write() {
            *slot = next;
        }
    }

    /// 从 `webhook.endpoints` 的 JSON 值热重载端点表。
    ///
    /// 整份不是 JSON 数组时**保留旧表并记日志**——一份手改坏了的配置不该让通知
    /// 静默消失；数组里的非对象元素逐项跳过，字段级类型错误按 [`lenient`] 回退。
    pub fn reload_endpoints(&self, json: &str) {
        let trimmed = json.trim();
        let parsed: Vec<EndpointSpec> = if trimmed.is_empty() {
            Vec::new()
        } else {
            match serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
                Ok(items) => {
                    let total = items.len();
                    let list: Vec<EndpointSpec> = items
                        .into_iter()
                        .filter(serde_json::Value::is_object)
                        .filter_map(|item| serde_json::from_value(item).ok())
                        .collect();
                    if list.len() < total {
                        log_info!(
                            "[webhook] skipped {} malformed endpoint entries",
                            total - list.len()
                        );
                    }
                    list
                }
                Err(e) => {
                    log_info!("[webhook] endpoints config parse error, keeping previous: {e}");
                    return;
                }
            }
        };
        let enabled = parsed.iter().any(|e| e.enabled);
        if let Ok(mut slot) = self.inner.endpoints.write() {
            log_info!(
                "[webhook] endpoints reloaded: {} total, {} enabled",
                parsed.len(),
                parsed.iter().filter(|e| e.enabled).count()
            );
            *slot = parsed;
        }
        self.inner.any_enabled.store(enabled, Ordering::Relaxed);
    }

    /// 当前端点表快照（宿主校验/测试用）。
    pub fn endpoints(&self) -> Vec<EndpointSpec> {
        self.inner
            .endpoints
            .read()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// 投递日志快照，**新的在前**。
    pub fn deliveries(&self) -> Vec<WebhookDelivery> {
        self.inner
            .log
            .lock()
            .map(|log| log.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// 清空投递日志（内存 + 落盘）。
    ///
    /// 这是记录唯一会消失的路径 —— 用户显式点的，所以库也一起清干净。
    pub async fn clear_deliveries(&self) {
        if let Ok(mut log) = self.inner.log.lock() {
            log.clear();
        }
        let db = self.inner.db.read().ok().and_then(|s| s.clone());
        if let Some(db) = db
            && let Err(e) = db.clear_webhook_deliveries().await
        {
            log_info!("[webhook] delivery log clear failed: {e}");
        }
        // 显式清空立即广播当前快照；不能只删存储而让已打开的客户端保留旧记录。
        self.inner.emit_snapshot();
    }

    /// 发一条事件。**同步、不阻塞**：只筛端点 + 投队列，网络 IO 全在
    /// 端点 worker 里跑。
    ///
    /// 投队列这一步是同步的，所以**同一端点的事件顺序 = `emit` 调用顺序**。
    /// 无匹配端点时零开销返回（批量任务完成的热路径）。
    ///
    /// 返回**投出去的端点数**。宿主据此给「模拟一次下载完成」一个确定的
    /// 收尾：0 说明没有端点订阅这个事件，UI 该立刻说清楚，而不是转圈等一个
    /// 永远不会到的投递记录。
    pub fn emit(&self, event: WebhookEvent) -> usize {
        if !self.inner.any_enabled.load(Ordering::Relaxed) {
            return 0;
        }
        let Ok(endpoints) = self.inner.endpoints.read() else {
            return 0;
        };
        let targets: Vec<EndpointSpec> = endpoints
            .iter()
            .filter(|e| e.enabled && e.subscribes(event.kind) && e.matches_queue(&event.queue_id))
            .cloned()
            .collect();
        drop(endpoints);
        if targets.is_empty() {
            return 0;
        }
        let dispatched = targets.len();
        let event = Arc::new(event);
        for spec in targets {
            let queue = self.inner.worker_for(&spec.id);
            queue.push(Job {
                spec,
                event: event.clone(),
            });
        }
        dispatched
    }

    /// 「模拟一次 task.completed」——配完端点无需真实下载即可端到端验证。
    /// 返回投出去的端点数（0 = 没有端点订阅 `task.completed`）。
    pub fn emit_sample(&self) -> usize {
        self.emit(WebhookEvent::sample())
    }

    /// 「发送测试」：对**尚未保存**的草稿端点单发一次样例事件，**不重试**
    /// （用户在等内联反馈，8s 退避没有意义）。结果同时进投递日志。
    pub async fn test_endpoint(&self, spec: EndpointSpec) -> WebhookDelivery {
        let event = WebhookEvent::sample();
        let record = self.inner.deliver(&spec, &event, 1).await;
        self.inner.push_log(record.clone());
        record
    }
}

fn build_clients(proxy_config: &ProxyConfig) -> Clients {
    let direct = build_webhook_client(&ProxyConfig::default()).unwrap_or_else(|e| {
        log_info!("[webhook] direct client build failed, using default: {e}");
        Client::new()
    });
    let proxied = build_webhook_client(proxy_config).unwrap_or_else(|e| {
        log_info!("[webhook] proxied client build failed, falling back to direct: {e}");
        direct.clone()
    });
    Clients { direct, proxied }
}

/// 端点 worker：从队列里逐条取，串行投递。队列的 FIFO 语义就是投递保序的
/// 全部实现——worker 里没有任何锁。
///
/// dispatcher 释放后（队列被 `Inner::drop` 关闭或 `Weak::upgrade` 失败）自行退出。
fn spawn_worker(inner: std::sync::Weak<Inner>, queue: Arc<EndpointQueue>) {
    tokio::spawn(async move {
        loop {
            let Some(step) = queue.take() else {
                queue.wake.notified().await;
                continue;
            };
            let Some(inner) = inner.upgrade() else { break };
            match step {
                QueueStep::Closed => break,
                QueueStep::ReportDropped(dropped) => {
                    inner.push_log(dropped_record(&dropped));
                }
                QueueStep::Deliver(job) => {
                    let record = inner
                        .deliver_impl(&job.spec, &job.event, MAX_ATTEMPTS, true)
                        .await;
                    inner.push_log(record);
                }
            }
        }
    });
}

/// 队列满丢弃的汇总投递记录（失败、0 次尝试）。
fn dropped_record(dropped: &DroppedJobs) -> WebhookDelivery {
    let spec = &dropped.spec;
    let url = if spec.preset() == Preset::Ntfy {
        ntfy_endpoint(spec.url.trim()).0
    } else {
        spec.url.trim().to_string()
    };
    log_info!(
        "[webhook] endpoint={} queue full, dropped {} oldest pending delivery(ies)",
        spec.display_name(),
        dropped.count
    );
    WebhookDelivery {
        delivery_id: uuid::Uuid::new_v4().to_string(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        event: dropped.event.to_string(),
        endpoint_id: spec.id.clone(),
        endpoint_name: spec.display_name(),
        url,
        request_headers: String::new(),
        request_body: String::new(),
        status_code: 0,
        response_body: String::new(),
        latency_ms: 0,
        attempts: 0,
        success: false,
        error: format!(
            "dropped {} queued delivery(ies): endpoint queue full (limit {MAX_QUEUED_JOBS_PER_ENDPOINT}), oldest discarded",
            dropped.count
        ),
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let map = self
            .workers
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for queue in map.values() {
            queue.close();
        }
    }
}

impl Inner {
    fn push_log(self: &Arc<Self>, record: WebhookDelivery) {
        self.persist(&record);
        if let Ok(mut log) = self.log.lock() {
            if log.len() >= MAX_DELIVERY_LOG {
                log.pop_front();
            }
            log.push_back(record);
        }
        self.notify_hosts();
    }

    /// 落盘。写库是异步的而 `push_log` 是同步的，所以派生一个任务去写——
    /// 内存环形缓冲仍是读路径，落盘只为重启后还能看见。
    ///
    /// 没有 db（单测）时什么都不做。
    fn persist(&self, record: &WebhookDelivery) {
        let Some(db) = self.db.read().ok().and_then(|s| s.clone()) else {
            return;
        };
        let row = WebhookDeliveryRow::from(record);
        tokio::spawn(async move {
            if let Err(e) = db
                .insert_webhook_delivery(&row, MAX_DELIVERY_LOG as i64)
                .await
            {
                log_info!("[webhook] delivery persist failed: {e}");
            }
        });
    }

    /// 把整份日志快照推给宿主：节流窗口外立即推，窗口内排一次尾随推送。
    ///
    /// 尾随那次是必须的——只丢不补的话，一串投递里的**最后**一条正好落在
    /// 窗口里，面板就停在旧状态（用户看到的就是「点了没反应」）。
    fn notify_hosts(self: &Arc<Self>) {
        if self.host_sink().is_none() {
            return;
        }
        let now = Instant::now();
        let within_window = {
            let Ok(mut last) = self.last_emit.lock() else {
                return;
            };
            match *last {
                Some(prev) if now.duration_since(prev) < EMIT_MIN_INTERVAL => {
                    Some(EMIT_MIN_INTERVAL - now.duration_since(prev))
                }
                _ => {
                    *last = Some(now);
                    None
                }
            }
        };
        let Some(delay) = within_window else {
            self.emit_snapshot();
            return;
        };
        if self.emit_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        // worker 持 `Weak`：尾随任务不该把 dispatcher 钉住。
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.emit_pending.store(false, Ordering::Release);
            if let Ok(mut last) = inner.last_emit.lock() {
                *last = Some(Instant::now());
            }
            inner.emit_snapshot();
        });
    }

    /// 推**最新一小段**给宿主，而不是整仓。
    ///
    /// 日志现在落盘且能攒到 [`MAX_DELIVERY_LOG`] 条：整仓一份实测 0.94MB
    /// （极端 4MB），每 500ms 推一次纯属自残。宿主拿这段做增量合并，
    /// 完整历史走「打开面板时拉一次」那条路。
    fn emit_snapshot(&self) {
        let Some(sink) = self.host_sink() else {
            return;
        };
        let entries: Vec<WebhookDelivery> = self
            .log
            .lock()
            .map(|log| log.iter().rev().take(WIRE_DELTA_LIMIT).cloned().collect())
            .unwrap_or_default();
        sink.emit(EngineEvent::WebhookDeliveriesChanged(entries));
    }

    fn host_sink(&self) -> Option<Arc<dyn EventSink>> {
        self.sink.read().ok().and_then(|s| s.clone())
    }

    /// 取（必要时惰性创建）某端点的投递队列并起 worker。
    ///
    /// worker 持 `Weak<Inner>`：`Inner` 持有队列，worker 若持强引用就是
    /// 引用环，dispatcher 永远不会释放。
    fn worker_for(self: &Arc<Self>, id: &str) -> Arc<EndpointQueue> {
        let queue = EndpointQueue::new();
        let Ok(mut map) = self.workers.lock() else {
            // 锁中毒：退化为一次性 worker，宁可乱序也不丢事件。
            spawn_worker(Arc::downgrade(self), queue.clone());
            return queue;
        };
        if let Some(existing) = map.get(id) {
            return existing.clone();
        }
        spawn_worker(Arc::downgrade(self), queue.clone());
        map.insert(id.to_string(), queue.clone());
        queue
    }

    fn vars(&self, event: &WebhookEvent, ntfy_topic: &str) -> Vars {
        let now = chrono::Utc::now();
        let mut vars: Vars = HashMap::with_capacity(TEMPLATE_VARIABLES.len());
        vars.insert("{event}", event.kind.wire().to_string());
        vars.insert("{event.title}", event.kind.title().to_string());
        vars.insert("{event.summary}", event_summary(event));
        vars.insert("{timestamp}", now.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        vars.insert("{instance.app}", self.instance.app.to_string());
        vars.insert("{instance.version}", self.instance.version.to_string());
        vars.insert("{instance.host}", self.instance.host.clone());
        vars.insert("{queue.id}", event.queue_id.clone());
        vars.insert("{queue.name}", event.queue_name.clone());
        vars.insert("{ntfy.topic}", ntfy_topic.to_string());
        let task = event.task.clone().unwrap_or_default();
        vars.insert("{task.id}", task.id);
        vars.insert("{task.fileName}", task.file_name);
        vars.insert("{task.url}", task.url);
        vars.insert("{task.saveDir}", task.save_dir);
        vars.insert("{task.totalBytes}", task.total_bytes.to_string());
        vars.insert("{task.totalBytesHuman}", format_bytes(task.total_bytes));
        vars.insert("{task.status}", task.status.to_string());
        vars.insert("{task.errorMessage}", task.error_message);
        vars
    }

    /// 渲染 §3.2 信封（`custom` 预设 / 无预设模板时）。
    fn envelope(&self, event: &WebhookEvent, delivery_id: &str, timestamp: &str) -> String {
        let mut body = serde_json::json!({
            "schemaVersion": 1,
            "event": event.kind.wire(),
            "deliveryId": delivery_id,
            "timestamp": timestamp,
            "instance": {
                "app": self.instance.app,
                "version": self.instance.version,
                "host": self.instance.host,
            },
            "queue": {
                "id": event.queue_id,
                "name": event.queue_name,
            },
        });
        if let Some(task) = &event.task
            && let Ok(value) = serde_json::to_value(task)
            && let Some(map) = body.as_object_mut()
        {
            map.insert("task".to_string(), value);
        }
        body.to_string()
    }

    /// 投递一条事件到一个端点，含重试，不复查端点表（「发送测试」的草稿端点不在表里）。
    async fn deliver(
        &self,
        spec: &EndpointSpec,
        event: &WebhookEvent,
        max_attempts: u32,
    ) -> WebhookDelivery {
        self.deliver_impl(spec, event, max_attempts, false).await
    }

    /// 端点当前是否仍在端点表里且启用。
    fn endpoint_active(&self, id: &str) -> bool {
        self.endpoints
            .read()
            .map(|list| list.iter().any(|e| e.id == id && e.enabled))
            .unwrap_or(false)
    }

    /// 投递一条事件到一个端点，含重试。返回投递记录（成败都返回）。
    ///
    /// `recheck_endpoint` 为真时，**每次尝试前**重新确认端点仍存在且启用：
    /// 排队/退避期间被删除或停用的端点不再收到请求。
    async fn deliver_impl(
        &self,
        spec: &EndpointSpec,
        event: &WebhookEvent,
        max_attempts: u32,
        recheck_endpoint: bool,
    ) -> WebhookDelivery {
        let delivery_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let timestamp = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let preset = spec.preset();

        let (target_url, ntfy_topic) = if preset == Preset::Ntfy {
            ntfy_endpoint(spec.url.trim())
        } else {
            (spec.url.trim().to_string(), String::new())
        };

        let mut record = WebhookDelivery {
            delivery_id: delivery_id.clone(),
            timestamp_ms: now.timestamp_millis(),
            event: event.kind.wire().to_string(),
            endpoint_id: spec.id.clone(),
            endpoint_name: spec.display_name(),
            url: target_url.clone(),
            request_headers: String::new(),
            request_body: String::new(),
            status_code: 0,
            response_body: String::new(),
            latency_ms: 0,
            attempts: 0,
            success: false,
            error: String::new(),
        };

        if let Err(e) = validate_endpoint(spec) {
            record.error = e;
            return record;
        }

        // ---- body ----
        let vars = self.vars(event, &ntfy_topic);
        let template = if !spec.body_template.trim().is_empty() {
            spec.body_template.clone()
        } else {
            preset.default_template().to_string()
        };
        let body = if template.is_empty() {
            self.envelope(event, &delivery_id, &timestamp)
        } else {
            render_template(&template, &vars)
        };
        record.request_body = truncate(&body, MAX_LOG_BODY);

        // ---- headers ----
        let mut headers: Vec<(String, String)> = vec![
            (
                "Content-Type".to_string(),
                preset.content_type().to_string(),
            ),
            (
                "X-FluxDown-Event".to_string(),
                event.kind.wire().to_string(),
            ),
            ("X-FluxDown-Delivery".to_string(), delivery_id.clone()),
        ];
        if !spec.sign_secret.is_empty() {
            let sig = sign_header(&spec.sign_secret, now.timestamp(), &body);
            if !sig.is_empty() {
                headers.push(("X-FluxDown-Signature".to_string(), sig));
            }
        }
        // 用户自定义头最后落，允许覆盖上面任意一项（含 Content-Type）。
        for (k, v) in &spec.headers {
            if k.trim().is_empty() {
                continue;
            }
            headers.retain(|(name, _)| !name.eq_ignore_ascii_case(k.trim()));
            headers.push((k.trim().to_string(), v.clone()));
        }
        record.request_headers = headers
            .iter()
            .map(|(k, v)| format!("{k}: {}", mask_header_value(k, v)))
            .collect::<Vec<_>>()
            .join("\n");

        // ---- 出站限流（同端点保序由调用方的 worker 队列保证）----
        // 许可只在单次请求期间持有，退避 sleep 期间释放，避免离线端点占满全局并发。
        if self.sema.is_closed() {
            record.error = "webhook dispatcher shut down".to_string();
            return record;
        }

        let client = {
            let Ok(clients) = self.clients.read() else {
                record.error = "webhook client unavailable".to_string();
                return record;
            };
            if spec.use_proxy {
                clients.proxied.clone()
            } else {
                clients.direct.clone()
            }
        };

        let started = std::time::Instant::now();
        for attempt in 1..=max_attempts {
            if recheck_endpoint && !self.endpoint_active(&spec.id) {
                record.error = "endpoint removed or disabled; delivery cancelled".to_string();
                break;
            }
            record.attempts = attempt as i32;
            let Ok(permit) = self.sema.clone().acquire_owned().await else {
                record.error = "webhook dispatcher shut down".to_string();
                break;
            };
            let mut req = client
                .post(&target_url)
                .timeout(REQUEST_TIMEOUT)
                .body(body.clone());
            for (k, v) in &headers {
                req = req.header(k, v);
            }
            let mut retry_after: Option<Duration> = None;
            let result = req.send().await;
            let outcome = match result {
                Ok(resp) => {
                    let status = resp.status();
                    record.status_code = status.as_u16() as i32;
                    if !status.is_success() {
                        retry_after = resp
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .and_then(parse_retry_after);
                    }
                    let location = if status.is_redirection() {
                        resp.headers()
                            .get(reqwest::header::LOCATION)
                            .and_then(|v| v.to_str().ok())
                            .map(|l| url_host(l).unwrap_or_default())
                    } else {
                        None
                    };
                    let text = resp.text().await.unwrap_or_default();
                    record.response_body = truncate(&text, MAX_LOG_BODY);
                    if status.is_success() {
                        // HTTP 200 但平台业务码失败（wecom / feishu / dingtalk）：配置或关键词
                        // 错误，重试无益——记为失败、错误文本进投递日志、不重试。
                        if let Some(error) = preset.business_error(&text) {
                            record.error = error;
                        } else {
                            record.success = true;
                            record.error = String::new();
                        }
                        true
                    } else if status.is_redirection() {
                        record.error = match location {
                            Some(host) if !host.is_empty() => format!(
                                "HTTP {} redirect to {host} not followed; set the final URL",
                                status.as_u16()
                            ),
                            _ => format!(
                                "HTTP {} redirect not followed; set the final URL",
                                status.as_u16()
                            ),
                        };
                        true
                    } else {
                        record.error = format!("HTTP {}", status.as_u16());
                        !is_retryable_status(status)
                    }
                }
                Err(e) => {
                    record.status_code = 0;
                    record.error = if e.is_timeout() {
                        "request timed out".to_string()
                    } else {
                        // reqwest 的错误文本带完整 URL（可能含令牌），URL 已单独记录在 record。
                        e.without_url().to_string()
                    };
                    false
                }
            };
            drop(permit);
            if outcome {
                break;
            }
            if attempt < max_attempts {
                let backoff =
                    Duration::from_secs(RETRY_BASE_SECS.saturating_mul(1u64 << (attempt - 1)));
                let delay = retry_after.map_or(backoff, |ra| ra.min(MAX_RETRY_AFTER));
                tokio::time::sleep(delay).await;
            }
        }
        record.latency_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
        if !record.success && record.error.is_empty() {
            record.error = "delivery failed".to_string();
        }
        if !record.success {
            log_info!(
                "[webhook] delivery failed: endpoint={} host={} event={} attempts={} status={} error={}",
                record.endpoint_name,
                url_host(&target_url).unwrap_or_default(),
                record.event,
                record.attempts,
                record.status_code,
                crate::logger::sanitize_log_str(&record.error)
            );
        }
        record
    }
}

/// 408/429 是暂时性失败，值得重试；其余 4xx 是配置错误。
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    !status.is_client_error() || status.as_u16() == 408 || status.as_u16() == 429
}

/// 解析 `Retry-After` 的秒数形式；HTTP-date 形式忽略，回退指数退避。
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// 只取 host，日志里不出现路径/query 中的密钥。
fn url_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}

/// webhook 专用 client：不跟随重定向，避免 https→http 降级或跨主机时
/// 把签名与自定义令牌头转发出去；也不启用 cookie。
fn build_webhook_client(proxy_config: &ProxyConfig) -> Result<Client, reqwest::Error> {
    use crate::proxy_config::{ProxyMode, detect_system_proxy};
    let mut builder = crate::downloader::client_builder()
        .user_agent(format!("FluxDown/{}", env!("FLUXDOWN_APP_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15));
    let manual = |cfg: &ProxyConfig| -> Option<reqwest::Proxy> {
        let url = cfg.to_proxy_url()?;
        let mut proxy = reqwest::Proxy::all(&url).ok()?;
        if !cfg.username.is_empty() {
            proxy = proxy.basic_auth(&cfg.username, &cfg.password);
        }
        if !cfg.no_proxy_list.is_empty() {
            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(
                &crate::proxy_config::normalize_no_proxy(&cfg.no_proxy_list),
            ));
        }
        Some(proxy)
    };
    builder = match proxy_config.mode {
        ProxyMode::Manual => match manual(proxy_config) {
            Some(p) => builder.proxy(p),
            None => builder.no_proxy(),
        },
        ProxyMode::System => match detect_system_proxy() {
            Ok(Some(sys)) => match manual(&sys) {
                Some(p) => builder.proxy(p),
                None => builder.no_proxy(),
            },
            _ => builder.no_proxy(),
        },
        ProxyMode::None | ProxyMode::Auto => builder.no_proxy(),
    };
    builder.build()
}

/// `{event.summary}` 的默认取值——一行人类可读摘要。
fn event_summary(event: &WebhookEvent) -> String {
    match (&event.task, event.kind) {
        (_, WebhookEventKind::QueueStarted | WebhookEventKind::QueueDrained) => {
            let name = if event.queue_name.is_empty() {
                event.queue_id.as_str()
            } else {
                event.queue_name.as_str()
            };
            if event.kind == WebhookEventKind::QueueStarted {
                format!("Queue \"{name}\" started processing tasks")
            } else {
                format!("Queue \"{name}\" has no active or pending tasks")
            }
        }
        (Some(task), WebhookEventKind::TaskFailed) => {
            if task.error_message.is_empty() {
                task.file_name.clone()
            } else {
                format!("{} — {}", task.file_name, task.error_message)
            }
        }
        (Some(task), _) => {
            if task.total_bytes > 0 {
                format!("{} · {}", task.file_name, format_bytes(task.total_bytes))
            } else {
                task.file_name.clone()
            }
        }
        (None, _) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::atomic::AtomicUsize;

    fn vars_for(event: &WebhookEvent, topic: &str) -> Vars {
        let inner = Inner {
            endpoints: StdRwLock::new(Vec::new()),
            log: StdMutex::new(VecDeque::new()),
            clients: StdRwLock::new(Arc::new(Clients {
                direct: Client::new(),
                proxied: Client::new(),
            })),
            workers: StdMutex::new(HashMap::new()),
            sema: Arc::new(Semaphore::new(1)),
            instance: InstanceInfo {
                app: "fluxdown",
                version: "9.9.9",
                host: "TESTHOST".to_string(),
            },
            any_enabled: AtomicBool::new(false),
            sink: StdRwLock::new(None),
            last_emit: StdMutex::new(None),
            emit_pending: AtomicBool::new(false),
            db: StdRwLock::new(None),
        };
        inner.vars(event, topic)
    }

    fn sample_event() -> WebhookEvent {
        WebhookEvent::task(
            WebhookEventKind::TaskCompleted,
            WebhookTask {
                id: "t1".to_string(),
                file_name: "a\"b.iso".to_string(),
                url: "https://example.com/a.iso".to_string(),
                save_dir: "D:\\Downloads".to_string(),
                total_bytes: 6_442_450_944,
                status: 3,
                error_message: String::new(),
            },
            "main".to_string(),
            "Main".to_string(),
        )
    }

    /// 一条成功投递记录，只有 id/时间戳按序号变化。
    fn log_record(i: usize) -> WebhookDelivery {
        WebhookDelivery {
            delivery_id: i.to_string(),
            timestamp_ms: i as i64,
            event: "task.completed".to_string(),
            endpoint_id: "e".to_string(),
            endpoint_name: "e".to_string(),
            url: "https://x.dev".to_string(),
            request_headers: String::new(),
            request_body: String::new(),
            status_code: 200,
            response_body: String::new(),
            latency_ms: 1,
            attempts: 1,
            success: true,
            error: String::new(),
        }
    }

    // ---- 模板渲染 ----

    #[test]
    fn renders_known_vars_and_keeps_json_literals() {
        let event = sample_event();
        let vars = vars_for(&event, "");
        let out = render_template(
            r#"{"text":"{event.title}: {task.fileName}","n":{task.totalBytes}}"#,
            &vars,
        );
        assert_eq!(
            out,
            r#"{"text":"Download completed: a\"b.iso","n":6442450944}"#
        );
        // 结果必须是合法 JSON——转义漏了这里就炸。
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["text"], "Download completed: a\"b.iso");
    }

    #[test]
    fn unknown_placeholder_is_left_verbatim() {
        let event = sample_event();
        let vars = vars_for(&event, "");
        assert_eq!(
            render_template("{not.a.var} {event}", &vars),
            "{not.a.var} task.completed"
        );
    }

    #[test]
    fn serverchan_template_keeps_url_verbatim() {
        let event = sample_event();
        let vars = vars_for(&event, "");
        let out = render_template(Preset::ServerChan.default_template(), &vars);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        // JSON 载荷里 URL / 路径不做百分号编码，接收端按原文显示。
        assert_eq!(parsed["title"], "Download completed");
        assert!(!out.contains("%2F"), "no percent-encoding in JSON payload");
    }

    #[test]
    fn newline_in_value_becomes_json_escape() {
        let mut event = sample_event();
        if let Some(task) = event.task.as_mut() {
            task.error_message = "line1\nline2".to_string();
        }
        event.kind = WebhookEventKind::TaskFailed;
        let vars = vars_for(&event, "");
        let out = render_template(r#"{"e":"{task.errorMessage}"}"#, &vars);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["e"], "line1\nline2");
    }

    #[test]
    fn every_preset_default_template_renders_valid_payload() -> Result<(), String> {
        let event = sample_event();
        for preset in Preset::ALL {
            let template = preset.default_template();
            if template.is_empty() {
                if preset != Preset::Custom {
                    return Err(format!(
                        "{} unexpectedly has an empty template",
                        preset.wire()
                    ));
                }
                continue;
            }
            let vars = vars_for(&event, "mytopic");
            let out = render_template(template, &vars);
            serde_json::from_str::<serde_json::Value>(&out)
                .map_err(|e| format!("{} produced invalid JSON: {e}\n{out}", preset.wire()))?;
            if out.contains("{event.title}") {
                return Err(format!("{} left a placeholder unrendered", preset.wire()));
            }
        }
        Ok(())
    }

    #[test]
    fn ntfy_default_template_carries_derived_topic() {
        let event = sample_event();
        let vars = vars_for(&event, "zero-downloads");
        let out = render_template(Preset::Ntfy.default_template(), &vars);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["topic"], "zero-downloads");
    }

    // ---- ntfy URL 派生 ----

    #[test]
    fn ntfy_endpoint_strips_topic_segment() {
        assert_eq!(
            ntfy_endpoint("https://ntfy.sh/zero-downloads"),
            ("https://ntfy.sh/".to_string(), "zero-downloads".to_string())
        );
        assert_eq!(
            ntfy_endpoint("https://nas.local/ntfy/alerts"),
            ("https://nas.local/ntfy/".to_string(), "alerts".to_string())
        );
        // 无 topic 段：原样投递，交给服务端报错。
        assert_eq!(
            ntfy_endpoint("https://ntfy.sh/"),
            ("https://ntfy.sh/".to_string(), String::new())
        );
    }

    // ---- 签名（RFC 4231 测试向量） ----

    #[test]
    fn hmac_matches_rfc4231_vector() -> Result<(), String> {
        // RFC 4231 Test Case 2: key="Jefe", data="what do ya want for nothing?"
        let mut mac = <SimpleHmac<Sha256> as Mac>::new_from_slice(b"Jefe")
            .map_err(|_| "hmac init failed".to_string())?;
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        Ok(())
    }

    #[test]
    fn sign_header_has_stripe_shape_and_binds_timestamp() {
        let a = sign_header("whsec_x", 1_700_000_000, "{\"a\":1}");
        let b = sign_header("whsec_x", 1_700_000_001, "{\"a\":1}");
        assert!(a.starts_with("t=1700000000,v1="));
        assert_eq!(a.len(), "t=1700000000,v1=".len() + 64);
        // 时间戳参与签名 → 换一秒签名必须变（防重放的前提）。
        assert_ne!(a, b);
    }

    // ---- 校验 ----

    #[test]
    fn validation_rejects_plaintext_http_unless_opted_in() {
        let mut spec = EndpointSpec {
            url: "http://192.168.1.9:8080/hook".to_string(),
            ..Default::default()
        };
        assert!(validate_endpoint(&spec).is_err());
        spec.allow_http = true;
        assert!(validate_endpoint(&spec).is_ok());
    }

    #[test]
    fn validation_rejects_non_http_schemes() {
        let spec = EndpointSpec {
            url: "file:///etc/passwd".to_string(),
            allow_http: true,
            ..Default::default()
        };
        assert!(validate_endpoint(&spec).is_err());
    }

    #[test]
    fn validation_accepts_lan_https_targets() {
        // 与插件 bridge 相反：局域网地址是核心场景，不得拦截。
        let spec = EndpointSpec {
            url: "https://homeassistant.local:8123/api/webhook/x".to_string(),
            ..Default::default()
        };
        assert!(validate_endpoint(&spec).is_ok());
    }

    // ---- 订阅过滤 ----

    #[test]
    fn subscription_and_queue_filter() {
        let spec = EndpointSpec {
            events: vec!["task.completed".to_string()],
            queue_id: "anime".to_string(),
            ..Default::default()
        };
        assert!(spec.subscribes(WebhookEventKind::TaskCompleted));
        assert!(!spec.subscribes(WebhookEventKind::TaskFailed));
        assert!(spec.matches_queue("anime"));
        assert!(!spec.matches_queue("main"));

        let any_queue = EndpointSpec::default();
        assert!(any_queue.matches_queue("whatever"));
    }

    #[test]
    fn missing_enabled_field_defaults_to_true() {
        let list: Vec<EndpointSpec> =
            serde_json::from_str(r#"[{"id":"a","url":"https://x.dev/h"}]"#).unwrap();
        assert!(list[0].enabled);
        assert_eq!(list[0].preset(), Preset::Custom);
    }

    #[test]
    fn format_bytes_is_binary_scaled() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(6_442_450_944), "6.0 GB");
    }

    #[test]
    fn sensitive_headers_are_masked_in_log() {
        assert_eq!(
            mask_header_value("Authorization", "Bearer tk_1234567890"),
            "Bearer t…"
        );
        assert_eq!(
            mask_header_value("Content-Type", "application/json"),
            "application/json"
        );
    }

    #[test]
    fn rate_limit_and_timeout_are_retryable_other_4xx_are_not() {
        use reqwest::StatusCode;
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn retry_after_parses_seconds_only() {
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    }

    #[test]
    fn url_host_drops_path_and_query_secrets() {
        assert_eq!(
            url_host("https://api.telegram.org/bot1:SECRET/send?token=x").as_deref(),
            Some("api.telegram.org")
        );
    }

    #[tokio::test]
    async fn redirect_is_not_followed_and_not_retried() {
        let server = spawn_mock("HTTP/1.1 302 Found");
        let d = dispatcher();
        let record = d
            .inner
            .deliver(
                &EndpointSpec {
                    id: "e302".to_string(),
                    url: format!("http://{}/hook", server.addr),
                    allow_http: true,
                    ..Default::default()
                },
                &WebhookEvent::sample(),
                MAX_ATTEMPTS,
            )
            .await;
        assert!(!record.success);
        assert_eq!(record.status_code, 302);
        assert_eq!(record.attempts, 1);
        assert!(record.error.contains("redirect"), "{}", record.error);
    }

    // ---- 投递语义（真实 HTTP，最小 mock 服务器） ----

    /// 极简 HTTP/1.1 服务器：每个连接读完请求头后回一条固定响应。
    /// 记录收到的请求数与 `X-FluxDown-Event` 头到达顺序，供重试/保序断言。
    struct MockServer {
        addr: std::net::SocketAddr,
        hits: Arc<AtomicUsize>,
        events: Arc<StdMutex<Vec<String>>>,
    }

    fn spawn_mock(status_line: &'static str) -> MockServer {
        spawn_mock_body(status_line, "ok")
    }

    fn spawn_mock_body(status_line: &'static str, body: &'static str) -> MockServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let events: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let hits_c = hits.clone();
        let events_c = events.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                hits_c.fetch_add(1, Ordering::SeqCst);
                // 读到请求头结束即可（body 长度不影响本测试的响应）。
                let mut buf = [0u8; 4096];
                use std::io::Read as _;
                let read = stream.read(&mut buf).unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..read]).to_ascii_lowercase();
                if let Some(rest) = text.split("x-fluxdown-event:").nth(1)
                    && let Some(line) = rest.split("\r\n").next()
                    && let Ok(mut seen) = events_c.lock()
                {
                    seen.push(line.trim().to_string());
                }

                stream
                    .write_all(
                        format!(
                            "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap_or_else(|error| {
                        assert!(
                            matches!(
                                error.kind(),
                                std::io::ErrorKind::BrokenPipe
                                    | std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                            ),
                            "test server response failed: {error}"
                        );
                    });

                stream.flush().unwrap_or_else(|error| {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ),
                        "test server response failed: {error}"
                    );
                });
            }
        });
        MockServer { addr, hits, events }
    }

    fn dispatcher() -> WebhookDispatcher {
        WebhookDispatcher::new(&ProxyConfig::default())
    }

    /// 落盘 → 重开 → 还在。
    ///
    /// 回归守卫：日志曾经只活在内存环里，重启清零 —— 而用户恰恰是隔天回来
    /// 看「昨晚那批到底发出去没有」，那时面板是空的。
    #[tokio::test]
    async fn deliveries_survive_restart() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_webhook_persist_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        if let Err(error) = std::fs::remove_dir_all(&dir) {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "clean test path: {error}"
            );
        }
        std::fs::create_dir_all(&dir).expect("create temp dir");

        // 第一次「运行」：写三条。
        let db = Db::open(&dir).await.expect("open db");
        let d = dispatcher();
        d.attach_db(db.clone()).await;
        for i in 0..3 {
            d.inner.push_log(log_record(i));
        }
        // `persist` 是 spawn 出去写的，等它落库。
        for _ in 0..100 {
            if db
                .load_webhook_deliveries(10)
                .await
                .unwrap_or_default()
                .len()
                == 3
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // 第二次「运行」：全新 dispatcher，只靠库回灌。
        let db2 = Db::open(&dir).await.expect("reopen db");
        let d2 = dispatcher();
        assert!(d2.deliveries().is_empty(), "回灌前应当是空的");
        d2.attach_db(db2.clone()).await;
        let log = d2.deliveries();
        assert_eq!(log.len(), 3, "重启后记录必须还在");
        // 新→旧：最后写的 id=2 排第一。
        assert_eq!(log[0].delivery_id, "2");
        assert_eq!(log[2].delivery_id, "0");

        // 手动清空是唯一的消失路径，且要连库一起清。
        d2.clear_deliveries().await;
        assert!(d2.deliveries().is_empty());
        assert!(
            db2.load_webhook_deliveries(10)
                .await
                .unwrap_or_default()
                .is_empty(),
            "清空必须落到库里，否则重启又冒出来"
        );

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }

    #[tokio::test]
    async fn success_records_2xx_and_stops_after_one_attempt() {
        let server = spawn_mock("HTTP/1.1 204 No Content");
        let d = dispatcher();
        let record = d
            .test_endpoint(EndpointSpec {
                id: "e1".to_string(),
                name: "mock".to_string(),
                url: format!("http://{}/hook", server.addr),
                allow_http: true,
                ..Default::default()
            })
            .await;
        assert!(record.success, "error: {}", record.error);
        assert_eq!(record.status_code, 204);
        assert_eq!(record.attempts, 1);
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        // 测试结果同时进投递日志。
        assert_eq!(d.deliveries().len(), 1);
    }

    #[tokio::test]
    async fn client_error_is_not_retried() {
        let server = spawn_mock("HTTP/1.1 404 Not Found");
        let d = dispatcher();
        let inner = d.inner.clone();
        let record = inner
            .deliver(
                &EndpointSpec {
                    id: "e404".to_string(),
                    url: format!("http://{}/hook", server.addr),
                    allow_http: true,
                    ..Default::default()
                },
                &WebhookEvent::sample(),
                MAX_ATTEMPTS,
            )
            .await;
        assert!(!record.success);
        assert_eq!(record.status_code, 404);
        assert_eq!(record.attempts, 1, "4xx must not be retried");
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn chat_presets_render_valid_json_with_their_default_templates() {
        let event = sample_event();
        let vars = vars_for(&event, "");
        for preset in [Preset::Wecom, Preset::Feishu, Preset::Dingtalk] {
            let out = render_template(preset.default_template(), &vars);
            let parsed: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|error| {
                panic!("{} renders invalid JSON: {error}: {out}", preset.wire())
            });
            match preset {
                Preset::Wecom => {
                    assert_eq!(parsed["msgtype"], "markdown");
                    let content = parsed["markdown"]["content"].as_str().unwrap();
                    assert!(content.starts_with("**Download completed**\n"), "{content}");
                }
                Preset::Feishu => {
                    assert_eq!(parsed["msg_type"], "text");
                    let text = parsed["content"]["text"].as_str().unwrap();
                    assert!(text.starts_with("Download completed\n"), "{text}");
                }
                _ => {
                    assert_eq!(parsed["msgtype"], "markdown");
                    // 标题含关键词 FluxDown，满足钉钉「自定义关键词」安全设置。
                    assert_eq!(parsed["markdown"]["title"], "FluxDown · Download completed");
                    let text = parsed["markdown"]["text"].as_str().unwrap();
                    assert!(
                        text.starts_with("**FluxDown · Download completed**\n\n"),
                        "{text}"
                    );
                }
            }
        }
    }

    #[test]
    fn chat_presets_are_cataloged_between_serverchan_and_telegram() {
        let ids: Vec<&str> = preset_catalog().iter().map(|p| p.id).collect();
        let at = |id: &str| ids.iter().position(|candidate| *candidate == id).unwrap();
        assert_eq!(at("wecom"), at("serverchan") + 1);
        assert_eq!(at("feishu"), at("wecom") + 1);
        assert_eq!(at("dingtalk"), at("feishu") + 1);
        assert_eq!(at("telegram"), at("dingtalk") + 1);
        let catalog = preset_catalog();
        let label = |id: &str| catalog.iter().find(|p| p.id == id).unwrap().label;
        assert_eq!(label("wecom"), "企业微信");
        assert_eq!(label("feishu"), "飞书");
        assert_eq!(label("dingtalk"), "钉钉");
        let placeholder = |id: &str| catalog.iter().find(|p| p.id == id).unwrap().url_placeholder;
        assert_eq!(
            placeholder("wecom"),
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=<key>"
        );
        assert_eq!(
            placeholder("feishu"),
            "https://open.feishu.cn/open-apis/bot/v2/hook/<id>"
        );
        assert_eq!(
            placeholder("dingtalk"),
            "https://oapi.dingtalk.com/robot/send?access_token=<token>"
        );
    }

    #[test]
    fn business_error_follows_each_platforms_body_shape() {
        // 企业微信 / 钉钉：errcode != 0。
        assert_eq!(
            Preset::Wecom.business_error(r#"{"errcode":0,"errmsg":"ok"}"#),
            None
        );
        assert_eq!(
            Preset::Wecom
                .business_error(r#"{"errcode":93000,"errmsg":"invalid webhook url"}"#)
                .as_deref(),
            Some("企业微信 error 93000: invalid webhook url")
        );
        assert_eq!(
            Preset::Dingtalk
                .business_error(r#"{"errcode":310000,"errmsg":"keywords not in content"}"#)
                .as_deref(),
            Some("钉钉 error 310000: keywords not in content")
        );
        // 飞书：code（或旧版 StatusCode）!= 0，文本取 msg。
        assert_eq!(
            Preset::Feishu.business_error(
                r#"{"StatusCode":0,"StatusMessage":"success","code":0,"msg":"success"}"#
            ),
            None
        );
        assert_eq!(
            Preset::Feishu
                .business_error(r#"{"code":19024,"msg":"Key Words Not Found"}"#)
                .as_deref(),
            Some("飞书 error 19024: Key Words Not Found")
        );
        assert_eq!(
            Preset::Feishu
                .business_error(r#"{"StatusCode":9499,"StatusMessage":"Bad Request"}"#)
                .as_deref(),
            Some("飞书 error 9499: Bad Request")
        );
        // 非 JSON / 缺字段按成功处理；其余预设永不解析响应体。
        assert_eq!(Preset::Wecom.business_error("ok"), None);
        assert_eq!(Preset::Feishu.business_error(r#"{"data":{}}"#), None);
        assert_eq!(Preset::Dingtalk.business_error(""), None);
        for preset in [Preset::Ntfy, Preset::Bark, Preset::Telegram, Preset::Custom] {
            assert_eq!(preset.business_error(r#"{"errcode":1,"code":1}"#), None);
        }
    }

    fn chat_spec(preset: &str, addr: std::net::SocketAddr) -> EndpointSpec {
        EndpointSpec {
            id: format!("e-{preset}"),
            preset: preset.to_string(),
            url: format!("http://{addr}/hook"),
            allow_http: true,
            ..Default::default()
        }
    }

    /// HTTP 200 + 业务错误码：记为失败、错误文本进投递日志、只尝试一次（不重试）。
    #[tokio::test]
    async fn chat_preset_business_failure_is_recorded_and_not_retried() {
        for (preset, body, expected) in [
            (
                "wecom",
                r#"{"errcode":93000,"errmsg":"invalid webhook url"}"#,
                "93000",
            ),
            (
                "dingtalk",
                r#"{"errcode":310000,"errmsg":"keywords not in content"}"#,
                "310000",
            ),
            (
                "feishu",
                r#"{"code":19024,"msg":"Key Words Not Found"}"#,
                "19024",
            ),
        ] {
            let server = spawn_mock_body("HTTP/1.1 200 OK", body);
            let d = dispatcher();
            let record = d
                .inner
                .deliver(
                    &chat_spec(preset, server.addr),
                    &WebhookEvent::sample(),
                    MAX_ATTEMPTS,
                )
                .await;
            assert!(!record.success, "{preset} must fail");
            assert_eq!(record.status_code, 200);
            assert_eq!(
                record.attempts, 1,
                "{preset} business errors are not retried"
            );
            assert!(record.error.contains(expected), "{}", record.error);
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
            assert!(record.response_body.contains(expected));
        }
    }

    #[tokio::test]
    async fn chat_preset_business_success_and_other_presets_ignore_the_body() {
        let ok = spawn_mock_body("HTTP/1.1 200 OK", r#"{"errcode":0,"errmsg":"ok"}"#);
        let d = dispatcher();
        let record = d
            .inner
            .deliver(
                &chat_spec("wecom", ok.addr),
                &WebhookEvent::sample(),
                MAX_ATTEMPTS,
            )
            .await;
        assert!(record.success, "{}", record.error);
        assert_eq!(record.attempts, 1);

        // 其余预设行为不变：HTTP 2xx 即成功，即便响应体里恰好有 errcode。
        let other = spawn_mock_body("HTTP/1.1 200 OK", r#"{"errcode":5,"errmsg":"x"}"#);
        let record = d
            .inner
            .deliver(
                &chat_spec("bark", other.addr),
                &WebhookEvent::sample(),
                MAX_ATTEMPTS,
            )
            .await;
        assert!(record.success, "{}", record.error);
    }

    /// 5xx 会重试（与 4xx 相反）。跑 2 次尝试（一次 2s 退避）而不是完整的
    /// `MAX_ATTEMPTS`，避免测试白等 2+4+8 秒；重试**次数上限**由
    /// `MAX_ATTEMPTS` 常量本身担保，这里验的是「5xx 走重试分支」。
    #[tokio::test]
    async fn server_error_is_retried() {
        assert_eq!(MAX_ATTEMPTS, 4, "设计约定：首次 + 3 次重试");
        let server = spawn_mock("HTTP/1.1 503 Service Unavailable");
        let d = dispatcher();
        let inner = d.inner.clone();
        let record = inner
            .deliver(
                &EndpointSpec {
                    id: "e503".to_string(),
                    url: format!("http://{}/hook", server.addr),
                    allow_http: true,
                    ..Default::default()
                },
                &WebhookEvent::sample(),
                2,
            )
            .await;
        assert!(!record.success);
        assert_eq!(record.status_code, 503);
        assert_eq!(record.attempts, 2);
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn invalid_endpoint_never_hits_the_network() {
        let d = dispatcher();
        let record = d
            .test_endpoint(EndpointSpec {
                id: "bad".to_string(),
                url: "ftp://example.com/hook".to_string(),
                ..Default::default()
            })
            .await;
        assert!(!record.success);
        assert_eq!(record.attempts, 0);
        assert!(record.error.contains("http"));
    }

    #[tokio::test]
    async fn emit_skips_when_no_endpoint_subscribes() {
        let d = dispatcher();
        d.reload_endpoints(
            r#"[{"id":"a","url":"https://127.0.0.1:1/h","enabled":true,"events":["task.failed"]}]"#,
        );
        d.emit(WebhookEvent::sample()); // task.completed
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(d.deliveries().is_empty());
    }

    #[tokio::test]
    async fn emit_delivers_to_subscribed_endpoint() {
        let server = spawn_mock("HTTP/1.1 200 OK");
        let d = dispatcher();
        d.reload_endpoints(&format!(
            r#"[{{"id":"a","name":"mock","url":"http://{}/h","enabled":true,"allowHttp":true,"events":["task.completed"]}}]"#,
            server.addr
        ));
        d.emit(WebhookEvent::sample());
        for _ in 0..100 {
            if !d.deliveries().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let log = d.deliveries();
        assert_eq!(log.len(), 1);
        assert!(log[0].success, "error: {}", log[0].error);
        assert_eq!(log[0].event, "task.completed");
    }

    /// 投递落库后必须把快照推给宿主。
    ///
    /// 回归守卫：没有这条推送，UI 只能在打开面板那一刻拉一次快照——真任务
    /// 完成时的投递、以及「模拟一次下载完成」按钮，全都发生在那之后，面板
    /// 停在旧状态，用户看到的就是「点了没反应」。
    #[tokio::test]
    async fn delivery_pushes_snapshot_to_sink() {
        struct CollectSink(StdMutex<Vec<usize>>);
        impl EventSink for CollectSink {
            fn emit(&self, event: EngineEvent) {
                if let EngineEvent::WebhookDeliveriesChanged(entries) = event
                    && let Ok(mut seen) = self.0.lock()
                {
                    seen.push(entries.len());
                }
            }
        }

        let server = spawn_mock("HTTP/1.1 200 OK");
        let sink = Arc::new(CollectSink(StdMutex::new(Vec::new())));
        let d = dispatcher();
        d.set_sink(sink.clone());
        d.reload_endpoints(&format!(
            r#"[{{"id":"a","name":"mock","url":"http://{}/h","enabled":true,"allowHttp":true,"events":["task.completed"]}}]"#,
            server.addr
        ));
        d.emit(WebhookEvent::sample());
        for _ in 0..100 {
            if !sink.0.lock().map(|v| v.is_empty()).unwrap_or(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let seen = sink.0.lock().map(|v| v.clone()).unwrap_or_default();
        assert_eq!(seen, vec![1], "应当恰好推一份含 1 条记录的快照");
        d.clear_deliveries().await;
        let seen = sink.0.lock().map(|v| v.clone()).unwrap_or_default();
        assert_eq!(seen, vec![1, 0], "清空后已订阅的 UI 必须立即收到空快照");
    }

    /// 节流窗口内的最后一条变化必须由尾随推送补上。
    ///
    /// 只丢不补的话，一串投递里最后那条正好落在窗口内，面板就永远停在
    /// 倒数第二条上。
    #[tokio::test]
    async fn throttled_burst_still_pushes_final_state() {
        struct LastSink(StdMutex<Option<usize>>);
        impl EventSink for LastSink {
            fn emit(&self, event: EngineEvent) {
                if let EngineEvent::WebhookDeliveriesChanged(entries) = event
                    && let Ok(mut slot) = self.0.lock()
                {
                    *slot = Some(entries.len());
                }
            }
        }

        let sink = Arc::new(LastSink(StdMutex::new(None)));
        let d = dispatcher();
        d.set_sink(sink.clone());
        // 直接灌日志：只验节流/补发语义，不必真发 HTTP。
        for i in 0..5 {
            d.inner.push_log(log_record(i));
        }
        // 首条立即推（1 条），其余落在 500ms 窗口里等尾随。
        assert_eq!(*sink.0.lock().unwrap(), Some(1));
        tokio::time::sleep(EMIT_MIN_INTERVAL + Duration::from_millis(150)).await;
        assert_eq!(
            *sink.0.lock().unwrap(),
            Some(5),
            "尾随推送必须补上窗口内的最终状态"
        );
    }

    /// 同端点**保序**：送达顺序必须等于 `emit` 调用顺序。
    ///
    /// 回归守卫。旧实现是「每条事件 spawn 一个任务 + 端点级 async Mutex」，
    /// 串行成立但顺序不成立：在多线程 runtime 上抢锁先后是真随机的，
    /// headless server 实测出现过 `queue.drained` 抢在 `task.completed`
    /// 前面送达。必须用多线程 flavor 跑，current_thread 掩盖这个 bug。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn same_endpoint_deliveries_keep_emit_order() -> Result<(), String> {
        let server = spawn_mock("HTTP/1.1 200 OK");
        let d = dispatcher();
        d.reload_endpoints(&format!(
            r#"[{{"id":"a","name":"mock","url":"http://{}/h","enabled":true,"allowHttp":true,
                 "events":["task.created","queue.started","task.started","task.completed","queue.drained"]}}]"#,
            server.addr
        ));
        let order = [
            WebhookEventKind::TaskCreated,
            WebhookEventKind::QueueStarted,
            WebhookEventKind::TaskStarted,
            WebhookEventKind::TaskCompleted,
            WebhookEventKind::QueueDrained,
        ];
        for kind in order {
            d.emit(WebhookEvent::task(
                kind,
                WebhookTask::default(),
                "main".to_string(),
                "Main".to_string(),
            ));
        }
        for _ in 0..200 {
            if server.hits.load(Ordering::SeqCst) >= order.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let seen = server
            .events
            .lock()
            .map_err(|_| "mock lock poisoned".to_string())?;
        assert_eq!(
            seen.as_slice(),
            order.map(|k| k.wire().to_string()),
            "同端点投递必须保序"
        );
        Ok(())
    }

    /// 离线端点的队列有界：满了丢最旧的，丢弃汇总先于后续投递被取出，
    /// 其余投递仍按入队顺序。
    #[test]
    fn endpoint_queue_drops_oldest_when_full_and_reports_it() {
        let queue = EndpointQueue::new();
        let extra = 3;
        for i in 0..(MAX_QUEUED_JOBS_PER_ENDPOINT + extra) {
            queue.push(Job {
                spec: EndpointSpec::default(),
                event: Arc::new(WebhookEvent::task(
                    WebhookEventKind::TaskCompleted,
                    WebhookTask::default(),
                    i.to_string(),
                    String::new(),
                )),
            });
        }
        let Some(QueueStep::ReportDropped(dropped)) = queue.take() else {
            panic!("dropped summary must come first");
        };
        assert_eq!(dropped.count as usize, extra);
        let mut delivered = Vec::new();
        while let Some(QueueStep::Deliver(job)) = queue.take() {
            delivered.push(job.event.queue_id.clone());
        }
        assert_eq!(delivered.len(), MAX_QUEUED_JOBS_PER_ENDPOINT);
        assert_eq!(delivered.first().map(String::as_str), Some("3"));
        let expected_last = (MAX_QUEUED_JOBS_PER_ENDPOINT + extra - 1).to_string();
        assert_eq!(delivered.last(), Some(&expected_last));
    }

    /// 退避期间端点被删除：下一次尝试前复查，不再发请求，记录里写明取消原因。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removed_endpoint_cancels_pending_retry() -> Result<(), String> {
        let server = spawn_mock("HTTP/1.1 503 Service Unavailable");
        let d = dispatcher();
        d.reload_endpoints(&format!(
            r#"[{{"id":"gone","name":"mock","url":"http://{}/h","enabled":true,"allowHttp":true,
                 "events":["task.completed"]}}]"#,
            server.addr
        ));
        d.emit(WebhookEvent::task(
            WebhookEventKind::TaskCompleted,
            WebhookTask::default(),
            "main".to_string(),
            "Main".to_string(),
        ));
        for _ in 0..100 {
            if server.hits.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        d.reload_endpoints("[]");
        for _ in 0..250 {
            if !d.deliveries().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let log = d.deliveries();
        let record = log.first().ok_or("no delivery record")?;
        assert_eq!(
            server.hits.load(Ordering::SeqCst),
            1,
            "no request after removal"
        );
        assert_eq!(record.attempts, 1);
        assert!(!record.success);
        assert!(
            record.error.contains("removed or disabled"),
            "error: {}",
            record.error
        );
        Ok(())
    }

    #[test]
    fn bad_config_json_keeps_previous_endpoints() {
        let d = dispatcher();
        d.reload_endpoints(r#"[{"id":"a","url":"https://x.dev/h","enabled":true}]"#);
        assert_eq!(d.endpoints().len(), 1);
        d.reload_endpoints("{ not json");
        assert_eq!(d.endpoints().len(), 1, "parse failure must not wipe config");
        d.reload_endpoints("[]");
        assert!(d.endpoints().is_empty());
    }

    #[test]
    fn malformed_fields_fall_back_and_non_object_entries_are_skipped() {
        let d = dispatcher();
        d.reload_endpoints(
            r#"[
                {"id":"a","url":"https://x.dev/h","enabled":"yes","allowHttp":1,
                 "events":["task.completed",7],"headers":{"X-Ok":"1","X-Bad":2},"queueId":null},
                "junk",
                {"id":"b","url":"https://y.dev/h","enabled":false,"events":"task.failed"}
            ]"#,
        );
        let list = d.endpoints();
        assert_eq!(
            list.len(),
            2,
            "one bad field must not drop the whole endpoint"
        );
        assert!(
            list[0].enabled,
            "non-bool enabled falls back to the default (on)"
        );
        assert!(!list[0].allow_http);
        assert_eq!(list[0].events, ["task.completed"]);
        assert_eq!(list[0].headers.len(), 1);
        assert!(list[0].queue_id.is_empty());
        assert!(!list[1].enabled);
        assert!(list[1].events.is_empty());
    }

    #[test]
    fn delivery_log_is_capped_and_newest_first() {
        let d = dispatcher();
        for i in 0..(MAX_DELIVERY_LOG + 10) {
            d.inner.push_log(log_record(i));
        }
        let log = d.deliveries();
        assert_eq!(log.len(), MAX_DELIVERY_LOG);
        assert_eq!(log[0].delivery_id, (MAX_DELIVERY_LOG + 9).to_string());
    }

    #[test]
    fn custom_preset_emits_versioned_envelope() {
        let d = dispatcher();
        let envelope = d
            .inner
            .envelope(&WebhookEvent::sample(), "did-1", "2026-07-17T12:34:56Z");
        let parsed: serde_json::Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(parsed["schemaVersion"], 1);
        assert_eq!(parsed["event"], "task.completed");
        assert_eq!(parsed["deliveryId"], "did-1");
        assert_eq!(
            parsed["task"]["fileName"],
            "ubuntu-24.04.2-desktop-amd64.iso"
        );
        assert_eq!(parsed["task"]["totalBytes"], 6_442_450_944i64);
        assert_eq!(parsed["queue"]["id"], "main");
    }

    #[test]
    fn queue_drained_envelope_has_no_task() {
        let d = dispatcher();
        let event = WebhookEvent::queue_drained("anime".to_string(), "Anime".to_string());
        let envelope = d.inner.envelope(&event, "did-2", "2026-07-17T12:34:56Z");
        let parsed: serde_json::Value = serde_json::from_str(&envelope).unwrap();
        assert!(parsed.get("task").is_none());
        assert_eq!(parsed["queue"]["name"], "Anime");
    }

    #[test]
    fn queue_started_envelope_has_no_task_and_summary_names_queue() {
        let d = dispatcher();
        let event = WebhookEvent::queue_started("anime".to_string(), "Anime".to_string());
        assert_eq!(event.kind.wire(), "queue.started");
        assert!(event.task.is_none());
        let envelope = d.inner.envelope(&event, "did-3", "2026-07-17T12:34:56Z");
        let parsed: serde_json::Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(parsed["event"], "queue.started");
        assert!(parsed.get("task").is_none());
        assert_eq!(parsed["queue"]["name"], "Anime");
        assert!(event_summary(&event).contains("Anime"));
        assert_ne!(
            event_summary(&event),
            event_summary(&WebhookEvent::queue_drained(
                "anime".to_string(),
                "Anime".to_string()
            ))
        );
    }

    #[test]
    fn preset_catalog_covers_every_preset() {
        let catalog = preset_catalog();
        assert_eq!(catalog.len(), Preset::ALL.len());
        assert!(catalog.iter().any(|p| p.id == "ntfy"));
        assert!(
            catalog
                .iter()
                .find(|p| p.id == "custom")
                .is_some_and(|p| p.default_template.is_empty())
        );
    }

    #[test]
    fn event_wire_names_round_trip() {
        for kind in WebhookEventKind::ALL {
            assert_eq!(WebhookEventKind::from_wire(kind.wire()), Some(kind));
        }
        assert_eq!(WebhookEventKind::from_wire("task.unknown"), None);
    }
}
