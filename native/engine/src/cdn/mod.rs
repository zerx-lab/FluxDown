//! Multi-CDN 多节点并发下载（P0：纯客户端）。
//!
//! 架构（`docs/multi-cdn-implementation-plan.md`）：多来源解析聚合候选 IP
//! （[`resolver`]）→ TCP connect 预筛 → 构造 [`node_pool::NodePool`]（node[0]
//! 恒为无钉定的 SYS client）→ segment coordinator 的 worker 按段租借节点，
//! 真实下载流量喂 EWMA 健康度（[`health`] 持久化）。
//!
//! ## 聚合启用前置条件（§3.2，全部满足才建多节点池）
//!
//! | 条件 | 依据 |
//! |---|---|
//! | 全局开关开启（默认关，实验性） | 灰度控制 |
//! | URL scheme == `https` 且 host 非 IP-literal | 完整性锚 = TLS 证书（§1.2 规则 2） |
//! | 任务未忽略 TLS 错误、未启用代理 | manager 侧折算进 [`CdnTaskInput::enabled`]（§1.2 规则 1 / §11-5） |
//! | Range 能力已验证（probe/插件担保/历史 206） | 排除配额型端点的 plain-GET 生命线与节点切换的交互 |
//! | `domain_conn_caps[host] != 1` | 已学习为单连接域名（FnOS/网盘类）不聚合 |
//! | 无聚合熔断标记（24h TTL） | 反例记忆（§3.5） |
//! | 去重候选 ≥2 且 connect 预筛存活 ≥2 | 单 IP 聚合无意义 |
//!
//! 任一不满足 → [`node_pool::NodePool::single`]，行为与现状逐字节一致。
//! 失败路径（解析全灭、预筛不足、任务 panic）同样退单节点池——多 CDN 是
//! 纯增益层，没有新的全局失败模式（不变量 1）。

pub mod health;
pub mod hints;
pub mod node_pool;
pub mod resolver;
pub mod telemetry;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;

use crate::db::Db;
use crate::downloader::DownloadError;
use crate::events::{EngineEvent, EventSink};
use crate::logger::log_info;
pub use node_pool::{ClientTemplate, NodeLease, NodePool};

/// `cdn_max_nodes` 的硬上限（设置面 clamp 同步此范围；`0` = 自动档，
/// 见 [`auto_max_nodes`]）。
pub const MAX_NODES_LIMIT: usize = 8;

/// TCP connect 预筛的单 IP 超时。
const CONNECT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 任务级聚合输入——manager 在任务起飞时按全局设置与任务上下文折算。
///
/// `enabled` 已折算三个 manager 侧条件：全局开关 && 任务/全局均无代理 &&
/// 未忽略 TLS 错误。引擎下载路径只需再校验 URL/Range/域名级条件。
#[derive(Clone, Default)]
pub struct CdnTaskInput {
    pub enabled: bool,
    /// 钉定节点数上限（SYS 兜底节点不计入）。**0 = 自动**：按文件大小与
    /// 并发连接数在 [`finish_pool`] 里经 [`auto_max_nodes`] 推导。
    pub max_nodes: usize,
    /// 云端下发的节点数上限（P1 config 拉取落 `cdn_cloud_max_nodes`；
    /// 0 = 云端未下发）。有效上限 = min(本地设置/自动值, 云端值)。
    pub cloud_max_nodes: usize,
    /// 任务解析后的有效 UA（任务 > 队列 > 全局），pinned client 与任务
    /// client 保持一致。
    pub user_agent: String,
}

/// 自动模式的钉定节点数推导（确定性先验；运行时优劣由 EWMA/踢除自适应）：
///
/// - **文件大小阶梯**：<32MB → 2、<256MB → 3、<1GB → 4、≥1GB → 6。小文件
///   段少、TLS 握手与 connect 预筛开销占比高，多节点边际收益低；大文件在
///   per-IP 限速 CDN 上节点数就是吞吐上限的分母（aria2#808 同理）。
/// - **不超过并发连接数**：节点只有被 worker 租用才产生流量，节点数 >
///   worker 数的部分永远闲置（`lease()` 的 cap 分散约束下也轮不到）。
/// - 恒 ≥2（<2 无聚合意义，调用方已保证候选 ≥2）、≤ [`MAX_NODES_LIMIT`]。
///
/// 与 aria2 `--uri-selector=adaptive`（ServerStat 反馈择优）同一思路：先验
/// 只定池子大小，优劣排序交给运行时实测。
fn auto_max_nodes(total_bytes: i64, segment_cap: i32) -> usize {
    let by_size: usize = if total_bytes < 32 * 1024 * 1024 {
        2
    } else if total_bytes < 256 * 1024 * 1024 {
        3
    } else if total_bytes < 1024 * 1024 * 1024 {
        4
    } else {
        6
    };
    by_size
        .min((segment_cap.max(2)) as usize)
        .clamp(2, MAX_NODES_LIMIT)
}

/// 后台聚合任务的产出：预筛存活节点 + 归因/诊断信息（喂 `TaskCdnEvent`）。
#[derive(Default)]
pub struct AggregationOutcome {
    /// connect 预筛存活的候选（按 hints 重排序、截断硬上限）。
    alive: Vec<IpAddr>,
    /// 每个候选 IP 的解析来源（`resolver::CandidateSet::origins`）。
    origins: std::collections::HashMap<IpAddr, String>,
    /// 去重候选 IP 总数（预筛前）。
    candidates: usize,
    /// 预筛存活总数（截断前）。
    alive_total: usize,
}

/// 后台候选聚合任务的句柄（与 probe 并行执行，不增加起飞延迟）。
pub struct PendingAggregation {
    handle: tokio::task::JoinHandle<AggregationOutcome>,
    host: String,
}

impl PendingAggregation {
    /// 单流路径等不再需要聚合结果时中止后台任务。
    pub fn abort(self) {
        self.handle.abort();
    }
}

/// 静态门控：URL/设置级条件（不含需要网络的候选数条件）。
/// 通过 → 返回 `(host, port)`。
fn aggregation_gates_open(
    url: &str,
    input: &CdnTaskInput,
    range_verified: bool,
) -> Option<(String, u16)> {
    if !input.enabled || !range_verified {
        return None;
    }
    let parsed = reqwest::Url::parse(url).ok()?;
    // 仅 https（§1.2 规则 2：明文 URL 钉到聚合 IP = 主动选择攻击者端点）。
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_string();
    // IP-literal host 无钉定意义（`.resolve()` 只对域名生效）。
    if host.parse::<IpAddr>().is_ok() || host.starts_with('[') {
        return None;
    }
    let port = parsed.port_or_known_default()?;
    // 已学习为单连接的域名（FnOS/网盘类）不聚合。
    if crate::segment_coordinator::domain_conn_cap(url) == Some(1) {
        return None;
    }
    // 聚合熔断反例记忆（24h TTL）。
    if health::is_no_aggregate(&host) {
        return None;
    }
    Some((host, port))
}

/// TCP connect 预筛：并发探测候选，保序返回存活者，并采遥测样本
/// （连接耗时/死活，P2）。死 IP（连不通/超时）被剔除；本机无 v6 路由时
/// AAAA 候选在此天然过滤。
async fn probe_alive(host: &str, ips: Vec<IpAddr>, port: u16, db: &Db) -> Vec<IpAddr> {
    let futs = ips.into_iter().map(|ip| async move {
        let started = std::time::Instant::now();
        let connect = tokio::net::TcpStream::connect(SocketAddr::new(ip, port));
        let ok = tokio::time::timeout(CONNECT_PROBE_TIMEOUT, connect)
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);
        (ip, ok, started.elapsed().as_millis() as u32)
    });
    let results = futures_util::future::join_all(futs).await;
    let mut alive = Vec::new();
    for (ip, ok, connect_ms) in results {
        telemetry::record_connect(host, ip, connect_ms, ok, db);
        if ok {
            alive.push(ip);
        }
    }
    alive
}

/// 任务起飞时发起候选聚合（解析 + hints 先验 + connect 预筛），与 meta
/// probe 并行。静态门控不通过 → `None`（调用方最终走 [`NodePool::single`]）。
pub fn spawn_aggregation(
    url: &str,
    input: &CdnTaskInput,
    range_verified: bool,
    task_id: &str,
    db: &Db,
) -> Option<PendingAggregation> {
    let (host, port) = aggregation_gates_open(url, input, range_verified)?;
    // 此处只按硬上限截断；有效上限（本地/自动/云端合并）在 finish_pool
    // 里裁剪——自动档需要 probe 后才知道的 total_bytes。
    let max_nodes = MAX_NODES_LIMIT;
    let task_id = task_id.to_string();
    let spawn_host = host.clone();
    let db = db.clone();
    let handle = tokio::spawn(async move {
        // 多来源解析与云端 hints（P2 排序先验）并发拉取。
        let (set, hint_ips) = futures_util::join!(
            resolver::resolve_candidates(&spawn_host, port),
            hints::fetch_hints(&spawn_host)
        );
        if set.ips.len() < 2 {
            return AggregationOutcome {
                candidates: set.ips.len(),
                ..Default::default()
            };
        }
        // hints 只重排（热门节点优先被预筛/早期租约探索），绝不增删候选。
        let candidates = set.ips.len();
        let ordered = hints::order_by_hints(set.ips, &hint_ips);
        let alive = probe_alive(&spawn_host, ordered, port, &db).await;
        log_info!(
            "[cdn] task {} host {} 候选预筛: {} 存活，取前 {}",
            task_id,
            spawn_host,
            alive.len(),
            max_nodes
        );
        let alive_total = alive.len();
        AggregationOutcome {
            alive: alive.into_iter().take(max_nodes).collect(),
            origins: set.origins,
            candidates,
            alive_total,
        }
    });
    Some(PendingAggregation { handle, host })
}

/// 多段路径起飞前收割聚合结果并构造节点池。
///
/// `total_bytes` / `segment_cap`：自动档（`input.max_nodes == 0`）的推导
/// 输入（probe 后的有效总大小与本次多段并发上限）。
/// 任何失败（任务 panic/中止、存活 <2）→ 单节点池（与现状等价）。
///
/// 事件（详情面板「日志」Tab）：聚合已发起（`pending` 非 None）时必有一条——
/// 成功 → `kind="pool"`（节点清单 + 来源归因 + 健康度先验）；
/// 失败 → `kind="fallback"`（候选/存活计数或聚合任务异常）。
/// 未发起聚合（门控不通过/开关关闭）→ 零事件，日志不产生噪音。
#[allow(clippy::too_many_arguments)]
pub async fn finish_pool(
    pending: Option<PendingAggregation>,
    task_client: &Client,
    input: &CdnTaskInput,
    db: &Db,
    task_id: &str,
    total_bytes: i64,
    segment_cap: i32,
    sink: &Arc<dyn EventSink>,
) -> Arc<NodePool> {
    let Some(PendingAggregation { handle, host }) = pending else {
        return NodePool::single(task_client.clone());
    };
    let fallback = |reason: &str, candidates: usize, alive: usize| EngineEvent::TaskCdnEvent {
        task_id: task_id.to_string(),
        kind: "fallback".to_string(),
        host: host.clone(),
        nodes: Vec::new(),
        ip: String::new(),
        reason: reason.to_string(),
        candidates: candidates as i32,
        alive: alive as i32,
        cap: 0,
        auto_cap: false,
    };
    let mut outcome = match handle.await {
        Ok(outcome) => outcome,
        Err(e) => {
            log_info!("[cdn] task {} 候选聚合任务异常（退单节点）: {}", task_id, e);
            sink.emit(fallback("error", 0, 0));
            return NodePool::single(task_client.clone());
        }
    };
    // 有效节点上限：本地设置（0 = 自动阶梯）与云端下发上限取更低。
    let auto = input.max_nodes == 0;
    let local_cap = if auto {
        auto_max_nodes(total_bytes, segment_cap)
    } else {
        input.max_nodes.min(MAX_NODES_LIMIT)
    };
    let cap = if input.cloud_max_nodes > 0 {
        local_cap.min(input.cloud_max_nodes)
    } else {
        local_cap
    };
    outcome.alive.truncate(cap.max(2));
    if outcome.alive.len() < 2 {
        log_info!(
            "[cdn] task {} host {} 存活候选不足（{} < 2），退单节点池",
            task_id,
            host,
            outcome.alive.len()
        );
        sink.emit(fallback("few", outcome.candidates, outcome.alive_total));
        return NodePool::single(task_client.clone());
    }
    log_info!(
        "[cdn] task {} host {} 多节点池就绪（上限 {}{}）: {:?} + SYS",
        task_id,
        host,
        cap.max(2),
        if auto { "，自动档" } else { "" },
        outcome.alive
    );
    // 就绪事件：每节点带来源归因与健康度先验（无先验 → 0，UI 侧隐藏）。
    let nodes: Vec<crate::model::CdnNodeInfo> = outcome
        .alive
        .iter()
        .map(|ip| crate::model::CdnNodeInfo {
            ip: ip.to_string(),
            origin: outcome.origins.get(ip).cloned().unwrap_or_default(),
            bytes: 0,
            ewma_bps: health::lookup_ewma(&host, *ip).unwrap_or(0.0) as i64,
            active: 0,
        })
        .collect();
    sink.emit(EngineEvent::TaskCdnEvent {
        task_id: task_id.to_string(),
        kind: "pool".to_string(),
        host: host.clone(),
        nodes,
        ip: String::new(),
        reason: String::new(),
        candidates: outcome.candidates as i32,
        alive: outcome.alive_total as i32,
        cap: cap.max(2) as i32,
        auto_cap: auto,
    });
    let template = ClientTemplate {
        // 聚合前置条件保证任务无代理——模板与任务 client 同为直连。
        proxy: crate::proxy_config::ProxyConfig::default(),
        user_agent: input.user_agent.clone(),
    };
    NodePool::multi(
        template,
        &host,
        outcome.alive,
        outcome.origins,
        task_client.clone(),
        db.clone(),
        task_id,
        Some(sink.clone()),
    )
}

/// 节点可归因错误判定（§3.5）：worker 在多节点池的**钉定**租约上，对这类
/// 错误在上报 `WorkerEvent::Failed` 前翻译为 [`DownloadError::CdnNodeFailed`]
/// （retryable）——协调器按既有回收语义把段重派，绝不把单节点的问题升级
/// 为任务失败。SYS 节点的错误保持原样（语义 == 现状）。
///
/// - `Request`：连接失败/超时/重置/HTTP 状态错误（含 403/429——若是主机级
///   连接数限制，所有节点将相继被踢、熔断退 SYS 后由既有链路学习）；
/// - `VersionChanged`：跨节点 validator 不一致（`first_validators` latch 对
///   所有 worker 共享，无论段来自哪个节点）——该节点内容不一致，立即踢除；
///   若为文件真实变更，所有节点相继被踢后 SYS 上的原样错误触发既有清盘回退；
/// - stall（`Other("segment N stalled: ...")`）：节点停滞。
pub(crate) fn is_node_attributable(e: &DownloadError) -> bool {
    match e {
        DownloadError::Request(_) => true,
        DownloadError::VersionChanged(_) => true,
        DownloadError::Other(msg) => msg.contains("stalled"),
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        CdnTaskInput, MAX_NODES_LIMIT, aggregation_gates_open, auto_max_nodes,
        is_node_attributable, probe_alive,
    };
    use crate::downloader::DownloadError;
    use std::net::{IpAddr, Ipv4Addr};

    fn enabled_input() -> CdnTaskInput {
        CdnTaskInput {
            enabled: true,
            max_nodes: 3,
            ..Default::default()
        }
    }

    #[test]
    fn gates_truth_table() {
        let input = enabled_input();
        // 通过：https + 域名 + range verified。
        assert_eq!(
            aggregation_gates_open("https://cdn.example.com/f.bin", &input, true),
            Some(("cdn.example.com".to_string(), 443))
        );
        // 明文 URL 拒绝。
        assert!(aggregation_gates_open("http://cdn.example.com/f.bin", &input, true).is_none());
        // IP-literal host 拒绝。
        assert!(aggregation_gates_open("https://93.184.216.34/f.bin", &input, true).is_none());
        assert!(aggregation_gates_open("https://[2606:2800::1]/f.bin", &input, true).is_none());
        // Range 未验证拒绝。
        assert!(aggregation_gates_open("https://cdn.example.com/f.bin", &input, false).is_none());
        // 开关关闭拒绝。
        let mut off = enabled_input();
        off.enabled = false;
        assert!(aggregation_gates_open("https://cdn.example.com/f.bin", &off, true).is_none());
        // 非法 URL 拒绝。
        assert!(aggregation_gates_open("not a url", &input, true).is_none());
    }

    #[test]
    fn gates_respect_single_conn_domain() {
        let host = "single-conn-cdn.example";
        let url = format!("https://{host}/f.bin");
        crate::segment_coordinator::record_domain_conn_cap(&url, 1);
        assert!(aggregation_gates_open(&url, &enabled_input(), true).is_none());
    }

    #[test]
    fn auto_ladder_by_size_and_segment_cap() {
        // 文件大小阶梯（并发充足时）。
        assert_eq!(auto_max_nodes(16 * 1024 * 1024, 16), 2);
        assert_eq!(auto_max_nodes(128 * 1024 * 1024, 16), 3);
        assert_eq!(auto_max_nodes(512 * 1024 * 1024, 16), 4);
        assert_eq!(auto_max_nodes(4 * 1024 * 1024 * 1024, 16), 6);
        // 并发连接数封顶（节点数 > worker 数的部分永远闲置）。
        assert_eq!(auto_max_nodes(4 * 1024 * 1024 * 1024, 3), 3);
        // 下限 2（含病态 segment_cap），上限 MAX_NODES_LIMIT。
        assert_eq!(auto_max_nodes(4 * 1024 * 1024 * 1024, 1), 2);
        assert_eq!(auto_max_nodes(0, 0), 2);
        assert!(auto_max_nodes(i64::MAX, i32::MAX) <= MAX_NODES_LIMIT);
    }

    #[test]
    fn node_attribution_classification() {
        assert!(is_node_attributable(&DownloadError::VersionChanged(
            "200 OK".into()
        )));
        assert!(is_node_attributable(&DownloadError::Other(
            "segment 3 stalled: no data received for 5s".into()
        )));
        // 非节点可归因：磁盘/取消/校验和等保持原语义。
        assert!(!is_node_attributable(&DownloadError::Cancelled));
        assert!(!is_node_attributable(&DownloadError::ChecksumMismatch(
            "x".into()
        )));
        assert!(!is_node_attributable(&DownloadError::Other(
            "something else".into()
        )));
        assert!(!is_node_attributable(&DownloadError::TrueSizeLarger(1)));
    }

    #[tokio::test]
    async fn probe_alive_filters_dead_candidates() {
        // 活端口：本地 listener；死端口：TEST-NET-1 保留地址（不可路由，
        // 依赖 2s 超时剔除）。遥测采样需要 Db：临时目录建库。
        let dir = std::env::temp_dir().join(format!("fluxdown_cdnprobe_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let alive_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let dead_ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let survivors = probe_alive("probe.example", vec![dead_ip, alive_ip], port, &db).await;
        assert_eq!(survivors, vec![alive_ip]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
