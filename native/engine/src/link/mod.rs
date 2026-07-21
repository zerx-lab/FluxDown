//! 本地设备互联（device link）—— P2P 局域网配对 + mDNS 发现 + 可扩展直连传输。
//!
//! 仅 `link` feature 下编译（desktop hub + headless server 开启；mobile 关闭）。
//!
//! # 分层（可扩展性设计）
//! - [`identity`]：Ed25519 本机身份（设备 ID = 公钥指纹，TOFU 固定）。
//! - [`discovery`]：发现层 trait + mDNS/手动实现（找到可加入/可连接的设备）。
//! - [`pairing`]：配对协议（一次性码 + X25519 ECDH + SAS + 身份签名）。
//! - [`transport`]：**数据面传输 seam** —— Direct(v1) / 未来 iroh、relay 插拔。
//! - [`store`]：已配对设备名册持久化。
//! - [`crypto`]：指纹 / SAS / 链路密钥 / HMAC 鉴权原语。

pub mod crypto;
pub mod discovery;
pub mod error;
pub mod identity;
pub mod manager;
pub mod pairing;
pub mod store;
pub mod transport;
pub mod types;

pub use error::{LinkError, LinkResult};
pub use identity::LinkIdentity;
pub use manager::{BeginPairingResult, LinkEngineEvent, LinkManager, WireHello, WireHelloResponse};
pub use pairing::{HelloRequest, HelloResponse, PairingInitiator, PairingResponder, SelfInfo};
pub use store::LinkStore;
pub use transport::{DirectTransport, PeerConn, Transport, TransportStack};
pub use types::{DiscoveredPeer, DiscoveryKind, PeerCandidate, PeerRecord, TransportKind};
