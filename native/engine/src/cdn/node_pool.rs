//! `NodePool` / `NodeLease`：多路径（直连 SYS / 多 CDN 钉定节点 / 候选代理 /
//! 多网卡额外链路）的连接租借池。
//!
//! 池内 node[0] 恒为 **SYS 节点**（任务 client：Auto 任务按起飞路径是直连
//! 或代理）——所有其它槽位被踢除后自动退到 SYS，任何情况下不比现状差
//! （方案不变量 1）。其余槽位三类：
//! - **钉定节点**（`ip = Some`）：直连路径上的多 CDN 候选 IP，懒建 client；
//! - **备选路径**（`ip = None`，`route` 与 SYS 不同）：`ProxyMode::Auto`
//!   的候选代理（或代理起飞任务的直连回路），构造时注入 client；
//! - **网卡链路**（`route = Link`）：多网卡聚合的额外出口，构造时注入绑定
//!   出口的 client。
//!
//! 调度（[`NodePool::lease_for`]）全部确定性、无随机，判据见
//! [`crate::path_scheduler`]：
//! - **探索**：无先验、未实测的冷路径在允许时各分 1 条连接，采样即真实
//!   下载，字节零丢弃；
//! - **竞争集**：单连接速率估计低于最优路径一半的槽位不参与派工；
//! - **分散**：竞争集内 per-节点并发上限 `cap = ceil((租约+1)/竞争集大小)`
//!   避免全部 worker 涌向当前最优节点（aria2#808：per-IP 限速下分散本身
//!   就是收益），上限内选 `score = 估计 × 0.5^连续失败数` 最高者。
//! - **独立容量均衡**（网卡链路）：链路之间不共享瓶颈，不适用竞争集。
//!   每条链路（主链路 = 全部非链路槽位）容量 = 单连接估计 × 上窗实测
//!   连接数，新租约给「容量 /（在途 + 1）」最高者（注水式分配，连接数随
//!   容量成比例）；容量占比 ≥ [`crate::path_scheduler::LINK_FLOOR_SHARE`]
//!   的空闲链路在可探索时保底 1 条连接。
//! - 踢除：非 SYS 槽位连续失败 ≥3 或 validator 不一致（立即）→ 本任务内
//!   不再选中；钉定节点跨任务由持久化健康度 TTL 衰减自然恢复；网卡链路
//!   记入 [`crate::multi_nic`] 近期失败记忆。
//!
//! 速率估计由 coordinator 每个 ramp 窗口以连接稳态样本喂入
//! （[`NodePool::observe_window`]）；段完成回报只为尚未被窗口实测的槽位
//! 提供初值。在途连接登记在活跃表（[`LiveConn`]），coordinator 据此做
//! 窗口采样与完成时间抢占（[`NodePool::preempt`]）。
//!
//! 聚合熔断（方案 §3.5）：任务内被踢钉定节点数 > 存活钉定节点数 → 判定该
//! host 不适合聚合，写 [`super::health`] 熔断标记（24h），本任务退 SYS 继续。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use reqwest::Client;
use tokio_util::sync::CancellationToken;

use crate::auto_proxy::{CandidateSource, RoutePath};
use crate::db::Db;
use crate::downloader::DownloadError;
use crate::events::{EngineEvent, EventSink};
use crate::logger::log_info;
use crate::model::SourceBytes;
use crate::multi_nic::LinkBinding;
use crate::proxy_config::ProxyConfig;

/// 段完成回报喂估计时的 EWMA 平滑系数（新样本权重）。
const EWMA_ALPHA: f64 = 0.3;

/// 参与段完成回报估计的最小段字节数——滤除微段噪声（尾部微拆分段最低
/// 64KB，其吞吐主要反映请求开销而非链路容量）。
const MIN_SAMPLE_BYTES: u64 = 256 * 1024;

/// 连续失败踢除阈值。
const KICK_STREAK: u32 = 3;

/// 每任务 SYS 传输失败改道配额。
const SYS_FALLBACK_BUDGET: u32 = 8;

/// 无任何健康度先验时钉定节点/SYS 的初始估计（4 MB/s，中位量级）：保证
/// 冷节点的初始 score 不为 0（可被探索），又不至于压过已被证明的快节点。
const DEFAULT_EWMA_BPS: f64 = 4.0 * 1024.0 * 1024.0;

/// `kind="leases"` 节点并发快照的最小发射间隔——租约借还极高频（每段一次），
/// 快照按"分布变化 + 节流"双门控，避免事件与日志刷屏。
const LEASES_EMIT_MIN_GAP: Duration = Duration::from_secs(2);

/// pinned client 的构建模板 = `build_client_with_tls_policy` 的参数基座。
/// `ignore_tls_errors` 恒为 false——聚合启用前置条件（方案 §3.2）保证忽略
/// TLS 错误的任务根本不会构造多节点池，故模板上不提供该自由度。
#[derive(Clone)]
pub struct ClientTemplate {
    pub proxy: ProxyConfig,
    pub user_agent: String,
}

/// 池内单节点。
struct NodeSlot {
    /// `Some` = 直连钉定 IP；`None` = SYS 或备选路径（hostname 路由）。
    ip: Option<IpAddr>,
    /// 槽位所属路径（直连 / 指定来源的代理）。
    route: RoutePath,
    /// 常驻 client（keep-alive 连接池挂在上面）。SYS 与备选路径构造时注入；
    /// 钉定节点懒建（首次被选中时 `build_pinned_client`）。
    client: Option<Client>,
    /// 单连接稳态速率估计（B/s）。
    ewma_bps: f64,
    /// 已吸收的窗口样本数（0 = 只有先验/默认值/段完成初值）。
    measured_windows: u32,
    /// 无先验且未实测：须先探索（至多 1 条在途连接）才能参与竞争。
    cold: bool,
    /// 估计来自本任务的真实传输（窗口稳态样本或 ≥256KB 的完成租约），
    /// 而非跨任务先验/默认值。只有这类估计能作为抢占的最优基准。
    evidence: bool,
    /// 本任务是否承载过任何租约（链路标签「已采样」判定，一经置位不回退）。
    ever_leased: bool,
    fail_streak: u32,
    kicked: bool,
    /// 当前未归还的租约数（lease 递增，NodeLease Drop 递减）。
    outstanding: u32,
    /// 候选来源标记（`resolver::CandidateSet::origins`；非钉定槽位为空串）。
    origin: String,
    /// 本任务经该节点实际传输的字节数（租约结束时由 worker 回报，含失败/
    /// 被抢占的租约；喂 `kind="summary"` 事件与主导路径判定）。
    bytes_done: u64,
    /// 网卡链路的出口绑定（非链路槽位为 `None`）。
    link: Option<Arc<LinkBinding>>,
    /// 最近一个有样本的窗口里该槽位的实测连接数（链路容量 = 估计 × 此值）。
    window_conns: u32,
}

impl NodeSlot {
    fn new(
        ip: Option<IpAddr>,
        route: RoutePath,
        client: Option<Client>,
        prior: Option<f64>,
    ) -> Self {
        Self {
            ip,
            route,
            client,
            ewma_bps: prior.unwrap_or(DEFAULT_EWMA_BPS),
            measured_windows: 0,
            cold: false,
            evidence: false,
            ever_leased: false,
            fail_streak: 0,
            kicked: false,
            outstanding: 0,
            origin: String::new(),
            bytes_done: 0,
            link: None,
            window_conns: 0,
        }
    }

    fn score(&self) -> f64 {
        self.ewma_bps * 0.5f64.powi(self.fail_streak.min(60) as i32)
    }

    /// 槽位承载的容量估计：单连接估计 × 上窗实测连接数（未实测按 1 条）。
    fn capacity(&self) -> f64 {
        self.score() * f64::from(self.window_conns.max(1))
    }
}

/// 一条在途连接（租约）的登记信息。
#[derive(Clone)]
pub struct LiveConn {
    pub lease_id: u64,
    pub node_id: usize,
    /// 租约服务的分段索引（coordinator 坐标）。
    pub seg_index: i32,
    /// 租约开始时该段的已下字节（段内相对）；在途传输量 = 当前 − 该值。
    pub start_downloaded: i64,
    /// 租约开始时刻；首次窗口采样以此计算实际经过的时间。
    pub started_at: Instant,
    cancel: CancellationToken,
}

struct PoolInner {
    slots: Vec<NodeSlot>,
    /// 聚合熔断只记一次。
    no_aggregate_recorded: bool,
    /// 上次 `kind="leases"` 快照的发射时刻（节流窗口）。
    last_leases_emit: Option<Instant>,
    /// 上次已发射快照的各槽位租约数（变化检测；与 `slots` 等长或为空）。
    last_leases_sig: Vec<u32>,
    /// 在途租约登记（lease_id → 连接）。
    live: HashMap<u64, LiveConn>,
    /// 因 validator 不一致被踢除的代理路径（coordinator 取走后记 NoSwitch）。
    validator_kicked: Vec<RoutePath>,
    /// 待写入跨任务路径健康度的 Auto 路径传输失败（coordinator 取走）。
    path_failures: Vec<RoutePath>,
    /// 本任务已登记过传输失败的路径（同一路径只记一次，避免反复写盘）。
    path_failures_noted: Vec<RoutePath>,
    /// 无 sink 的池挂了网卡链路时暂存的节点事件（coordinator 每窗转发）。
    deferred: Vec<EngineEvent>,
}

impl PoolInner {
    /// 登记一条 Auto 路径的传输失败（直连 / 代理；网卡链路不属于 Auto 路径）。
    /// 同一路径整个任务只登记一次：跨任务健康度写盘是同步落库，不能随每个
    /// 失败段反复触发。
    fn note_path_failure(&mut self, route: RoutePath) {
        if route.is_link() || self.path_failures_noted.contains(&route) {
            return;
        }
        self.path_failures_noted.push(route);
        self.path_failures.push(route);
    }
}

/// 多路径节点池。见模块文档。
pub struct NodePool {
    /// 钉定目标 host（单节点池为空串）。
    host: String,
    template: Option<ClientTemplate>,
    /// 健康度/熔断持久化句柄（单节点池为 None）。
    db: Option<Db>,
    /// 所属任务（`TaskCdnEvent` 归属；单节点池为空串）。
    task_id: String,
    /// 引擎事件接收端（踢除/熔断事件；单节点池为 None，零事件）。
    sink: Option<Arc<dyn EventSink>>,
    /// 是否允许探索冷路径（coordinator 按限速器节流/串行/连接敏感等守卫每窗更新）。
    explore: AtomicBool,
    next_lease_id: AtomicU64,
    /// 剩余 SYS 传输失败改道次数（见 [`Self::try_sys_transport_fallback`]）。
    sys_fallback_budget: AtomicU32,
    inner: StdMutex<PoolInner>,
}

/// 一次租借请求。
pub struct LeaseRequest {
    /// 本租约服务的分段索引。
    pub seg_index: i32,
    /// 租约开始时该段的已下字节（段内相对）。
    pub start_downloaded: i64,
    /// 本租约计划传输的字节数。冷路径只在足以进入稳态（≥
    /// [`crate::path_scheduler::EXPLORE_MIN_PIECE`]）的工作上探索。
    pub bytes: i64,
    /// 是否允许落到起飞路径之外的备选路径。开放式首段 / 无 Range 的 plain
    /// GET 必须留在起飞路径（配额型端点的唯一生命线不能交给未验证的路径）。
    pub allow_alternates: bool,
    /// 本租约的取消令牌（任务级令牌的子令牌）；[`NodePool::preempt`] 取消它。
    pub cancel: CancellationToken,
}

/// 一次段派工的节点租约。持有期间计入节点并发；Drop 归还。
/// [`NodePool::report`] 只负责健康度回报，不承担归还职责——两者解耦使
/// 取消/异常路径（不回报直接 drop）也绝不泄漏并发额度。
pub struct NodeLease {
    pool: Arc<NodePool>,
    node_id: usize,
    lease_id: u64,
    client: Client,
    ip: Option<IpAddr>,
    route: RoutePath,
    link: Option<Arc<LinkBinding>>,
}

impl NodeLease {
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// 是否直连钉定节点。
    pub fn is_pinned(&self) -> bool {
        self.ip.is_some()
    }

    /// 是否非 SYS 槽位（钉定节点或备选路径）：其节点可归因错误翻译为
    /// `CdnNodeFailed`（回收重派），段内重试预算收紧。
    pub fn is_attributable(&self) -> bool {
        self.node_id != 0
    }

    /// 租约所属路径。
    pub fn route(&self) -> RoutePath {
        self.route
    }

    /// 诊断用节点描述。
    pub fn describe(&self) -> String {
        slot_label(self.node_id, self.ip, self.route, self.link.as_deref())
    }
}

impl Drop for NodeLease {
    fn drop(&mut self) {
        let evt = {
            let Ok(mut inner) = self.pool.inner.lock() else {
                return;
            };
            inner.live.remove(&self.lease_id);
            if let Some(slot) = inner.slots.get_mut(self.node_id) {
                slot.outstanding = slot.outstanding.saturating_sub(1);
            }
            self.pool.leases_event_locked(&mut inner)
        };
        // 锁外发射（EventSink 契约）。
        if let Some(evt) = evt {
            self.pool.emit_all(vec![evt]);
        }
    }
}

/// 槽位的诊断/事件标签：钉定 IP、`PROXY:manual|system`、`NIC:<网卡名>`、
/// `DIRECT`（代理起飞任务的直连回路）或 `SYS`。
fn slot_label(
    node_id: usize,
    ip: Option<IpAddr>,
    route: RoutePath,
    link: Option<&LinkBinding>,
) -> String {
    match (ip, route) {
        (Some(ip), _) => ip.to_string(),
        (None, _) if node_id == 0 => "SYS".to_string(),
        (None, RoutePath::Proxy(CandidateSource::ManualFields)) => "PROXY:manual".to_string(),
        (None, RoutePath::Proxy(CandidateSource::System)) => "PROXY:system".to_string(),
        (None, RoutePath::Link(index)) => {
            link.map_or_else(|| format!("NIC:#{index}"), LinkBinding::label)
        }
        (None, RoutePath::Direct) => "DIRECT".to_string(),
    }
}

impl NodePool {
    fn build(
        host: &str,
        template: Option<ClientTemplate>,
        db: Option<Db>,
        task_id: &str,
        sink: Option<Arc<dyn EventSink>>,
        slots: Vec<NodeSlot>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host: host.to_string(),
            template,
            db,
            task_id: task_id.to_string(),
            sink,
            explore: AtomicBool::new(false),
            next_lease_id: AtomicU64::new(1),
            sys_fallback_budget: AtomicU32::new(SYS_FALLBACK_BUDGET),
            inner: StdMutex::new(PoolInner {
                slots,
                no_aggregate_recorded: false,
                last_leases_emit: None,
                last_leases_sig: Vec::new(),
                live: HashMap::new(),
                validator_kicked: Vec::new(),
                path_failures: Vec::new(),
                path_failures_noted: Vec::new(),
                deferred: Vec::new(),
            }),
        })
    }

    /// 单节点退化：包裹现有任务 client，无钉定，行为 == 现状（零事件）。
    pub fn single(client: Client) -> Arc<Self> {
        Self::build(
            "",
            None,
            None,
            "",
            None,
            vec![NodeSlot::new(None, RoutePath::Direct, Some(client), None)],
        )
    }

    /// SYS 槽位（node[0]）的任务 client：完成后内容审计等不属于任何分段租约的
    /// 一次性请求使用，不占用租约/不影响调度统计。
    pub fn sys_client(&self) -> Option<Client> {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.slots.first().and_then(|slot| slot.client.clone())
    }

    /// 多节点池：node[0] 恒为 SYS（`task_client`），`candidates` 每 IP 一个
    /// 懒建 pinned client 槽位。初始估计取持久化健康度（fresh）或冷启动
    /// 中位初值。`origins` = 每 IP 的解析来源归因（缺失 → 空串）；
    /// `task_id`/`sink` 供踢除/熔断 `TaskCdnEvent` 上报（sink=None 零事件）。
    #[allow(clippy::too_many_arguments)]
    pub fn multi(
        template: ClientTemplate,
        host: &str,
        candidates: Vec<IpAddr>,
        origins: HashMap<IpAddr, String>,
        task_client: Client,
        db: Db,
        task_id: &str,
        sink: Option<Arc<dyn EventSink>>,
    ) -> Arc<Self> {
        let mut slots = vec![NodeSlot::new(
            None,
            RoutePath::Direct,
            Some(task_client),
            None,
        )];
        for ip in candidates {
            let prior = super::health::lookup_ewma(host, ip);
            let mut slot = NodeSlot::new(Some(ip), RoutePath::Direct, None, prior);
            slot.origin = origins.get(&ip).cloned().unwrap_or_default();
            slots.push(slot);
        }
        Self::build(host, Some(template), Some(db), task_id, sink, slots)
    }

    /// `ProxyMode::Auto`：声明 SYS 槽位的起飞路径与先验，并挂入备选路径
    /// （每条路径一个预建 client）。先验为 `None` 的备选路径是冷路径，须经
    /// 探索（1 条真实分段连接）实测后才参与竞争。
    pub fn add_paths(
        &self,
        sys_route: RoutePath,
        sys_prior: Option<f64>,
        alternates: Vec<(RoutePath, Client, Option<f64>)>,
    ) {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(sys) = inner.slots.first_mut() {
            sys.route = sys_route;
            if let Some(prior) = sys_prior {
                sys.ewma_bps = prior;
            }
        }
        for (route, client, prior) in alternates {
            let mut slot = NodeSlot::new(None, route, Some(client), prior);
            slot.cold = prior.is_none();
            inner.slots.push(slot);
        }
    }

    /// 多网卡聚合：挂入额外网卡链路（每条一个预建的绑定出口 client）。链路
    /// 均为冷路径，经探索（1 条真实分段连接）实测后按独立容量参与均衡。
    /// 同一网卡重复挂入被忽略。
    pub fn add_links(&self, links: Vec<(Arc<LinkBinding>, Client)>) {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        for (link, client) in links {
            let route = RoutePath::Link(link.index);
            if inner.slots.iter().any(|s| s.route == route) {
                continue;
            }
            let mut slot = NodeSlot::new(None, route, Some(client), None);
            slot.cold = true;
            slot.origin = link.local_ip().map(|ip| ip.to_string()).unwrap_or_default();
            slot.link = Some(link);
            inner.slots.push(slot);
        }
    }

    /// 网卡链路槽位的在途连接数（非链路槽位为 `None`）。抢占守卫据此保证
    /// 链路至少留 1 条非停滞连接：链路承载独立容量，慢于最优单连接不是
    /// 交出最后一条连接的理由。
    pub fn link_outstanding(&self, node_id: usize) -> Option<u32> {
        self.inner.lock().ok().and_then(|inner| {
            inner
                .slots
                .get(node_id)
                .filter(|s| s.route.is_link())
                .map(|s| s.outstanding)
        })
    }

    /// 是否多节点池（含 ≥1 个钉定或备选槽位，无论是否已被踢）。
    pub fn is_multi(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.slots.len() > 1)
            .unwrap_or(false)
    }

    /// 存活槽位数 > 1（存在可调度的多条路径/节点）。
    pub fn is_multipath(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.slots.iter().filter(|s| !s.kicked).count() > 1)
            .unwrap_or(false)
    }

    /// 存活钉定节点数（诊断/测试用）。
    pub fn alive_pinned(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .slots
                    .iter()
                    .filter(|s| s.ip.is_some() && !s.kicked)
                    .count()
            })
            .unwrap_or(0)
    }

    /// SYS 路径传输层失败的改道配额：仍有与 SYS 不同路径的存活槽位（Auto
    /// 多路径）且本任务配额未耗尽时消耗一次并返回 `true`——该失败回收重派，
    /// 由其它路径接手，不让整个任务失败。配额有界，保证整体断网时失败仍会
    /// 按原语义上抛（进而触发 manager 的 failover/通用重试）。授予改道的同时
    /// 登记 SYS 路径的传输失败（同一路径整个任务只登记一次）。
    pub fn try_sys_transport_fallback(&self) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let start = inner.slots.first().map_or(RoutePath::Direct, |s| s.route);
        if !Self::alternate_alive(&inner, start) {
            return false;
        }
        if self
            .sys_fallback_budget
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_err()
        {
            return false;
        }
        inner.note_path_failure(start);
        true
    }

    /// SYS 路径的传输失败当前是否仍有改道可用：存在与 SYS 不同路径的存活槽位
    /// 且改道配额未耗尽（只读，不消耗配额）。coordinator 据此对 SYS 租约的
    /// **传输层失败**收紧段内重试预算——失败可由节点池改道时不必在已坏的路径上
    /// 耗时间重试；HTTP 类瞬时错误换路修不好，仍按原预算重试。
    pub fn sys_fallback_available(&self) -> bool {
        self.sys_fallback_budget.load(Ordering::Relaxed) > 0
            && self
                .inner
                .lock()
                .map(|inner| {
                    let start = inner.slots.first().map_or(RoutePath::Direct, |s| s.route);
                    Self::alternate_alive(&inner, start)
                })
                .unwrap_or(false)
    }

    fn alternate_alive(inner: &PoolInner, start: RoutePath) -> bool {
        inner.slots.iter().any(|s| !s.kicked && s.route != start)
    }

    /// 允许/禁止探索冷路径（coordinator 每窗按守卫更新）。
    pub fn set_explore(&self, enabled: bool) {
        self.explore.store(enabled, Ordering::Relaxed);
    }

    /// 是否存在可探索的冷路径（未踢、无在途连接）。coordinator 据此为其
    /// 额外放出 1 个 worker，避免等待既有连接完成分段才轮到探索。
    pub fn has_idle_cold_path(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .slots
                    .iter()
                    .any(|s| s.cold && !s.kicked && s.outstanding == 0)
            })
            .unwrap_or(false)
    }

    /// 永不阻塞、永不失败地租借一个节点。
    ///
    /// 钉定 client 懒建失败（极罕见的 builder 错误）→ 该节点按踢除处理并
    /// 继续选择；全部其它槽位不可用 → SYS 兜底。
    pub fn lease_for(self: &Arc<Self>, req: LeaseRequest) -> NodeLease {
        let explore = self.explore.load(Ordering::Relaxed)
            && req.allow_alternates
            && req.bytes >= crate::path_scheduler::EXPLORE_MIN_PIECE;
        // 踢除/熔断事件在锁外发射（EventSink 契约不要求可重入，caller 侧
        // 也不应在持锁时调用外部代码）。
        let mut events: Vec<EngineEvent> = Vec::new();
        let lease = {
            let mut inner = match self.inner.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            let (node_id, client) = loop {
                let chosen = Self::pick(&inner.slots, req.allow_alternates, explore);
                let slot = &mut inner.slots[chosen];
                // 懒建 pinned client（SYS 与备选路径的 client 恒存在）。
                if slot.client.is_none() {
                    let built = self.template.as_ref().and_then(|t| {
                        slot.ip.map(|ip| {
                            crate::downloader::build_pinned_client(
                                &t.proxy,
                                &t.user_agent,
                                false,
                                &self.host,
                                ip,
                            )
                        })
                    });
                    match built {
                        Some(Ok(client)) => slot.client = Some(client),
                        _ => {
                            log_info!(
                                "[cdn-pool] host {} 节点 {:?} pinned client 构建失败，踢除",
                                self.host,
                                slot.ip
                            );
                            slot.kicked = true;
                            if let Some(ip) = slot.ip {
                                events.push(self.kick_event(ip.to_string(), "build", 0));
                            }
                            if let Some(evt) = self.check_breaker(&mut inner) {
                                events.push(evt);
                            }
                            continue;
                        }
                    }
                }
                let slot = &mut inner.slots[chosen];
                let Some(client) = slot.client.clone() else {
                    // 不可达（上方已保证）；防御性踢除后重选。
                    slot.kicked = chosen != 0;
                    continue;
                };
                slot.outstanding += 1;
                slot.ever_leased = true;
                break (chosen, client);
            };
            let lease_id = self.next_lease_id.fetch_add(1, Ordering::Relaxed);
            inner.live.insert(
                lease_id,
                LiveConn {
                    lease_id,
                    node_id,
                    seg_index: req.seg_index,
                    start_downloaded: req.start_downloaded,
                    started_at: Instant::now(),
                    cancel: req.cancel,
                },
            );
            let slot = &inner.slots[node_id];
            let lease = NodeLease {
                pool: self.clone(),
                node_id,
                lease_id,
                client,
                ip: slot.ip,
                route: slot.route,
                link: slot.link.clone(),
            };
            // 节点并发分布快照（节流 + 变化检测；详情面板「日志」Tab）。
            if let Some(evt) = self.leases_event_locked(&mut inner) {
                events.push(evt);
            }
            lease
        };
        self.emit_all(events);
        lease
    }

    /// 测试便捷入口：允许备选路径、无分段归属、独立取消令牌。
    #[cfg(test)]
    fn lease(self: &Arc<Self>) -> NodeLease {
        self.lease_for(LeaseRequest {
            seg_index: -1,
            start_downloaded: 0,
            bytes: i64::MAX,
            allow_alternates: true,
            cancel: CancellationToken::new(),
        })
    }

    /// `kind="leases"` 节点并发快照（持锁调用，事件由调用方在锁外发射）。
    ///
    /// 门控：多节点池且有事件出口（sink 或网卡链路的暂存转发）；各槽位未
    /// 归还租约数相对上次已发射快照发生变化；距上次发射 ≥
    /// [`LEASES_EMIT_MIN_GAP`]。载荷 `nodes` 只含参与中的槽位（未被踢，或
    /// 虽被踢但仍有在途租约），`active` = 当前未归还租约数，`bytes`/
    /// `ewma_bps` 为该节点截至目前的累计与估计。
    fn leases_event_locked(&self, inner: &mut PoolInner) -> Option<EngineEvent> {
        if inner.slots.len() < 2 || (self.sink.is_none() && !Self::has_links(&inner.slots)) {
            return None;
        }
        let sig: Vec<u32> = inner.slots.iter().map(|s| s.outstanding).collect();
        if sig == inner.last_leases_sig {
            return None;
        }
        if let Some(last) = inner.last_leases_emit
            && last.elapsed() < LEASES_EMIT_MIN_GAP
        {
            return None;
        }
        inner.last_leases_emit = Some(Instant::now());
        inner.last_leases_sig = sig;
        let nodes: Vec<crate::model::CdnNodeInfo> = inner
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.kicked || s.outstanding > 0)
            .map(|(i, s)| Self::node_info(i, s))
            .collect();
        log_info!(
            "[cdn-pool] task {} host {} 节点并发快照: {}",
            self.task_id,
            self.host,
            nodes
                .iter()
                .map(|n| format!("{}×{}", n.ip, n.active))
                .collect::<Vec<_>>()
                .join(" ")
        );
        Some(EngineEvent::TaskCdnEvent {
            task_id: self.task_id.clone(),
            kind: "leases".to_string(),
            host: self.host.clone(),
            nodes,
            ip: String::new(),
            reason: String::new(),
            candidates: 0,
            alive: 0,
            cap: 0,
            auto_cap: false,
        })
    }

    fn node_info(i: usize, s: &NodeSlot) -> crate::model::CdnNodeInfo {
        crate::model::CdnNodeInfo {
            ip: slot_label(i, s.ip, s.route, s.link.as_deref()),
            origin: s.origin.clone(),
            bytes: s.bytes_done.min(i64::MAX as u64) as i64,
            ewma_bps: s.ewma_bps as i64,
            active: s.outstanding.min(i32::MAX as u32) as i32,
        }
    }

    /// 构造一条 `kind="kick"` 事件（`label` = 节点标签，见 `slot_label`；
    /// `count` = 连续失败次数，validator/build 路径为 0）。
    fn kick_event(&self, label: String, reason: &str, count: u32) -> EngineEvent {
        EngineEvent::TaskCdnEvent {
            task_id: self.task_id.clone(),
            kind: "kick".to_string(),
            host: self.host.clone(),
            nodes: Vec::new(),
            ip: label,
            reason: reason.to_string(),
            candidates: count as i32,
            alive: 0,
            cap: 0,
            auto_cap: false,
        }
    }

    fn has_links(slots: &[NodeSlot]) -> bool {
        slots.iter().any(|s| s.route.is_link())
    }

    /// 锁外批量发射。无 sink 的池（单节点池）挂了网卡链路时暂存，由
    /// coordinator 每窗经 [`Self::take_deferred_events`] 补齐归属后转发；
    /// 其余无 sink 的池静默丢弃（行为与现状一致）。
    fn emit_all(&self, events: Vec<EngineEvent>) {
        if events.is_empty() {
            return;
        }
        if let Some(sink) = &self.sink {
            for evt in events {
                sink.emit(evt);
            }
            return;
        }
        if let Ok(mut inner) = self.inner.lock()
            && Self::has_links(&inner.slots)
        {
            inner.deferred.extend(events);
        }
    }

    /// 取走暂存的节点事件（见 [`Self::emit_all`]）。
    pub fn take_deferred_events(&self) -> Vec<EngineEvent> {
        self.inner
            .lock()
            .map(|mut inner| std::mem::take(&mut inner.deferred))
            .unwrap_or_default()
    }

    /// 确定性选择（判据见模块文档）：
    /// 1. 探索：允许时，首个无在途连接的冷路径（含冷网卡链路）；
    /// 2. 主链路内（全部非链路槽位）：竞争集 + 分散，见 [`Self::pick_shared`]；
    /// 3. 存在网卡链路时再做跨链路独立容量均衡，见 [`Self::pick_across_links`]。
    ///
    /// `allow_alternates = false` 时只在与 SYS 同路径的槽位（SYS + 钉定）内
    /// 选择。SYS 永不被踢、永不为冷，保证恒有解。
    fn pick(slots: &[NodeSlot], allow_alternates: bool, explore: bool) -> usize {
        let start_route = slots.first().map_or(RoutePath::Direct, |s| s.route);
        let usable: Vec<usize> = (0..slots.len())
            .filter(|&i| {
                i == 0 || (!slots[i].kicked && (allow_alternates || slots[i].route == start_route))
            })
            .collect();
        if explore
            && let Some(&cold) = usable
                .iter()
                .find(|&&i| slots[i].cold && slots[i].outstanding == 0)
        {
            return cold;
        }
        let (links, shared): (Vec<usize>, Vec<usize>) = usable
            .into_iter()
            .filter(|&i| !slots[i].cold)
            .partition(|&i| slots[i].route.is_link());
        let primary_capacity: f64 = shared.iter().map(|&i| slots[i].capacity()).sum();
        let primary_outstanding: u32 = shared.iter().map(|&i| slots[i].outstanding).sum();
        let chosen = Self::pick_shared(slots, shared);
        if links.is_empty() {
            return chosen;
        }
        Self::pick_across_links(
            slots,
            chosen,
            (primary_capacity, primary_outstanding),
            &links,
            explore,
        )
    }

    /// 主链路内选择：
    /// 1. 竞争集：`known` 中 score ≥ 最优 × `COMPETITIVE_RATIO` 者；
    /// 2. 分散：竞争集内租约数低于 `cap = ceil((竞争集租约+1)/竞争集大小)`
    ///    者按 score 取最高（同分 → 租约更少 → 编号更小）。
    fn pick_shared(slots: &[NodeSlot], known: Vec<usize>) -> usize {
        let best = known
            .iter()
            .map(|&i| slots[i].ewma_bps)
            .fold(0.0_f64, f64::max);
        let competitive: Vec<usize> = known
            .into_iter()
            .filter(|&i| crate::path_scheduler::is_competitive(slots[i].ewma_bps, best))
            .collect();
        if competitive.is_empty() {
            return 0;
        }
        let total_outstanding: u32 = competitive.iter().map(|&i| slots[i].outstanding).sum();
        let cap = (total_outstanding + 1).div_ceil(competitive.len() as u32);
        let under_cap: Vec<usize> = competitive
            .iter()
            .copied()
            .filter(|&i| slots[i].outstanding < cap)
            .collect();
        let pool = if under_cap.is_empty() {
            competitive
        } else {
            under_cap
        };
        let mut chosen = pool[0];
        for &i in &pool[1..] {
            let (a, b) = (&slots[i], &slots[chosen]);
            let better = a.score() > b.score()
                || (a.score() == b.score()
                    && (a.outstanding < b.outstanding
                        || (a.outstanding == b.outstanding && i < chosen)));
            if better {
                chosen = i;
            }
        }
        chosen
    }

    /// 跨网卡链路的独立容量均衡（注水式）：
    /// 1. 保底：可探索时，容量占比 ≥ [`crate::path_scheduler::LINK_FLOOR_SHARE`]
    ///    且无在途连接的链路先拿 1 条（保持其估计新鲜，防被永久饿死）；
    /// 2. 其余按边际单连接估计 [`crate::path_scheduler::link_marginal`] 取最高，
    ///    同值留在主链路（`shared_choice`）。
    fn pick_across_links(
        slots: &[NodeSlot],
        shared_choice: usize,
        (primary_capacity, primary_outstanding): (f64, u32),
        links: &[usize],
        explore: bool,
    ) -> usize {
        use crate::path_scheduler::{LINK_FLOOR_SHARE, link_marginal};
        let total_capacity =
            primary_capacity + links.iter().map(|&i| slots[i].capacity()).sum::<f64>();
        if explore
            && let Some(&idle) = links.iter().find(|&&i| {
                slots[i].outstanding == 0
                    && slots[i].capacity() >= LINK_FLOOR_SHARE * total_capacity
            })
        {
            return idle;
        }
        let mut chosen = shared_choice;
        let mut best = link_marginal(primary_capacity, primary_outstanding);
        for &i in links {
            let marginal = link_marginal(slots[i].capacity(), slots[i].outstanding);
            if marginal > best {
                best = marginal;
                chosen = i;
            }
        }
        chosen
    }

    /// worker 段结束回报。
    ///
    /// - `Ok`：清零失败计数；段 ≥ [`MIN_SAMPLE_BYTES`] 且
    ///   该槽位尚未被窗口实测时，按 `bytes/elapsed` 喂估计（冷路径直接采纳）；
    ///   钉定节点落盘健康度与遥测；
    /// - `Err`：降权（估计减半 + 失败计数递增）；validator 不一致
    ///   （[`DownloadError::VersionChanged`]，多路径语境 = 内容不一致）
    ///   立即踢除，其余连续失败 ≥ [`KICK_STREAK`] 踢除。SYS 永不踢除。
    ///   代理路径因 validator 被踢时登记，供 coordinator 记 NoSwitch。
    pub fn report(
        &self,
        lease: &NodeLease,
        bytes: u64,
        elapsed: Duration,
        outcome: Result<(), &DownloadError>,
    ) {
        let mut events: Vec<EngineEvent> = Vec::new();
        {
            let mut inner = match self.inner.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            let mut validator_kick: Option<RoutePath> = None;
            let mut path_failure: Option<RoutePath> = None;
            let Some(slot) = inner.slots.get_mut(lease.node_id) else {
                return;
            };
            match outcome {
                Ok(()) => {
                    slot.fail_streak = 0;
                    if bytes >= MIN_SAMPLE_BYTES && !elapsed.is_zero() {
                        let rate = bytes as f64 / elapsed.as_secs_f64();
                        if slot.measured_windows == 0 {
                            slot.ewma_bps = if slot.cold {
                                rate
                            } else {
                                (1.0 - EWMA_ALPHA) * slot.ewma_bps + EWMA_ALPHA * rate
                            };
                            slot.cold = false;
                            slot.evidence = true;
                        }
                        if let (Some(ip), Some(db)) = (slot.ip, self.db.as_ref()) {
                            super::health::record_ewma(&self.host, ip, slot.ewma_bps, db);
                            // P2 遥测：段吞吐样本（仅钉定节点——SYS 无法归因 IP）。
                            super::telemetry::record_segment(
                                &self.host,
                                ip,
                                Some(rate as u64),
                                true,
                                db,
                            );
                        }
                    }
                }
                Err(e) => {
                    // 失败只降排序（score 按连续失败数减半），不改写速率估计：
                    // 失败不是「该路径慢」的证据，竞争集按速率判定，避免一次
                    // 瞬时失败就把节点永久挤出派工（连续失败 ≥3 才踢除）。
                    slot.fail_streak += 1;
                    let ip = slot.ip;
                    let immediate = super::is_validator_mismatch(e);
                    let should_kick =
                        lease.node_id != 0 && (immediate || slot.fail_streak >= KICK_STREAK);
                    if let (Some(ip), Some(db)) = (ip, self.db.as_ref()) {
                        // 跨任务健康度按降权后的 score 落盘（下个任务先验排序靠后）。
                        super::health::record_ewma(&self.host, ip, slot.score(), db);
                        // P2 遥测：节点失败样本。
                        super::telemetry::record_segment(&self.host, ip, None, false, db);
                    }
                    if should_kick {
                        slot.kicked = true;
                        let streak = slot.fail_streak;
                        // 仅 Auto 路径（hostname 路由的直连/代理槽位）的传输失败写
                        // 跨任务路径健康度；CDN 钉定 IP 与网卡链路另有各自的记忆。
                        if ip.is_none()
                            && slot.link.is_none()
                            && crate::net_failure::classify_download_error(e).is_some()
                        {
                            path_failure = Some(slot.route);
                        }
                        if immediate && matches!(slot.route, RoutePath::Proxy(_)) {
                            validator_kick = Some(slot.route);
                        }
                        // 网卡链路连续失败（无路由/被上游拒绝）：后续任务暂不再
                        // 规划它。validator 不一致只说明该出口命中了不同 CDN
                        // edge，与链路可用性无关，不记忆。
                        if !immediate && let Some(link) = slot.link.as_deref() {
                            crate::multi_nic::record_link_failure(link);
                        }
                        log_info!(
                            "[cdn-pool] host {} 节点 {} 被踢除（{}）",
                            self.host,
                            lease.describe(),
                            if immediate {
                                "validator 不一致".to_string()
                            } else {
                                format!("连续失败 {streak}")
                            }
                        );
                        if ip.is_some() || slot.link.is_some() {
                            let label = lease.describe();
                            events.push(if immediate {
                                self.kick_event(label, "validator", 0)
                            } else {
                                self.kick_event(label, "fail", streak)
                            });
                        }
                        if let Some(evt) = self.check_breaker(&mut inner) {
                            events.push(evt);
                        }
                    }
                }
            }
            if let Some(route) = validator_kick {
                inner.validator_kicked.push(route);
            }
            if let Some(route) = path_failure {
                inner.note_path_failure(route);
            }
        }
        self.emit_all(events);
    }

    /// 在途连接快照（coordinator 窗口采样与抢占用）。
    pub fn live_conns(&self) -> Vec<LiveConn> {
        self.inner
            .lock()
            .map(|inner| inner.live.values().cloned().collect())
            .unwrap_or_default()
    }

    /// 完成时间抢占：取消指定租约的分段令牌。worker 走取消分支刷盘并登记
    /// 已下进度后上报 `Preempted`，剩余字节由 coordinator 重新派工。
    /// 租约已归还 → `false`。
    pub fn preempt(&self, lease_id: u64) -> bool {
        let token = self
            .inner
            .lock()
            .ok()
            .and_then(|inner| inner.live.get(&lease_id).map(|c| c.cancel.clone()));
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// 喂入一个 ramp 窗口的观测：`samples` = (槽位, 单连接稳态 B/s)，同槽位
    /// 取中位数融合进估计（首个实测样本直接覆盖先验），并以样本数刷新该
    /// 槽位的窗口连接数（链路容量估计的乘数）。
    pub fn observe_window(&self, samples: &[(usize, f64)]) {
        self.observe(samples, true);
    }

    /// 喂入被判停滞并抢占的连接（0 速率实证）：只融合速率估计，不改写窗口
    /// 连接数——同窗 [`Self::observe_window`] 已给出真实连接规模，停滞子集
    /// 不能把链路容量压成单连接。
    pub fn observe_stalled(&self, samples: &[(usize, f64)]) {
        self.observe(samples, false);
    }

    fn observe(&self, samples: &[(usize, f64)], count_conns: bool) {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut per_node: HashMap<usize, Vec<f64>> = HashMap::new();
        for &(node_id, bps) in samples {
            if bps.is_finite() && bps >= 0.0 {
                per_node.entry(node_id).or_default().push(bps);
            }
        }
        for (node_id, mut values) in per_node {
            let Some(med) = crate::path_scheduler::median(&mut values) else {
                continue;
            };
            let Some(slot) = inner.slots.get_mut(node_id) else {
                continue;
            };
            slot.ewma_bps =
                crate::path_scheduler::blend_rate(slot.ewma_bps, med, slot.measured_windows == 0);
            slot.measured_windows = slot.measured_windows.saturating_add(1);
            if count_conns {
                slot.window_conns = u32::try_from(values.len()).unwrap_or(u32::MAX);
            }
            slot.cold = false;
            slot.evidence = true;
            if let (Some(ip), Some(db)) = (slot.ip, self.db.as_ref()) {
                super::health::record_ewma(&self.host, ip, slot.ewma_bps, db);
            }
        }
    }

    /// 估计来自本任务真实传输的存活槽位中最高的单连接估计（抢占基准）。
    /// 窗口稳态样本或完成租约均算实证——短命连接的路径可能从未跨过两个
    /// 窗口，但其完成租约同样证明了该路径的速度。
    pub fn best_measured_rate(&self) -> Option<f64> {
        let inner = self.inner.lock().ok()?;
        inner
            .slots
            .iter()
            .filter(|s| !s.kicked && s.evidence)
            .map(NodeSlot::score)
            .filter(|v| *v > 0.0)
            .max_by(f64::total_cmp)
    }

    /// 租约结束时回报其实际传输字节（成功/失败/被抢占一律回报）。
    pub fn record_transfer(&self, lease: &NodeLease, bytes: u64) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(slot) = inner.slots.get_mut(lease.node_id)
        {
            slot.bytes_done = slot.bytes_done.saturating_add(bytes);
        }
    }

    /// 各路径的累计传输字节（主导路径判定）：已结束租约的回报 + `live` 给出
    /// 的在途租约进度（槽位, 字节）。
    pub fn route_bytes(&self, live: &[(usize, u64)]) -> Vec<(RoutePath, u64)> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut acc: Vec<(RoutePath, u64)> = Vec::new();
        let mut add = |route: RoutePath, bytes: u64| match acc.iter_mut().find(|(r, _)| *r == route)
        {
            Some((_, total)) => *total = total.saturating_add(bytes),
            None => acc.push((route, bytes)),
        };
        for slot in &inner.slots {
            add(slot.route, slot.bytes_done);
        }
        for &(node_id, bytes) in live {
            if let Some(slot) = inner.slots.get(node_id) {
                add(slot.route, bytes);
            }
        }
        acc
    }

    /// 加速来源累计字节（任务「来源构成」）：已结束租约的回报 + `live` 给出的
    /// 在途进度，按槽位所属路径归类——代理路径 → `proxy`，网卡链路 → `nic`，
    /// 直连钉定 IP → `cdn`；SYS / 无钉定的直连槽位是源站主链路，不计入
    /// （源站字节 = 已下载 − 三者之和）。整池一次加锁，无逐块开销。
    pub fn source_bytes(&self, live: &[(usize, u64)]) -> SourceBytes {
        let Ok(inner) = self.inner.lock() else {
            return SourceBytes::default();
        };
        let mut acc = SourceBytes::default();
        let mut add = |slot: &NodeSlot, bytes: u64| {
            let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
            let field = match (slot.route, slot.ip) {
                (RoutePath::Proxy(_), _) => &mut acc.proxy,
                (RoutePath::Link(_), _) => &mut acc.nic,
                (RoutePath::Direct, Some(_)) => &mut acc.cdn,
                (RoutePath::Direct, None) => return,
            };
            *field = field.saturating_add(bytes);
        };
        for slot in &inner.slots {
            add(slot, slot.bytes_done);
        }
        for &(node_id, bytes) in live {
            if let Some(slot) = inner.slots.get(node_id) {
                add(slot, bytes);
            }
        }
        acc
    }

    /// 起飞路径之外的备选路径是否已被实际使用（承载过连接即算，不回退）。
    pub fn alternates_explored(&self) -> bool {
        let Ok(inner) = self.inner.lock() else {
            return false;
        };
        let start = inner.slots.first().map_or(RoutePath::Direct, |s| s.route);
        inner
            .slots
            .iter()
            .any(|s| s.route != start && s.ever_leased)
    }

    /// 取走本任务因 validator 不一致被踢除的代理路径。
    pub fn take_validator_kicks(&self) -> Vec<RoutePath> {
        self.inner
            .lock()
            .map(|mut inner| std::mem::take(&mut inner.validator_kicked))
            .unwrap_or_default()
    }

    /// 取走本任务新增的 Auto 路径传输失败（每条路径整个任务至多出现一次），
    /// 由 coordinator 写入跨任务路径健康度（`route_health::record_path_failure`）。
    pub fn take_path_failures(&self) -> Vec<RoutePath> {
        self.inner
            .lock()
            .map(|mut inner| std::mem::take(&mut inner.path_failures))
            .unwrap_or_default()
    }

    /// 已实测的 hostname 路径估计（SYS 与备选路径；钉定节点另有 IP 级
    /// 健康度），供任务结束时写跨任务先验。本任务登记过传输失败的路径不在
    /// 其列：`record_path_rate` 会清零该路径的失败权重，早先的实测不能抹掉
    /// 后来的断流（被中途阻断的直连恰恰是「先测得到速度、后来不通」）。
    pub fn path_estimates(&self) -> Vec<(RoutePath, f64)> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner
            .slots
            .iter()
            .filter(|s| {
                s.ip.is_none()
                    && s.measured_windows > 0
                    && s.ewma_bps > 0.0
                    && !inner.path_failures_noted.contains(&s.route)
            })
            .map(|s| (s.route, s.ewma_bps))
            .collect()
    }

    /// 各节点的贡献统计快照（`kind="summary"` 事件载荷）：SYS 节点 ip 恒为
    /// `"SYS"`；`bytes` = 本任务经该节点实际下载字节数；`ewma_bps` = 当前
    /// 估计。含已被踢节点（其贡献仍真实存在）。
    pub fn node_stats(&self) -> Vec<crate::model::CdnNodeInfo> {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| Self::node_info(i, s))
            .collect()
    }

    /// 池的钉定目标 host（单节点池为空串）。
    pub fn host(&self) -> &str {
        &self.host
    }

    /// 聚合熔断判定：被踢钉定节点数 > 存活钉定节点数 → 记录该 host 24h 内
    /// 不再聚合（仅记一次；本任务照常在 SYS 上继续）。触发时返回
    /// `kind="breaker"` 事件（由调用方在锁外发射）。只统计钉定节点——
    /// 代理路径的失败与 CDN 聚合适配性无关。
    fn check_breaker(&self, inner: &mut PoolInner) -> Option<EngineEvent> {
        if inner.no_aggregate_recorded {
            return None;
        }
        let kicked = inner
            .slots
            .iter()
            .filter(|s| s.ip.is_some() && s.kicked)
            .count();
        let alive = inner
            .slots
            .iter()
            .filter(|s| s.ip.is_some() && !s.kicked)
            .count();
        if kicked > alive {
            inner.no_aggregate_recorded = true;
            if let Some(db) = self.db.as_ref() {
                super::health::record_no_aggregate(&self.host, db);
            }
            return Some(EngineEvent::TaskCdnEvent {
                task_id: self.task_id.clone(),
                kind: "breaker".to_string(),
                host: self.host.clone(),
                nodes: Vec::new(),
                ip: String::new(),
                reason: String::new(),
                candidates: 0,
                alive: 0,
                cap: 0,
                auto_cap: false,
            });
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        DEFAULT_EWMA_BPS, KICK_STREAK, LeaseRequest, MIN_SAMPLE_BYTES, NodePool, NodeSlot,
    };
    use crate::auto_proxy::{CandidateSource, RoutePath};
    use crate::downloader::DownloadError;
    use crate::model::SourceBytes;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    const MANUAL: RoutePath = RoutePath::Proxy(CandidateSource::ManualFields);
    const SYSTEM: RoutePath = RoutePath::Proxy(CandidateSource::System);

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, n))
    }

    /// 直接构造带真实 client 槽位的多节点池（测试不经懒建路径，
    /// 不发任何网络请求——lease 只 clone client）。
    fn test_pool(ips: &[IpAddr]) -> Arc<NodePool> {
        let client = reqwest::Client::new();
        let pool = NodePool::single(client.clone());
        {
            let mut inner = pool.inner.lock().unwrap();
            for &ip in ips {
                inner.slots.push(NodeSlot::new(
                    Some(ip),
                    RoutePath::Direct,
                    Some(client.clone()),
                    None,
                ));
            }
        }
        pool
    }

    /// 获取一个钉定节点的租约。等分数时 tie-break 恒选低编号（SYS），
    /// 故需持有沿途的 SYS/其他租约占满其并发额度，逼出钉定节点。
    fn pinned_lease(pool: &Arc<NodePool>) -> (super::NodeLease, Vec<super::NodeLease>) {
        let mut held = Vec::new();
        loop {
            let l = pool.lease();
            if l.is_pinned() {
                return (l, held);
            }
            held.push(l);
        }
    }

    fn node_of(pool: &Arc<NodePool>, route: RoutePath) -> usize {
        pool.inner
            .lock()
            .unwrap()
            .slots
            .iter()
            .position(|s| s.route == route && s.ip.is_none())
            .unwrap()
    }

    #[test]
    fn single_pool_always_leases_sys() {
        let pool = NodePool::single(reqwest::Client::new());
        assert!(!pool.is_multi());
        let l1 = pool.lease();
        let l2 = pool.lease();
        assert!(!l1.is_pinned());
        assert!(!l2.is_attributable());
        assert_eq!(l1.describe(), "SYS");
    }

    #[test]
    fn lease_disperses_across_nodes_under_cap() {
        let pool = test_pool(&[ip(2), ip(3)]);
        assert!(pool.is_multi());
        // 3 节点（SYS + 2 钉定）等分数起步：3 个并发租约必须落在 3 个不同节点。
        let l1 = pool.lease();
        let l2 = pool.lease();
        let l3 = pool.lease();
        let mut nodes = vec![l1.node_id, l2.node_id, l3.node_id];
        nodes.sort_unstable();
        nodes.dedup();
        assert_eq!(nodes.len(), 3, "cap 约束下并发租约不得聚集单节点");
    }

    #[test]
    fn lease_release_frees_capacity() {
        let pool = test_pool(&[ip(2)]);
        let l1 = pool.lease();
        let first = l1.node_id;
        drop(l1);
        // 归还后单租约仍应落在 score 最高者上（确定性），不会因泄漏挤到他处。
        let l2 = pool.lease();
        assert_eq!(l2.node_id, first);
    }

    #[test]
    fn failed_node_gets_kicked_and_pool_falls_back_to_sys() {
        let pool = test_pool(&[ip(2)]);
        let err = DownloadError::Other("segment 0 stalled: no data".to_string());
        for _ in 0..KICK_STREAK {
            let (lease, _held) = pinned_lease(&pool);
            pool.report(&lease, 0, Duration::from_secs(1), Err(&err));
        }
        // 连续失败后钉定节点被踢：此后所有租约都是 SYS。
        assert_eq!(pool.alive_pinned(), 0);
        let mut held = Vec::new();
        for _ in 0..4 {
            let l = pool.lease();
            assert!(!l.is_pinned());
            held.push(l);
        }
    }

    #[test]
    fn version_changed_kicks_immediately() {
        let pool = test_pool(&[ip(2), ip(3), ip(4)]);
        // 找到一个钉定租约并报 validator 不一致。
        let (lease, _held) = pinned_lease(&pool);
        let err = DownloadError::VersionChanged("200 OK".to_string());
        let before = pool.alive_pinned();
        pool.report(&lease, 0, Duration::from_secs(1), Err(&err));
        assert_eq!(pool.alive_pinned(), before - 1, "validator 不一致立即踢除");
        assert!(
            pool.take_validator_kicks().is_empty(),
            "钉定节点不登记代理 NoSwitch"
        );
    }

    #[test]
    fn completion_report_seeds_estimate_only_before_window_measurement() {
        let pool = test_pool(&[ip(2)]);
        let (lease, _held) = pinned_lease(&pool);
        let node_id = lease.node_id;
        // 微段：不喂估计。
        pool.report(&lease, MIN_SAMPLE_BYTES - 1, Duration::from_secs(1), Ok(()));
        assert_eq!(
            pool.inner.lock().unwrap().slots[node_id].ewma_bps,
            DEFAULT_EWMA_BPS
        );
        // 大段：估计移动。
        pool.report(&lease, 8 * 1024 * 1024, Duration::from_secs(1), Ok(()));
        let after = pool.inner.lock().unwrap().slots[node_id].ewma_bps;
        assert!(after > DEFAULT_EWMA_BPS, "8MB/s 样本应抬升 4MB/s 先验");
        // 窗口实测后，段完成（含握手的粗粒度）不再覆盖稳态估计。
        pool.observe_window(&[(node_id, 1000.0)]);
        pool.report(&lease, 8 * 1024 * 1024, Duration::from_secs(1), Ok(()));
        assert_eq!(pool.inner.lock().unwrap().slots[node_id].ewma_bps, 1000.0);
    }

    #[test]
    fn success_resets_fail_streak() {
        let pool = test_pool(&[ip(2)]);
        let (lease, _held) = pinned_lease(&pool);
        let err = DownloadError::Other("segment 1 stalled: no data".to_string());
        pool.report(&lease, 0, Duration::from_secs(1), Err(&err));
        pool.report(&lease, 0, Duration::from_secs(1), Err(&err));
        pool.report(&lease, MIN_SAMPLE_BYTES, Duration::from_secs(1), Ok(()));
        assert_eq!(
            pool.inner.lock().unwrap().slots[lease.node_id].fail_streak,
            0,
            "成功必须清零失败计数（避免累积到踢除阈值）"
        );
    }

    #[test]
    fn cold_alternate_is_explored_by_exactly_one_connection() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), None)],
        );
        pool.set_explore(true);
        let first = pool.lease();
        assert_eq!(first.route(), MANUAL, "冷路径优先获得 1 条探索连接");
        let second = pool.lease();
        let third = pool.lease();
        assert_eq!(second.route(), RoutePath::Direct, "未实测前不再加码冷路径");
        assert_eq!(third.route(), RoutePath::Direct);
    }

    #[test]
    fn exploration_disabled_or_forbidden_stays_on_start_route() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), None)],
        );
        // 守卫关闭探索：冷路径不被选中。
        assert_eq!(pool.lease().route(), RoutePath::Direct);
        // 开放式首段禁止备选路径：即便已知代理更快也留在起飞路径。
        pool.set_explore(true);
        let manual = node_of(&pool, MANUAL);
        pool.observe_window(&[(manual, 10e6), (0, 1e3)]);
        let lease = pool.lease_for(LeaseRequest {
            seg_index: 0,
            start_downloaded: 0,
            bytes: i64::MAX,
            allow_alternates: false,
            cancel: CancellationToken::new(),
        });
        assert_eq!(lease.route(), RoutePath::Direct);
    }

    #[test]
    fn slow_path_leaves_competitive_set_fast_path_takes_all_leases() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), None)],
        );
        let manual = node_of(&pool, MANUAL);
        // 同窗实测：直连 100KB/s，代理 2MB/s。
        pool.observe_window(&[(0, 100e3), (manual, 2e6)]);
        let held: Vec<_> = (0..6).map(|_| pool.lease()).collect();
        assert!(
            held.iter().all(|l| l.route() == MANUAL),
            "慢于最优一半的路径不得分到连接"
        );
    }

    #[test]
    fn comparable_paths_are_aggregated() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(SYSTEM, reqwest::Client::new(), None)],
        );
        let system = node_of(&pool, SYSTEM);
        // 代理已饱和：单连接速率降到与直连相当 → 两条路径分担连接。
        pool.observe_window(&[(0, 1.0e6), (system, 1.2e6)]);
        let held: Vec<_> = (0..4).map(|_| pool.lease()).collect();
        let on_proxy = held.iter().filter(|l| l.route() == SYSTEM).count();
        assert_eq!(on_proxy, 2, "竞争集内按 cap 分散");
    }

    #[test]
    fn first_window_measurement_overrides_optimistic_prior() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            Some(50e3),
            vec![(MANUAL, reqwest::Client::new(), Some(10e6))],
        );
        // 先验：代理远快 → 起飞即走代理。
        assert_eq!(pool.lease().route(), MANUAL);
        let manual = node_of(&pool, MANUAL);
        // 实测翻转：代理其实很慢。
        pool.observe_window(&[(0, 2e6), (manual, 30e3)]);
        assert_eq!(pool.lease().route(), RoutePath::Direct);
    }

    #[test]
    fn proxy_validator_mismatch_is_kicked_and_reported_once() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), Some(1e6))],
        );
        let manual = node_of(&pool, MANUAL);
        pool.observe_window(&[(0, 1e3), (manual, 1e6)]);
        let lease = pool.lease();
        assert_eq!(lease.route(), MANUAL);
        assert!(lease.is_attributable());
        // 206 路径的 validator 不一致以 Other 报出，同样立即踢除。
        let err = DownloadError::Other("segment 2: validator mismatch — probe etag".to_string());
        pool.report(&lease, 0, Duration::from_secs(1), Err(&err));
        assert_eq!(pool.take_validator_kicks(), vec![MANUAL]);
        assert!(pool.take_validator_kicks().is_empty());
        assert_eq!(pool.lease().route(), RoutePath::Direct, "被踢路径不再派工");
    }

    #[test]
    fn preempt_cancels_live_lease_token_and_drop_unregisters() {
        let pool = NodePool::single(reqwest::Client::new());
        let token = CancellationToken::new();
        let lease = pool.lease_for(LeaseRequest {
            seg_index: 7,
            start_downloaded: 0,
            bytes: i64::MAX,
            allow_alternates: true,
            cancel: token.clone(),
        });
        let live = pool.live_conns();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].seg_index, 7);
        assert!(pool.preempt(live[0].lease_id));
        assert!(token.is_cancelled());
        let id = live[0].lease_id;
        drop(lease);
        assert!(pool.live_conns().is_empty());
        assert!(!pool.preempt(id), "已归还租约不可抢占");
    }

    #[test]
    fn route_bytes_count_finished_and_in_flight_transfers() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), Some(1e6))],
        );
        let manual = node_of(&pool, MANUAL);
        pool.observe_window(&[(0, 1e3), (manual, 1e6)]);
        let proxy_lease = pool.lease();
        assert_eq!(proxy_lease.route(), MANUAL);
        pool.record_transfer(&proxy_lease, 30);
        drop(proxy_lease);
        // 已结束租约 30 B + 在途 5 B 走代理；直连在途 10 B。
        let bytes = pool.route_bytes(&[(0, 10), (manual, 5)]);
        assert!(bytes.contains(&(RoutePath::Direct, 10)));
        assert!(bytes.contains(&(MANUAL, 35)));
        assert!(pool.alternates_explored());
    }

    #[test]
    fn source_bytes_classify_slots_by_path_kind() {
        // SYS(0) + 钉定 CDN(1,2) + 代理路径 + 网卡链路。
        let pool = test_pool(&[ip(2), ip(3)]);
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), None)],
        );
        pool.add_links(vec![(nic("en1", 2), reqwest::Client::new())]);
        let manual = node_of(&pool, MANUAL);
        let link = node_of(&pool, RoutePath::Link(2));
        assert_eq!(pool.source_bytes(&[]), SourceBytes::default());

        // 已结束租约回报：经 record_transfer 落到各槽位。
        {
            let mut inner = pool.inner.lock().unwrap();
            inner.slots[0].bytes_done = 1_000; // SYS 源站：不计入
            inner.slots[1].bytes_done = 200; // 钉定 CDN
            inner.slots[manual].bytes_done = 30; // 代理
            inner.slots[link].bytes_done = 4; // 网卡
        }
        assert_eq!(
            pool.source_bytes(&[]),
            SourceBytes {
                cdn: 200,
                proxy: 30,
                nic: 4
            }
        );

        // 在途进度叠加到对应槽位；SYS 在途仍是源站，不计入。
        let bytes = pool.source_bytes(&[(0, 500), (1, 8), (2, 7), (manual, 5), (link, 6), (99, 9)]);
        assert_eq!(
            bytes,
            SourceBytes {
                cdn: 215,
                proxy: 35,
                nic: 10
            }
        );
    }

    #[test]
    fn completed_leases_count_as_evidence_for_preemption_baseline() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            Some(5e6),
            vec![(MANUAL, reqwest::Client::new(), None)],
        );
        // 先验不是实证：没有任何真实传输时不提供抢占基准。
        assert_eq!(pool.best_measured_rate(), None);
        let lease = pool.lease();
        assert_eq!(lease.route(), RoutePath::Direct);
        // 直连租约 1s 完成 3MB（从未跨过两个窗口）：仍是实证。
        pool.report(&lease, 3_000_000, Duration::from_secs(1), Ok(()));
        let best = pool.best_measured_rate().unwrap();
        assert!(best > 1e6, "完成租约的速率必须进入抢占基准");
    }

    #[test]
    fn sys_transport_fallback_needs_live_alternate_and_is_bounded() {
        let single = NodePool::single(reqwest::Client::new());
        assert!(
            !single.try_sys_transport_fallback(),
            "无其它路径时保持原语义"
        );
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), Some(1e6))],
        );
        let granted = (0..20)
            .filter(|_| pool.try_sys_transport_fallback())
            .count();
        assert_eq!(
            granted,
            super::SYS_FALLBACK_BUDGET as usize,
            "配额耗尽后失败按原语义上抛"
        );
    }

    fn stall_err() -> DownloadError {
        DownloadError::Other("segment 1 stalled: no data".to_string())
    }

    /// 备选路径因传输失败被踢除：该路径记一次失败，之后的失败不再重复登记。
    #[test]
    fn kicked_alternate_path_reports_transport_failure_once() {
        for (start, alt) in [(RoutePath::Direct, MANUAL), (MANUAL, RoutePath::Direct)] {
            let pool = NodePool::single(reqwest::Client::new());
            pool.add_paths(start, None, vec![(alt, reqwest::Client::new(), Some(1e6))]);
            let alt_node = node_of(&pool, alt);
            pool.observe_window(&[(0, 1e3), (alt_node, 1e6)]);
            let lease = pool.lease();
            assert_eq!(lease.route(), alt);
            for _ in 0..KICK_STREAK {
                pool.report(&lease, 0, Duration::from_secs(1), Err(&stall_err()));
            }
            assert_eq!(pool.take_path_failures(), vec![alt]);
            // 被踢后再有失败回报（在途租约收尾）不重复登记。
            pool.report(&lease, 0, Duration::from_secs(1), Err(&stall_err()));
            assert!(pool.take_path_failures().is_empty());
        }
    }

    /// 只有传输失败才登记：HTTP 状态类失败即使把路径踢除也不是「路径不通」；
    /// CDN 钉定节点不属于 Auto 路径。
    #[test]
    fn non_transport_kicks_and_pinned_nodes_do_not_report_path_failure() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), Some(1e6))],
        );
        let manual = node_of(&pool, MANUAL);
        pool.observe_window(&[(0, 1e3), (manual, 1e6)]);
        let lease = pool.lease();
        assert_eq!(lease.route(), MANUAL);
        let http = DownloadError::Other("segment 1: HTTP 503 Service Unavailable".to_string());
        for _ in 0..KICK_STREAK {
            pool.report(&lease, 0, Duration::from_secs(1), Err(&http));
        }
        assert!(pool.take_path_failures().is_empty());

        let pinned_pool = test_pool(&[ip(2)]);
        let (pinned, _held) = pinned_lease(&pinned_pool);
        for _ in 0..KICK_STREAK {
            pinned_pool.report(&pinned, 0, Duration::from_secs(1), Err(&stall_err()));
        }
        assert!(pinned_pool.take_path_failures().is_empty());
    }

    /// SYS 起飞路径触发传输改道：登记 SYS 路径失败（一次），且该路径此后不再
    /// 以早先的实测速率写先验（`record_path_rate` 会清掉刚记的失败）。
    #[test]
    fn sys_fallback_reports_start_path_once_and_drops_its_prior() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), Some(1e6))],
        );
        let manual = node_of(&pool, MANUAL);
        pool.observe_window(&[(0, 5e6), (manual, 2e6)]);
        assert_eq!(pool.path_estimates().len(), 2);

        assert!(pool.try_sys_transport_fallback());
        assert!(pool.try_sys_transport_fallback());
        assert_eq!(pool.take_path_failures(), vec![RoutePath::Direct]);
        assert!(pool.take_path_failures().is_empty());
        let estimates = pool.path_estimates();
        assert_eq!(estimates.len(), 1);
        assert_eq!(estimates[0].0, MANUAL);
    }

    /// 无改道可用（无备选 / 备选被踢 / 配额耗尽）时 SYS 租约保持完整重试预算。
    #[test]
    fn sys_fallback_availability_follows_alternates_and_budget() {
        let single = NodePool::single(reqwest::Client::new());
        assert!(!single.sys_fallback_available());

        let kicked = NodePool::single(reqwest::Client::new());
        kicked.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), Some(1e6))],
        );
        assert!(kicked.sys_fallback_available());
        let manual = node_of(&kicked, MANUAL);
        kicked.observe_window(&[(0, 1e3), (manual, 1e6)]);
        let lease = kicked.lease();
        let err = DownloadError::Other("segment 2: validator mismatch — probe etag".to_string());
        kicked.report(&lease, 0, Duration::from_secs(1), Err(&err));
        assert!(!kicked.sys_fallback_available(), "备选被踢后无改道可用");

        let drained = NodePool::single(reqwest::Client::new());
        drained.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), Some(1e6))],
        );
        for _ in 0..super::SYS_FALLBACK_BUDGET {
            assert!(drained.try_sys_transport_fallback());
        }
        assert!(!drained.sys_fallback_available(), "配额耗尽后恢复耐心");
    }

    #[test]
    fn cold_path_is_not_explored_on_small_pieces() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_paths(
            RoutePath::Direct,
            None,
            vec![(MANUAL, reqwest::Client::new(), None)],
        );
        pool.set_explore(true);
        let small = pool.lease_for(LeaseRequest {
            seg_index: 1,
            start_downloaded: 0,
            bytes: crate::path_scheduler::EXPLORE_MIN_PIECE - 1,
            allow_alternates: true,
            cancel: CancellationToken::new(),
        });
        assert_eq!(
            small.route(),
            RoutePath::Direct,
            "小片不足以进入稳态，不拿来探索"
        );
    }

    #[test]
    fn lease_snapshot_emitted_throttled_with_active_counts() {
        use crate::events::{EngineEvent, EventSink};
        use std::sync::Mutex;

        struct CaptureSink(Mutex<Vec<EngineEvent>>);
        impl EventSink for CaptureSink {
            fn emit(&self, evt: EngineEvent) {
                self.0.lock().unwrap().push(evt);
            }
        }

        let sink = Arc::new(CaptureSink(Mutex::new(Vec::new())));
        let pool = test_pool(&[ip(2), ip(3)]);
        // test_pool 产物 sink=None；同模块直接重建带 sink 的池（槽位复用）。
        let slots = std::mem::take(&mut pool.inner.lock().unwrap().slots);
        let sink_dyn: Arc<dyn EventSink> = sink.clone();
        let pool = NodePool::build("h", None, None, "t", Some(sink_dyn), slots);

        let _l1 = pool.lease();
        {
            let events = sink.0.lock().unwrap();
            let leases: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    EngineEvent::TaskCdnEvent { kind, nodes, .. } if kind == "leases" => {
                        Some(nodes)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(leases.len(), 1, "首次租借必须发射一条 leases 快照");
            let total: i32 = leases[0].iter().map(|n| n.active).sum();
            assert_eq!(total, 1, "快照 active 总数 == 在途租约数");
        }

        // 2s 节流窗口内的后续借还不得再发射。
        let _l2 = pool.lease();
        let count = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::TaskCdnEvent { kind, .. } if kind == "leases"))
            .count();
        assert_eq!(count, 1, "节流窗口内不得重复发射快照");
    }

    fn nic(name: &str, index: u8) -> Arc<crate::multi_nic::LinkBinding> {
        Arc::new(crate::multi_nic::LinkBinding {
            name: name.to_string(),
            index: u32::from(index),
            v4: Some(Ipv4Addr::new(172, 20, 10, index)),
            v6: None,
        })
    }

    /// 单节点池 + 一条已实测的网卡链路：主链路 `primary_conns` 条连接各
    /// `primary_bps`，链路 1 条连接 `link_bps`。
    fn measured_link_pool(
        primary_conns: usize,
        primary_bps: f64,
        link_bps: f64,
    ) -> (Arc<NodePool>, usize) {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_links(vec![(nic("en1", 2), reqwest::Client::new())]);
        let link = node_of(&pool, RoutePath::Link(2));
        let mut samples = vec![(0usize, primary_bps); primary_conns];
        samples.push((link, link_bps));
        pool.observe_window(&samples);
        pool.set_explore(true);
        (pool, link)
    }

    #[test]
    fn links_take_connections_in_proportion_to_capacity() {
        // 主链路容量 80MB/s（8 × 10MB/s），热点 20MB/s（1 × 20MB/s）。
        let (pool, _) = measured_link_pool(8, 10e6, 20e6);
        let held: Vec<_> = (0..10).map(|_| pool.lease()).collect();
        let on_link = held.iter().filter(|l| l.route().is_link()).count();
        assert_eq!(on_link, 2, "容量 80:20 → 连接 8:2");
        assert_eq!(held[0].describe(), "NIC:en1", "空闲链路先拿保底连接");
    }

    #[test]
    fn saturated_primary_hands_extra_connections_to_a_fast_link() {
        // 主链路已饱和：8 条连接每条只剩 2MB/s（容量 16MB/s）；热点单连接 30MB/s。
        let (pool, _) = measured_link_pool(8, 2e6, 30e6);
        let held: Vec<_> = (0..6).map(|_| pool.lease()).collect();
        let on_link = held.iter().filter(|l| l.route().is_link()).count();
        // 注水：30/(n+1) 与 16/(m+1) 交替取大 → 链路拿到多数新连接。
        assert_eq!(on_link, 4);
    }

    #[test]
    fn negligible_link_gets_no_connections() {
        // 链路容量不足总量 5%：不值得占连接（也不保底）。
        let (pool, _) = measured_link_pool(8, 10e6, 1e6);
        let held: Vec<_> = (0..10).map(|_| pool.lease()).collect();
        assert!(held.iter().all(|l| !l.route().is_link()));
    }

    #[test]
    fn cold_link_is_explored_by_one_connection_first() {
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_links(vec![(nic("en1", 2), reqwest::Client::new())]);
        // 未开放探索：冷链路不被选中。
        assert_eq!(pool.lease().route(), RoutePath::Direct);
        pool.set_explore(true);
        let first = pool.lease();
        assert_eq!(first.route(), RoutePath::Link(2));
        let second = pool.lease();
        assert_eq!(second.route(), RoutePath::Direct, "未实测前不加码冷链路");
    }

    #[test]
    fn start_route_only_leases_never_use_links() {
        let (pool, _) = measured_link_pool(1, 1e6, 50e6);
        let lease = pool.lease_for(LeaseRequest {
            seg_index: 0,
            start_downloaded: 0,
            bytes: i64::MAX,
            allow_alternates: false,
            cancel: CancellationToken::new(),
        });
        assert_eq!(
            lease.route(),
            RoutePath::Direct,
            "开放式首段/plain GET 留在主链路"
        );
    }

    #[test]
    fn link_outstanding_is_reported_only_for_link_slots() {
        let (pool, link) = measured_link_pool(8, 10e6, 20e6);
        let first = pool.lease();
        assert_eq!(first.route(), RoutePath::Link(2));
        assert_eq!(pool.link_outstanding(link), Some(1));
        assert_eq!(pool.link_outstanding(0), None, "主链路不受链路守卫保护");
    }

    #[test]
    fn stalled_observation_keeps_window_connection_count() {
        let (pool, _) = measured_link_pool(8, 10e6, 20e6);
        pool.observe_stalled(&[(0, 0.0)]);
        let sys_conns = pool.inner.lock().unwrap().slots[0].window_conns;
        assert_eq!(sys_conns, 8, "停滞子集不得把主链路容量压成单连接");
        pool.observe_window(&[(0, 10e6), (0, 10e6)]);
        assert_eq!(pool.inner.lock().unwrap().slots[0].window_conns, 2);
    }

    #[test]
    fn failing_link_is_kicked_remembered_and_reported() {
        let binding = nic("en-kick-test", 9);
        let pool = NodePool::single(reqwest::Client::new());
        pool.add_links(vec![(binding.clone(), reqwest::Client::new())]);
        pool.set_explore(true);
        let err = DownloadError::Other("connection refused".to_string());
        for _ in 0..KICK_STREAK {
            let lease = pool.lease();
            assert_eq!(
                lease.route(),
                RoutePath::Link(9),
                "冷链路失败后仍按探索重试"
            );
            pool.report(&lease, 0, Duration::from_millis(5), Err(&err));
        }
        assert_eq!(pool.lease().route(), RoutePath::Direct, "被踢链路不再派工");
        assert!(
            crate::multi_nic::is_recently_failed(&binding),
            "踢除记入跨任务失败记忆"
        );
        let kicks: Vec<String> = pool
            .take_deferred_events()
            .into_iter()
            .filter_map(|e| match e {
                crate::events::EngineEvent::TaskCdnEvent { kind, ip, .. } if kind == "kick" => {
                    Some(ip)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            kicks,
            vec!["NIC:en-kick-test".to_string()],
            "无 sink 池暂存踢除事件"
        );
    }
}
