//! Kad UDP 节点与迭代找源。
//!
//! 单 UDP socket，三阶段：
//! 0. **Bootstrap + Hello**：对初始联系点发 `BootstrapReq`（响应即证存活，
//!    回传当前鲜活联系点）+ `HelloReq`（握手，让对端把我方计入路由表）。
//!    KADEMLIA2 节点通常不响应未握手的发送方，故此阶段是收到回包的前提。
//! 1. **FindNode**：并发（≤[`BRANCH`]）向最近的未查询节点发 `Req{FindNode, target}`，
//!    把应答里的联系点并入候选集（去重、按 XOR 距离排序、上限 [`MAX_CANDIDATES`]），
//!    直到无更近节点或阶段超时。
//! 2. **SearchSources**：对最近的 [`SEARCH_TARGETS`] 个节点发 `SearchSourcesReq`，
//!    收 `SearchRes` 提取源，去重返回。
//!
//! 无节点 / 超时 / cancel → 返回空 vec 或 [`DownloadError`]，绝不 panic。

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use rand::Rng;
use rand::seq::SliceRandom;
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use super::proto::{self, Contact, KadId};
use crate::downloader::DownloadError;
use crate::ed2k::server::PeerAddr;
use crate::ed2k::status::{KadLive, KadNodeOrigin, KadNodeRow, KadNodeState, KadPhase, KadView};
use crate::logger::log_info;

/// 每轮 FindNode 并发查询的节点数。
const BRANCH: usize = 5;
/// 候选集上限（按距离截断）。
const MAX_CANDIDATES: usize = 100;
/// Phase 2 发 SearchSourcesReq 的最近节点数。
const SEARCH_TARGETS: usize = 12;
/// UDP 接收缓冲。
const RECV_BUF: usize = 8192;
/// 单轮收包窗口。
const ROUND_WINDOW: Duration = Duration::from_millis(2000);
/// Phase 1 无改善的最大连续轮数（收敛判据）。
const MAX_STALLS: u32 = 3;
/// Phase 0 bootstrap 并发扇出（对多少个初始联系点发 BootstrapReq）。
const BOOTSTRAP_FANOUT: usize = 40;
/// Phase 0 bootstrap 收包窗口。
const BOOTSTRAP_WINDOW: Duration = Duration::from_millis(3000);

/// 迭代查找中的候选节点。
struct Candidate {
    addr: SocketAddrV4,
    id: KadId,
    tcp_port: u16,
    version: u8,
    origin: KadNodeOrigin,
    /// 已向它发过 `FindNode`（Phase 1 据此挑选下一批）。
    queried: bool,
    /// 已向它发过任意请求（握手 / 查找 / 搜索），仅用于展示。
    contacted: bool,
    responded: bool,
}

impl Candidate {
    fn from_contact(c: &Contact, origin: KadNodeOrigin) -> Option<Self> {
        if c.endpoint.udp_port == 0 || c.endpoint.ip.is_unspecified() {
            return None;
        }
        Some(Candidate {
            addr: SocketAddrV4::new(c.endpoint.ip, c.endpoint.udp_port),
            id: c.id,
            tcp_port: c.endpoint.tcp_port,
            version: c.version,
            origin,
            queried: false,
            contacted: false,
            responded: false,
        })
    }

    fn row(&self) -> KadNodeRow {
        KadNodeRow {
            addr: self.addr.to_string(),
            id: hex::encode(self.id.0),
            origin: self.origin,
            state: if self.responded {
                KadNodeState::Responded
            } else if self.contacted {
                KadNodeState::Queried
            } else {
                KadNodeState::Known
            },
            tcp_port: self.tcp_port,
            version: self.version,
        }
    }
}

/// 一轮找源的进度：在阶段边界整份发布给详情页（没有观察者时不做任何事）。
struct Progress<'a> {
    live: Option<&'a KadLive>,
    bootstrap_nodes: u32,
    /// 应答过的节点（含已被按距离截断出候选集的）。
    responders: HashSet<SocketAddrV4>,
    /// 经应答新发现的节点数。
    discovered: u32,
}

impl Progress<'_> {
    fn mark_responded(&mut self, from: SocketAddr, candidates: &mut [Candidate]) {
        self.mark_responder(from, None, candidates);
    }

    /// 记一次应答；应答头自报了身份时，补全我们尚不知道 KadID 的节点（手动指定的地址）。
    fn mark_responder(
        &mut self,
        from: SocketAddr,
        header: Option<proto::NodeHeader>,
        candidates: &mut [Candidate],
    ) {
        let SocketAddr::V4(from) = from else {
            return;
        };
        self.responders.insert(from);
        if let Some(candidate) = candidates.iter_mut().find(|c| c.addr == from) {
            candidate.responded = true;
            if let Some(header) = header
                && candidate.id == KadId::UNKNOWN
            {
                candidate.id = header.id;
                candidate.tcp_port = header.tcp_port;
                candidate.version = header.version;
            }
        }
    }

    /// 把应答里的联系点并入候选集（按地址去重）。
    fn merge(
        &mut self,
        contacts: &[Contact],
        seen: &mut HashSet<SocketAddrV4>,
        candidates: &mut Vec<Candidate>,
    ) {
        for contact in contacts {
            if let Some(candidate) = Candidate::from_contact(contact, KadNodeOrigin::Discovered)
                && seen.insert(candidate.addr)
            {
                candidates.push(candidate);
                self.discovered = self.discovered.saturating_add(1);
            }
        }
    }

    fn publish(&self, phase: KadPhase, candidates: &[Candidate], sources: usize) {
        let Some(live) = self.live else {
            return;
        };
        live.publish(KadView {
            phase,
            bootstrap_nodes: self.bootstrap_nodes,
            responded: u32::try_from(self.responders.len()).unwrap_or(u32::MAX),
            discovered: self.discovered,
            sources: u32::try_from(sources).unwrap_or(u32::MAX),
            last_search_at: if phase == KadPhase::Done {
                crate::ed2k::status::unix_now()
            } else {
                0
            },
            nodes: candidates.iter().map(Candidate::row).collect(),
        });
    }
}

/// 握手轮：对候选集里前 [`BOOTSTRAP_FANOUT`] 个节点发 `BootstrapReq`（应答即证明存活，并回传
/// 它当前的鲜活联系点）与 `HelloReq`（让对端把我方计入路由表——KADEMLIA2 节点通常不理会
/// 未握手的发送方），在 `window_end` 前收应答并入候选集。返回发出请求的节点数。
async fn handshake_round(
    socket: &UdpSocket,
    self_id: &KadId,
    tcp_port: u16,
    candidates: &mut Vec<Candidate>,
    seen: &mut HashSet<SocketAddrV4>,
    progress: &mut Progress<'_>,
    window_end: Instant,
) -> u32 {
    let mut buf = vec![0u8; RECV_BUF];
    let boot_targets: Vec<SocketAddrV4> = candidates
        .iter_mut()
        .take(BOOTSTRAP_FANOUT)
        .map(|c| {
            c.contacted = true;
            c.addr
        })
        .collect();
    progress.publish(KadPhase::Bootstrapping, candidates, 0);
    let boot_req = proto::build_bootstrap_req();
    let hello_req = proto::build_hello_req(self_id, tcp_port);
    let mut send_ok = 0u32;
    let mut send_err = 0u32;
    for addr in &boot_targets {
        match socket.send_to(&boot_req, SocketAddr::V4(*addr)).await {
            Ok(_) => send_ok += 1,
            Err(_) => send_err += 1,
        }
        if let Err(error) = socket.send_to(&hello_req, SocketAddr::V4(*addr)).await {
            tracing::debug!(%addr, %error, "Kad hello send failed; continuing with other nodes");
        }
    }
    log_info!(
        "[ed2k-kad] bootstrap sent to {} targets ({} ok, {} send-err)",
        boot_targets.len(),
        send_ok,
        send_err
    );

    let mut boot_responses = 0u32;
    while let Some((n, from)) = recv_within(socket, &mut buf, window_end).await {
        let Some((opcode, payload)) = buf
            .get(..n)
            .and_then(|datagram| proto::parse_datagram(datagram, RECV_BUF * 8))
        else {
            continue;
        };
        match opcode {
            proto::OP_BOOTSTRAP_RES => {
                if let Some(contacts) = proto::decode_bootstrap_res(&payload) {
                    boot_responses += 1;
                    progress.mark_responder(from, proto::decode_node_header(&payload), candidates);
                    progress.merge(&contacts, seen, candidates);
                }
            }
            // Hello 应答 / HelloAck：对端已握手（存活即够），应答头带它的身份。
            proto::OP_HELLO_RES | proto::OP_HELLO_RES_ACK => {
                boot_responses += 1;
                progress.mark_responder(from, proto::decode_node_header(&payload), candidates);
            }
            _ => {}
        }
    }
    log_info!(
        "[ed2k-kad] bootstrap: {} responses, {} candidates after merge",
        boot_responses,
        candidates.len()
    );
    u32::try_from(boot_targets.len()).unwrap_or(u32::MAX)
}

/// 一次 Kad 引导握手的结果。
#[derive(Debug, Clone, Default)]
pub struct BootstrapOutcome {
    /// 发过握手请求的节点数。
    pub contacted: u32,
    /// 应答的节点数。
    pub responded: u32,
    /// 应答里新告知的节点数。
    pub discovered: u32,
    /// 本次联系与发现的节点（应答的在前，至多 [`MAX_CANDIDATES`] 个）。
    pub nodes: Vec<KadNodeRow>,
    /// 确认存活或刚被告知的联系点（应答的在前），可并入已知节点缓存。
    pub live: Vec<Contact>,
}

/// Kad 引导：向 `contacts`（超过 [`BOOTSTRAP_FANOUT`] 个时随机取样）发握手，在 `window` 内收
/// 应答。用于检查 Kad 网络是否可达并取得鲜活节点；不查找任何文件。
///
/// # Errors
///
/// UDP socket 绑定失败。
///
/// # Examples
///
/// ```
/// # async fn run() {
/// use fluxdown_engine::ed2k::kad::node::bootstrap;
/// use std::time::Duration;
///
/// // 没有可联系的节点：立即返回空结果。
/// let outcome = bootstrap(&[], 4662, Duration::from_secs(1)).await.unwrap_or_default();
/// assert_eq!(outcome.contacted, 0);
/// # }
/// ```
pub async fn bootstrap(
    contacts: &[Contact],
    tcp_port: u16,
    window: Duration,
) -> Result<BootstrapOutcome, DownloadError> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen: HashSet<SocketAddrV4> = HashSet::new();
    for contact in contacts {
        if let Some(candidate) = Candidate::from_contact(contact, KadNodeOrigin::Bootstrap)
            && seen.insert(candidate.addr)
        {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        return Ok(BootstrapOutcome::default());
    }
    // 每次引导换一批节点：固定取前几十个会让同一批失效节点反复被问。
    candidates.shuffle(&mut rand::rng());

    let mut self_bytes = [0u8; 16];
    rand::rng().fill(&mut self_bytes);
    let self_id = KadId::from_memory(&self_bytes);
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(DownloadError::Io)?;
    let mut progress = Progress {
        live: None,
        bootstrap_nodes: u32::try_from(candidates.len()).unwrap_or(u32::MAX),
        responders: HashSet::new(),
        discovered: 0,
    };
    let contacted = handshake_round(
        &socket,
        &self_id,
        tcp_port,
        &mut candidates,
        &mut seen,
        &mut progress,
        Instant::now() + window,
    )
    .await;

    // 应答的在前，其次是问过没答的，最后是刚被告知的。
    candidates.sort_by_key(|c| (!c.responded, !c.contacted));
    let live = candidates
        .iter()
        .filter(|c| c.id != KadId::UNKNOWN)
        .filter(|c| c.responded || c.origin == KadNodeOrigin::Discovered)
        .map(|c| Contact {
            id: c.id,
            endpoint: proto::Endpoint {
                ip: *c.addr.ip(),
                udp_port: c.addr.port(),
                tcp_port: c.tcp_port,
            },
            version: c.version,
        })
        .collect();
    Ok(BootstrapOutcome {
        contacted,
        responded: u32::try_from(progress.responders.len()).unwrap_or(u32::MAX),
        discovered: progress.discovered,
        nodes: candidates
            .iter()
            .filter(|c| c.contacted || c.origin == KadNodeOrigin::Discovered)
            .take(MAX_CANDIDATES)
            .map(Candidate::row)
            .collect(),
        live,
    })
}

/// Kad 去中心化找源入口。见模块文档。`live` 给出时，在各阶段边界发布节点与进度实况。
///
/// # Examples
///
/// ```
/// # async fn run() {
/// use fluxdown_engine::ed2k::kad::node::find_sources_kad;
/// use std::time::Duration;
/// use tokio_util::sync::CancellationToken;
///
/// let cancel = CancellationToken::new();
/// // 空 nodes.dat → 无 bootstrap，立即返回空。
/// let sources = find_sources_kad(
///     &[0u8; 16], 1024, 0, 4662, &[], Duration::from_secs(1), &cancel, None,
/// )
/// .await
/// .unwrap_or_default();
/// assert!(sources.is_empty());
/// # }
/// ```
#[allow(clippy::too_many_arguments)]
pub async fn find_sources_kad(
    file_hash: &[u8; 16],
    total_bytes: u64,
    udp_port: u16,
    tcp_port: u16,
    nodes_dat: &[u8],
    timeout: Duration,
    cancel: &CancellationToken,
    live: Option<&KadLive>,
) -> Result<Vec<PeerAddr>, DownloadError> {
    // 自身 KadID（随机）：Hello 握手上报，供对端把我方计入路由表。
    let mut self_bytes = [0u8; 16];
    rand::rng().fill(&mut self_bytes);
    let self_id = KadId::from_memory(&self_bytes);
    let bootstrap = proto::parse_nodes_dat(nodes_dat);
    if bootstrap.is_empty() {
        return Ok(Vec::new());
    }

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, udp_port))
        .await
        .map_err(DownloadError::Io)?;

    let target = KadId::from_memory(file_hash);

    // 初始候选集（去重 addr）。
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen: HashSet<SocketAddrV4> = HashSet::new();
    for c in &bootstrap {
        if let Some(cand) = Candidate::from_contact(c, KadNodeOrigin::Bootstrap)
            && seen.insert(cand.addr)
        {
            candidates.push(cand);
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    candidates.sort_by(|a, b| a.id.xor_cmp(&b.id, &target));
    let mut progress = Progress {
        live,
        bootstrap_nodes: u32::try_from(candidates.len()).unwrap_or(u32::MAX),
        responders: HashSet::new(),
        discovered: 0,
    };

    let overall_deadline = Instant::now() + timeout;
    let mut buf = vec![0u8; RECV_BUF];

    // ---------------- Phase 0: Bootstrap + Hello 握手 ----------------
    let window_end = (Instant::now() + BOOTSTRAP_WINDOW).min(overall_deadline);
    handshake_round(
        &socket,
        &self_id,
        tcp_port,
        &mut candidates,
        &mut seen,
        &mut progress,
        window_end,
    )
    .await;
    candidates.sort_by(|a, b| a.id.xor_cmp(&b.id, &target));
    candidates.truncate(MAX_CANDIDATES);
    progress.publish(KadPhase::FindingNodes, &candidates, 0);

    let p1_deadline = Instant::now() + (overall_deadline - Instant::now()) / 2;

    // ---------------- Phase 1: FindNode ----------------
    let mut stalls: u32 = 0;
    let mut fn_responses = 0u32;
    while Instant::now() < p1_deadline {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        // 取最近的未查询节点，最多 BRANCH 个。
        let batch: Vec<(SocketAddrV4, KadId)> = candidates
            .iter_mut()
            .filter(|c| !c.queried)
            .take(BRANCH)
            .map(|c| {
                c.queried = true;
                c.contacted = true;
                (c.addr, c.id)
            })
            .collect();
        if batch.is_empty() {
            break;
        }

        for (addr, id) in &batch {
            let pkt = proto::build_find_node_req(&target, id);
            if let Err(error) = socket.send_to(&pkt, SocketAddr::V4(*addr)).await {
                tracing::debug!(%addr, %error, "Kad find-node send failed; continuing with other nodes");
            }
        }

        let prev_best = candidates.first().map(|c| c.id);
        let window_end = (Instant::now() + ROUND_WINDOW).min(p1_deadline);
        while let Some((n, from)) = recv_within(&socket, &mut buf, window_end).await {
            let Some((opcode, payload)) = proto::parse_datagram(&buf[..n], RECV_BUF * 8) else {
                continue;
            };
            if opcode != proto::OP_RES {
                continue;
            }
            let Some(contacts) = proto::decode_find_node_res(&payload) else {
                continue;
            };
            fn_responses += 1;
            progress.mark_responded(from, &mut candidates);
            progress.merge(&contacts, &mut seen, &mut candidates);
        }

        candidates.sort_by(|a, b| a.id.xor_cmp(&b.id, &target));
        candidates.truncate(MAX_CANDIDATES);
        progress.publish(KadPhase::FindingNodes, &candidates, 0);

        let improved = match (prev_best, candidates.first()) {
            (Some(prev), Some(now)) => now.id.xor_cmp(&prev, &target) == std::cmp::Ordering::Less,
            _ => false,
        };
        if improved {
            stalls = 0;
        } else {
            stalls += 1;
            if stalls >= MAX_STALLS {
                break;
            }
        }
    }
    log_info!(
        "[ed2k-kad] findnode: {} responses, {} candidates",
        fn_responses,
        candidates.len()
    );

    // ---------------- Phase 2: SearchSources ----------------
    let search_targets: Vec<SocketAddrV4> = candidates
        .iter_mut()
        .take(SEARCH_TARGETS)
        .map(|c| {
            c.contacted = true;
            c.addr
        })
        .collect();
    progress.publish(KadPhase::Searching, &candidates, 0);
    let req = proto::build_search_sources_req(&target, total_bytes);
    for addr in &search_targets {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        if let Err(error) = socket.send_to(&req, SocketAddr::V4(*addr)).await {
            tracing::debug!(%addr, %error, "Kad source-search send failed; continuing with other nodes");
        }
    }

    let mut sources: Vec<PeerAddr> = Vec::new();
    let mut source_seen: HashSet<PeerAddr> = HashSet::new();
    let mut idle_gaps = 0u32;
    let mut search_responses = 0u32;
    while Instant::now() < overall_deadline {
        if cancel.is_cancelled() {
            break;
        }
        let window_end = (Instant::now() + ROUND_WINDOW).min(overall_deadline);
        match recv_within(&socket, &mut buf, window_end).await {
            Some((n, from)) => {
                idle_gaps = 0;
                let Some((opcode, payload)) = proto::parse_datagram(&buf[..n], RECV_BUF * 8) else {
                    continue;
                };
                if opcode != proto::OP_SEARCH_RES {
                    continue;
                }
                let Some(entries) = proto::decode_search_res(&payload) else {
                    continue;
                };
                search_responses += 1;
                progress.mark_responded(from, &mut candidates);
                for e in &entries {
                    if let Some(peer) = e.source_peer()
                        && source_seen.insert(peer)
                    {
                        sources.push(peer);
                    }
                }
            }
            None => {
                idle_gaps += 1;
                if idle_gaps >= MAX_STALLS {
                    break;
                }
            }
        }
    }

    log_info!(
        "[ed2k-kad] done: {} sources ({} search-res) from {} bootstrap contacts, {} search targets",
        sources.len(),
        search_responses,
        bootstrap.len(),
        search_targets.len()
    );
    progress.publish(KadPhase::Done, &candidates, sources.len());
    Ok(sources)
}

/// 在 `deadline` 前收一个 datagram，返回字节数与发送方；超时/致命错误返回 `None`。
///
/// Windows 专属陷阱：向已死节点发包会触发 ICMP 端口不可达，Windows 把它作为
/// **下一次 `recv_from` 的 WSAECONNRESET(10054)** 上报。若据此返回 `None`，
/// 单个死节点就会终止整个接收窗口，漏掉其后所有存活节点的回包。故遇 10054
/// 忽略并继续等到 deadline。
async fn recv_within(
    socket: &UdpSocket,
    buf: &mut [u8],
    deadline: Instant,
) -> Option<(usize, SocketAddr)> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        match tokio::time::timeout(deadline - now, socket.recv_from(buf)).await {
            Ok(Ok(received)) => return Some(received),
            Ok(Err(e)) if e.raw_os_error() == Some(10054) => continue,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find_sources_kad;
    use crate::ed2k::kad::proto::{self, Contact, Endpoint, KadId};
    use crate::ed2k::status::{KadLive, KadNodeOrigin, KadNodeState, KadPhase};
    use std::net::Ipv4Addr;
    use std::time::Duration;
    use tokio::net::UdpSocket;
    use tokio_util::sync::CancellationToken;

    fn contact(id: u8, udp_port: u16) -> Contact {
        Contact {
            id: KadId([id; 16]),
            endpoint: Endpoint {
                ip: Ipv4Addr::LOCALHOST,
                udp_port,
                tcp_port: 4662,
            },
            version: 9,
        }
    }

    #[tokio::test]
    async fn bootstrap_learns_the_identity_of_a_manually_given_node() {
        // 用户只给了地址（KadID 未知）：节点在 BootstrapRes 头里自报身份并告知一个联系点。
        let node = UdpSocket::bind("127.0.0.1:0").await.expect("bind node");
        let node_port = node.local_addr().expect("node address").port();
        let reply = proto::build_bootstrap_res(&KadId([1; 16]), 4672, &[contact(9, 1)]);
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = node.recv_from(&mut buf).await {
                let is_bootstrap = proto::parse_frame(&buf[..n])
                    .is_some_and(|(_, opcode, _)| opcode == proto::OP_BOOTSTRAP_REQ);
                if is_bootstrap {
                    node.send_to(&reply, from).await.expect("bootstrap reply");
                }
            }
        });
        // 另一个在线但从不应答的节点。
        let silent = UdpSocket::bind("127.0.0.1:0").await.expect("bind silent");
        let silent_port = silent.local_addr().expect("silent address").port();
        let manual = Contact {
            id: KadId::UNKNOWN,
            endpoint: Endpoint {
                ip: Ipv4Addr::LOCALHOST,
                udp_port: node_port,
                tcp_port: 0,
            },
            version: 0,
        };

        let outcome = super::bootstrap(
            &[manual, contact(5, silent_port)],
            4662,
            Duration::from_millis(800),
        )
        .await
        .expect("bootstrap");
        responder.abort();

        assert_eq!(
            (outcome.contacted, outcome.responded, outcome.discovered),
            (2, 1, 1)
        );
        // 应答的节点排最前，身份已从应答头补全。
        assert_eq!(outcome.nodes[0].addr, format!("127.0.0.1:{node_port}"));
        assert_eq!(outcome.nodes[0].state, KadNodeState::Responded);
        assert_eq!(outcome.nodes[0].id, "01".repeat(16));
        assert_eq!(outcome.nodes.len(), 3);
        // 可并入缓存的只有应答的节点与它告知的节点；问了没答的不算。
        let live: Vec<(KadId, u16, u16)> = outcome
            .live
            .iter()
            .map(|c| (c.id, c.endpoint.udp_port, c.endpoint.tcp_port))
            .collect();
        assert_eq!(
            live,
            [(KadId([1; 16]), node_port, 4672), (KadId([9; 16]), 1, 4662)]
        );
    }

    #[tokio::test]
    async fn publishes_contacted_and_discovered_nodes() {
        // 假引导节点：只应答 BootstrapReq，并在应答里告知一个新联系点（端口 1，无人监听）。
        let node = UdpSocket::bind("127.0.0.1:0").await.expect("bind node");
        let node_port = node.local_addr().expect("node address").port();
        let reply = proto::build_bootstrap_res(&KadId([1; 16]), 4662, &[contact(9, 1)]);
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = node.recv_from(&mut buf).await {
                let is_bootstrap = proto::parse_frame(&buf[..n])
                    .is_some_and(|(_, opcode, _)| opcode == proto::OP_BOOTSTRAP_REQ);
                if is_bootstrap {
                    node.send_to(&reply, from).await.expect("bootstrap reply");
                }
            }
        });

        let live = KadLive::default();
        let cancel = CancellationToken::new();
        let sources = find_sources_kad(
            &[3u8; 16],
            2048,
            0,
            4662,
            &proto::encode_nodes_dat(&[contact(2, node_port)]),
            Duration::from_millis(1500),
            &cancel,
            Some(&live),
        )
        .await
        .expect("search finishes");
        responder.abort();

        assert!(sources.is_empty());
        let view = live.view();
        assert_eq!(view.phase, KadPhase::Done);
        assert_eq!(
            (view.bootstrap_nodes, view.responded, view.discovered),
            (1, 1, 1)
        );
        assert!(view.last_search_at > 0);
        let bootstrap = view
            .nodes
            .iter()
            .find(|node| node.origin == KadNodeOrigin::Bootstrap)
            .expect("bootstrap row");
        assert_eq!(bootstrap.addr, format!("127.0.0.1:{node_port}"));
        assert_eq!(bootstrap.state, KadNodeState::Responded);
        let discovered = view
            .nodes
            .iter()
            .find(|node| node.origin == KadNodeOrigin::Discovered)
            .expect("discovered row");
        assert_eq!(discovered.addr, "127.0.0.1:1");
        assert_eq!(discovered.id, "09".repeat(16));
        assert_eq!(
            discovered.state,
            KadNodeState::Queried,
            "向它发过请求但没有应答"
        );
    }

    #[tokio::test]
    async fn empty_nodes_dat_returns_empty_fast() {
        let cancel = CancellationToken::new();
        let start = std::time::Instant::now();
        let res = find_sources_kad(
            &[0u8; 16],
            1024,
            0,
            4662,
            &[],
            Duration::from_secs(5),
            &cancel,
            None,
        )
        .await;
        // 空 bootstrap 立即返回，不触网、不等满 timeout。
        assert!(start.elapsed() < Duration::from_secs(1));
        match res {
            Ok(v) => assert!(v.is_empty()),
            Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn unreachable_bootstrap_returns_within_timeout() {
        // 构造 1 条指向本机高位保留端口的联系点（无人应答）。
        // 短 timeout 内返回空，不 panic、不挂死。
        let mut nodes = Vec::new();
        nodes.extend_from_slice(&1u32.to_le_bytes()); // 旧格式 count=1
        // Contact: KadID(16) + IP(4=127.0.0.1) + UDP(2) + TCP(2) + Version(1)
        nodes.extend_from_slice(&[0u8; 16]);
        nodes.extend_from_slice(&[127, 0, 0, 1]);
        nodes.extend_from_slice(&1u16.to_le_bytes()); // udp_port=1（无监听）
        nodes.extend_from_slice(&4662u16.to_le_bytes());
        nodes.push(0x05);

        let cancel = CancellationToken::new();
        let start = std::time::Instant::now();
        let res = find_sources_kad(
            &[1u8; 16],
            2048,
            0,
            4662,
            &nodes,
            Duration::from_millis(600),
            &cancel,
            None,
        )
        .await;
        assert!(start.elapsed() < Duration::from_secs(3));
        match res {
            Ok(v) => assert!(v.is_empty()),
            Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn cancelled_returns_cancelled() {
        let mut nodes = Vec::new();
        nodes.extend_from_slice(&1u32.to_le_bytes());
        nodes.extend_from_slice(&[0u8; 16]);
        nodes.extend_from_slice(&[127, 0, 0, 1]);
        nodes.extend_from_slice(&1u16.to_le_bytes());
        nodes.extend_from_slice(&4662u16.to_le_bytes());
        nodes.push(0x05);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let res = find_sources_kad(
            &[2u8; 16],
            2048,
            0,
            4662,
            &nodes,
            Duration::from_secs(5),
            &cancel,
            None,
        )
        .await;
        assert!(matches!(
            res,
            Err(crate::downloader::DownloadError::Cancelled)
        ));
    }
}
