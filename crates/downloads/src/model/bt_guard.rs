//! 连接页签的防吸血投影：嫌疑 / 处置分类、右键菜单动作到 `entries.add` 参数的映射，
//! 以及「查看证据」对话框用的条目 / 事件匹配与证据字段展开。全部为纯函数，便于单测。

use fluxdown_protocol::{BtGuardEntriesAddParams, BtPeerDto};
use fluxdown_ui_i18n::Translator;
use serde_json::Value;

use super::format_bytes;

/// 嫌疑等级（`BtPeerDto::suspicion`；缺失 / `none` / 未知值都视为无）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Suspicion {
    None,
    Weak,
    Medium,
    Strong,
}

impl Suspicion {
    pub(crate) fn of(peer: &BtPeerDto) -> Self {
        match peer.suspicion.as_deref().map(str::trim) {
            Some("weak") => Self::Weak,
            Some("medium") => Self::Medium,
            Some("strong") => Self::Strong,
            _ => Self::None,
        }
    }

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::None => "btGuardSuspicionNone",
            Self::Weak => "btGuardSuspicionWeak",
            Self::Medium => "btGuardSuspicionMedium",
            Self::Strong => "btGuardSuspicionStrong",
        }
    }
}

/// 当前处置（`BtPeerDto::action`；未知值按放行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeerAction {
    Allow,
    Throttle,
    Choke,
    Banned,
}

impl PeerAction {
    pub(crate) fn of(peer: &BtPeerDto) -> Self {
        match peer.action.trim() {
            "throttle" => Self::Throttle,
            "choke" => Self::Choke,
            "banned" => Self::Banned,
            _ => Self::Allow,
        }
    }

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Allow => "btGuardActionAllow",
            Self::Throttle => "btGuardActionThrottle",
            Self::Choke => "btGuardActionChoke",
            Self::Banned => "btGuardActionBanned",
        }
    }
}

/// 一个连接在防吸血视角下的投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerGuard<'a> {
    pub(crate) group: &'a str,
    pub(crate) delivered: u64,
    pub(crate) suspicion: Suspicion,
    pub(crate) rules: &'a [String],
    pub(crate) action: PeerAction,
    /// 命中规则但仅观察：`action` 是本应执行的动作而未真正执行。
    pub(crate) observe_only: bool,
}

impl<'a> PeerGuard<'a> {
    /// 防吸血未启用 / 旧 daemon（没有 IP 组）时为 `None`。
    pub(crate) fn of(peer: &'a BtPeerDto) -> Option<Self> {
        let group = peer.ip_group.trim();
        (!group.is_empty()).then(|| Self {
            group,
            delivered: peer.group_delivered_bytes,
            suspicion: Suspicion::of(peer),
            rules: &peer.rules,
            action: PeerAction::of(peer),
            observe_only: peer.observe_only,
        })
    }
}

/// `{prefix}{CamelValue}` 的翻译；目录里没有该值（旧客户端遇到新枚举）时显示原始值。
pub(crate) fn label_or_raw(translator: &Translator, prefix: &str, value: &str) -> String {
    let camel: String = value
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect();
    let key = format!("{prefix}{camel}");
    let text = translator.text(&key);
    if text == key {
        value.to_owned()
    } else {
        text.to_owned()
    }
}

/// 命中规则的本地化名称，逗号分隔；没有命中时为空串。
pub(crate) fn rules_text(translator: &Translator, rules: &[String]) -> String {
    rules
        .iter()
        .map(|rule| label_or_raw(translator, "btGuardRule", rule))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 处置文案；放行为 `None`。仅观察时标注「（仅观察）」，表示这是本应执行而未执行的动作。
pub(crate) fn action_text(translator: &Translator, guard: &PeerGuard<'_>) -> Option<String> {
    if guard.action == PeerAction::Allow {
        return None;
    }
    let label = translator.text(guard.action.label_key()).to_owned();
    Some(if guard.observe_only {
        translator.text_with("btGuardPeerObserved", &[("action", &label)])
    } else {
        label
    })
}

/// 悬浮提示的证据摘要：IP 组 / 累计上传 / 嫌疑 / 命中规则 / 处置。
pub(crate) fn tooltip_text(translator: &Translator, guard: &PeerGuard<'_>) -> String {
    let t = |key: &str| translator.text(key).to_owned();
    let mut lines = vec![
        format!("{}: {}", t("btGuardPeerGroup"), guard.group),
        format!(
            "{}: {}",
            t("btGuardPeerUploaded"),
            format_bytes(guard.delivered)
        ),
        format!(
            "{}: {}",
            t("btGuardPeerSuspicion"),
            t(guard.suspicion.label_key())
        ),
    ];
    if !guard.rules.is_empty() {
        lines.push(format!(
            "{}: {}",
            t("btGuardPeerRules"),
            rules_text(translator, guard.rules)
        ));
    }
    if let Some(action) = action_text(translator, guard) {
        lines.push(format!("{}: {action}", t("btGuardPeerAction")));
    }
    lines.join("\n")
}

/// 状态里是否有任何连接带防吸血信息（决定是否显示新增列）。
pub(crate) fn any_guard(peers: &[BtPeerDto]) -> bool {
    peers.iter().any(|peer| PeerGuard::of(peer).is_some())
}

/// 临时封禁时长预设。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BanChoice {
    Hour,
    Day,
    Week,
    Forever,
}

impl BanChoice {
    pub(crate) const ALL: [Self; 4] = [Self::Hour, Self::Day, Self::Week, Self::Forever];

    pub(crate) const fn secs(self) -> Option<i64> {
        match self {
            Self::Hour => Some(3_600),
            Self::Day => Some(86_400),
            Self::Week => Some(7 * 86_400),
            Self::Forever => None,
        }
    }

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Hour => "btGuardPeerBan1h",
            Self::Day => "btGuardPeerBan1d",
            Self::Week => "btGuardPeerBan7d",
            Self::Forever => "btGuardPeerBanForever",
        }
    }
}

/// 限速动作的有效期（秒）：与菜单文案「限速 1 天」一致。
pub(crate) const THROTTLE_SECS: i64 = 86_400;

/// 右键菜单里的防吸血动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuardMenuAction {
    Ban(BanChoice),
    Throttle,
    Allow,
}

impl GuardMenuAction {
    /// 成功提示的文案键（参数 `{group}`）。
    pub(crate) const fn success_key(self) -> &'static str {
        match self {
            Self::Ban(_) => "btGuardPeerBanned",
            Self::Throttle => "btGuardPeerThrottled",
            Self::Allow => "btGuardPeerAllowed",
        }
    }
}

/// 菜单动作 → `daemon.bt.guard.entries.add` 参数；IP 组为空（防吸血未启用）时无动作可做。
pub(crate) fn entry_params(
    group: &str,
    action: GuardMenuAction,
) -> Option<BtGuardEntriesAddParams> {
    let group = group.trim();
    if group.is_empty() {
        return None;
    }
    let (wire, duration_secs) = match action {
        GuardMenuAction::Ban(choice) => ("ban", choice.secs()),
        GuardMenuAction::Throttle => ("throttle", Some(THROTTLE_SECS)),
        GuardMenuAction::Allow => ("allow", None),
    };
    Some(BtGuardEntriesAddParams {
        cidr: group.to_owned(),
        action: wire.to_owned(),
        duration_secs,
        reason: String::new(),
    })
}

/// 证据 / 事件详情里的一个字段（已知键带文案键，其余原样）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EvidenceLine {
    pub(crate) label_key: Option<&'static str>,
    pub(crate) raw_key: String,
    pub(crate) value: String,
}

/// 已知数值证据键：`(JSON 键, 文案键, 渲染)`。
type NumericKey = (&'static str, &'static str, fn(u64) -> String);

const NUMERIC_KEYS: &[NumericKey] = &[
    ("torrentSize", "btGuardEvTorrentSize", super::format_bytes),
    ("deliveredBytes", "btGuardEvDelivered", super::format_bytes),
    ("dupBytes", "btGuardEvDup", super::format_bytes),
    ("ratioMilli", "btGuardEvRatio", ratio_text),
    ("rerequests", "btGuardEvRerequests", count_text),
    ("rewindPermille", "btGuardEvRewind", permille_text),
    ("strikes", "btGuardEvStrikes", count_text),
];

const TEXT_KEYS: &[(&str, &str)] = &[
    ("detail", "btGuardEvDetail"),
    ("category", "btGuardEvCategory"),
];

fn ratio_text(milli: u64) -> String {
    format!("{}%", milli / 10)
}

fn permille_text(permille: u64) -> String {
    format!("{:.1}%", permille as f64 / 10.0)
}

fn count_text(count: u64) -> String {
    count.to_string()
}

/// 把证据 JSON 展开成有序字段：已知数值（为 0 的省略）→ 已知文本 → 其余键按名排序。
pub(crate) fn evidence_lines(value: &Value) -> Vec<EvidenceLine> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for (key, label_key, render) in NUMERIC_KEYS {
        if let Some(number) = object.get(*key).and_then(Value::as_u64)
            && number > 0
        {
            lines.push(EvidenceLine {
                label_key: Some(label_key),
                raw_key: (*key).to_owned(),
                value: render(number),
            });
        }
    }
    for (key, label_key) in TEXT_KEYS {
        if let Some(text) = object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            lines.push(EvidenceLine {
                label_key: Some(label_key),
                raw_key: (*key).to_owned(),
                value: text.to_owned(),
            });
        }
    }
    let mut rest: Vec<_> = object
        .iter()
        .filter(|(key, _)| {
            let key = key.as_str();
            key != "rule"
                && !NUMERIC_KEYS.iter().any(|known| known.0 == key)
                && !TEXT_KEYS.iter().any(|known| known.0 == key)
        })
        .collect();
    rest.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in rest {
        let text = match value {
            Value::Null => continue,
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        lines.push(EvidenceLine {
            label_key: None,
            raw_key: key.clone(),
            value: text,
        });
    }
    lines
}

/// 事件 `detail` 文本 → 字段：JSON 对象展开，其余整体作为一条详情。
pub(crate) fn event_detail_lines(detail: &str) -> Vec<EvidenceLine> {
    let detail = detail.trim();
    if detail.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(detail) {
        Ok(value @ Value::Object(_)) => evidence_lines(&value),
        _ => vec![EvidenceLine {
            label_key: Some("btGuardEvDetail"),
            raw_key: "detail".to_owned(),
            value: detail.to_owned(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn peer() -> BtPeerDto {
        BtPeerDto {
            addr: "1.2.3.4:6881".to_owned(),
            ip_group: "1.2.3.4".to_owned(),
            ..BtPeerDto::default()
        }
    }

    #[test]
    fn suspicion_levels_order_and_unknown_values_are_none() {
        let mut p = peer();
        assert_eq!(Suspicion::of(&p), Suspicion::None);
        for (wire, level) in [
            ("none", Suspicion::None),
            ("weak", Suspicion::Weak),
            ("medium", Suspicion::Medium),
            ("strong", Suspicion::Strong),
            (" strong ", Suspicion::Strong),
            ("critical", Suspicion::None),
        ] {
            p.suspicion = Some(wire.to_owned());
            assert_eq!(Suspicion::of(&p), level, "{wire:?}");
        }
        assert!(Suspicion::Strong > Suspicion::Medium && Suspicion::Medium > Suspicion::Weak);
    }

    #[test]
    fn peer_actions_map_wire_values() {
        let mut p = peer();
        assert_eq!(PeerAction::of(&p), PeerAction::Allow);
        for (wire, action) in [
            ("allow", PeerAction::Allow),
            ("throttle", PeerAction::Throttle),
            ("choke", PeerAction::Choke),
            ("banned", PeerAction::Banned),
            ("future-action", PeerAction::Allow),
        ] {
            p.action = wire.to_owned();
            assert_eq!(PeerAction::of(&p), action, "{wire:?}");
        }
    }

    #[test]
    fn guard_projection_requires_an_ip_group() {
        let mut p = peer();
        p.ip_group = "  ".to_owned();
        assert!(PeerGuard::of(&p).is_none());
        assert!(!any_guard(&[p.clone()]));
        p.ip_group = "2001:db8::/64".to_owned();
        p.group_delivered_bytes = 5 * 1024 * 1024 * 1024;
        let guard = PeerGuard::of(&p);
        assert_eq!(guard.as_ref().map(|g| g.group), Some("2001:db8::/64"));
        assert_eq!(
            guard.as_ref().map(|g| g.delivered),
            Some(5 * 1024 * 1024 * 1024)
        );
        assert!(any_guard(&[peer(), p]));
    }

    #[test]
    fn ban_choices_cover_the_menu_durations() {
        let secs: Vec<_> = BanChoice::ALL.iter().map(|c| c.secs()).collect();
        assert_eq!(secs, vec![Some(3_600), Some(86_400), Some(604_800), None]);
    }

    #[test]
    fn menu_actions_become_entry_params() {
        let ban = entry_params("1.2.3.4", GuardMenuAction::Ban(BanChoice::Week)).unwrap();
        assert_eq!(
            (ban.cidr.as_str(), ban.action.as_str(), ban.duration_secs),
            ("1.2.3.4", "ban", Some(604_800))
        );
        let forever =
            entry_params("2001:db8::/64", GuardMenuAction::Ban(BanChoice::Forever)).unwrap();
        assert_eq!(forever.duration_secs, None);
        let throttle = entry_params(" 1.2.3.4 ", GuardMenuAction::Throttle).unwrap();
        assert_eq!(
            (
                throttle.cidr.as_str(),
                throttle.action.as_str(),
                throttle.duration_secs
            ),
            ("1.2.3.4", "throttle", Some(THROTTLE_SECS))
        );
        let allow = entry_params("1.2.3.4", GuardMenuAction::Allow).unwrap();
        assert_eq!(
            (allow.action.as_str(), allow.duration_secs),
            ("allow", None)
        );
        assert!(entry_params("", GuardMenuAction::Allow).is_none());
        assert!(entry_params("   ", GuardMenuAction::Throttle).is_none());
    }

    #[test]
    fn evidence_lines_follow_a_fixed_order_and_skip_zeros() {
        let value = json!({
            "rule": "rewind",
            "rewindPermille": 125,
            "torrentSize": 1024,
            "dupBytes": 0,
            "ratioMilli": 3700,
            "detail": " lost 12% ",
            "extra": 5,
            "gone": null,
        });
        let keys: Vec<_> = evidence_lines(&value)
            .into_iter()
            .map(|line| (line.raw_key, line.value))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("torrentSize".to_owned(), "1.0 KB".to_owned()),
                ("ratioMilli".to_owned(), "370%".to_owned()),
                ("rewindPermille".to_owned(), "12.5%".to_owned()),
                ("detail".to_owned(), "lost 12%".to_owned()),
                ("extra".to_owned(), "5".to_owned()),
            ]
        );
        assert!(evidence_lines(&json!("text")).is_empty());
        assert!(evidence_lines(&json!(null)).is_empty());
    }

    #[test]
    fn event_detail_accepts_json_objects_and_plain_text() {
        assert!(event_detail_lines("  ").is_empty());
        let lines = event_detail_lines(r#"{"strikes":2}"#);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].value, "2");
        let plain = event_detail_lines("boom");
        assert_eq!(plain[0].label_key, Some("btGuardEvDetail"));
        assert_eq!(plain[0].value, "boom");
        assert_eq!(event_detail_lines("[1]").len(), 1);
    }
}

#[cfg(test)]
mod text_tests {
    use std::sync::Arc;

    use fluxdown_protocol::BtPeerDto;
    use fluxdown_ui_i18n::{I18nCatalog, Translator};

    use super::{PeerGuard, action_text, label_or_raw, rules_text, tooltip_text};

    fn translator(locale: &str) -> Result<Translator, Box<dyn std::error::Error>> {
        let catalog = Arc::new(I18nCatalog::load_embedded()?);
        Ok(catalog.translator(locale))
    }

    fn flagged_peer() -> BtPeerDto {
        BtPeerDto {
            ip_group: "1.2.3.4".to_owned(),
            group_delivered_bytes: 3 * 1024 * 1024,
            suspicion: Some("strong".to_owned()),
            rules: vec!["rewind".to_owned(), "future_rule".to_owned()],
            action: "throttle".to_owned(),
            ..BtPeerDto::default()
        }
    }

    #[test]
    fn labels_use_the_catalog_and_fall_back_to_the_raw_value()
    -> Result<(), Box<dyn std::error::Error>> {
        let t = translator("en")?;
        assert_eq!(
            label_or_raw(&t, "btGuardEventKind", "probe_cleared"),
            "Probe cleared"
        );
        assert_eq!(label_or_raw(&t, "btGuardLayer", "cloud"), "Cloud");
        assert_eq!(
            label_or_raw(&t, "btGuardLayer", "future_layer"),
            "future_layer"
        );
        assert_eq!(label_or_raw(&t, "btGuardRule", ""), "");
        Ok(())
    }

    #[test]
    fn rules_are_localized_and_unknown_ones_kept() -> Result<(), Box<dyn std::error::Error>> {
        let peer = flagged_peer();
        assert_eq!(
            rules_text(&translator("en")?, &peer.rules),
            "Progress rewind, future_rule"
        );
        assert_eq!(
            rules_text(&translator("zh")?, &peer.rules),
            "进度回退, future_rule"
        );
        assert_eq!(rules_text(&translator("en")?, &[]), "");
        Ok(())
    }

    #[test]
    fn action_text_marks_observe_only_and_hides_allow() -> Result<(), Box<dyn std::error::Error>> {
        let t = translator("en")?;
        let mut peer = flagged_peer();
        assert_eq!(
            PeerGuard::of(&peer)
                .and_then(|g| action_text(&t, &g))
                .as_deref(),
            Some("Throttle")
        );
        peer.observe_only = true;
        assert_eq!(
            PeerGuard::of(&peer)
                .and_then(|g| action_text(&t, &g))
                .as_deref(),
            Some("Throttle (observe only)")
        );
        peer.action = "allow".to_owned();
        assert_eq!(PeerGuard::of(&peer).and_then(|g| action_text(&t, &g)), None);
        Ok(())
    }

    #[test]
    fn tooltip_lists_group_volume_suspicion_rules_and_action()
    -> Result<(), Box<dyn std::error::Error>> {
        let peer = flagged_peer();
        let guard = PeerGuard::of(&peer).ok_or("guard")?;
        assert_eq!(
            tooltip_text(&translator("en")?, &guard),
            "IP group: 1.2.3.4\nUploaded to group: 3.0 MB\nSuspicion: Strong\nRules: Progress rewind, future_rule\nAction: Throttle"
        );
        let quiet = BtPeerDto {
            ip_group: "5.6.7.8".to_owned(),
            ..BtPeerDto::default()
        };
        let guard = PeerGuard::of(&quiet).ok_or("guard")?;
        assert_eq!(
            tooltip_text(&translator("en")?, &guard),
            "IP group: 5.6.7.8\nUploaded to group: 0 B\nSuspicion: None"
        );
        Ok(())
    }
}
