//! 云名单 payload（契约 §5）的 daemon 侧应用逻辑。验签不在 engine（agent 负责）；
//! 这里只做防护：format / 过期 / sequence 防回滚 / 金丝雀覆盖 / 50% 差异防护 / 宽条目丢弃，
//! 以及 clientRules 的编译与匹配。

use std::collections::HashSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use super::model::FilterAction;
use super::ranges::IpRange;

pub const CLOUD_FORMAT: &str = "fluxdown-bt-guard-list/1";

/// 内置金丝雀（以内置为准，文档自带的 `canaries` 不参与判定）。
pub const BUILTIN_CANARIES: [&str; 4] = ["1.1.1.1", "8.8.8.8", "223.5.5.5", "2606:4700:4700::1111"];

/// 差异防护：上一份条目数达到该值才启用。
const DIFF_GUARD_MIN_PREVIOUS: usize = 1000;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudEntry {
    pub cidr: String,
    pub category: String,
    pub action: String,
    pub reason: String,
    pub expires_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudClientRule {
    pub id: String,
    /// peerIdPrefix | clientName
    pub field: String,
    /// prefix | contains
    pub op: String,
    pub value: String,
    /// throttle | choke
    pub action: String,
    pub seeding_only: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudDocument {
    pub format: String,
    pub sequence: i64,
    pub generated_at: i64,
    pub expires_at: i64,
    pub entries: Vec<CloudEntry>,
    pub client_rules: Vec<CloudClientRule>,
    pub canaries: Vec<String>,
    pub attribution: Vec<String>,
}

impl CloudDocument {
    pub fn from_value(value: serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value).map_err(|e| format!("invalid cloud payload: {e}"))
    }
}

/// 整份文档被拒绝的原因（保留上一份，`Display` 写入 `rejectedReason`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudReject {
    Format(String),
    Expired {
        expires_at: i64,
    },
    Rollback {
        current: i64,
        got: i64,
    },
    CanaryCovered {
        cidr: String,
        canary: String,
    },
    DiffTooLarge {
        added: usize,
        removed: usize,
        previous: usize,
    },
}

impl std::fmt::Display for CloudReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(found) => write!(f, "unsupported format: {found:?}"),
            Self::Expired { expires_at } => write!(f, "document expired at {expires_at}"),
            Self::Rollback { current, got } => {
                write!(f, "sequence rollback: have {current}, got {got}")
            }
            Self::CanaryCovered { cidr, canary } => {
                write!(f, "entry {cidr} covers canary {canary}")
            }
            Self::DiffTooLarge {
                added,
                removed,
                previous,
            } => write!(
                f,
                "diff too large: +{added} -{removed} against {previous} previous entries"
            ),
        }
    }
}

impl std::error::Error for CloudReject {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltCloudEntry {
    pub range: IpRange,
    pub action: FilterAction,
    pub category: String,
    pub reason: String,
    pub expires_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientField {
    PeerIdPrefix,
    ClientName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientOp {
    Prefix,
    Contains,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRule {
    pub id: String,
    pub field: ClientField,
    pub op: ClientOp,
    pub value: String,
    pub action: FilterAction,
    pub seeding_only: bool,
    pub reason: String,
}

impl ClientRule {
    pub fn matches(&self, peer_id: Option<&[u8; 20]>, client: Option<&str>) -> bool {
        match self.field {
            ClientField::PeerIdPrefix => peer_id.is_some_and(|id| match self.op {
                ClientOp::Prefix => id.starts_with(self.value.as_bytes()),
                ClientOp::Contains => id
                    .windows(self.value.len())
                    .any(|w| w == self.value.as_bytes()),
            }),
            ClientField::ClientName => client.is_some_and(|name| match self.op {
                ClientOp::Prefix => name.starts_with(&self.value),
                ClientOp::Contains => name.contains(&self.value),
            }),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloudBuild {
    pub entries: Vec<BuiltCloudEntry>,
    pub client_rules: Vec<ClientRule>,
    pub dropped_wide: usize,
    pub dropped_invalid: usize,
    pub dropped_expired: usize,
    pub dropped_invalid_rules: usize,
}

fn canary_ips() -> impl Iterator<Item = IpAddr> {
    BUILTIN_CANARIES.iter().filter_map(|c| c.parse().ok())
}

/// 校验并编译文档。`current_sequence` 为已应用文档的 sequence（无则 0）；
/// `previous` 为已应用文档的条目键集合、`previous_valid_until` 为其过期时刻（0 = 无）。
/// 差异防护只在「上一份仍有效且版本连续（sequence 间隔 ≤ 1）」时启用：长期离线的客户端
/// （上一份已过期，或跳了版本）拿到的新文档本来就该与旧文档差异很大，不能被永久卡死。
/// 纯函数，不改任何状态。
pub fn build_cloud_layer(
    doc: &CloudDocument,
    now: i64,
    current_sequence: i64,
    previous: &HashSet<(IpRange, FilterAction)>,
    previous_valid_until: i64,
) -> Result<CloudBuild, CloudReject> {
    if doc.format != CLOUD_FORMAT {
        return Err(CloudReject::Format(doc.format.clone()));
    }
    if doc.expires_at <= now {
        return Err(CloudReject::Expired {
            expires_at: doc.expires_at,
        });
    }
    if doc.sequence < current_sequence {
        return Err(CloudReject::Rollback {
            current: current_sequence,
            got: doc.sequence,
        });
    }

    let mut build = CloudBuild::default();
    let mut seen: HashSet<(IpRange, FilterAction)> = HashSet::new();
    for entry in &doc.entries {
        let Ok(range) = IpRange::parse_cidr(&entry.cidr) else {
            build.dropped_invalid += 1;
            continue;
        };
        // 金丝雀检查先于宽度丢弃：覆盖金丝雀的宽条目同样是整份文档作废的信号。
        if let Some(canary) = canary_ips().find(|ip| range.contains(*ip)) {
            return Err(CloudReject::CanaryCovered {
                cidr: entry.cidr.clone(),
                canary: canary.to_string(),
            });
        }
        let action = match FilterAction::parse(&entry.action) {
            Some(a @ (FilterAction::Ban | FilterAction::Choke | FilterAction::Throttle)) => a,
            _ => {
                build.dropped_invalid += 1;
                continue;
            }
        };
        if range.is_wide() {
            build.dropped_wide += 1;
            continue;
        }
        let expires_at = entry.expires_at.unwrap_or(doc.expires_at);
        if expires_at <= now {
            build.dropped_expired += 1;
            continue;
        }
        if !seen.insert((range, action)) {
            continue;
        }
        build.entries.push(BuiltCloudEntry {
            range,
            action,
            category: entry.category.clone(),
            reason: entry.reason.clone(),
            expires_at,
        });
    }

    let added = seen.difference(previous).count();
    let removed = previous.difference(&seen).count();
    let previous_alive = previous_valid_until == 0 || previous_valid_until > now;
    let continuous = doc.sequence <= current_sequence.saturating_add(1);
    if previous_alive
        && continuous
        && previous.len() >= DIFF_GUARD_MIN_PREVIOUS
        && (added + removed) * 2 > previous.len()
    {
        return Err(CloudReject::DiffTooLarge {
            added,
            removed,
            previous: previous.len(),
        });
    }

    for rule in &doc.client_rules {
        let field = match rule.field.as_str() {
            "peerIdPrefix" => ClientField::PeerIdPrefix,
            "clientName" => ClientField::ClientName,
            _ => {
                build.dropped_invalid_rules += 1;
                continue;
            }
        };
        let op = match rule.op.as_str() {
            "prefix" => ClientOp::Prefix,
            "contains" => ClientOp::Contains,
            _ => {
                build.dropped_invalid_rules += 1;
                continue;
            }
        };
        let action = match FilterAction::parse(&rule.action) {
            Some(a @ (FilterAction::Choke | FilterAction::Throttle)) => a,
            _ => {
                build.dropped_invalid_rules += 1;
                continue;
            }
        };
        if rule.value.is_empty() || rule.id.is_empty() {
            build.dropped_invalid_rules += 1;
            continue;
        }
        build.client_rules.push(ClientRule {
            id: rule.id.clone(),
            field,
            op,
            value: rule.value.clone(),
            action,
            seeding_only: rule.seeding_only,
            reason: rule.reason.clone(),
        });
    }
    Ok(build)
}
