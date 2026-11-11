//! ED2K 服务器订阅 —— 拉取社区维护的 `server.met` 列表。
//!
//! 用户可订阅一个或多个 `server.met` URL（`ed2k_server_sub_urls` 配置，每行一个）。
//! 拉取到的二进制 `.met` 经 [`parse_server_met`] 解析为 `ip:port` 列表、去重，
//! 缓存进 `ed2k_server_sub_cache` 配置；`run_ed2k_download` 在找源时把缓存与
//! 用户手填的 `ed2k_server_list` 合并。
//!
//! 刷新触发：
//! - 启动时缓存超过 [`REFRESH_INTERVAL_SECS`]。
//! - 手动，经 `daemon.ed2k.serverSubscription.refresh` daemon RPC。
//! - 设置里订阅 URL 列表变化时。
//!
//! `server.met` 格式（amule wiki，全小端）：
//! `header(1B, 0x0E/0xE0) + count(4B) + count × [IP(4B) + port(2B) + tagCount(4B) + tags]`。
//! 本模块只取 `IP:port`，正确跳过每个 tag 以定位下一服务器。

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::time::Duration;

use crate::logger::{log_error, log_info};

/// 默认社区订阅源（`server.met`）。
///
/// - `upd.emule-security.org` —— eMule Security Team 官方列表，社区最常用。
/// - `shortypower.org` —— 长期维护的备用列表，https。
const DEFAULT_SERVER_MET_URLS: &[&str] = &[
    "https://upd.emule-security.org/server.met",
    "https://www.shortypower.org/server.met",
];

/// 缓存超过此时长即重新拉取（24 小时）。
pub const REFRESH_INTERVAL_SECS: i64 = 24 * 3600;

/// 订阅缓存格式/解析器语义版本。递增即令所有已存缓存失效并强制重取。
///
/// v2：修正 `parse_server_met` 的 IP 字节序（曾误按小端反转，把每个服务器
/// 地址打乱成死主机/组播段）。v1 写入的缓存全部反转，必须丢弃重取。
pub const CACHE_FORMAT_VERSION: i64 = 2;

/// 单源响应大小上限 —— `server.met` 通常几十 KB；超此视为非服务器列表。
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// 单源解析出的服务器数量硬上限（防畸形 count 撒谎导致过量分配/循环）。
const MAX_SERVERS_PER_SOURCE: usize = 10_000;

/// 拉取单个订阅源的 HTTP 超时。
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// 返回内置订阅 URL 列表（换行分隔），用于首次启动填充默认配置。
#[must_use]
pub fn default_server_met_urls() -> String {
    DEFAULT_SERVER_MET_URLS.join("\n")
}

/// 解析 `server.met` 二进制内容为 `ip:port` 字符串列表。
///
/// 只提取每个服务器的 IPv4 与端口，逐个 tag 按类型正确跳过以定位下一条目。
/// 任一处越界（长度撒谎/截断）即停止解析并返回已成功解出的条目（尽力而为，
/// 不 panic）。端口为 0 的条目丢弃。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::server_subscription::parse_server_met;
/// // header(0x0E) + count(1) + [IP + port 4661 + tagCount 0]
/// let mut bytes = vec![0x0E];
/// bytes.extend_from_slice(&1u32.to_le_bytes());
/// bytes.extend_from_slice(&[1, 2, 3, 4]);          // IP 网络序 → 1.2.3.4
/// bytes.extend_from_slice(&4661u16.to_le_bytes()); // port
/// bytes.extend_from_slice(&0u32.to_le_bytes());    // tagCount = 0
/// assert_eq!(parse_server_met(&bytes), vec!["1.2.3.4:4661".to_string()]);
/// ```
#[must_use]
pub fn parse_server_met(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    // header(1) + count(4)
    let Some(header) = bytes.first() else {
        return out;
    };
    if *header != 0x0E && *header != 0xE0 {
        return out;
    }
    let Some(count_bytes) = bytes.get(1..5) else {
        return out;
    };
    let count = u32::from_le_bytes([
        count_bytes[0],
        count_bytes[1],
        count_bytes[2],
        count_bytes[3],
    ]) as usize;
    let count = count.min(MAX_SERVERS_PER_SOURCE);

    let mut off = 5usize;
    for _ in 0..count {
        // IP(4) + port(2)
        let Some(ip_bytes) = bytes.get(off..off + 4) else {
            break;
        };
        // server.met 的 IP 字段按网络序（大端）直存：字节 [a,b,c,d] → a.b.c.d。
        // 经验证：本序与 eMule/gopeed 的实际服务器列表一致，且解出的服务器
        // 全部 TCP 可连通（曾误按小端反转，导致每个地址被打乱成死主机）。
        let ip = Ipv4Addr::new(ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]);
        off += 4;
        let Some(port_bytes) = bytes.get(off..off + 2) else {
            break;
        };
        let port = u16::from_le_bytes([port_bytes[0], port_bytes[1]]);
        off += 2;
        // tagCount(4)
        let Some(tc_bytes) = bytes.get(off..off + 4) else {
            break;
        };
        let tag_count =
            u32::from_le_bytes([tc_bytes[0], tc_bytes[1], tc_bytes[2], tc_bytes[3]]) as usize;
        off += 4;

        let mut tags_ok = true;
        for _ in 0..tag_count {
            match skip_tag(bytes, off) {
                Some(next) => off = next,
                None => {
                    tags_ok = false;
                    break;
                }
            }
        }
        if !tags_ok {
            break;
        }
        if port != 0 && crate::ed2k::server::is_routable_server_ip(ip) {
            out.push(format!("{ip}:{port}"));
        }
    }
    out
}

/// 跳过一个 `.met` tag，返回其后偏移；越界返回 `None`。
///
/// tag 布局：`type(1B) + nameLen(2B) + name(nameLen) + value`。
/// value 长度由 type 决定：
/// - `0x02` 字符串：`valueLen(2B) + value`
/// - `0x03` u32：4B
/// - `0x04` float：4B
/// - `0x05` bool：1B
/// - `0x06` 变长字符串（罕见）：`valueLen(2B) + value`
/// - `0x07` blob：`valueLen(4B) + value`
/// - `0x08` u16：2B
/// - `0x09` u8：1B
/// - 其它：`0x80` 以上为紧凑 tag，无法可靠跳过 → 返回 `None`（停止解析）。
fn skip_tag(bytes: &[u8], off: usize) -> Option<usize> {
    let tag_type = *bytes.get(off)?;
    let name_len = u16::from_le_bytes([*bytes.get(off + 1)?, *bytes.get(off + 2)?]) as usize;
    let mut cur = off + 3 + name_len;
    match tag_type {
        0x02 | 0x06 => {
            let vlen = u16::from_le_bytes([*bytes.get(cur)?, *bytes.get(cur + 1)?]) as usize;
            cur += 2 + vlen;
        }
        0x03 | 0x04 => cur += 4,
        0x05 | 0x09 => cur += 1,
        0x07 => {
            let vlen = u32::from_le_bytes([
                *bytes.get(cur)?,
                *bytes.get(cur + 1)?,
                *bytes.get(cur + 2)?,
                *bytes.get(cur + 3)?,
            ]) as usize;
            cur += 4 + vlen;
        }
        0x08 => cur += 2,
        _ => return None,
    }
    // 校验落点不越界（下一 tag / 下一服务器起点必须在界内或恰为末尾）。
    if cur > bytes.len() { None } else { Some(cur) }
}

/// 合并多源的 `ip:port` 列表，保序去重（丢弃格式非法项）。
#[must_use]
pub fn merge_dedup<'a, I>(sources: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in sources {
        let s = s.trim();
        if s.is_empty() || !is_valid_addr(s) {
            continue;
        }
        if seen.insert(s.to_owned()) {
            out.push(s.to_owned());
        }
    }
    out
}

/// `host:port` 是否格式合法（端口 1..=65535，host 非空）。
fn is_valid_addr(s: &str) -> bool {
    let Some((host, port)) = s.rsplit_once(':') else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    matches!(port.parse::<u16>(), Ok(p) if p > 0)
}

/// 拉取所有订阅源的结果。
#[derive(Debug)]
pub struct ServerFetchOutcome {
    /// 所有响应成功的源合并去重后的 `ip:port` 列表。
    pub servers: Vec<String>,
    /// 成功拉取的源数。
    pub ok_sources: usize,
    /// 尝试的订阅源总数。
    pub total_sources: usize,
    /// 仅当**全部**源失败时非空（拼接的错误摘要）。
    pub error: String,
}

impl ServerFetchOutcome {
    /// 至少一个源成功即为成功。
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.ok_sources > 0
    }
}

/// 拉取每个订阅 URL（换行分隔，允许 `#` 注释）并返回合并去重后的服务器列表。
/// 请求经宿主统一出口（[`crate::egress::RoutedClients`]），与下载任务同一套全局代理与站点规则。
///
/// 单源失败可容忍：只要至少一源响应即为成功。所有网络错误都记日志。
pub async fn fetch_server_subscriptions(
    urls: &str,
    proxy: &crate::proxy_config::ProxyConfig,
) -> ServerFetchOutcome {
    let sources: Vec<&str> = urls
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let total_sources = sources.len();

    if total_sources == 0 {
        return ServerFetchOutcome {
            servers: Vec::new(),
            ok_sources: 0,
            total_sources: 0,
            error: "no subscription URLs configured".to_string(),
        };
    }

    let client = match crate::egress::RoutedClients::new(proxy, "", |b| b.timeout(FETCH_TIMEOUT)) {
        Ok(c) => c,
        Err(e) => {
            return ServerFetchOutcome {
                servers: Vec::new(),
                ok_sources: 0,
                total_sources,
                error: format!("failed to build http client: {e}"),
            };
        }
    };

    let mut raw: Vec<String> = Vec::new();
    let mut ok_sources = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for src in &sources {
        let normalized = normalize_subscription_url(src);
        match fetch_one(&client, &normalized).await {
            Ok(servers) => {
                log_info!(
                    "[ed2k-server-sub] fetched {} servers from {}",
                    servers.len(),
                    src
                );
                ok_sources += 1;
                raw.extend(servers);
            }
            Err(e) => {
                log_error!("[ed2k-server-sub] fetch failed: {}: {}", src, e);
                errors.push(format!("{src}: {e}"));
            }
        }
    }

    let servers = merge_dedup(raw.iter().map(String::as_str));
    log_info!(
        "[ed2k-server-sub] refresh done: {}/{} sources ok, {} unique servers",
        ok_sources,
        total_sources,
        servers.len()
    );

    ServerFetchOutcome {
        servers,
        ok_sources,
        total_sources,
        error: if ok_sources == 0 {
            errors.join("; ")
        } else {
            String::new()
        },
    }
}
/// 归一化订阅 URL：解开 eD2K 服务器列表链接壳 `ed2k://|serverlist|<url>|/`，
/// 取出内层 http(s) URL；普通 URL 原样返回。
///
/// eMule/gopeed 用 `ed2k://|serverlist|<url>|/` 作为"服务器列表订阅"链接的标准
/// 形态；直接把整串喂给 HTTP 客户端会因 `ed2k://` scheme 报 builder error。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::server_subscription::normalize_subscription_url;
/// assert_eq!(
///     normalize_subscription_url("ed2k://|serverlist|http://x.org/server.met|/"),
///     "http://x.org/server.met"
/// );
/// assert_eq!(
///     normalize_subscription_url("https://y.org/server.met"),
///     "https://y.org/server.met"
/// );
/// ```
#[must_use]
pub fn normalize_subscription_url(src: &str) -> String {
    unwrap_list_link(src, "serverlist")
}

/// 解开 eD2K 列表链接壳 `ed2k://|<kind>|<url>|/`（`kind` 为小写 ASCII，如 `serverlist` /
/// `nodeslist`），取出内层 URL；不是该形态的输入去掉首尾空白后原样返回。
pub(crate) fn unwrap_list_link(src: &str, kind: &str) -> String {
    let s = src.trim();
    let lower = s.to_ascii_lowercase();
    let prefix = format!("ed2k://|{kind}|");
    if let Some(rest) = lower.strip_prefix(&prefix) {
        // 用小写前缀长度切原串（ASCII 前缀，字节偏移安全），保留内层大小写。
        let inner = &s[s.len() - rest.len()..];
        // 去掉尾部 `|/` 或 `|`。
        let inner = inner
            .strip_suffix("|/")
            .or_else(|| inner.strip_suffix('|'))
            .unwrap_or(inner);
        return inner.trim().to_string();
    }
    s.to_string()
}

/// 拉取单个订阅源并解析为 `ip:port` 列表。
async fn fetch_one(
    clients: &crate::egress::RoutedClients,
    url: &str,
) -> Result<Vec<String>, String> {
    let client = clients.client(url).await.map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("http status {status}"));
    }
    let body = resp.bytes().await.map_err(|e| e.to_string())?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(format!("response too large ({} bytes)", body.len()));
    }
    let servers = parse_server_met(&body);
    if servers.is_empty() {
        return Err("no servers parsed (not a server.met?)".to_string());
    }
    Ok(servers)
}

#[cfg(test)]
mod tests {
    use super::{merge_dedup, normalize_subscription_url, parse_server_met};

    /// 拼出 `header + count(4B LE)` 前缀。
    fn header_with_count(header: u8, count: u32) -> Vec<u8> {
        let mut b = vec![header];
        b.extend_from_slice(&count.to_le_bytes());
        b
    }

    /// 追加一个 `tagCount=0` 的服务器条目：IP(网络序直存) + port(LE) + tagCount(0)。
    fn push_server(b: &mut Vec<u8>, ip_octets: [u8; 4], port: u16) {
        b.extend_from_slice(&ip_octets);
        b.extend_from_slice(&port.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
    }

    /// 追加一个 `0x03`（u32，定长 4B）tag：`type + nameLen(2B) + name + value(4B)`。
    fn push_u32_tag(b: &mut Vec<u8>, name: &[u8], value: u32) {
        b.push(0x03);
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        b.extend_from_slice(name);
        b.extend_from_slice(&value.to_le_bytes());
    }

    /// 追加一个 `0x02`（字符串）tag：`type + nameLen(2B) + name + valueLen(2B) + value`。
    fn push_str_tag(b: &mut Vec<u8>, name: &[u8], value: &[u8]) {
        b.push(0x02);
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        b.extend_from_slice(name);
        b.extend_from_slice(&(value.len() as u16).to_le_bytes());
        b.extend_from_slice(value);
    }

    #[test]
    fn empty_or_too_short_returns_empty() {
        assert_eq!(parse_server_met(&[]), Vec::<String>::new());
        // 只有 header，没有 count(4B) —— 长度 < 5。
        assert_eq!(parse_server_met(&[0x0E, 0x01, 0x00]), Vec::<String>::new());
    }

    #[test]
    fn invalid_header_returns_empty() {
        let mut b = header_with_count(0x00, 1);
        push_server(&mut b, [4, 3, 2, 1], 4661);
        assert_eq!(parse_server_met(&b), Vec::<String>::new());
    }

    #[test]
    fn header_0xe0_accepted_like_0x0e() {
        let mut b = header_with_count(0xE0, 1);
        push_server(&mut b, [5, 6, 7, 8], 4662);
        assert_eq!(parse_server_met(&b), vec!["5.6.7.8:4662".to_string()]);
    }

    #[test]
    fn single_server_direct_ip_order() {
        // met 的 IP 字段按网络序直存：字节 [1,2,3,4] → 1.2.3.4（不反转）。
        let mut b = header_with_count(0x0E, 1);
        push_server(&mut b, [1, 2, 3, 4], 4661);
        assert_eq!(parse_server_met(&b), vec!["1.2.3.4:4661".to_string()]);
    }

    #[test]
    fn multiple_servers_in_order() {
        let mut b = header_with_count(0x0E, 3);
        push_server(&mut b, [11, 0, 0, 1], 4661); // 11.0.0.1
        push_server(&mut b, [11, 0, 0, 2], 4662); // 11.0.0.2
        push_server(&mut b, [11, 0, 0, 3], 4663); // 11.0.0.3
        assert_eq!(
            parse_server_met(&b),
            vec![
                "11.0.0.1:4661".to_string(),
                "11.0.0.2:4662".to_string(),
                "11.0.0.3:4663".to_string(),
            ]
        );
    }

    #[test]
    fn zero_port_server_dropped_others_kept() {
        let mut b = header_with_count(0x0E, 3);
        push_server(&mut b, [11, 0, 0, 1], 4661);
        push_server(&mut b, [11, 0, 0, 2], 0); // port=0 → 丢弃
        push_server(&mut b, [11, 0, 0, 3], 4663);
        assert_eq!(
            parse_server_met(&b),
            vec!["11.0.0.1:4661".to_string(), "11.0.0.3:4663".to_string()]
        );
    }

    #[test]
    fn count_overstated_truncated_payload_returns_parsed_prefix() {
        // 声明 3 个，实际只完整给出 2 个，第 3 个只有 4 字节 IP 就截断。
        let mut b = header_with_count(0x0E, 3);
        push_server(&mut b, [11, 0, 0, 1], 4661);
        push_server(&mut b, [11, 0, 0, 2], 4662);
        b.extend_from_slice(&[9, 9, 9, 9]); // 第三台只有 IP，无 port/tagCount
        assert_eq!(
            parse_server_met(&b),
            vec!["11.0.0.1:4661".to_string(), "11.0.0.2:4662".to_string()]
        );
    }

    #[test]
    fn count_wildly_overstated_clamped_and_no_hang() {
        // count 声明为 u32::MAX，但实际数据只够 2 台服务器 —— 必须不 panic、
        // 立即在越界处停止（越界 break 保护），且不因巨大 count 卡死/超量分配。
        let mut b = header_with_count(0x0E, u32::MAX);
        push_server(&mut b, [11, 0, 0, 1], 4661);
        push_server(&mut b, [11, 0, 0, 2], 4662);
        let start = std::time::Instant::now();
        let sparse_result = parse_server_met(&b);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(
            sparse_result,
            vec!["11.0.0.1:4661".to_string(), "11.0.0.2:4662".to_string()]
        );

        // 真的给够 10_005 台合法服务器、count 依旧声明 u32::MAX —— 必须被
        // MAX_SERVERS_PER_SOURCE(10_000) 精确截断，而不是解出全部 10_005 台。
        const N: u32 = 10_005;
        let mut abundant = header_with_count(0x0E, u32::MAX);
        for i in 0..N {
            // 全部路由可达（首字节 11，末字节 1），避免被 IP 过滤影响计数。
            let ip = [11u8, ((i >> 8) & 0xFF) as u8, (i & 0xFF) as u8, 1];
            push_server(&mut abundant, ip, 4661);
        }
        let start = std::time::Instant::now();
        let clamped_result = parse_server_met(&abundant);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(clamped_result.len(), 10_000);
    }

    #[test]
    fn tags_skipped_precisely_second_server_still_correct() {
        // 第一台服务器带两个 tag（0x03 定长 + 0x02 字符串变长），验证 tag 被
        // 精确跳过后，第二台服务器的 IP:port 仍能准确定位解出。
        let mut b = header_with_count(0x0E, 2);
        b.extend_from_slice(&[1, 2, 3, 4]); // 1.2.3.4
        b.extend_from_slice(&4661u16.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes()); // tagCount = 2
        push_u32_tag(&mut b, b"ping", 42);
        push_str_tag(&mut b, b"name", b"eMule Security");
        push_server(&mut b, [5, 6, 7, 8], 4662); // 第二台：5.6.7.8:4662
        assert_eq!(
            parse_server_met(&b),
            vec!["1.2.3.4:4661".to_string(), "5.6.7.8:4662".to_string()]
        );
    }

    #[test]
    fn unknown_tag_type_stops_parsing_keeps_prior_servers() {
        // 第一台服务器正常，第二台带一个未知 type(0x99) 的 tag —— skip_tag
        // 返回 None，解析在此停止；第一台已解出的结果必须保留。
        let mut b = header_with_count(0x0E, 2);
        push_server(&mut b, [11, 0, 0, 1], 4661); // 11.0.0.1:4661
        b.extend_from_slice(&[2, 0, 0, 10]); // 第二台 IP
        b.extend_from_slice(&4662u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes()); // tagCount = 1
        b.push(0x99); // 未知 type
        b.extend_from_slice(&0u16.to_le_bytes()); // nameLen = 0
        assert_eq!(parse_server_met(&b), vec!["11.0.0.1:4661".to_string()]);
    }

    #[test]
    fn tag_value_len_overruns_buffer_stops_without_panic() {
        // 第一台服务器正常；第二台的字符串 tag 声明 valueLen 远超剩余字节
        // （截断）—— 必须不 panic，停止解析，第一台结果保留。
        let mut b = header_with_count(0x0E, 2);
        push_server(&mut b, [11, 0, 0, 1], 4661); // 11.0.0.1:4661
        b.extend_from_slice(&[2, 0, 0, 10]); // 第二台 IP
        b.extend_from_slice(&4662u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes()); // tagCount = 1
        b.push(0x02); // 字符串 tag
        b.extend_from_slice(&0u16.to_le_bytes()); // nameLen = 0
        b.extend_from_slice(&60_000u16.to_le_bytes()); // valueLen 远超剩余字节
        // 故意不追加 60_000 字节的 value —— buffer 在此截断。
        assert_eq!(parse_server_met(&b), vec!["11.0.0.1:4661".to_string()]);
    }

    #[test]
    fn merge_dedup_preserves_order_drops_repeats() {
        let merged = merge_dedup(vec![
            "1.2.3.4:4661",
            "5.6.7.8:80",
            "1.2.3.4:4661", // 重复，只保留首次
            "9.9.9.9:1",
        ]);
        assert_eq!(
            merged,
            vec![
                "1.2.3.4:4661".to_string(),
                "5.6.7.8:80".to_string(),
                "9.9.9.9:1".to_string(),
            ]
        );
    }

    #[test]
    fn merge_dedup_drops_malformed_entries() {
        let merged = merge_dedup(vec![
            "",             // 空串
            "noport",       // 无冒号
            ":4661",        // host 空
            "h:0",          // port=0
            "h:x",          // port 非数字
            "h:99999",      // port 越界
            "1.2.3.4:4661", // 唯一合法项
        ]);
        assert_eq!(merged, vec!["1.2.3.4:4661".to_string()]);
    }

    #[test]
    fn normalize_unwraps_serverlist_link() {
        assert_eq!(
            normalize_subscription_url(
                "ed2k://|serverlist|http://upd.emule-security.org/server.met|/"
            ),
            "http://upd.emule-security.org/server.met"
        );
        // 大小写不敏感的 scheme 前缀，内层大小写保留。
        assert_eq!(
            normalize_subscription_url("ED2K://|serverlist|https://X.ORG/Server.met|/"),
            "https://X.ORG/Server.met"
        );
        // 尾部只有 `|`（无 `/`）也能解开。
        assert_eq!(
            normalize_subscription_url("ed2k://|serverlist|http://a.b/s.met|"),
            "http://a.b/s.met"
        );
    }

    #[test]
    fn normalize_passes_plain_url_through() {
        assert_eq!(
            normalize_subscription_url("https://y.org/server.met"),
            "https://y.org/server.met"
        );
        assert_eq!(
            normalize_subscription_url("  http://z.org/s.met  "),
            "http://z.org/s.met"
        );
    }
}
