//! 任务类只读工具:`fd_list_tasks`、`fd_get_task`、`fd_get_bt_status`。
//!
//! 任务表取自 agent 事件投影(与界面同源、无需再走 RPC);分段、活动时间线、速度历史、BT 实况走
//! daemon RPC。输出里任务地址只留必要信息(见 `output::display_url` / `output::origin_only`),保存
//! 路径把主目录换成 `~`,BT 对端 IP 不输出(只给汇总)。
//!
//! RPC 对应:
//! - `fd_list_tasks`:agent 事件投影(`system.snapshot` 同源任务表)+ 实时速率采样;
//! - `fd_get_task`:事件投影 + `daemon.task.activity` + `daemon.task.speedHistory`;
//! - `fd_get_bt_status`:`daemon.bt.portStatus` + `daemon.task.btStatus` + `daemon.config.get`。

use std::collections::{BTreeMap, HashMap};

use fluxdown_api::service::LiveSpeed;
use fluxdown_protocol::{
    BT_FILE_PRIORITY_HIGH, BT_FILE_PRIORITY_MAXIMUM, BT_FILE_PRIORITY_NORMAL,
    BT_FILE_PRIORITY_SKIP, BtDhtDto, BtFilesDto, BtPeerDto, BtPortStatusDto, BtStatusDto,
    BtTrackerDto, DaemonBtStatusParams, DaemonSnapshot, QueueDto, TaskActivityPage,
    TaskActivityQuery, TaskDto, TaskRuntimeDto, TaskSpeedHistoryDto, TaskSpeedHistoryQuery, method,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::output::{
    display_path, display_url, iso_millis, iso_secs, origin_only, percent, truncate_chars,
};
use super::read::{config_json, short_text, text_or_null};
use super::tasks::{check_max, invalid};
use super::{ToolContext, ToolError, parse_args, to_json};

const STATUS_PENDING: i32 = 0;
const STATUS_DOWNLOADING: i32 = 1;
const STATUS_PAUSED: i32 = 2;
const STATUS_COMPLETED: i32 = 3;
const STATUS_ERROR: i32 = 4;
const STATUS_PREPARING: i32 = 5;

/// `seedingStatus`:1 = 做种中,8 = 排队等做种位,2..=7 = 已停止(原因见 `seedingMessage`)。
const SEEDING_ACTIVE: i32 = 1;
const SEEDING_QUEUED: i32 = 8;

const DEFAULT_LIST_LIMIT: u32 = 30;
const MAX_LIST_LIMIT: u32 = 100;
const MAX_QUERY_CHARS: usize = 200;
const MAX_TASK_ID_CHARS: usize = 128;
const MAX_NAME_CHARS: usize = 200;
/// 任务详情里最近的活动事件条数。
const ACTIVITY_LIMIT: u32 = 25;
const MAX_ACTIVITY_MESSAGE_CHARS: usize = 300;
const MAX_SEGMENTS_SHOWN: usize = 32;
const MAX_SPEED_SAMPLES: usize = 30;
const MAX_TRACKERS: usize = 30;
const MAX_WEB_SEEDS: usize = 10;
const MAX_PEER_CLIENTS: usize = 8;
const MAX_CLIENT_NAME_CHARS: usize = 40;
const MAX_MAPPINGS: usize = 8;

// ───────────────────────── 任务通用 ─────────────────────────

fn is_seeding(task: &TaskDto) -> bool {
    task.status == STATUS_COMPLETED
        && matches!(task.seeding_status, SEEDING_ACTIVE | SEEDING_QUEUED)
}

/// 正在上传(做种中,不含排队等位)。
fn is_uploading(task: &TaskDto) -> bool {
    task.status == STATUS_COMPLETED && task.seeding_status == SEEDING_ACTIVE
}

fn status_label(task: &TaskDto) -> &'static str {
    match task.status {
        STATUS_PENDING => "queued",
        STATUS_DOWNLOADING => "downloading",
        STATUS_PAUSED => "paused",
        STATUS_COMPLETED if is_seeding(task) => "seeding",
        STATUS_COMPLETED => "completed",
        STATUS_ERROR => "failed",
        STATUS_PREPARING => "preparing",
        _ => "unknown",
    }
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn contains_ignore_case(text: &str, needle: &str) -> bool {
    text.to_ascii_lowercase().contains(needle)
}

/// 任务协议:`magnet:` / `torrent-file://` → bt,`ed2k://` → ed2k,`ftp(s)://` → ftp,
/// 清单(`.m3u8` / `.mpd`)→ hls / dash,其余 http。
fn protocol_of(task: &TaskDto) -> &'static str {
    let url = task.url.trim_start();
    if starts_with_ignore_case(url, "magnet:") || starts_with_ignore_case(url, "torrent-file://") {
        "bt"
    } else if starts_with_ignore_case(url, "ed2k://") {
        "ed2k"
    } else if starts_with_ignore_case(url, "ftp://") || starts_with_ignore_case(url, "ftps://") {
        "ftp"
    } else if contains_ignore_case(url, ".m3u8") || contains_ignore_case(&task.file_name, ".m3u8") {
        "hls"
    } else if contains_ignore_case(url, ".mpd") || contains_ignore_case(&task.file_name, ".mpd") {
        "dash"
    } else {
        "http"
    }
}

/// 不分配的主机名提取;只认 http / https / ftp / ftps,magnet / ed2k 等没有主机。
fn quick_host(url: &str) -> Option<&str> {
    let (scheme, rest) = url.trim().split_once("://")?;
    if !["http", "https", "ftp", "ftps"]
        .iter()
        .any(|known| scheme.eq_ignore_ascii_case(known))
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host_port.strip_prefix('[') {
        Some(inner) => inner.split(']').next()?,
        None => host_port.split(':').next()?,
    };
    (!host.is_empty()).then_some(host)
}

/// 任务创建时间(Unix 秒);空 / 非法为 0。
fn created_secs(task: &TaskDto) -> i64 {
    task.created_at.trim().parse().unwrap_or(0)
}

fn timestamp_value(text: &str) -> Value {
    iso_secs(text.trim().parse().unwrap_or(0))
}

fn queue_name<'a>(queues: &'a [QueueDto], queue_id: &'a str) -> &'a str {
    if queue_id.is_empty() {
        return "default";
    }
    queues
        .iter()
        .find(|queue| queue.queue_id == queue_id)
        .map_or(queue_id, |queue| queue.name.as_str())
}

fn speed_fields(speed: Option<&LiveSpeed>) -> (Value, Value) {
    speed.map_or((Value::Null, Value::Null), |speed| {
        (json!(speed.download_bps), json!(speed.upload_bps))
    })
}

// ───────────────────────── fd_list_tasks ─────────────────────────

#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum StatusFilter {
    #[default]
    All,
    Active,
    Downloading,
    Queued,
    Paused,
    Completed,
    Seeding,
    Failed,
}

impl StatusFilter {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Downloading => "downloading",
            Self::Queued => "queued",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Seeding => "seeding",
            Self::Failed => "failed",
        }
    }

    /// `active` = 正在传输(下载 / 准备中 / 正在做种上传);`completed` 含做种中的任务。
    fn accepts(self, task: &TaskDto) -> bool {
        match self {
            Self::All => true,
            Self::Active => {
                matches!(task.status, STATUS_DOWNLOADING | STATUS_PREPARING) || is_uploading(task)
            }
            Self::Downloading => task.status == STATUS_DOWNLOADING,
            Self::Queued => task.status == STATUS_PENDING,
            Self::Paused => task.status == STATUS_PAUSED,
            Self::Completed => task.status == STATUS_COMPLETED,
            Self::Seeding => is_seeding(task),
            Self::Failed => task.status == STATUS_ERROR,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    status: StatusFilter,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

/// 一行任务;实时速率要等异步采样,所以先留空位。
struct Row {
    task_id: String,
    wants_speed: bool,
    value: Value,
}

struct Listing {
    matched: usize,
    status_counts: BTreeMap<&'static str, usize>,
    rows: Vec<Row>,
}

fn row_of(task: &TaskDto, queues: &[QueueDto]) -> Row {
    let error = if task.error_message.is_empty() {
        Value::Null
    } else {
        short_text(&task.error_message)
    };
    let value = json!({
        "taskId": task.task_id,
        "name": truncate_chars(&task.file_name, MAX_NAME_CHARS),
        "protocol": protocol_of(task),
        "status": status_label(task),
        "progressPercent": percent(task.downloaded_bytes, task.total_bytes),
        "downloadedBytes": task.downloaded_bytes,
        "totalBytes": task.total_bytes,
        "host": quick_host(&task.url),
        "queue": queue_name(queues, &task.queue_id),
        "error": error,
        "createdAt": iso_secs(created_secs(task)),
    });
    Row {
        task_id: task.task_id.clone(),
        wants_speed: task.status == STATUS_DOWNLOADING || is_uploading(task),
        value,
    }
}

/// 过滤 + 统计 + 取最新的 `limit` 条;在事件投影的临界区内跑,所以只拿引用、只克隆入选行。
fn collect_listing(
    daemon: &DaemonSnapshot,
    status: StatusFilter,
    query: Option<&str>,
    limit: usize,
) -> Listing {
    let mut status_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut matched: Vec<(i64, &TaskDto)> = Vec::new();
    for task in &daemon.tasks {
        if let Some(query) = query {
            let in_name = contains_ignore_case(&task.file_name, query);
            let in_host =
                quick_host(&task.url).is_some_and(|host| contains_ignore_case(host, query));
            if !in_name && !in_host {
                continue;
            }
        }
        *status_counts.entry(status_label(task)).or_default() += 1;
        if status.accepts(task) {
            matched.push((created_secs(task), task));
        }
    }
    let total = matched.len();
    // 最新在前;同一秒创建的按任务 ID 倒序,保证结果稳定。
    let newest_first = |a: &(i64, &TaskDto), b: &(i64, &TaskDto)| {
        b.0.cmp(&a.0).then_with(|| b.1.task_id.cmp(&a.1.task_id))
    };
    if total > limit {
        matched.select_nth_unstable_by(limit - 1, newest_first);
        matched.truncate(limit);
    }
    matched.sort_by(newest_first);
    let rows = matched
        .iter()
        .map(|(_, task)| row_of(task, &daemon.queues))
        .collect();
    Listing {
        matched: total,
        status_counts,
        rows,
    }
}

fn listing_json(
    listing: Listing,
    status: StatusFilter,
    query: Option<&str>,
    limit: usize,
    speeds: Option<&HashMap<String, LiveSpeed>>,
) -> Value {
    let returned = listing.rows.len();
    let tasks: Vec<Value> = listing
        .rows
        .into_iter()
        .map(|mut row| {
            if row.wants_speed {
                let (download, upload) =
                    speed_fields(speeds.and_then(|speeds| speeds.get(&row.task_id)));
                if let Some(object) = row.value.as_object_mut() {
                    object.insert("speedBytesPerSec".to_owned(), download);
                    object.insert("uploadBytesPerSec".to_owned(), upload);
                }
            }
            row.value
        })
        .collect();
    json!({
        "filter": { "status": status.as_str(), "query": query, "limit": limit },
        "matched": listing.matched,
        "returned": returned,
        "statusCounts": listing.status_counts,
        "tasks": tasks,
    })
}

pub(super) async fn list_tasks(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    let ListArgs {
        status,
        query,
        limit,
    } = parse_args(args)?;
    let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(invalid(format!(
            "limit must be between 1 and {MAX_LIST_LIMIT}"
        )));
    }
    let limit = limit as usize;
    if let Some(query) = &query {
        check_max("query", query, MAX_QUERY_CHARS)?;
    }
    let query = query
        .map(|query| query.trim().to_ascii_lowercase())
        .filter(|query| !query.is_empty());
    let listing =
        ctx.inspect(|snapshot| collect_listing(&snapshot.daemon, status, query.as_deref(), limit));
    let speeds = if listing.rows.iter().any(|row| row.wants_speed) {
        Some(ctx.live_speeds().await)
    } else {
        None
    };
    Ok(listing_json(
        listing,
        status,
        query.as_deref(),
        limit,
        speeds.as_ref(),
    ))
}

// ───────────────────────── fd_get_task ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskIdArgs {
    task_id: String,
}

/// 一个任务在事件投影里的全部相关事实。
struct TaskFacts {
    task: TaskDto,
    runtime: Option<TaskRuntimeDto>,
    queue: String,
}

fn task_facts(ctx: &ToolContext, task_id: &str) -> Option<TaskFacts> {
    ctx.inspect(|snapshot| {
        let task = snapshot.daemon.task(task_id)?.clone();
        Some(TaskFacts {
            runtime: snapshot.daemon.task_runtime.get(task_id).cloned(),
            queue: queue_name(&snapshot.daemon.queues, &task.queue_id).to_owned(),
            task,
        })
    })
}

fn check_task_id(task_id: &str) -> Result<(), ToolError> {
    if task_id.is_empty() {
        return Err(invalid("taskId must not be empty"));
    }
    check_max("taskId", task_id, MAX_TASK_ID_CHARS)
}

fn not_found(task_id: &str) -> ToolError {
    ToolError::Rpc(format!(
        "no task with id '{}'",
        truncate_chars(task_id, MAX_TASK_ID_CHARS)
    ))
}

fn runtime_json(runtime: &TaskRuntimeDto) -> Result<Value, ToolError> {
    let shown = runtime.segments.len().min(MAX_SEGMENTS_SHOWN);
    let active_segments = runtime
        .segments
        .iter()
        .filter(|segment| segment.active == Some(true))
        .count();
    let segments: Vec<Value> = runtime
        .segments
        .iter()
        .take(MAX_SEGMENTS_SHOWN)
        .map(|segment| {
            json!({
                "index": segment.index,
                "startByte": segment.start_byte,
                "endByte": segment.end_byte,
                "downloadedBytes": segment.downloaded_bytes,
                "active": segment.active,
            })
        })
        .collect();
    Ok(json!({
        "sampledAt": iso_millis(runtime.sampled_at_ms),
        "activeTransfers": runtime.active_transfers,
        "connectedPeers": runtime.connected_peers,
        "parallelismLimit": runtime.parallelism_limit,
        "segmentCount": runtime.segments.len(),
        "activeSegments": active_segments,
        "segmentsShown": shown,
        "segments": segments,
        "hlsFragments": to_json("hls fragment progress", &runtime.hls_fragments)?,
        "bytesBySource": to_json("source bytes", &runtime.source_bytes)?,
    }))
}

fn activity_json(page: &TaskActivityPage) -> Value {
    let entries: Vec<Value> = page
        .entries
        .iter()
        .map(|entry| {
            json!({
                "at": iso_millis(entry.timestamp_ms),
                "kind": entry.kind,
                "message": truncate_chars(&entry.message, MAX_ACTIVITY_MESSAGE_CHARS),
                "status": entry.status,
            })
        })
        .collect();
    json!({
        "entries": entries,
        "olderEntriesExist": page.has_more,
        "historyTruncated": page.truncated,
    })
}

fn speed_history_json(history: Option<&TaskSpeedHistoryDto>) -> Value {
    let Some(history) = history else {
        return Value::Null;
    };
    let start = history.samples.len().saturating_sub(MAX_SPEED_SAMPLES);
    json!({
        "downloadSecs": history.active_secs,
        "transferredBytes": history.transferred_bytes,
        "averageBytesPerSec": history.average_bps(),
        "peakBytesPerSec": history.peak_bps,
        "bucketSecs": history.bucket_secs,
        "recentSamplesBytesPerSec": &history.samples[start..],
    })
}

/// 软失败:附加数据取不到时只在该字段记录原因,不让整个详情失败。
fn soft_error(error: &ToolError) -> Value {
    json!({ "error": error.to_string() })
}

fn seeding_label(code: i32) -> &'static str {
    match code {
        SEEDING_ACTIVE => "seeding",
        SEEDING_QUEUED => "queued",
        2..=7 => "stopped",
        _ => "off",
    }
}

fn seeding_json(task: &TaskDto) -> Value {
    let ratio = (task.total_bytes > 0 && task.uploaded_bytes > 0)
        .then(|| task.uploaded_bytes as f64 / task.total_bytes as f64)
        .map(|ratio| (ratio * 100.0).round() / 100.0);
    json!({
        "status": seeding_label(task.seeding_status),
        "message": short_text(&task.seeding_message),
        "uploadedBytes": task.uploaded_bytes,
        "shareRatio": ratio,
        "seedingSecs": task.seeding_time_secs,
    })
}

fn task_json(facts: &TaskFacts, speed: Option<&LiveSpeed>) -> Result<Value, ToolError> {
    let task = &facts.task;
    let (download, upload) = speed_fields(speed);
    let proxy = if task.proxy_url.is_empty() {
        Value::Null
    } else {
        Value::String(fluxdown_protocol::proxy_rules::proxy_display_endpoint(
            &task.proxy_url,
        ))
    };
    let properties = &task.properties;
    let runtime = match &facts.runtime {
        Some(runtime) => runtime_json(runtime)?,
        None => Value::Null,
    };
    Ok(json!({
        "taskId": task.task_id,
        "name": truncate_chars(&task.file_name, MAX_NAME_CHARS),
        "protocol": protocol_of(task),
        "status": status_label(task),
        "error": short_text(&task.error_message),
        "progress": {
            "downloadedBytes": task.downloaded_bytes,
            "totalBytes": task.total_bytes,
            "percent": percent(task.downloaded_bytes, task.total_bytes),
            "speedBytesPerSec": download,
            "uploadBytesPerSec": upload,
        },
        "source": {
            "url": display_url(&task.url),
            "host": quick_host(&task.url),
            "originPage": text_or_null(&display_url(&task.origin_url)),
            "referrerOrigin": if task.referrer.is_empty() { Value::Null } else { Value::String(origin_only(&task.referrer)) },
            "autoRoute": short_text(&task.auto_route),
            "ignoreTlsErrors": task.ignore_tls_errors,
            "hasChecksum": !task.checksum.is_empty(),
            "fileHash": text_or_null(&task.file_hash),
            "groupId": text_or_null(&task.group_id),
            "rssSourceId": text_or_null(&task.rss_source_id),
        },
        "saveDir": text_or_null(&display_path(&task.save_dir)),
        "fileMissing": task.file_missing,
        "queue": { "name": facts.queue, "order": task.queue_order },
        "proxy": {
            "taskProxy": proxy,
            "note": "null means the task follows the global proxy mode and site proxy rules",
        },
        "options": {
            "downloadLimitBytesPerSec": properties.download_limit_bps,
            "customUserAgent": !properties.user_agent.is_empty(),
            "siteLoginAttached": !properties.auth_ref.is_empty(),
            "category": text_or_null(&properties.category),
            "tags": properties.tags,
            "scheduledAt": iso_secs(properties.scheduled_at),
        },
        "createdAt": timestamp_value(&task.created_at),
        "completedAt": timestamp_value(&task.completed_at),
        "bytesBySource": to_json("source bytes", &task.source_bytes)?,
        "runtime": runtime,
        "seeding": if protocol_of(task) == "bt" { seeding_json(task) } else { Value::Null },
    }))
}

pub(super) async fn get_task(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    let TaskIdArgs { task_id } = parse_args(args)?;
    check_task_id(&task_id)?;
    let facts = task_facts(ctx, &task_id).ok_or_else(|| not_found(&task_id))?;
    let speed = if facts.task.status == STATUS_DOWNLOADING || is_uploading(&facts.task) {
        ctx.live_speeds().await.get(&task_id).copied()
    } else {
        None
    };
    let mut detail = task_json(&facts, speed.as_ref())?;
    let activity_query = TaskActivityQuery {
        task_id: task_id.clone(),
        before_id: None,
        after_id: None,
        limit: ACTIVITY_LIMIT,
    };
    let activity = match ctx
        .call_as::<TaskActivityPage>(
            method::DAEMON_TASK_ACTIVITY,
            to_json("task activity query", &activity_query)?,
        )
        .await
    {
        Ok(page) => activity_json(&page),
        Err(error) => soft_error(&error),
    };
    let history_query = TaskSpeedHistoryQuery {
        task_id: task_id.clone(),
    };
    let history = match ctx
        .call_as::<Option<TaskSpeedHistoryDto>>(
            method::DAEMON_TASK_SPEED_HISTORY,
            to_json("task speed history query", &history_query)?,
        )
        .await
    {
        Ok(history) => speed_history_json(history.as_ref()),
        Err(error) => soft_error(&error),
    };
    if let Some(object) = detail.as_object_mut() {
        object.insert("recentActivity".to_owned(), activity);
        object.insert("speedHistory".to_owned(), history);
    }
    Ok(detail)
}

// ───────────────────────── fd_get_bt_status ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BtArgs {
    #[serde(default)]
    task_id: Option<String>,
    /// 版本 2:BT 文件列表的起点 / 条数。
    #[serde(default)]
    files_offset: Option<u32>,
    #[serde(default)]
    files_limit: Option<u32>,
}

/// BT 文件列表默认 / 最多返回的条数(云端 Schema `filesLimit`)。
const DEFAULT_FILES_LIMIT: usize = 100;
const MAX_FILES_LIMIT: usize = 500;
const MAX_FILE_PATH_CHARS: usize = 200;

/// BT 文件优先级的展示名;界面只用四档,其余档位原样给数字。
pub(super) fn file_priority_label(priority: u8) -> String {
    match priority {
        BT_FILE_PRIORITY_SKIP => "skip".to_owned(),
        BT_FILE_PRIORITY_NORMAL => "normal".to_owned(),
        BT_FILE_PRIORITY_HIGH => "high".to_owned(),
        BT_FILE_PRIORITY_MAXIMUM => "maximum".to_owned(),
        other => other.to_string(),
    }
}

/// BT 任务的文件表(按 `index` 升序分页):`setFilePriorities` 要用这里的 `index`。
fn files_json(files: &BtFilesDto, offset: usize, limit: usize) -> Value {
    let mut sorted: Vec<_> = files.files.iter().collect();
    sorted.sort_by_key(|file| file.index);
    let items: Vec<Value> = sorted
        .iter()
        .skip(offset)
        .take(limit)
        .map(|file| {
            json!({
                "index": file.index,
                "path": truncate_chars(&file.path, MAX_FILE_PATH_CHARS),
                "sizeBytes": file.size,
                "downloadedBytes": file.downloaded,
                "priority": file_priority_label(file.priority),
            })
        })
        .collect();
    json!({
        "editable": files.editable,
        "total": sorted.len(),
        "offset": offset,
        "listed": items.len(),
        "items": items,
    })
}

/// BT 相关的 daemon 设置(平铺键)。
const BT_SETTING_KEYS: [&str; 10] = [
    "bt_enabled",
    "bt_enable_dht",
    "bt_enable_upnp",
    "bt_enable_utp",
    "bt_port_start",
    "bt_port_end",
    "bt_max_connections",
    "bt_max_connections_per_torrent",
    "bt_enable_webseed",
    "bt_proxy_enabled",
];

fn port_status_json(status: &BtPortStatusDto) -> Value {
    let mappings: Vec<Value> = status
        .mappings
        .iter()
        .take(MAX_MAPPINGS)
        .map(|mapping| {
            json!({
                "protocol": mapping.protocol,
                "internalPort": mapping.internal_port,
                "externalPort": mapping.external_port,
                "method": text_or_null(&mapping.method),
                "state": mapping.state,
                "externalAddressKnown": !mapping.external_ip.is_empty(),
                "message": short_text(&mapping.message),
            })
        })
        .collect();
    json!({
        "sessionRunning": status.running,
        "listenPort": status.listen_port,
        "portMappings": mappings,
    })
}

fn dht_json(dht: Option<&BtDhtDto>, enabled: bool) -> Result<Value, ToolError> {
    match dht {
        Some(dht) => Ok(json!({
            "state": to_json("dht state", &dht.state)?,
            "nodesV4": dht.nodes_v4,
            "nodesV6": dht.nodes_v6,
        })),
        None => Ok(json!({ "enabled": enabled })),
    }
}

fn tracker_json(tracker: &BtTrackerDto) -> Result<Value, ToolError> {
    Ok(json!({
        "tracker": origin_only(&tracker.url),
        "tier": tracker.tier,
        "source": to_json("tracker source", &tracker.source)?,
        "status": to_json("tracker status", &tracker.status)?,
        "peers": tracker.peers,
        "seeders": tracker.seeders,
        "leechers": tracker.leechers,
        "completed": tracker.downloaded,
        "message": short_text(&tracker.message),
        "lastOkAt": iso_secs(tracker.last_ok_at),
        "nextAnnounceAt": iso_secs(tracker.next_announce_at),
    }))
}

/// 对端只给汇总:客户端分布、传输协议、总速率、被防吸血标记的数量;不含任何地址。
fn peers_json(status: &BtStatusDto) -> Value {
    let peers: &[BtPeerDto] = &status.peers;
    let mut clients: BTreeMap<String, usize> = BTreeMap::new();
    let mut transports: BTreeMap<String, usize> = BTreeMap::new();
    let mut down_bps = 0_u64;
    let mut up_bps = 0_u64;
    for peer in peers {
        let name = if peer.client.is_empty() {
            "unknown".to_owned()
        } else {
            truncate_chars(&peer.client, MAX_CLIENT_NAME_CHARS)
        };
        *clients.entry(name).or_default() += 1;
        let transport = if peer.transport.is_empty() {
            "unknown"
        } else {
            peer.transport.as_str()
        };
        *transports.entry(transport.to_owned()).or_default() += 1;
        down_bps = down_bps.saturating_add(peer.down_bps);
        up_bps = up_bps.saturating_add(peer.up_bps);
    }
    let mut top: Vec<(String, usize)> = clients.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(MAX_PEER_CLIENTS);
    let top: Vec<Value> = top
        .into_iter()
        .map(|(client, count)| json!({ "client": client, "count": count }))
        .collect();
    json!({
        "counts": {
            "connected": status.peer_counts.live,
            "connecting": status.peer_counts.connecting,
            "queued": status.peer_counts.queued,
            "seen": status.peer_counts.seen,
            "dead": status.peer_counts.dead,
        },
        "listed": peers.len(),
        "transports": transports,
        "topClients": top,
        "downloadBytesPerSec": down_bps,
        "uploadBytesPerSec": up_bps,
        "flaggedByAntiLeech": peers.iter().filter(|peer| peer.suspicion.is_some()).count(),
    })
}

fn bt_task_json(facts: Option<&TaskFacts>, status: &BtStatusDto) -> Result<Value, ToolError> {
    let trackers = status
        .trackers
        .iter()
        .take(MAX_TRACKERS)
        .map(tracker_json)
        .collect::<Result<Vec<_>, _>>()?;
    let web_seeds: Vec<Value> = status
        .web_seeds
        .iter()
        .take(MAX_WEB_SEEDS)
        .map(|seed| {
            json!({
                "source": origin_only(&seed.url),
                "state": seed.state,
                "downloadedBytes": seed.downloaded,
                "message": short_text(&seed.message),
            })
        })
        .collect();
    let task = facts.map_or(Value::Null, |facts| {
        json!({
            "taskId": facts.task.task_id,
            "name": truncate_chars(&facts.task.file_name, MAX_NAME_CHARS),
            "status": status_label(&facts.task),
            "seeding": seeding_json(&facts.task),
        })
    });
    Ok(json!({
        "task": task,
        "infoHash": text_or_null(&status.info_hash),
        "infoHashV2": text_or_null(&status.info_hash_v2),
        "dht": dht_json(status.dht.as_ref(), status.dht_enabled)?,
        "peers": peers_json(status),
        "trackersTotal": status.trackers.len(),
        "trackers": trackers,
        "webSeeds": web_seeds,
    }))
}

/// `paging` = 工具版本 2:接受 `filesOffset` / `filesLimit`;版本 1 只返回默认的前 100 个文件。
pub(super) async fn bt_status(
    ctx: &ToolContext,
    args: Value,
    paging: bool,
) -> Result<Value, ToolError> {
    let BtArgs {
        task_id,
        files_offset,
        files_limit,
    } = parse_args(args)?;
    if !paging && (files_offset.is_some() || files_limit.is_some()) {
        return Err(invalid(
            "filesOffset / filesLimit need fd_get_bt_status version 2",
        ));
    }
    if task_id.is_none() && (files_offset.is_some() || files_limit.is_some()) {
        return Err(invalid("filesOffset / filesLimit need a taskId"));
    }
    let files_limit = match files_limit {
        None => DEFAULT_FILES_LIMIT,
        Some(limit) if (1..=MAX_FILES_LIMIT as u32).contains(&limit) => limit as usize,
        Some(_) => {
            return Err(invalid(format!(
                "filesLimit must be between 1 and {MAX_FILES_LIMIT}"
            )));
        }
    };
    let files_offset = files_offset.unwrap_or(0) as usize;
    if let Some(task_id) = &task_id {
        check_task_id(task_id)?;
    }
    let config = ctx.daemon_config().await?;
    let settings: serde_json::Map<String, Value> = BT_SETTING_KEYS
        .iter()
        .map(|key| ((*key).to_owned(), config_json(&config, key)))
        .collect();
    let session = match ctx
        .call_as::<BtPortStatusDto>(method::DAEMON_BT_PORT_STATUS, json!({}))
        .await
    {
        Ok(status) => port_status_json(&status),
        Err(error) => soft_error(&error),
    };
    let mut result = json!({ "settings": settings, "session": session });
    if let Some(task_id) = task_id {
        let params = DaemonBtStatusParams {
            task_id: task_id.clone(),
        };
        let status: BtStatusDto = ctx
            .call_as(
                method::DAEMON_TASK_BT_STATUS,
                to_json("bt status params", &params)?,
            )
            .await?;
        let facts = task_facts(ctx, &task_id);
        let files = match ctx
            .call_as::<BtFilesDto>(
                method::DAEMON_TASK_BT_FILES,
                to_json("bt files params", &params)?,
            )
            .await
        {
            Ok(files) => files_json(&files, files_offset, files_limit),
            Err(error) => soft_error(&error),
        };
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "taskStatus".to_owned(),
                bt_task_json(facts.as_ref(), &status)?,
            );
            object.insert("files".to_owned(), files);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{BtPeerCountsDto, BtTrackerSource, BtTrackerStatus};

    use super::*;

    fn task(id: &str, name: &str, url: &str, status: i32, created: i64) -> TaskDto {
        serde_json::from_value(json!({
            "taskId": id,
            "url": url,
            "fileName": name,
            "saveDir": "/tmp",
            "status": status,
            "downloadedBytes": 0,
            "totalBytes": 0,
            "createdAt": created.to_string(),
        }))
        .expect("task dto fixture")
    }

    fn snapshot(tasks: Vec<TaskDto>) -> DaemonSnapshot {
        DaemonSnapshot {
            tasks,
            ..DaemonSnapshot::default()
        }
    }

    #[test]
    fn protocol_detection() {
        let proto = |url: &str| protocol_of(&task("t", "f.bin", url, 0, 0));
        assert_eq!(proto("magnet:?xt=urn:btih:abc"), "bt");
        assert_eq!(proto("MAGNET:?xt=urn:btih:abc"), "bt");
        assert_eq!(proto("torrent-file://abc"), "bt");
        assert_eq!(proto("ed2k://|file|a|1|h|/"), "ed2k");
        assert_eq!(proto("ftp://h/f"), "ftp");
        assert_eq!(proto("https://h/live/index.m3u8?x=1"), "hls");
        assert_eq!(proto("https://h/stream.mpd"), "dash");
        assert_eq!(proto("https://h/file.zip"), "http");
    }

    #[test]
    fn host_extraction_skips_credentials_and_ports() {
        assert_eq!(
            quick_host("https://user:pw@example.com:8443/a?b#c"),
            Some("example.com")
        );
        assert_eq!(quick_host("ftp://h.example/f"), Some("h.example"));
        assert_eq!(quick_host("http://[::1]:80/x"), Some("::1"));
        assert_eq!(quick_host("magnet:?xt=urn:btih:abc"), None);
        assert_eq!(quick_host("ed2k://|file|a|1|h|/"), None);
        assert_eq!(quick_host("https:///nohost"), None);
    }

    #[test]
    fn listing_is_newest_first_and_limited() {
        let daemon = snapshot(vec![
            task(
                "a",
                "old.zip",
                "https://one.example/a",
                STATUS_COMPLETED,
                100,
            ),
            task(
                "b",
                "new.zip",
                "https://two.example/b",
                STATUS_DOWNLOADING,
                300,
            ),
            task("c", "mid.iso", "https://one.example/c", STATUS_ERROR, 200),
        ]);
        let all = collect_listing(&daemon, StatusFilter::All, None, 2);
        assert_eq!(all.matched, 3);
        let ids: Vec<&str> = all.rows.iter().map(|row| row.task_id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
        assert_eq!(all.status_counts.get("completed"), Some(&1));

        let failed = collect_listing(&daemon, StatusFilter::Failed, None, 30);
        assert_eq!(failed.rows.len(), 1);
        assert_eq!(failed.rows[0].value["status"], "failed");

        let by_host = collect_listing(&daemon, StatusFilter::All, Some("one.example"), 30);
        assert_eq!(by_host.matched, 2);
        let by_name = collect_listing(&daemon, StatusFilter::All, Some("new."), 30);
        assert_eq!(by_name.rows[0].task_id, "b");
        assert!(by_name.rows[0].wants_speed);
    }

    #[test]
    fn listing_rows_hide_url_secrets() {
        let daemon = snapshot(vec![task(
            "a",
            "f.bin",
            "https://u:p@host.example/f?token=abc",
            STATUS_PAUSED,
            1,
        )]);
        let listing = collect_listing(&daemon, StatusFilter::All, None, 30);
        let text = listing.rows[0].value.to_string();
        assert!(text.contains("host.example"));
        assert!(!text.contains("token") && !text.contains("u:p"), "{text}");
    }

    #[test]
    fn seeding_counts_as_completed_and_active() {
        let mut seeding = task(
            "s",
            "a.torrent",
            "magnet:?xt=urn:btih:abc",
            STATUS_COMPLETED,
            1,
        );
        seeding.seeding_status = SEEDING_ACTIVE;
        let mut queued_seed = seeding.clone();
        queued_seed.task_id = "q".to_owned();
        queued_seed.seeding_status = SEEDING_QUEUED;
        let plain = task("p", "b.zip", "https://h/b", STATUS_COMPLETED, 2);
        assert!(StatusFilter::Completed.accepts(&seeding));
        assert!(StatusFilter::Seeding.accepts(&seeding));
        assert!(StatusFilter::Seeding.accepts(&queued_seed));
        assert!(StatusFilter::Active.accepts(&seeding));
        assert!(!StatusFilter::Active.accepts(&queued_seed));
        assert!(!StatusFilter::Seeding.accepts(&plain));
        assert_eq!(status_label(&seeding), "seeding");
        assert_eq!(status_label(&plain), "completed");
    }

    #[test]
    fn list_args_reject_unknown_fields_and_bad_status() {
        assert!(parse_args::<ListArgs>(json!({ "status": "bogus" })).is_err());
        assert!(parse_args::<ListArgs>(json!({ "sort": "size" })).is_err());
        let ok = parse_args::<ListArgs>(json!({ "status": "seeding", "limit": 5 })).expect("valid");
        assert_eq!(ok.status, StatusFilter::Seeding);
        assert_eq!(ok.limit, Some(5));
    }

    #[test]
    fn peers_summary_has_no_addresses() {
        let peer = |addr: &str, client: &str| BtPeerDto {
            addr: addr.to_owned(),
            client: client.to_owned(),
            transport: "tcp".to_owned(),
            down_bps: 10,
            up_bps: 5,
            ..BtPeerDto::default()
        };
        let status = BtStatusDto {
            peer_counts: BtPeerCountsDto {
                live: 2,
                ..BtPeerCountsDto::default()
            },
            peers: vec![
                peer("203.0.113.7:51413", "qBittorrent 4.6"),
                peer("198.51.100.9:6881", "qBittorrent 4.6"),
            ],
            ..BtStatusDto::default()
        };
        let summary = peers_json(&status);
        assert_eq!(summary["downloadBytesPerSec"], 20);
        assert_eq!(summary["topClients"][0]["count"], 2);
        let text = summary.to_string();
        assert!(
            !text.contains("203.0.113.7") && !text.contains("198.51.100.9"),
            "{text}"
        );
    }

    #[test]
    fn tracker_rows_drop_the_announce_path() {
        let tracker = BtTrackerDto {
            url: "https://tracker.example:443/announce/PASSKEY123".to_owned(),
            source: BtTrackerSource::Torrent,
            status: BtTrackerStatus::Working,
            ..BtTrackerDto::default()
        };
        let row = tracker_json(&tracker).expect("encodes");
        assert_eq!(row["tracker"], "https://tracker.example");
        assert!(!row.to_string().contains("PASSKEY123"));
        assert_eq!(row["status"], "working");
    }

    #[test]
    fn task_detail_keeps_only_needed_source_information() {
        let mut dto = task(
            "t1",
            "movie.mkv",
            "https://u:p@cdn.example.com/v/movie.mkv?sig=abc",
            STATUS_ERROR,
            1_700_000_000,
        );
        dto.error_message = "HTTP 403".to_owned();
        dto.proxy_url = "socks5://name:secret@10.0.0.2:1080".to_owned();
        dto.referrer = "https://site.example/page?id=1".to_owned();
        let facts = TaskFacts {
            task: dto,
            runtime: None,
            queue: "default".to_owned(),
        };
        let detail = task_json(&facts, None).expect("encodes");
        let text = detail.to_string();
        assert_eq!(detail["source"]["host"], "cdn.example.com");
        assert_eq!(detail["proxy"]["taskProxy"], "socks5://10.0.0.2:1080");
        assert_eq!(detail["source"]["referrerOrigin"], "https://site.example");
        assert!(
            !text.contains("secret") && !text.contains("sig=abc") && !text.contains("u:p"),
            "{text}"
        );
        assert_eq!(detail["createdAt"], "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn speed_history_keeps_the_most_recent_samples() {
        let history = TaskSpeedHistoryDto {
            task_id: "t".to_owned(),
            bucket_secs: 1,
            samples: (0..100).collect(),
            active_secs: 100,
            transferred_bytes: 5000,
            peak_bps: 99,
        };
        let value = speed_history_json(Some(&history));
        let samples = value["recentSamplesBytesPerSec"]
            .as_array()
            .expect("samples");
        assert_eq!(samples.len(), MAX_SPEED_SAMPLES);
        assert_eq!(samples[MAX_SPEED_SAMPLES - 1], 99);
        assert_eq!(value["averageBytesPerSec"], 50);
        assert_eq!(speed_history_json(None), Value::Null);
    }
}
