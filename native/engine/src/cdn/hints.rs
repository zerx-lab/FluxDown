//! 云端众包 hints 消费（P2 客户端侧）：候选 IP 的**排序先验**。
//!
//! `GET {base}/api/v1/cdn/hints?host=` 是公开限频端点（k-匿名聚合数据，
//! 无个体信息），引擎用轻量 client 直接拉取——不需要云端会话，`base` 由
//! Dart 云服务在登录/拉到 cdn config 后写入 config 表 `cdn_hints_base`
//! （空 = 禁用，未登录/断云天然禁用）。
//!
//! 定位（方案 §5.4 原则）：hints **只影响候选排序**（热门节点排前，优先
//! 被 connect 预筛与早期租约探索），实测（预筛存活 + EWMA）永远覆盖先验；
//! 拉取失败/超时/禁用 → 排序保持原样，零功能影响。

use std::net::IpAddr;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::logger::log_info;

/// hints 拉取超时（与 resolver 单源预算同级，绝不拖慢聚合起飞）。
const FETCH_TIMEOUT: Duration = Duration::from_millis(1500);

/// host 级 hints 缓存 TTL（服务端聚合周期 30min，同级）。
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);

/// 云端 base origin（空 = 禁用）。
static HINTS_BASE: OnceLock<StdMutex<String>> = OnceLock::new();

fn hints_base() -> &'static StdMutex<String> {
    HINTS_BASE.get_or_init(|| StdMutex::new(String::new()))
}

/// host → (拉取时刻, hint IP 有序清单) 缓存。
type HintsCacheMap = std::collections::HashMap<String, (Instant, Vec<IpAddr>)>;

static HINTS_CACHE: OnceLock<StdMutex<HintsCacheMap>> = OnceLock::new();

fn hints_cache() -> &'static StdMutex<HintsCacheMap> {
    HINTS_CACHE.get_or_init(|| StdMutex::new(std::collections::HashMap::new()))
}

/// 设置云端 base（config `cdn_hints_base`）。仅接受 https（云端 origin 不
/// 允许明文）；空串 = 禁用。base 变化清空缓存。
pub fn set_base(base: &str) {
    let trimmed = base.trim().trim_end_matches('/');
    let valid = trimmed.is_empty()
        || reqwest::Url::parse(trimmed)
            .map(|u| u.scheme() == "https")
            .unwrap_or(false);
    let effective = if valid { trimmed } else { "" };
    if let Ok(mut b) = hints_base().lock()
        && *b != effective
    {
        *b = effective.to_string();
        if let Ok(mut cache) = hints_cache().lock() {
            cache.clear();
        }
        log_info!(
            "[cdn-hints] hints base 已更新（{}）",
            if effective.is_empty() {
                "禁用"
            } else {
                "启用"
            }
        );
    }
}

/// Engine 启动时从 config 表读回。
pub(crate) async fn load_base_from_config(db: &crate::db::Db) {
    if let Ok(Some(v)) = db.get_config("cdn_hints_base").await {
        set_base(&v);
    }
}

#[derive(Deserialize)]
struct HintsResponse {
    #[serde(default)]
    ips: Vec<HintEntry>,
}

#[derive(Deserialize)]
struct HintEntry {
    ip: String,
}

/// 拉取某 host 的 hints（有序，最优在前）。禁用/失败/超时 → 空。
pub(crate) async fn fetch_hints(host: &str) -> Vec<IpAddr> {
    let base = match hints_base().lock() {
        Ok(b) if !b.is_empty() => b.clone(),
        _ => return Vec::new(),
    };
    if let Ok(cache) = hints_cache().lock()
        && let Some((at, ips)) = cache.get(host)
        && at.elapsed() < CACHE_TTL
    {
        return ips.clone();
    }
    let Some(client) = super::resolver::light_client() else {
        return Vec::new();
    };
    let url = format!("{base}/api/v1/cdn/hints?host={host}");
    let fut = async {
        let resp = client
            .get(&url)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let parsed: HintsResponse = resp.json().await.ok()?;
        Some(
            parsed
                .ips
                .into_iter()
                .filter_map(|e| e.ip.parse::<IpAddr>().ok())
                .collect::<Vec<_>>(),
        )
    };
    let ips = match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
        Ok(Some(ips)) => ips,
        _ => Vec::new(),
    };
    if let Ok(mut cache) = hints_cache().lock() {
        cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        cache.insert(host.to_string(), (Instant::now(), ips.clone()));
    }
    if !ips.is_empty() {
        log_info!("[cdn-hints] host {} 拿到 {} 条云端 hints", host, ips.len());
    }
    ips
}

/// 按 hints 重排候选：命中 hints 的候选按 hints 顺序排前，其余保持原序
/// 排后。**不增删候选**——hints 只是排序先验，未被本地解析出的 hint IP
/// 不会被凭空引入（完整性锚只信 TLS + 本地聚合来源）。
pub(crate) fn order_by_hints(candidates: Vec<IpAddr>, hints: &[IpAddr]) -> Vec<IpAddr> {
    if hints.is_empty() || candidates.is_empty() {
        return candidates;
    }
    let mut hinted: Vec<IpAddr> = Vec::new();
    for h in hints {
        if candidates.contains(h) && !hinted.contains(h) {
            hinted.push(*h);
        }
    }
    let mut rest: Vec<IpAddr> = candidates
        .into_iter()
        .filter(|c| !hinted.contains(c))
        .collect();
    hinted.append(&mut rest);
    hinted
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{hints_base, order_by_hints, set_base};
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(9, 9, 9, n))
    }

    #[test]
    fn ordering_is_prior_only_never_adds_candidates() {
        let candidates = vec![ip(1), ip(2), ip(3)];
        // hint 含未解析出的 ip(9)：不得被引入；ip(3) 提前，其余保序。
        let ordered = order_by_hints(candidates, &[ip(9), ip(3)]);
        assert_eq!(ordered, vec![ip(3), ip(1), ip(2)]);
        // 空 hints → 原样。
        assert_eq!(order_by_hints(vec![ip(1), ip(2)], &[]), vec![ip(1), ip(2)]);
        // 重复 hint 去重。
        assert_eq!(
            order_by_hints(vec![ip(1), ip(2)], &[ip(2), ip(2)]),
            vec![ip(2), ip(1)]
        );
    }

    #[test]
    fn base_validation_rejects_plaintext() {
        set_base("https://cloud.example.com/");
        assert_eq!(*hints_base().lock().unwrap(), "https://cloud.example.com");
        set_base("http://cloud.example.com");
        assert_eq!(*hints_base().lock().unwrap(), "", "明文 base 必须被拒绝");
        set_base("");
        assert_eq!(*hints_base().lock().unwrap(), "");
    }
}
