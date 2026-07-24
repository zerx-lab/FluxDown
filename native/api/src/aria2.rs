//! aria2 兼容层纯函数映射层 —— GID 编解码、status 映射、选项翻译、
//! 响应字段拼装。全部函数无 I/O、无 `&dyn ApiHost` 依赖，可直接单测。
//!
//! `jsonrpc.rs` 的 dispatch 层只做「解析 params → 调本模块函数 → 调
//! `&dyn ApiHost` → 包装响应」的胶水工作，业务映射规则全部收敛于此。
//!
//! 依据：`local://aria2_compat_contract.md`（GID 方案/错误码/status 映射/
//! 选项映射契约）、`local://aria2_rpc_methods.md`（aria2 官方源码级字段与
//! 错误文案）、`local://aria2_input_options.md`（选项可写性/取值规则）。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use crate::service::{LiveSpeed, TaskEventKind};
use crate::types::{CreateTaskRequest, TaskDto};

// ---------------------------------------------------------------------------
// GID：task_id ↔ GID 编解码与反查
// ---------------------------------------------------------------------------

/// FluxDown `task_id`（UUID v4 字符串）→ aria2 兼容 GID：
/// 去除连字符、转小写后取前 16 个十六进制字符。
///
/// 无状态、重启安全：GID 可随时由 `task_id` 重新推导，不需要额外映射表。
pub(crate) fn task_id_to_gid(task_id: &str) -> String {
    task_id
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase()
        .chars()
        .take(16)
        .collect()
}

/// 按 GID（或其唯一前缀）在任务列表中反查任务（aria2 `GroupId::expandUnique`
/// 语义）：命中 0 个 → not found；命中 ≥2 个 → not unique。
///
/// 大小写不敏感；传入值本身也先去除连字符再匹配，因此完整 36 位 `task_id`
/// （管理 API 使用的原生 ID）同样可直接作为 `gid` 传入。空串、含非十六进制
/// 字符、或长度超过完整 GID（32 位无连字符 UUID）一律直接判定 not
/// found，不进入前缀匹配——避免畸形输入被当成「空前缀命中全表」。
pub(crate) fn resolve_gid<'a>(tasks: &'a [TaskDto], gid: &str) -> Result<&'a TaskDto, String> {
    let needle: String = gid
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    // 详见上方文档;这里额外说明:不做该检查的话,空前缀会命中全表第一条,
    // 或因长度必然唯一而误判 unique。
    let is_malformed =
        needle.is_empty() || needle.len() > 32 || !needle.chars().all(|c| c.is_ascii_hexdigit());
    if is_malformed {
        return Err(format!("GID {gid} is not found"));
    }
    let mut matches = tasks.iter().filter(|t| {
        let stripped: String = t.task_id.chars().filter(|c| *c != '-').collect();
        stripped.to_ascii_lowercase().starts_with(&needle)
    });
    match (matches.next(), matches.next()) {
        (None, _) => Err(format!("GID {gid} is not found")),
        (Some(_), Some(_)) => Err(format!("GID {gid} is not unique")),
        (Some(t), None) => Ok(t),
    }
}

// ---------------------------------------------------------------------------
// status 映射
// ---------------------------------------------------------------------------

/// FluxDown `TaskDto.status` → aria2 `status` 字符串。
///
/// `0=pending→waiting, 1=downloading→active, 2=paused→paused,
/// 3=completed→complete, 4=error→error, 5=preparing→active`。
pub(crate) fn aria2_status_str(status: i32) -> &'static str {
    match status {
        0 => "waiting",
        1 => "active",
        2 => "paused",
        3 => "complete",
        5 => "active",
        _ => "error",
    }
}

/// `tellActive` 归属判定：downloading(1) / preparing(5)。
pub(crate) fn is_active_status(status: i32) -> bool {
    matches!(status, 1 | 5)
}

/// `tellWaiting` 归属判定：pending(0) / paused(2)（aria2 语义里 paused
/// 任务仍在等待队列中，只是 `status` 字段单独显示为 `"paused"`）。
pub(crate) fn is_waiting_status(status: i32) -> bool {
    matches!(status, 0 | 2)
}

/// `tellStopped` 归属判定：completed(3) / error(4)。
pub(crate) fn is_stopped_status(status: i32) -> bool {
    matches!(status, 3 | 4)
}

// ---------------------------------------------------------------------------
// tellStatus / getFiles / getUris / getOption 响应字段拼装
// ---------------------------------------------------------------------------

/// 拼装单个任务的 aria2 status 对象（`tellStatus`/`tellActive`/`tellWaiting`/
/// `tellStopped` 共用）。
///
/// `connections`/`numPieces`/`pieceLength`/`uploadLength` 恒为 `"0"`——
/// FluxDown 引擎不是按 piece 调度、不做逐连接计数、HTTP 任务不统计上传
/// 字节，如实反映「不可用」而非伪造非零值。`errorCode`/`errorMessage`
/// 仅在已停止（complete/error）任务上输出，与 aria2 行为一致。
pub(crate) fn build_status_object(task: &TaskDto, speed: LiveSpeed) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "gid".to_string(),
        Value::String(task_id_to_gid(&task.task_id)),
    );
    obj.insert(
        "status".to_string(),
        Value::String(aria2_status_str(task.status).to_string()),
    );
    obj.insert(
        "totalLength".to_string(),
        Value::String(task.total_bytes.to_string()),
    );
    obj.insert(
        "completedLength".to_string(),
        Value::String(task.downloaded_bytes.to_string()),
    );
    obj.insert("uploadLength".to_string(), Value::String("0".to_string()));
    obj.insert(
        "downloadSpeed".to_string(),
        Value::String(speed.download_bps.max(0).to_string()),
    );
    obj.insert(
        "uploadSpeed".to_string(),
        Value::String(speed.upload_bps.max(0).to_string()),
    );
    obj.insert("connections".to_string(), Value::String("0".to_string()));
    obj.insert("numPieces".to_string(), Value::String("0".to_string()));
    obj.insert("pieceLength".to_string(), Value::String("0".to_string()));
    obj.insert("dir".to_string(), Value::String(task.save_dir.clone()));
    obj.insert(
        "files".to_string(),
        Value::Array(vec![build_file_entry(task)]),
    );
    if is_stopped_status(task.status) {
        let error_code = if task.status == 3 { "0" } else { "1" };
        obj.insert(
            "errorCode".to_string(),
            Value::String(error_code.to_string()),
        );
        obj.insert(
            "errorMessage".to_string(),
            Value::String(task.error_message.clone()),
        );
    }
    Value::Object(obj)
}

/// 单文件条目（`getFiles`/`tellStatus.files`）：FluxDown 任务恒单文件，
/// 因此恒返回一个 `index="1"`、`selected="true"` 的条目。
pub(crate) fn build_file_entry(task: &TaskDto) -> Value {
    let path = std::path::Path::new(&task.save_dir)
        .join(&task.file_name)
        .to_string_lossy()
        .into_owned();
    json!({
        "index": "1",
        "path": path,
        "length": task.total_bytes.to_string(),
        "completedLength": task.downloaded_bytes.to_string(),
        "selected": "true",
        "uris": build_uris_array(task),
    })
}

/// `getUris`/`files[].uris`：FluxDown 只跟踪单一 URL，非空时返回一个
/// `status="used"` 条目；`url` 为空（如种子任务）时返回空数组。
pub(crate) fn build_uris_array(task: &TaskDto) -> Value {
    if task.url.trim().is_empty() {
        Value::Array(vec![])
    } else {
        json!([{ "uri": task.url, "status": "used" }])
    }
}

/// `getOption`（单任务）：拼装 `dir`/`out`/`all-proxy`/`checksum`/
/// `check-certificate`，
/// 且仅在对应字段非空时才出现（对齐 aria2 `defined()` 语义：未设置的
/// 选项不以空字符串占位）。
pub(crate) fn build_get_option(task: &TaskDto) -> Value {
    let mut obj = Map::new();
    if !task.save_dir.is_empty() {
        obj.insert("dir".to_string(), Value::String(task.save_dir.clone()));
    }
    if !task.file_name.is_empty() {
        obj.insert("out".to_string(), Value::String(task.file_name.clone()));
    }
    if !task.proxy_url.is_empty() {
        obj.insert(
            "all-proxy".to_string(),
            Value::String(task.proxy_url.clone()),
        );
    }
    if !task.checksum.is_empty() {
        obj.insert("checksum".to_string(), Value::String(task.checksum.clone()));
    }
    if task.ignore_tls_errors {
        obj.insert(
            "check-certificate".to_string(),
            Value::String("false".to_string()),
        );
    }
    Value::Object(obj)
}

// ---------------------------------------------------------------------------
// keys 过滤（tellStatus/tellActive/tellWaiting/tellStopped 共用）
// ---------------------------------------------------------------------------

/// 从可选的 `keys` 参数（字符串数组）解析过滤白名单；缺省/非数组/空数组
/// 均返回空 `Vec`（约定为「不过滤」，见 [`filter_keys`]）。
pub(crate) fn parse_keys(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 按 `keys` 白名单过滤一个 JSON 对象；`keys` 为空表示不过滤（原样返回）。
/// 白名单中不存在于对象里的键被静默忽略（不报错，对齐 aria2）。
pub(crate) fn filter_keys(obj: Value, keys: &[String]) -> Value {
    if keys.is_empty() {
        return obj;
    }
    match obj {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| keys.iter().any(|kk| kk == k))
                .collect(),
        ),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// tellWaiting / tellStopped 分页
// ---------------------------------------------------------------------------

/// aria2 `tellWaiting`/`tellStopped` 共用的分页算法。
///
/// - `num <= 0` → 空结果。
/// - `offset >= 0`：从 `offset` 处正向取最多 `num` 个（越界截断），保持
///   原顺序。
/// - `offset < 0`：以「从末尾倒数 `abs(offset)` 处」为**结束位置**
///   （含，0-based），向容器开头方向最多取 `num` 个（越界截断到开头），
///   结果整体反转（新→旧排列）。例：`offset=-1, num=5` 取最近 5 条，
///   最新的排最前。`abs(offset)` 超出容器长度（结束位置早于起点）时
///   返回空。
pub(crate) fn paginate<T>(items: &[T], offset: i64, num: i64) -> Vec<&T> {
    if num <= 0 {
        return Vec::new();
    }
    let len = items.len() as i64;
    if offset >= 0 {
        if offset >= len {
            return Vec::new();
        }
        let start = offset as usize;
        let end = (offset + num).min(len) as usize;
        items[start..end].iter().collect()
    } else {
        let end = len + offset;
        if end < 0 {
            return Vec::new();
        }
        let end = end as usize;
        let start = end.saturating_sub((num - 1) as usize);
        let mut v: Vec<&T> = items[start..=end].iter().collect();
        v.reverse();
        v
    }
}

// ---------------------------------------------------------------------------
// addUri / addTorrent 的 options 字典解析
// ---------------------------------------------------------------------------

/// `addUri`/`addTorrent` 的 `options` 字典解析结果，直接对应
/// [`CreateTaskRequest`] 里由 aria2 选项映射而来的字段（`pause` 映射为
/// `start_paused`：建时即暂停）。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct RequestOptions {
    pub file_name: String,
    pub save_dir: String,
    pub referrer: String,
    pub cookies: String,
    pub headers: Option<HashMap<String, String>>,
    pub segments: i32,
    pub proxy_url: String,
    pub user_agent: String,
    pub checksum: String,
    pub ignore_tls_errors: bool,
    pub pause: bool,
}

/// 解析 `addUri`/`addTorrent` 的 `options` 字典。
///
/// 映射：`dir`→save_dir、`out`→file_name、`referer`→referrer、
/// `header`（字符串或字符串数组）→ Cookie/Referer/其余头、
/// `split`→segments、`all-proxy`|`http-proxy`|`https-proxy`→proxy_url
/// （优先 `all-proxy`）、`user-agent`→user_agent、`checksum`→checksum
/// （原样透传，格式已是 `algo=hex`）、`check-certificate=false`→忽略 TLS
/// 证书错误、`pause`→pause（一次性动作标记）。
/// 其余未知键静默忽略（对齐 aria2：不认识的选项不报错）。
///
/// `options.gid`（自定义 GID）显式拒绝——GID 由 `task_id` 派生，
/// 不支持客户端预留指定值。
pub(crate) fn parse_request_options(
    options: Option<&Map<String, Value>>,
) -> Result<RequestOptions, String> {
    let Some(opts) = options else {
        return Ok(RequestOptions::default());
    };
    if opts.contains_key("gid") {
        return Err("GID reservation is not supported".to_string());
    }

    let mut out = RequestOptions::default();
    if let Some(v) = opts.get("out").and_then(|v| v.as_str()) {
        out.file_name = v.to_string();
    }
    if let Some(v) = opts.get("dir").and_then(|v| v.as_str()) {
        out.save_dir = v.to_string();
    }
    if let Some(v) = opts.get("referer").and_then(|v| v.as_str()) {
        out.referrer = v.to_string();
    }
    if let Some(v) = opts
        .get("split")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
    {
        out.segments = v;
    }
    out.proxy_url = opts
        .get("all-proxy")
        .or_else(|| opts.get("http-proxy"))
        .or_else(|| opts.get("https-proxy"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(v) = opts.get("user-agent").and_then(|v| v.as_str()) {
        out.user_agent = v.to_string();
    }
    if let Some(v) = opts.get("checksum").and_then(|v| v.as_str()) {
        out.checksum = v.to_string();
    }
    out.ignore_tls_errors = option_is_false(opts.get("check-certificate"));
    out.pause = option_is_true(opts.get("pause"));

    let mut extra_headers: HashMap<String, String> = HashMap::new();
    for line in header_lines(opts.get("header")) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "cookie" => out.cookies = value.to_string(),
            "referer" => {
                if out.referrer.is_empty() {
                    out.referrer = value.to_string();
                }
            }
            _ => {
                extra_headers.insert(name.to_string(), value.to_string());
            }
        }
    }
    out.headers = if extra_headers.is_empty() {
        None
    } else {
        Some(extra_headers)
    };

    Ok(out)
}

/// aria2 布尔型选项值：既接受字符串 `"true"`（RPC 惯例），也接受原生
/// JSON 布尔（部分客户端会这么发）。
fn option_is_true(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

fn option_is_false(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => !*b,
        Some(Value::String(s)) => s == "false",
        _ => false,
    }
}

/// `header` 选项同时接受单个字符串与字符串数组（aria2 `Cumulative`
/// 选项语义）。
fn header_lines(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// 把 [`RequestOptions`] 与 `url`/`torrent_b64` 组装为 [`CreateTaskRequest`]。
/// `addUri` 传 `torrent_b64=None`；`addTorrent` 传 `url=String::new()`。
/// `opts.pause` → `start_paused`（aria2 语义：新建下载以暂停态入队）。
pub(crate) fn build_create_task_request(
    url: String,
    torrent_b64: Option<String>,
    opts: RequestOptions,
) -> CreateTaskRequest {
    CreateTaskRequest {
        url,
        file_name: opts.file_name,
        save_dir: opts.save_dir,
        segments: opts.segments,
        cookies: opts.cookies,
        referrer: opts.referrer,
        proxy_url: opts.proxy_url,
        user_agent: opts.user_agent,
        queue_id: String::new(),
        checksum: opts.checksum,
        ignore_tls_errors: opts.ignore_tls_errors,
        headers: opts.headers,
        torrent_b64,
        method: None,
        body: None,
        audio_url: None,
        start_paused: opts.pause,
    }
}

// ---------------------------------------------------------------------------
// 全局选项映射（getGlobalOption / changeGlobalOption）
// ---------------------------------------------------------------------------

/// 一条 aria2 全局选项 ↔ FluxDown config key 的映射规则。
struct GlobalOptionMapping {
    aria2_key: &'static str,
    config_key: &'static str,
    /// aria2 选项值 → 原生 config 值（`changeGlobalOption` 方向）。
    to_native: fn(&str) -> Result<String, String>,
    /// `get_config()` 未提供该键时的兜底值（`getGlobalOption` 方向，
    /// 对齐 aria2 对应选项的出厂默认值）。
    default_native: &'static str,
}

fn identity(s: &str) -> Result<String, String> {
    Ok(s.to_string())
}

fn to_native_bytes(s: &str) -> Result<String, String> {
    parse_aria2_unit_bytes(s).map(|n| n.to_string())
}

/// aria2 `Integer` 格式的整数选项值（`max-concurrent-downloads`/`split`）：
/// 必须能解析为合法 `i64`，否则视为非法值，由调用方
/// （[`map_change_global_options`]）整体拒绝该次 `changeGlobalOption`——
/// 非法值绝不能写进 config 表（否则会在后续 `getGlobalOption` 回显或引擎
/// 热应用时才暴露，定位更困难）。不做范围校验：aria2 的
/// `IntegerGE`/`IntegerRange` 语义因选项而异，这里只做「确实是个整数」这一
/// 层最基础的净化。
fn to_native_int(s: &str) -> Result<String, String> {
    s.trim()
        .parse::<i64>()
        .map(|n| n.to_string())
        .map_err(|_| format!("invalid integer: {s}"))
}

/// aria2 `Boolean` 格式的选项值：仅接受 `true`/`false`，其余视为非法值
/// 整体拒绝（同 [`to_native_int`] 的净化理由：非法值绝不写进 config 表）。
fn to_native_bool(s: &str) -> Result<String, String> {
    match s.trim() {
        "true" => Ok("true".to_string()),
        "false" => Ok("false".to_string()),
        _ => Err(format!("invalid boolean: {s}")),
    }
}

/// 契约表：`local://aria2_compat_contract.md` §「aria2 全局选项 ↔ FluxDown
/// config key 映射」。
const GLOBAL_OPTION_MAPPINGS: &[GlobalOptionMapping] = &[
    GlobalOptionMapping {
        aria2_key: "dir",
        config_key: "default_save_dir",
        to_native: identity,
        default_native: "",
    },
    GlobalOptionMapping {
        aria2_key: "max-concurrent-downloads",
        config_key: "max_concurrent_tasks",
        to_native: to_native_int,
        default_native: "5",
    },
    GlobalOptionMapping {
        aria2_key: "max-overall-download-limit",
        config_key: "speed_limit_bytes",
        to_native: to_native_bytes,
        default_native: "0",
    },
    GlobalOptionMapping {
        aria2_key: "split",
        config_key: "default_segments",
        to_native: to_native_int,
        default_native: "5",
    },
    GlobalOptionMapping {
        aria2_key: "user-agent",
        config_key: "global_user_agent",
        to_native: identity,
        default_native: "aria2/1.37.0",
    },
    GlobalOptionMapping {
        aria2_key: "remote-time",
        config_key: "use_server_time",
        to_native: to_native_bool,
        default_native: "false",
    },
];

/// AriaNg 等客户端常探测、但 FluxDown 无对应可写配置的选项：给出 aria2
/// 出厂默认值（纯静态展示，不可通过 `changeGlobalOption` 改变）。
const STATIC_GLOBAL_OPTION_DEFAULTS: &[(&str, &str)] = &[
    ("max-connection-per-server", "1"),
    ("min-split-size", "20M"),
    ("continue", "false"),
    ("max-download-limit", "0"),
    ("max-upload-limit", "0"),
    ("max-overall-upload-limit", "0"),
    ("timeout", "60"),
    ("connect-timeout", "60"),
    ("retry-wait", "0"),
    ("max-tries", "5"),
];

/// `changeGlobalOption`：把 aria2 `options` 字典按映射表翻译为
/// 原生 config key → value；映射表之外的键静默忽略。
/// 命中键但值非法（如 `max-overall-download-limit` 单位格式错误）时整体失败。
pub(crate) fn map_change_global_options(
    options: &Map<String, Value>,
) -> Result<HashMap<String, String>, String> {
    let mut changes = HashMap::new();
    for mapping in GLOBAL_OPTION_MAPPINGS {
        let Some(v) = options.get(mapping.aria2_key).and_then(|v| v.as_str()) else {
            continue;
        };
        let native = (mapping.to_native)(v)
            .map_err(|_| format!("The value {v} is invalid for --{}.", mapping.aria2_key))?;
        changes.insert(mapping.config_key.to_string(), native);
    }
    Ok(changes)
}

/// `getGlobalOption`：映射表各键的真实值（缺省时用 aria2 出厂默认兜底）
/// + 一组静态合理默认，全部字符串值。
pub(crate) fn build_global_option(config: &HashMap<String, String>) -> Value {
    let mut obj = Map::new();
    for mapping in GLOBAL_OPTION_MAPPINGS {
        let value = config
            .get(mapping.config_key)
            .cloned()
            .unwrap_or_else(|| mapping.default_native.to_string());
        obj.insert(mapping.aria2_key.to_string(), Value::String(value));
    }
    for (k, v) in STATIC_GLOBAL_OPTION_DEFAULTS {
        obj.insert((*k).to_string(), Value::String((*v).to_string()));
    }
    Value::Object(obj)
}

/// 解析 aria2 `UnitNumber` 格式的值：十进制数字，可选 `K`/`M`/`G`
/// 后缀（1024 进制，大小写不敏感）。`"0"` 表示不限（原样返回 0）。
pub(crate) fn parse_aria2_unit_bytes(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty unit number".to_string());
    }
    let (digits, mult): (&str, i64) = if let Some(d) = s.strip_suffix(['k', 'K']) {
        (d, 1024)
    } else if let Some(d) = s.strip_suffix(['m', 'M']) {
        (d, 1024 * 1024)
    } else if let Some(d) = s.strip_suffix(['g', 'G']) {
        (d, 1024 * 1024 * 1024)
    } else {
        (s, 1)
    };
    let n: i64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("invalid unit number: {s}"))?;
    Ok(n.saturating_mul(mult))
}

// ---------------------------------------------------------------------------
// 静态元数据：版本号 / 会话 ID / 方法与通知名单
// ---------------------------------------------------------------------------

/// `getVersion` 声明的 aria2 版本号。保留一个真实存在的 aria2 版本号
/// （而非本 crate 版本），避免客户端按版本号做的特性探测误判。
pub(crate) const ARIA2_VERSION: &str = "1.37.0";

/// `getVersion.enabledFeatures`：如实反映已支持的能力，移除
/// FluxDown 不支持的 `Metalink`/`XML-RPC`/`Firefox3 Cookie`。
pub(crate) const ENABLED_FEATURES: &[&str] =
    &["Async DNS", "BitTorrent", "GZip", "HTTPS", "Message Digest"];

/// `system.listMethods` 全名单，顺序对齐 aria2 官方 `RpcMethodFactory.cc`
/// 注册表顺序。**必须**与 `jsonrpc::dispatch_method` 的实际分支一一对应。
pub(crate) const METHOD_NAMES: &[&str] = &[
    "aria2.addUri",
    "aria2.addTorrent",
    "aria2.getPeers",
    "aria2.addMetalink",
    "aria2.remove",
    "aria2.pause",
    "aria2.forcePause",
    "aria2.pauseAll",
    "aria2.forcePauseAll",
    "aria2.unpause",
    "aria2.unpauseAll",
    "aria2.forceRemove",
    "aria2.changePosition",
    "aria2.tellStatus",
    "aria2.getUris",
    "aria2.getFiles",
    "aria2.getServers",
    "aria2.tellActive",
    "aria2.tellWaiting",
    "aria2.tellStopped",
    "aria2.getOption",
    "aria2.changeUri",
    "aria2.changeOption",
    "aria2.getGlobalOption",
    "aria2.changeGlobalOption",
    "aria2.purgeDownloadResult",
    "aria2.removeDownloadResult",
    "aria2.getVersion",
    "aria2.getSessionInfo",
    "aria2.shutdown",
    "aria2.forceShutdown",
    "aria2.getGlobalStat",
    "aria2.saveSession",
    "system.multicall",
    "system.listMethods",
    "system.listNotifications",
];

/// `system.listNotifications` 名单——真实 aria2 客户端也是先探测名单再决定是否
/// 订阅。顺序与 [`TaskEventKind`] 声明顺序一一对应（见下方
/// [`notification_method`]），两者若不同步会被单测捕获。
pub(crate) const NOTIFICATION_NAMES: &[&str] = &[
    "aria2.onDownloadStart",
    "aria2.onDownloadPause",
    "aria2.onDownloadStop",
    "aria2.onDownloadComplete",
    "aria2.onDownloadError",
    "aria2.onBtDownloadComplete",
];

// ---------------------------------------------------------------------------
// WebSocket 通知帧拼装（`/jsonrpc` WS 会话广播，见 `crate::jsonrpc_ws`）
// ---------------------------------------------------------------------------

/// [`TaskEventKind`] → aria2 通知方法名，一一对应（见该类型文档每个变体的
/// 映射说明）。
pub(crate) fn notification_method(kind: TaskEventKind) -> &'static str {
    match kind {
        TaskEventKind::Start => "aria2.onDownloadStart",
        TaskEventKind::Pause => "aria2.onDownloadPause",
        TaskEventKind::Stop => "aria2.onDownloadStop",
        TaskEventKind::Complete => "aria2.onDownloadComplete",
        TaskEventKind::Error => "aria2.onDownloadError",
        TaskEventKind::BtComplete => "aria2.onBtDownloadComplete",
    }
}

/// 拼装一条 WS 通知帧：
/// `{"jsonrpc":"2.0","method":"aria2.onDownloadStart","params":[{"gid":"..."}]}`。
///
/// **无 `id` 字段**（JSON-RPC 2.0 Notification 语义）；`params` 恒为长度 1 的
/// 数组，只含 `gid`，不携带 status/totalLength 等其它字段——客户端需自行调用
/// `aria2.tellStatus` 补全详情。逐字对齐真实 aria2
/// `WebSocketSessionMan::addNotification()` 的固定格式（见
/// `local://aria2_rpc_methods.md` §5）。
pub(crate) fn build_notification_frame(task_id: &str, kind: TaskEventKind) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": notification_method(kind),
        "params": [{ "gid": task_id_to_gid(task_id) }],
    })
}

static SESSION_ID: OnceLock<String> = OnceLock::new();

/// 进程级会话 ID（40 位小写十六进制），首次访问时生成一次并缓存。
///
/// aria2 `getSessionInfo.sessionId` 是 20 字节随机数的十六进制表示
/// （40 字符），与 GID（16 字符）长度、语义均不同——它标识「本次进程
/// 运行」而非单个任务。用两个 UUID v4 的 simple（无连字符）形式拼接
/// 取前 40 字符，复用现有 `uuid` 依赖，无需额外随机源。
pub(crate) fn session_id() -> &'static str {
    SESSION_ID.get_or_init(|| {
        let a = uuid::Uuid::new_v4().simple().to_string();
        let b = uuid::Uuid::new_v4().simple().to_string();
        format!("{a}{b}")[..40].to_string()
    })
}

// ---------------------------------------------------------------------------
// aria2 风格错误文案
// ---------------------------------------------------------------------------

/// `"The parameter at {index} is required but missing."`（aria2
/// `checkRequiredParam` 文案）。
pub(crate) fn err_missing_param(index: usize) -> String {
    format!("The parameter at {index} is required but missing.")
}

/// `"The parameter at {index} has wrong type."`（aria2 `checkParam` 文案）。
pub(crate) fn err_wrong_type_param(index: usize) -> String {
    format!("The parameter at {index} has wrong type.")
}

/// `"The integer parameter at {index} has invalid value: the value must
/// be greater than or equal to {min}."`（aria2 `IntegerGE` 文案）。
pub(crate) fn err_integer_ge(index: usize, min: i64) -> String {
    format!(
        "The integer parameter at {index} has invalid value: the value must be greater than or equal to {min}."
    )
}

/// 降级拒绝方法的统一文案：FluxDown 明确不支持该 aria2 能力。
pub(crate) fn err_unsupported(method: &str) -> String {
    format!("{method} is not supported by FluxDown.")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn task(task_id: &str, status: i32) -> TaskDto {
        TaskDto {
            task_id: task_id.to_string(),
            url: "https://example.com/f.zip".to_string(),
            file_name: "f.zip".to_string(),
            save_dir: "/downloads".to_string(),
            status,
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
        }
    }

    // -- GID -----------------------------------------------------------

    #[test]
    fn task_id_to_gid_strips_hyphens_lowercases_and_truncates_to_16() {
        let gid = task_id_to_gid("550E8400-e29b-41d4-a716-446655440000");
        assert_eq!(gid, "550e8400e29b41d4");
        assert_eq!(gid.len(), 16);
    }

    #[test]
    fn task_id_to_gid_handles_short_ids_without_panicking() {
        assert_eq!(task_id_to_gid("abc"), "abc");
        assert_eq!(task_id_to_gid(""), "");
    }

    #[test]
    fn resolve_gid_roundtrips_through_task_id_to_gid() {
        let tasks = vec![task("550e8400-e29b-41d4-a716-446655440000", 1)];
        let gid = task_id_to_gid(&tasks[0].task_id);
        let found = resolve_gid(&tasks, &gid).unwrap();
        assert_eq!(found.task_id, tasks[0].task_id);
    }

    #[test]
    fn resolve_gid_accepts_full_task_id_with_hyphens() {
        let tasks = vec![task("550e8400-e29b-41d4-a716-446655440000", 1)];
        let found = resolve_gid(&tasks, "550E8400-E29B-41D4-A716-446655440000").unwrap();
        assert_eq!(found.task_id, tasks[0].task_id);
    }

    #[test]
    fn resolve_gid_matches_unique_prefix() {
        let tasks = vec![task("550e8400-e29b-41d4-a716-446655440000", 1)];
        let found = resolve_gid(&tasks, "550e84").unwrap();
        assert_eq!(found.task_id, tasks[0].task_id);
    }

    #[test]
    fn resolve_gid_not_found_for_unknown_prefix() {
        let tasks = vec![task("550e8400-e29b-41d4-a716-446655440000", 1)];
        let err = resolve_gid(&tasks, "ffffffff").unwrap_err();
        assert_eq!(err, "GID ffffffff is not found");
    }

    #[test]
    fn resolve_gid_rejects_empty_gid_without_matching_any_task() {
        // 空 needle 不得被当成「匹配任意任务」的空前缀——否则会命中列表里
        // 第一个任务（或因长度必然唯一而误判 unique），而非真正的 not found。
        let tasks = vec![task("550e8400-e29b-41d4-a716-446655440000", 1)];
        let err = resolve_gid(&tasks, "").unwrap_err();
        assert_eq!(err, "GID  is not found");
    }

    #[test]
    fn resolve_gid_rejects_non_hex_characters() {
        let tasks = vec![task("550e8400-e29b-41d4-a716-446655440000", 1)];
        let err = resolve_gid(&tasks, "zz-not-hex").unwrap_err();
        assert_eq!(err, "GID zz-not-hex is not found");
    }

    #[test]
    fn resolve_gid_rejects_needle_longer_than_full_gid() {
        // 32 hex 字符是无连字符 UUID 的完整长度；再长必然不是合法 GID/task_id。
        let tasks = vec![task("550e8400-e29b-41d4-a716-446655440000", 1)];
        let overlong = "5".repeat(33);
        let err = resolve_gid(&tasks, &overlong).unwrap_err();
        assert_eq!(err, format!("GID {overlong} is not found"));
    }

    #[test]
    fn resolve_gid_not_unique_for_ambiguous_prefix() {
        let tasks = vec![
            task("aaaa1111-0000-0000-0000-000000000000", 1),
            task("aaaa2222-0000-0000-0000-000000000000", 1),
        ];
        let err = resolve_gid(&tasks, "aaaa").unwrap_err();
        assert_eq!(err, "GID aaaa is not unique");
    }

    // -- status 映射 -----------------------------------------------------

    #[test]
    fn aria2_status_str_matches_contract_table() {
        assert_eq!(aria2_status_str(0), "waiting");
        assert_eq!(aria2_status_str(1), "active");
        assert_eq!(aria2_status_str(2), "paused");
        assert_eq!(aria2_status_str(3), "complete");
        assert_eq!(aria2_status_str(4), "error");
        assert_eq!(aria2_status_str(5), "active");
    }

    #[test]
    fn status_bucket_predicates_partition_all_statuses() {
        for s in 0..=5 {
            let buckets = [
                is_active_status(s),
                is_waiting_status(s),
                is_stopped_status(s),
            ];
            assert_eq!(buckets.iter().filter(|b| **b).count(), 1, "status {s}");
        }
    }

    // -- tellStatus 字段拼装 ----------------------------------------------

    #[test]
    fn build_status_object_omits_error_fields_for_active_task() {
        let obj = build_status_object(
            &task("t1", 1),
            LiveSpeed {
                download_bps: 1024,
                upload_bps: 0,
            },
        );
        assert_eq!(obj["gid"], "t1");
        assert_eq!(obj["status"], "active");
        assert_eq!(obj["totalLength"], "100");
        assert_eq!(obj["completedLength"], "10");
        assert_eq!(obj["downloadSpeed"], "1024");
        assert_eq!(obj["dir"], "/downloads");
        assert!(obj.get("errorCode").is_none());
        assert!(obj.get("errorMessage").is_none());
    }

    #[test]
    fn build_status_object_includes_error_fields_for_stopped_task() {
        let mut t = task("t1", 4);
        t.error_message = "network problem".to_string();
        let obj = build_status_object(&t, LiveSpeed::default());
        assert_eq!(obj["status"], "error");
        assert_eq!(obj["errorCode"], "1");
        assert_eq!(obj["errorMessage"], "network problem");
    }

    #[test]
    fn build_status_object_completed_error_code_is_zero() {
        let obj = build_status_object(&task("t1", 3), LiveSpeed::default());
        assert_eq!(obj["errorCode"], "0");
    }

    #[test]
    fn build_file_entry_joins_dir_and_name() {
        let entry = build_file_entry(&task("t1", 1));
        assert_eq!(entry["index"], "1");
        assert_eq!(entry["selected"], "true");
        let path = entry["path"].as_str().unwrap();
        assert!(path.contains("f.zip"));
        assert_eq!(entry["uris"][0]["uri"], "https://example.com/f.zip");
        assert_eq!(entry["uris"][0]["status"], "used");
    }

    #[test]
    fn build_uris_array_empty_when_url_blank() {
        let mut t = task("t1", 1);
        t.url = String::new();
        assert_eq!(build_uris_array(&t), Value::Array(vec![]));
    }

    #[test]
    fn build_get_option_only_includes_nonempty_fields() {
        let t = task("t1", 1);
        let opt = build_get_option(&t);
        assert_eq!(opt["dir"], "/downloads");
        assert_eq!(opt["out"], "f.zip");
        assert!(opt.get("all-proxy").is_none());
        assert!(opt.get("checksum").is_none());
        assert!(opt.get("check-certificate").is_none());
    }

    #[test]
    fn build_get_option_exposes_explicit_insecure_tls_policy() {
        let mut t = task("t1", 1);
        t.ignore_tls_errors = true;
        let opt = build_get_option(&t);
        assert_eq!(opt["check-certificate"], "false");
    }

    // -- keys 过滤 ---------------------------------------------------------

    #[test]
    fn filter_keys_returns_everything_when_keys_empty() {
        let obj = json!({"a": 1, "b": 2});
        assert_eq!(filter_keys(obj.clone(), &[]), obj);
    }

    #[test]
    fn filter_keys_keeps_only_listed_keys_and_ignores_unknown() {
        let obj = json!({"a": 1, "b": 2, "c": 3});
        let filtered = filter_keys(obj, &["a".to_string(), "z".to_string()]);
        assert_eq!(filtered, json!({"a": 1}));
    }

    // -- 分页 ---------------------------------------------------------------

    #[test]
    fn paginate_positive_offset_slices_forward() {
        let items: Vec<i32> = (0..10).collect();
        let page = paginate(&items, 3, 2);
        assert_eq!(page, vec![&3, &4]);
    }

    #[test]
    fn paginate_num_zero_or_negative_is_empty() {
        let items: Vec<i32> = (0..10).collect();
        assert!(paginate(&items, 0, 0).is_empty());
        assert!(paginate(&items, 0, -1).is_empty());
    }

    #[test]
    fn paginate_offset_beyond_length_is_empty() {
        let items: Vec<i32> = (0..10).collect();
        assert!(paginate(&items, 20, 5).is_empty());
    }

    #[test]
    fn paginate_negative_offset_returns_most_recent_newest_first() {
        // 契约文档给出的例子：offset=-1, num=5 → 最近 5 条，最新排最前。
        let items: Vec<i32> = (0..10).collect();
        let page = paginate(&items, -1, 5);
        assert_eq!(page, vec![&9, &8, &7, &6, &5]);
    }

    #[test]
    fn paginate_negative_offset_not_anchored_at_end() {
        let items: Vec<i32> = (0..10).collect();
        let page = paginate(&items, -3, 2);
        assert_eq!(page, vec![&7, &6]);
    }

    #[test]
    fn paginate_negative_offset_beyond_start_is_empty() {
        let items: Vec<i32> = (0..10).collect();
        assert!(paginate(&items, -20, 5).is_empty());
    }

    // -- addUri/addTorrent options 解析 -------------------------------------

    #[test]
    fn parse_request_options_maps_all_documented_keys() {
        let options = json!({
            "dir": "D:/dl",
            "out": "file.bin",
            "referer": "https://ref.example/",
            "split": "8",
            "all-proxy": "http://proxy:8080",
            "http-proxy": "http://ignored:1",
            "user-agent": "UA/2.0",
            "checksum": "sha-256=deadbeef",
            "check-certificate": "false",
            "pause": "true",
            "header": ["Cookie: a=b", "X-Custom: v"],
        });
        let opts = parse_request_options(options.as_object()).unwrap();
        assert_eq!(opts.save_dir, "D:/dl");
        assert_eq!(opts.file_name, "file.bin");
        assert_eq!(opts.referrer, "https://ref.example/");
        assert_eq!(opts.segments, 8);
        assert_eq!(opts.proxy_url, "http://proxy:8080");
        assert_eq!(opts.user_agent, "UA/2.0");
        assert_eq!(opts.checksum, "sha-256=deadbeef");
        assert!(opts.ignore_tls_errors);
        assert!(opts.pause);
        assert_eq!(opts.cookies, "a=b");
        assert_eq!(opts.headers.unwrap().get("X-Custom").unwrap(), "v");
    }

    #[test]
    fn parse_request_options_proxy_priority_falls_back_when_all_proxy_absent() {
        let options = json!({ "https-proxy": "http://s:1" });
        let opts = parse_request_options(options.as_object()).unwrap();
        assert_eq!(opts.proxy_url, "http://s:1");
    }

    #[test]
    fn parse_request_options_header_string_form_is_accepted() {
        let options = json!({ "header": "Referer: https://h.example/" });
        let opts = parse_request_options(options.as_object()).unwrap();
        assert_eq!(opts.referrer, "https://h.example/");
    }

    #[test]
    fn parse_request_options_direct_referer_wins_over_header() {
        let options = json!({
            "referer": "https://direct.example/",
            "header": ["Referer: https://header.example/"],
        });
        let opts = parse_request_options(options.as_object()).unwrap();
        assert_eq!(opts.referrer, "https://direct.example/");
    }

    #[test]
    fn parse_request_options_rejects_custom_gid() {
        let options = json!({ "gid": "0123456789abcdef" });
        let err = parse_request_options(options.as_object()).unwrap_err();
        assert_eq!(err, "GID reservation is not supported");
    }

    #[test]
    fn parse_request_options_none_yields_defaults() {
        let opts = parse_request_options(None).unwrap();
        assert_eq!(opts, RequestOptions::default());
    }

    #[test]
    fn build_create_task_request_threads_torrent_b64_with_empty_url() {
        let opts = RequestOptions {
            file_name: "x.torrent".to_string(),
            ..Default::default()
        };
        let req = build_create_task_request(String::new(), Some("YmFzZTY0".to_string()), opts);
        assert_eq!(req.url, "");
        assert_eq!(req.torrent_b64.as_deref(), Some("YmFzZTY0"));
        assert_eq!(req.file_name, "x.torrent");
    }

    // -- 全局选项映射 ---------------------------------------------------------

    #[test]
    fn parse_aria2_unit_bytes_supports_k_m_g_suffixes_case_insensitive() {
        assert_eq!(parse_aria2_unit_bytes("0").unwrap(), 0);
        assert_eq!(parse_aria2_unit_bytes("1024").unwrap(), 1024);
        assert_eq!(parse_aria2_unit_bytes("1K").unwrap(), 1024);
        assert_eq!(parse_aria2_unit_bytes("2k").unwrap(), 2048);
        assert_eq!(parse_aria2_unit_bytes("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_aria2_unit_bytes("1m").unwrap(), 1024 * 1024);
        assert_eq!(parse_aria2_unit_bytes("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_aria2_unit_bytes_rejects_garbage() {
        assert!(parse_aria2_unit_bytes("abc").is_err());
        assert!(parse_aria2_unit_bytes("").is_err());
    }

    #[test]
    fn map_change_global_options_translates_known_keys_and_ignores_unknown() {
        let options = json!({
            "dir": "/data",
            "max-concurrent-downloads": "3",
            "max-overall-download-limit": "10M",
            "split": "16",
            "user-agent": "UA/9",
            "remote-time": "true",
            "totally-unknown-option": "ignored",
        });
        let changes = map_change_global_options(options.as_object().unwrap()).unwrap();
        assert_eq!(changes.get("default_save_dir").unwrap(), "/data");
        assert_eq!(changes.get("max_concurrent_tasks").unwrap(), "3");
        assert_eq!(
            changes.get("speed_limit_bytes").unwrap(),
            &(10 * 1024 * 1024).to_string()
        );
        assert_eq!(changes.get("default_segments").unwrap(), "16");
        assert_eq!(changes.get("global_user_agent").unwrap(), "UA/9");
        assert_eq!(changes.get("use_server_time").unwrap(), "true");
        assert_eq!(changes.len(), 6);
    }

    #[test]
    fn map_change_global_options_empty_when_no_known_keys() {
        let options = json!({ "unknown-a": "1", "unknown-b": "2" });
        let changes = map_change_global_options(options.as_object().unwrap()).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn map_change_global_options_errors_on_invalid_unit_number() {
        let options = json!({ "max-overall-download-limit": "not-a-number" });
        assert!(map_change_global_options(options.as_object().unwrap()).is_err());
    }

    #[test]
    fn map_change_global_options_errors_on_invalid_max_concurrent_downloads() {
        let options = json!({ "max-concurrent-downloads": "not-a-number" });
        assert!(map_change_global_options(options.as_object().unwrap()).is_err());
    }

    #[test]
    fn map_change_global_options_errors_on_invalid_split() {
        let options = json!({ "split": "lots" });
        assert!(map_change_global_options(options.as_object().unwrap()).is_err());
    }

    #[test]
    fn map_change_global_options_errors_on_invalid_remote_time() {
        let options = json!({ "remote-time": "yes" });
        assert!(map_change_global_options(options.as_object().unwrap()).is_err());
    }

    #[test]
    fn map_change_global_options_rejects_invalid_integer_without_partial_writes() {
        // 非法整数必须让整次调用失败,不能把它前面已解析成功的其它键悄悄
        // 落到返回值里——changeGlobalOption 是「整体成功或整体失败」。
        let options = json!({
            "dir": "/data",
            "max-concurrent-downloads": "not-a-number",
        });
        let err = map_change_global_options(options.as_object().unwrap()).unwrap_err();
        assert!(err.contains("max-concurrent-downloads"), "{err}");
    }

    #[test]
    fn build_global_option_uses_config_values_and_static_defaults() {
        let mut config = HashMap::new();
        config.insert("default_save_dir".to_string(), "/home/user/dl".to_string());
        config.insert("max_concurrent_tasks".to_string(), "10".to_string());
        let obj = build_global_option(&config);
        assert_eq!(obj["dir"], "/home/user/dl");
        assert_eq!(obj["max-concurrent-downloads"], "10");
        // 未提供的映射键回落到 aria2 出厂默认值。
        assert_eq!(obj["split"], "5");
        assert_eq!(obj["max-overall-download-limit"], "0");
        // 静态默认。
        assert_eq!(obj["max-connection-per-server"], "1");
        assert_eq!(obj["min-split-size"], "20M");
    }

    // -- 元数据 ---------------------------------------------------------------

    #[test]
    fn method_names_has_36_unique_entries() {
        assert_eq!(METHOD_NAMES.len(), 36);
        let unique: std::collections::HashSet<_> = METHOD_NAMES.iter().collect();
        assert_eq!(unique.len(), 36);
    }

    #[test]
    fn notification_names_has_6_entries() {
        assert_eq!(NOTIFICATION_NAMES.len(), 6);
    }

    #[test]
    fn session_id_is_40_lowercase_hex_chars_and_stable() {
        let a = session_id();
        let b = session_id();
        assert_eq!(a, b);
        assert_eq!(a.len(), 40);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    // -- WS 通知帧拼装 -----------------------------------------------------

    #[test]
    fn notification_method_covers_every_kind_in_notification_names_order() {
        let kinds = [
            TaskEventKind::Start,
            TaskEventKind::Pause,
            TaskEventKind::Stop,
            TaskEventKind::Complete,
            TaskEventKind::Error,
            TaskEventKind::BtComplete,
        ];
        let methods: Vec<&str> = kinds.iter().map(|k| notification_method(*k)).collect();
        assert_eq!(methods, NOTIFICATION_NAMES);
    }

    #[test]
    fn notification_method_maps_each_kind_to_its_aria2_name() {
        assert_eq!(
            notification_method(TaskEventKind::Start),
            "aria2.onDownloadStart"
        );
        assert_eq!(
            notification_method(TaskEventKind::Pause),
            "aria2.onDownloadPause"
        );
        assert_eq!(
            notification_method(TaskEventKind::Stop),
            "aria2.onDownloadStop"
        );
        assert_eq!(
            notification_method(TaskEventKind::Complete),
            "aria2.onDownloadComplete"
        );
        assert_eq!(
            notification_method(TaskEventKind::Error),
            "aria2.onDownloadError"
        );
        assert_eq!(
            notification_method(TaskEventKind::BtComplete),
            "aria2.onBtDownloadComplete"
        );
    }

    #[test]
    fn build_notification_frame_matches_aria2_wire_format_exactly() {
        let frame =
            build_notification_frame("550e8400-e29b-41d4-a716-446655440000", TaskEventKind::Start);
        assert_eq!(
            frame,
            json!({
                "jsonrpc": "2.0",
                "method": "aria2.onDownloadStart",
                "params": [{ "gid": "550e8400e29b41d4" }],
            })
        );
    }

    #[test]
    fn build_notification_frame_has_no_id_field() {
        let frame = build_notification_frame("t1", TaskEventKind::Complete);
        let obj = frame.as_object().unwrap();
        assert!(obj.get("id").is_none(), "aria2 通知帧不带 id 字段");
        assert_eq!(obj.len(), 3, "只应有 jsonrpc/method/params 三个键");
    }

    #[test]
    fn build_notification_frame_params_is_single_element_array_with_only_gid() {
        let task_id = "abcd1234-0000-0000-0000-000000000000";
        let frame = build_notification_frame(task_id, TaskEventKind::BtComplete);
        assert_eq!(frame["method"], "aria2.onBtDownloadComplete");
        let params = frame["params"].as_array().unwrap();
        assert_eq!(params.len(), 1);
        let entry = params[0].as_object().unwrap();
        assert_eq!(entry.len(), 1, "params[0] 只应有 gid 一个键");
        assert_eq!(entry["gid"], task_id_to_gid(task_id));
    }

    // -- 错误文案 ---------------------------------------------------------------

    #[test]
    fn error_messages_match_aria2_wording() {
        assert_eq!(
            err_missing_param(0),
            "The parameter at 0 is required but missing."
        );
        assert_eq!(
            err_wrong_type_param(1),
            "The parameter at 1 has wrong type."
        );
        assert_eq!(
            err_integer_ge(1, 0),
            "The integer parameter at 1 has invalid value: the value must be greater than or equal to 0."
        );
        assert_eq!(
            err_unsupported("aria2.shutdown"),
            "aria2.shutdown is not supported by FluxDown."
        );
    }
}
