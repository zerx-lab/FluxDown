//! [`LinkManager`] —— 设备互联子系统门面。
//!
//! 聚合身份、名册存储（[`LinkStorage`]）、配对协议（响应方 + 发起方）、mDNS 发现、
//! 可扩展传输栈，供宿主（agent / 冻结的 server）驱动。宿主只跟本门面 +
//! 一个事件通道打交道。
//!
//! # 角色
//! - **响应方**（被添加设备）：生成配对码、处理 `hello`/`confirm`、mDNS 广播。
//! - **发起方**（正在添加设备）：mDNS 浏览、`begin_pairing`（发 hello、算 SAS）、
//!   `confirm_pairing`（发 confirm、落库）。
//! - **数据面（v2）**：`dispatch`（把下载下发给已配对设备）、`exchange_peer_info`（双向交换
//!   设备信息）、`remove_device`（解除配对并尽力通知对端）、`authorize`（校验入站请求并解密）、
//!   `answer_peer_info` / `answer_unpair`（响应端处理）。请求体先 AEAD 加密再对密文做 MAC，
//!   **响应体同样被封装**，发起方只在响应成功解封后才相信对端；线格式见 [`crate::crypto`]。
//! - **连通性维护**：名册非空且本机可被局域网访问时常驻 mDNS 存在性广播；宿主发现设备长时间
//!   离线时可调用 [`LinkManager::browse_for`] 短时浏览，线索用于下一轮拨号。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use tokio::sync::mpsc;

use super::address::PeerAddress;
use super::crypto::{
    LINK_AUTH_SKEW_SECS, LINK_CONTROL_VERSION, LINK_ENC_V2, LINK_PROTOCOL_VERSION, LinkExchange,
    derive_link_aead_key, is_fingerprint_shaped, link_auth_tag, open_link_body, request_aad,
    response_aad, seal_link_body, verify_link_auth_tag,
};
use super::discovery::{self, BrowseEvent, MdnsAdvertiser, MdnsBrowser};
use super::error::{LinkError, LinkResult};
use super::identity::LinkIdentity;
use super::pairing::{
    HelloRequest, HelloResponse, PAIRING_PROTOCOL_VERSION, PairingInitiator, PairingResponder,
    RevealRequest, RevealResponse, SelfInfo,
};
use super::storage::LinkStorage;
use super::text::{
    MAX_NAME_CHARS, MAX_PLATFORM_CHARS, MAX_VERSION_CHARS, is_valid_cloud_device_id,
    sanitize_optional, sanitize_text,
};
use super::transport::{DirectTransport, PeerConn};
use super::types::{
    DiscoveredPeer, DiscoveryKind, MAX_DIRECT_CANDIDATES, PeerCandidate, PeerInfo, PeerRecord,
    TransportKind,
};
use super::wire::{self, classify_failure, read_json_object, send_failure};

/// 数据面：经已认证链路交换设备信息的路径（与 `fluxdown_api::routes::API_LINK_INFO` 一致）。
pub const LINK_INFO_PATH: &str = "/api/v1/link/info";
/// 数据面：下发下载任务的路径（与 `fluxdown_api::routes::API_LINK_TASKS` 一致）。
pub const LINK_TASKS_PATH: &str = "/api/v1/link/tasks";
/// 数据面：通知对端解除配对的路径（与 `fluxdown_api::routes::API_LINK_UNPAIR` 一致）。
pub const LINK_UNPAIR_PATH: &str = "/api/v1/link/unpair";

/// 解除配对时通知在线对端的总时限（含拨号）。对端不可达 / 太慢不阻断本地删除。
const UNPAIR_NOTIFY_TIMEOUT: Duration = Duration::from_secs(3);

/// 请求头：发送方指纹 / 时间戳 / nonce / MAC / 方案版本。
const HEADER_DEVICE: &str = "X-FluxLink-Device";
const HEADER_TS: &str = "X-FluxLink-Ts";
const HEADER_NONCE: &str = "X-FluxLink-Nonce";
const HEADER_TAG: &str = "X-FluxLink-Tag";
const HEADER_ENC: &str = "X-FluxLink-Enc";

/// 设备互联事件，由宿主消费并转换为协议通知。
#[derive(Debug, Clone)]
pub enum LinkEngineEvent {
    /// mDNS/手动发现到一台设备。
    Discovered(DiscoveredPeer),
    /// 一个 mDNS 服务实例消失（对端下线 / 记录过期），发现快照里对应条目已移除：宿主应重新发布
    /// 发现快照。
    DiscoveredLost,
    /// 一台设备完成配对并入册（含 link_secret，宿主转 UI 前须剥除敏感字段）。
    Paired(PeerRecord),
    /// 一台设备被解除配对（fingerprint）。
    Unpaired(String),
    /// 对端经已认证链路通知本机：它已解除与本机的配对。记录已标记 `revoked` 并持久化。
    Revoked(String),
    /// 有设备正在向本机发起配对，等待本机用户核对 SAS 后批准/拒绝。
    IncomingPairing {
        session_id: String,
        sas: String,
        peer_name: String,
        peer_platform: Option<String>,
        /// 发起方 Ed25519 身份指纹（设备 ID）。
        peer_fingerprint: String,
        /// 发起方请求「完全控制」本机：批准即授权它经 `/rpc` 操作本机。
        requests_control: bool,
    },
    /// 子系统错误（供 UI 提示）。
    Error(String),
    /// 发起方放弃/拒绝了一次入站配对：响应方会话已移除，宿主应关闭对应的待确认请求。
    IncomingCancelled { session_id: String },
}

/// 响应方处理一次入站 `confirm` 的终局。四种都是协议的正常结果，**不是错误**——
/// 发起方要据此给出准确提示，不能一律显示「会话过期」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairConfirmOutcome {
    /// 本机用户批准，发起方已入册。
    Paired,
    /// 发起方自己传了 `confirm=false`。
    Declined,
    /// 本机用户核对 SAS 后拒绝。
    Rejected,
    /// 等待本机用户核验超时（60s 决策窗口耗尽）。
    TimedOut,
}

impl PairConfirmOutcome {
    /// 是否真的完成了配对。
    #[must_use]
    pub fn paired(self) -> bool {
        matches!(self, Self::Paired)
    }

    /// 供 HTTP 响应体透出的稳定判别串（发起方据此还原语义）；`Paired`/`Declined`
    /// 无需额外理由。
    #[must_use]
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Rejected => Some("rejected"),
            Self::TimedOut => Some("timeout"),
            Self::Paired | Self::Declined => None,
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 发现快照去重键：优先用指纹（mDNS TXT 记录携带，稳定）；探测阶段指纹未知
/// 时退化到 `host:port`。
fn discovered_peer_key(p: &DiscoveredPeer) -> String {
    match &p.fingerprint {
        Some(fp) => fp.clone(),
        None => format!("{}:{}", p.host, p.port),
    }
}

/// 按 [`discovered_peer_key`] 去重更新发现快照：已存在则原地覆盖（刷新地址/
/// 名称等可能变化的字段），否则追加。
fn upsert_discovered(snapshot: &mut Vec<DiscoveredPeer>, peer: DiscoveredPeer) {
    let key = discovered_peer_key(&peer);
    match snapshot.iter_mut().find(|e| discovered_peer_key(e) == key) {
        Some(existing) => *existing = peer,
        None => snapshot.push(peer),
    }
}

/// 从发现快照里移除一个已消失的 mDNS 服务实例（实例名 = 设备指纹前 12 位，见
/// [`discovery::BrowseEvent::Lost`]）；返回是否有条目被移除。只动 mDNS 来源的条目——手动探测加入的
/// 设备没有「下线」概念。
fn remove_lost_instance(snapshot: &mut Vec<DiscoveredPeer>, instance: &str) -> bool {
    let before = snapshot.len();
    snapshot.retain(|peer| {
        !(matches!(peer.kind, DiscoveryKind::Mdns)
            && peer
                .fingerprint
                .as_deref()
                .is_some_and(|fingerprint| fingerprint.starts_with(instance)))
    });
    snapshot.len() != before
}

/// 发起方待确认会话（[`PendingInit`]）存活上限。**必须**与响应方
/// `pairing.rs` 的 `SESSION_TTL`（180s）保持一致——双端各自独立维护一套
/// 过期判定，[`LinkManager::begin_pairing`] 的剪枝与
/// [`LinkManager::confirm_pairing`] 的本地过期检查共用这一个具名常量，
/// 未来两端 TTL 一起改，不会各自漂移。
const PENDING_INIT_TTL: Duration = Duration::from_secs(180);

/// 手动探测（`/ping`）得到的设备指纹的保留时长：配对时用来校验对端出示的身份，
/// 过久的探测结果不再可信（设备可能已换身份）。
const PROBED_FINGERPRINT_TTL: Duration = Duration::from_secs(600);

/// 探测指纹表的条数上限，超出时淘汰最旧的一条，保证内存占用有界。
const MAX_PROBED_FINGERPRINTS: usize = 64;

/// 数据面 nonce 的长度上限与字符集：发起方用 UUID（32 hex），这里放宽到 `[A-Za-z0-9_-]{1,64}`。
const MAX_NONCE_LEN: usize = 64;

/// 发起方待确认会话（begin_pairing 与 confirm_pairing 之间的状态）。
struct PendingInit {
    initiator: PairingInitiator,
    session_id: String,
    peer: PeerAddress,
    created: std::time::Instant,
    /// 一次 `accept=true` 的 confirm 正在等对端用户表态（最长约 70s）。
    confirming: bool,
    /// 通知在途的 confirm 中止（用户在「等待对方确认」时取消 / 关闭面板）。
    cancel: Arc<tokio::sync::Notify>,
}

/// [`LinkManager`] 的宿主相关选项。
#[derive(Debug, Clone)]
pub struct LinkOptions {
    /// 本机 fluxdown API 端口（mDNS 广播 + 自报候选用）。
    pub api_port: u16,
    /// 本机 API 是否可被局域网内其它设备访问。`false`（如桌面 agent 默认只监听
    /// 127.0.0.1）时不做 mDNS 广播、配对时也不向对端自报回连地址——那些地址对端
    /// 根本连不上。
    pub reachable: bool,
    /// 是否允许 mDNS 广播（`FLUXDOWN_MDNS=off` 的开关）。`false` 时仍可用配对码 +
    /// 手动地址配对，只是不在局域网内广播本机。
    pub advertise: bool,
    /// 覆盖本机自报的回连地址（配对 `initiatorAddrs` 与 `/info` 的 `addrs`）；空 = 自动探测。
    /// 容器 / NAS / 多网卡下自动探测到的往往是对端连不通的内部地址。每条是 `host:port` 或完整
    /// URL，至多 [`MAX_DIRECT_CANDIDATES`] 条；运行期可经 [`LinkManager::set_advertise_addrs`] 改。
    pub advertise_addrs: Vec<String>,
}

impl LinkOptions {
    /// 局域网可达且允许广播（保持既有发现行为）。
    #[must_use]
    pub fn reachable(api_port: u16) -> Self {
        Self {
            api_port,
            reachable: true,
            advertise: true,
            advertise_addrs: Vec::new(),
        }
    }
}

/// 设备互联门面。宿主持 `Arc<LinkManager>`。
pub struct LinkManager {
    identity: LinkIdentity,
    store: Arc<dyn LinkStorage>,
    /// 响应方持有本机自报信息（设备名 / 云设备 ID，运行期可变）。
    responder: PairingResponder,
    transport: DirectTransport,
    client: reqwest::Client,
    api_port: AtomicU16,
    reachable: bool,
    advertise: bool,
    /// 覆盖自报的回连地址（候选形态，已规范化）。
    advertise_addrs: RwLock<Vec<String>>,
    events: mpsc::Sender<LinkEngineEvent>,
    advertiser: Mutex<Option<MdnsAdvertiser>>,
    /// 配对窗口（配对码有效期内）：广播让发起方能发现本机。
    pairing_open: AtomicBool,
    /// 名册里有未被解除的设备：常驻存在性广播，让对端在本机换 IP / 端口后能重新找到。
    roster_present: AtomicBool,
    /// 宿主显式要求长期广播（[`LinkManager::start_advertising`]）。
    forced_advertising: AtomicBool,
    browser: Mutex<Option<MdnsBrowser>>,
    /// 用户界面是否开着发现：开着才把未配对设备汇入发现快照 / 事件；自愈浏览只收集回连线索。
    ui_discovery: Arc<AtomicBool>,
    pending: Mutex<HashMap<String, PendingInit>>,
    /// 数据面防重放：已见过的 `device:nonce` → 过期时刻（Unix 秒）。过期时刻取
    /// `max(请求 ts, 当前时间) + LINK_AUTH_SKEW_SECS`：发送方时钟超前时，同一份请求在整个
    /// 「ts ± 容忍窗」内都能通过时间窗，nonce 必须保留到窗口真正关闭为止。
    ///
    /// **纯内存态，无持久化**：进程重启即清空全部历史。为堵住「截获合法 `(ts, nonce, tag)` 后在
    /// 响应方重启后、原始 `ts` 仍在 `LINK_AUTH_SKEW_SECS` 窗口内重放一次」，[`Self::authorize`]
    /// 拒绝时间戳**早于本进程启动时刻**（[`Self::started_at`]）的请求：重启之前发出的请求其 `ts`
    /// 必然早于重启，重放不了。
    seen_nonces: Mutex<HashMap<String, i64>>,
    /// 本进程（本 `LinkManager`）的启动时刻（Unix 秒）。取舍：发送方时钟比本机慢 `δ` 秒时，
    /// 它在本机启动后的头 `δ`（≤ `LINK_AUTH_SKEW_SECS`）秒内发来的合法请求 `ts` 也会早于启动时刻，
    /// 被当作时钟偏差拒绝；对端的周期性信息交换 / 重试在之后自然恢复，换来的是重启重放窗口
    /// 彻底关闭。
    started_at: AtomicI64,
    /// 发现快照（发起方侧）：浏览转发任务里按 [`upsert_discovered`]
    /// 去重更新；`start_discovery` 调用时清空；`probe` 的结果不入此快照。
    /// `Arc` 包裹以便转发任务（`tokio::spawn` 的 `'static` 闭包）持有写句柄。
    discovered: Arc<Mutex<Vec<DiscoveredPeer>>>,
    /// mDNS 广播给出、尚未经认证链路验证的回连候选（指纹 → 候选）。仅存内存，
    /// 拨号时**排在已验证候选之前**（换 IP 后新地址最可能有效），但只有认证请求经它拿到可验证的
    /// 封装响应后才由 [`Self::promote_hinted`] 写入名册。每个已配对指纹至多一条，天然有界。
    hinted: Arc<Mutex<HashMap<String, PeerCandidate>>>,
    /// 手动探测（`/ping`）得到的设备指纹（`PeerAddress::base_url` → 指纹 + 记录时刻）。
    /// 与 [`Self::discovered`] 一起，是发起方配对时比对对端身份的“发现指纹”来源，见
    /// [`Self::known_fingerprints`]。带 TTL 与条数上限。
    probed: Mutex<HashMap<String, (String, std::time::Instant)>>,
}

/// 清洗宿主给出的本机自报信息：设备名剔除控制 / 双向控制字符并限长（空则用 `FluxDown`），
/// 云设备 ID 只接受 UUID 形态。
fn clean_self_info(mut info: SelfInfo) -> SelfInfo {
    info.name = sanitize_text(&info.name, MAX_NAME_CHARS);
    if info.name.is_empty() {
        info.name = "FluxDown".to_owned();
    }
    info.platform = sanitize_optional(info.platform.as_deref(), MAX_PLATFORM_CHARS);
    info.app_version = sanitize_optional(info.app_version.as_deref(), MAX_VERSION_CHARS);
    info.cloud_device_id = info
        .cloud_device_id
        .filter(|id| is_valid_cloud_device_id(id));
    info
}

/// 把一组宿主给出的地址规范化为候选形态（非法的丢弃、去重、截断）。
fn normalize_advertised(addrs: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for addr in addrs {
        let Ok(parsed) = PeerAddress::parse(addr) else {
            tracing::warn!(address = %addr, "ignoring an unusable advertised link address");
            continue;
        };
        let candidate = parsed.to_candidate();
        if !normalized.contains(&candidate) {
            normalized.push(candidate);
        }
    }
    normalized.truncate(MAX_DIRECT_CANDIDATES);
    normalized
}

impl LinkManager {
    /// 从存储加载（或首次生成并持久化）本机身份，构造门面。
    ///
    /// 身份种子「存在但无法解析」时存储后端返回错误，本函数随之失败——绝不静默生成新身份
    /// 覆盖它：指纹一变，所有已配对设备立即失效。
    pub async fn load(
        storage: Arc<dyn LinkStorage>,
        self_info: SelfInfo,
        options: LinkOptions,
        events: mpsc::Sender<LinkEngineEvent>,
    ) -> LinkResult<Arc<Self>> {
        let identity = Self::load_or_create_identity(storage.as_ref()).await?;
        let client = wire::http_client();
        let responder = PairingResponder::new(identity.clone(), clean_self_info(self_info));
        let transport = DirectTransport::new(client.clone());
        let mgr = Arc::new(Self {
            identity,
            store: storage,
            responder,
            transport,
            client,
            api_port: AtomicU16::new(options.api_port),
            reachable: options.reachable,
            advertise: options.advertise,
            advertise_addrs: RwLock::new(normalize_advertised(&options.advertise_addrs)),
            events,
            advertiser: Mutex::new(None),
            pairing_open: AtomicBool::new(false),
            roster_present: AtomicBool::new(false),
            forced_advertising: AtomicBool::new(false),
            browser: Mutex::new(None),
            ui_discovery: Arc::new(AtomicBool::new(false)),
            pending: Mutex::new(HashMap::new()),
            seen_nonces: Mutex::new(HashMap::new()),
            started_at: AtomicI64::new(now_unix()),
            discovered: Arc::new(Mutex::new(Vec::new())),
            hinted: Arc::new(Mutex::new(HashMap::new())),
            probed: Mutex::new(HashMap::new()),
        });
        Self::spawn_gc(&mgr);
        mgr.refresh_presence().await;
        Ok(mgr)
    }

    /// 启动后台周期清理任务：每 60s 剪枝 `pending`/`seen_nonces`，并转发给
    /// 响应方剪枝其 `codes`/`sessions`（见
    /// [`super::pairing::PairingResponder::prune_expired`]）。状态表若只在「恰好又发生
    /// 一次同类调用」时顺带清理，半途放弃的配对/过期 nonce 记录会一直驻留到进程重启才被回收。
    ///
    /// 持 `Weak` 而非 `Arc`：循环体自身绝不能成为 `LinkManager` 存活的
    /// 理由——宿主释放最后一个 `Arc` 后，下一次 `upgrade()` 必然失败，循环
    /// 随之退出；否则这个 `'static` 后台任务会永久多攥一份 `Arc`，
    /// `LinkManager`（及其持有的 `advertiser`/`browser` 等资源）就再也
    /// 不会被析构。
    fn spawn_gc(mgr: &Arc<Self>) {
        let weak = Arc::downgrade(mgr);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let Some(mgr) = weak.upgrade() else {
                    break;
                };
                mgr.prune_expired();
            }
        });
    }

    async fn load_or_create_identity(storage: &dyn LinkStorage) -> LinkResult<LinkIdentity> {
        if let Some(seed) = storage.load_identity_seed().await? {
            return Ok(LinkIdentity::from_secret_bytes(&seed));
        }
        let identity = LinkIdentity::generate();
        storage.save_identity_seed(&identity.secret_bytes()).await?;
        Ok(identity)
    }

    async fn emit(&self, event: LinkEngineEvent) {
        if self.events.send(event).await.is_err() {
            tracing::debug!("link event receiver closed");
        }
    }

    /// 本机设备指纹（设备 ID）。
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        self.identity.fingerprint()
    }

    /// 本机当前自报信息（快照）。
    #[must_use]
    pub fn self_info(&self) -> SelfInfo {
        self.responder.self_info()
    }

    /// 本机展示名（供 `/ping` 透出）。
    #[must_use]
    pub fn self_name(&self) -> String {
        self.responder.self_info().name
    }

    /// 本机平台（供 `/ping` 透出）。
    #[must_use]
    pub fn self_platform(&self) -> Option<String> {
        self.responder.self_info().platform
    }

    /// 更新本机自报的设备名与云设备 ID（改名、云账号登录 / 登出后调用）；返回是否有变化。
    /// 设备名变化会同步到已开启的 mDNS 广播、`/ping`、之后的配对 hello 与信息交换；云设备 ID
    /// 只经已认证的信息交换告知已配对对端。
    pub fn update_self(&self, name: &str, cloud_device_id: Option<&str>) -> LinkResult<bool> {
        let current = self.responder.self_info();
        let next = clean_self_info(SelfInfo {
            name: name.to_owned(),
            platform: current.platform.clone(),
            app_version: current.app_version.clone(),
            cloud_device_id: cloud_device_id.map(str::to_owned),
        });
        let name_changed = next.name != current.name;
        if !name_changed && next.cloud_device_id == current.cloud_device_id {
            return Ok(false);
        }
        self.responder.set_self_info(next.clone());
        if name_changed {
            let advertiser = self.advertiser.lock().map_err(|_| LinkError::Unavailable)?;
            if let Some(advertiser) = advertiser.as_ref()
                && let Err(error) = advertiser.update(
                    self.api_port.load(Ordering::Relaxed),
                    self.identity.fingerprint(),
                    &next.name,
                    next.platform.as_deref(),
                    next.app_version.as_deref(),
                )
            {
                tracing::warn!(error = %error, "could not refresh the mDNS advertisement after a rename");
            }
        }
        Ok(true)
    }

    /// 更新覆盖自报的回连地址（见 [`LinkOptions::advertise_addrs`]）；返回是否有变化。
    pub fn set_advertise_addrs(&self, addrs: &[String]) -> bool {
        let next = normalize_advertised(addrs);
        let mut current = match self.advertise_addrs.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *current == next {
            return false;
        }
        *current = next;
        true
    }

    /// 更新实际 API 端口及已开启的 mDNS 广播；不改变可达范围、身份、配对或浏览状态。
    ///
    /// 广播更新失败时保留旧端口并恢复旧广告。未开始广播时只更新未来的自报候选与广播端口。
    pub fn update_api_port(&self, api_port: u16) -> LinkResult<()> {
        let advertiser = self.advertiser.lock().map_err(|_| LinkError::Unavailable)?;
        let old_port = self.api_port.load(Ordering::Relaxed);
        if let Some(advertiser) = advertiser.as_ref() {
            let me = self.responder.self_info();
            let update = |port| {
                advertiser.update(
                    port,
                    self.identity.fingerprint(),
                    &me.name,
                    me.platform.as_deref(),
                    me.app_version.as_deref(),
                )
            };
            if let Err(error) = update(api_port) {
                // register 的通知 socket 失败时，命令可能已经排队；必须显式恢复旧广告。
                update(old_port).map_err(|rollback| {
                    LinkError::Io(format!(
                        "mDNS port update failed: {error}; restoring previous port failed: {rollback}"
                    ))
                })?;
                return Err(error);
            }
        }
        self.api_port.store(api_port, Ordering::Relaxed);
        Ok(())
    }

    // ── 响应方（被添加设备侧）─────────────────────────────────────────────

    /// 生成一次性配对码（在被添加设备 UI 展示），并确保 mDNS 广播已开启。
    pub fn generate_code(&self) -> String {
        self.pairing_open.store(true, Ordering::Relaxed);
        self.ensure_advertising();
        self.responder.generate_code()
    }

    /// 处理入站 `hello`（wire 形式，base64 编解码全在引擎内完成，宿主纯字段搬运）。
    /// `source`：请求来源地址，直接透传给 [`PairingResponder::handle_hello`] 做按来源
    /// 分桶节流；拿不到时传 `None`。
    ///
    /// 这一步只回出本次会话的临时公钥与随机数：响应方此刻还看不到发起方的临时公钥，
    /// 没有 SAS，也不会广播 `IncomingPairing`——见 [`Self::pair_reveal_wire`]。
    pub fn pair_hello_wire(
        &self,
        w: WireHello,
        source: Option<IpAddr>,
    ) -> LinkResult<WireHelloResponse> {
        // 版本门禁先于字段解码：旧版发起方的 hello 没有 `initiatorCommit` 等新字段，
        // 先解码只会得到含糊的「载荷非法」，而不是明确的版本不兼容。
        if w.protocol_version != PAIRING_PROTOCOL_VERSION {
            return Err(LinkError::UnsupportedVersion);
        }
        let req = HelloRequest {
            protocol_version: w.protocol_version,
            code: w.code,
            initiator_id_pub: decode_b64_array::<32>(&w.initiator_id_pub)?,
            initiator_commit: decode_b64_array::<32>(&w.initiator_commit)?,
            initiator_sig: decode_b64_array::<64>(&w.initiator_sig)?,
            name: w.name,
            platform: w.platform,
            app_version: w.app_version,
            initiator_addrs: w.initiator_addrs,
            request_control: w.request_control,
        };
        let resp = self.responder.handle_hello(&req, source)?;
        Ok(WireHelloResponse {
            protocol_version: resp.protocol_version,
            session_id: resp.session_id,
            responder_eph_pub: B64.encode(resp.responder_eph_pub),
            responder_nonce: B64.encode(resp.responder_nonce),
            responder_id_pub: B64.encode(resp.responder_id_pub),
            name: resp.name,
            platform: resp.platform,
            app_version: resp.app_version,
        })
    }

    /// 处理入站 `reveal`（wire 形式）：核对承诺、算出 SAS，随后向宿主广播
    /// `IncomingPairing`（本机用户从这一刻起核对 SAS），并把响应方对完整转录的签名回给
    /// 发起方。回复里不含 SAS——SAS 只在两端各自的屏幕上展示。
    pub fn pair_reveal_wire(&self, w: WireReveal) -> LinkResult<WireRevealResponse> {
        let req = RevealRequest {
            session_id: w.session_id,
            initiator_eph_pub: decode_b64_array::<32>(&w.initiator_eph_pub)?,
            initiator_nonce: decode_b64_array::<32>(&w.initiator_nonce)?,
        };
        let accepted = self.responder.handle_reveal(&req)?;
        self.emit_incoming_pairing(
            req.session_id,
            accepted.sas,
            accepted.peer_name,
            accepted.peer_platform,
            &accepted.peer_id_pub,
            accepted.request_control,
        );
        Ok(WireRevealResponse {
            responder_sig: B64.encode(accepted.response.responder_sig),
        })
    }

    /// 处理入站 `confirm`：本机用户批准则把发起方入册并广播 `Paired` 事件。
    ///
    /// 返回 [`PairConfirmOutcome`] 而非 `bool`：本机用户**拒绝**与**核验超时**都是
    /// 协议的正常终局（不是服务端错误），必须能被发起方区分——它们若经 `Err` 冒泡成
    /// HTTP 400，发起方的 [`Self::confirm_pairing`] 见非 2xx 一律映射成
    /// `SessionExpired`，用户看到的是「会话过期」而不是「对方拒绝了配对」。
    pub async fn pair_confirm(
        &self,
        session_id: &str,
        confirm: bool,
    ) -> LinkResult<PairConfirmOutcome> {
        match self.responder.handle_confirm(session_id, confirm).await {
            Ok(Some(record)) => {
                // 重新配对同一指纹：新记录覆盖旧记录，`revoked` 随之清除。
                self.store.upsert(&record).await?;
                self.emit(LinkEngineEvent::Paired(record)).await;
                self.refresh_presence().await;
                Ok(PairConfirmOutcome::Paired)
            }
            // 发起方自己传了 confirm=false —— 它当然知道自己拒绝了，无需额外语义。
            Ok(None) => {
                // 发起方放弃/拒绝：响应方会话已被删除，通知宿主关闭待确认弹窗。
                self.emit(LinkEngineEvent::IncomingCancelled {
                    session_id: session_id.to_string(),
                })
                .await;
                Ok(PairConfirmOutcome::Declined)
            }
            Err(LinkError::RejectedByPeer) => Ok(PairConfirmOutcome::Rejected),
            Err(LinkError::PairingTimeout) => Ok(PairConfirmOutcome::TimedOut),
            Err(e) => Err(e),
        }
    }

    /// 批准/拒绝一次入站配对请求（本机用户核对 SAS 后调用）。转发到响应方
    /// 的本地决策记录，唤醒 [`super::pairing::PairingResponder::handle_confirm`]
    /// 里等待用户核验、且已收到发起方 `confirm=true` 的那次 HTTP 请求。
    pub fn approve_incoming(&self, session_id: &str, accept: bool) -> LinkResult<()> {
        self.responder.set_local_decision(session_id, accept)
    }

    /// 后台广播「有入站配对待核对」事件。`events` 是 async `mpsc::Sender`，
    /// 本方法的调用方 `pair_reveal_wire` 是同步 fn，沿用
    /// [`Self::ensure_advertising`] 已有的 `tokio::spawn` 写法把发送挪到
    /// 后台，不阻塞 reveal 的返回路径。
    fn emit_incoming_pairing(
        &self,
        session_id: String,
        sas: String,
        peer_name: String,
        peer_platform: Option<String>,
        peer_id_pub: &[u8; 32],
        requests_control: bool,
    ) {
        let tx = self.events.clone();
        let event = LinkEngineEvent::IncomingPairing {
            session_id,
            sas,
            peer_name,
            peer_platform,
            peer_fingerprint: super::crypto::fingerprint(peer_id_pub),
            requests_control,
        };
        tokio::spawn(async move {
            if tx.send(event).await.is_err() {
                tracing::debug!("link event receiver closed before incoming pairing notification");
            }
        });
    }

    /// 是否应当保持 mDNS 广播：配对窗口开着、名册里有未解除的设备，或宿主显式要求。
    fn advertising_needed(&self) -> bool {
        self.pairing_open.load(Ordering::Relaxed)
            || self.roster_present.load(Ordering::Relaxed)
            || self.forced_advertising.load(Ordering::Relaxed)
    }

    /// 确保 mDNS 广播运行（幂等）。本机 API 局域网不可达（[`LinkOptions::reachable`]
    /// 为 `false`）时不广播。失败仅记 Error 事件，不阻断配对（手动地址可兜底）。
    /// 广播只含指纹 / 名称 / 平台 / 版本 / 端口，**不含**任何配对标志或云账号信息。
    fn ensure_advertising(&self) {
        if !self.reachable || !self.advertise {
            return;
        }
        let mut guard = match self.advertiser.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_some() {
            return;
        }
        let me = self.responder.self_info();
        match MdnsAdvertiser::start(
            self.api_port.load(Ordering::Relaxed),
            self.identity.fingerprint(),
            &me.name,
            me.platform.as_deref(),
            me.app_version.as_deref(),
        ) {
            Ok(a) => *guard = Some(a),
            Err(e) => {
                let tx = self.events.clone();
                let msg = e.to_string();
                tokio::spawn(async move {
                    if tx.send(LinkEngineEvent::Error(msg)).await.is_err() {
                        tracing::debug!(
                            "link event receiver closed before advertising failure notification"
                        );
                    }
                });
            }
        }
    }

    /// 主动开启并长期保持 mDNS 广播（宿主常驻服务，如 headless server）。
    pub fn start_advertising(&self) {
        self.forced_advertising.store(true, Ordering::Relaxed);
        self.ensure_advertising();
    }

    /// 结束配对窗口并撤回广播（配对码过期 / 用户关闭配对界面时调用）。名册里还有未解除的设备、
    /// 或宿主要求长期广播时，存在性广播继续保持（对端据此在本机换地址后重新找到本机）。
    pub fn stop_advertising(&self) {
        self.pairing_open.store(false, Ordering::Relaxed);
        if self.advertising_needed() {
            return;
        }
        if let Ok(mut guard) = self.advertiser.lock() {
            *guard = None; // Drop → daemon.shutdown()
        }
    }

    /// 停止配对：立即作废当前配对码并撤回配对窗口的广播。
    pub fn stop_pairing(&self) {
        self.responder.revoke_codes();
        self.stop_advertising();
    }

    /// 按名册当前内容刷新「常驻存在性广播」：有未解除的设备则开启（本机可被局域网访问且允许
    /// 广播时），否则撤回。名册变化（配对、解除、对端解除）后调用。
    pub async fn refresh_presence(&self) {
        let present = match self.store.list().await {
            Ok(records) => records.iter().any(|record| !record.revoked),
            Err(error) => {
                tracing::warn!(error = %error, "cannot read the linked roster to refresh presence");
                return;
            }
        };
        self.roster_present.store(present, Ordering::Relaxed);
        if present {
            self.ensure_advertising();
        } else if !self.advertising_needed()
            && let Ok(mut guard) = self.advertiser.lock()
        {
            *guard = None;
        }
    }

    // ── 发现（发起方侧）───────────────────────────────────────────────────

    /// 开始 mDNS 浏览：发现的设备经事件通道以 `Discovered` 汇出，并同步去重
    /// 更新发现快照。幂等——重复调用不重启浏览，但仍清空发现快照，与管理面
    /// `POST /link/discovery {"action":"start"}` 的语义对齐。
    /// 命中已配对指纹的广播不入快照/不发事件，而是作为该设备的回连线索（见
    /// [`Self::browse_for`]）。
    pub fn start_discovery(&self) -> LinkResult<()> {
        if let Ok(mut snapshot) = self.discovered.lock() {
            snapshot.clear();
        }
        // `FLUXDOWN_MDNS=off`：界面发起的浏览同样不起（手动地址配对照常可用）。
        if !self.advertise {
            return Ok(());
        }
        self.ui_discovery.store(true, Ordering::Relaxed);
        self.ensure_browser()
    }

    /// 浏览器已在运行则什么都不做；否则启动，并起转发任务处理发现结果。
    fn ensure_browser(&self) -> LinkResult<()> {
        let mut guard = self.browser.lock().map_err(|_| LinkError::Unavailable)?;
        if guard.is_some() {
            return Ok(());
        }
        let (tx, mut rx) = mpsc::channel::<BrowseEvent>(64);
        let out = self.events.clone();
        let hinted = Arc::clone(&self.hinted);
        let self_fp = self.identity.fingerprint().to_string();
        let discovered = Arc::clone(&self.discovered);
        let ui_discovery = Arc::clone(&self.ui_discovery);
        let store = self.store.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let peer = match event {
                    BrowseEvent::Found(peer) => peer,
                    BrowseEvent::Lost { instance } => {
                        let removed = discovered.lock().is_ok_and(|mut snapshot| {
                            remove_lost_instance(&mut snapshot, &instance)
                        });
                        if removed && out.send(LinkEngineEvent::DiscoveredLost).await.is_err() {
                            break;
                        }
                        continue;
                    }
                };
                // 过滤掉本机自身的广播。
                if peer.fingerprint.as_deref() == Some(self_fp.as_str()) {
                    continue;
                }
                // 命中已配对名册（且对端没有解除配对）：多半是设备 DHCP 换了 IP 后重新广播——
                // 记为回连线索，不当作「可添加的新设备」推进 discovered 快照/事件通道
                // （否则已配对设备会污染「发现列表」UI）。已被对端解除的设备按未配对处理，
                // 以便用户重新配对。
                if let Some(fp) = peer.fingerprint.as_deref()
                    && let Ok(Some(record)) = store.get(fp).await
                    && !record.revoked
                {
                    let Ok(address) = PeerAddress::from_host_port(&peer.host, peer.port) else {
                        continue;
                    };
                    let fresh = PeerCandidate {
                        kind: TransportKind::Direct,
                        address: address.to_candidate(),
                    };
                    // mDNS 通告的指纹是公开值，地址未经任何认证：不写库、不挤占
                    // 已验证候选，只作为内存中的待验证线索；认证请求经它拿到可验证的
                    // 封装响应后才持久化。已知地址/重复线索直接跳过。
                    if record.candidates.contains(&fresh) {
                        continue;
                    }
                    if let Ok(mut map) = hinted.lock() {
                        map.insert(fp.to_string(), fresh);
                    }
                    continue;
                }
                // 用户没开发现（自愈浏览）时，未配对设备既不进快照也不发事件。
                if !ui_discovery.load(Ordering::Relaxed) {
                    continue;
                }
                if let Ok(mut snapshot) = discovered.lock() {
                    upsert_discovered(&mut snapshot, peer.clone());
                }
                if out.send(LinkEngineEvent::Discovered(peer)).await.is_err() {
                    break;
                }
            }
        });
        *guard = Some(MdnsBrowser::start(tx)?);
        Ok(())
    }

    /// 停止 mDNS 浏览。
    pub fn stop_discovery(&self) {
        self.ui_discovery.store(false, Ordering::Relaxed);
        if let Ok(mut guard) = self.browser.lock() {
            *guard = None; // Drop → daemon.shutdown()
        }
    }

    /// 短时 mDNS 浏览 `duration`，只为收集已配对设备的回连线索（设备换 IP / 端口后重新
    /// 广播的新地址），线索用于之后的拨号。用户界面已开着发现时复用同一个浏览器、直接返回；
    /// mDNS 被禁用（[`LinkOptions::advertise`] 为 `false`）时什么都不做。期间用户打开发现，
    /// 浏览器归用户界面所有，到期不会被关掉。
    pub async fn browse_for(&self, duration: Duration) -> LinkResult<()> {
        if !self.advertise || self.ui_discovery.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.ensure_browser()?;
        tokio::time::sleep(duration).await;
        if !self.ui_discovery.load(Ordering::Relaxed)
            && let Ok(mut guard) = self.browser.lock()
        {
            *guard = None;
        }
        Ok(())
    }

    /// 当前发现快照（发起方侧 UI 轮询用）；`start_discovery` 时清空，浏览期间
    /// 持续去重更新。
    #[must_use]
    pub fn discovered_peers(&self) -> Vec<DiscoveredPeer> {
        self.discovered
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// 手动地址探测（mDNS 失效兜底）：`/ping` 一台设备，返回其信息（不配对）。探测到的
    /// 设备指纹会被记住一小段时间，随后对同一地址发起配对时用来校验对端身份。
    pub async fn probe(&self, address: &PeerAddress) -> LinkResult<DiscoveredPeer> {
        let peer = discovery::probe(&self.client, address).await?;
        if let Some(fingerprint) = &peer.fingerprint {
            self.remember_probe(address, fingerprint);
        }
        Ok(peer)
    }

    /// 记住一次探测得到的指纹；先剪掉过期项，满员时淘汰最旧的一条。
    fn remember_probe(&self, address: &PeerAddress, fingerprint: &str) {
        let Ok(mut probed) = self.probed.lock() else {
            return;
        };
        probed.retain(|_, (_, at)| at.elapsed() < PROBED_FINGERPRINT_TTL);
        let key = address.base_url();
        if probed.len() >= MAX_PROBED_FINGERPRINTS
            && !probed.contains_key(&key)
            && let Some(oldest) = probed
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
        {
            probed.remove(&oldest);
        }
        probed.insert(key, (fingerprint.to_string(), std::time::Instant::now()));
    }

    /// 发起方已知的、`address` 上设备的指纹：mDNS 发现快照里落在该地址的条目，加上最近
    /// 一次对该地址手动探测得到的指纹。配对握手里响应方出示的身份必须命中其中之一；
    /// 没有任何发现记录（直接输入地址配对）时返回空，不做这项比对。
    fn known_fingerprints(&self, address: &PeerAddress) -> Vec<String> {
        let base = address.base_url();
        let mut known = Vec::new();
        if let Ok(probed) = self.probed.lock()
            && let Some((fingerprint, at)) = probed.get(&base)
            && at.elapsed() < PROBED_FINGERPRINT_TTL
        {
            known.push(fingerprint.clone());
        }
        if let Ok(snapshot) = self.discovered.lock() {
            known.extend(snapshot.iter().filter_map(|peer| {
                let fingerprint = peer.fingerprint.as_ref()?;
                let peer_address = PeerAddress::from_host_port(&peer.host, peer.port).ok()?;
                (peer_address.base_url() == base).then(|| fingerprint.clone())
            }));
        }
        known
    }

    // ── 配对（发起方侧）───────────────────────────────────────────────────

    /// 发起配对：向 `address` 依次发送 `hello`（带配对码与临时公钥承诺）和 `reveal`
    /// （揭示临时公钥），返回 `(token, sas, 对端名)`。UI 展示 SAS 供用户与对端核对，
    /// 随后调 [`confirm_pairing`]。
    ///
    /// `request_control`：同时请求「完全控制」对端（受 hello 签名保护）。对端用户批准后本机
    /// 记录标记 `controllable`，可用 [`Self::control_credential`] 取得其 `/rpc` 的设备控制凭据。
    /// 先读对端 `/ping`：没有 `linkControl >= 1`（旧版对端不认识该标志）→
    /// [`LinkError::UnsupportedVersion`]，不发 hello。
    ///
    /// 发起方已知该地址上设备的发现指纹（mDNS 快照或最近一次手动探测）时，对端出示的
    /// 身份必须与之一致，否则 [`LinkError::IdentityMismatch`]——此时还没有揭示临时
    /// 公钥。对端配对协议版本不符（含旧版对端）→ [`LinkError::UnsupportedVersion`]。
    ///
    /// 错误语义（见 [`crate::wire`]）：网络失败 → [`LinkError::Io`]；对端（或其反代）返回
    /// 非 FluxDown 的 4xx / 重定向 / HTML → [`LinkError::NotFluxDown`]；只有对端 FluxDown
    /// 明确回了「配对码无效」才是 [`LinkError::InvalidCode`]。
    pub async fn begin_pairing(
        &self,
        address: &PeerAddress,
        code: &str,
        request_control: bool,
    ) -> LinkResult<BeginPairingResult> {
        if request_control
            && discovery::probe_link_control(&self.client, address).await? < LINK_CONTROL_VERSION
        {
            return Err(LinkError::UnsupportedVersion);
        }
        let mut initiator =
            PairingInitiator::new(self.identity.clone()).request_control(request_control);
        let addrs = self.self_addrs_towards(address).await;
        let hello = initiator.build_hello(code, &self.responder.self_info(), addrs);

        let mut body = serde_json::json!({
            "protocolVersion": hello.protocol_version,
            "code": hello.code,
            "initiatorIdPub": B64.encode(hello.initiator_id_pub),
            "initiatorCommit": B64.encode(hello.initiator_commit),
            "initiatorSig": B64.encode(hello.initiator_sig),
            "name": hello.name,
            "platform": hello.platform.clone().unwrap_or_default(),
            "appVersion": hello.app_version.clone().unwrap_or_default(),
            "initiatorAddrs": hello.initiator_addrs,
        });
        if hello.request_control {
            body["requestControl"] = serde_json::Value::Bool(true);
        }
        let json = self
            .post_pairing_step(address, "/api/v1/link/pair/hello", &body)
            .await
            .map_err(hello_rejection)?;
        let hello_resp = parse_hello_response(&json)?;

        let responder_addr = address.to_candidate();
        let known = self.known_fingerprints(address);
        let reveal = initiator.on_hello_response(&hello_resp, &responder_addr, &known)?;

        let body = serde_json::json!({
            "sessionId": reveal.session_id,
            "initiatorEphPub": B64.encode(reveal.initiator_eph_pub),
            "initiatorNonce": B64.encode(reveal.initiator_nonce),
        });
        let json = self
            .post_pairing_step(address, "/api/v1/link/pair/reveal", &body)
            .await?;
        let reveal_resp = parse_reveal_response(&json)?;
        let sas = initiator.on_reveal_response(&reveal_resp)?;

        let token = uuid::Uuid::new_v4().simple().to_string();
        let peer_fingerprint = super::crypto::fingerprint(&hello_resp.responder_id_pub);
        let peer_name = {
            let cleaned = sanitize_text(&hello_resp.name, MAX_NAME_CHARS);
            if cleaned.is_empty() {
                peer_fingerprint.chars().take(12).collect()
            } else {
                cleaned
            }
        };
        if let Ok(mut pending) = self.pending.lock() {
            // 剪枝：丢弃用户已放弃、超过会话时窗的待确认项，避免无界增长 + 及时释放临时密钥。
            pending.retain(|_, p| p.created.elapsed() < PENDING_INIT_TTL);
            // 剪枝后仍达到硬上限（短时间内连续发起多次配对但都未确认）——
            // 拒绝新请求而非无界增长；正常使用不可能同时挂 32 个待确认配对。
            if pending.len() >= 32 {
                return Err(LinkError::Unavailable);
            }
            pending.insert(
                token.clone(),
                PendingInit {
                    initiator,
                    session_id: hello_resp.session_id.clone(),
                    peer: address.clone(),
                    created: std::time::Instant::now(),
                    confirming: false,
                    cancel: Arc::new(tokio::sync::Notify::new()),
                },
            );
        }
        Ok(BeginPairingResult {
            token,
            sas,
            peer_name,
            peer_fingerprint,
        })
    }

    /// 向对端的配对端点 POST 一步握手消息（`hello` / `reveal`），返回成功响应的 JSON
    /// 对象；失败按 [`crate::wire`] 的规则分类。
    async fn post_pairing_step(
        &self,
        address: &PeerAddress,
        path: &str,
        body: &serde_json::Value,
    ) -> LinkResult<serde_json::Value> {
        let resp = self
            .client
            .post(address.url(path))
            .json(body)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| send_failure(&e))?;
        if !resp.status().is_success() {
            return Err(classify_failure(resp).await);
        }
        read_json_object(resp).await
    }

    /// 本机朝向 `peer` 的可回连地址（`initiatorAddrs`，供对端存为回连候选）。本机 API
    /// 局域网不可达、或对端是 https 站点（多半是公网反代，回连无意义）时为空。
    async fn self_addrs_towards(&self, peer: &PeerAddress) -> Vec<String> {
        self.advertised_addrs(Some(peer)).await
    }

    /// 本机自报的可回连地址：覆盖地址（[`LinkOptions::advertise_addrs`]）优先；否则朝向
    /// `towards`（对端地址）做出站网卡探测，没有对端地址可参照时按各常见网段的默认出站网卡。
    /// 本机 API 局域网不可达、或对端是 https 站点时为空。
    async fn advertised_addrs(&self, towards: Option<&PeerAddress>) -> Vec<String> {
        if !self.reachable || towards.is_some_and(|peer| peer.is_https()) {
            return Vec::new();
        }
        let configured = match self.advertise_addrs.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if !configured.is_empty() {
            return configured;
        }
        let port = self.api_port.load(Ordering::Relaxed);
        let Some(peer) = towards else {
            return discovery::default_route_addrs(port);
        };
        let host = peer.host();
        let ip_text = if host.parse::<IpAddr>().is_ok() {
            host.to_string()
        } else {
            // 域名先解析成 IP（下面的出站网卡探测需要 IP）。
            match tokio::net::lookup_host((host, peer.port())).await {
                Ok(mut resolved) => resolved
                    .next()
                    .map(|addr| addr.ip().to_string())
                    .unwrap_or_default(),
                Err(_) => String::new(),
            }
        };
        discovery::local_direct_addrs(&ip_text, port)
    }

    /// SAS 核对后确认/拒绝配对。`accept=true` 且对端确认成功 → 落库 + 广播 Paired。
    ///
    /// 待确认条目保留到 confirm 结束。`accept=true` 的调用在等对端用户表态（最长约 70s）期间，同一
    /// token 上的 `accept=false` 就是**中止**：立即通知对端放弃（对端弹窗随之关闭），在途调用以
    /// [`LinkError::SessionExpired`] 尽快返回，取消调用本身返回 `Ok(None)`。
    pub async fn confirm_pairing(
        &self,
        token: &str,
        accept: bool,
    ) -> LinkResult<Option<PeerRecord>> {
        let (session_id, peer, cancel) = {
            let mut guard = self.pending.lock().map_err(|_| LinkError::Unavailable)?;
            let Some(entry) = guard.get_mut(token) else {
                return Err(LinkError::SessionExpired);
            };
            // 本地也校验过期：双端各自维护一套过期逻辑，未来任一端常量独立调整就会行为不一致。
            // 本机用户迟迟不核验 SAS 时在这里直接短路，不必再发起一次注定失败、还要等待
            // 下方最多 70s 超时的网络往返。
            if entry.created.elapsed() > PENDING_INIT_TTL {
                guard.remove(token);
                return Err(LinkError::SessionExpired);
            }
            let info = (
                entry.session_id.clone(),
                entry.peer.clone(),
                entry.cancel.clone(),
            );
            if accept {
                if entry.confirming {
                    // 同一待确认项上已有在途确认：重复确认没有意义。
                    return Err(LinkError::SessionExpired);
                }
                entry.confirming = true;
            } else if entry.confirming {
                // 中止在途确认：条目由它自己在返回前移除。
                entry.cancel.notify_one();
            } else {
                guard.remove(token);
            }
            info
        };
        let body = serde_json::json!({ "sessionId": session_id, "confirm": accept });
        let request = self
            .client
            .post(peer.url("/api/v1/link/pair/confirm"))
            .json(&body)
            // 对端响应方要等本机用户在 confirm 阶段核验 SAS 并点击批准/拒绝
            // （PairingResponder::handle_confirm 等待用户决策，上限 60s），因此这里的超时
            // 必须盖过那 60s 决策窗口再留出网络往返余量，否则发起方会在对端用户点确认前就
            // 先行超时掉线。
            .timeout(Duration::from_secs(70));
        if !accept {
            confirm_response(request).await?;
            return Ok(None);
        }
        let outcome = tokio::select! {
            outcome = confirm_response(request) => outcome,
            () = cancel.notified() => Err(LinkError::SessionExpired),
        };
        // 无论结局如何，这个待确认项到此为止。
        let pending = self
            .pending
            .lock()
            .map_err(|_| LinkError::Unavailable)?
            .remove(token);
        let json = outcome?;
        let Some(pending) = pending else {
            return Err(LinkError::SessionExpired);
        };
        // 对端 confirm 响应体 `{"success":true,"paired":<bool>,"reason":<"rejected"|
        // "timeout"|null>}`。对端用户拒绝与核验超时都是**协议正常终局**，对端以 2xx +
        // `paired=false` 返回（而非 4xx），否则非 2xx 分支会把它们一律压成
        // SessionExpired，用户看到的是「会话过期」而不是「对方拒绝了配对」。
        let paired = json
            .get("paired")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !paired {
            let reason = json.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            return Err(match reason {
                "timeout" => LinkError::PairingTimeout,
                _ => LinkError::RejectedByPeer,
            });
        }
        let record = pending.initiator.finalize()?;
        // 重新配对同一指纹：新记录覆盖旧记录，`revoked` 随之清除。
        self.store.upsert(&record).await?;
        self.emit(LinkEngineEvent::Paired(record.clone())).await;
        self.refresh_presence().await;
        Ok(Some(record))
    }

    // ── 名册 ──────────────────────────────────────────────────────────────

    /// 全部已配对设备。
    pub async fn list_devices(&self) -> LinkResult<Vec<PeerRecord>> {
        self.store.list().await
    }

    /// 按指纹读取单台已配对设备。
    pub async fn get_device(&self, fingerprint: &str) -> LinkResult<Option<PeerRecord>> {
        self.store.get(fingerprint).await
    }

    /// 解除配对：对**在线**对端经已认证链路尽力通知（总时限 [`UNPAIR_NOTIFY_TIMEOUT`]，失败 /
    /// 超时不阻断），再本地删除并广播 `Unpaired`。已被对端解除（`revoked`）的记录不再通知。
    pub async fn remove_device(&self, fingerprint: &str) -> LinkResult<bool> {
        if let Some(record) = self.store.get(fingerprint).await?
            && !record.revoked
        {
            self.notify_unpair(record).await;
        }
        let removed = self.store.remove(fingerprint).await?;
        if removed {
            self.emit(LinkEngineEvent::Unpaired(fingerprint.to_string()))
                .await;
            self.refresh_presence().await;
        }
        Ok(removed)
    }

    /// 尽力通知对端「本机已解除配对」。整段（拨号 + 请求）受 [`UNPAIR_NOTIFY_TIMEOUT`] 约束，
    /// 任何失败只记日志。
    async fn notify_unpair(&self, record: PeerRecord) {
        let fingerprint = record.fingerprint.clone();
        let notify = async {
            let record = self.with_hinted(record);
            let conn = self.transport.connect(&record).await?;
            self.post_sealed(
                &record,
                &conn,
                LINK_UNPAIR_PATH,
                br#"{"v":2}"#,
                UNPAIR_NOTIFY_TIMEOUT,
            )
            .await
            .map(|_| ())
        };
        match tokio::time::timeout(UNPAIR_NOTIFY_TIMEOUT, notify).await {
            Ok(Ok(())) => tracing::debug!(fingerprint, "peer notified about the unpairing"),
            Ok(Err(error)) => {
                tracing::debug!(fingerprint, error = %error, "could not notify the peer about the unpairing");
            }
            Err(_) => tracing::debug!(
                fingerprint,
                "notifying the peer about the unpairing timed out"
            ),
        }
    }

    /// 设置 / 清除已配对设备的本机备注名（仅本机可见）。`name` 为 `None` 或全空白 = 清除；
    /// 否则去首尾空白后须为 1–64 个字符且不含控制字符，否则 [`LinkError::BadPayload`]。
    /// 返回更新后的记录；设备不存在 → [`LinkError::NotPaired`]。
    pub async fn rename_device(
        &self,
        fingerprint: &str,
        name: Option<&str>,
    ) -> LinkResult<PeerRecord> {
        let custom = match name.map(str::trim).filter(|name| !name.is_empty()) {
            None => None,
            Some(name) => {
                if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
                    return Err(LinkError::BadPayload("invalid device name".to_owned()));
                }
                Some(name.to_owned())
            }
        };
        let updated = self
            .store
            .update_record(
                fingerprint,
                Box::new(move |record| {
                    if record.custom_name == custom {
                        false
                    } else {
                        record.custom_name = custom;
                        true
                    }
                }),
            )
            .await?;
        if updated.is_none() {
            return Err(LinkError::NotPaired);
        }
        self.store
            .get(fingerprint)
            .await?
            .ok_or(LinkError::NotPaired)
    }

    /// 把 mDNS 待验证候选放到**本次拨号**候选的最前面（不持久化）：设备换 IP 后新地址最可能
    /// 有效，旧地址留作回退。
    fn with_hinted(&self, mut record: PeerRecord) -> PeerRecord {
        if let Ok(map) = self.hinted.lock()
            && let Some(hint) = map.get(&record.fingerprint)
        {
            record.candidates.retain(|candidate| candidate != hint);
            record.candidates.insert(0, hint.clone());
        }
        record
    }

    /// 认证请求（链路密钥 MAC / AEAD 往返成功，且响应经封装校验）经 `base_url` 完成后，若该地址
    /// 正是待验证候选，则将其置首并持久化；否则不动名册。
    async fn promote_hinted(&self, fingerprint: &str, base_url: &str) {
        let hint = match self.hinted.lock() {
            Ok(map) => map.get(fingerprint).cloned(),
            Err(_) => None,
        };
        let Some(hint) = hint else {
            return;
        };
        let matches = PeerAddress::parse(&hint.address)
            .map(|a| a.base_url() == base_url)
            .unwrap_or(false);
        if !matches {
            return;
        }
        let record = match self.store.get(fingerprint).await {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(fingerprint, %error, "cannot load linked peer for candidate promotion");
                return;
            }
        };
        let candidates = promoted_candidates(&record.candidates, hint);
        if candidates != record.candidates
            && let Err(error) = self.store.update_candidates(fingerprint, &candidates).await
        {
            tracing::warn!(fingerprint, %error, "cannot persist verified linked peer candidate");
            return;
        }
        if let Ok(mut map) = self.hinted.lock()
            && map.get(fingerprint) == candidates.first()
        {
            map.remove(fingerprint);
        }
    }

    /// 探测一台已配对设备是否在线（走传输栈拨号），成功则刷新 last_seen。已被对端解除配对
    /// 的设备不再探测（恒为离线）。
    pub async fn is_online(&self, fingerprint: &str) -> bool {
        let Ok(Some(record)) = self.store.get(fingerprint).await else {
            return false;
        };
        if record.revoked {
            return false;
        }
        let record = self.with_hinted(record);
        match self.transport.connect(&record).await {
            Ok(_) => {
                if let Err(error) = self.store.touch(fingerprint, now_unix()).await {
                    tracing::warn!(fingerprint, %error, "cannot persist linked peer last_seen");
                }
                true
            }
            Err(LinkError::Unreachable) => false,
            Err(error) => {
                // 身份不符等：不是「暂时离线」，需要留下线索。
                tracing::warn!(fingerprint, error = %error, "linked device probe rejected");
                false
            }
        }
    }

    // ── 数据面（发起方）────────────────────────────────────────────────────

    /// 读取一台可下发 / 可交换信息的已配对设备：不存在或已被对端解除 → [`LinkError::NotPaired`]。
    async fn active_record(&self, fingerprint: &str) -> LinkResult<PeerRecord> {
        let record = self
            .store
            .get(fingerprint)
            .await?
            .ok_or(LinkError::NotPaired)?;
        if record.revoked {
            return Err(LinkError::NotPaired);
        }
        Ok(self.with_hinted(record))
    }

    /// 把一个下载任务下发给已配对设备（一次性投递）。走传输栈解析可达 base_url，请求体先 AEAD
    /// 加密再对**密文**做 MAC（encrypt-then-MAC），POST 对端 `/api/v1/link/tasks`；响应必须经
    /// 链路密钥解封成功才算成功。返回新任务 ID。对端数据面协议过旧 →
    /// [`LinkError::PeerOutdated`]（不发请求）；对端已解除配对 / 本机记录已被对端撤销 →
    /// [`LinkError::NotPaired`] / [`LinkError::Unauthorized`]。
    pub async fn dispatch(
        &self,
        fingerprint: &str,
        url: &str,
        save_dir: Option<&str>,
        file_name: Option<&str>,
    ) -> LinkResult<String> {
        let record = self.active_record(fingerprint).await?;
        let conn = self.transport.connect(&record).await?;
        // 明文序列化**一次**，加密**一次**——同一份密文字节既用于 MAC 也
        // 用于发送，保证签名覆盖的字节与对端收到并校验的字节完全一致
        // （Option 空值序列化为 ""，非 null，否则响应方 `LinkTaskRequest`
        // (非 Option String) 反序列化会 400）。
        let body_json = serde_json::json!({
            "url": url,
            "saveDir": save_dir.unwrap_or_default(),
            "fileName": file_name.unwrap_or_default(),
        });
        let plaintext = serde_json::to_vec(&body_json).unwrap_or_default();
        let reply = self
            .post_sealed(
                &record,
                &conn,
                LINK_TASKS_PATH,
                &plaintext,
                Duration::from_secs(15),
            )
            .await?;
        let json: serde_json::Value = serde_json::from_slice(&reply)
            .map_err(|_| LinkError::Io("malformed dispatch reply".into()))?;
        let task_id = json
            .get("taskId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| LinkError::Io("missing taskId in dispatch response".into()))?
            .to_string();
        // 到这里响应已经过链路密钥解封校验：对端确实持有配对密钥，才有资格把该地址写进名册。
        self.promote_hinted(fingerprint, &conn.base_url).await;
        if let Err(error) = self.store.touch(fingerprint, now_unix()).await {
            tracing::warn!(fingerprint, %error, "cannot persist linked peer last_seen");
        }
        Ok(task_id)
    }

    /// 数据面出站请求（v2）：对明文 AEAD 加密，再用每对独立链路密钥对**密文**做 MAC
    /// （encrypt-then-MAC，MAC 与 AAD 都绑定收发双方指纹），POST 到 `{base_url}{path}`；
    /// 成功响应必须带 `X-FluxLink-Enc: v2` 且能用同一密钥按响应 AAD 解封，否则按鉴权失败处理。
    /// 返回解封后的响应明文。
    async fn post_sealed(
        &self,
        record: &PeerRecord,
        conn: &PeerConn,
        path: &str,
        plaintext: &[u8],
        timeout: Duration,
    ) -> LinkResult<Vec<u8>> {
        if conn.link_protocol < LINK_PROTOCOL_VERSION {
            return Err(LinkError::PeerOutdated);
        }
        let ts = now_unix();
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let me = self.identity.fingerprint();
        let exchange = LinkExchange {
            method: "POST",
            path,
            nonce: &nonce,
            sender_fp: me,
            receiver_fp: &record.fingerprint,
        };
        let aead_key = derive_link_aead_key(&record.link_secret);
        let sealed = seal_link_body(&aead_key, &request_aad(&exchange), plaintext);
        let tag = link_auth_tag(&record.link_secret, &exchange, ts, &sealed);
        let resp = self
            .client
            .post(format!("{}{path}", conn.base_url))
            .header(HEADER_DEVICE, me)
            .header(HEADER_TS, ts.to_string())
            .header(HEADER_NONCE, nonce.as_str())
            .header(HEADER_TAG, tag)
            .header(HEADER_ENC, LINK_ENC_V2)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(sealed)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| send_failure(&e))?;
        if !resp.status().is_success() {
            return Err(classify_failure(resp).await);
        }
        let enc_ok = resp
            .headers()
            .get(HEADER_ENC)
            .and_then(|value| value.to_str().ok())
            == Some(LINK_ENC_V2);
        let body = resp.bytes().await.map_err(|e| send_failure(&e))?;
        if !enc_ok {
            return Err(LinkError::Unauthorized);
        }
        open_link_body(&aead_key, &response_aad(&exchange), &body).ok_or(LinkError::Unauthorized)
    }

    /// 组装本机经信息交换自报的 [`PeerInfo`]：名称 / 平台 / 版本 / 云设备 ID 取自本机自报信息，
    /// 目录与路径风格取自宿主传入的 `local`，回连地址按 [`Self::advertised_addrs`]。
    async fn compose_local_info(
        &self,
        local: &PeerInfo,
        towards: Option<&PeerAddress>,
    ) -> PeerInfo {
        let me = self.responder.self_info();
        PeerInfo {
            name: Some(me.name),
            platform: me.platform,
            app_version: me.app_version,
            addrs: self.advertised_addrs(towards).await,
            cloud_device_id: me.cloud_device_id,
            default_save_dir: local.default_save_dir.clone(),
            path_style: local.path_style.clone(),
        }
    }

    /// 把对端自报的信息并入名册记录；返回记录是否真的变化（只有变化才写盘）。
    async fn apply_peer_info(&self, fingerprint: &str, info: PeerInfo) -> LinkResult<bool> {
        self.store
            .update_record(
                fingerprint,
                Box::new(move |record| record.apply_peer_info(&info)),
            )
            .await?
            .ok_or(LinkError::NotPaired)
    }

    /// 经已认证链路与对端**双向交换设备信息**：本机信息随请求发给对端（对端据此更新它名册里的
    /// 本机记录），对端信息随响应回来并并入本机名册（备注名之外的名称 / 平台 / 版本 / 云设备 ID /
    /// 目录 / 路径风格，自报回连地址放候选首位）。`local` 只需携带宿主才知道的默认目录与路径风格，
    /// 其余自报字段由本方法填充。
    ///
    /// 返回名册记录是否**变化**：只有变化才落盘（空闲时周期性交换零写入），宿主据此决定是否通知界面。
    /// 对端数据面协议过旧 → [`LinkError::PeerOutdated`]。
    pub async fn exchange_peer_info(
        &self,
        fingerprint: &str,
        local: &PeerInfo,
    ) -> LinkResult<bool> {
        let record = self.active_record(fingerprint).await?;
        let conn = self.transport.connect(&record).await?;
        let mine = self
            .compose_local_info(local, PeerAddress::parse(&conn.base_url).ok().as_ref())
            .await;
        let reply = self
            .post_sealed(
                &record,
                &conn,
                LINK_INFO_PATH,
                &encode_peer_info(&mine),
                Duration::from_secs(8),
            )
            .await?;
        let info = decode_peer_info(&reply)
            .ok_or_else(|| LinkError::BadPayload("malformed peer info reply".to_owned()))?;
        self.promote_hinted(fingerprint, &conn.base_url).await;
        let changed = self.apply_peer_info(fingerprint, info).await?;
        if let Err(error) = self.store.touch(fingerprint, now_unix()).await {
            tracing::warn!(fingerprint, %error, "cannot persist linked peer last_seen");
        }
        Ok(changed)
    }

    // ── 数据面（响应端）────────────────────────────────────────────────────

    /// 响应端处理已通过 [`Self::authorize`] 的 `POST /api/v1/link/info`：并入请求方自报的信息，
    /// 返回封装好的本机信息 `mine`（响应体）以及名册记录是否变化。
    pub async fn answer_peer_info(
        &self,
        request: &LinkRequest,
        local: &PeerInfo,
    ) -> LinkResult<PeerInfoAnswer> {
        let theirs = decode_peer_info(&request.body)
            .ok_or_else(|| LinkError::BadPayload("malformed peer info request".to_owned()))?;
        let record = self
            .store
            .get(&request.device)
            .await?
            .ok_or(LinkError::Unauthorized)?;
        let towards = record
            .direct_address()
            .and_then(|address| PeerAddress::parse(address).ok());
        let changed = self.apply_peer_info(&request.device, theirs).await?;
        let mine = self.compose_local_info(local, towards.as_ref()).await;
        Ok(PeerInfoAnswer {
            sealed: request.seal_response(&encode_peer_info(&mine)),
            changed,
        })
    }

    /// 响应端处理已通过 [`Self::authorize`] 的 `POST /api/v1/link/unpair`：把请求方记录标记为
    /// `revoked` 并持久化（不删除，让用户看到「对方已解除」并自行删除或重新配对），广播
    /// [`LinkEngineEvent::Revoked`]。重复通知幂等。返回封装好的确认响应。
    pub async fn answer_unpair(&self, request: &LinkRequest) -> LinkResult<Vec<u8>> {
        let updated = self
            .store
            .update_record(
                &request.device,
                Box::new(|record| {
                    if record.revoked {
                        false
                    } else {
                        record.revoked = true;
                        true
                    }
                }),
            )
            .await?;
        match updated {
            None => return Err(LinkError::NotPaired),
            Some(true) => {
                self.emit(LinkEngineEvent::Revoked(request.device.clone()))
                    .await;
                self.refresh_presence().await;
            }
            Some(false) => {}
        }
        Ok(request.seal_response(br#"{"ok":true}"#))
    }

    /// 校验入站链路数据面请求（v2）：方案头 / 头部格式 / 时间窗（廉价检查，先于名册查询）→
    /// 取设备记录（已被对端解除的记录视同未配对）→ 验 MAC（覆盖密文摘要与收发双方指纹，
    /// encrypt-then-MAC）→ nonce 防重放 → 按请求 AAD 解密。成功返回发起方指纹 + **已解密**的
    /// 明文 body（见 [`LinkRequest`]），并携带封装响应所需的上下文。
    ///
    /// `enc` 是 `X-FluxLink-Enc` 请求头的原始值：缺失或不是 `"v2"`（含旧的 `"v1"`）一律当
    /// 鉴权失败拒绝，不留降级回退路径。错误只作诊断：[`LinkError::ClockSkew`]（时间窗）、
    /// [`LinkError::NotPaired`]（未配对 / 已解除）、[`LinkError::Unauthorized`]（其余）。
    #[allow(clippy::too_many_arguments)]
    pub async fn authorize(
        &self,
        method: &str,
        path: &str,
        device_fp: &str,
        ts: i64,
        nonce: &str,
        body: &[u8],
        tag: &str,
        enc: &str,
    ) -> LinkResult<LinkRequest> {
        if enc != LINK_ENC_V2 {
            return Err(LinkError::Unauthorized);
        }
        if !is_fingerprint_shaped(device_fp) || !is_nonce_shaped(nonce) || !is_tag_shaped(tag) {
            return Err(LinkError::Unauthorized);
        }
        let now = now_unix();
        // i128 比较，防攻击者构造的极端 ts 触发 i64 溢出（debug 下 panic）。
        if (now as i128 - ts as i128).abs() > LINK_AUTH_SKEW_SECS as i128 {
            return Err(LinkError::ClockSkew);
        }
        // 重启重放防护：早于本进程启动时刻的请求一律拒绝（取舍见 `started_at`）。
        if ts < self.started_at.load(Ordering::Relaxed) {
            return Err(LinkError::ClockSkew);
        }
        let record = self
            .store
            .get(device_fp)
            .await?
            .filter(|record| !record.revoked)
            .ok_or(LinkError::NotPaired)?;
        let me = self.identity.fingerprint();
        let exchange = LinkExchange {
            method,
            path,
            nonce,
            sender_fp: device_fp,
            receiver_fp: me,
        };
        // encrypt-then-MAC：`body` 此刻仍是密文，先验 MAC（覆盖密文摘要与收发双方指纹）
        // ——与发起方 [`Self::post_sealed`] 对同一份密文字节算 tag 的顺序一致。
        if !verify_link_auth_tag(&record.link_secret, &exchange, ts, body, tag) {
            return Err(LinkError::Unauthorized);
        }
        // 防重放：同 (device, nonce) 在时窗内仅接受一次。保留到 `max(ts, now) + SKEW`：
        // 发送方时钟超前时请求在整个 ts ± SKEW 内都有效，只按「收到时刻」计时会让 nonce 过早过期。
        {
            let mut seen = self
                .seen_nonces
                .lock()
                .map_err(|_| LinkError::Unavailable)?;
            seen.retain(|_, expires| *expires > now);
            let key = format!("{device_fp}:{nonce}");
            if seen.contains_key(&key) {
                return Err(LinkError::Unauthorized);
            }
            seen.insert(key, ts.max(now).saturating_add(LINK_AUTH_SKEW_SECS));
        }
        // MAC 通过后才解密：密文完整性此刻已由 MAC 保证；解密失败统一按鉴权失败处理，不额外泄露
        // 「MAC 过但解密失败」这种细分信息。
        let aead_key = derive_link_aead_key(&record.link_secret);
        let plaintext = open_link_body(&aead_key, &request_aad(&exchange), body)
            .ok_or(LinkError::Unauthorized)?;
        Ok(LinkRequest {
            device: device_fp.to_string(),
            body: plaintext,
            receiver: me.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            nonce: nonce.to_string(),
            aead_key,
        })
    }

    // ── 设备控制凭据（`/rpc`）──────────────────────────────────────────────

    /// 为「本机可控制」的已配对主机签发 `/rpc` 设备控制凭据（发起方 / 控制端侧）。仅当名册记录
    /// `controllable && !revoked` 时成功，否则 [`LinkError::NotPaired`]。凭据绑定双方指纹，由
    /// 配对时派生的 `link_secret` 做 HMAC，见 [`super::crypto::control_credential`]。
    pub async fn control_credential(&self, fingerprint: &str) -> LinkResult<String> {
        let record = self
            .store
            .get(fingerprint)
            .await?
            .filter(|record| record.controllable && !record.revoked)
            .ok_or(LinkError::NotPaired)?;
        super::crypto::control_credential(
            &record.link_secret,
            self.identity.fingerprint(),
            &record.fingerprint,
        )
        .ok_or(LinkError::NotPaired)
    }

    /// 校验对端出示的 `/rpc` 设备控制凭据（响应方 / 被控主机侧）：解析 → 按凭据里的指纹找名册
    /// 记录 → 要求 `grants_control && !revoked` → 重算 HMAC 常量时间比较。通过返回该设备指纹。
    /// 任何失败（含名册读取失败）一律 `None`：宁可拒绝也不放行。
    pub async fn verify_control_credential(&self, token: &str) -> Option<String> {
        let (fingerprint, _) = super::crypto::parse_control_credential(token)?;
        let record = match self.store.get(fingerprint).await {
            Ok(record) => record?,
            Err(error) => {
                tracing::warn!(error = %error, "could not read the link roster to verify a control credential");
                return None;
            }
        };
        (record.grants_control
            && !record.revoked
            && super::crypto::verify_control_credential(
                &record.link_secret,
                &record.fingerprint,
                self.identity.fingerprint(),
                token,
            ))
        .then_some(record.fingerprint)
    }

    /// 剪枝全部「只靠恰好又发生一次同类调用才顺带清理」的过期状态：
    /// `pending`（本机发起、SAS 核对/confirm 未完成的待确认会话）、
    /// `seen_nonces`（数据面防重放时窗）、`probed`（手动探测指纹），并转发给响应方
    /// 剪枝其 `codes`/`sessions`。由 [`Self::spawn_gc`] 每 60s 调用一次。
    pub fn prune_expired(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|_, p| p.created.elapsed() < PENDING_INIT_TTL);
        }
        if let Ok(mut seen) = self.seen_nonces.lock() {
            let now = now_unix();
            seen.retain(|_, expires| *expires > now);
        }
        if let Ok(mut probed) = self.probed.lock() {
            probed.retain(|_, (_, at)| at.elapsed() < PROBED_FINGERPRINT_TTL);
        }
        self.responder.prune_expired();
    }
}

/// [`LinkManager::begin_pairing`] 的结果：待确认令牌 + 供核对的 SAS + 对端信息。
#[derive(Debug, Clone)]
pub struct BeginPairingResult {
    pub token: String,
    pub sas: String,
    pub peer_name: String,
    pub peer_fingerprint: String,
}

/// [`LinkManager::answer_peer_info`] 的结果。
#[derive(Debug, Clone)]
pub struct PeerInfoAnswer {
    /// 封装好的响应体（`application/octet-stream`，响应头 `X-FluxLink-Enc: v2`）。
    pub sealed: Vec<u8>,
    /// 请求方的自报信息让本机名册记录发生了变化。
    pub changed: bool,
}

/// [`LinkManager::authorize`] 成功后的产出：通过链路鉴权的一次入站数据面请求——发起方指纹 +
/// **已解密**的明文 body，以及封装响应所需的上下文（方法 / 路径 / 请求 nonce / 链路 AEAD 密钥）。
///
/// 把「鉴权通过」与「拿到明文」在返回值里绑在一起，杜绝调用方漏改、继续拿密文当明文解析的错误
/// 用法；响应必须经 [`LinkRequest::seal_response`] 封装后再回给发起方。
#[derive(Clone)]
pub struct LinkRequest {
    /// 发起方设备指纹。
    pub device: String,
    /// 已解密的明文请求体。
    pub body: Vec<u8>,
    receiver: String,
    method: String,
    path: String,
    nonce: String,
    aead_key: [u8; 32],
}

impl std::fmt::Debug for LinkRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkRequest")
            .field("device", &self.device)
            .field("method", &self.method)
            .field("path", &self.path)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl LinkRequest {
    /// 封装响应体：用链路 AEAD 密钥 + 响应 AAD（绑定方向、方法、路径、双方指纹与**本次请求的
    /// nonce**）加密。HTTP 层必须同时回 `X-FluxLink-Enc: v2`。
    #[must_use]
    pub fn seal_response(&self, plaintext: &[u8]) -> Vec<u8> {
        let exchange = LinkExchange {
            method: &self.method,
            path: &self.path,
            nonce: &self.nonce,
            sender_fp: &self.device,
            receiver_fp: &self.receiver,
        };
        seal_link_body(&self.aead_key, &response_aad(&exchange), plaintext)
    }
}

/// nonce 形态：`[A-Za-z0-9_-]{1,64}`。
fn is_nonce_shaped(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NONCE_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// MAC 标签形态：64 位 hex（HMAC-SHA256）。
fn is_tag_shaped(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn b64_to_array<const N: usize>(json: &serde_json::Value, key: &str) -> LinkResult<[u8; N]> {
    let s = json
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinkError::BadPayload(format!("missing {key}")))?;
    let bytes = B64
        .decode(s)
        .map_err(|_| LinkError::BadPayload(format!("bad base64 {key}")))?;
    <[u8; N]>::try_from(bytes.as_slice())
        .map_err(|_| LinkError::BadPayload(format!("bad length {key}")))
}

/// 旧版响应方把本端按新协议构造的 `hello` 当成无法解析的载荷拒绝时返回的 message 前缀
/// （`invalid link hello payload: …`，由 API 层的 hello 处理器生成）。本端发出的 hello
/// 恒为良构，所以这类拒绝只可能意味着对端不认识新协议。
const HELLO_PAYLOAD_REJECTION_PREFIX: &str = "invalid link hello payload";

/// 把对端对 `hello` 的拒绝归类：旧版响应方的载荷拒绝 → [`LinkError::UnsupportedVersion`]，
/// 其余错误原样返回。
fn hello_rejection(error: LinkError) -> LinkError {
    match error {
        LinkError::BadPayload(message) if message.starts_with(HELLO_PAYLOAD_REJECTION_PREFIX) => {
            LinkError::UnsupportedVersion
        }
        other => other,
    }
}

fn parse_hello_response(json: &serde_json::Value) -> LinkResult<HelloResponse> {
    let get_str = |k: &str| {
        json.get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    // 没有 sessionId 说明对端根本不是 FluxDown 响应方——比如反代把请求转给了别的
    // JSON 服务。
    let session_id = get_str("sessionId")
        .ok_or_else(|| LinkError::NotFluxDown("hello response has no sessionId".into()))?;
    // 版本先于其它字段校验：旧版响应方的回复没有 protocolVersion（还带着明文 SAS 与
    // 它自己的签名），不能按新协议去解读。
    let protocol_version = json
        .get("protocolVersion")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    if protocol_version != PAIRING_PROTOCOL_VERSION {
        return Err(LinkError::UnsupportedVersion);
    }
    Ok(HelloResponse {
        protocol_version,
        session_id,
        responder_eph_pub: b64_to_array::<32>(json, "responderEphPub")?,
        responder_nonce: b64_to_array::<32>(json, "responderNonce")?,
        responder_id_pub: b64_to_array::<32>(json, "responderIdPub")?,
        name: get_str("name").unwrap_or_default(),
        platform: get_str("platform"),
        app_version: get_str("appVersion"),
    })
}

fn parse_reveal_response(json: &serde_json::Value) -> LinkResult<RevealResponse> {
    Ok(RevealResponse {
        responder_sig: b64_to_array::<64>(json, "responderSig")?,
    })
}

/// 入站 `hello` 的 wire 形式（base64 字符串字段），供 HTTP 宿主纯字段搬运。
#[derive(Debug, Clone)]
pub struct WireHello {
    pub protocol_version: u32,
    pub code: String,
    pub initiator_id_pub: String,
    pub initiator_commit: String,
    pub initiator_sig: String,
    pub name: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    pub initiator_addrs: Vec<String>,
    pub request_control: bool,
}

/// 出站 `hello` 回复的 wire 形式（base64 字符串字段）。
#[derive(Debug, Clone)]
pub struct WireHelloResponse {
    pub protocol_version: u32,
    pub session_id: String,
    pub responder_eph_pub: String,
    pub responder_nonce: String,
    pub responder_id_pub: String,
    pub name: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
}

/// 入站 `reveal` 的 wire 形式（base64 字符串字段）。
#[derive(Debug, Clone)]
pub struct WireReveal {
    pub session_id: String,
    pub initiator_eph_pub: String,
    pub initiator_nonce: String,
}

/// 出站 `reveal` 回复的 wire 形式：响应方对完整转录的签名（base64）。
#[derive(Debug, Clone)]
pub struct WireRevealResponse {
    pub responder_sig: String,
}

/// 发出 confirm 请求并读回成功响应的 JSON 对象；非 2xx 按 [`crate::wire`] 的规则分类。
async fn confirm_response(request: reqwest::RequestBuilder) -> LinkResult<serde_json::Value> {
    let resp = request.send().await.map_err(|e| send_failure(&e))?;
    if !resp.status().is_success() {
        return Err(classify_failure(resp).await);
    }
    read_json_object(resp).await
}
fn decode_b64_array<const N: usize>(s: &str) -> LinkResult<[u8; N]> {
    let bytes = B64
        .decode(s)
        .map_err(|_| LinkError::BadPayload("bad base64".into()))?;
    <[u8; N]>::try_from(bytes.as_slice()).map_err(|_| LinkError::BadPayload("bad length".into()))
}

/// 默认下载目录字段的长度上限（对端自报值一律不信任，超限即丢弃）。
const PEER_INFO_MAX_DIR_LEN: usize = 1024;
/// 自报回连地址单条长度上限。
const PEER_INFO_MAX_ADDR_LEN: usize = 256;

/// 设备信息的线上形式（加密前的明文 JSON）：
/// `{"v":2,"name":…,"platform":…,"appVersion":…,"addrs":[…],"cloudDeviceId":…,"defaultSaveDir":…,"pathStyle":…}`。
fn encode_peer_info(info: &PeerInfo) -> Vec<u8> {
    let mut json = serde_json::json!({ "v": 2, "addrs": info.addrs });
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(value) = value {
            json[key] = serde_json::Value::String(value.clone());
        }
    };
    put("name", &info.name);
    put("platform", &info.platform);
    put("appVersion", &info.app_version);
    put("cloudDeviceId", &info.cloud_device_id);
    put("defaultSaveDir", &info.default_save_dir);
    put("pathStyle", &info.path_style);
    serde_json::to_vec(&json).unwrap_or_default()
}

/// 解析对端信息；不是 v2 信息包 → `None`。字段一律做长度 / 取值收敛：名称 / 平台 / 版本清洗并
/// 限长，回连地址至多 [`MAX_DIRECT_CANDIDATES`] 条且必须能被 [`PeerAddress::parse`] 解析（规范化后
/// 去重，非法条目丢弃），云设备 ID 只接受 UUID 形态，目录 ≤ 1024 字节且无 NUL，风格只认
/// `windows` / `posix`。
fn decode_peer_info(plaintext: &[u8]) -> Option<PeerInfo> {
    let json: serde_json::Value = serde_json::from_slice(plaintext).ok()?;
    if json.get("v").and_then(serde_json::Value::as_i64) != Some(2) {
        return None;
    }
    let text = |key: &str| json.get(key).and_then(serde_json::Value::as_str);
    let mut addrs: Vec<String> = Vec::new();
    if let Some(reported) = json.get("addrs").and_then(serde_json::Value::as_array) {
        for address in reported.iter().filter_map(serde_json::Value::as_str) {
            if addrs.len() >= MAX_DIRECT_CANDIDATES {
                break;
            }
            if address.len() > PEER_INFO_MAX_ADDR_LEN {
                continue;
            }
            let Ok(parsed) = PeerAddress::parse(address) else {
                continue;
            };
            let candidate = parsed.to_candidate();
            if !addrs.contains(&candidate) {
                addrs.push(candidate);
            }
        }
    }
    Some(PeerInfo {
        name: sanitize_optional(text("name"), MAX_NAME_CHARS),
        platform: sanitize_optional(text("platform"), MAX_PLATFORM_CHARS),
        app_version: sanitize_optional(text("appVersion"), MAX_VERSION_CHARS),
        addrs,
        cloud_device_id: text("cloudDeviceId")
            .filter(|id| is_valid_cloud_device_id(id))
            .map(str::to_owned),
        default_save_dir: text("defaultSaveDir")
            .map(str::trim)
            .filter(|dir| {
                !dir.is_empty() && dir.len() <= PEER_INFO_MAX_DIR_LEN && !dir.contains('\0')
            })
            .map(str::to_owned),
        path_style: text("pathStyle")
            .filter(|style| matches!(*style, "windows" | "posix"))
            .map(str::to_owned),
    })
}

/// 已验证的新候选置首，去重后截断到 [`MAX_DIRECT_CANDIDATES`]。
fn promoted_candidates(old: &[PeerCandidate], verified: PeerCandidate) -> Vec<PeerCandidate> {
    let mut candidates = Vec::with_capacity(old.len() + 1);
    candidates.push(verified.clone());
    candidates.extend(old.iter().filter(|c| **c != verified).cloned());
    candidates.truncate(MAX_DIRECT_CANDIDATES);
    candidates
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
