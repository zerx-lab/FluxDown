//! 多来源候选 IP 解析聚合器。
//!
//! 并发查询【系统 DNS】与【内置 DoH-JSON 端点】的 A/AAAA 记录，去重合并为
//! 候选集（系统 DNS 结果排前）。所有来源各自受单源超时约束（1.5s），故整体
//! 耗时有界（< 2s 预算）；任一来源超时/失败仅丢弃该源，绝不影响其余来源。
//!
//! 安全边界（方案 §1.2 规则 3）：
//! - DoH 端点强制 `https://` scheme 白名单 + 数量上限（≤16）；
//! - 端点必须是 IP-literal（`https://223.5.5.5/resolve`），防"解析器地址
//!   本身需要 DNS 解析"的鸡生蛋问题；证书按 IP SAN 严格校验。
//!
//! host 级结果做 5 分钟内存缓存——同一批任务（多文件下载）复用解析结果。

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::logger::log_info;

/// 单来源（系统 DNS / 单个 DoH 端点）超时。
const PER_SOURCE_TIMEOUT: Duration = Duration::from_millis(1500);

/// host 级候选缓存 TTL。
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// 云端可下发的 resolver 端点数量上限（§1.2 规则 3）。
const MAX_DOH_ENDPOINTS: usize = 16;

/// 内置 DoH-JSON 端点 baseline（Google JSON API 格式，`?name=&type=`，
/// `Accept: application/dns-json`）。必须是 IP-literal HTTPS 端点：
/// - AliDNS `223.5.5.5`：`/resolve`，证书含 IP SAN，国内可达性最好，
///   支持 `edns_client_subnet` 参数（ECS）；
/// - Cloudflare `1.1.1.1`：`/dns-query`，证书含 IP SAN，海外兜底
///   （隐私立场明确不支持 ECS）。
///
/// 未登录/断云/云端未配置时的兜底——动态清单见 [`set_dynamic_endpoints`]。
fn builtin_endpoints() -> Vec<ResolverEndpoint> {
    vec![
        ResolverEndpoint {
            url: "https://223.5.5.5/resolve".to_string(),
            ecs: true,
        },
        ResolverEndpoint {
            url: "https://1.1.1.1/dns-query".to_string(),
            ecs: false,
        },
    ]
}

/// DoH resolver 端点：`ecs = true` 表示支持 `edns_client_subnet` 查询参数
/// （P2 冷启动多子网探测用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverEndpoint {
    pub url: String,
    pub ecs: bool,
}

/// 云端下发的动态端点清单（config 表 `cdn_resolver_endpoints`）。
/// 空 = 使用内置 baseline。三级回退：云端清单 → 缓存（config 表天然就是
/// 缓存）→ 内置 baseline，任何一级失败都不影响下载功能。
static DYNAMIC_ENDPOINTS: OnceLock<StdMutex<Vec<ResolverEndpoint>>> = OnceLock::new();

fn dynamic_endpoints() -> &'static StdMutex<Vec<ResolverEndpoint>> {
    DYNAMIC_ENDPOINTS.get_or_init(|| StdMutex::new(Vec::new()))
}

/// 云端下发的 ECS 探测子网（config 表 `cdn_ecs_subnets`，JSON 字符串数组，
/// 形如 `"202.96.128.0/24"`）。仅 IPv4 CIDR（方案：IPv6 hints 只收不下发）。
static ECS_SUBNETS: OnceLock<StdMutex<Vec<String>>> = OnceLock::new();

/// 单次解析中 ECS 探测查询总数上限（子网 × ECS 端点的乘积裁剪）。
const MAX_ECS_QUERIES: usize = 8;

/// ECS 子网数量上限。
const MAX_ECS_SUBNETS: usize = 4;

fn ecs_subnets() -> &'static StdMutex<Vec<String>> {
    ECS_SUBNETS.get_or_init(|| StdMutex::new(Vec::new()))
}

/// 校验 ECS 子网字面量：`a.b.c.d/len`，len 1..=32。
fn ecs_subnet_valid(s: &str) -> bool {
    let Some((ip, len)) = s.split_once('/') else {
        return false;
    };
    ip.parse::<std::net::Ipv4Addr>().is_ok()
        && len
            .parse::<u8>()
            .map(|l| (1..=32).contains(&l))
            .unwrap_or(false)
}

/// 安装云端下发的 ECS 子网清单（非法条目丢弃，上限 [`MAX_ECS_SUBNETS`]）。
pub fn set_ecs_subnets(json: &str) -> usize {
    let parsed: Vec<String> = serde_json::from_str(json).unwrap_or_default();
    let valid: Vec<String> = parsed
        .into_iter()
        .filter(|s| ecs_subnet_valid(s))
        .take(MAX_ECS_SUBNETS)
        .collect();
    let n = valid.len();
    if let Ok(mut subs) = ecs_subnets().lock() {
        *subs = valid;
    }
    if let Ok(mut cache) = resolve_cache().lock() {
        cache.clear();
    }
    n
}

/// Engine 启动时从 config 表读回 ECS 子网清单。
pub(crate) async fn load_ecs_from_config(db: &crate::db::Db) {
    if let Ok(Some(raw)) = db.get_config("cdn_ecs_subnets").await
        && !raw.trim().is_empty()
    {
        set_ecs_subnets(&raw);
    }
}

/// 解析并安装云端下发的端点清单。**向后兼容两种元素形式**：纯字符串
/// `"https://..."`（ecs=false）或对象 `{"url":"https://...","ecs":true}`。
/// 校验（§1.2 规则 3）：强制 `https://` scheme 白名单 + 数量 ≤
/// [`MAX_DOH_ENDPOINTS`]——管理面被攻破也无法把客户端解析流量导向明文/
/// 任意端点。非法条目静默丢弃；整体解析失败或结果为空 → 清空动态清单
/// （回退内置 baseline）。返回生效条数。
pub fn set_dynamic_endpoints(json: &str) -> usize {
    let parsed: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    let valid: Vec<ResolverEndpoint> = parsed
        .into_iter()
        .filter_map(|v| match v {
            serde_json::Value::String(url) => Some(ResolverEndpoint { url, ecs: false }),
            serde_json::Value::Object(map) => {
                let url = map.get("url")?.as_str()?.to_string();
                let ecs = map.get("ecs").and_then(|e| e.as_bool()).unwrap_or(false);
                Some(ResolverEndpoint { url, ecs })
            }
            _ => None,
        })
        .filter(|e| endpoint_allowed(&e.url))
        .take(MAX_DOH_ENDPOINTS)
        .collect();
    let n = valid.len();
    if let Ok(mut eps) = dynamic_endpoints().lock() {
        *eps = valid;
    }
    // 清单变化使旧解析结果失去代表性：清空 host 级候选缓存。
    if let Ok(mut cache) = resolve_cache().lock() {
        cache.clear();
    }
    log_info!("[cdn-resolver] 动态 resolver 端点已更新: {} 条生效", n);
    n
}

/// Engine 启动时从 config 表读回动态端点清单（与健康度加载同一生命周期点）。
pub(crate) async fn load_endpoints_from_config(db: &crate::db::Db) {
    if let Ok(Some(raw)) = db.get_config("cdn_resolver_endpoints").await
        && !raw.trim().is_empty()
    {
        set_dynamic_endpoints(&raw);
    }
}

/// 当前生效的端点清单：动态非空用动态，否则内置 baseline。
fn effective_endpoints() -> Vec<ResolverEndpoint> {
    if let Ok(eps) = dynamic_endpoints().lock()
        && !eps.is_empty()
    {
        return eps.clone();
    }
    builtin_endpoints()
}

/// 解析聚合结果。
#[derive(Debug, Clone, Default)]
pub struct CandidateSet {
    /// 去重后的候选 IP，系统 DNS 结果排前（保序）。
    pub ips: Vec<IpAddr>,
    /// 实际给出 ≥1 个应答的来源数（诊断用）。
    pub sources: u8,
    /// 每个候选 IP 的首次给出来源：`"sys"`（系统 DNS）/ `"doh:<端点IP>"` /
    /// `"ecs:<端点IP>"`。供多 CDN 事件（详情面板日志）做来源归因。
    pub origins: HashMap<IpAddr, String>,
}

/// host → (缓存时刻, 候选集) 的进程级缓存。
static RESOLVE_CACHE: OnceLock<
    StdMutex<std::collections::HashMap<String, (Instant, CandidateSet)>>,
> = OnceLock::new();

fn resolve_cache() -> &'static StdMutex<std::collections::HashMap<String, (Instant, CandidateSet)>>
{
    RESOLVE_CACHE.get_or_init(|| StdMutex::new(std::collections::HashMap::new()))
}

/// DoH / hints 共用轻量 client（懒建一次）：无代理、短超时、严格 TLS。
/// 构建失败（极罕见）→ None，DoH/hints 来源整体禁用，系统 DNS 仍工作。
static LIGHT_CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();

pub(crate) fn light_client() -> Option<&'static reqwest::Client> {
    LIGHT_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(PER_SOURCE_TIMEOUT)
                .connect_timeout(Duration::from_millis(1200))
                .http1_only()
                .build()
                .ok()
        })
        .as_ref()
}

/// Google JSON API 格式的 DoH 响应（只取需要的字段）。
#[derive(Deserialize)]
struct DohJson {
    #[serde(rename = "Answer", default)]
    answer: Vec<DohAnswer>,
}

#[derive(Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    rtype: u16,
    data: String,
}

/// 校验 DoH 端点是否可用：https scheme（IP-literal 端点自然满足）。
/// 非 https 一律拒绝——resolver 流量绝不允许降级明文（§1.2 规则 3）。
fn endpoint_allowed(endpoint: &str) -> bool {
    reqwest::Url::parse(endpoint)
        .map(|u| u.scheme() == "https")
        .unwrap_or(false)
}

/// 从 DoH JSON 应答中提取 A/AAAA 记录的 IP（容忍 CNAME 等其他记录混入）。
fn extract_ips(json: &DohJson) -> Vec<IpAddr> {
    json.answer
        .iter()
        .filter(|a| a.rtype == 1 || a.rtype == 28)
        .filter_map(|a| a.data.parse::<IpAddr>().ok())
        .collect()
}

/// 端点 URL → 主机部分（IP-literal），用作来源标记的可读后缀。
/// 解析失败（不应发生，端点已经过校验）→ 原 URL 兜底。
fn endpoint_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string())
}

/// 合并多来源解析结果：`system` 保序排前，随后按来源顺序合并各标记来源
/// （`(来源标记, ips)`）的应答，全局去重并记录每 IP 的首次来源。
/// `sources` = 给出 ≥1 IP 的来源数。
fn merge_candidates(system: Vec<IpAddr>, labeled: Vec<(String, Vec<IpAddr>)>) -> CandidateSet {
    let mut seen: HashSet<IpAddr> = HashSet::new();
    let mut ips = Vec::new();
    let mut origins = HashMap::new();
    let mut sources = 0u8;
    if !system.is_empty() {
        sources += 1;
    }
    for ip in system {
        if seen.insert(ip) {
            ips.push(ip);
            origins.insert(ip, "sys".to_string());
        }
    }
    for (label, source_ips) in labeled {
        if !source_ips.is_empty() {
            sources = sources.saturating_add(1);
        }
        for ip in source_ips {
            if seen.insert(ip) {
                ips.push(ip);
                origins.insert(ip, label.clone());
            }
        }
    }
    CandidateSet {
        ips,
        sources,
        origins,
    }
}

/// 查询单个 DoH 端点的一种记录类型。`ecs_subnet` 非空时追加
/// `edns_client_subnet` 参数（Google JSON API 约定，AliDNS 兼容）——
/// 用于 P2 冷启动的多子网低置信候选探测。任何失败（超时/非 2xx/解析
/// 失败）→ 空。
async fn query_doh(endpoint: &str, host: &str, rtype: &str, ecs_subnet: &str) -> Vec<IpAddr> {
    let Some(client) = light_client() else {
        return Vec::new();
    };
    let mut url = format!("{endpoint}?name={host}&type={rtype}");
    if !ecs_subnet.is_empty() {
        url.push_str(&format!("&edns_client_subnet={ecs_subnet}"));
    }
    let fut = async {
        let resp = client
            .get(&url)
            .header("accept", "application/dns-json")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let json: DohJson = resp.json().await.ok()?;
        Some(extract_ips(&json))
    };
    match tokio::time::timeout(PER_SOURCE_TIMEOUT, fut).await {
        Ok(Some(ips)) => ips,
        _ => Vec::new(),
    }
}

/// 系统 DNS 解析（tokio getaddrinfo）。broken resolver 场景可能悬挂，
/// 故同样受单源超时约束。
async fn query_system_dns(host: &str, port: u16) -> Vec<IpAddr> {
    let fut = tokio::net::lookup_host((host, port));
    match tokio::time::timeout(PER_SOURCE_TIMEOUT, fut).await {
        Ok(Ok(addrs)) => addrs.map(|a| a.ip()).collect(),
        _ => Vec::new(),
    }
}

/// 多来源并发解析 `host` 的候选 IP（含 5min 缓存）。
///
/// 永不失败：所有来源全灭时返回空集（调用方据此退单节点池）。
pub async fn resolve_candidates(host: &str, port: u16) -> CandidateSet {
    if let Ok(cache) = resolve_cache().lock()
        && let Some((at, set)) = cache.get(host)
        && at.elapsed() < CACHE_TTL
    {
        return set.clone();
    }

    // 动态清单（云端下发，已在 set_dynamic_endpoints 校验）→ 内置 baseline。
    let endpoints = effective_endpoints();

    // 系统 DNS 与所有 DoH 端点（A + AAAA 各一请求）全并发；各自独立超时。
    let system_fut = query_system_dns(host, port);
    let doh_futs = endpoints.iter().map(|ep| {
        let url = ep.url.clone();
        let label = format!("doh:{}", endpoint_host(&ep.url));
        async move {
            let (v4, v6) = futures_util::join!(
                query_doh(&url, host, "A", ""),
                query_doh(&url, host, "AAAA", "")
            );
            let mut ips = v4;
            ips.extend(v6);
            (label, ips)
        }
    });
    // ECS 多子网低置信探测（P2）：仅对 ecs 端点 × 云端下发子网，总量封顶。
    // 结果作为独立来源【排在常规应答之后】合并（低置信 → 排后，仅扩大
    // 候选池；connect 预筛与 EWMA 实测永远覆盖先验）。
    let subnets: Vec<String> = ecs_subnets().lock().map(|s| s.clone()).unwrap_or_default();
    let ecs_pairs: Vec<(String, String)> = endpoints
        .iter()
        .filter(|ep| ep.ecs)
        .flat_map(|ep| subnets.iter().map(move |s| (ep.url.clone(), s.clone())))
        .take(MAX_ECS_QUERIES)
        .collect();
    let ecs_futs = ecs_pairs.iter().map(|(url, subnet)| {
        let label = format!("ecs:{}", endpoint_host(url));
        async move { (label, query_doh(url, host, "A", subnet).await) }
    });
    let (system, mut doh, ecs) = futures_util::join!(
        system_fut,
        futures_util::future::join_all(doh_futs),
        futures_util::future::join_all(ecs_futs)
    );
    doh.extend(ecs);

    let set = merge_candidates(system, doh);
    log_info!(
        "[cdn-resolver] host {} 聚合解析: {} 个候选 IP（{} 个来源应答）",
        host,
        set.ips.len(),
        set.sources
    );
    if let Ok(mut cache) = resolve_cache().lock() {
        cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        cache.insert(host.to_string(), (Instant::now(), set.clone()));
    }
    set
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        DohJson, MAX_DOH_ENDPOINTS, MAX_ECS_SUBNETS, ResolverEndpoint, builtin_endpoints,
        effective_endpoints, endpoint_allowed, extract_ips, merge_candidates,
        set_dynamic_endpoints, set_ecs_subnets,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn v4(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(1, 2, 3, n))
    }

    #[test]
    fn merge_dedups_and_keeps_system_first() {
        let system = vec![v4(1), v4(2)];
        let doh = vec![
            ("doh:223.5.5.5".to_string(), vec![v4(2), v4(3)]),
            ("doh:1.1.1.1".to_string(), vec![v4(3), v4(4)]),
        ];
        let set = merge_candidates(system, doh);
        assert_eq!(set.ips, vec![v4(1), v4(2), v4(3), v4(4)]);
        assert_eq!(set.sources, 3);
        // 来源归因：首个给出该 IP 的来源获胜（系统 DNS 排前）。
        assert_eq!(set.origins[&v4(1)], "sys");
        assert_eq!(set.origins[&v4(2)], "sys");
        assert_eq!(set.origins[&v4(3)], "doh:223.5.5.5");
        assert_eq!(set.origins[&v4(4)], "doh:1.1.1.1");
    }

    #[test]
    fn merge_counts_only_answering_sources() {
        let set = merge_candidates(
            Vec::new(),
            vec![
                ("doh:223.5.5.5".to_string(), Vec::new()),
                ("ecs:223.5.5.5".to_string(), vec![v4(9)]),
            ],
        );
        assert_eq!(set.ips, vec![v4(9)]);
        assert_eq!(set.sources, 1);
        assert_eq!(set.origins[&v4(9)], "ecs:223.5.5.5");
    }

    #[test]
    fn endpoint_whitelist_rejects_non_https() {
        assert!(endpoint_allowed("https://223.5.5.5/resolve"));
        assert!(!endpoint_allowed("http://223.5.5.5/resolve"));
        assert!(!endpoint_allowed("ftp://x"));
        assert!(!endpoint_allowed("not a url"));
    }

    /// 动态清单校验与回退（同进程全局态：结束时恢复空清单，避免串扰）。
    #[test]
    fn dynamic_endpoints_validate_and_fall_back() {
        // 非法条目（http/垃圾）被丢弃，超量截断；字符串与对象两种形式混用。
        let mut many: Vec<serde_json::Value> = (0..20)
            .map(|i| serde_json::json!({"url": format!("https://10.0.0.{i}/dns-query"), "ecs": i % 2 == 0}))
            .collect();
        many.push(serde_json::json!("http://evil.example/resolve"));
        many.push(serde_json::json!("not a url"));
        many.push(serde_json::json!(42));
        let json = serde_json::to_string(&many).unwrap();
        assert_eq!(set_dynamic_endpoints(&json), MAX_DOH_ENDPOINTS);
        assert_eq!(effective_endpoints().len(), MAX_DOH_ENDPOINTS);
        assert!(
            effective_endpoints()
                .iter()
                .all(|e| e.url.starts_with("https://"))
        );
        // ecs 标志逐条保留。
        assert!(effective_endpoints()[0].ecs);
        assert!(!effective_endpoints()[1].ecs);
        // 纯字符串形式（向后兼容）→ ecs=false。
        assert_eq!(set_dynamic_endpoints(r#"["https://9.9.9.9/dns-query"]"#), 1);
        assert_eq!(
            effective_endpoints(),
            vec![ResolverEndpoint {
                url: "https://9.9.9.9/dns-query".into(),
                ecs: false
            }]
        );
        // 全非法 → 生效 0 条 → 回退内置 baseline。
        assert_eq!(set_dynamic_endpoints(r#"["http://a", "junk"]"#), 0);
        assert_eq!(effective_endpoints(), builtin_endpoints());
        // 整体解析失败 → 同样回退 baseline。
        assert_eq!(set_dynamic_endpoints("{broken"), 0);
        assert_eq!(effective_endpoints(), builtin_endpoints());
    }

    #[test]
    fn ecs_subnets_validated_and_capped() {
        assert_eq!(
            set_ecs_subnets(
                r#"["202.96.128.0/24","junk","1.2.3.4","10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","100.64.0.0/10"]"#
            ),
            MAX_ECS_SUBNETS,
            "非法条目丢弃后按上限截断"
        );
        assert_eq!(set_ecs_subnets("[]"), 0);
        assert_eq!(set_ecs_subnets("broken"), 0);
        // 边界：len 0 与 33 非法。
        assert_eq!(set_ecs_subnets(r#"["1.2.3.0/0","1.2.3.0/33"]"#), 0);
    }

    #[test]
    fn doh_json_parses_a_and_aaaa_ignores_cname() {
        let raw = r#"{
            "Status": 0,
            "Answer": [
                {"name":"example.com","type":5,"TTL":60,"data":"cdn.example.com."},
                {"name":"cdn.example.com","type":1,"TTL":60,"data":"93.184.216.34"},
                {"name":"cdn.example.com","type":28,"TTL":60,"data":"2606:2800:220:1:248:1893:25c8:1946"},
                {"name":"cdn.example.com","type":1,"TTL":60,"data":"not-an-ip"}
            ]
        }"#;
        let json: DohJson = serde_json::from_str(raw).unwrap();
        let ips = extract_ips(&json);
        assert_eq!(
            ips,
            vec![
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                IpAddr::V6(
                    "2606:2800:220:1:248:1893:25c8:1946"
                        .parse::<Ipv6Addr>()
                        .unwrap()
                ),
            ]
        );
    }

    #[test]
    fn doh_json_tolerates_missing_answer() {
        let json: DohJson = serde_json::from_str(r#"{"Status": 3}"#).unwrap();
        assert!(extract_ips(&json).is_empty());
    }
}
