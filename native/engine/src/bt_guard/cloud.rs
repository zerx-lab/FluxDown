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

/// 客户端名字规则的 wire 形态：云名单 `clientRules` 与本机 `bt_guard_client_rules` 共用。
///
/// `action` / `seedingOnly` 是旧客户端读取的字段（旧客户端只认 throttle / choke，遇到别的值
/// 整条丢弃）；`verdict` / `when` / `banMinutes` 是新增字段，存在时覆盖前两者。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudClientRule {
    pub id: String,
    /// peerIdPrefix | clientName
    pub field: String,
    /// prefix | contains
    pub op: String,
    pub value: String,
    /// allow | throttle | choke | ban
    pub action: String,
    pub seeding_only: bool,
    pub reason: String,
    /// allow | throttle | choke | ban；存在时覆盖 `action`。
    pub verdict: Option<String>,
    /// seeding | downloading | always；存在时覆盖 `seeding_only`。
    pub when: Option<String>,
    /// `ban` 的封禁时长（分钟），缺省 [`DEFAULT_NAME_BAN_MINUTES`]。
    pub ban_minutes: Option<u32>,
}

/// 名字规则封禁的默认时长（3 天）与上限（1 年）。
pub const DEFAULT_NAME_BAN_MINUTES: u32 = 3 * 24 * 60;
pub const MAX_NAME_BAN_MINUTES: u32 = 365 * 24 * 60;
/// 本机名字规则条数上限。
pub const MAX_LOCAL_CLIENT_RULES: usize = 200;

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

/// 规则生效的任务状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleWhen {
    /// 本端已拥有全部分片。
    Seeding,
    /// 本端仍在下载。
    Downloading,
    #[default]
    Always,
}

impl RuleWhen {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seeding => "seeding",
            Self::Downloading => "downloading",
            Self::Always => "always",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Seeding, Self::Downloading, Self::Always]
            .into_iter()
            .find(|w| w.as_str() == text)
    }

    pub fn applies(self, seeding: bool) -> bool {
        match self {
            Self::Seeding => seeding,
            Self::Downloading => !seeding,
            Self::Always => true,
        }
    }
}

/// 编译后的名字规则。按列表顺序首条命中生效；`Allow` 表示放行（不再看后面的名字规则）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "StoredClientRule")]
pub struct ClientRule {
    pub id: String,
    pub field: ClientField,
    pub op: ClientOp,
    pub value: String,
    pub action: FilterAction,
    pub when: RuleWhen,
    /// 仅 `Ban` 使用。
    pub ban_minutes: u32,
    pub reason: String,
}

/// 落库形态：兼容只有 `seedingOnly` 的旧记录。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredClientRule {
    id: String,
    field: ClientField,
    op: ClientOp,
    value: String,
    action: FilterAction,
    #[serde(default)]
    seeding_only: bool,
    #[serde(default)]
    when: Option<RuleWhen>,
    #[serde(default)]
    ban_minutes: Option<u32>,
    #[serde(default)]
    reason: String,
}

impl From<StoredClientRule> for ClientRule {
    fn from(s: StoredClientRule) -> Self {
        Self {
            id: s.id,
            field: s.field,
            op: s.op,
            value: s.value,
            action: s.action,
            when: s.when.unwrap_or(if s.seeding_only {
                RuleWhen::Seeding
            } else {
                RuleWhen::Always
            }),
            ban_minutes: s.ban_minutes.unwrap_or(DEFAULT_NAME_BAN_MINUTES),
            reason: s.reason,
        }
    }
}

fn starts_with_ignore_case(hay: &[u8], needle: &[u8]) -> bool {
    hay.get(..needle.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(needle))
}

fn contains_ignore_case(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

impl ClientRule {
    /// 忽略 ASCII 大小写匹配。`client` 是扩展握手的 `v` 字段。
    pub fn matches(&self, peer_id: Option<&[u8; 20]>, client: Option<&str>) -> bool {
        let needle = self.value.as_bytes();
        if needle.is_empty() {
            return false;
        }
        let hay: Option<&[u8]> = match self.field {
            ClientField::PeerIdPrefix => peer_id.map(<[u8; 20]>::as_slice),
            ClientField::ClientName => client.map(str::as_bytes),
        };
        hay.is_some_and(|hay| match self.op {
            ClientOp::Prefix => starts_with_ignore_case(hay, needle),
            ClientOp::Contains => contains_ignore_case(hay, needle),
        })
    }

    /// 校验并编译一条 wire 规则；任何字段不合法返回 `None`。
    pub fn compile(raw: &CloudClientRule) -> Option<Self> {
        let field = match raw.field.as_str() {
            "peerIdPrefix" => ClientField::PeerIdPrefix,
            "clientName" => ClientField::ClientName,
            _ => return None,
        };
        let op = match raw.op.as_str() {
            "prefix" => ClientOp::Prefix,
            "contains" => ClientOp::Contains,
            _ => return None,
        };
        let action = FilterAction::parse(raw.verdict.as_deref().unwrap_or(&raw.action))?;
        let when = match raw.when.as_deref() {
            Some(text) => RuleWhen::parse(text)?,
            None if raw.seeding_only => RuleWhen::Seeding,
            None => RuleWhen::Always,
        };
        if raw.value.is_empty() || raw.id.is_empty() {
            return None;
        }
        Some(Self {
            id: raw.id.clone(),
            field,
            op,
            value: raw.value.clone(),
            action,
            when,
            ban_minutes: raw
                .ban_minutes
                .unwrap_or(DEFAULT_NAME_BAN_MINUTES)
                .clamp(1, MAX_NAME_BAN_MINUTES),
            reason: raw.reason.clone(),
        })
    }
}

/// 解析本机名字规则（`bt_guard_client_rules`，JSON 数组）。空串 = 无规则；
/// 整体不是合法 JSON 时记日志并视为无规则，单条不合法只丢该条；超过上限的部分丢弃。
pub fn parse_local_client_rules(json: &str) -> Vec<ClientRule> {
    if json.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<CloudClientRule>>(json) {
        Ok(raw) => raw
            .iter()
            .filter_map(ClientRule::compile)
            .take(MAX_LOCAL_CLIENT_RULES)
            .collect(),
        Err(error) => {
            crate::log_warn!("[BtGuard] ignored malformed local client rules: {error}");
            Vec::new()
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
        match ClientRule::compile(rule) {
            Some(rule) => build.client_rules.push(rule),
            None => build.dropped_invalid_rules += 1,
        }
    }
    Ok(build)
}
