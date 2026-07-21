//! 设备互联领域类型。
//!
//! 这些类型是子系统内部的**领域模型**（非 wire 契约）。HTTP wire 类型定义在
//! `fluxdown_api::types`（camelCase JSON）；Dart↔Rust 信号类型定义在
//! `hub::signals`。本模块只描述引擎内部如何表达「发现的设备 / 已配对设备 /
//! 传输候选」，与具体序列化格式解耦。

use serde::{Deserialize, Serialize};

/// 设备被发现的途径。
///
/// **扩展点**：未来新增发现方式（如登录账户名册回填、二维码带地址）只需加变体，
/// 发现层 [`crate::link::discovery::Discovery`] 的实现互不影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryKind {
    /// mDNS 局域网自动发现（`_fluxdown._tcp.local.`）。
    Mdns,
    /// 用户手动输入地址后 `/ping` 探测。
    Manual,
}

/// 到达一台**已知**设备数据面的连接策略。
///
/// **可扩展性核心**：v1 仅实现 [`TransportKind::Direct`]（网络可达直连，无打洞）。
/// 未来的 NAT 穿透策略（iroh QUIC 打洞、云中继）作为新变体加入，并各自实现
/// [`crate::link::transport::Transport`] trait，注册进
/// [`crate::link::transport::TransportStack`] 的优先级链——配对协议、数据面调度、
/// UI 全部只依赖 trait 抽象，新增打洞方式**不触碰本 seam 以上任何代码**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportKind {
    /// 网络可达直连：直接拨号候选 `ip:port`，无 NAT 穿透。v1 唯一实现。
    Direct,
    /// iroh QUIC 打洞（未来）。占位——尚未实现，仅保留 seam。
    Iroh,
    /// 云中继兜底（未来）。占位——尚未实现，仅保留 seam。
    Relay,
}

impl TransportKind {
    /// 稳定字符串标签（日志 / 候选记录序列化）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Direct => "direct",
            TransportKind::Iroh => "iroh",
            TransportKind::Relay => "relay",
        }
    }
}

/// 一台在网络上被发现、**尚未配对**的设备。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPeer {
    /// 对端 Ed25519 身份指纹（经 `/ping` TOFU 获得；未探测到时为 `None`）。
    pub fingerprint: Option<String>,
    /// 设备展示名。
    pub name: String,
    /// 平台标识（windows/macos/linux/android/ios/server）。
    pub platform: Option<String>,
    /// 可达地址（IPv4/IPv6 字符串）。
    pub host: String,
    /// 对端 fluxdown API 端口。
    pub port: u16,
    /// 对端客户端版本（`/ping` 透出）。
    pub app_version: Option<String>,
    /// 发现途径。
    pub kind: DiscoveryKind,
}

/// 到达已配对设备的一个候选端点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCandidate {
    /// 该候选走哪种传输。
    pub kind: TransportKind,
    /// 地址（Direct = `ip:port`；未来 iroh = node id；relay = relay url + route）。
    pub address: String,
}

/// 一台**已配对**并持久化的设备。是本地设备名册（LinkStore）的行模型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecord {
    /// 设备 ID = 对端 Ed25519 公钥指纹（hex(sha256(pub))，64 hex）。主键。
    pub fingerprint: String,
    /// 对端 Ed25519 身份公钥（32 字节），用于后续连接的身份重校验（TOFU 固定）。
    pub identity_pub: Vec<u8>,
    /// 设备展示名。
    pub name: String,
    /// 平台标识。
    pub platform: Option<String>,
    /// 配对时经 X25519 ECDH 派生的**每对设备独立**共享密钥（32 字节），
    /// 用于数据面链路鉴权（HMAC）。绝不上网络明文传输。
    pub link_secret: Vec<u8>,
    /// 已知候选端点（按优先级尝试；Direct 为 `ip:port`）。
    pub candidates: Vec<PeerCandidate>,
    /// 配对时间（Unix 秒）。
    pub paired_at: i64,
    /// 最近一次成功连接/探活时间（Unix 秒）。
    pub last_seen_at: i64,
}

impl PeerRecord {
    /// 展示用短指纹（前 12 hex）。
    #[must_use]
    pub fn short_fingerprint(&self) -> String {
        self.fingerprint.chars().take(12).collect()
    }

    /// 首个 Direct 候选地址（供数据面拼 base_url）。
    #[must_use]
    pub fn direct_address(&self) -> Option<&str> {
        self.candidates
            .iter()
            .find(|c| c.kind == TransportKind::Direct)
            .map(|c| c.address.as_str())
    }
}
