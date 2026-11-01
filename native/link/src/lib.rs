//! 本地设备互联（device link）—— P2P 局域网配对 + mDNS 发现 + 网络可达直连。
//!
//! 与宿主无关：宿主（agent / 冻结的 server）只需实现 [`LinkStorage`]
//! （身份种子 + 已配对名册的持久化），并驱动 [`LinkManager`]。
//!
//! # 分层（可扩展性设计）
//! - [`identity`]：Ed25519 本机身份（设备 ID = 公钥指纹，TOFU 固定）。
//! - [`address`]：对端地址模型（http/https、域名、反代 base path）。
//! - [`discovery`]：发现层：mDNS 广播/浏览 + 手动地址 `/ping` 探测。
//! - [`pairing`]：配对协议（一次性码 + 临时公钥承诺-揭示 + X25519 ECDH + SAS + 身份签名）。
//! - [`transport`]：数据面直连（拨号候选地址 + `/ping` 身份复核）。
//! - [`storage`]：持久化 trait（身份种子 + 已配对设备名册）。
//! - [`crypto`]：指纹 / SAS / 链路密钥 / 数据面 v2 的 MAC 与 AEAD 原语。
//! - [`text`]：对端自报文本的清洗（控制 / 双向控制字符、长度）。

pub mod address;
pub mod crypto;
pub mod discovery;
pub mod error;
pub mod identity;
pub mod manager;
pub mod pairing;
pub mod storage;
pub mod text;
pub mod transport;
pub mod types;
pub mod wire;

pub use address::PeerAddress;
pub use crypto::{LINK_CONTROL_VERSION, LINK_ENC_V2, LINK_PROTOCOL_VERSION};
pub use error::{LinkError, LinkResult};
pub use identity::LinkIdentity;
pub use manager::{
    BeginPairingResult, LINK_INFO_PATH, LINK_TASKS_PATH, LINK_UNPAIR_PATH, LinkEngineEvent,
    LinkManager, LinkOptions, LinkRequest, PairConfirmOutcome, PeerInfoAnswer, WireHello,
    WireHelloResponse, WireReveal, WireRevealResponse,
};
pub use pairing::{
    HelloRequest, HelloResponse, PAIRING_PROTOCOL_VERSION, PairingInitiator, PairingResponder,
    RevealAccepted, RevealRequest, RevealResponse, SelfInfo,
};
pub use storage::{LinkStorage, RecordUpdate};
pub use transport::{DirectTransport, PeerConn};
pub use types::{
    DiscoveredPeer, DiscoveryKind, MAX_DIRECT_CANDIDATES, PeerCandidate, PeerInfo, PeerRecord,
    TransportKind,
};
