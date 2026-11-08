//! 多网卡聚合下载（config `multi_nic_enabled`）的链路规划层。
//!
//! # 什么时候有收益
//!
//! 把分段连接绑定到不同网卡，只有当各网卡通向**不同的上游线路**（家宽 +
//! 手机热点、双运营商）且瓶颈在本机出口时才会叠加带宽。网线与 Wi‑Fi 接在
//! 同一台路由器上时瓶颈在宽带本身，聚合零收益——规划器按「同一局域网」
//! 判据直接剔除这类重复网卡，不交给运行期去试错。
//!
//! # 分工
//!
//! - 本模块：枚举网卡 → 排除隧道/虚拟/同局域网/近期失败的网卡 → 为每条
//!   额外链路构建绑定出口的 client（[`prepare_links`]）。纯判据
//!   [`plan_links`] 与 I/O 分离，单测覆盖。
//! - [`crate::cdn::NodePool`]：额外链路是 `RoutePath::Link` 槽位，与主链路
//!   按「独立容量」均衡派工（见 `NodePool::pick`）。
//! - coordinator（`segment_coordinator/multipath.rs`）：起飞后后台规划，首个
//!   完整 ramp 窗口挂入节点池，不推迟首连接。
//!
//! # 出口绑定
//!
//! - Linux / macOS：`SO_BINDTODEVICE` / `IP_BOUND_IF`（reqwest `interface`），
//!   路由查找限定在该网卡；
//! - Windows：强主机模型下绑定源地址即限定出口网卡（reqwest
//!   `local_address`），只能单地址族，优先 IPv4。
//!
//! 链路 client 的 DNS 解析只返回该链路可达的地址族，避免 hyper 在地址族
//! 不匹配时静默放弃绑定、从默认网卡发出。
//!
//! # 安全边界
//!
//! 绑定物理网卡会绕开全局 VPN / TUN 隧道。因此主链路（系统为目标选择的
//! 出口）是隧道或点对点接口、目标解析到 fake-IP 段（198.18.0.0/15）时整个
//! 任务不聚合；隧道类网卡永远不作为额外链路。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::Client;

use crate::logger::log_info;

/// 单任务最多挂入的额外链路数（主链路不计）。
pub const MAX_EXTRA_LINKS: usize = 3;

/// 链路被踢除（连续失败/建连失败）后，后续任务跳过它的时长。网卡恢复或
/// 换了地址（键含本地 IP）会自然重新纳入。
const LINK_FAIL_TTL: Duration = Duration::from_secs(300);

/// `links_off` 事件的原因码（wire 契约：客户端详情日志按码本地化）。
pub mod off_reason {
    /// 任务走代理（含 Auto 多路径）：出口是代理，绑定网卡没有意义。
    pub const PROXY: &str = "proxy";
    /// 目标解析到 fake-IP 段：TUN 模式代理接管了 DNS，绑定物理网卡会断流。
    pub const FAKE_IP: &str = "fake_ip";
    /// 目标是局域网/回环/CGNAT 地址，不经过上游线路。
    pub const LOCAL_TARGET: &str = "local_target";
    /// 系统为目标选择的出口是 VPN/隧道接口，绑定物理网卡会绕开隧道。
    pub const VPN: &str = "vpn";
    /// 无法确定系统为目标选择的出口网卡。
    pub const PRIMARY_UNKNOWN: &str = "primary_unknown";
    /// 没有通向其它上游的网卡（同一局域网的网卡被视为同一条线路）。
    pub const NO_EXTRA: &str = "no_extra";
    /// 目标主机解析失败。
    pub const DNS: &str = "dns";
}

/// manager 折算的任务级输入（穿透 `DownloadParams` 进 coordinator）。
#[derive(Debug, Clone)]
pub struct MultiNicInput {
    /// 任务有效 UA（与任务 client 逐字节一致）。
    pub user_agent: String,
    /// 任务级 TLS 策略（用户对该任务的显式选择，链路 client 原样继承）。
    pub ignore_tls_errors: bool,
    /// 任务走代理或 Auto 多路径：不规划，仅上报 `links_off/proxy`。
    pub blocked_by_proxy: bool,
}

/// 一块网卡及其地址（按名称聚合 if-addrs 的逐地址条目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NicInfo {
    pub name: String,
    pub index: u32,
    pub up: bool,
    pub p2p: bool,
    pub v4: Vec<(Ipv4Addr, u8)>,
    pub v6: Vec<(Ipv6Addr, u8)>,
}

/// 一条额外链路的出口绑定。`v4`/`v6` 已按目标地址族过滤：只有目标同时
/// 具备该地址族时才会填入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkBinding {
    pub name: String,
    /// OS 接口索引（`RoutePath::Link` 的身份）。
    pub index: u32,
    pub v4: Option<Ipv4Addr>,
    pub v6: Option<Ipv6Addr>,
}

impl LinkBinding {
    /// 事件/日志中的节点标签（wire：`NIC:<网卡名>`）。
    #[must_use]
    pub fn label(&self) -> String {
        format!("NIC:{}", self.name)
    }

    /// 链路的本地出口地址（优先 IPv4，与 Windows 绑定选择一致）。
    #[must_use]
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.v4.map(IpAddr::V4).or_else(|| self.v6.map(IpAddr::V6))
    }

    /// 近期失败记忆的键：名称 + 本地地址（换地址视为新链路）。
    fn health_key(&self) -> String {
        match self.local_ip() {
            Some(ip) => format!("{}|{ip}", self.name),
            None => self.name.clone(),
        }
    }
}

/// [`plan_links`] 的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPlan {
    /// 可聚合：主链路名 + 额外链路 + 被排除网卡及原因（日志用）。
    Links {
        primary: String,
        links: Vec<LinkBinding>,
        skipped: Vec<(String, &'static str)>,
    },
    /// 本任务不聚合（原因码见 [`off_reason`]）。
    Off(&'static str),
}

/// [`prepare_links`] 的产出：链路规划结论 + 已构建的链路 client。
pub struct PreparedLinks {
    /// 目标 host（事件归属）。
    pub host: String,
    pub outcome: Result<Vec<(Arc<LinkBinding>, Client)>, &'static str>,
}

// ---------------------------------------------------------------------------
// 纯判据
// ---------------------------------------------------------------------------

/// fake-IP 段（RFC 2544 基准测试段，Clash/sing-box 等 TUN 代理的默认 fake-IP）。含
/// IPv4-mapped IPv6。插件 fetch 守卫（`plugin::bridge::guard_filter`）共用此判定。
pub(crate) fn is_fake_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 198 && (o[1] & 0xfe) == 18
        }
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .is_some_and(|v4| is_fake_ip(IpAddr::V4(v4))),
    }
}

/// 不经过上游线路的目标：私网、回环、链路本地、CGNAT（Tailscale 等覆盖网）、
/// IPv6 ULA / 链路本地 / 回环。
fn is_local_target(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local_target(IpAddr::V4(v4)),
            None => {
                let seg0 = v6.segments()[0];
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (seg0 & 0xfe00) == 0xfc00
                    || (seg0 & 0xffc0) == 0xfe80
            }
        },
    }
}

/// 可作为出口的 IPv4 地址（非回环/链路本地/未指定）。
fn usable_v4(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified()
}

/// 可达公网的 IPv6 地址（全局单播：排除回环/未指定/链路本地/ULA/组播）。
fn usable_v6(ip: Ipv6Addr) -> bool {
    let seg0 = ip.segments()[0];
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && (seg0 & 0xffc0) != 0xfe80
        && (seg0 & 0xfe00) != 0xfc00
        && ip.to_ipv4_mapped().is_none()
}

fn v4_network(ip: Ipv4Addr, prefix: u8) -> u32 {
    let bits = u32::from(ip);
    match prefix {
        0 => 0,
        p if p >= 32 => bits,
        p => bits & (u32::MAX << (32 - u32::from(p))),
    }
}

/// 两个 IPv4 地址是否处于同一子网（取两者较短前缀比较，任一方视角下同网即同网）。
fn same_v4_lan(a: (Ipv4Addr, u8), b: (Ipv4Addr, u8)) -> bool {
    let prefix = a.1.min(b.1);
    prefix > 0 && v4_network(a.0, prefix) == v4_network(b.0, prefix)
}

/// 两个全局 IPv6 地址是否共享 /64 前缀（同一路由器通告的 SLAAC 前缀）。
fn same_v6_lan(a: Ipv6Addr, b: Ipv6Addr) -> bool {
    a.segments()[..4] == b.segments()[..4]
}

fn same_lan(a: &NicInfo, b: &NicInfo) -> bool {
    let v4 =
        a.v4.iter()
            .any(|&x| usable_v4(x.0) && b.v4.iter().any(|&y| usable_v4(y.0) && same_v4_lan(x, y)));
    let v6 = a.v6.iter().any(|&(x, _)| {
        usable_v6(x) && b.v6.iter().any(|&(y, _)| usable_v6(y) && same_v6_lan(x, y))
    });
    v4 || v6
}

/// 隧道/VPN/TUN 代理接口（按名称；点对点标志另行判断）。
fn is_tunnel_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    const PREFIXES: [&str; 8] = ["utun", "tun", "tap", "wg", "ppp", "ipsec", "gif", "stf"];
    const CONTAINS: [&str; 14] = [
        "vpn",
        "wireguard",
        "wintun",
        "openvpn",
        "tailscale",
        "zerotier",
        "nordlynx",
        "clash",
        "mihomo",
        "sing-box",
        "singbox",
        "v2ray",
        "tap-windows",
        "tunnel",
    ];
    PREFIXES.iter().any(|p| lower.starts_with(p))
        || CONTAINS.iter().any(|c| lower.contains(c))
        || lower == "meta"
        || lower.starts_with("zt")
}

/// 虚拟化/容器/系统内部接口：没有独立上游，不作为额外链路。
fn is_virtual_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    const PREFIXES: [&str; 10] = [
        "docker", "veth", "br-", "virbr", "vmnet", "vboxnet", "bridge", "awdl", "llw", "anpi",
    ];
    const CONTAINS: [&str; 6] = [
        "vethernet",
        "vmware",
        "virtualbox",
        "hyper-v",
        "loopback",
        "pseudo",
    ];
    PREFIXES.iter().any(|p| lower.starts_with(p))
        || CONTAINS.iter().any(|c| lower.contains(c))
        || is_loopback_name(&lower)
}

/// `lo` / `lo0` / `lo1`…；不能用前缀匹配，否则 Windows 的
/// 「Local Area Connection」会被误判。
fn is_loopback_name(lower: &str) -> bool {
    lower
        .strip_prefix("lo")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
}

/// 链路规划纯判据。
///
/// - `nics`：本机网卡（[`enumerate_nics`]）；
/// - `targets`：目标 host 的全部解析地址；
/// - `primary_src`：系统为首个公网目标选择的源地址（UDP connect 探测）；
/// - `recently_failed`：近期被踢除的链路（[`LinkBinding`] 健康键）。
pub fn plan_links(
    nics: &[NicInfo],
    targets: &[IpAddr],
    primary_src: Option<IpAddr>,
    recently_failed: impl Fn(&LinkBinding) -> bool,
) -> LinkPlan {
    if targets.is_empty() {
        return LinkPlan::Off(off_reason::DNS);
    }
    if targets.iter().any(|&ip| is_fake_ip(ip)) {
        return LinkPlan::Off(off_reason::FAKE_IP);
    }
    let public: Vec<IpAddr> = targets
        .iter()
        .copied()
        .filter(|&ip| !is_local_target(ip))
        .collect();
    if public.is_empty() {
        return LinkPlan::Off(off_reason::LOCAL_TARGET);
    }
    let want_v4 = public.iter().any(IpAddr::is_ipv4);
    let want_v6 = public.iter().any(IpAddr::is_ipv6);

    let Some(src) = primary_src else {
        return LinkPlan::Off(off_reason::PRIMARY_UNKNOWN);
    };
    let owns = |nic: &NicInfo| match src {
        IpAddr::V4(v4) => nic.v4.iter().any(|&(ip, _)| ip == v4),
        IpAddr::V6(v6) => nic.v6.iter().any(|&(ip, _)| ip == v6),
    };
    let Some(primary) = nics.iter().find(|nic| owns(nic)) else {
        return LinkPlan::Off(off_reason::PRIMARY_UNKNOWN);
    };
    if primary.p2p || is_tunnel_name(&primary.name) {
        return LinkPlan::Off(off_reason::VPN);
    }

    let mut links: Vec<LinkBinding> = Vec::new();
    let mut accepted: Vec<&NicInfo> = vec![primary];
    let mut skipped: Vec<(String, &'static str)> = Vec::new();
    for nic in nics {
        if nic.name == primary.name {
            continue;
        }
        let v4 = want_v4
            .then(|| nic.v4.iter().map(|&(ip, _)| ip).find(|&ip| usable_v4(ip)))
            .flatten();
        let v6 = want_v6
            .then(|| nic.v6.iter().map(|&(ip, _)| ip).find(|&ip| usable_v6(ip)))
            .flatten();
        let reason = if !nic.up {
            Some("down")
        } else if nic.p2p || is_tunnel_name(&nic.name) {
            Some("tunnel")
        } else if is_virtual_name(&nic.name) {
            Some("virtual")
        } else if v4.is_none() && v6.is_none() {
            Some("family")
        } else if accepted.iter().any(|other| same_lan(nic, other)) {
            Some("same_lan")
        } else if links.len() >= MAX_EXTRA_LINKS {
            Some("limit")
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push((nic.name.clone(), reason));
            continue;
        }
        let binding = LinkBinding {
            name: nic.name.clone(),
            index: nic.index,
            v4,
            v6,
        };
        if recently_failed(&binding) {
            skipped.push((nic.name.clone(), "recent_fail"));
            continue;
        }
        accepted.push(nic);
        links.push(binding);
    }
    if links.is_empty() {
        return LinkPlan::Off(off_reason::NO_EXTRA);
    }
    LinkPlan::Links {
        primary: primary.name.clone(),
        links,
        skipped,
    }
}

// ---------------------------------------------------------------------------
// 近期失败记忆（进程内；网络拓扑易变，不落库）
// ---------------------------------------------------------------------------

fn failures() -> &'static Mutex<HashMap<String, Instant>> {
    static FAILURES: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 链路在任务内被踢除（连续失败/建连失败）：后续任务 [`LINK_FAIL_TTL`]
/// 内跳过它，避免每个任务都在无路由的网卡上重复试错。
pub fn record_link_failure(link: &LinkBinding) {
    let mut map = failures().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, at| at.elapsed() < LINK_FAIL_TTL);
    map.insert(link.health_key(), Instant::now());
}

pub(crate) fn is_recently_failed(link: &LinkBinding) -> bool {
    let map = failures().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&link.health_key())
        .is_some_and(|at| at.elapsed() < LINK_FAIL_TTL)
}

// ---------------------------------------------------------------------------
// I/O：网卡枚举、主链路探测、链路 client
// ---------------------------------------------------------------------------

/// 枚举本机网卡（桌面平台；其余平台恒为空 → 规划结论 `no_extra`/`primary_unknown`）。
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub fn enumerate_nics() -> std::io::Result<Vec<NicInfo>> {
    let mut nics: Vec<NicInfo> = Vec::new();
    for iface in if_addrs::get_if_addrs()? {
        let Some(index) = iface.index else {
            continue;
        };
        let pos = match nics.iter().position(|n| n.name == iface.name) {
            Some(pos) => pos,
            None => {
                nics.push(NicInfo {
                    name: iface.name.clone(),
                    index,
                    up: false,
                    p2p: false,
                    v4: Vec::new(),
                    v6: Vec::new(),
                });
                nics.len() - 1
            }
        };
        let nic = &mut nics[pos];
        nic.up |= iface.is_oper_up();
        nic.p2p |= iface.is_p2p();
        match &iface.addr {
            if_addrs::IfAddr::V4(a) => nic.v4.push((a.ip, a.prefixlen)),
            if_addrs::IfAddr::V6(a) => nic.v6.push((a.ip, a.prefixlen)),
        }
    }
    Ok(nics)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn enumerate_nics() -> std::io::Result<Vec<NicInfo>> {
    Ok(Vec::new())
}

/// 系统为 `target` 选择的源地址：UDP `connect` 只做路由查找、不发包。
fn primary_source(target: IpAddr) -> Option<IpAddr> {
    let bind: SocketAddr = match target {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = std::net::UdpSocket::bind(bind).ok()?;
    socket.connect(SocketAddr::new(target, 443)).ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

/// 只返回链路可达地址族的系统 DNS 解析器。
struct LinkResolver {
    v4: bool,
    v6: bool,
}

impl reqwest::dns::Resolve for LinkResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let (v4, v6) = (self.v4, self.v6);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|addr| (addr.is_ipv4() && v4) || (addr.is_ipv6() && v6))
                .collect();
            if addrs.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    "no address of the link's family",
                )
                .into());
            }
            let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// 把 client 出口绑定到 `link`（见模块文档「出口绑定」）。
pub(crate) fn bind_to_link(
    builder: reqwest::ClientBuilder,
    link: &LinkBinding,
) -> reqwest::ClientBuilder {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        builder
            .interface(&link.name)
            .dns_resolver(Arc::new(LinkResolver {
                v4: link.v4.is_some(),
                v6: link.v6.is_some(),
            }))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        match link.local_ip() {
            Some(ip) => builder
                .local_address(ip)
                .dns_resolver(Arc::new(LinkResolver {
                    v4: ip.is_ipv4(),
                    v6: ip.is_ipv6(),
                })),
            None => builder,
        }
    }
}

/// 规划并构建本任务的额外链路（coordinator 后台调用，不阻塞首连接）。
///
/// 解析目标 → 探测主链路 → 枚举网卡 → [`plan_links`] → 逐链路构建绑定
/// client（构建失败的链路记入失败记忆并跳过）。
pub async fn prepare_links(url: &str, input: &MultiNicInput) -> PreparedLinks {
    let parsed = reqwest::Url::parse(url).ok();
    let host = parsed
        .as_ref()
        .and_then(|u| u.host_str())
        .unwrap_or_default()
        .to_string();
    if input.blocked_by_proxy {
        return PreparedLinks {
            host,
            outcome: Err(off_reason::PROXY),
        };
    }
    let port = parsed
        .as_ref()
        .and_then(reqwest::Url::port_or_known_default)
        .unwrap_or(443);
    // IP 字面量（含 IPv6 的 `[..]` 形式）不经 DNS；只对域名解析。
    let literal = parsed.as_ref().and_then(|u| match u.host() {
        Some(url::Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
        _ => None,
    });
    let targets: Vec<IpAddr> = match literal {
        Some(ip) => vec![ip],
        None => match tokio::net::lookup_host((host.as_str(), port)).await {
            Ok(addrs) => addrs.map(|addr| addr.ip()).collect(),
            Err(e) => {
                log_info!("[multi-nic] host {} 解析失败: {e}", host);
                Vec::new()
            }
        },
    };
    let probe_targets = targets.clone();
    let scan = tokio::task::spawn_blocking(move || {
        let primary = probe_targets
            .iter()
            .copied()
            .find(|&ip| !is_local_target(ip))
            .and_then(primary_source);
        (primary, enumerate_nics())
    })
    .await;
    let (primary_src, nics) = match scan {
        Ok((primary, Ok(nics))) => (primary, nics),
        Ok((primary, Err(e))) => {
            log_info!("[multi-nic] 网卡枚举失败: {e}");
            (primary, Vec::new())
        }
        Err(e) => {
            log_info!("[multi-nic] 网卡枚举任务异常: {e}");
            (None, Vec::new())
        }
    };
    match plan_links(&nics, &targets, primary_src, is_recently_failed) {
        LinkPlan::Off(reason) => {
            log_info!("[multi-nic] host {} 不聚合: {}", host, reason);
            PreparedLinks {
                host,
                outcome: Err(reason),
            }
        }
        LinkPlan::Links {
            primary,
            links,
            skipped,
        } => {
            log_info!(
                "[multi-nic] host {} 主链路 {}，额外链路 {:?}，排除 {:?}",
                host,
                primary,
                links.iter().map(LinkBinding::label).collect::<Vec<_>>(),
                skipped
            );
            let mut built: Vec<(Arc<LinkBinding>, Client)> = Vec::with_capacity(links.len());
            for link in links {
                match crate::downloader::build_link_client(
                    &input.user_agent,
                    input.ignore_tls_errors,
                    &link,
                ) {
                    Ok(client) => built.push((Arc::new(link), client)),
                    Err(e) => {
                        log_info!("[multi-nic] {} client 构建失败，跳过: {e}", link.label());
                        record_link_failure(&link);
                    }
                }
            }
            let outcome = if built.is_empty() {
                Err(off_reason::NO_EXTRA)
            } else {
                Ok(built)
            };
            PreparedLinks { host, outcome }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn loopback_names_are_virtual_but_local_area_connection_is_not() {
        for name in ["lo", "lo0", "LO1"] {
            assert!(is_virtual_name(name), "{name}");
        }
        for name in ["Local Area Connection", "Local Area Connection 2"] {
            assert!(!is_virtual_name(name), "{name}");
        }
    }

    fn nic(name: &str, index: u32, v4: &[(&str, u8)], v6: &[&str]) -> NicInfo {
        NicInfo {
            name: name.to_string(),
            index,
            up: true,
            p2p: false,
            v4: v4.iter().map(|(ip, p)| (ip.parse().unwrap(), *p)).collect(),
            v6: v6.iter().map(|ip| (ip.parse().unwrap(), 64)).collect(),
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn never(_: &LinkBinding) -> bool {
        false
    }

    fn link_names(plan: &LinkPlan) -> Vec<String> {
        match plan {
            LinkPlan::Links { links, .. } => links.iter().map(|l| l.name.clone()).collect(),
            LinkPlan::Off(reason) => panic!("expected links, got Off({reason})"),
        }
    }

    const TARGET: &str = "203.0.113.10";

    #[test]
    fn ethernet_and_wifi_on_the_same_router_are_one_uplink() {
        let nics = [
            nic("en0", 1, &[("192.168.1.10", 24)], &["2001:db8:1:1::10"]),
            nic("en1", 2, &[("192.168.1.11", 24)], &["2001:db8:1:1::11"]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), never);
        assert_eq!(plan, LinkPlan::Off(off_reason::NO_EXTRA));
    }

    #[test]
    fn hotspot_on_a_different_network_is_an_extra_link() {
        let nics = [
            nic("en0", 1, &[("192.168.1.10", 24)], &[]),
            nic("en1", 2, &[("172.20.10.2", 28)], &[]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), never);
        let LinkPlan::Links { primary, links, .. } = plan else {
            panic!("expected links");
        };
        assert_eq!(primary, "en0");
        assert_eq!(
            links,
            vec![LinkBinding {
                name: "en1".into(),
                index: 2,
                v4: Some("172.20.10.2".parse().unwrap()),
                v6: None,
            }]
        );
    }

    #[test]
    fn nested_subnets_count_as_the_same_lan() {
        // /16 与 /24 重叠：从 /16 一侧看两者同网。
        let nics = [
            nic("eth0", 1, &[("10.0.5.2", 16)], &[]),
            nic("eth1", 2, &[("10.0.9.3", 24)], &[]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("10.0.5.2")), never);
        assert_eq!(plan, LinkPlan::Off(off_reason::NO_EXTRA));
    }

    #[test]
    fn shared_ipv6_prefix_means_same_router_even_with_distinct_ipv4() {
        let nics = [
            nic("en0", 1, &[("192.168.1.10", 24)], &["2001:db8:1:1::10"]),
            nic("en1", 2, &[("10.1.0.2", 24)], &["2001:db8:1:1::20"]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), never);
        assert_eq!(plan, LinkPlan::Off(off_reason::NO_EXTRA));
    }

    #[test]
    fn two_extra_links_on_the_same_lan_are_deduplicated() {
        let nics = [
            nic("en0", 1, &[("192.168.1.10", 24)], &[]),
            nic("en5", 5, &[("192.168.8.100", 24)], &[]),
            nic("en6", 6, &[("192.168.8.101", 24)], &[]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), never);
        assert_eq!(link_names(&plan), vec!["en5".to_string()]);
    }

    #[test]
    fn vpn_or_tunnel_primary_disables_aggregation() {
        let mut tun = nic("utun4", 9, &[("10.8.0.2", 24)], &[]);
        tun.p2p = true;
        let nics = [tun, nic("en0", 1, &[("192.168.1.10", 24)], &[])];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("10.8.0.2")), never);
        assert_eq!(plan, LinkPlan::Off(off_reason::VPN));

        // Windows 上的 TAP/Wintun 适配器不带点对点标志，按名称识别。
        let nics = [
            nic("OpenVPN TAP-Windows6", 7, &[("10.8.0.2", 24)], &[]),
            nic("Wi-Fi", 3, &[("192.168.1.10", 24)], &[]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("10.8.0.2")), never);
        assert_eq!(plan, LinkPlan::Off(off_reason::VPN));
    }

    #[test]
    fn tunnels_and_virtual_adapters_are_never_extra_links() {
        let mut wg = nic("wg0", 4, &[("10.9.0.2", 24)], &[]);
        wg.p2p = true;
        let nics = [
            nic("eth0", 1, &[("192.168.1.10", 24)], &[]),
            wg,
            nic("docker0", 5, &[("172.17.0.1", 16)], &[]),
            nic("vEthernet (WSL)", 6, &[("172.28.0.1", 20)], &[]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), never);
        assert_eq!(plan, LinkPlan::Off(off_reason::NO_EXTRA));
    }

    #[test]
    fn fake_ip_and_local_targets_disable_aggregation() {
        let nics = [
            nic("en0", 1, &[("192.168.1.10", 24)], &[]),
            nic("en1", 2, &[("172.20.10.2", 28)], &[]),
        ];
        let src = Some(ip("192.168.1.10"));
        assert_eq!(
            plan_links(&nics, &[ip("198.18.0.42")], src, never),
            LinkPlan::Off(off_reason::FAKE_IP)
        );
        for local in [
            "192.168.1.50",
            "10.0.0.2",
            "100.100.1.1",
            "127.0.0.1",
            "fd00::1",
        ] {
            assert_eq!(
                plan_links(&nics, &[ip(local)], src, never),
                LinkPlan::Off(off_reason::LOCAL_TARGET),
                "{local}"
            );
        }
        assert_eq!(
            plan_links(&nics, &[], src, never),
            LinkPlan::Off(off_reason::DNS)
        );
    }

    #[test]
    fn extra_link_needs_an_address_family_the_target_has() {
        let nics = [
            nic("en0", 1, &[("192.168.1.10", 24)], &["2001:db8:1:1::10"]),
            // 热点只有 IPv6（链路本地 IPv4 不可用），目标只有 IPv4。
            nic("en1", 2, &[("169.254.3.3", 16)], &["2001:db8:2:2::20"]),
        ];
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), never);
        assert_eq!(plan, LinkPlan::Off(off_reason::NO_EXTRA));

        // 目标双栈时 IPv6 链路可用，且只填目标具备的地址族。
        let plan = plan_links(
            &nics,
            &[ip(TARGET), ip("2001:db8:ffff::1")],
            Some(ip("192.168.1.10")),
            never,
        );
        let LinkPlan::Links { links, .. } = plan else {
            panic!("expected links");
        };
        assert_eq!(links[0].v4, None);
        assert_eq!(links[0].v6, Some("2001:db8:2:2::20".parse().unwrap()));
    }

    #[test]
    fn unknown_primary_and_recent_failures_are_respected() {
        let nics = [
            nic("en0", 1, &[("192.168.1.10", 24)], &[]),
            nic("en1", 2, &[("172.20.10.2", 28)], &[]),
        ];
        assert_eq!(
            plan_links(&nics, &[ip(TARGET)], None, never),
            LinkPlan::Off(off_reason::PRIMARY_UNKNOWN)
        );
        // 系统出口地址不属于任何枚举到的网卡（例如被隐藏的隧道）。
        assert_eq!(
            plan_links(&nics, &[ip(TARGET)], Some(ip("10.66.0.1")), never),
            LinkPlan::Off(off_reason::PRIMARY_UNKNOWN)
        );
        let failed = |l: &LinkBinding| l.name == "en1";
        assert_eq!(
            plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), failed),
            LinkPlan::Off(off_reason::NO_EXTRA)
        );
    }

    #[test]
    fn down_interfaces_are_skipped_and_extra_links_are_capped() {
        let mut down = nic("en9", 9, &[("10.50.0.2", 24)], &[]);
        down.up = false;
        let mut nics = vec![nic("en0", 1, &[("192.168.1.10", 24)], &[]), down];
        for i in 0..5u8 {
            nics.push(nic(
                &format!("usb{i}"),
                20 + u32::from(i),
                &[(&format!("172.16.{i}.2"), 24)],
                &[],
            ));
        }
        let plan = plan_links(&nics, &[ip(TARGET)], Some(ip("192.168.1.10")), never);
        let names = link_names(&plan);
        assert_eq!(names.len(), MAX_EXTRA_LINKS);
        assert!(!names.contains(&"en9".to_string()));
    }

    #[test]
    fn link_failure_memory_is_keyed_by_name_and_address() {
        let link = LinkBinding {
            name: "test-nic-memory".into(),
            index: 77,
            v4: Some("172.20.10.9".parse().unwrap()),
            v6: None,
        };
        assert!(!is_recently_failed(&link));
        record_link_failure(&link);
        assert!(is_recently_failed(&link));
        let readdressed = LinkBinding {
            v4: Some("172.20.10.10".parse().unwrap()),
            ..link
        };
        assert!(!is_recently_failed(&readdressed), "换地址视为新链路");
    }
}
