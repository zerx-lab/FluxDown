//! BT 任务的运行时状态：peer 列表与 tracker 状态（对齐 qBittorrent 的 Peers / Trackers 页签）。
//!
//! librqbit 不暴露逐 tracker 的汇报状态，所以 tracker 状态由 [`layer`] 旁路采集：
//! `librqbit_tracker_comms` 为每个 tracker 的监控任务开 `http_tracker` / `udp_tracker` span（带
//! `tracker`、`info_hash` 字段），并在 announce 成功（info）/ 失败（debug）/ 休眠（debug）时
//! 打事件。本层把这些事件折叠成 `(info_hash, tracker) -> 状态`。该层必须挂在宿主装的全局
//! subscriber 上（自带每层过滤，不受主输出 `EnvFilter` 影响）；没挂时 [`installed`] 为
//! `false`，调用方应显示「未知」而不是「未联系」。
//!
//! peer 列表取自 librqbit 的逐 peer 计数器；速度由相邻两次采样的字节差折算。

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// 表容量上限：超过即整体清空（正常使用远低于此，仅防异常增长）。
const TABLE_CAP: usize = 20_000;

/// 单个 tracker 的汇报状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackerState {
    /// 监控任务已启动，尚无结果。
    Updating,
    /// 最近一次 announce 成功。
    Working,
    /// 最近一次 announce 失败（解析 / 连接 / tracker 报错…）。
    NotWorking,
}

/// 一个 tracker 的采集结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerEntry {
    pub state: TrackerState,
    /// 最近一次成功 announce 返回的 peer 数。
    pub peers: Option<u64>,
    /// 最近一次失败的简短归类（对齐 qBittorrent 的消息列，如 `Timed out`）；成功后清空。
    pub message: String,
    /// 最近一次失败的原始错误文本（供「复制错误」）；成功后清空。
    pub detail: String,
    /// 最近一次成功的 unix 秒，0 = 从未成功。
    pub last_ok_at: i64,
    /// 预计下次 announce 的 unix 秒，0 = 未知。
    pub next_announce_at: i64,
}

type Key = (String, String);

/// 同一 tracker 下单个端点（UDP 解析出的 v4 / v6 地址）的最近结果。
#[derive(Clone, Debug)]
enum Endpoint {
    Ok { peers: Option<u64> },
    Failed { message: String, detail: String },
}

/// UDP tracker 的端点数上限：超过即清空（DNS 换址后旧地址不无限累积）。
const ENDPOINT_CAP: usize = 8;

/// 表内一行：对外的聚合结果 + 逐端点的最近结果。
struct Slot {
    entry: TrackerEntry,
    endpoints: HashMap<String, Endpoint>,
}

impl Slot {
    fn new() -> Self {
        Self {
            entry: TrackerEntry {
                state: TrackerState::Updating,
                peers: None,
                message: String::new(),
                detail: String::new(),
                last_ok_at: 0,
                next_announce_at: 0,
            },
            endpoints: HashMap::new(),
        }
    }

    /// 记录一个端点的结果并重算聚合状态（对齐 qBittorrent：任一端点正常即「正常」，
    /// 全部失败才「无法连接」）。librqbit 对双栈 UDP tracker 并发向 v4 / v6 各发一次，
    /// 若按最后一个事件覆盖，v6 不通时会把刚成功的 tracker 误报为失败。
    fn record(&mut self, endpoint: String, result: Endpoint) {
        if !endpoint.is_empty() {
            // 解析失败等无地址事件只在尚无具体端点结果时有意义。
            self.endpoints.remove("");
        }
        if self.endpoints.len() >= ENDPOINT_CAP && !self.endpoints.contains_key(&endpoint) {
            self.endpoints.clear();
        }
        self.endpoints.insert(endpoint.clone(), result);
        let best_peers = self
            .endpoints
            .values()
            .filter_map(|result| match result {
                Endpoint::Ok { peers } => Some(*peers),
                Endpoint::Failed { .. } => None,
            })
            .reduce(|a, b| a.max(b));
        let entry = &mut self.entry;
        if let Some(peers) = best_peers {
            entry.state = TrackerState::Working;
            entry.peers = peers;
            entry.message.clear();
            entry.detail.clear();
        } else if let Some(Endpoint::Failed { message, detail }) = self.endpoints.get(&endpoint) {
            entry.state = TrackerState::NotWorking;
            entry.message.clone_from(message);
            entry.detail.clone_from(detail);
        }
    }
}

static TABLE: LazyLock<Mutex<HashMap<Key, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static INSTALLED: AtomicBool = AtomicBool::new(false);

fn table() -> &'static Mutex<HashMap<Key, Slot>> {
    &TABLE
}

fn with_table<R>(f: impl FnOnce(&mut HashMap<Key, Slot>) -> R) -> R {
    let mut guard = match table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut guard)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

/// tracker URL 的比较形态（与 `url::Url` 的规范化一致）。
#[must_use]
pub fn normalize_tracker(url: &str) -> String {
    url::Url::parse(url.trim()).map_or_else(|_| url.trim().to_owned(), |parsed| parsed.to_string())
}

/// 采集层是否已挂到全局 subscriber 上。
#[must_use]
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// 某个 info hash（小写十六进制）下各 tracker 的当前状态，键为规范化 URL。
#[must_use]
pub fn snapshot(info_hash: &str) -> HashMap<String, TrackerEntry> {
    with_table(|table| {
        table
            .iter()
            .filter(|((hash, _), _)| hash == info_hash)
            .map(|((_, tracker), slot)| (tracker.clone(), slot.entry.clone()))
            .collect()
    })
}

/// 挂在 tracker span 上：表键 + 是否 UDP（UDP 事件按端点地址区分）。
#[derive(Clone)]
struct SpanKey {
    key: Key,
    udp: bool,
}

#[derive(Default)]
struct Fields {
    tracker: Option<String>,
    info_hash: Option<String>,
    message: Option<String>,
    addr: Option<String>,
    /// UDP 监控循环 `trace!(interval = ?Option<Duration>, "sleeping")` 的休眠时长。
    interval: Option<String>,
    peers: Option<u64>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        match field.name() {
            "tracker" => self.tracker = Some(format!("{value:?}")),
            "info_hash" => self.info_hash = Some(format!("{value:?}")),
            "message" => self.message = Some(format!("{value:?}")),
            "addr" => self.addr = Some(format!("{value:?}")),
            "interval" => self.interval = Some(format!("{value:?}")),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "peers" {
            self.peers = Some(value);
        }
    }
}

/// `Duration` 的 Debug 文本（`1800s` / `1.5s` / `500ms`）折算成整秒。
fn parse_duration_secs(token: &str) -> Option<i64> {
    let (number, scale) = match token.strip_suffix("ms") {
        Some(value) => (value, 0.001),
        None => (token.strip_suffix('s')?, 1.0),
    };
    let seconds = number.parse::<f64>().ok()? * scale;
    if seconds.is_finite() && seconds >= 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        Some(seconds.round() as i64)
    } else {
        None
    }
}

/// 下次 announce 前的休眠秒数：HTTP 为 `"sleeping for 1800s after calling tracker"`，
/// UDP 为 `"sleeping"` + `interval = Some(1800s)`。
fn parse_sleep_secs(message: &str, interval: Option<&str>) -> Option<i64> {
    if let Some(rest) = message.strip_prefix("sleeping for ") {
        return parse_duration_secs(rest.split_whitespace().next()?);
    }
    if message != "sleeping" {
        return None;
    }
    parse_duration_secs(interval?.strip_prefix("Some(")?.strip_suffix(')')?)
}

/// 错误事件文案去掉 `error ...:` 前缀，只留原因。
fn error_reason(message: &str) -> String {
    message
        .split_once(": ")
        .map_or(message, |(_, reason)| reason)
        .trim()
        .to_owned()
}

/// `"... responded with 522"` / `"... status code 503"` 里的三位 HTTP 状态码。
fn http_status(lower: &str) -> Option<&str> {
    ["responded with ", "status code "]
        .iter()
        .find_map(|marker| {
            let (_, rest) = lower.split_once(marker)?;
            rest.get(..3)
                .filter(|code| code.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

/// 去掉 reqwest 的 ` for url (...)` 段（含 announce 查询串），保留其后的根因链，
/// 避免 URL 里的主机名 / info_hash 误触发关键词。
fn without_request_url(lower: &str) -> String {
    let Some((head, rest)) = lower.split_once(" for url (") else {
        return lower.to_owned();
    };
    // URL 中的空格已被编码，`") "` / `"):"` 只会出现在 URL 结束处。
    let tail = rest
        .find("): ")
        .or_else(|| rest.find(") "))
        .and_then(|end| rest.get(end + 1..))
        .unwrap_or("");
    format!("{head}{tail}")
}

/// 把底层（librqbit / reqwest / tokio）的原始错误文本归类成 qBittorrent 风格的短消息。
///
/// tracker 自己返回的失败原因（HTTP `failure reason` / UDP error action）原样展示，同 qBittorrent；
/// 认不出的原文原样返回（界面自带截断与悬停提示）。
fn summarize_error(raw: &str) -> String {
    for prefix in [
        "tracker returned failure. Failure reason: ",
        "remote errored: ",
    ] {
        if let Some(reason) = raw.strip_prefix(prefix) {
            return reason.trim().to_owned();
        }
    }
    let lower = raw.to_ascii_lowercase();
    let text = without_request_url(&lower);
    if let Some(code) = http_status(&text) {
        return format!("HTTP {code}");
    }
    let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    let label = if has(&["timed out", "timeout", "deadline has elapsed"]) {
        "Timed out"
    } else if has(&["connection refused"]) {
        "Connection refused"
    } else if has(&["connection reset", "broken pipe"]) {
        "Connection reset"
    } else if has(&["unreachable", "no route to host"]) {
        "Network is unreachable"
    } else if has(&["permission denied"]) {
        "Permission denied"
    } else if has(&[
        "dns error",
        "failed to lookup",
        "error looking up hostname",
        "name or service not known",
        "no such host",
        "nodename nor servname",
        "no address associated",
    ]) {
        "Host not found"
    } else if has(&["certificate", "tls", "ssl", "handshake"]) {
        "SSL error"
    } else if has(&[
        "could not parse",
        "bencode",
        "deserial",
        "invalid",
        "unexpected",
    ]) {
        "Invalid response"
    } else if text.starts_with("error sending request") {
        "Connection failed"
    } else {
        return raw.to_owned();
    };
    label.to_owned()
}

struct TrackerStatusLayer;

impl<S> Layer<S> for TrackerStatusLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_layer(&mut self, _subscriber: &mut S) {
        INSTALLED.store(true, Ordering::Relaxed);
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !matches!(attrs.metadata().name(), "http_tracker" | "udp_tracker") {
            return;
        }
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let (Some(tracker), Some(info_hash)) = (fields.tracker, fields.info_hash) else {
            return;
        };
        let key = (info_hash, normalize_tracker(&tracker));
        let udp = attrs.metadata().name() == "udp_tracker";
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanKey {
                key: key.clone(),
                udp,
            });
        }
        with_table(|table| {
            if table.len() >= TABLE_CAP {
                table.clear();
            }
            table.entry(key).or_insert_with(Slot::new);
        });
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(scope) = ctx.event_scope(event) else {
            return;
        };
        let Some(span_key) = scope
            .into_iter()
            .find_map(|span| span.extensions().get::<SpanKey>().cloned())
        else {
            return;
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        let Some(message) = fields.message else {
            return;
        };
        let now = now_secs();
        let succeeded = message.contains("tracker announce succeeded");
        // UDP：成功事件的 `tracker` / 失败事件的 `addr` 是端点地址；HTTP 只有一个端点。
        let endpoint = if span_key.udp {
            if succeeded {
                fields.tracker
            } else {
                fields.addr
            }
            .unwrap_or_default()
        } else {
            String::new()
        };
        with_table(|table| {
            // 同一 tracker 的旧 span 关闭可能晚于新 span 的创建（暂停后立即恢复）而把条目删掉；
            // 事件到达时按需补建，状态不丢。
            let slot = table.entry(span_key.key).or_insert_with(Slot::new);
            if succeeded {
                slot.entry.last_ok_at = now;
                slot.record(
                    endpoint,
                    Endpoint::Ok {
                        peers: fields.peers,
                    },
                );
            } else if message.starts_with("error ") {
                let detail = error_reason(&message);
                let message = summarize_error(&detail);
                slot.record(endpoint, Endpoint::Failed { message, detail });
            } else if let Some(secs) = parse_sleep_secs(&message, fields.interval.as_deref()) {
                slot.entry.next_announce_at = now.saturating_add(secs);
            }
        });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let key = span.extensions().get::<SpanKey>().cloned();
        if let Some(SpanKey { key, .. }) = key {
            with_table(|table| {
                table.remove(&key);
            });
        }
    }
}

/// 采集层：挂到全局 subscriber（`registry().with(layer())`）。只接 `librqbit_tracker_comms` 的 span、
/// DEBUG 及以上事件，以及带 `interval` 字段的 TRACE 事件（UDP 只在 TRACE 级报休眠时长）。
/// 过滤按 callsite 缓存，不会放大其余 TRACE 日志。
pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    TrackerStatusLayer.with_filter(filter_fn(|meta| {
        meta.target().starts_with("librqbit_tracker_comms")
            && (meta.is_span()
                || *meta.level() <= Level::DEBUG
                || meta.fields().field("interval").is_some())
    }))
}

// ───────────────────────── 任务状态 ─────────────────────────

/// tracker 行的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackerSource {
    /// 种子 / 磁力链接自带。
    Torrent,
    /// 会话按设置追加（自定义 / 内置公共 / 订阅）。
    Session,
}

/// 展示用 tracker 行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerRow {
    pub url: String,
    pub source: TrackerSource,
    /// `None` = 无采集结果（任务未在跑 / 尚未联系）。
    pub entry: Option<TrackerEntry>,
}

/// 展示用 peer 行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRow {
    pub addr: String,
    pub client: String,
    pub transport: String,
    pub downloaded: u64,
    pub uploaded: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    /// ISO 3166-1 国家码（大写）；库未就绪 / 内网 / 未收录为 `None`。
    pub country: Option<String>,
    /// 防吸血研判（该连接已被 guard 观察到时才有）。
    pub insight: Option<crate::bt_guard::PeerInsight>,
}

/// peer 数量汇总（来自 librqbit）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerCounts {
    pub live: u32,
    pub connecting: u32,
    pub queued: u32,
    pub seen: u32,
    pub dead: u32,
}

/// 合并种子自带 / 会话追加 / 已采集的 tracker，按规范化 URL 去重，顺序 = 种子自带 → 追加 → 其余。
#[must_use]
pub fn merge_trackers(
    own: &[String],
    session: &[String],
    entries: &HashMap<String, TrackerEntry>,
) -> Vec<TrackerRow> {
    let mut rows: Vec<TrackerRow> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |url: &str, source: TrackerSource| {
        let key = normalize_tracker(url);
        if key.is_empty() || !seen.insert(key.clone()) {
            return;
        }
        rows.push(TrackerRow {
            entry: entries.get(&key).cloned(),
            url: url.trim().to_owned(),
            source,
        });
    };
    for url in own {
        push(url, TrackerSource::Torrent);
    }
    for url in session {
        push(url, TrackerSource::Session);
    }
    let mut rest: Vec<&String> = entries.keys().collect();
    rest.sort();
    for url in rest {
        push(url, TrackerSource::Session);
    }
    rows
}

/// [`crate::download_manager::DownloadManager::bt_status`] 的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BtStatusView {
    pub dht_enabled: bool,
    pub counts: PeerCounts,
    pub trackers: Vec<TrackerRow>,
    pub peers: Vec<PeerRow>,
}

#[derive(Default)]
struct PeerSample {
    at: Option<Instant>,
    /// addr -> (downloaded, uploaded, down_bps, up_bps)
    peers: HashMap<String, (u64, u64, u64, u64)>,
}

static SAMPLES: LazyLock<Mutex<HashMap<String, PeerSample>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 用上一次采样折算每个 peer 的速度（字节/秒），并更新采样。相邻调用不足 400ms 时沿用上次速度。
pub fn apply_speeds(task_id: &str, rows: &mut [PeerRow]) {
    let samples = &*SAMPLES;
    let mut guard = match samples.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.len() > 256 {
        guard.clear();
    }
    let sample = guard.entry(task_id.to_owned()).or_default();
    let now = Instant::now();
    let elapsed_ms = sample
        .at
        .map(|at| now.duration_since(at).as_millis())
        .unwrap_or(0);
    let fresh = elapsed_ms >= 400;
    let mut next = HashMap::with_capacity(rows.len());
    for row in rows.iter_mut() {
        let previous = sample.peers.get(&row.addr).copied();
        let (down_bps, up_bps) = match previous {
            Some((down, up, _, _)) if fresh => {
                let ms = u64::try_from(elapsed_ms).unwrap_or(u64::MAX).max(1);
                (
                    row.downloaded.saturating_sub(down).saturating_mul(1000) / ms,
                    row.uploaded.saturating_sub(up).saturating_mul(1000) / ms,
                )
            }
            Some((_, _, down_bps, up_bps)) => (down_bps, up_bps),
            None => (0, 0),
        };
        row.down_bps = down_bps;
        row.up_bps = up_bps;
        next.insert(
            row.addr.clone(),
            (row.downloaded, row.uploaded, down_bps, up_bps),
        );
    }
    if fresh || sample.at.is_none() {
        sample.at = Some(now);
        sample.peers = next;
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    #[test]
    fn layer_folds_announce_events_into_tracker_state() {
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::debug_span!(
                target: "librqbit_tracker_comms::tracker_comms",
                parent: None,
                "http_tracker",
                tracker = %"http://t.example:80/announce",
                info_hash = %"abc123"
            );
            let _guard = span.enter();
            let entry = || {
                snapshot("abc123")
                    .get(&normalize_tracker("http://t.example:80/announce"))
                    .cloned()
            };
            assert_eq!(entry().map(|e| e.state), Some(TrackerState::Updating));

            tracing::debug!(
                target: "librqbit_tracker_comms::tracker_comms",
                "error calling tracker: connection refused"
            );
            let failed = entry().expect("entry");
            assert_eq!(failed.state, TrackerState::NotWorking);
            assert_eq!(failed.message, "Connection refused");
            assert_eq!(failed.detail, "connection refused");

            tracing::info!(
                target: "librqbit_tracker_comms::tracker_comms",
                peers = 7usize,
                "tracker announce succeeded"
            );
            tracing::debug!(
                target: "librqbit_tracker_comms::tracker_comms",
                "sleeping for 1800s after calling tracker"
            );
            let ok = entry().expect("entry");
            assert_eq!(ok.state, TrackerState::Working);
            assert_eq!(ok.peers, Some(7));
            assert!(ok.message.is_empty());
            assert!(ok.last_ok_at > 0);
            assert!(ok.next_announce_at >= ok.last_ok_at + 1799);
            drop(_guard);
            drop(span);
            assert!(
                snapshot("abc123").is_empty(),
                "closing the span forgets the tracker"
            );
        });
        assert!(installed());
    }

    #[test]
    fn dual_stack_udp_tracker_is_working_if_any_endpoint_succeeds() {
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::debug_span!(
                target: "librqbit_tracker_comms::tracker_comms",
                parent: None,
                "udp_tracker",
                tracker = %"udp://u.example:80/announce",
                info_hash = %"dual"
            );
            let _guard = span.enter();
            let entry = || {
                snapshot("dual")
                    .get(&normalize_tracker("udp://u.example:80/announce"))
                    .cloned()
                    .expect("entry")
            };
            let v4: std::net::SocketAddr = "1.2.3.4:80".parse().expect("v4");
            let v6: std::net::SocketAddr = "[2001:db8::1]:80".parse().expect("v6");

            // librqbit 并发向 v4 / v6 announce：v4 成功、v6 随后失败。
            tracing::info!(
                target: "librqbit_tracker_comms::tracker_comms",
                tracker = ?v4,
                peers = 3usize,
                "tracker announce succeeded"
            );
            tracing::debug!(
                target: "librqbit_tracker_comms::tracker_comms",
                addr = ?v6,
                "error reading announce response: remote replied with something we could not parse"
            );
            let mixed = entry();
            assert_eq!(mixed.state, TrackerState::Working);
            assert_eq!(mixed.peers, Some(3));
            assert!(mixed.message.is_empty());

            // 下一轮 v4 也失败：全部端点失败才「无法连接」，消息取最近一次失败。
            tracing::debug!(
                target: "librqbit_tracker_comms::tracker_comms",
                addr = ?v4,
                "error reading announce response: timeout connecting: deadline has elapsed"
            );
            let failed = entry();
            assert_eq!(failed.state, TrackerState::NotWorking);
            assert_eq!(failed.message, "Timed out");
            assert_eq!(failed.detail, "timeout connecting: deadline has elapsed");
            assert!(
                failed.last_ok_at > 0,
                "last success survives later failures"
            );

            // UDP 只在 TRACE 级报休眠时长；采集层须单独放行它。
            tracing::trace!(
                target: "librqbit_tracker_comms::tracker_comms",
                interval = ?Some(std::time::Duration::from_secs(1800)),
                "sleeping"
            );
            let slept = entry();
            assert!(slept.next_announce_at >= slept.last_ok_at + 1799);
        });
    }

    #[test]
    fn sleep_and_error_text_parse() {
        assert_eq!(
            parse_sleep_secs("sleeping for 1800s after calling tracker", None),
            Some(1800)
        );
        assert_eq!(parse_sleep_secs("sleeping for 500ms after", None), Some(1));
        assert_eq!(parse_sleep_secs("sleeping for 30m after", None), None);
        assert_eq!(
            parse_sleep_secs("sleeping", Some("Some(1800s)")),
            Some(1800)
        );
        assert_eq!(parse_sleep_secs("sleeping", Some("None")), None);
        assert_eq!(parse_sleep_secs("starting monitor", Some("Some(5s)")), None);
        assert_eq!(
            error_reason("error reading announce response: timed out"),
            "timed out"
        );
    }

    #[test]
    fn summarize_error_matches_qbittorrent_style() {
        let cases = [
            ("timeout connecting: deadline has elapsed", "Timed out"),
            ("tracker responded with 522", "HTTP 522"),
            (
                "remote replied with something we could not parse",
                "Invalid response",
            ),
            (
                "error sending request for url (https://t.example/announce?info_hash=%C1q%93)",
                "Connection failed",
            ),
            (
                "error sending request for url (https://timeout.example/a)",
                "Connection failed",
            ),
            ("Connection refused (os error 111)", "Connection refused"),
            ("dns error: failed to lookup address", "Host not found"),
            (
                "error sending request for url (https://t.example/a?x=1): client error (Connect): tcp connect error: Connection refused (os error 61)",
                "Connection refused",
            ),
            (
                "error sending request for url (https://t.example/a): client error (Connect): invalid peer certificate: Expired",
                "SSL error",
            ),
            (
                "tracker returned failure. Failure reason: Invalid info_hash",
                "Invalid info_hash",
            ),
            (
                "remote errored: torrent not registered",
                "torrent not registered",
            ),
            ("something novel", "something novel"),
        ];
        for (raw, expected) in cases {
            assert_eq!(summarize_error(raw), expected, "{raw}");
        }
    }

    #[test]
    fn merge_orders_dedups_and_attaches_entries() {
        let mut entries = HashMap::new();
        let working = TrackerEntry {
            state: TrackerState::Working,
            peers: Some(3),
            message: String::new(),
            detail: String::new(),
            last_ok_at: 1,
            next_announce_at: 0,
        };
        entries.insert(
            normalize_tracker("udp://a.example:80/announce"),
            working.clone(),
        );
        entries.insert(
            normalize_tracker("udp://orphan.example:80/announce"),
            working,
        );
        let rows = merge_trackers(
            &["udp://a.example:80/announce".into()],
            &[
                "udp://a.example:80/announce".into(),
                "https://b.example/announce".into(),
            ],
            &entries,
        );
        let urls: Vec<&str> = rows.iter().map(|row| row.url.as_str()).collect();
        assert_eq!(urls.len(), 3);
        assert_eq!(rows[0].source, TrackerSource::Torrent);
        assert!(rows[0].entry.is_some());
        assert!(rows[1].entry.is_none());
        assert_eq!(rows[2].source, TrackerSource::Session);
    }

    #[test]
    fn speeds_come_from_counter_deltas() {
        let row = |down, up| PeerRow {
            addr: "1.2.3.4:5".into(),
            client: String::new(),
            transport: "tcp".into(),
            downloaded: down,
            uploaded: up,
            down_bps: 0,
            up_bps: 0,
            country: None,
            insight: None,
        };
        let mut first = [row(0, 0)];
        apply_speeds("speed-task", &mut first);
        assert_eq!((first[0].down_bps, first[0].up_bps), (0, 0));
        std::thread::sleep(std::time::Duration::from_millis(450));
        let mut second = [row(45_000, 90_000)];
        apply_speeds("speed-task", &mut second);
        assert!(
            second[0].down_bps > 50_000 && second[0].down_bps < 110_000,
            "{}",
            second[0].down_bps
        );
        assert!(second[0].up_bps > second[0].down_bps);
    }
}
