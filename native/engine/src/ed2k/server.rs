//! eD2K 服务器会话 —— 登录 + GETSOURCES 找源（leech-only，仅出站直连）。
//!
//! [`find_sources`] 逐服务器尝试：TCP 连接 → `OP_LOGINREQUEST` → 收
//! `OP_IDCHANGE`（跳过其间的 MOTD/状态帧）→ `OP_GETSOURCES` → 收
//! `OP_FOUNDSOURCES`。任一步失败/超时/畸形帧即换下一服务器；对端 LowID
//! 源直接跳过（直连不可达）；过滤本机自身 ID:port（防自连接）。
//!
//! **反滥用节流**：进程级 [`static@LAST_LOGIN`] 记录每 `host:port` 上次登录
//! 时刻，`MIN_RELOGIN_INTERVAL` 内不重复登录（多任务并发时避免把服务器打成
//! 异常特征触发封禁）。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::downloader::DownloadError;
use crate::ed2k::proto::{
    self, Ed2kMessage, LOWID_THRESHOLD, MAX_SERVER_FRAME, OP_GETSOURCES, OP_LOGINREQUEST,
    SRV_TCPFLG_AUXPORT, SRV_TCPFLG_COMPRESSION, SRV_TCPFLG_LARGEFILES, SRV_TCPFLG_NEWTAGS,
    SRV_TCPFLG_UNICODE,
};
use crate::logger::{log_error, log_info};

/// 单服务器连接+登录+找源的总超时。
const SERVER_TIMEOUT: Duration = Duration::from_secs(8);

/// 同一 `host:port` 两次登录的最小间隔（反滥用节流）。
const MIN_RELOGIN_INTERVAL: Duration = Duration::from_secs(60);

/// 一个可直连的 HighID peer 地址。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerAddr {
    /// peer 的 IPv4 地址。
    pub ip: Ipv4Addr,
    /// peer 的 TCP 端口。
    pub port: u16,
}

impl std::fmt::Display for PeerAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

/// 把 `OP_FOUNDSOURCES` 里的 client ID（HighID）转为 IPv4 地址。
///
/// eD2K 的 HighID = IPv4 四字节按**小端**打包为 u32（即 `a.b.c.d` →
/// `a | b<<8 | c<<16 | d<<24`），故还原时低字节即第一段。
///
/// **字节序 `unverified — confirm first`**：对照 aMule `OP_FOUNDSOURCES`
/// 解析核实；若实际为大端，改用 `Ipv4Addr::from(id.to_be_bytes())`。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::server::id_to_ipv4;
/// use std::net::Ipv4Addr;
/// // 1.2.3.4 小端打包 = 0x04030201
/// assert_eq!(id_to_ipv4(0x0403_0201), Ipv4Addr::new(1, 2, 3, 4));
/// ```
#[must_use]
pub fn id_to_ipv4(id: u32) -> Ipv4Addr {
    let b = id.to_le_bytes();
    Ipv4Addr::new(b[0], b[1], b[2], b[3])
}

/// 进程级登录节流缓存：`host:port` → 上次登录时刻。
static LAST_LOGIN: OnceLock<StdMutex<HashMap<String, Instant>>> = OnceLock::new();

fn last_login_cache() -> &'static StdMutex<HashMap<String, Instant>> {
    LAST_LOGIN.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// 该服务器是否在节流窗口内（近 `MIN_RELOGIN_INTERVAL` 已登录过）。
fn is_throttled(server: &str) -> bool {
    if let Ok(cache) = last_login_cache().lock()
        && let Some(last) = cache.get(server)
    {
        return last.elapsed() < MIN_RELOGIN_INTERVAL;
    }
    false
}

/// 记录一次登录时刻。
fn mark_login(server: &str) {
    if let Ok(mut cache) = last_login_cache().lock() {
        cache.insert(server.to_owned(), Instant::now());
    }
}

/// 判定一个 IPv4 是否**可能**是一台单播主机（能作为 eD2K 服务器拨号目标）。
///
/// 只拒绝在任何网络下都**不可能是单播主机**的段：未指定(0/8)、组播(224/4)、
/// 广播(255.255.255.255)、保留(240/4)。这些正是字节序解析 bug 把正常地址
/// 打乱后可能落入的段（如反转后落入 `239.x` 组播 → `os error 10049`）。
///
/// **不**拒绝私网/回环/链路本地：eMule 允许 LAN 内自建服务器，且这些是合法
/// 单播地址；能连通与否交由实际 TCP 连接判定，不在此预筛。
#[must_use]
pub fn is_routable_server_ip(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()      // 0.0.0.0
        || ip.is_multicast()   // 224.0.0.0/4（含 239.x）
        || ip.is_broadcast()   // 255.255.255.255
        || o[0] == 0           // 0.0.0.0/8
        || o[0] >= 240) // 240.0.0.0/4 保留（含 255）
}

/// 解析 `host:port` 字符串为 `(host, port)`；格式非法返回 `None`。
///
/// 若 host 是字面 IPv4 且落在不可能是单播主机的段（见 [`is_routable_server_ip`]），
/// 一并拒绝；host 是主机名（非字面 IP）时无从判定，放行。
fn parse_server_addr(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    let (host, port_str) = s.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    let port: u16 = port_str.parse().ok()?;
    if port == 0 {
        return None;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>()
        && !is_routable_server_ip(ip)
    {
        return None;
    }
    Some((host.to_owned(), port))
}

/// 把逗号分隔的 `ed2k_server_list` 配置值解析为服务器地址列表。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::server::parse_server_list;
/// let list = parse_server_list("1.2.3.4:4661, 5.6.7.8:4242");
/// assert_eq!(list, vec!["1.2.3.4:4661".to_string(), "5.6.7.8:4242".to_string()]);
/// ```
#[must_use]
pub fn parse_server_list(config_value: &str) -> Vec<String> {
    config_value
        .split(',')
        .map(str::trim)
        .filter(|s| parse_server_addr(s).is_some())
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// eD2K tag 编码（登录用）
// ---------------------------------------------------------------------------

/// tag 特殊名（单字节 name）常量。
const CT_NAME: u8 = 0x01;
const CT_VERSION: u8 = 0x11;
const CT_SERVER_FLAGS: u8 = 0x20;
/// eMule 版本 tag（现代 Lugdunum 服务器据此判定客户端非"过旧的 eDonkey"；
/// 缺失即被拒 "Your edonkey client is too old"）。
const CT_EMULE_VERSION: u8 = 0xFB;

/// tag 类型字节。
const TAGTYPE_STRING: u8 = 0x02;
const TAGTYPE_UINT32: u8 = 0x03;

/// eDonkey 客户端版本号（登录 CT_VERSION 值，取 eMule 兼容常见值 0x3C=60）。
const ED2K_VERSION: u32 = 0x3C;

/// eMule 版本号（CT_EMULE_VERSION 值）：`(major<<24)|(minor<<17)|(tiny<<10)|(1<<7)`。
/// 取 jed2k/goed2k 同款 1.1.0 → `(1<<24)|(1<<17)|(0<<10)|(1<<7)` = 0x01020080。
const EMULE_VERSION: u32 = (1 << 24) | (1 << 17) | (1 << 7);

/// 编码一个"特殊名"字符串 tag：`<type 1><name_len 2 LE=1><name 1><value>`。
fn encode_string_tag(name: u8, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TAGTYPE_STRING);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.push(name);
    let bytes = value.as_bytes();
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// 编码一个"特殊名"u32 tag：`<type 1><name_len 2 LE=1><name 1><value 4 LE>`。
fn encode_u32_tag(name: u8, value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TAGTYPE_UINT32);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.push(name);
    out.extend_from_slice(&value.to_le_bytes());
    out
}

/// 构造 `OP_LOGINREQUEST` payload：`user_hash(16) + client_id(4) + port(2) + tags`。
///
/// `listen_port` 为本机 TCP 监听端口：报 0 = 声明 LowID（不接收入站）；
/// 报真实监听端口 = 请求 HighID（服务器验证可回连后分配公网 ID）。
pub(crate) fn build_login_payload(listen_port: u16) -> Vec<u8> {
    // 固定 user hash（leech-only 身份无实际意义，末两字节按 eMule 惯例标记）。
    let mut user_hash = [0u8; 16];
    user_hash[5] = 14;
    user_hash[14] = 111;

    let flags = SRV_TCPFLG_COMPRESSION
        | SRV_TCPFLG_AUXPORT
        | SRV_TCPFLG_NEWTAGS
        | SRV_TCPFLG_UNICODE
        | SRV_TCPFLG_LARGEFILES;
    let tags: Vec<Vec<u8>> = vec![
        encode_u32_tag(CT_VERSION, ED2K_VERSION),
        encode_u32_tag(CT_SERVER_FLAGS, flags),
        encode_string_tag(CT_NAME, "FluxDown"),
        encode_u32_tag(CT_EMULE_VERSION, EMULE_VERSION),
    ];

    let mut payload = Vec::new();
    payload.extend_from_slice(&user_hash);
    payload.extend_from_slice(&0u32.to_le_bytes()); // client_id = 0（服务器分配）
    payload.extend_from_slice(&listen_port.to_le_bytes());
    payload.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for t in tags {
        payload.extend_from_slice(&t);
    }
    payload
}

/// 构造 `OP_GETSOURCES` payload。
///
/// 经典（≤4GB）：`file_hash(16) + size(4 LE)`。
/// 大文件：`file_hash(16) + 0u32 + size(8 LE)`（LARGEFILES 扩展）。
pub(crate) fn build_getsources_payload(
    file_hash: &[u8; 16],
    total_bytes: u64,
    large_file: bool,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(if large_file { 28 } else { 20 });
    payload.extend_from_slice(file_hash);
    if large_file {
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&total_bytes.to_le_bytes());
    } else {
        payload.extend_from_slice(&(total_bytes as u32).to_le_bytes());
    }
    payload
}

// ---------------------------------------------------------------------------
// find_sources
// ---------------------------------------------------------------------------

/// 遍历服务器列表找源，返回可直连的 HighID peer 列表。
///
/// 每服务器独立 [`SERVER_TIMEOUT`] 超时；任一失败换下一个。全部失败返回
/// [`DownloadError::Ed2k`]（调用方据此进入源补给重试/终态）。
///
/// # Errors
///
/// 无可用服务器（全部超时/拒绝/畸形/节流且无缓存）时返回
/// [`DownloadError::Ed2k`]。
pub async fn find_sources(
    servers: &[String],
    file_hash: &[u8; 16],
    total_bytes: u64,
    large_file: bool,
    cancel: &CancellationToken,
) -> Result<Vec<PeerAddr>, DownloadError> {
    for server in servers {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        if is_throttled(server) {
            log_info!("[ed2k-server] {} 处于登录节流窗口，跳过", server);
            continue;
        }
        let Some((host, port)) = parse_server_addr(server) else {
            log_error!("[ed2k-server] 服务器地址格式非法，跳过: {}", server);
            continue;
        };

        match tokio::time::timeout(
            SERVER_TIMEOUT,
            query_one_server(&host, port, file_hash, total_bytes, large_file),
        )
        .await
        {
            Ok(Ok(sources)) => {
                mark_login(server);
                let mut peers = Vec::new();
                for (id, peer_port) in sources {
                    if id < LOWID_THRESHOLD {
                        log_info!("[ed2k-server] 源 id={:#x} 为 LowID，直连不可达，跳过", id);
                        continue;
                    }
                    if peer_port == 0 {
                        continue;
                    }
                    peers.push(PeerAddr {
                        ip: id_to_ipv4(id),
                        port: peer_port,
                    });
                }
                if peers.is_empty() {
                    log_info!("[ed2k-server] {} 返回 0 个可用 HighID 源", server);
                    continue;
                }
                log_info!(
                    "[ed2k-server] {} 返回 {} 个可用 HighID 源",
                    server,
                    peers.len()
                );
                return Ok(peers);
            }
            Ok(Err(e)) => {
                mark_login(server);
                log_info!("[ed2k-server] {} 查询失败，换下一个: {}", server, e);
            }
            Err(_) => {
                log_info!(
                    "[ed2k-server] {} 超时 {:?}，换下一个",
                    server,
                    SERVER_TIMEOUT
                );
            }
        }
    }
    Err(DownloadError::Ed2k("no server available".into()))
}

/// 对单个服务器完成 连接→登录→(跳过 MOTD)→GETSOURCES→FOUNDSOURCES。
/// 返回原始 `(client_id, port)` 源列表（调用方负责 LowID 过滤/去重）。
async fn query_one_server(
    host: &str,
    port: u16,
    file_hash: &[u8; 16],
    total_bytes: u64,
    large_file: bool,
) -> Result<Vec<(u32, u16)>, DownloadError> {
    let mut stream = TcpStream::connect((host, port))
        .await
        .map_err(DownloadError::Io)?;

    // 登录。
    let login = proto::frame(OP_LOGINREQUEST, &build_login_payload(0));
    stream.write_all(&login).await.map_err(DownloadError::Io)?;

    // 收帧直到 IdChange（跳过 ServerMessage/ServerStatus/Unknown）。
    let _client_id = read_until_id_change(&mut stream).await?;

    // GETSOURCES。
    let gs = proto::frame(
        OP_GETSOURCES,
        &build_getsources_payload(file_hash, total_bytes, large_file),
    );
    stream.write_all(&gs).await.map_err(DownloadError::Io)?;

    // 收帧直到匹配 file_hash 的 FoundSources（跳过其它）。
    read_until_found_sources(&mut stream, file_hash).await
}

/// 循环读帧直到 `OP_IDCHANGE`，跳过 MOTD/状态帧；`OP_REJECT` → Err。
pub(crate) async fn read_until_id_change(stream: &mut TcpStream) -> Result<u32, DownloadError> {
    for _ in 0..32 {
        let (proto_byte, opcode, payload) = proto::read_frame(stream, MAX_SERVER_FRAME).await?;
        match proto::dispatch(proto_byte, opcode, &payload, false)? {
            Ed2kMessage::IdChange { client_id } => return Ok(client_id),
            Ed2kMessage::Reject => {
                return Err(DownloadError::Ed2k("server rejected login".into()));
            }
            Ed2kMessage::ServerMessage(msg) => {
                log_info!("[ed2k-server] MOTD: {}", msg.replace('\n', " "));
            }
            Ed2kMessage::ServerStatus | Ed2kMessage::Unknown(_) => {}
            _ => {}
        }
    }
    Err(DownloadError::Ed2k("no IdChange after 32 frames".into()))
}

/// 循环读帧直到匹配 `file_hash` 的 `OP_FOUNDSOURCES`。
pub(crate) async fn read_until_found_sources(
    stream: &mut TcpStream,
    file_hash: &[u8; 16],
) -> Result<Vec<(u32, u16)>, DownloadError> {
    for _ in 0..32 {
        let (proto_byte, opcode, payload) = proto::read_frame(stream, MAX_SERVER_FRAME).await?;
        match proto::dispatch(proto_byte, opcode, &payload, false)? {
            Ed2kMessage::FoundSources {
                file_hash: fh,
                sources,
            } if &fh == file_hash => {
                return Ok(sources);
            }
            Ed2kMessage::FoundSources { .. } => {
                // 非本文件的源应答，忽略继续等。
            }
            Ed2kMessage::ServerMessage(_) | Ed2kMessage::ServerStatus | Ed2kMessage::Unknown(_) => {
            }
            Ed2kMessage::Reject => {
                return Err(DownloadError::Ed2k("server rejected getsources".into()));
            }
            _ => {}
        }
    }
    Err(DownloadError::Ed2k(
        "no FoundSources after 32 frames".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CT_EMULE_VERSION, build_login_payload, id_to_ipv4, is_routable_server_ip,
        parse_server_addr, parse_server_list,
    };
    use std::net::Ipv4Addr;

    #[test]
    fn id_to_ipv4_little_endian() {
        // 1.2.3.4 小端 = 0x04030201
        assert_eq!(id_to_ipv4(0x0403_0201), Ipv4Addr::new(1, 2, 3, 4));
        assert_eq!(id_to_ipv4(0), Ipv4Addr::new(0, 0, 0, 0));
    }

    #[test]
    fn parse_server_addr_ok() {
        assert_eq!(
            parse_server_addr("1.2.3.4:4661"),
            Some(("1.2.3.4".into(), 4661))
        );
        assert_eq!(
            parse_server_addr(" host.example:80 "),
            Some(("host.example".into(), 80))
        );
    }

    #[test]
    fn parse_server_addr_rejects_bad() {
        assert_eq!(parse_server_addr("noport"), None);
        assert_eq!(parse_server_addr(":4661"), None);
        assert_eq!(parse_server_addr("host:0"), None);
        assert_eq!(parse_server_addr("host:notanumber"), None);
        assert_eq!(parse_server_addr("host:99999"), None);
    }

    #[test]
    fn parse_server_list_filters_invalid() {
        let list = parse_server_list("1.2.3.4:4661,,bad,5.6.7.8:80, host:0 ");
        assert_eq!(
            list,
            vec!["1.2.3.4:4661".to_string(), "5.6.7.8:80".to_string()]
        );
    }

    #[test]
    fn routable_ip_accepts_public() {
        // 已知真实 eD2K 服务器（默认列表）。
        assert!(is_routable_server_ip(Ipv4Addr::new(176, 123, 5, 89)));
        assert!(is_routable_server_ip(Ipv4Addr::new(45, 82, 80, 155)));
        assert!(is_routable_server_ip(Ipv4Addr::new(213, 252, 245, 239)));
    }

    #[test]
    fn routable_ip_rejects_impossible_hosts() {
        // 日志实证的字节序 bug 产物：反转后落进组播段（239.x）。
        assert!(!is_routable_server_ip(Ipv4Addr::new(239, 245, 252, 213)));
        // 其余不可能是单播主机的段。
        assert!(!is_routable_server_ip(Ipv4Addr::new(0, 0, 0, 0)));
        assert!(!is_routable_server_ip(Ipv4Addr::new(0, 1, 2, 3))); // 0/8
        assert!(!is_routable_server_ip(Ipv4Addr::new(224, 0, 0, 1))); // 组播下界
        assert!(!is_routable_server_ip(Ipv4Addr::new(255, 255, 255, 255))); // 广播
        assert!(!is_routable_server_ip(Ipv4Addr::new(240, 0, 0, 1))); // 保留
    }

    #[test]
    fn routable_ip_accepts_private_and_loopback() {
        // 私网/回环是合法单播地址（LAN 自建服务器 / 测试 mock），不预筛。
        assert!(is_routable_server_ip(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_routable_server_ip(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_routable_server_ip(Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[test]
    fn parse_server_addr_rejects_impossible_ip() {
        // 组播字面 IP 直接拒绝；私网/主机名放行。
        assert_eq!(parse_server_addr("239.245.252.213:33333"), None);
        assert_eq!(parse_server_addr("255.255.255.255:4661"), None);
        assert_eq!(
            parse_server_addr("192.168.1.1:4661"),
            Some(("192.168.1.1".into(), 4661))
        );
        assert_eq!(
            parse_server_addr("server.example.com:4661"),
            Some(("server.example.com".into(), 4661))
        );
    }

    #[test]
    fn login_payload_includes_emule_version_tag() {
        // 现代 Lugdunum 服务器缺 0xFB tag 即拒 "client too old"（实网 A/B 验证）。
        // 守护该 tag 存在 + tagCount 正确，防回归。
        let payload = build_login_payload(4662);
        // 头部：user_hash(16) + client_id(4) + port(2) + tagCount(4 LE)。
        assert!(payload.len() > 26, "payload too short");
        let tag_count = u32::from_le_bytes([payload[22], payload[23], payload[24], payload[25]]);
        assert_eq!(tag_count, 4, "expected 4 login tags (incl. eMule version)");
        // 0xFB tag name 字节必须出现在 tag 区（type 0x03 + nameLen 1 + name 0xFB）。
        let has_emule_tag = payload
            .windows(4)
            .any(|w| w[0] == 0x03 && w[1] == 0x01 && w[2] == 0x00 && w[3] == CT_EMULE_VERSION);
        assert!(has_emule_tag, "eMule version tag (0xFB) missing from login");
    }
}
