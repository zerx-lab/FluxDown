//! `(host, ip)` 级 CDN 节点健康度持久化——完全照搬 `domain_conn_caps` 范式
//! （进程级内存缓存 + config 表 JSON 落盘 + 24h TTL + 版本标记整体丢弃重学）。
//!
//! 两类观察：
//! - **节点吞吐 EWMA**（`bps`）：worker 段完成时由 [`super::node_pool::NodePool::report`]
//!   喂入，跨任务/跨重启为同 `(host, ip)` 的节点选择提供先验；
//! - **聚合熔断标记**（`na`）：单任务内被踢节点数超过存活节点数时写入，
//!   24h 内该 host 不再尝试多节点聚合（§3.5 反例记忆）。
//!
//! 学习数据是可再生的性能缓存——版本不匹配 / 过期 / 解析失败一律丢弃重学，
//! 绝不迁移、绝不影响下载正确性。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::logger::log_info;

/// config 表 key。
const HEALTH_CONFIG_KEY: &str = "cdn_node_health";

/// 持久化格式版本。语义规则变化时递增——旧版本数据加载时整体丢弃重学。
const HEALTH_FORMAT_VERSION: u32 = 1;

/// 观察 TTL：与 `domain_conn_caps` 一致的 24h——CDN 调度/节点池随时变化，
/// 过期观察不再可信。
const HEALTH_TTL: Duration = Duration::from_secs(24 * 3600);

/// 容量上限（prune-on-save）：512 host × 16 ip。超限时按时间戳淘汰最旧条目。
const MAX_HOSTS: usize = 512;
const MAX_IPS_PER_HOST: usize = 16;

/// 成功回报的最小落盘间隔（秒）。段完成事件高频（64 段任务每几秒一个），
/// 而学习数据可再生——丢最后几秒观察无害，无需逐次落盘。
/// 熔断标记（低频、语义重要）不受此限，逐次落盘。
const PERSIST_MIN_GAP_SECS: u64 = 5;

/// 单 IP 的健康观察：吞吐 EWMA（字节/秒）+ 记录时间戳（Unix 秒）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct IpHealth {
    bps: f64,
    ts: u64,
}

/// 单 host 的健康观察集合。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HostHealth {
    /// 聚合熔断标记（Unix 秒，0 = 无标记）。
    #[serde(default)]
    na: u64,
    #[serde(default)]
    ips: HashMap<String, IpHealth>,
}

/// 落盘格式：`{"v":1,"hosts":{...}}`。
#[derive(Serialize, Deserialize)]
struct HealthFile {
    v: u32,
    hosts: HashMap<String, HostHealth>,
}

static HEALTH: OnceLock<StdMutex<HashMap<String, HostHealth>>> = OnceLock::new();

/// 上次成功落盘的 Unix 秒（成功回报的落盘 debounce）。
static LAST_PERSIST_SECS: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static StdMutex<HashMap<String, HostHealth>> {
    HEALTH.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// 当前 Unix 秒（病态时钟回退为 0，仅影响 TTL 判定的保守性）。
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 时间戳是否仍在 TTL 内。
fn fresh(recorded_secs: u64, now_secs: u64) -> bool {
    recorded_secs > 0 && now_secs.saturating_sub(recorded_secs) < HEALTH_TTL.as_secs()
}

/// 就地清除过期观察 + 容量裁剪（淘汰最旧）。返回是否有条目被移除。
fn prune(map: &mut HashMap<String, HostHealth>, now: u64) {
    for entry in map.values_mut() {
        if !fresh(entry.na, now) {
            entry.na = 0;
        }
        entry.ips.retain(|_, h| fresh(h.ts, now));
        if entry.ips.len() > MAX_IPS_PER_HOST {
            let mut ts_sorted: Vec<u64> = entry.ips.values().map(|h| h.ts).collect();
            ts_sorted.sort_unstable();
            // 淘汰时间戳最旧的多余条目：保留 ts 排名前 MAX_IPS_PER_HOST 的。
            let cutoff = ts_sorted[ts_sorted.len() - MAX_IPS_PER_HOST];
            let mut kept = 0usize;
            entry.ips.retain(|_, h| {
                if h.ts >= cutoff && kept < MAX_IPS_PER_HOST {
                    kept += 1;
                    true
                } else {
                    false
                }
            });
        }
    }
    map.retain(|_, e| e.na != 0 || !e.ips.is_empty());
    if map.len() > MAX_HOSTS {
        // host 级淘汰：按该 host 最新观察时间排序，保留最新 MAX_HOSTS 个。
        let newest = |e: &HostHealth| e.ips.values().map(|h| h.ts).max().unwrap_or(e.na);
        let mut ts_sorted: Vec<u64> = map.values().map(newest).collect();
        ts_sorted.sort_unstable();
        let cutoff = ts_sorted[ts_sorted.len() - MAX_HOSTS];
        let mut kept = 0usize;
        map.retain(|_, e| {
            if newest(e) >= cutoff && kept < MAX_HOSTS {
                kept += 1;
                true
            } else {
                false
            }
        });
    }
}

/// Engine 启动时从 config 表读回持久化健康度（与 [`crate::segment_coordinator::load_domain_conn_caps`]
/// 同一生命周期点调用）。版本不匹配/解析失败 → 空表重学。
pub(crate) async fn load_cdn_health(db: &Db) {
    let raw = match db.get_config(HEALTH_CONFIG_KEY).await {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(e) => {
            log_info!("[cdn-health] 读取持久化健康度失败（忽略，重新学习）: {}", e);
            return;
        }
    };
    let parsed: HealthFile = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            log_info!("[cdn-health] 健康度数据解析失败（丢弃重学）: {}", e);
            return;
        }
    };
    if parsed.v != HEALTH_FORMAT_VERSION {
        log_info!(
            "[cdn-health] 健康度格式版本不匹配（{} != {}），整体丢弃重学",
            parsed.v,
            HEALTH_FORMAT_VERSION
        );
        return;
    }
    let now = now_unix_secs();
    let mut incoming = parsed.hosts;
    prune(&mut incoming, now);
    let loaded = incoming.len();
    if let Ok(mut map) = cache().lock() {
        // 启动早于任何下载，内存表通常为空；已有条目（理论竞态）以内存为准。
        for (host, entry) in incoming {
            map.entry(host).or_insert(entry);
        }
    }
    log_info!("[cdn-health] 已加载 {} 个 host 的节点健康度", loaded);
}

/// 查询某 `(host, ip)` 的持久化吞吐 EWMA（未记录或已过期 → None）。
pub(crate) fn lookup_ewma(host: &str, ip: IpAddr) -> Option<f64> {
    let now = now_unix_secs();
    let map = cache().lock().ok()?;
    let h = map.get(host)?.ips.get(&ip.to_string())?;
    fresh(h.ts, now).then_some(h.bps)
}

/// 记录某 `(host, ip)` 的最新吞吐 EWMA（NodePool 侧已完成 EWMA 合并，此处
/// 只存储）。落盘按 [`PERSIST_MIN_GAP_SECS`] debounce。
pub(crate) fn record_ewma(host: &str, ip: IpAddr, ewma_bps: f64, db: &Db) {
    let now = now_unix_secs();
    if let Ok(mut map) = cache().lock() {
        map.entry(host.to_string()).or_default().ips.insert(
            ip.to_string(),
            IpHealth {
                bps: ewma_bps,
                ts: now,
            },
        );
    }
    let last = LAST_PERSIST_SECS.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= PERSIST_MIN_GAP_SECS
        && LAST_PERSIST_SECS
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        persist(db);
    }
}

/// 某 host 是否被聚合熔断标记覆盖（且未过期）。
pub(crate) fn is_no_aggregate(host: &str) -> bool {
    let now = now_unix_secs();
    cache()
        .lock()
        .ok()
        .and_then(|map| map.get(host).map(|e| fresh(e.na, now)))
        .unwrap_or(false)
}

/// 写入聚合熔断标记（低频且语义重要，立即落盘）。
pub(crate) fn record_no_aggregate(host: &str, db: &Db) {
    let now = now_unix_secs();
    if let Ok(mut map) = cache().lock() {
        map.entry(host.to_string()).or_default().na = now;
    }
    log_info!(
        "[cdn-health] host {} 记录聚合熔断标记（24h 内不再多节点聚合）",
        host
    );
    persist(db);
}

/// 把当前缓存快照异步写回 config 表（fire-and-forget；顺带 prune）。
fn persist(db: &Db) {
    let snapshot = {
        let Ok(mut map) = cache().lock() else { return };
        prune(&mut map, now_unix_secs());
        map.clone()
    };
    let file = HealthFile {
        v: HEALTH_FORMAT_VERSION,
        hosts: snapshot,
    };
    let Ok(json) = serde_json::to_string(&file) else {
        return;
    };
    let db = db.clone();
    tokio::spawn(async move {
        if let Err(e) = db.set_config(HEALTH_CONFIG_KEY, &json).await {
            log_info!("[cdn-health] 健康度持久化失败（忽略）: {}", e);
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        HEALTH_TTL, HostHealth, IpHealth, MAX_IPS_PER_HOST, cache, fresh, is_no_aggregate,
        lookup_ewma, now_unix_secs, prune,
    };
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    #[test]
    fn lookup_respects_ttl() {
        let now = now_unix_secs();
        let host = "ttl-test.example";
        {
            let mut map = cache().lock().unwrap();
            let entry = map.entry(host.to_string()).or_default();
            entry.ips.insert(
                ip(1).to_string(),
                IpHealth {
                    bps: 1000.0,
                    ts: now,
                },
            );
            entry.ips.insert(
                ip(2).to_string(),
                IpHealth {
                    bps: 2000.0,
                    ts: now.saturating_sub(HEALTH_TTL.as_secs() + 10),
                },
            );
        }
        assert_eq!(lookup_ewma(host, ip(1)), Some(1000.0));
        assert_eq!(lookup_ewma(host, ip(2)), None, "过期观察不可见");
    }

    #[test]
    fn no_aggregate_flag_respects_ttl() {
        let now = now_unix_secs();
        {
            let mut map = cache().lock().unwrap();
            map.entry("na-fresh.example".to_string()).or_default().na = now;
            map.entry("na-stale.example".to_string()).or_default().na =
                now.saturating_sub(HEALTH_TTL.as_secs() + 10);
        }
        assert!(is_no_aggregate("na-fresh.example"));
        assert!(!is_no_aggregate("na-stale.example"));
        assert!(!is_no_aggregate("na-absent.example"));
    }

    #[test]
    fn prune_drops_stale_and_caps_ips() {
        let now = now_unix_secs();
        let mut map: HashMap<String, HostHealth> = HashMap::new();
        let entry = map.entry("prune.example".to_string()).or_default();
        // 1 个过期 + 超额新鲜条目。
        entry.ips.insert(
            "0.0.0.0".to_string(),
            IpHealth {
                bps: 1.0,
                ts: now.saturating_sub(HEALTH_TTL.as_secs() + 1),
            },
        );
        for n in 0..(MAX_IPS_PER_HOST + 4) {
            entry.ips.insert(
                format!("10.1.0.{n}"),
                IpHealth {
                    bps: 1.0,
                    ts: now - n as u64, // 时间戳递减，最旧的应被淘汰
                },
            );
        }
        prune(&mut map, now);
        let pruned = &map["prune.example"];
        assert!(pruned.ips.len() <= MAX_IPS_PER_HOST);
        assert!(!pruned.ips.contains_key("0.0.0.0"), "过期条目必须被清除");
    }

    #[test]
    fn fresh_boundary() {
        let now = 1_000_000u64;
        assert!(fresh(now, now));
        assert!(!fresh(0, now), "0 = 无记录");
        assert!(!fresh(now - HEALTH_TTL.as_secs(), now));
    }
}
