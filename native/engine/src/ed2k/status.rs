//! eD2K 实况：运行中任务的源 / 服务器 / Kad 进度与本机网络状态快照（详情页按需读取）。
//!
//! 调度状态的事实源仍是 [`super::run_ed2k_download`] 循环里的局部集合；本模块只保存它在每个
//! 调度点发布的**派生快照**，加上块下载任务自己更新的少量原子（阶段 / 排队名次 / 累计字节）。
//! 读取方（[`task_view`]）不触碰调度状态，任务退出时登记自动撤销。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, MutexGuard};
use std::time::Instant;

use crate::ed2k::client::{ClientConfig, ServerQuery, Source};

/// 相邻两次读取间隔不足该时长时沿用上次折算的速度（同 `bt_status::apply_speeds`）。
const SPEED_SAMPLE_MIN_MS: u128 = 400;

/// 本模块的锁只保护纯数据快照：持锁方 panic 后数据仍自洽，直接取回继续用。
fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

// ---------------------------------------------------------------------------
// 本机网络状态
// ---------------------------------------------------------------------------

/// 本机在 eD2K 服务器上的 ID 类型。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IdKind {
    /// 当前没有登录任何服务器。
    #[default]
    None,
    /// NAT / 防火墙之后，服务器无法回连监听端口。
    Low,
    /// 监听端口可被公网回连。
    High,
}

impl IdKind {
    /// 由服务器分配的 client ID 判定（0 = 未登录）。
    ///
    /// ```
    /// use fluxdown_engine::ed2k::status::IdKind;
    /// assert_eq!(IdKind::from_client_id(0), IdKind::None);
    /// assert_eq!(IdKind::from_client_id(0x00FF_FFFF), IdKind::Low);
    /// assert_eq!(IdKind::from_client_id(0x0100_0000), IdKind::High);
    /// ```
    #[must_use]
    pub fn from_client_id(client_id: u32) -> Self {
        if client_id == 0 {
            Self::None
        } else if client_id < crate::ed2k::proto::LOWID_THRESHOLD {
            Self::Low
        } else {
            Self::High
        }
    }
}

/// 监听端口的 UPnP 映射状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpnpState {
    /// 设置里未启用，或监听器尚未启动。
    #[default]
    Disabled,
    /// 正在查找网关 / 建立映射。
    Pending,
    Mapped,
    /// 没有 IGD 网关或映射被拒绝。
    Failed,
}

/// 共享客户端的运行期网络状态（不含设置项）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientNetwork {
    pub id: IdKind,
    /// 服务器分配的 client ID；未登录为 0。
    pub client_id: u32,
    /// 会话（或检测所用）服务器 `host:port`；没有为空。
    pub server: String,
    /// 实际监听端口；监听器未启动为 0。
    pub listen_port: u16,
    pub upnp: UpnpState,
    /// UPnP 网关报告的公网 IP；未知为空。
    pub external_ip: String,
    /// 映射失败（[`UpnpState::Failed`]）时各方式 / 网关给出的原因；其余为空。
    pub upnp_error: String,
}

/// 本机 eD2K 网络状态：运行期状态 + 相关设置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkView {
    pub client: ClientNetwork,
    pub upnp_enabled: bool,
    pub kad_enabled: bool,
    /// Kad 引导节点数（nodes.dat 订阅缓存）。
    pub kad_nodes: u32,
    /// 可用服务器数（手填 + 订阅，去重后）。
    pub server_count: u32,
    /// HighID / LowID 检测未能登录任何服务器时的原因；其余情况为空。
    pub error: String,
}

impl NetworkView {
    pub(crate) fn compose(client: ClientNetwork, config: &ClientConfig, kad_nodes: u32) -> Self {
        Self {
            client,
            upnp_enabled: config.enable_upnp,
            kad_enabled: config.enable_kad,
            kad_nodes,
            server_count: u32::try_from(config.servers.len()).unwrap_or(u32::MAX),
            error: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 任务实况模型
// ---------------------------------------------------------------------------

/// 任务运行阶段。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskPhase {
    #[default]
    Idle,
    /// 正在向服务器 / Kad 找源。
    Searching,
    Downloading,
    /// 没有可用的源，等待下一轮找源或重询冷却结束。
    Waiting,
    /// 全部块已下完，正在读盘终验。
    Verifying,
}

/// 源的发现途径。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PeerOrigin {
    #[default]
    Server,
    Kad,
}

/// 源的连接状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PeerState {
    /// 已发现，尚未连接。
    #[default]
    Known,
    /// 正在连接 / 握手 / 申请上传槽。
    Connecting,
    /// 在对端上传队列里排队。
    Queued,
    Transferring,
    /// 持有上传槽的空闲连接。
    Connected,
    /// 最近申请过上传槽，冷却结束前不再申请。
    Cooldown,
    /// 连接 / 传输失败，本轮不再使用。
    Failed,
    /// 发来的数据未通过校验，已拉黑。
    Banned,
}

impl PeerState {
    /// 列表排序权重：越小越靠前。
    fn rank(self) -> u8 {
        match self {
            Self::Transferring => 0,
            Self::Connected => 1,
            Self::Queued => 2,
            Self::Connecting => 3,
            Self::Known => 4,
            Self::Cooldown => 5,
            Self::Failed => 6,
            Self::Banned => 7,
        }
    }
}

/// 一个源。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerRow {
    /// HighID 源的 `ip:port`；LowID 源为空。
    pub addr: String,
    /// LowID 源的 client ID；HighID 源为 0。
    pub low_id: u32,
    pub origin: PeerOrigin,
    pub state: PeerState,
    /// 对端上传队列名次；未排队为 0。
    pub queue_rank: u32,
    /// 对端客户端软件与版本；未知为空。
    pub client: String,
    /// 对端昵称；未知为空。
    pub nickname: String,
    /// 已校验通过的字节 + 当前块已收到的字节。
    pub downloaded: u64,
    pub down_bps: u64,
    /// 对端持有的块数；尚未握手为 `None`。
    pub parts: Option<u32>,
    /// 正在下载的块序号。
    pub part: Option<u32>,
    /// 最近一次失败原因。
    pub message: String,
    /// 冷却结束的 unix 秒；不在冷却为 0。
    pub retry_at: i64,
}

/// 源数量汇总。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerCounts {
    pub total: u32,
    pub transferring: u32,
    pub connected: u32,
    pub queued: u32,
    pub connecting: u32,
    pub cooldown: u32,
    pub failed: u32,
    pub banned: u32,
}

impl PeerCounts {
    fn of(rows: &[PeerRow]) -> Self {
        let mut counts = Self {
            total: u32::try_from(rows.len()).unwrap_or(u32::MAX),
            ..Self::default()
        };
        for row in rows {
            let slot = match row.state {
                PeerState::Known => continue,
                PeerState::Transferring => &mut counts.transferring,
                PeerState::Connected => &mut counts.connected,
                PeerState::Queued => &mut counts.queued,
                PeerState::Connecting => &mut counts.connecting,
                PeerState::Cooldown => &mut counts.cooldown,
                PeerState::Failed => &mut counts.failed,
                PeerState::Banned => &mut counts.banned,
            };
            *slot += 1;
        }
        counts
    }
}

/// 服务器来源。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServerOrigin {
    #[default]
    Manual,
    Subscription,
}

/// 服务器找源状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServerState {
    #[default]
    Querying,
    Working,
    NotWorking,
}

/// 本任务查询过的一台服务器。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerRow {
    pub addr: String,
    pub origin: ServerOrigin,
    pub state: ServerState,
    /// 本机当前登录的会话服务器。
    pub connected: bool,
    /// 最近一次查询返回的源数。
    pub sources: Option<u32>,
    pub message: String,
    /// 最近一次查询结束的 unix 秒；尚无结果为 0。
    pub last_query_at: i64,
}

/// Kad 找源阶段。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KadPhase {
    #[default]
    Idle,
    Bootstrapping,
    FindingNodes,
    Searching,
    Done,
}

/// Kad 节点的来历。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KadNodeOrigin {
    /// nodes.dat 里的引导节点。
    #[default]
    Bootstrap,
    /// 其它节点在应答里告知的节点。
    Discovered,
}

/// 本机与 Kad 节点的交互进度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KadNodeState {
    #[default]
    Known,
    Queried,
    Responded,
}

/// 一个 Kad 节点。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KadNodeRow {
    /// `ip:udpPort`。
    pub addr: String,
    /// Kad ID（32 位小写十六进制）。
    pub id: String,
    pub origin: KadNodeOrigin,
    pub state: KadNodeState,
    pub tcp_port: u16,
    pub version: u8,
}

/// 任务的 Kad 找源实况。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KadView {
    pub phase: KadPhase,
    /// 本轮使用的引导节点数。
    pub bootstrap_nodes: u32,
    /// 应答过的节点数。
    pub responded: u32,
    /// 经网络新发现的节点数（不含引导节点）。
    pub discovered: u32,
    /// 最近一轮找到的源数。
    pub sources: u32,
    /// 最近一轮结束的 unix 秒；尚未完成过为 0。
    pub last_search_at: i64,
    /// 离文件哈希最近的节点（按 XOR 距离升序）。
    pub nodes: Vec<KadNodeRow>,
}

/// Kad 找源过程的发布口：[`crate::ed2k::kad::node::find_sources_kad`] 在阶段边界整份替换。
#[derive(Debug, Default)]
pub struct KadLive(StdMutex<KadView>);

impl KadLive {
    pub(crate) fn publish(&self, view: KadView) {
        *lock(&self.0) = view;
    }

    pub(crate) fn view(&self) -> KadView {
        lock(&self.0).clone()
    }
}

/// 运行中任务的实况（[`task_view`] 的结果）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskView {
    pub phase: TaskPhase,
    /// 预计下一次找源 / 申请上传槽的 unix 秒；未知为 0。
    pub next_search_at: i64,
    /// 已下完并通过校验的块数。
    pub parts_done: u32,
    /// 当前生效的同时连接源数上限。
    pub max_sources: u32,
    pub peer_counts: PeerCounts,
    /// 已按活跃度排序。
    pub peers: Vec<PeerRow>,
    pub servers: Vec<ServerRow>,
    pub kad: KadView,
}

/// 读取任务实况失败。
#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    /// 任务不存在或不是 eD2K 任务。
    #[error("{0}")]
    InvalidArgument(String),
    #[error("{0}")]
    Operation(String),
}

/// `DownloadManager::ed2k_status` 的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ed2kStatusView {
    /// 文件的 eD2K 哈希（32 位小写十六进制）。
    pub file_hash: String,
    pub parts_total: u32,
    /// 任务的线程数设置；0 = 自动。
    pub segments: i32,
    /// 任务未在运行时 `phase` 为 [`TaskPhase::Idle`]，列表为空。
    pub task: TaskView,
    pub network: NetworkView,
}

// ---------------------------------------------------------------------------
// 单源实况（块下载任务 ↔ 读取方共享）
// ---------------------------------------------------------------------------

/// 握手阶段取得的对端身份。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PeerInfo {
    pub client: String,
    pub nickname: String,
}

const STAGE_CONNECTING: u8 = 0;
const STAGE_QUEUED: u8 = 1;
const STAGE_TRANSFERRING: u8 = 2;

/// 一个源在连接 / 传输期间由块下载任务更新的实况。跨多轮连接复用，累计量不清零。
#[derive(Debug, Default)]
pub(crate) struct PeerLive {
    stage: AtomicU8,
    queue_rank: AtomicU32,
    /// 正在下载的块序号 + 1；0 = 没有在下。
    block: AtomicU64,
    verified_bytes: AtomicU64,
    /// 对端持有的块数 + 1；0 = 未知。
    parts: AtomicU32,
    info: StdMutex<PeerInfo>,
}

impl PeerLive {
    /// 开始为 `block` 服务：复用已持槽的连接直接进入传输，否则从连接开始。
    pub(crate) fn begin(&self, block: u64, has_session: bool) {
        self.block.store(block.saturating_add(1), Ordering::Relaxed);
        self.queue_rank.store(0, Ordering::Relaxed);
        self.stage.store(
            if has_session {
                STAGE_TRANSFERRING
            } else {
                STAGE_CONNECTING
            },
            Ordering::Relaxed,
        );
    }

    pub(crate) fn set_queued(&self, rank: u32) {
        self.queue_rank.store(rank, Ordering::Relaxed);
        self.stage.store(STAGE_QUEUED, Ordering::Relaxed);
    }

    pub(crate) fn set_transferring(&self) {
        self.queue_rank.store(0, Ordering::Relaxed);
        self.stage.store(STAGE_TRANSFERRING, Ordering::Relaxed);
    }

    pub(crate) fn set_info(&self, info: PeerInfo) {
        *lock(&self.info) = info;
    }

    pub(crate) fn set_parts(&self, parts: u32) {
        self.parts.store(parts.saturating_add(1), Ordering::Relaxed);
    }

    /// 本次服务结束；`verified` 为通过校验的块长度（未通过 / 失败为 0）。
    pub(crate) fn finish(&self, verified: u64) {
        self.block.store(0, Ordering::Relaxed);
        self.queue_rank.store(0, Ordering::Relaxed);
        self.verified_bytes.fetch_add(verified, Ordering::Relaxed);
    }
}

/// 调度循环发布的源粗状态（在途细分阶段由 [`PeerLive`] 提供）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeerCoarse {
    Known,
    /// 有块下载任务在跑（连接 / 排队 / 传输）。
    Busy,
    /// 持槽空闲连接。
    Connected,
    /// 冷却到给定 unix 秒。
    Cooldown(i64),
    Failed,
    Banned,
}

/// 调度循环发布的一个源。
#[derive(Clone, Debug)]
pub(crate) struct PeerSlot {
    pub source: Source,
    pub origin: PeerOrigin,
    pub coarse: PeerCoarse,
    pub live: Option<Arc<PeerLive>>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// 任务登记
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TaskInner {
    phase: TaskPhase,
    peers: Vec<PeerSlot>,
    servers: Vec<ServerRow>,
}

#[derive(Default)]
struct SpeedSamples {
    at: Option<Instant>,
    /// 源 → （累计字节, 速度）。
    peers: HashMap<Source, (u64, u64)>,
}

/// 一个运行中任务的实况。
pub(crate) struct TaskLive {
    inner: StdMutex<TaskInner>,
    kad: KadLive,
    parts_done: AtomicU32,
    max_sources: AtomicU32,
    /// 任务线程数设置（`<= 0` = 自动）；宿主改动后调度循环在下一个调度点读到。
    segment_count: AtomicI32,
    next_search_at: AtomicI64,
    /// 各块已收到的未校验字节（与进度上报共用）。
    progress: Arc<StdMutex<HashMap<u64, i64>>>,
    samples: StdMutex<SpeedSamples>,
}

impl TaskLive {
    pub(crate) fn kad(&self) -> &KadLive {
        &self.kad
    }

    pub(crate) fn segment_count(&self) -> i32 {
        self.segment_count.load(Ordering::Relaxed)
    }

    pub(crate) fn set_max_sources(&self, value: u32) {
        self.max_sources.store(value, Ordering::Relaxed);
    }

    pub(crate) fn set_parts_done(&self, value: u32) {
        self.parts_done.store(value, Ordering::Relaxed);
    }

    /// 进入阶段；`until` 为预计下一次找源 / 申请槽位的 unix 秒（未知传 0）。
    pub(crate) fn set_phase(&self, phase: TaskPhase, until: i64) {
        lock(&self.inner).phase = phase;
        self.next_search_at.store(until, Ordering::Relaxed);
    }

    pub(crate) fn publish_peers(&self, peers: Vec<PeerSlot>) {
        lock(&self.inner).peers = peers;
    }

    /// 一轮服务器查询开始：把本轮要查询的服务器标为查询中（保留上轮结果里不在本轮的行）。
    pub(crate) fn servers_querying(&self, targets: &[(String, ServerOrigin)]) {
        let mut inner = lock(&self.inner);
        for (addr, origin) in targets {
            match inner.servers.iter_mut().find(|row| &row.addr == addr) {
                Some(row) => {
                    row.state = ServerState::Querying;
                    row.origin = *origin;
                }
                None => inner.servers.push(ServerRow {
                    addr: addr.clone(),
                    origin: *origin,
                    ..ServerRow::default()
                }),
            }
        }
    }

    /// 一轮服务器查询结束：`results` 为各服务器返回的源数或失败原因。
    pub(crate) fn servers_answered(&self, results: &[ServerQuery]) {
        let now = unix_now();
        let mut inner = lock(&self.inner);
        for row in &mut inner.servers {
            if let Some(ServerQuery { result, .. }) =
                results.iter().find(|answer| answer.addr == row.addr)
            {
                row.last_query_at = now;
                match result {
                    Ok(count) => {
                        row.state = ServerState::Working;
                        row.sources = Some(*count);
                        row.message.clear();
                    }
                    Err(message) => {
                        row.state = ServerState::NotWorking;
                        row.sources = None;
                        row.message.clone_from(message);
                    }
                }
            } else if row.state == ServerState::Querying {
                // 查询被取消或整体失败，没有这台服务器的结果。
                row.state = ServerState::NotWorking;
            }
        }
    }

    fn view(&self, session_server: &str) -> TaskView {
        let (phase, slots, mut servers) = {
            let inner = lock(&self.inner);
            (inner.phase, inner.peers.clone(), inner.servers.clone())
        };
        let in_flight = lock(&self.progress).clone();
        let mut peers: Vec<(Source, PeerRow)> = slots
            .into_iter()
            .map(|slot| {
                let row = peer_row(&slot, &in_flight);
                (slot.source, row)
            })
            .collect();
        self.apply_speeds(&mut peers);
        let mut peers: Vec<PeerRow> = peers.into_iter().map(|(_, row)| row).collect();
        peers.sort_by(|a, b| {
            a.state
                .rank()
                .cmp(&b.state.rank())
                .then_with(|| b.down_bps.cmp(&a.down_bps))
                .then_with(|| a.queue_rank.cmp(&b.queue_rank))
                .then_with(|| b.downloaded.cmp(&a.downloaded))
                .then_with(|| a.addr.cmp(&b.addr))
                .then_with(|| a.low_id.cmp(&b.low_id))
        });
        for row in &mut servers {
            row.connected = !session_server.is_empty() && row.addr == session_server;
        }
        TaskView {
            phase,
            next_search_at: self.next_search_at.load(Ordering::Relaxed),
            parts_done: self.parts_done.load(Ordering::Relaxed),
            max_sources: self.max_sources.load(Ordering::Relaxed),
            peer_counts: PeerCounts::of(&peers),
            peers,
            servers,
            kad: self.kad.view(),
        }
    }

    /// 用上一次读取折算每个源的速度并更新采样。
    fn apply_speeds(&self, rows: &mut [(Source, PeerRow)]) {
        let mut sample = lock(&self.samples);
        let now = Instant::now();
        let elapsed_ms = sample.at.map_or(0, |at| now.duration_since(at).as_millis());
        let fresh = elapsed_ms >= SPEED_SAMPLE_MIN_MS;
        let mut next = HashMap::with_capacity(rows.len());
        for (source, row) in rows.iter_mut() {
            let bps = match sample.peers.get(source).copied() {
                Some((previous, _)) if fresh => {
                    let ms = u64::try_from(elapsed_ms).unwrap_or(u64::MAX).max(1);
                    row.downloaded.saturating_sub(previous).saturating_mul(1000) / ms
                }
                Some((_, bps)) => bps,
                None => 0,
            };
            // 只有在传的源有速度；排队 / 空闲连接上一轮残留的差值不算。
            row.down_bps = if row.state == PeerState::Transferring {
                bps
            } else {
                0
            };
            next.insert(*source, (row.downloaded, row.down_bps));
        }
        if fresh || sample.at.is_none() {
            sample.at = Some(now);
            sample.peers = next;
        }
    }
}

fn peer_row(slot: &PeerSlot, in_flight: &HashMap<u64, i64>) -> PeerRow {
    let (addr, low_id) = match slot.source {
        Source::HighId(peer) => (peer.to_string(), 0),
        Source::LowId(id) => (String::new(), id),
    };
    let mut row = PeerRow {
        addr,
        low_id,
        origin: slot.origin,
        message: slot.message.clone(),
        ..PeerRow::default()
    };
    let mut block = None;
    if let Some(live) = &slot.live {
        let info = lock(&live.info).clone();
        row.client = info.client;
        row.nickname = info.nickname;
        row.downloaded = live.verified_bytes.load(Ordering::Relaxed);
        row.parts = live.parts.load(Ordering::Relaxed).checked_sub(1);
        block = live.block.load(Ordering::Relaxed).checked_sub(1);
    }
    row.state = match slot.coarse {
        PeerCoarse::Known => PeerState::Known,
        PeerCoarse::Connected => PeerState::Connected,
        PeerCoarse::Failed => PeerState::Failed,
        PeerCoarse::Banned => PeerState::Banned,
        PeerCoarse::Cooldown(until) => {
            row.retry_at = until;
            PeerState::Cooldown
        }
        PeerCoarse::Busy => match slot.live.as_deref() {
            Some(live) => match live.stage.load(Ordering::Relaxed) {
                STAGE_QUEUED => {
                    row.queue_rank = live.queue_rank.load(Ordering::Relaxed);
                    PeerState::Queued
                }
                STAGE_TRANSFERRING => PeerState::Transferring,
                _ => PeerState::Connecting,
            },
            None => PeerState::Connecting,
        },
    };
    if slot.coarse == PeerCoarse::Busy
        && let Some(block) = block
    {
        let received = in_flight.get(&block).copied().unwrap_or(0);
        row.downloaded = row
            .downloaded
            .saturating_add(u64::try_from(received).unwrap_or(0));
        if row.state == PeerState::Transferring {
            row.part = u32::try_from(block).ok();
        }
    }
    row
}

static TASKS: LazyLock<StdMutex<HashMap<String, Arc<TaskLive>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// 任务运行期间的登记句柄；drop 时撤销（任务暂停 / 完成 / 失败后实况即消失）。
pub(crate) struct TaskRegistration {
    task_id: String,
    live: Arc<TaskLive>,
}

impl TaskRegistration {
    pub(crate) fn live(&self) -> &Arc<TaskLive> {
        &self.live
    }
}

impl Drop for TaskRegistration {
    fn drop(&mut self) {
        let mut tasks = lock(&TASKS);
        // 同一任务被快速暂停 → 恢复时，旧运行的 drop 可能晚于新运行的登记。
        if tasks
            .get(&self.task_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.live))
        {
            tasks.remove(&self.task_id);
        }
    }
}

/// 登记一个开始运行的任务。`progress` 与进度上报共用，用于折算在途块的字节。
pub(crate) fn register(
    task_id: &str,
    segment_count: i32,
    progress: Arc<StdMutex<HashMap<u64, i64>>>,
) -> TaskRegistration {
    let live = Arc::new(TaskLive {
        inner: StdMutex::new(TaskInner::default()),
        kad: KadLive::default(),
        parts_done: AtomicU32::new(0),
        max_sources: AtomicU32::new(0),
        segment_count: AtomicI32::new(segment_count),
        next_search_at: AtomicI64::new(0),
        progress,
        samples: StdMutex::new(SpeedSamples::default()),
    });
    lock(&TASKS).insert(task_id.to_owned(), Arc::clone(&live));
    TaskRegistration {
        task_id: task_id.to_owned(),
        live,
    }
}

/// 运行中任务的实况；任务未在运行返回 `None`。`session_server` 为本机当前登录的服务器
/// （用于标记服务器行）。
#[must_use]
pub fn task_view(task_id: &str, session_server: &str) -> Option<TaskView> {
    let live = lock(&TASKS).get(task_id).cloned()?;
    Some(live.view(session_server))
}

/// 把新的线程数设置交给运行中的任务（下一个调度点生效，不断开现有连接）。
/// 任务未在运行返回 `false`。
pub fn set_task_segments(task_id: &str, segments: i32) -> bool {
    match lock(&TASKS).get(task_id) {
        Some(live) => {
            live.segment_count.store(segments, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::ed2k::server::PeerAddr;

    fn high(last: u8) -> Source {
        Source::HighId(PeerAddr {
            ip: Ipv4Addr::new(10, 0, 0, last),
            port: 4662,
        })
    }

    fn slot(source: Source, coarse: PeerCoarse, live: Option<Arc<PeerLive>>) -> PeerSlot {
        PeerSlot {
            source,
            origin: PeerOrigin::Server,
            coarse,
            live,
            message: String::new(),
        }
    }

    #[test]
    fn busy_peer_reports_stage_and_in_flight_bytes() {
        let progress = Arc::new(StdMutex::new(HashMap::from([(3_u64, 500_i64)])));
        let registration = register("status-busy", 0, Arc::clone(&progress));
        let live = Arc::new(PeerLive::default());
        live.finish(1_000);
        live.begin(3, false);
        registration.live().publish_peers(vec![slot(
            high(1),
            PeerCoarse::Busy,
            Some(Arc::clone(&live)),
        )]);

        let view = task_view("status-busy", "").expect("registered");
        assert_eq!(view.peers[0].state, PeerState::Connecting);
        assert_eq!(view.peers[0].downloaded, 1_500);
        assert_eq!(view.peers[0].part, None, "只有传输中才报告块序号");

        live.set_queued(7);
        let view = task_view("status-busy", "").expect("registered");
        assert_eq!(view.peers[0].state, PeerState::Queued);
        assert_eq!(view.peers[0].queue_rank, 7);

        live.set_transferring();
        let view = task_view("status-busy", "").expect("registered");
        assert_eq!(view.peers[0].state, PeerState::Transferring);
        assert_eq!(view.peers[0].queue_rank, 0);
        assert_eq!(view.peers[0].part, Some(3));
        assert_eq!(view.peer_counts.transferring, 1);
    }

    #[test]
    fn idle_connection_ignores_stale_block_bytes() {
        // 块失败后连接被丢弃、源回到「已连接 / 冷却」：残留的在途字节不得再计入该源。
        let progress = Arc::new(StdMutex::new(HashMap::from([(0_u64, 900_i64)])));
        let registration = register("status-idle", 0, progress);
        let live = Arc::new(PeerLive::default());
        live.begin(0, true);
        registration.live().publish_peers(vec![
            slot(high(1), PeerCoarse::Cooldown(42), Some(Arc::clone(&live))),
            slot(Source::LowId(77), PeerCoarse::Banned, None),
        ]);

        let view = task_view("status-idle", "").expect("registered");
        let cooling = view
            .peers
            .iter()
            .find(|row| row.state == PeerState::Cooldown)
            .expect("cooldown row");
        assert_eq!(cooling.downloaded, 0);
        assert_eq!(cooling.retry_at, 42);
        let banned = view
            .peers
            .iter()
            .find(|row| row.state == PeerState::Banned)
            .expect("banned row");
        assert!(banned.addr.is_empty());
        assert_eq!(banned.low_id, 77);
        assert_eq!(view.peer_counts.total, 2);
        assert_eq!(view.peer_counts.cooldown, 1);
        assert_eq!(view.peer_counts.banned, 1);
    }

    #[test]
    fn peers_sort_by_activity() {
        let registration = register("status-sort", 0, Arc::default());
        let transferring = Arc::new(PeerLive::default());
        transferring.begin(0, true);
        let queued = Arc::new(PeerLive::default());
        queued.begin(1, false);
        queued.set_queued(3);
        registration.live().publish_peers(vec![
            slot(high(1), PeerCoarse::Failed, None),
            slot(high(2), PeerCoarse::Known, None),
            slot(high(3), PeerCoarse::Busy, Some(queued)),
            slot(high(4), PeerCoarse::Connected, None),
            slot(high(5), PeerCoarse::Busy, Some(transferring)),
        ]);

        let view = task_view("status-sort", "").expect("registered");
        let states: Vec<PeerState> = view.peers.iter().map(|row| row.state).collect();
        assert_eq!(
            states,
            [
                PeerState::Transferring,
                PeerState::Connected,
                PeerState::Queued,
                PeerState::Known,
                PeerState::Failed,
            ]
        );
    }

    #[test]
    fn server_rows_follow_query_rounds() {
        let registration = register("status-servers", 0, Arc::default());
        let live = registration.live();
        live.servers_querying(&[
            ("1.1.1.1:4661".to_owned(), ServerOrigin::Manual),
            ("2.2.2.2:4661".to_owned(), ServerOrigin::Subscription),
            ("3.3.3.3:4661".to_owned(), ServerOrigin::Subscription),
        ]);
        live.servers_answered(&[
            ServerQuery {
                addr: "1.1.1.1:4661".to_owned(),
                result: Ok(4),
            },
            ServerQuery {
                addr: "2.2.2.2:4661".to_owned(),
                result: Err("login timed out".to_owned()),
            },
        ]);

        let view = task_view("status-servers", "1.1.1.1:4661").expect("registered");
        assert_eq!(view.servers[0].state, ServerState::Working);
        assert_eq!(view.servers[0].sources, Some(4));
        assert!(view.servers[0].connected);
        assert_eq!(view.servers[1].state, ServerState::NotWorking);
        assert_eq!(view.servers[1].message, "login timed out");
        assert!(!view.servers[1].connected);
        // 没有结果的服务器不会停在「查询中」。
        assert_eq!(view.servers[2].state, ServerState::NotWorking);
        assert_eq!(view.servers[2].last_query_at, 0);

        // 下一轮只查询其中一台：其余行保留上轮结果。
        live.servers_querying(&[("2.2.2.2:4661".to_owned(), ServerOrigin::Subscription)]);
        let view = task_view("status-servers", "").expect("registered");
        assert_eq!(view.servers[0].state, ServerState::Working);
        assert_eq!(view.servers[1].state, ServerState::Querying);
    }

    #[test]
    fn registration_drop_keeps_a_newer_run() {
        let old = register("status-rerun", 0, Arc::default());
        let new = register("status-rerun", 4, Arc::default());
        drop(old);
        assert!(set_task_segments("status-rerun", 2), "新运行仍在登记中");
        assert_eq!(new.live().segment_count(), 2);
        drop(new);
        assert!(task_view("status-rerun", "").is_none());
        assert!(!set_task_segments("status-rerun", 2));
    }
}
