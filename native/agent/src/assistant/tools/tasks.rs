//! `fd_create_tasks` 与 `fd_manage_tasks`：把云端下发的任务工具调用映射为本机 daemon RPC。
//!
//! 参数语义以 FluxCloud `ai/catalog.rs` 的 Schema 为准：多余字段、类型不符、超长 / 越界一律拒绝，
//! 云端目录里没有的选项不在此扩展。AI 建的任务就是普通任务：不加标记、不自动清理。
//! 批量动作先解析成明确的任务 ID 列表，结果逐条列出受影响的任务；只有 `deleteFiles` 显式为
//! `true` 时才会连文件一起删。

use std::collections::{BTreeMap, HashMap, HashSet};

use fluxdown_protocol::{
    AgentPreferencesDto, CREATE_TASKS_BATCH_ID_MAX_LEN, CUSTOM_CATEGORIES_PREF_KEY,
    CategoryRouteDto, CreateTaskItem, CreateTaskRequest, CustomCategoryDto, DaemonConfigSnapshot,
    DaemonCreateTasksParams, DaemonCreateTasksResult, DaemonDeleteTasksParams, DaemonTaskIdsParams,
    EditTaskParams, QueueDto, TaskEditCapabilities, TaskEditSnapshot, method,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::task_ops::{self, FilePriorityArg, SeedLimitsArgs};
use super::{ToolContext, ToolError, parse_args};

/// 单次创建的条目上限（云端 Schema `items.maxItems`）。
const MAX_CREATE_ITEMS: usize = 50;
/// 单次管理的任务数上限（云端 Schema `taskIds.maxItems`）。
const MAX_MANAGE_IDS: usize = 200;
const MAX_ID_CHARS: usize = 128;
pub(super) const MAX_URL_CHARS: usize = 8192;
const MAX_FILE_NAME_CHARS: usize = 255;
const MAX_CHECKSUM_CHARS: usize = 200;
const MAX_SAVE_DIR_CHARS: usize = 1024;
const MAX_CATEGORY_CHARS: usize = 128;
const MAX_QUEUE_CHARS: usize = 128;
const MAX_PROXY_CHARS: usize = 300;
const MAX_USER_AGENT_CHARS: usize = 512;
const MAX_REFERRER_CHARS: usize = 2048;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_VALUE_CHARS: usize = 4096;
const MAX_TAGS: usize = 20;
const MAX_TAG_CHARS: usize = 64;
/// 线程数上限（云端 Schema `connections.maximum`；引擎自身上限更高）。
const MAX_CONNECTIONS: u32 = 256;
/// 单任务限速的合理性上限（字节 / 秒），挡住溢出与荒谬值。
const MAX_TASK_LIMIT_BPS: i64 = 1_000_000_000_000;
/// 统一编辑遇到 `edit-conflict`（任务刚被引擎改写）时的最多尝试次数。
const EDIT_ATTEMPTS: usize = 3;

/// 任务状态码（`TaskDto::status`）：0 排队、1 下载中、2 已暂停、3 已完成、5 准备中，其余视为失败。
pub(super) const STATUS_PENDING: i32 = 0;
pub(super) const STATUS_DOWNLOADING: i32 = 1;
pub(super) const STATUS_PAUSED: i32 = 2;
pub(super) const STATUS_COMPLETED: i32 = 3;
pub(super) const STATUS_PREPARING: i32 = 5;

/// 本机 RPC 句柄：生产实现是 [`ToolContext`]，单测用记录式替身。
pub(super) trait ToolRpc: Sync {
    /// 以本机身份调 gateway 方法，返回 data。
    fn call(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, ToolError>> + Send;
    /// 当前 daemon 配置（`daemon.config.get`）。
    fn daemon_config(&self)
    -> impl Future<Output = Result<DaemonConfigSnapshot, ToolError>> + Send;
    /// 当前 agent 偏好。
    fn preferences(&self) -> impl Future<Output = Result<AgentPreferencesDto, ToolError>> + Send;
}

impl ToolRpc for ToolContext {
    async fn call(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        ToolContext::call(self, method, params).await
    }

    async fn daemon_config(&self) -> Result<DaemonConfigSnapshot, ToolError> {
        ToolContext::daemon_config(self).await
    }

    async fn preferences(&self) -> Result<AgentPreferencesDto, ToolError> {
        ToolContext::preferences(self).await
    }
}

pub(super) async fn create_tasks(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    create_tasks_with(ctx, args).await
}

/// `extended` = 工具版本 2:开放重新下载、换源、BT 校验 / 重新汇报 / 做种限制 / 文件优先级。
pub(super) async fn manage_tasks(
    ctx: &ToolContext,
    args: Value,
    extended: bool,
) -> Result<Value, ToolError> {
    manage_tasks_versioned(ctx, args, extended).await
}

// ───────────────────────── 通用校验 / 转换 ─────────────────────────

pub(super) fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments(message.into())
}

pub(super) fn encode<T: Serialize>(method: &str, value: &T) -> Result<Value, ToolError> {
    serde_json::to_value(value)
        .map_err(|error| ToolError::Rpc(format!("cannot encode {method} params: {error}")))
}

/// 调 RPC 并把 data 解析成 `T`。
pub(super) async fn rpc_json<T: DeserializeOwned>(
    rpc: &impl ToolRpc,
    method: &str,
    params: Value,
) -> Result<T, ToolError> {
    let value = rpc.call(method, params).await?;
    serde_json::from_value(value)
        .map_err(|error| ToolError::Rpc(format!("{method}: unexpected response: {error}")))
}

/// JSON Schema `maxLength` 按字符（Unicode 码点）计。
pub(super) fn check_max(field: &str, value: &str, max: usize) -> Result<(), ToolError> {
    if value.chars().count() > max {
        return Err(invalid(format!("{field} must not exceed {max} characters")));
    }
    Ok(())
}

pub(super) fn check_no_control(field: &str, value: &str) -> Result<(), ToolError> {
    if value.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

/// 该错误是否应继续上抛（取消）；其余 RPC 失败只记在对应条目上。
pub(super) fn soft(error: ToolError) -> Result<String, ToolError> {
    match error {
        ToolError::Cancelled => Err(ToolError::Cancelled),
        other => Ok(other.to_string()),
    }
}

pub(super) fn is_conflict(message: &str) -> bool {
    message.to_ascii_lowercase().contains("conflict")
}

fn is_not_found(message: &str) -> bool {
    message
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase()
        .contains("notfound")
}

fn unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// 不带账号密码的代理 URL：`http(s)` / `socks5(h)`，显式端口，无路径 / 查询 / 片段。
/// 返回 scheme 小写后的规范形式；不合规返回 `None`。
pub(super) fn normalize_proxy_url(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    let explicit_port = value
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0);
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !explicit_port
        || value.ends_with('/')
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    // 引擎按字面比较 scheme 前缀：统一成小写。
    let scheme_len = url.scheme().len();
    Some(format!("{}{}", url.scheme(), value.get(scheme_len..)?))
}

/// 出口取值：`''` 跟随全局与站点规则，`direct` 强制直连，`system` 跟随系统代理，
/// 其余必须是不带账号密码的代理 URL（见 [`normalize_proxy_url`]）。
fn map_proxy(value: &str) -> Result<String, ToolError> {
    check_max("proxy", value, MAX_PROXY_CHARS)?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    match value.to_ascii_lowercase().as_str() {
        "direct" => return Ok("direct://".to_owned()),
        "system" => return Ok("system://".to_owned()),
        _ => {}
    }
    normalize_proxy_url(value).ok_or_else(|| {
        invalid(
            "proxy must be '', 'direct', 'system' or a proxy URL like 'http://host:port', \
             'https://host:port', 'socks5://host:port' or 'socks5h://host:port' without credentials",
        )
    })
}

/// KiB/s → 字节/秒；0 = 不限速。
fn speed_limit_bps(kbps: u64) -> Result<i64, ToolError> {
    kbps.checked_mul(1024)
        .and_then(|bps| i64::try_from(bps).ok())
        .filter(|bps| *bps <= MAX_TASK_LIMIT_BPS)
        .ok_or_else(|| invalid("speedLimitKBps is too large"))
}

fn connections_to_segments(value: u32) -> Result<i32, ToolError> {
    if value > MAX_CONNECTIONS {
        return Err(invalid(format!(
            "connections must be between 0 and {MAX_CONNECTIONS}"
        )));
    }
    i32::try_from(value).map_err(|_| invalid("connections is too large"))
}

fn check_user_agent(value: &str) -> Result<(), ToolError> {
    check_max("userAgent", value, MAX_USER_AGENT_CHARS)?;
    reqwest::header::HeaderValue::from_str(value)
        .map(|_| ())
        .map_err(|_| invalid("userAgent is not a valid HTTP header value"))
}

/// 绝对目录（agent 与 daemon 同机）；不含 NUL / 控制字符。
pub(super) fn check_abs_dir(field: &str, value: &str) -> Result<(), ToolError> {
    check_max(field, value, MAX_SAVE_DIR_CHARS)?;
    check_no_control(field, value)?;
    if !std::path::Path::new(value.trim()).is_absolute() {
        return Err(invalid(format!(
            "{field} must be an absolute directory path"
        )));
    }
    Ok(())
}

fn check_tags(tags: &[String]) -> Result<Vec<String>, ToolError> {
    if tags.len() > MAX_TAGS {
        return Err(invalid(format!(
            "tags must contain at most {MAX_TAGS} entries"
        )));
    }
    tags.iter()
        .map(|tag| -> Result<String, ToolError> {
            check_max("tags[]", tag, MAX_TAG_CHARS)?;
            check_no_control("tags[]", tag)?;
            Ok(tag.trim().to_owned())
        })
        .collect()
}

/// 额外请求头：至多 32 条，名 / 值合法，大小写不敏感去重；`Cookie` 头单独提出（走任务的 cookies 字段）。
fn split_headers(
    raw: &BTreeMap<String, String>,
) -> Result<(HashMap<String, String>, Option<String>), ToolError> {
    if raw.len() > MAX_HEADERS {
        return Err(invalid(format!(
            "headers must contain at most {MAX_HEADERS} entries"
        )));
    }
    let mut headers = HashMap::with_capacity(raw.len());
    let mut cookie = None;
    let mut seen = HashSet::with_capacity(raw.len());
    for (name, value) in raw {
        reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid(format!("header name {name:?} is not valid")))?;
        check_max("headers[] value", value, MAX_HEADER_VALUE_CHARS)?;
        reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| invalid(format!("value of header {name:?} is not valid")))?;
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(invalid(format!("header {name:?} is given more than once")));
        }
        if name.eq_ignore_ascii_case("cookie") {
            cookie = Some(value.clone());
        } else {
            headers.insert(name.clone(), value.clone());
        }
    }
    Ok((headers, cookie))
}

// ───────────────────────── 队列 / 分类解析 ─────────────────────────

/// 队列 ID 或精确队列名 → 队列 ID。
fn pick_queue(queues: &[QueueDto], wanted: &str) -> Result<String, ToolError> {
    let wanted = wanted.trim();
    if let Some(queue) = queues.iter().find(|queue| queue.queue_id == wanted) {
        return Ok(queue.queue_id.clone());
    }
    let by_name = |exact: bool| {
        queues
            .iter()
            .filter(|queue| {
                if exact {
                    queue.name == wanted
                } else {
                    queue.name.eq_ignore_ascii_case(wanted)
                }
            })
            .collect::<Vec<_>>()
    };
    for exact in [true, false] {
        match by_name(exact).as_slice() {
            [] => {}
            [queue] => return Ok(queue.queue_id.clone()),
            _ => {
                return Err(invalid(format!(
                    "queue name {wanted:?} matches several queues; use the queue id"
                )));
            }
        }
    }
    let available = queues
        .iter()
        .map(|queue| format!("{} ({})", queue.name, queue.queue_id))
        .collect::<Vec<_>>()
        .join(", ");
    Err(invalid(format!(
        "unknown queue {wanted:?}; available queues: {available}"
    )))
}

pub(super) async fn resolve_queue(rpc: &impl ToolRpc, wanted: &str) -> Result<String, ToolError> {
    let queues: Vec<QueueDto> = rpc_json(rpc, method::DAEMON_QUEUE_LIST, json!({})).await?;
    pick_queue(&queues, wanted)
}

/// 分类选择结果：写进任务的覆盖 ID 与该分类配置的保存目录。
#[derive(Debug, PartialEq, Eq)]
struct CategoryChoice {
    id: String,
    save_dir: String,
}

fn category_label(category: &CustomCategoryDto) -> String {
    if category.name.is_empty() {
        category
            .builtin_type
            .clone()
            .unwrap_or_else(|| category.id.clone())
    } else {
        category.name.clone()
    }
}

/// 分类名（自定义名 / 内置类型名 / 分类 ID，大小写不敏感）→ 分类。内置「全部」不是可选分类。
fn pick_category(
    categories: &[CustomCategoryDto],
    wanted: &str,
) -> Result<CategoryChoice, ToolError> {
    let wanted = wanted.trim();
    let lowered = wanted.to_lowercase();
    let selectable = || {
        categories
            .iter()
            .filter(|category| category.builtin_type.as_deref() != Some("all"))
    };
    let found = selectable().find(|category| {
        category.id == wanted
            || category
                .builtin_type
                .as_deref()
                .is_some_and(|kind| kind.to_lowercase() == lowered)
            || (!category.name.is_empty() && category.name.to_lowercase() == lowered)
    });
    match found {
        Some(category) => Ok(CategoryChoice {
            id: category.id.clone(),
            save_dir: category.save_dir.trim().to_owned(),
        }),
        None => {
            let available = selectable()
                .map(category_label)
                .collect::<Vec<_>>()
                .join(", ");
            Err(invalid(format!(
                "unknown category {wanted:?}; available categories: {available}"
            )))
        }
    }
}

// ───────────────────────── 统一任务编辑 ─────────────────────────

/// 一次任务属性修改（`daemon.task.edit`）；未设置的字段保持原样。
#[derive(Debug, Default, Clone, PartialEq)]
struct EditPlan {
    proxy_url: Option<String>,
    segments: Option<i32>,
    download_limit_bps: Option<i64>,
    user_agent: Option<String>,
    headers: Option<HashMap<String, String>>,
    cookies: Option<String>,
    save_dir: Option<String>,
    file_name: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
}

impl EditPlan {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 该任务当前能力不支持的字段名（空 = 全部可改）。
    fn unsupported(&self, caps: &TaskEditCapabilities) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.proxy_url.is_some() && !caps.proxy {
            fields.push("proxy");
        }
        if self.segments.is_some() && !caps.segments {
            fields.push("connections");
        }
        if self.user_agent.is_some() && !caps.request {
            fields.push("userAgent");
        }
        if (self.headers.is_some() || self.cookies.is_some()) && !caps.request {
            fields.push("headers");
        }
        if self.save_dir.is_some() && !caps.storage {
            fields.push("saveDir");
        }
        if self.file_name.is_some() && !caps.rename {
            fields.push("fileName");
        }
        fields
    }

    fn to_params(&self, snapshot: &TaskEditSnapshot) -> EditTaskParams {
        EditTaskParams {
            task_id: snapshot.task_id.clone(),
            expected_revision: snapshot.revision.clone(),
            save_dir: self.save_dir.clone(),
            file_name: self.file_name.clone(),
            proxy_url: self.proxy_url.clone(),
            segments: self.segments,
            download_limit_bps: self.download_limit_bps,
            user_agent: self.user_agent.clone(),
            cookies: self.cookies.clone(),
            headers: self.headers.clone(),
            category: self.category.clone(),
            tags: self.tags.clone(),
            ..EditTaskParams::default()
        }
    }
}

enum EditOutcome {
    Applied,
    /// 任务不支持其中的字段等：整条不改，原因给模型看。
    Skipped(String),
}

/// 对一个任务应用 [`EditPlan`]：先读可编辑快照取 revision，再整块提交；
/// 任务刚被引擎改写（`edit-conflict`）时重读重试。
async fn edit_one(
    rpc: &impl ToolRpc,
    plan: &EditPlan,
    task_id: &str,
) -> Result<EditOutcome, ToolError> {
    for attempt in 1..=EDIT_ATTEMPTS {
        let snapshot: TaskEditSnapshot = rpc_json(
            rpc,
            method::DAEMON_TASK_EDIT_INFO,
            json!({ "taskId": task_id }),
        )
        .await?;
        let unsupported = plan.unsupported(&snapshot.capabilities);
        if !unsupported.is_empty() {
            return Ok(EditOutcome::Skipped(format!(
                "this task does not support changing: {}",
                unsupported.join(", ")
            )));
        }
        let params = encode(method::DAEMON_TASK_EDIT, &plan.to_params(&snapshot))?;
        match rpc.call(method::DAEMON_TASK_EDIT, params).await {
            Ok(_) => return Ok(EditOutcome::Applied),
            Err(ToolError::Rpc(message)) if attempt < EDIT_ATTEMPTS && is_conflict(&message) => {}
            Err(error) => return Err(error),
        }
    }
    Err(ToolError::Rpc(
        "the task kept changing while it was being edited".to_owned(),
    ))
}

/// `daemon.task.get` 的最小投影。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TaskView {
    #[serde(default)]
    pub(super) task_id: String,
    #[serde(default)]
    pub(super) file_name: String,
    #[serde(default)]
    pub(super) status: i32,
    #[serde(default)]
    pub(super) seeding_status: i32,
    #[serde(default)]
    pub(super) file_missing: bool,
    #[serde(default)]
    pub(super) created_at: String,
    #[serde(default)]
    pub(super) queue_id: String,
    #[serde(default)]
    pub(super) url: String,
    #[serde(default)]
    pub(super) origin_url: String,
    #[serde(default)]
    pub(super) save_dir: String,
    #[serde(default)]
    pub(super) referrer: String,
    #[serde(default)]
    pub(super) proxy_url: String,
    #[serde(default)]
    pub(super) checksum: String,
    #[serde(default)]
    pub(super) ignore_tls_errors: bool,
    /// 做种限制：与 `TaskDto` 一致，缺省 = 跟随全局（线上哨兵 -2，序列化时省略）。
    #[serde(default = "seed_limit_inherit")]
    pub(super) seed_ratio_limit_milli: i64,
    #[serde(default = "seed_limit_inherit")]
    pub(super) seed_post_ratio_limit_milli: i64,
    #[serde(default = "seed_limit_inherit")]
    pub(super) seed_time_limit_minutes: i64,
    #[serde(default = "seed_limit_inherit")]
    pub(super) seed_inactive_time_limit_minutes: i64,
    #[serde(default)]
    pub(super) seed_upload_limit_bps: i64,
}

/// 任务级做种限制的「跟随全局」哨兵（与 `native/protocol` 一致）。
pub(super) const SEED_LIMIT_FOLLOW_GLOBAL: i64 = -2;

fn seed_limit_inherit() -> i64 {
    SEED_LIMIT_FOLLOW_GLOBAL
}

/// 本地种子文件任务的地址哨兵。
const TORRENT_FILE_SCHEME: &str = "torrent-file://";

/// 任务地址是否属于 BitTorrent（`magnet:` / `torrent-file://` 哨兵，不分大小写）。
pub(super) fn is_bt_url(url: &str) -> bool {
    let url = url.trim_start();
    ["magnet:", TORRENT_FILE_SCHEME].iter().any(|prefix| {
        url.as_bytes()
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
    })
}

/// 任务地址是否为本地种子文件哨兵。
pub(super) fn is_torrent_file_url(url: &str) -> bool {
    url.trim_start()
        .as_bytes()
        .get(..TORRENT_FILE_SCHEME.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(TORRENT_FILE_SCHEME.as_bytes()))
}

impl TaskView {
    /// 暂停对其有意义：排队 / 下载 / 准备中，或正在（排队）做种。
    fn can_pause(&self) -> bool {
        matches!(
            self.status,
            STATUS_PENDING | STATUS_DOWNLOADING | STATUS_PREPARING
        ) || (self.status == STATUS_COMPLETED && matches!(self.seeding_status, 1 | 8))
    }

    /// 失败：状态码不在已知的非失败集合内（与 GPUI 的展示态映射一致）。
    pub(super) fn is_failed(&self) -> bool {
        !matches!(
            self.status,
            STATUS_PENDING
                | STATUS_DOWNLOADING
                | STATUS_PAUSED
                | STATUS_COMPLETED
                | STATUS_PREPARING
        )
    }

    /// 继续对其有意义：已暂停、失败（重试），或已停止做种且产物仍在。
    fn can_resume(&self) -> bool {
        self.status == STATUS_PAUSED
            || self.is_failed()
            || (self.status == STATUS_COMPLETED
                && (2..=7).contains(&self.seeding_status)
                && !self.file_missing)
    }

    pub(super) fn is_bt(&self) -> bool {
        is_bt_url(&self.url)
    }

    pub(super) fn describe(&self) -> Value {
        json!({ "taskId": self.task_id, "name": self.file_name })
    }

    pub(super) fn describe_with(&self, reason: &str) -> Value {
        json!({ "taskId": self.task_id, "name": self.file_name, "reason": reason })
    }
}

pub(super) async fn fetch_task(rpc: &impl ToolRpc, task_id: &str) -> Result<TaskView, ToolError> {
    let mut task: TaskView =
        rpc_json(rpc, method::DAEMON_TASK_GET, json!({ "taskId": task_id })).await?;
    if task.task_id.is_empty() {
        task.task_id = task_id.to_owned();
    }
    Ok(task)
}

// ───────────────────────── fd_create_tasks ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateArgs {
    pub(super) items: Vec<ItemArgs>,
    #[serde(default)]
    pub(super) options: CreateOptions,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ItemArgs {
    pub(super) url: String,
    #[serde(default)]
    pub(super) file_name: String,
    #[serde(default)]
    checksum: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateOptions {
    pub(super) save_dir: Option<String>,
    pub(super) category: Option<String>,
    pub(super) queue: Option<String>,
    connections: Option<u32>,
    proxy: Option<String>,
    #[serde(rename = "speedLimitKBps")]
    speed_limit_kbps: Option<u64>,
    user_agent: Option<String>,
    referrer: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    tags: Option<Vec<String>>,
    ignore_tls_errors: Option<bool>,
    pub(super) start_paused: Option<bool>,
}

impl CreateArgs {
    /// Schema 层面的结构约束（条数 / 长度）；违反即整次调用无效。
    fn check(&self) -> Result<(), ToolError> {
        if self.items.is_empty() || self.items.len() > MAX_CREATE_ITEMS {
            return Err(invalid(format!(
                "items must contain 1..={MAX_CREATE_ITEMS} entries"
            )));
        }
        for (index, item) in self.items.iter().enumerate() {
            let url_chars = item.url.chars().count();
            if url_chars == 0 || url_chars > MAX_URL_CHARS {
                return Err(invalid(format!(
                    "items[{index}].url must be 1..={MAX_URL_CHARS} characters"
                )));
            }
            check_max(
                &format!("items[{index}].fileName"),
                &item.file_name,
                MAX_FILE_NAME_CHARS,
            )?;
            check_max(
                &format!("items[{index}].checksum"),
                &item.checksum,
                MAX_CHECKSUM_CHARS,
            )?;
        }
        Ok(())
    }
}

/// 整块共享选项的映射结果（队列 / 分类需要查询，由调用方补全）。
struct CreatePlan {
    request: CreateTaskRequest,
    /// 建后统一属性（限速 / 分类 / 标签：`createMany` 不带，只能经统一编辑写入）。
    post: EditPlan,
    queue: Option<String>,
    category: Option<String>,
}

fn plan_create(options: &CreateOptions) -> Result<CreatePlan, ToolError> {
    let mut request = CreateTaskRequest {
        url: String::new(),
        file_name: String::new(),
        save_dir: String::new(),
        segments: 0,
        cookies: String::new(),
        referrer: String::new(),
        proxy_url: String::new(),
        user_agent: String::new(),
        queue_id: String::new(),
        checksum: String::new(),
        ignore_tls_errors: options.ignore_tls_errors.unwrap_or(false),
        headers: None,
        torrent_b64: None,
        method: None,
        body: None,
        audio_url: None,
        start_paused: options.start_paused.unwrap_or(false),
        http_user: String::new(),
        http_password: String::new(),
        save_site_auth: false,
    };
    let mut post = EditPlan::default();

    if let Some(dir) = options
        .save_dir
        .as_deref()
        .filter(|dir| !dir.trim().is_empty())
    {
        check_abs_dir("saveDir", dir)?;
        request.save_dir = dir.trim().to_owned();
    }
    if let Some(category) = &options.category {
        check_max("category", category, MAX_CATEGORY_CHARS)?;
    }
    if let Some(queue) = &options.queue {
        check_max("queue", queue, MAX_QUEUE_CHARS)?;
    }
    if let Some(connections) = options.connections {
        request.segments = connections_to_segments(connections)?;
    }
    if let Some(proxy) = &options.proxy {
        request.proxy_url = map_proxy(proxy)?;
    }
    if let Some(kbps) = options.speed_limit_kbps {
        let bps = speed_limit_bps(kbps)?;
        // 0 = 不限速，就是建任务的默认值，不必再写一次属性。
        if bps > 0 {
            post.download_limit_bps = Some(bps);
        }
    }
    if let Some(user_agent) = &options.user_agent {
        check_user_agent(user_agent)?;
        request.user_agent.clone_from(user_agent);
    }
    if let Some(referrer) = &options.referrer {
        check_max("referrer", referrer, MAX_REFERRER_CHARS)?;
        check_no_control("referrer", referrer)?;
        request.referrer.clone_from(referrer);
    }
    if let Some(raw) = &options.headers {
        let (headers, cookie) = split_headers(raw)?;
        request.headers = (!headers.is_empty()).then_some(headers);
        if let Some(cookie) = cookie {
            request.cookies = cookie;
        }
    }
    if let Some(tags) = &options.tags {
        let tags = check_tags(tags)?;
        if !tags.is_empty() {
            post.tags = Some(tags);
        }
    }
    Ok(CreatePlan {
        request,
        post,
        queue: options.queue.clone(),
        category: options.category.clone(),
    })
}

/// 条目级校验：协议白名单、文件名不得逃出保存目录、校验值形态。违规只让该条失败。
fn validate_item(item: &ItemArgs) -> Result<CreateTaskItem, String> {
    let url = item.url.trim();
    if url.is_empty() {
        return Err("url must not be empty".to_owned());
    }
    let lowered = url.to_ascii_lowercase();
    if lowered.starts_with("magnet:") {
        if !lowered.starts_with("magnet:?") {
            return Err("not a valid magnet link".to_owned());
        }
    } else if lowered.starts_with("ed2k://") {
        // eD2K 链接由 daemon 解析。
    } else {
        let parsed = reqwest::Url::parse(url).map_err(|_| "not a valid URL".to_owned())?;
        if !matches!(parsed.scheme(), "http" | "https" | "ftp" | "ftps")
            || parsed.host_str().is_none()
        {
            return Err(
                "unsupported link: only http(s), ftp, magnet and ed2k links can be downloaded"
                    .to_owned(),
            );
        }
    }
    let file_name = item.file_name.trim();
    if !file_name.is_empty() {
        let unsafe_name = file_name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\\' | ':'))
            || file_name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..");
        if unsafe_name {
            return Err(
                "fileName must be a plain name or a relative 'subfolder/name' inside the save directory"
                    .to_owned(),
            );
        }
    }
    let checksum = item.checksum.trim();
    if !checksum.is_empty() {
        let well_formed = checksum
            .split_once('=')
            .is_some_and(|(algo, hash)| !algo.is_empty() && !hash.trim().is_empty());
        if !well_formed {
            return Err("checksum must look like 'sha256=<hex digest>'".to_owned());
        }
    }
    Ok(CreateTaskItem {
        url: url.to_owned(),
        file_name: file_name.to_owned(),
        checksum: checksum.to_owned(),
    })
}

async fn create_tasks_with(rpc: &impl ToolRpc, args: Value) -> Result<Value, ToolError> {
    let args: CreateArgs = parse_args(args)?;
    args.check()?;
    let mut plan = plan_create(&args.options)?;

    let mut valid: Vec<(usize, CreateTaskItem)> = Vec::with_capacity(args.items.len());
    let mut rejected: HashMap<usize, String> = HashMap::new();
    for (index, item) in args.items.iter().enumerate() {
        match validate_item(item) {
            Ok(item) => valid.push((index, item)),
            Err(message) => {
                rejected.insert(index, message);
            }
        }
    }

    let mut outcomes: HashMap<usize, Value> = HashMap::new();
    if !valid.is_empty() {
        if let Some(queue) = plan
            .queue
            .as_deref()
            .filter(|queue| !queue.trim().is_empty())
        {
            plan.request.queue_id = resolve_queue(rpc, queue).await?;
        }
        let mut route = None;
        let wants_category = plan
            .category
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned);
        if wants_category.is_some() || plan.request.save_dir.is_empty() {
            let prefs = rpc.preferences().await?;
            let stored = prefs.values.get(CUSTOM_CATEGORIES_PREF_KEY);
            if let Some(name) = wants_category {
                let categories = CustomCategoryDto::from_preference(stored);
                let choice = pick_category(&categories, &name)?;
                if plan.request.save_dir.is_empty() && !choice.save_dir.is_empty() {
                    plan.request.save_dir = choice.save_dir;
                }
                plan.post.category = Some(choice.id);
            } else {
                // 没有指定目录与分类：与应用里的新建一致，按文件名走分类目录路由。
                route = CategoryRouteDto::from_preference(stored);
            }
        }

        // 开启 URL 去重时 createMany 可能回已有任务的 ID：记下调用时刻，只给本次新建的任务补写属性。
        let dedup_since = if plan.post.is_empty() {
            None
        } else {
            rpc.daemon_config()
                .await?
                .values
                .get("dedup_same_url")
                .is_some_and(|value| value == "true")
                .then(unix_secs)
        };
        let count = valid.len();
        let params = DaemonCreateTasksParams {
            batch_id: uuid::Uuid::new_v4().to_string(),
            common: plan.request.clone(),
            items: valid.iter().map(|(_, item)| item.clone()).collect(),
            category_route: route,
        };
        debug_assert!(params.batch_id.len() <= CREATE_TASKS_BATCH_ID_MAX_LEN);
        let result: DaemonCreateTasksResult = rpc_json(
            rpc,
            method::DAEMON_TASK_CREATE_MANY,
            encode(method::DAEMON_TASK_CREATE_MANY, &params)?,
        )
        .await?;
        if result.results.len() != count {
            return Err(ToolError::Rpc(format!(
                "{} returned {} results for {count} items",
                method::DAEMON_TASK_CREATE_MANY,
                result.results.len()
            )));
        }
        for ((index, _), outcome) in valid.iter().zip(result.results) {
            let entry = match (outcome.task_id, outcome.error) {
                (Some(task_id), _) => {
                    let mut entry =
                        json!({ "index": index, "status": "created", "taskId": task_id });
                    if !plan.post.is_empty() {
                        let note =
                            apply_new_task_props(rpc, &plan.post, &task_id, dedup_since).await?;
                        if let Some(note) = note {
                            entry["warning"] = Value::String(note);
                        }
                    }
                    entry
                }
                (None, Some(error)) => {
                    json!({ "index": index, "status": "error", "error": encode("error", &error)? })
                }
                (None, None) => json!({
                    "index": index,
                    "status": "error",
                    "error": "the daemon returned neither a task id nor an error",
                }),
            };
            outcomes.insert(*index, entry);
        }
    }

    let mut results = Vec::with_capacity(args.items.len());
    let (mut created, mut failed) = (0_usize, 0_usize);
    for index in 0..args.items.len() {
        let entry = match (outcomes.remove(&index), rejected.remove(&index)) {
            (Some(entry), _) => entry,
            (None, Some(message)) => json!({ "index": index, "status": "error", "error": message }),
            (None, None) => json!({ "index": index, "status": "error", "error": "not processed" }),
        };
        if entry["status"] == "created" {
            created += 1;
        } else {
            failed += 1;
        }
        results.push(entry);
    }
    Ok(
        json!({ "requested": args.items.len(), "created": created, "failed": failed, "results": results }),
    )
}

/// 给刚建好的任务写限速 / 分类 / 标签（`createMany` 不带这些）。返回 `Some(说明)` = 任务已建但属性没写上。
/// `dedup_since` 有值 = 开启了 URL 去重：创建时间早于它的任务是已有任务，不改，免得动到别的任务。
async fn apply_new_task_props(
    rpc: &impl ToolRpc,
    plan: &EditPlan,
    task_id: &str,
    dedup_since: Option<i64>,
) -> Result<Option<String>, ToolError> {
    let not_applied = |reason: &str| {
        Some(format!(
            "created, but its options were not applied: {reason}"
        ))
    };
    if let Some(since) = dedup_since {
        let task = match fetch_task(rpc, task_id).await {
            Ok(task) => task,
            Err(error) => return soft(error).map(|message| not_applied(&message)),
        };
        if task
            .created_at
            .parse::<i64>()
            .is_ok_and(|created| created < since.saturating_sub(2))
        {
            return Ok(Some(
                "this URL already existed as an earlier task; the existing task was returned and \
                 the per-task options (speed limit, category, tags) were not applied"
                    .to_owned(),
            ));
        }
    }
    match edit_one(rpc, plan, task_id).await {
        Ok(EditOutcome::Applied) => Ok(None),
        Ok(EditOutcome::Skipped(reason)) => Ok(not_applied(&reason)),
        Err(error) => soft(error).map(|message| not_applied(&message)),
    }
}

// ───────────────────────── fd_manage_tasks ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ManageArgs {
    pub(super) task_ids: Vec<String>,
    pub(super) action: ManageAction,
    pub(super) delete_files: Option<bool>,
    pub(super) queue: Option<String>,
    pub(super) edit: Option<EditArgs>,
    /// 版本 2:`changeUrl` 的新地址。
    pub(super) url: Option<String>,
    /// 版本 2:`setSeedLimits` 的做种限制(只写给出的字段,其余保持原值)。
    pub(super) seed_limits: Option<SeedLimitsArgs>,
    /// 版本 2:`setFilePriorities` 的文件优先级。
    pub(super) file_priorities: Option<Vec<FilePriorityArg>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ManageAction {
    Pause,
    Resume,
    Delete,
    MoveToQueue,
    Edit,
    Restart,
    ChangeUrl,
    Recheck,
    Reannounce,
    SetSeedLimits,
    SetFilePriorities,
}

impl ManageAction {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Delete => "delete",
            Self::MoveToQueue => "moveToQueue",
            Self::Edit => "edit",
            Self::Restart => "restart",
            Self::ChangeUrl => "changeUrl",
            Self::Recheck => "recheck",
            Self::Reannounce => "reannounce",
            Self::SetSeedLimits => "setSeedLimits",
            Self::SetFilePriorities => "setFilePriorities",
        }
    }

    /// 工具版本 2 才有的动作。
    pub(super) fn is_extended(self) -> bool {
        !matches!(
            self,
            Self::Pause | Self::Resume | Self::Delete | Self::MoveToQueue | Self::Edit
        )
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct EditArgs {
    pub(super) proxy: Option<String>,
    pub(super) connections: Option<u32>,
    #[serde(rename = "speedLimitKBps")]
    pub(super) speed_limit_kbps: Option<u64>,
    pub(super) user_agent: Option<String>,
    pub(super) headers: Option<BTreeMap<String, String>>,
    pub(super) save_dir: Option<String>,
    pub(super) file_name: Option<String>,
    pub(super) category: Option<String>,
    pub(super) tags: Option<Vec<String>>,
}

/// 校验后的管理请求：字段与动作的搭配、去重后的任务 ID。
struct ManagePlan {
    ids: Vec<String>,
    action: ManageAction,
    delete_files: bool,
    queue: String,
    edit: EditPlan,
    /// `edit.category` 里待解析的分类名（空串 = 清除分类）。
    edit_category: Option<String>,
    /// 版本 2 动作的参数。
    ext: task_ops::ExtPlan,
}

fn plan_manage(args: ManageArgs, extended: bool) -> Result<ManagePlan, ToolError> {
    task_ops::check_fields(&args, extended)?;
    if args.task_ids.is_empty() || args.task_ids.len() > MAX_MANAGE_IDS {
        return Err(invalid(format!(
            "taskIds must contain 1..={MAX_MANAGE_IDS} entries"
        )));
    }
    let mut seen = HashSet::with_capacity(args.task_ids.len());
    let mut ids = Vec::with_capacity(args.task_ids.len());
    for id in args.task_ids {
        let chars = id.chars().count();
        if chars == 0 || chars > MAX_ID_CHARS {
            return Err(invalid(format!(
                "each task id must be 1..={MAX_ID_CHARS} characters"
            )));
        }
        if seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    let action = args.action;
    if args.delete_files.is_some() && action != ManageAction::Delete {
        return Err(invalid("deleteFiles is only valid with action 'delete'"));
    }
    if args.queue.is_some() && action != ManageAction::MoveToQueue {
        return Err(invalid("queue is only valid with action 'moveToQueue'"));
    }
    if args.edit.is_some() && action != ManageAction::Edit {
        return Err(invalid("edit is only valid with action 'edit'"));
    }
    let mut plan = ManagePlan {
        ids,
        action,
        delete_files: args.delete_files.unwrap_or(false),
        queue: String::new(),
        edit: EditPlan::default(),
        edit_category: None,
        ext: task_ops::ExtPlan::default(),
    };
    match action {
        ManageAction::Pause | ManageAction::Resume | ManageAction::Delete => {}
        ManageAction::MoveToQueue => {
            let queue = args
                .queue
                .ok_or_else(|| invalid("queue is required with action 'moveToQueue'"))?;
            check_max("queue", &queue, MAX_QUEUE_CHARS)?;
            if queue.trim().is_empty() {
                return Err(invalid("queue must not be empty with action 'moveToQueue'"));
            }
            plan.queue = queue;
        }
        ManageAction::Edit => {
            let edit = args
                .edit
                .ok_or_else(|| invalid("edit is required with action 'edit'"))?;
            plan.edit_category = edit.category.clone();
            plan.edit = plan_edit(&edit, plan.ids.len())?;
            if plan.edit.is_empty() && plan.edit_category.is_none() {
                return Err(invalid("edit must contain at least one field"));
            }
        }
        ManageAction::Restart | ManageAction::Recheck | ManageAction::Reannounce => {}
        ManageAction::ChangeUrl => {
            let url = args
                .url
                .ok_or_else(|| invalid("url is required with action 'changeUrl'"))?;
            if plan.ids.len() != 1 {
                return Err(invalid("action 'changeUrl' needs exactly one task id"));
            }
            plan.ext.url = task_ops::plan_new_url(&url)?;
        }
        ManageAction::SetSeedLimits => {
            let limits = args
                .seed_limits
                .ok_or_else(|| invalid("seedLimits is required with action 'setSeedLimits'"))?;
            plan.ext.seed = Some(task_ops::plan_seed_limits(&limits)?);
        }
        ManageAction::SetFilePriorities => {
            let items = args.file_priorities.ok_or_else(|| {
                invalid("filePriorities is required with action 'setFilePriorities'")
            })?;
            if plan.ids.len() != 1 {
                return Err(invalid(
                    "action 'setFilePriorities' needs exactly one task id",
                ));
            }
            plan.ext.priorities = task_ops::plan_priorities(&items)?;
        }
    }
    Ok(plan)
}

fn plan_edit(edit: &EditArgs, task_count: usize) -> Result<EditPlan, ToolError> {
    let mut plan = EditPlan::default();
    if let Some(proxy) = &edit.proxy {
        plan.proxy_url = Some(map_proxy(proxy)?);
    }
    if let Some(connections) = edit.connections {
        plan.segments = Some(connections_to_segments(connections)?);
    }
    if let Some(kbps) = edit.speed_limit_kbps {
        plan.download_limit_bps = Some(speed_limit_bps(kbps)?);
    }
    if let Some(user_agent) = &edit.user_agent {
        check_user_agent(user_agent)?;
        plan.user_agent = Some(user_agent.clone());
    }
    if let Some(raw) = &edit.headers {
        let (headers, cookie) = split_headers(raw)?;
        plan.headers = Some(headers);
        plan.cookies = cookie;
    }
    if let Some(dir) = &edit.save_dir {
        check_abs_dir("edit.saveDir", dir)?;
        plan.save_dir = Some(dir.trim().to_owned());
    }
    if let Some(name) = &edit.file_name {
        check_max("edit.fileName", name, MAX_FILE_NAME_CHARS)?;
        check_no_control("edit.fileName", name)?;
        let name = name.trim();
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(invalid(
                "edit.fileName must be a plain file name without path separators",
            ));
        }
        if task_count != 1 {
            return Err(invalid(
                "edit.fileName can only be used with a single task id",
            ));
        }
        plan.file_name = Some(name.to_owned());
    }
    if let Some(category) = &edit.category {
        check_max("edit.category", category, MAX_CATEGORY_CHARS)?;
    }
    if let Some(tags) = &edit.tags {
        plan.tags = Some(check_tags(tags)?);
    }
    Ok(plan)
}

#[cfg(test)]
async fn manage_tasks_with(rpc: &impl ToolRpc, args: Value) -> Result<Value, ToolError> {
    manage_tasks_versioned(rpc, args, true).await
}

pub(super) async fn manage_tasks_versioned(
    rpc: &impl ToolRpc,
    args: Value,
    extended: bool,
) -> Result<Value, ToolError> {
    let args: ManageArgs = parse_args(args)?;
    let mut plan = plan_manage(args, extended)?;

    // 参数里依赖本机状态的部分先解析，失败时一个任务都不动。
    let target_queue = if plan.action == ManageAction::MoveToQueue {
        Some(resolve_queue(rpc, &plan.queue).await?)
    } else {
        None
    };
    if let Some(name) = plan.edit_category.take() {
        plan.edit.category = Some(if name.trim().is_empty() {
            String::new()
        } else {
            let prefs = rpc.preferences().await?;
            let categories =
                CustomCategoryDto::from_preference(prefs.values.get(CUSTOM_CATEGORIES_PREF_KEY));
            pick_category(&categories, &name)?.id
        });
    }

    // 逐个解析成明确的任务；查不到的直接报告，不进入批量调用。
    let mut tasks = Vec::with_capacity(plan.ids.len());
    let mut skipped = Vec::new();
    for id in &plan.ids {
        match fetch_task(rpc, id).await {
            Ok(task) => tasks.push(task),
            Err(ToolError::Rpc(message)) if is_not_found(&message) => {
                skipped.push(json!({ "taskId": id, "reason": "task not found" }));
            }
            Err(error) => return Err(error),
        }
    }

    let mut applied = Vec::new();
    let mut failed = Vec::new();
    match plan.action {
        ManageAction::Pause | ManageAction::Resume => {
            let pausing = plan.action == ManageAction::Pause;
            let mut ids = Vec::new();
            for task in &tasks {
                let eligible = if pausing {
                    task.can_pause()
                } else {
                    task.can_resume()
                };
                if eligible {
                    ids.push(task.task_id.clone());
                    applied.push(task.describe());
                } else {
                    skipped.push(task.describe_with(skip_reason(task, pausing)));
                }
            }
            if !ids.is_empty() {
                let name = if pausing {
                    method::DAEMON_TASK_PAUSE_MANY
                } else {
                    method::DAEMON_TASK_RESUME_MANY
                };
                let params = encode(name, &DaemonTaskIdsParams { task_ids: ids })?;
                rpc.call(name, params).await?;
            }
        }
        ManageAction::Delete => {
            let ids = tasks
                .iter()
                .map(|task| task.task_id.clone())
                .collect::<Vec<_>>();
            applied.extend(tasks.iter().map(TaskView::describe));
            if !ids.is_empty() {
                let params = DaemonDeleteTasksParams {
                    task_ids: ids,
                    delete_files: plan.delete_files,
                };
                rpc.call(
                    method::DAEMON_TASK_DELETE_MANY,
                    encode(method::DAEMON_TASK_DELETE_MANY, &params)?,
                )
                .await?;
            }
        }
        ManageAction::MoveToQueue => {
            let queue_id = target_queue.unwrap_or_default();
            for task in &tasks {
                if task.status == STATUS_COMPLETED {
                    skipped.push(task.describe_with("task is completed"));
                } else if task.queue_id == queue_id {
                    skipped.push(task.describe_with("task is already in that queue"));
                } else {
                    let params = json!({ "taskId": task.task_id, "queueId": queue_id });
                    match rpc.call(method::DAEMON_QUEUE_MOVE_TASK, params).await {
                        Ok(_) => applied.push(task.describe()),
                        Err(error) => failed.push(task.describe_with(&soft(error)?)),
                    }
                }
            }
        }
        ManageAction::Edit => {
            let moves_files = plan.edit.save_dir.is_some() || plan.edit.file_name.is_some();
            for task in &tasks {
                if moves_files && task.status == STATUS_COMPLETED {
                    skipped.push(task.describe_with(
                        "the save directory and file name can only be changed for unfinished tasks",
                    ));
                    continue;
                }
                match edit_one(rpc, &plan.edit, &task.task_id).await {
                    Ok(EditOutcome::Applied) => applied.push(task.describe()),
                    Ok(EditOutcome::Skipped(reason)) => skipped.push(task.describe_with(&reason)),
                    Err(error) => failed.push(task.describe_with(&soft(error)?)),
                }
            }
        }
        ManageAction::Restart => {
            let outcome = task_ops::restart(rpc, &tasks).await?;
            outcome.merge(&mut applied, &mut skipped, &mut failed);
        }
        ManageAction::ChangeUrl => {
            let outcome = task_ops::change_url(rpc, &tasks, &plan.ext.url).await?;
            outcome.merge(&mut applied, &mut skipped, &mut failed);
        }
        ManageAction::Recheck => {
            let outcome = task_ops::recheck(rpc, &tasks).await?;
            outcome.merge(&mut applied, &mut skipped, &mut failed);
        }
        ManageAction::Reannounce => {
            let outcome = task_ops::reannounce(rpc, &tasks).await?;
            outcome.merge(&mut applied, &mut skipped, &mut failed);
        }
        ManageAction::SetSeedLimits => {
            let seed = plan.ext.seed.unwrap_or_default();
            let outcome = task_ops::set_seed_limits(rpc, &tasks, seed).await?;
            outcome.merge(&mut applied, &mut skipped, &mut failed);
        }
        ManageAction::SetFilePriorities => {
            let outcome = task_ops::set_file_priorities(rpc, &tasks, &plan.ext.priorities).await?;
            outcome.merge(&mut applied, &mut skipped, &mut failed);
        }
    }

    let mut report = Map::new();
    report.insert("action".to_owned(), json!(plan.action.as_str()));
    report.insert("requested".to_owned(), json!(plan.ids.len()));
    if plan.action == ManageAction::Delete {
        report.insert("filesDeleted".to_owned(), json!(plan.delete_files));
    }
    report.insert("applied".to_owned(), Value::Array(applied));
    report.insert("skipped".to_owned(), Value::Array(skipped));
    report.insert("failed".to_owned(), Value::Array(failed));
    Ok(Value::Object(report))
}

fn skip_reason(task: &TaskView, pausing: bool) -> &'static str {
    if task.status == STATUS_COMPLETED {
        return "task is completed";
    }
    match (pausing, task.status) {
        (true, STATUS_PAUSED) => "task is already paused",
        (true, _) if task.is_failed() => "task has failed; use 'resume' to retry it",
        (false, STATUS_PENDING | STATUS_DOWNLOADING | STATUS_PREPARING) => {
            "task is already running or queued"
        }
        _ => "task is not in a state this action applies to",
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use std::sync::Mutex;

    use fluxdown_protocol::{AgentPreferencesDto, DaemonConfigSnapshot};
    use serde_json::Value;

    use super::{ToolError, ToolRpc};

    type Handler = Box<dyn Fn(&str, &Value) -> Result<Value, ToolError> + Send + Sync>;

    /// 记录每次调用的 RPC 替身；`handler` 决定应答。
    pub(in crate::assistant::tools) struct MockRpc {
        pub calls: Mutex<Vec<(String, Value)>>,
        handler: Handler,
        pub config: DaemonConfigSnapshot,
        pub prefs: AgentPreferencesDto,
    }

    impl MockRpc {
        pub fn new(
            handler: impl Fn(&str, &Value) -> Result<Value, ToolError> + Send + Sync + 'static,
        ) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                handler: Box::new(handler),
                config: DaemonConfigSnapshot::default(),
                prefs: AgentPreferencesDto::default(),
            }
        }

        /// 某方法被调用的全部参数（按调用顺序）。
        pub fn calls_of(&self, method: &str) -> Vec<Value> {
            self.calls
                .lock()
                .map(|calls| {
                    calls
                        .iter()
                        .filter(|(name, _)| name == method)
                        .map(|(_, params)| params.clone())
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    impl ToolRpc for MockRpc {
        async fn call(&self, method: &str, params: Value) -> Result<Value, ToolError> {
            let result = (self.handler)(method, &params);
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((method.to_owned(), params));
            }
            result
        }

        async fn daemon_config(&self) -> Result<DaemonConfigSnapshot, ToolError> {
            Ok(self.config.clone())
        }

        async fn preferences(&self) -> Result<AgentPreferencesDto, ToolError> {
            Ok(self.prefs.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        AgentPreferencesDto, ApplicationErrorCode, CustomCategoryDto, DaemonConfigSnapshot,
        RpcErrorData, method,
    };
    use serde_json::{Value, json};

    use super::test_support::MockRpc;
    use super::{ToolError, create_tasks_with, manage_tasks_with};

    fn queue(id: &str, name: &str) -> Value {
        json!({
            "queueId": id, "name": name, "speedLimitKbps": 0, "maxConcurrent": 0,
            "defaultSaveDir": "", "position": 0, "defaultSegments": 0, "defaultUserAgent": "",
        })
    }

    fn task(id: &str, name: &str, status: i32) -> Value {
        json!({
            "taskId": id, "url": "https://example.com/f", "fileName": name, "saveDir": "/d",
            "status": status, "downloadedBytes": 0, "totalBytes": 0, "createdAt": "1000",
            "queueId": "q-main",
        })
    }

    fn all_caps() -> Value {
        json!({
            "request": true, "proxy": true, "storage": true,
            "rename": true, "segments": true, "schedule": true,
        })
    }

    fn snapshot(id: &str, caps: &Value) -> Value {
        json!({
            "taskId": id, "url": "https://example.com/f", "revision": "rev1", "saveDir": "/d",
            "fileName": "f", "proxyUrl": "", "segments": 0, "properties": {}, "headers": {},
            "cookiesConfigured": false, "capabilities": caps,
        })
    }

    /// 默认应答的 daemon 替身：`daemon.task.get` 查给定任务，建任务逐条成功。
    fn mock_rpc(tasks: Vec<Value>) -> MockRpc {
        rpc_with_caps(tasks, all_caps())
    }

    fn rpc_with_caps(tasks: Vec<Value>, caps: Value) -> MockRpc {
        MockRpc::new(move |name, params| match name {
            method::DAEMON_TASK_GET => tasks
                .iter()
                .find(|task| task["taskId"] == params["taskId"])
                .cloned()
                .ok_or_else(|| ToolError::Rpc("not_found: task not found".to_owned())),
            method::DAEMON_QUEUE_LIST => {
                Ok(json!([queue("q-main", "Main"), queue("q-night", "Night")]))
            }
            method::DAEMON_TASK_EDIT_INFO => Ok(snapshot(
                params["taskId"].as_str().unwrap_or_default(),
                &caps,
            )),
            method::DAEMON_TASK_CREATE_MANY => {
                let count = params["items"].as_array().map_or(0, Vec::len);
                let results = (0..count)
                    .map(|index| json!({ "taskId": format!("new-{index}") }))
                    .collect::<Vec<_>>();
                Ok(json!({ "results": results }))
            }
            _ => Ok(json!({})),
        })
    }

    fn assert_invalid(result: &Result<Value, ToolError>) {
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
    }

    fn create_params(rpc: &MockRpc) -> Value {
        let calls = rpc.calls_of(method::DAEMON_TASK_CREATE_MANY);
        assert_eq!(calls.len(), 1, "createMany must be called exactly once");
        calls[0].clone()
    }

    // ───────────── fd_create_tasks ─────────────

    #[tokio::test]
    async fn create_rejects_unknown_fields() {
        for args in [
            json!({ "items": [{ "url": "https://a.example/x" }], "extra": 1 }),
            json!({ "items": [{ "url": "https://a.example/x", "mirror": "y" }] }),
            json!({ "items": [{ "url": "https://a.example/x" }], "options": { "password": "p" } }),
            json!({ "items": [{ "url": "https://a.example/x" }], "options": { "speedLimitKbps": 5 } }),
        ] {
            let rpc = mock_rpc(vec![]);
            let result = create_tasks_with(&rpc, args.clone()).await;
            assert_invalid(&result);
            assert!(
                rpc.calls
                    .lock()
                    .map(|calls| calls.is_empty())
                    .unwrap_or(false),
                "{args}"
            );
        }
    }

    #[tokio::test]
    async fn create_rejects_schema_violations() {
        let item = json!({ "url": "https://a.example/x" });
        let too_many = vec![item.clone(); 51];
        let long_url = format!("https://a.example/{}", "x".repeat(8200));
        let headers_33 = (0..33)
            .map(|index| (format!("X-H{index}"), json!("v")))
            .collect::<serde_json::Map<_, _>>();
        for args in [
            json!({ "items": [] }),
            json!({ "items": too_many }),
            json!({ "items": [{ "url": "" }] }),
            json!({ "items": [{ "url": long_url }] }),
            json!({ "items": [{ "url": "https://a.example/x", "fileName": "n".repeat(256) }] }),
            json!({ "items": [{ "url": "https://a.example/x", "checksum": "c".repeat(201) }] }),
            json!({ "items": [item.clone()], "options": { "connections": 257 } }),
            json!({ "items": [item.clone()], "options": { "connections": -1 } }),
            json!({ "items": [item.clone()], "options": { "connections": "4" } }),
            json!({ "items": [item.clone()], "options": { "speedLimitKBps": -5 } }),
            json!({ "items": [item.clone()], "options": { "ignoreTlsErrors": "yes" } }),
            json!({ "items": [item.clone()], "options": { "startPaused": 1 } }),
            json!({ "items": [item.clone()], "options": { "tags": vec!["t"; 21] } }),
            json!({ "items": [item.clone()], "options": { "tags": ["x".repeat(65)] } }),
            json!({ "items": [item.clone()], "options": { "headers": headers_33 } }),
            json!({ "items": [item.clone()], "options": { "headers": { "X-A": 1 } } }),
            json!({ "items": [item.clone()], "options": { "headers": { "Bad Name": "v" } } }),
            json!({ "items": [item.clone()], "options": { "headers": { "X-A": "a\r\nX-B: b" } } }),
            json!({ "items": [item.clone()], "options": { "saveDir": "relative/dir" } }),
            json!({ "items": [item.clone()], "options": { "userAgent": "ua\nx" } }),
            json!({ "items": [item.clone()], "options": { "queue": "q".repeat(129) } }),
            json!({ "items": [item.clone()], "options": { "proxy": "http://user:pw@proxy.example:8080" } }),
            json!({ "items": [item.clone()], "options": { "proxy": "ftp://proxy.example:21" } }),
            json!({ "items": [item.clone()], "options": { "proxy": "http://proxy.example" } }),
            json!({ "items": [item.clone()], "options": { "proxy": 3 } }),
            json!({ "items": "https://a.example/x" }),
            json!(["https://a.example/x"]),
        ] {
            let rpc = mock_rpc(vec![]);
            let result = create_tasks_with(&rpc, args.clone()).await;
            assert_invalid(&result);
            assert!(
                rpc.calls_of(method::DAEMON_TASK_CREATE_MANY).is_empty(),
                "no task may be created for {args}"
            );
        }
    }

    #[tokio::test]
    async fn create_maps_proxy_follow_direct_and_specific() {
        for (given, expected) in [
            (None, ""),
            (Some(""), ""),
            (Some("direct"), "direct://"),
            (Some("DIRECT"), "direct://"),
            (Some("system"), "system://"),
            (
                Some("http://proxy.example:8080"),
                "http://proxy.example:8080",
            ),
            (Some("SOCKS5://127.0.0.1:1080"), "socks5://127.0.0.1:1080"),
            (Some("socks5h://[::1]:1080"), "socks5h://[::1]:1080"),
        ] {
            let rpc = mock_rpc(vec![]);
            let options = given.map_or_else(|| json!({}), |proxy| json!({ "proxy": proxy }));
            let result = create_tasks_with(
                &rpc,
                json!({ "items": [{ "url": "https://a.example/x" }], "options": options }),
            )
            .await;
            assert!(result.is_ok(), "{given:?}: {result:?}");
            assert_eq!(
                create_params(&rpc)["common"]["proxyUrl"],
                json!(expected),
                "{given:?}"
            );
        }
    }

    #[tokio::test]
    async fn create_maps_options_cookie_and_headers() {
        let rpc = mock_rpc(vec![]);
        let result = create_tasks_with(
            &rpc,
            json!({
                "items": [
                    { "url": "https://a.example/x.zip", "fileName": "sub/x.zip", "checksum": "sha256=abc" },
                    { "url": "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567" },
                ],
                "options": {
                    "saveDir": " /data/dl ",
                    "queue": "night",
                    "connections": 8,
                    "userAgent": "UA/1.0",
                    "referrer": "https://a.example/",
                    "headers": { "Cookie": "sid=1; theme=dark", "X-Token": "t", "accept": "*/*" },
                    "ignoreTlsErrors": true,
                    "startPaused": true,
                },
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));

        let params = create_params(&rpc);
        let batch = params["batchId"].as_str().unwrap_or_default();
        assert!(
            !batch.is_empty() && batch.len() <= fluxdown_protocol::CREATE_TASKS_BATCH_ID_MAX_LEN
        );
        let common = &params["common"];
        assert_eq!(common["url"], json!(""));
        assert_eq!(common["fileName"], json!(""));
        assert_eq!(common["checksum"], json!(""));
        assert_eq!(common["saveDir"], json!("/data/dl"));
        assert_eq!(
            common["queueId"],
            json!("q-night"),
            "queue name resolves to its id"
        );
        assert_eq!(common["segments"], json!(8));
        assert_eq!(common["userAgent"], json!("UA/1.0"));
        assert_eq!(common["referrer"], json!("https://a.example/"));
        assert_eq!(common["cookies"], json!("sid=1; theme=dark"));
        assert_eq!(
            common["headers"],
            json!({ "X-Token": "t", "accept": "*/*" })
        );
        assert_eq!(common["ignoreTlsErrors"], json!(true));
        assert_eq!(common["startPaused"], json!(true));
        assert_eq!(params["items"][0]["url"], json!("https://a.example/x.zip"));
        assert_eq!(params["items"][0]["fileName"], json!("sub/x.zip"));
        assert_eq!(params["items"][0]["checksum"], json!("sha256=abc"));
        assert_eq!(params["items"].as_array().map(Vec::len), Some(2));
        assert!(
            params.get("categoryRoute").is_none(),
            "an explicit save dir is the final dir"
        );
        assert_eq!(result["created"], json!(2));
        assert_eq!(result["failed"], json!(0));
        assert_eq!(result["results"][1]["taskId"], json!("new-1"));
        assert!(
            rpc.calls_of(method::DAEMON_TASK_EDIT).is_empty(),
            "no per-task options to write"
        );
    }

    #[tokio::test]
    async fn create_without_options_uses_category_route() {
        let rpc = mock_rpc(vec![]);
        let result =
            create_tasks_with(&rpc, json!({ "items": [{ "url": "https://a.example/x" }] })).await;
        assert!(result.is_ok(), "{result:?}");
        let params = create_params(&rpc);
        assert_eq!(params["common"]["saveDir"], json!(""));
        assert!(
            params.get("categoryRoute").is_some(),
            "app defaults route by file type"
        );
        assert_eq!(params["common"]["proxyUrl"], json!(""));
        assert_eq!(params["common"]["segments"], json!(0));
        assert_eq!(params["common"]["ignoreTlsErrors"], json!(false));
        assert_eq!(params["common"]["startPaused"], json!(false));
    }

    #[tokio::test]
    async fn create_reports_each_item_and_skips_invalid_links() {
        let rpc = MockRpc::new(|name, params| {
            if name == method::DAEMON_TASK_CREATE_MANY {
                assert_eq!(params["items"].as_array().map(Vec::len), Some(2));
                return Ok(json!({ "results": [
                    { "taskId": "new-a" },
                    { "error": RpcErrorData::new(ApplicationErrorCode::Conflict, false) },
                ]}));
            }
            Ok(json!({}))
        });
        let result = create_tasks_with(
            &rpc,
            json!({ "items": [
                { "url": "https://a.example/ok" },
                { "url": "file:///etc/passwd" },
                { "url": "https://a.example/dup" },
                { "url": "https://a.example/y", "fileName": "../escape" },
                { "url": "not a url" },
                { "url": "https://a.example/z", "checksum": "nohash" },
            ]}),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["requested"], json!(6));
        assert_eq!(result["created"], json!(1));
        assert_eq!(result["failed"], json!(5));
        let results = result["results"].as_array().cloned().unwrap_or_default();
        assert_eq!(results.len(), 6);
        for (index, item) in results.iter().enumerate() {
            assert_eq!(item["index"], json!(index));
        }
        assert_eq!(results[0]["status"], json!("created"));
        assert_eq!(results[0]["taskId"], json!("new-a"));
        assert_eq!(results[1]["status"], json!("error"));
        assert_eq!(results[2]["status"], json!("error"));
        assert!(
            results[2]["error"].is_object(),
            "daemon error is passed through"
        );
        for index in [3, 4, 5] {
            assert_eq!(results[index]["status"], json!("error"), "item {index}");
        }
    }

    #[tokio::test]
    async fn create_with_only_invalid_links_makes_no_rpc_call() {
        let rpc = mock_rpc(vec![]);
        let result =
            create_tasks_with(&rpc, json!({ "items": [{ "url": "javascript:alert(1)" }] }))
                .await
                .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["created"], json!(0));
        assert!(
            rpc.calls
                .lock()
                .map(|calls| calls.is_empty())
                .unwrap_or(false)
        );
    }

    #[tokio::test]
    async fn create_writes_limit_tags_and_category_through_task_edit() {
        let video = CustomCategoryDto::from_preference(None)
            .into_iter()
            .find(|category| category.builtin_type.as_deref() == Some("video"))
            .unwrap_or_else(|| panic!("builtin video category"));
        let rpc = mock_rpc(vec![]);
        let result = create_tasks_with(
            &rpc,
            json!({
                "items": [{ "url": "https://a.example/v.mp4" }],
                "options": {
                    "speedLimitKBps": 512,
                    "tags": [" work ", "movie"],
                    "category": "Video",
                    "saveDir": "/data/dl",
                },
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["created"], json!(1));
        assert!(result["results"][0].get("warning").is_none(), "{result}");

        let edits = rpc.calls_of(method::DAEMON_TASK_EDIT);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["taskId"], json!("new-0"));
        assert_eq!(edits[0]["expectedRevision"], json!("rev1"));
        assert_eq!(edits[0]["downloadLimitBps"], json!(512 * 1024));
        assert_eq!(edits[0]["tags"], json!(["work", "movie"]));
        assert_eq!(edits[0]["category"], json!(video.id));
        for untouched in [
            "proxyUrl",
            "segments",
            "userAgent",
            "cookies",
            "headers",
            "saveDir",
            "fileName",
        ] {
            assert!(
                edits[0][untouched].is_null(),
                "{untouched} must stay unchanged"
            );
        }
    }

    #[tokio::test]
    async fn create_zero_speed_limit_needs_no_edit() {
        let rpc = mock_rpc(vec![]);
        let result = create_tasks_with(
            &rpc,
            json!({ "items": [{ "url": "https://a.example/x" }], "options": { "speedLimitKBps": 0 } }),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert!(rpc.calls_of(method::DAEMON_TASK_EDIT).is_empty());
    }

    #[tokio::test]
    async fn create_category_supplies_its_save_dir_and_unknown_category_is_rejected() {
        let mut rpc = mock_rpc(vec![]);
        rpc.prefs = AgentPreferencesDto {
            revision: 1,
            values: [(
                "custom_categories".to_owned(),
                json!([{
                    "id": "c-iso", "name": "ISOs", "matchMode": "extension", "extensions": ["iso"],
                    "position": 1, "visible": true, "isBuiltin": false, "saveDir": "/isos",
                }]),
            )]
            .into_iter()
            .collect(),
        };
        let result = create_tasks_with(
            &rpc,
            json!({ "items": [{ "url": "https://a.example/x" }], "options": { "category": "isos" } }),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(create_params(&rpc)["common"]["saveDir"], json!("/isos"));
        let edits = rpc.calls_of(method::DAEMON_TASK_EDIT);
        assert_eq!(edits[0]["category"], json!("c-iso"));

        let rpc = mock_rpc(vec![]);
        let result = create_tasks_with(
            &rpc,
            json!({ "items": [{ "url": "https://a.example/x" }], "options": { "category": "nope" } }),
        )
        .await;
        assert_invalid(&result);
        assert!(rpc.calls_of(method::DAEMON_TASK_CREATE_MANY).is_empty());
    }

    #[tokio::test]
    async fn create_unknown_queue_is_rejected_with_choices() {
        let rpc = mock_rpc(vec![]);
        let result = create_tasks_with(
            &rpc,
            json!({ "items": [{ "url": "https://a.example/x" }], "options": { "queue": "weekend" } }),
        )
        .await;
        match result {
            Err(ToolError::InvalidArguments(message)) => {
                assert!(
                    message.contains("Night") && message.contains("q-main"),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(rpc.calls_of(method::DAEMON_TASK_CREATE_MANY).is_empty());
    }

    #[tokio::test]
    async fn create_does_not_touch_tasks_returned_by_url_dedup() {
        let mut rpc = mock_rpc(vec![task("new-0", "old.bin", 1)]);
        rpc.config = DaemonConfigSnapshot {
            revision: 1,
            values: [("dedup_same_url".to_owned(), "true".to_owned())]
                .into_iter()
                .collect(),
        };
        let result = create_tasks_with(
            &rpc,
            json!({
                "items": [{ "url": "https://a.example/x" }],
                "options": { "speedLimitKBps": 64 },
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["created"], json!(1));
        assert!(result["results"][0]["warning"].is_string(), "{result}");
        assert!(rpc.calls_of(method::DAEMON_TASK_EDIT).is_empty());
    }

    // ───────────── fd_manage_tasks ─────────────

    #[tokio::test]
    async fn manage_rejects_unknown_fields_and_bad_combinations() {
        let ids = json!(["t1"]);
        let ids_201 = (0..201)
            .map(|index| format!("t{index}"))
            .collect::<Vec<_>>();
        for args in [
            json!({ "taskIds": ids, "action": "pause", "force": true }),
            json!({ "taskIds": ids, "action": "explode" }),
            json!({ "taskIds": ids, "action": "Delete" }),
            json!({ "taskIds": [], "action": "pause" }),
            json!({ "taskIds": ids_201, "action": "pause" }),
            json!({ "taskIds": [""], "action": "pause" }),
            json!({ "taskIds": ["x".repeat(129)], "action": "pause" }),
            json!({ "taskIds": [1], "action": "pause" }),
            json!({ "taskIds": "t1", "action": "pause" }),
            json!({ "action": "pause" }),
            json!({ "taskIds": ids }),
            json!({ "taskIds": ids, "action": "pause", "deleteFiles": true }),
            json!({ "taskIds": ids, "action": "delete", "deleteFiles": "yes" }),
            json!({ "taskIds": ids, "action": "pause", "queue": "Main" }),
            json!({ "taskIds": ids, "action": "moveToQueue" }),
            json!({ "taskIds": ids, "action": "moveToQueue", "queue": "" }),
            json!({ "taskIds": ids, "action": "pause", "edit": { "tags": [] } }),
            json!({ "taskIds": ids, "action": "edit" }),
            json!({ "taskIds": ids, "action": "edit", "edit": {} }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "secret": "x" } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "connections": 300 } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "connections": "8" } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "speedLimitKBps": -1 } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "proxy": "http://u:p@h:1" } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "proxy": 1 } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "headers": { "X": 1 } } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "saveDir": "rel" } }),
            json!({ "taskIds": ids, "action": "edit", "edit": { "fileName": "a/b" } }),
            json!({ "taskIds": ["t1", "t2"], "action": "edit", "edit": { "fileName": "a.bin" } }),
        ] {
            let rpc = mock_rpc(vec![task("t1", "a", 1), task("t2", "b", 1)]);
            let result = manage_tasks_with(&rpc, args.clone()).await;
            assert_invalid(&result);
            assert!(
                rpc.calls
                    .lock()
                    .map(|calls| calls.is_empty())
                    .unwrap_or(false),
                "{args}"
            );
        }
    }

    #[tokio::test]
    async fn delete_without_delete_files_keeps_files_and_lists_tasks() {
        let rpc = mock_rpc(vec![task("t1", "a.iso", 3), task("t2", "b.iso", 2)]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1", "t2", "t1", "missing"], "action": "delete" }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));

        let calls = rpc.calls_of(method::DAEMON_TASK_DELETE_MANY);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]["taskIds"],
            json!(["t1", "t2"]),
            "duplicates and unknown ids are dropped"
        );
        assert_eq!(
            calls[0]["deleteFiles"],
            json!(false),
            "files are kept unless explicitly requested"
        );
        assert_eq!(result["filesDeleted"], json!(false));
        assert_eq!(result["requested"], json!(3));
        assert_eq!(
            result["applied"],
            json!([
                { "taskId": "t1", "name": "a.iso" },
                { "taskId": "t2", "name": "b.iso" },
            ])
        );
        assert_eq!(result["skipped"][0]["taskId"], json!("missing"));
        assert_eq!(result["skipped"][0]["reason"], json!("task not found"));
    }

    #[tokio::test]
    async fn delete_with_explicit_delete_files_removes_files() {
        let rpc = mock_rpc(vec![task("t1", "a.iso", 3)]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1"], "action": "delete", "deleteFiles": true }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            rpc.calls_of(method::DAEMON_TASK_DELETE_MANY)[0]["deleteFiles"],
            json!(true)
        );
        assert_eq!(result["filesDeleted"], json!(true));

        let rpc = rpc_with_caps(vec![task("t1", "a.iso", 3)], all_caps());
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1"], "action": "delete", "deleteFiles": false }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            rpc.calls_of(method::DAEMON_TASK_DELETE_MANY)[0]["deleteFiles"],
            json!(false)
        );
        assert_eq!(result["filesDeleted"], json!(false));
    }

    #[tokio::test]
    async fn delete_of_only_unknown_tasks_calls_nothing() {
        let rpc = mock_rpc(vec![]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["ghost"], "action": "delete", "deleteFiles": true }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(rpc.calls_of(method::DAEMON_TASK_DELETE_MANY).is_empty());
        assert_eq!(result["applied"], json!([]));
        assert_eq!(result["skipped"][0]["reason"], json!("task not found"));
    }

    #[tokio::test]
    async fn rpc_failure_while_reading_a_task_fails_the_call() {
        let rpc =
            MockRpc::new(|_, _| Err(ToolError::Rpc("unavailable: daemon is down".to_owned())));
        let result = manage_tasks_with(&rpc, json!({ "taskIds": ["t1"], "action": "pause" })).await;
        assert!(matches!(result, Err(ToolError::Rpc(_))), "{result:?}");
    }

    #[tokio::test]
    async fn pause_and_resume_only_touch_applicable_tasks() {
        let mut seeding = task("seed", "s.iso", 3);
        seeding["seedingStatus"] = json!(1);
        let rpc = mock_rpc(vec![
            task("run", "r", 1),
            task("queued", "q", 0),
            task("prep", "p", 5),
            task("paused", "pa", 2),
            task("failed", "f", 4),
            task("done", "d", 3),
            seeding,
        ]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["run", "queued", "prep", "paused", "failed", "done", "seed"], "action": "pause" }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        let calls = rpc.calls_of(method::DAEMON_TASK_PAUSE_MANY);
        assert_eq!(
            calls[0]["taskIds"],
            json!(["run", "queued", "prep", "seed"])
        );
        let skipped = result["skipped"].as_array().cloned().unwrap_or_default();
        let skipped_ids = skipped
            .iter()
            .map(|item| item["taskId"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            skipped_ids,
            vec![json!("paused"), json!("failed"), json!("done")]
        );
        assert!(skipped.iter().all(|item| item["reason"].is_string()));

        let rpc = mock_rpc(vec![
            task("run", "r", 1),
            task("paused", "pa", 2),
            task("failed", "f", 4),
            task("done", "d", 3),
        ]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["run", "paused", "failed", "done"], "action": "resume" }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        let calls = rpc.calls_of(method::DAEMON_TASK_RESUME_MANY);
        assert_eq!(
            calls[0]["taskIds"],
            json!(["paused", "failed"]),
            "resume also retries failed tasks"
        );
        assert_eq!(result["skipped"].as_array().map(Vec::len), Some(2));
        assert!(rpc.calls_of(method::DAEMON_TASK_PAUSE_MANY).is_empty());
    }

    #[tokio::test]
    async fn move_to_queue_resolves_names_and_skips_inapplicable_tasks() {
        let rpc = mock_rpc(vec![task("a", "a", 1), task("done", "d", 3)]);
        let mut in_night = task("b", "b", 2);
        in_night["queueId"] = json!("q-night");
        let rpc2 = rpc_with_caps(
            vec![task("a", "a", 1), task("done", "d", 3), in_night],
            all_caps(),
        );
        drop(rpc);
        let result = manage_tasks_with(
            &rpc2,
            json!({ "taskIds": ["a", "done", "b"], "action": "moveToQueue", "queue": "Night" }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        let moves = rpc2.calls_of(method::DAEMON_QUEUE_MOVE_TASK);
        assert_eq!(moves, vec![json!({ "taskId": "a", "queueId": "q-night" })]);
        assert_eq!(result["applied"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["skipped"].as_array().map(Vec::len), Some(2));

        let rpc3 = mock_rpc(vec![task("a", "a", 1)]);
        let result = manage_tasks_with(
            &rpc3,
            json!({ "taskIds": ["a"], "action": "moveToQueue", "queue": "q-main" }),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        let result = manage_tasks_with(
            &rpc3,
            json!({ "taskIds": ["a"], "action": "moveToQueue", "queue": "nowhere" }),
        )
        .await;
        assert_invalid(&result);
    }

    #[tokio::test]
    async fn edit_maps_proxy_threads_limit_headers_and_cookie() {
        let rpc = mock_rpc(vec![task("t1", "a", 1), task("t2", "b", 2)]);
        let result = manage_tasks_with(
            &rpc,
            json!({
                "taskIds": ["t1", "t2"],
                "action": "edit",
                "edit": {
                    "proxy": "direct",
                    "connections": 16,
                    "speedLimitKBps": 100,
                    "userAgent": "UA/2",
                    "headers": { "Cookie": "a=b", "X-Key": "v" },
                    "tags": ["x"],
                },
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        let edits = rpc.calls_of(method::DAEMON_TASK_EDIT);
        assert_eq!(edits.len(), 2);
        for (edit, id) in edits.iter().zip(["t1", "t2"]) {
            assert_eq!(edit["taskId"], json!(id));
            assert_eq!(edit["proxyUrl"], json!("direct://"));
            assert_eq!(edit["segments"], json!(16));
            assert_eq!(edit["downloadLimitBps"], json!(100 * 1024));
            assert_eq!(edit["userAgent"], json!("UA/2"));
            assert_eq!(edit["headers"], json!({ "X-Key": "v" }));
            assert_eq!(edit["cookies"], json!("a=b"));
            assert_eq!(edit["tags"], json!(["x"]));
            assert!(edit["saveDir"].is_null() && edit["fileName"].is_null());
        }
        assert_eq!(result["applied"].as_array().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn edit_proxy_follow_global_is_an_explicit_empty_value() {
        let rpc = mock_rpc(vec![task("t1", "a", 1)]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1"], "action": "edit", "edit": { "proxy": "" } }),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        let edits = rpc.calls_of(method::DAEMON_TASK_EDIT);
        assert_eq!(
            edits[0]["proxyUrl"],
            json!(""),
            "empty = follow the global proxy mode"
        );

        let rpc = mock_rpc(vec![task("t1", "a", 1)]);
        manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1"], "action": "edit", "edit": { "proxy": "socks5://10.0.0.2:1080" } }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            rpc.calls_of(method::DAEMON_TASK_EDIT)[0]["proxyUrl"],
            json!("socks5://10.0.0.2:1080")
        );
    }

    #[tokio::test]
    async fn edit_category_none_clears_and_named_category_resolves() {
        let rpc = mock_rpc(vec![task("t1", "a", 1)]);
        manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1"], "action": "edit", "edit": { "category": "" } }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            rpc.calls_of(method::DAEMON_TASK_EDIT)[0]["category"],
            json!("")
        );

        let rpc = mock_rpc(vec![task("t1", "a", 1)]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1"], "action": "edit", "edit": { "category": "unknown-cat" } }),
        )
        .await;
        assert_invalid(&result);
        assert!(rpc.calls_of(method::DAEMON_TASK_EDIT).is_empty());
    }

    #[tokio::test]
    async fn edit_reports_unsupported_tasks_without_failing_the_call() {
        let caps = json!({
            "request": false, "proxy": false, "storage": true,
            "rename": true, "segments": false, "schedule": true,
        });
        let rpc = rpc_with_caps(vec![task("bt", "x", 1)], caps);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["bt"], "action": "edit", "edit": { "proxy": "direct", "connections": 4 } }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(rpc.calls_of(method::DAEMON_TASK_EDIT).is_empty());
        let reason = result["skipped"][0]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            reason.contains("proxy") && reason.contains("connections"),
            "{reason}"
        );
        assert_eq!(result["applied"], json!([]));
    }

    #[tokio::test]
    async fn edit_save_dir_and_file_name_only_for_unfinished_tasks() {
        let rpc = mock_rpc(vec![task("done", "d", 3), task("run", "r", 1)]);
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["done", "run"], "action": "edit", "edit": { "saveDir": "/new/dir" } }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        let edits = rpc.calls_of(method::DAEMON_TASK_EDIT);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["taskId"], json!("run"));
        assert_eq!(edits[0]["saveDir"], json!("/new/dir"));
        assert_eq!(result["skipped"][0]["taskId"], json!("done"));
    }

    #[tokio::test]
    async fn edit_retries_on_conflict_and_reports_partial_failure() {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&seen);
        let tasks = [task("t1", "a", 1), task("t2", "b", 1)];
        let caps = all_caps();
        let rpc = MockRpc::new(move |name, params| match name {
            method::DAEMON_TASK_GET => tasks
                .iter()
                .find(|task| task["taskId"] == params["taskId"])
                .cloned()
                .ok_or_else(|| ToolError::Rpc("not_found".to_owned())),
            method::DAEMON_TASK_EDIT_INFO => Ok(snapshot(
                params["taskId"].as_str().unwrap_or_default(),
                &caps,
            )),
            method::DAEMON_TASK_EDIT => {
                if params["taskId"] == "t2" {
                    return Err(ToolError::Rpc("invalid-argument: nope".to_owned()));
                }
                if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    return Err(ToolError::Rpc("conflict: edit-conflict".to_owned()));
                }
                Ok(json!({}))
            }
            _ => Ok(json!({})),
        });
        let result = manage_tasks_with(
            &rpc,
            json!({ "taskIds": ["t1", "t2"], "action": "edit", "edit": { "tags": ["k"] } }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["applied"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["applied"][0]["taskId"], json!("t1"));
        assert_eq!(result["failed"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["failed"][0]["taskId"], json!("t2"));
        assert_eq!(
            rpc.calls_of(method::DAEMON_TASK_EDIT_INFO).len(),
            3,
            "t1 is re-read after the conflict"
        );
    }

    #[tokio::test]
    async fn cancellation_is_propagated() {
        let rpc = MockRpc::new(|name, _| {
            if name == method::DAEMON_TASK_PAUSE_MANY {
                Err(ToolError::Cancelled)
            } else {
                Ok(json!({ "taskId": "t1", "fileName": "a", "status": 1 }))
            }
        });
        let result = manage_tasks_with(&rpc, json!({ "taskIds": ["t1"], "action": "pause" })).await;
        assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
    }
}
