//! BT 任务的运行时状态：peer 列表与 tracker 状态（对齐 qBittorrent 的 Peers / Trackers 页签）。
//!
//! tracker 状态取自 librqbit 的逐 tracker 实况（`ManagedTorrent::trackers`：所在层级、汇报
//! 状态、做种 / 下载人数、上次与下次汇报时间）；任务不在会话里时（未启动 / 重启后尚未恢复）
//! 只能列出它会用到的 tracker，状态一律「未联系」。
//!
//! peer 列表取自 librqbit 的逐 peer 计数器；速度由相邻两次采样的字节差折算。

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 单个 tracker 的汇报状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackerState {
    /// 还没有向它汇报过（任务未在跑，或它所在的层级当前不在用）。
    NotContacted,
    /// 正在汇报，尚无结果。
    Updating,
    /// 最近一次 announce 成功。
    Working,
    /// 最近一次 announce 失败（解析 / 连接 / tracker 报错…）。
    NotWorking,
}

/// 系统时间 → unix 秒（早于纪元或超出 `i64` 记 0 = 未知）。
fn unix_secs(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

/// tracker URL 的比较形态（与 `url::Url` 的规范化一致）。
#[must_use]
pub fn normalize_tracker(url: &str) -> String {
    url::Url::parse(url.trim()).map_or_else(|_| url.trim().to_owned(), |parsed| parsed.to_string())
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

/// tracker 行的短消息。双栈 UDP tracker 两个地址族都失败时 librqbit 给出
/// `IPv4: <原因>; IPv6: <原因>`：两边各自归类，归类相同只报一次。
fn summarize_tracker_error(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("IPv4: ")
        && let Some((v4, v6)) = rest.split_once("; IPv6: ")
    {
        let (v4, v6) = (summarize_error(v4), summarize_error(v6));
        return if v4 == v6 {
            v4
        } else {
            format!("IPv4: {v4}; IPv6: {v6}")
        };
    }
    summarize_error(raw)
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
    /// 所在层级（BEP 12，从 0 起）；会话追加的 tracker 排在种子自带的层之后。
    pub tier: u32,
    pub state: TrackerState,
    /// tracker 报告的做种 / 下载人数与完成次数（announce 响应或 scrape）；未知为 `None`。
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub downloaded: Option<u32>,
    /// 最近一次成功 announce 返回的 peer 数。
    pub peers: Option<u64>,
    /// 最近一次失败的简短归类（对齐 qBittorrent 的消息列，如 `Timed out`），
    /// 或成功时 tracker 附带的警告；都没有为空。
    pub message: String,
    /// 最近一次失败的原始错误文本（供「复制错误」）；成功后为空。
    pub detail: String,
    /// 最近一次成功的 unix 秒，0 = 从未成功。
    pub last_ok_at: i64,
    /// 预计下次 announce 的 unix 秒，0 = 未知。
    pub next_announce_at: i64,
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

/// 会话里的种子：把 librqbit 的逐 tracker 实况转成展示行（顺序即 librqbit 给出的层级顺序）。
/// `own` 是种子 / 磁力链接自带的 tracker，用来区分来源；其余算会话追加。
#[must_use]
pub fn tracker_rows(stats: Vec<librqbit::TrackerStats>, own: &[String]) -> Vec<TrackerRow> {
    let own: HashSet<String> = own.iter().map(|url| normalize_tracker(url)).collect();
    stats
        .into_iter()
        .map(|tracker| {
            let source = if own.contains(&normalize_tracker(&tracker.url)) {
                TrackerSource::Torrent
            } else {
                TrackerSource::Session
            };
            let state = match tracker.state {
                librqbit::TrackerState::NotContacted => TrackerState::NotContacted,
                librqbit::TrackerState::Updating => TrackerState::Updating,
                librqbit::TrackerState::Working => TrackerState::Working,
                librqbit::TrackerState::Error => TrackerState::NotWorking,
            };
            let detail = tracker.last_error.unwrap_or_default();
            let message = if detail.is_empty() {
                tracker.warning.unwrap_or_default()
            } else {
                summarize_tracker_error(&detail)
            };
            TrackerRow {
                url: tracker.url,
                source,
                tier: u32::try_from(tracker.tier).unwrap_or(u32::MAX),
                state,
                seeders: tracker.seeders,
                leechers: tracker.leechers,
                downloaded: tracker.downloaded,
                peers: tracker.peers_received.map(u64::from),
                message,
                detail,
                last_ok_at: tracker.last_announce.map_or(0, unix_secs),
                next_announce_at: tracker.next_announce.map_or(0, unix_secs),
            }
        })
        .collect()
}

/// 不在会话里的任务：列出它启动后会用到的 tracker，状态一律「未联系」。
///
/// `own_tiers` 是种子自带的层（磁力链接每个 `tr=` 自成一层）；`session` 是会话追加的
/// tracker，各自成层排在其后。按规范化 URL 去重，保留首次出现的位置。
#[must_use]
pub fn idle_tracker_rows(own_tiers: &[Vec<String>], session: &[String]) -> Vec<TrackerRow> {
    let mut rows: Vec<TrackerRow> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut tier: u32 = 0;
    let own = own_tiers
        .iter()
        .map(|urls| (urls.as_slice(), TrackerSource::Torrent));
    let extra = session
        .iter()
        .map(|url| (std::slice::from_ref(url), TrackerSource::Session));
    for (urls, source) in own.chain(extra) {
        let before = rows.len();
        for url in urls {
            let key = normalize_tracker(url);
            if key.is_empty() || !seen.insert(key) {
                continue;
            }
            rows.push(TrackerRow {
                url: url.trim().to_owned(),
                source,
                tier,
                state: TrackerState::NotContacted,
                seeders: None,
                leechers: None,
                downloaded: None,
                peers: None,
                message: String::new(),
                detail: String::new(),
                last_ok_at: 0,
                next_announce_at: 0,
            });
        }
        // 整层都重复 / 无效时不占层号。
        if rows.len() > before {
            tier = tier.saturating_add(1);
        }
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
    /// HTTP 源（BEP 19 webseed）；任务不在会话里时为空。
    pub web_seeds: Vec<WebSeedRow>,
    /// info hash（小写十六进制）：v1 / 混合种子为 SHA-1，纯 v2 种子为截断的 SHA-256。
    pub info_hash: String,
    /// v2 info hash（SHA-256，小写十六进制）；v1 种子 / 尚不知道为空。
    pub info_hash_v2: String,
}

/// 展示用的一个 HTTP 源（webseed）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebSeedRow {
    pub url: String,
    pub state: WebSeedState,
    /// 本次运行里从它收到并写进文件的字节。
    pub downloaded: u64,
    /// 最近一次错误；没有为空串。
    pub message: String,
}

/// HTTP 源的状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebSeedState {
    /// 没在用：任务暂停 / 已完成，或别的 HTTP 源已占满名额。
    #[default]
    Idle,
    Downloading,
    /// 暂时失败（超时、连接错误、5xx、429），过一阵再试。
    BackingOff,
    /// 本次运行不再用它（数据不对、不支持断点续传，或设置里关掉了 HTTP 源）。
    Disabled,
}

/// librqbit 的 HTTP 源实况 → 展示行。
#[must_use]
pub fn web_seed_rows(stats: Vec<librqbit::WebSeedStats>) -> Vec<WebSeedRow> {
    stats
        .into_iter()
        .map(|seed| WebSeedRow {
            url: seed.url,
            state: match seed.state {
                librqbit::WebSeedState::Idle => WebSeedState::Idle,
                librqbit::WebSeedState::Downloading => WebSeedState::Downloading,
                librqbit::WebSeedState::BackingOff => WebSeedState::BackingOff,
                librqbit::WebSeedState::Disabled => WebSeedState::Disabled,
            },
            downloaded: seed.downloaded_bytes,
            message: seed.last_error.unwrap_or_default(),
        })
        .collect()
}

/// 端口映射所用的协议。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortMappingMethod {
    Upnp,
    NatPmp,
    Pcp,
}

/// 一个端口的映射进展。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortMappingState {
    /// 还在找网关 / 等路由器答复。
    Pending,
    Mapped,
    /// 映射失败，附原因（可直接展示）。
    Failed(String),
}

/// 一个 `(协议, 内部端口)` 的映射状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortMappingRow {
    /// `true` = UDP（uTP / DHT），`false` = TCP。
    pub udp: bool,
    pub internal_port: u16,
    /// 路由器上的外部端口（可能与内部端口不同）；未映射成功为 `None`。
    pub external_port: Option<u16>,
    pub method: Option<PortMappingMethod>,
    pub external_ip: Option<std::net::IpAddr>,
    pub state: PortMappingState,
}

impl From<librqbit::PortMappingStatus> for PortMappingRow {
    fn from(status: librqbit::PortMappingStatus) -> Self {
        Self {
            udp: status.protocol == librqbit::PortMappingProtocol::Udp,
            internal_port: status.internal_port,
            external_port: status.external_port,
            method: status.method.map(|method| match method {
                librqbit::PortMappingMethod::Upnp => PortMappingMethod::Upnp,
                librqbit::PortMappingMethod::NatPmp => PortMappingMethod::NatPmp,
                librqbit::PortMappingMethod::Pcp => PortMappingMethod::Pcp,
            }),
            external_ip: status.external_ip,
            state: match status.state {
                librqbit::PortMappingState::Pending => PortMappingState::Pending,
                librqbit::PortMappingState::Mapped => PortMappingState::Mapped,
                librqbit::PortMappingState::Failed(reason) => PortMappingState::Failed(reason),
            },
        }
    }
}

/// [`crate::download_manager::DownloadManager::bt_port_status`] 的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BtPortStatusView {
    /// BT 会话是否在运行（没有 BT 任务时会话不创建，也就没有监听与映射）。
    pub running: bool,
    /// 监听端口；未运行为 0。
    pub listen_port: u16,
    /// 端口映射关闭时为空。
    pub mappings: Vec<PortMappingRow>,
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
    use super::*;

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

    fn stats(url: &str, tier: usize, state: librqbit::TrackerState) -> librqbit::TrackerStats {
        librqbit::TrackerStats {
            url: url.to_owned(),
            tier,
            state,
            seeders: None,
            leechers: None,
            downloaded: None,
            peers_received: None,
            last_announce: None,
            next_announce: None,
            last_error: None,
            warning: None,
        }
    }

    #[test]
    fn live_rows_carry_tier_counts_and_source() {
        let at = |secs: u64| UNIX_EPOCH + std::time::Duration::from_secs(secs);
        let mut working = stats(
            "udp://a.example:80/announce",
            0,
            librqbit::TrackerState::Working,
        );
        working.seeders = Some(12);
        working.leechers = Some(3);
        working.downloaded = Some(40);
        working.peers_received = Some(7);
        working.last_announce = Some(at(1_700_000_000));
        working.next_announce = Some(at(1_700_001_800));
        working.warning = Some("please update your client".to_owned());
        let mut failed = stats(
            "https://b.example/announce",
            2,
            librqbit::TrackerState::Error,
        );
        failed.last_error = Some("timed out after 30s".to_owned());
        // 失败时不展示上一次成功留下的警告。
        failed.warning = Some("stale".to_owned());

        let rows = tracker_rows(
            vec![working, failed],
            &["udp://a.example:80/announce".to_owned()],
        );
        assert_eq!(rows.len(), 2);
        let first = &rows[0];
        assert_eq!(first.source, TrackerSource::Torrent);
        assert_eq!((first.tier, first.state), (0, TrackerState::Working));
        assert_eq!(
            (first.seeders, first.leechers, first.downloaded, first.peers),
            (Some(12), Some(3), Some(40), Some(7))
        );
        assert_eq!(
            (first.last_ok_at, first.next_announce_at),
            (1_700_000_000, 1_700_001_800)
        );
        assert_eq!(first.message, "please update your client");
        assert!(first.detail.is_empty());

        let second = &rows[1];
        assert_eq!(second.source, TrackerSource::Session);
        assert_eq!((second.tier, second.state), (2, TrackerState::NotWorking));
        assert_eq!(second.message, "Timed out");
        assert_eq!(second.detail, "timed out after 30s");
        assert_eq!((second.last_ok_at, second.next_announce_at), (0, 0));
    }

    #[test]
    fn dual_stack_failures_are_summarized_per_family() {
        assert_eq!(
            summarize_tracker_error("IPv4: timed out after 30s; IPv6: timed out after 30s"),
            "Timed out"
        );
        assert_eq!(
            summarize_tracker_error(
                "IPv4: remote errored: torrent not registered; IPv6: timed out after 30s"
            ),
            "IPv4: torrent not registered; IPv6: Timed out"
        );
        assert_eq!(
            summarize_tracker_error("IPv4: only half"),
            "IPv4: only half"
        );
    }

    #[test]
    fn idle_rows_number_tiers_and_drop_duplicates() {
        let rows = idle_tracker_rows(
            &[
                vec![
                    "udp://a.example:80/announce".to_owned(),
                    "udp://b.example:80/announce".to_owned(),
                ],
                // 整层都是重复项：不占层号。
                vec!["udp://a.example:80/announce".to_owned()],
                vec![" https://c.example/announce ".to_owned()],
            ],
            &[
                "udp://b.example:80/announce".to_owned(),
                String::new(),
                "https://d.example/announce".to_owned(),
            ],
        );
        let view: Vec<(&str, u32, TrackerSource)> = rows
            .iter()
            .map(|row| (row.url.as_str(), row.tier, row.source))
            .collect();
        assert_eq!(
            view,
            vec![
                ("udp://a.example:80/announce", 0, TrackerSource::Torrent),
                ("udp://b.example:80/announce", 0, TrackerSource::Torrent),
                ("https://c.example/announce", 1, TrackerSource::Torrent),
                ("https://d.example/announce", 2, TrackerSource::Session),
            ]
        );
        assert!(
            rows.iter()
                .all(|row| row.state == TrackerState::NotContacted)
        );
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
