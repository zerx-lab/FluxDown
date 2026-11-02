//! bt_guard 的公共数据模型：事件 / 上下文 / 判决 / 条目 / 证据。
//!
//! `GuardCtx` / `GuardEvent` / `Verdict` 与契约 §1 的 `PeerCtx` / `PeerEvent` / `PeerVerdict`
//! 字段一一对应，宿主胶水做一对一转换。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::bitset::PieceSet;
use super::ranges::IpRange;
use crate::db::BtGuardEntryRow;

pub type InfoHash = [u8; 20];

/// 单连接内只升不降（`Ord` 即严重度）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Verdict {
    #[default]
    Allow,
    Throttle,
    Choke,
    Disconnect,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Throttle => "throttle",
            Self::Choke => "choke",
            Self::Disconnect => "disconnect",
        }
    }
}

/// 对应 `PeerCtx`。`addr` 可为 IPv4-mapped，guard 内部再做归一。
#[derive(Clone, Debug)]
pub struct GuardCtx {
    pub info_hash: InfoHash,
    pub addr: SocketAddr,
    pub private: bool,
    pub total_length: u64,
    pub piece_length: u32,
    pub num_pieces: u32,
    pub we_are_seeding: bool,
}

/// 对应 `PeerEvent`。
#[derive(Clone, Copy, Debug)]
pub enum GuardEvent<'a> {
    Handshake {
        peer_id: [u8; 20],
        reserved: u64,
        incoming: bool,
    },
    ExtendedHandshake {
        client: Option<&'a str>,
    },
    Bitfield {
        bits: &'a PieceSet,
    },
    Have {
        piece: u32,
    },
    Request {
        piece: u32,
        begin: u32,
        length: u32,
        peer_claims_piece: bool,
    },
    Cancel {
        piece: u32,
        begin: u32,
        length: u32,
    },
    Uploaded {
        piece: u32,
        begin: u32,
        length: u32,
    },
    Disconnected {
        bits: Option<&'a PieceSet>,
    },
}

/// 检测规则。`Cloud` 为云名单 IP 条目命中。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    Excessive,
    Rerequest,
    Rewind,
    Lag,
    Fingerprint,
    Cloud,
}

impl Rule {
    pub const ALL: [Rule; 6] = [
        Rule::Excessive,
        Rule::Rerequest,
        Rule::Rewind,
        Rule::Lag,
        Rule::Fingerprint,
        Rule::Cloud,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Excessive => "excessive",
            Self::Rerequest => "rerequest",
            Self::Rewind => "rewind",
            Self::Lag => "lag",
            Self::Fingerprint => "fingerprint",
            Self::Cloud => "cloud",
        }
    }

    pub(crate) fn bit(self) -> u8 {
        1 << (self as u8)
    }

    pub fn strength(self) -> Strength {
        match self {
            Self::Excessive | Self::Rerequest | Self::Rewind => Strength::Strong,
            Self::Lag => Strength::Medium,
            Self::Fingerprint | Self::Cloud => Strength::Weak,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Strength {
    Weak,
    Medium,
    Strong,
}

impl Strength {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Weak => "weak",
            Self::Medium => "medium",
            Self::Strong => "strong",
        }
    }
}

/// 过滤层。优先级 `Allow > Manual > {Auto, Cloud, File}`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    Allow,
    Manual,
    Auto,
    Cloud,
    File,
}

impl Layer {
    pub const ALL: [Layer; 5] = [
        Layer::Allow,
        Layer::Manual,
        Layer::Auto,
        Layer::Cloud,
        Layer::File,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Manual => "manual",
            Self::Auto => "auto",
            Self::Cloud => "cloud",
            Self::File => "file",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == text)
    }

    pub(crate) fn slot(self) -> usize {
        self as usize
    }
}

/// 条目动作，按严重度升序（`Allow` 仅 allow 层使用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterAction {
    Allow,
    Throttle,
    Choke,
    Ban,
}

impl FilterAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Throttle => "throttle",
            Self::Choke => "choke",
            Self::Ban => "ban",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Self::Allow, Self::Throttle, Self::Choke, Self::Ban]
            .into_iter()
            .find(|a| a.as_str() == text)
    }

    pub(crate) fn verdict(self) -> Verdict {
        match self {
            Self::Allow => Verdict::Allow,
            Self::Throttle => Verdict::Throttle,
            Self::Choke => Verdict::Choke,
            Self::Ban => Verdict::Disconnect,
        }
    }
}

/// 过滤条目。`range` 恒为单个 CIDR 块（文件区间在入库前已分解）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub layer: Layer,
    pub range: IpRange,
    pub action: FilterAction,
    pub rule: Arc<str>,
    pub reason: Arc<str>,
    /// 证据 JSON 摘要。
    pub evidence: Option<Arc<str>>,
    pub source: Arc<str>,
    pub created_at: i64,
    /// `None` = 永久。到期惰性判断。
    pub expires_at: Option<i64>,
}

impl Entry {
    pub fn is_active(&self, now: i64) -> bool {
        self.expires_at.is_none_or(|t| t > now)
    }

    pub fn cidr(&self) -> String {
        self.range.cidr_string().unwrap_or_default()
    }

    pub fn remaining_secs(&self, now: i64) -> Option<i64> {
        self.expires_at.map(|t| (t - now).max(0))
    }

    pub fn to_row(&self) -> BtGuardEntryRow {
        BtGuardEntryRow {
            id: self.id,
            layer: self.layer.as_str().to_owned(),
            cidr: self.cidr(),
            action: self.action.as_str().to_owned(),
            rule: self.rule.to_string(),
            reason: self.reason.to_string(),
            evidence: self.evidence.as_deref().unwrap_or_default().to_owned(),
            source: self.source.to_string(),
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }

    pub fn from_row(row: &BtGuardEntryRow) -> Result<Self, String> {
        let layer =
            Layer::parse(&row.layer).ok_or_else(|| format!("unknown layer {:?}", row.layer))?;
        let action = FilterAction::parse(&row.action)
            .ok_or_else(|| format!("unknown action {:?}", row.action))?;
        let range = IpRange::parse_cidr(&row.cidr).map_err(|e| e.to_string())?;
        Ok(Self {
            id: row.id,
            layer,
            range,
            action,
            rule: Arc::from(row.rule.as_str()),
            reason: Arc::from(row.reason.as_str()),
            evidence: (!row.evidence.is_empty()).then(|| Arc::from(row.evidence.as_str())),
            source: Arc::from(row.source.as_str()),
            created_at: row.created_at,
            expires_at: row.expires_at,
        })
    }
}

/// 需要宿主执行 / 通知的副作用（热路径只入队，宿主在事件驱动点取走）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardAction {
    /// 封禁集合变化：宿主应对所有 live 种子执行
    /// `disconnect_peers_where(|ip| guard.is_banned(ip))`。
    DisconnectBanned,
    /// 条目 / 状态变化（对应 `BtGuardChanged`）。
    Changed,
    /// 待上报证据队列有新增（对应 `BtGuardEvidenceReady`）。
    EvidenceReady,
}

/// 今日（本地日）统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuardStats {
    pub observed: u64,
    pub enforced: u64,
    pub blocked_connections: u64,
    pub refused_bytes: u64,
}

/// 条目计数（未过期）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerCounts {
    pub allow: usize,
    pub manual: usize,
    pub auto: usize,
    pub cloud: usize,
    pub file: usize,
}

/// 待上报证据（契约 §6 单条）。不含文件名 / 路径 / 种子名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceItem {
    pub ip_group: String,
    pub rule: String,
    pub strength: String,
    /// 40 位 hex（FluxCloud 收到后立即 HMAC）。
    pub info_hash: String,
    pub torrent_size: u64,
    pub delivered_bytes: u64,
    pub dup_bytes: u64,
    pub ratio_milli: u64,
    pub rerequests: u32,
    pub rewind_permille: u32,
    pub peer_id_prefix: String,
    pub client: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// 单个 peer 的当前研判（供 `BtPeerDto` 扩展字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInsight {
    pub ip_group: String,
    pub group_delivered_bytes: u64,
    /// none | weak | medium | strong
    pub suspicion: &'static str,
    pub rules: Vec<&'static str>,
    /// allow | throttle | choke | banned
    pub action: &'static str,
    /// 命中但仅观察（未执行任何动作）。
    pub observe_only: bool,
}

pub(crate) fn ip_of(addr: SocketAddr) -> IpAddr {
    super::ip::normalize_ip(addr.ip())
}
