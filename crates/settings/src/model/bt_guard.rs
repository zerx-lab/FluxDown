//! BT 防吸血设置页的纯视图模型：时长预设、CIDR 校验、剩余时长拆分、条目选择与证据摘要。
//!
//! 全部为无 GPUI 依赖的纯函数，文案键由调用方查表；协议常量与 DTO 以 `fluxdown_protocol` 为准。

use std::collections::HashSet;
use std::net::IpAddr;

use fluxdown_protocol::{
    BT_GUARD_LIST_DEFAULT_LIMIT, BT_GUARD_LIST_MAX_LIMIT, BT_GUARD_REMOVE_MAX_IDS,
    BtGuardEntriesListParams, BtGuardEntryDto, BtGuardEventsListParams,
};
use serde_json::Value;

/// 云层（不可直接删除，引导加白）。
pub(crate) const CLOUD_LAYER: &str = "cloud";

/// 手动添加条目的有效期预设。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DurationChoice {
    Hour,
    Day,
    Week,
    Forever,
}

impl DurationChoice {
    pub(crate) const ALL: [Self; 4] = [Self::Hour, Self::Day, Self::Week, Self::Forever];

    /// 有效期秒数；`None` = 永久。
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
            Self::Hour => "btGuardDuration1h",
            Self::Day => "btGuardDuration1d",
            Self::Week => "btGuardDuration7d",
            Self::Forever => "btGuardDurationForever",
        }
    }
}

/// 手动添加表单可选的动作（`allow` 进白名单层，其余进手动层）。
pub(crate) const ADD_ACTIONS: [&str; 4] = ["ban", "throttle", "choke", "allow"];

/// CIDR 输入校验失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CidrError {
    Empty,
    Invalid,
}

impl CidrError {
    pub(crate) const fn message_key(self) -> &'static str {
        match self {
            Self::Empty => "btGuardErrCidrEmpty",
            Self::Invalid => "btGuardErrCidrInvalid",
        }
    }
}

/// 校验并规范化 `ip` 或 `ip/prefix`：去首尾空白，前缀不得超过地址位宽。
/// 主机位是否对齐、过宽网段的取舍交给 daemon（唯一的校验实现）。
pub(crate) fn normalize_cidr(input: &str) -> Result<String, CidrError> {
    let text = input.trim();
    if text.is_empty() {
        return Err(CidrError::Empty);
    }
    let (address, prefix) = match text.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (text, None),
    };
    let ip: IpAddr = address.trim().parse().map_err(|_| CidrError::Invalid)?;
    let Some(prefix) = prefix else {
        return Ok(ip.to_string());
    };
    let prefix: u8 = prefix.trim().parse().map_err(|_| CidrError::Invalid)?;
    let width = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > width {
        return Err(CidrError::Invalid);
    }
    Ok(format!("{ip}/{prefix}"))
}

/// 剩余时长的显示单位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DurationUnit {
    Days,
    Hours,
    Minutes,
    Seconds,
}

impl DurationUnit {
    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Days => "btGuardDurationD",
            Self::Hours => "btGuardDurationH",
            Self::Minutes => "btGuardDurationM",
            Self::Seconds => "btGuardDurationS",
        }
    }
}

/// 把秒数折成最粗的至多两级单位（`3d 4h`、`2h`、`15m`、`40s`）；负数按 0。
pub(crate) fn duration_units(secs: i64) -> Vec<(DurationUnit, u64)> {
    let secs = secs.max(0).unsigned_abs();
    let (head, tail) = if secs >= 86_400 {
        (
            (DurationUnit::Days, secs / 86_400),
            (DurationUnit::Hours, secs % 86_400 / 3_600),
        )
    } else if secs >= 3_600 {
        (
            (DurationUnit::Hours, secs / 3_600),
            (DurationUnit::Minutes, secs % 3_600 / 60),
        )
    } else if secs >= 60 {
        (
            (DurationUnit::Minutes, secs / 60),
            (DurationUnit::Seconds, 0),
        )
    } else {
        ((DurationUnit::Seconds, secs), (DurationUnit::Seconds, 0))
    };
    let mut units = vec![head];
    if tail.1 > 0 {
        units.push(tail);
    }
    units
}

/// 距到期的剩余秒数：服务端快照值减去取回后已过去的秒数；永久（`None`）保持 `None`。
pub(crate) fn live_remaining(remaining_secs: Option<i64>, elapsed_secs: u64) -> Option<i64> {
    let elapsed = i64::try_from(elapsed_secs).unwrap_or(i64::MAX);
    remaining_secs.map(|secs| secs.saturating_sub(elapsed).max(0))
}

/// 条目列表请求参数；空查询不带 `query`，`limit` 钳制到协议上限。
pub(crate) fn entries_params(
    layer: Option<&str>,
    rule: Option<&str>,
    query: &str,
    offset: u32,
    limit: u32,
) -> BtGuardEntriesListParams {
    let query = query.trim();
    BtGuardEntriesListParams {
        layer: layer.map(str::to_owned),
        rule: rule.map(str::to_owned),
        query: (!query.is_empty()).then(|| query.to_owned()),
        offset,
        limit: limit.clamp(1, BT_GUARD_LIST_MAX_LIMIT),
        contains_ip: None,
    }
}

/// 事件列表请求参数。
pub(crate) fn events_params(
    kind: Option<&str>,
    rule: Option<&str>,
    offset: u32,
    limit: u32,
) -> BtGuardEventsListParams {
    BtGuardEventsListParams {
        offset,
        limit: limit.clamp(1, BT_GUARD_LIST_MAX_LIMIT),
        rule: rule.map(str::to_owned),
        kind: kind.map(str::to_owned),
        ip_group: None,
    }
}

/// 刷新时保持已加载的条数（至少一页，至多协议上限）。
pub(crate) fn reload_limit(loaded: usize) -> u32 {
    u32::try_from(loaded)
        .unwrap_or(u32::MAX)
        .clamp(BT_GUARD_LIST_DEFAULT_LIMIT, BT_GUARD_LIST_MAX_LIMIT)
}

/// 已选条目里可解除的 id（按列表顺序、云层除外，单次不超过协议上限）与被跳过的云层条数。
pub(crate) fn removable_ids(
    items: &[BtGuardEntryDto],
    selected: &HashSet<i64>,
) -> (Vec<i64>, usize) {
    let mut ids = Vec::new();
    let mut cloud = 0;
    for item in items.iter().filter(|item| selected.contains(&item.id)) {
        if item.layer == CLOUD_LAYER {
            cloud += 1;
        } else if ids.len() < BT_GUARD_REMOVE_MAX_IDS {
            ids.push(item.id);
        }
    }
    (ids, cloud)
}

/// 列表刷新后丢弃已不在列表里的选择。
pub(crate) fn retain_selected(selected: &mut HashSet<i64>, items: &[BtGuardEntryDto]) {
    let present: HashSet<i64> = items.iter().map(|item| item.id).collect();
    selected.retain(|id| present.contains(id));
}

/// 把下拉里的取值映射回协议常量表中的 `'static` 条目；空串（“全部”）与未知值为 `None`。
pub(crate) fn pick_static(list: &'static [&'static str], value: &str) -> Option<&'static str> {
    list.iter().copied().find(|candidate| *candidate == value)
}

/// 字节数 → `12.3 MiB`（二进制单位）。
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 证据字段的取值类别。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EvidenceValue {
    Bytes(u64),
    /// `ratioMilli`：千分比，显示为百分数。
    RatioMilli(u64),
    /// `rewindPermille`：千分比，显示为一位小数的百分数。
    Permille(u64),
    Count(u64),
    Text(String),
}

impl EvidenceValue {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Bytes(bytes) => format_bytes(*bytes),
            Self::RatioMilli(milli) => format!("{}%", milli / 10),
            Self::Permille(permille) => format!("{:.1}%", *permille as f64 / 10.0),
            Self::Count(count) => count.to_string(),
            Self::Text(text) => text.clone(),
        }
    }
}

/// 证据摘要的一行：已知字段带 i18n 标签键，未知字段回落为原始键名。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EvidenceField {
    pub(crate) label_key: Option<&'static str>,
    pub(crate) raw_key: String,
    pub(crate) value: EvidenceValue,
}

/// 已知数值证据键：`(JSON 键, 标签键, 取值解释, 为 0 / 空时是否省略)`。
type NumericEvidenceKey = (&'static str, &'static str, fn(u64) -> EvidenceValue, bool);

const EVIDENCE_KEYS: &[NumericEvidenceKey] = &[
    (
        "torrentSize",
        "btGuardEvTorrentSize",
        EvidenceValue::Bytes,
        true,
    ),
    (
        "deliveredBytes",
        "btGuardEvDelivered",
        EvidenceValue::Bytes,
        true,
    ),
    ("dupBytes", "btGuardEvDup", EvidenceValue::Bytes, true),
    (
        "ratioMilli",
        "btGuardEvRatio",
        EvidenceValue::RatioMilli,
        true,
    ),
    (
        "rerequests",
        "btGuardEvRerequests",
        EvidenceValue::Count,
        true,
    ),
    (
        "rewindPermille",
        "btGuardEvRewind",
        EvidenceValue::Permille,
        true,
    ),
    ("strikes", "btGuardEvStrikes", EvidenceValue::Count, true),
    ("entries", "btGuardEvEntries", EvidenceValue::Count, false),
];

/// 已知文本键。
const EVIDENCE_TEXT_KEYS: &[(&str, &str)] = &[
    ("detail", "btGuardEvDetail"),
    ("category", "btGuardEvCategory"),
    ("url", "btGuardEvUrl"),
];

/// 事件 / 条目里已有独立列的键，不在摘要里重复。
const EVIDENCE_SKIPPED_KEYS: &[&str] = &["rule"];

/// 把证据 / 事件详情 JSON 展开成有序摘要行：已知字段在前（固定顺序），其余按键名排序。
/// 非对象（空、字符串、数组）没有可展开的字段。
pub(crate) fn evidence_fields(value: &Value) -> Vec<EvidenceField> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    for (key, label_key, make, skip_zero) in EVIDENCE_KEYS {
        if let Some(number) = object.get(*key).and_then(Value::as_u64)
            && !(*skip_zero && number == 0)
        {
            fields.push(EvidenceField {
                label_key: Some(label_key),
                raw_key: (*key).to_owned(),
                value: make(number),
            });
        }
    }
    for (key, label_key) in EVIDENCE_TEXT_KEYS {
        if let Some(text) = object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            fields.push(EvidenceField {
                label_key: Some(label_key),
                raw_key: (*key).to_owned(),
                value: EvidenceValue::Text(text.to_owned()),
            });
        }
    }
    let mut rest: Vec<(&String, &Value)> = object
        .iter()
        .filter(|(key, _)| {
            let key = key.as_str();
            !EVIDENCE_SKIPPED_KEYS.contains(&key)
                && !EVIDENCE_KEYS.iter().any(|known| known.0 == key)
                && !EVIDENCE_TEXT_KEYS.iter().any(|known| known.0 == key)
        })
        .collect();
    rest.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in rest {
        let text = match value {
            Value::Null => continue,
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        fields.push(EvidenceField {
            label_key: None,
            raw_key: key.clone(),
            value: EvidenceValue::Text(text),
        });
    }
    fields
}

/// 事件详情文本 → 摘要行：JSON 对象展开；非 JSON 的原文整体作为一行文本。
pub(crate) fn event_detail_fields(detail: &str) -> Vec<EvidenceField> {
    let detail = detail.trim();
    if detail.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(detail) {
        Ok(value @ Value::Object(_)) => evidence_fields(&value),
        _ => vec![EvidenceField {
            label_key: Some("btGuardEvDetail"),
            raw_key: "detail".to_owned(),
            value: EvidenceValue::Text(detail.to_owned()),
        }],
    }
}

/// 云名单署名行里的 URL（供点击打开）；没有则 `None`。
pub(crate) fn attribution_url(line: &str) -> Option<&str> {
    line.split_whitespace()
        .map(|token| token.trim_matches(|c: char| matches!(c, ',' | ';' | '(' | ')')))
        .find(|token| token.starts_with("https://") || token.starts_with("http://"))
}

/// 条目 / 事件的 `rule` 取值（筛选下拉用）：检测规则、云名单，以及手动 / 过滤文件条目的来源标记。
pub(crate) const RULE_FILTERS: &[&str] = &[
    "excessive",
    "rerequest",
    "rewind",
    "lag",
    "fingerprint",
    "cloud",
    "manual",
    "file",
];

/// 事件日志的事件类型（筛选下拉的取值，与引擎写入的 `kind` 一致）。
pub(crate) const EVENT_KINDS: &[&str] = &[
    "observed",
    "enforced",
    "probe",
    "probe_cleared",
    "entry_added",
    "entry_removed",
    "cloud_applied",
    "cloud_rejected",
    "filter_applied",
];
#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use fluxdown_protocol::BtGuardEntryDto;
    use serde_json::json;

    use super::*;

    fn entry(id: i64, layer: &str) -> BtGuardEntryDto {
        BtGuardEntryDto {
            id,
            layer: layer.to_owned(),
            cidr: "1.2.3.4".to_owned(),
            action: "ban".to_owned(),
            ..BtGuardEntryDto::default()
        }
    }

    #[test]
    fn cidr_accepts_addresses_and_ranges_and_normalizes() {
        assert_eq!(normalize_cidr(" 1.2.3.4 "), Ok("1.2.3.4".to_owned()));
        assert_eq!(normalize_cidr("10.0.0.0/24"), Ok("10.0.0.0/24".to_owned()));
        assert_eq!(normalize_cidr("10.0.0.0 / 8"), Ok("10.0.0.0/8".to_owned()));
        assert_eq!(
            normalize_cidr("2001:DB8:0:0::/64"),
            Ok("2001:db8::/64".to_owned())
        );
        assert_eq!(normalize_cidr("::1/128"), Ok("::1/128".to_owned()));
        assert_eq!(normalize_cidr("1.2.3.4/0"), Ok("1.2.3.4/0".to_owned()));
    }

    #[test]
    fn cidr_rejects_empty_and_malformed_input() {
        assert_eq!(normalize_cidr(""), Err(CidrError::Empty));
        assert_eq!(normalize_cidr("   "), Err(CidrError::Empty));
        for bad in [
            "1.2.3",
            "1.2.3.4.5",
            "256.1.1.1",
            "1.2.3.4/33",
            "2001:db8::/129",
            "1.2.3.4/",
            "1.2.3.4/-1",
            "1.2.3.4/a",
            "example.com",
            "1.2.3.4 5.6.7.8",
            "/24",
            "1.2.3.4/24/1",
            "fe80::1%eth0",
        ] {
            assert_eq!(normalize_cidr(bad), Err(CidrError::Invalid), "{bad:?}");
        }
    }

    #[test]
    fn duration_presets_map_to_seconds() {
        let secs: Vec<_> = DurationChoice::ALL.iter().map(|c| c.secs()).collect();
        assert_eq!(secs, vec![Some(3_600), Some(86_400), Some(604_800), None]);
    }

    #[test]
    fn duration_units_pick_the_two_coarsest_levels() {
        use DurationUnit::{Days, Hours, Minutes, Seconds};
        assert_eq!(duration_units(-5), vec![(Seconds, 0)]);
        assert_eq!(duration_units(0), vec![(Seconds, 0)]);
        assert_eq!(duration_units(59), vec![(Seconds, 59)]);
        assert_eq!(duration_units(60), vec![(Minutes, 1)]);
        assert_eq!(duration_units(3_599), vec![(Minutes, 59)]);
        assert_eq!(duration_units(3_600), vec![(Hours, 1)]);
        assert_eq!(
            duration_units(3_600 + 15 * 60 + 9),
            vec![(Hours, 1), (Minutes, 15)]
        );
        assert_eq!(duration_units(86_400), vec![(Days, 1)]);
        assert_eq!(
            duration_units(3 * 86_400 + 4 * 3_600 + 59),
            vec![(Days, 3), (Hours, 4)]
        );
        assert_eq!(duration_units(400 * 86_400), vec![(Days, 400)]);
        assert_eq!(duration_units(i64::MAX).len(), 2);
    }

    #[test]
    fn live_remaining_counts_down_and_keeps_permanent() {
        assert_eq!(live_remaining(None, 1_000), None);
        assert_eq!(live_remaining(Some(100), 40), Some(60));
        assert_eq!(live_remaining(Some(100), 100), Some(0));
        assert_eq!(live_remaining(Some(100), 10_000), Some(0));
        assert_eq!(live_remaining(Some(i64::MIN), 1), Some(0));
        assert_eq!(live_remaining(Some(5), u64::MAX), Some(0));
    }

    #[test]
    fn entries_params_trim_query_and_clamp_limit() {
        let params = entries_params(Some("auto"), None, "  1.2.3  ", 100, 10_000);
        assert_eq!(params.layer.as_deref(), Some("auto"));
        assert_eq!(params.rule, None);
        assert_eq!(params.query.as_deref(), Some("1.2.3"));
        assert_eq!(params.offset, 100);
        assert_eq!(params.limit, BT_GUARD_LIST_MAX_LIMIT);
        assert_eq!(entries_params(None, None, "   ", 0, 0).query, None);
        assert_eq!(entries_params(None, None, "", 0, 0).limit, 1);
    }

    #[test]
    fn events_params_carry_filters() {
        let params = events_params(Some("enforced"), Some("rewind"), 0, 50);
        assert_eq!(params.kind.as_deref(), Some("enforced"));
        assert_eq!(params.rule.as_deref(), Some("rewind"));
        assert_eq!(params.limit, 50);
    }

    #[test]
    fn reload_keeps_loaded_pages_within_protocol_limits() {
        assert_eq!(reload_limit(0), BT_GUARD_LIST_DEFAULT_LIMIT);
        assert_eq!(reload_limit(250), 250);
        assert_eq!(reload_limit(100_000), BT_GUARD_LIST_MAX_LIMIT);
    }

    #[test]
    fn removable_ids_skip_cloud_entries_and_unselected() {
        let items = vec![
            entry(1, "manual"),
            entry(2, "cloud"),
            entry(3, "file"),
            entry(4, "auto"),
        ];
        let selected: HashSet<i64> = [1, 2, 3, 99].into_iter().collect();
        assert_eq!(removable_ids(&items, &selected), (vec![1, 3], 1));
        assert_eq!(removable_ids(&items, &HashSet::new()), (vec![], 0));
    }

    #[test]
    fn removable_ids_never_exceed_the_wire_limit() {
        let items: Vec<_> = (0..(BT_GUARD_REMOVE_MAX_IDS as i64 + 20))
            .map(|id| entry(id, "auto"))
            .collect();
        let selected: HashSet<i64> = items.iter().map(|item| item.id).collect();
        assert_eq!(
            removable_ids(&items, &selected).0.len(),
            BT_GUARD_REMOVE_MAX_IDS
        );
    }

    #[test]
    fn selection_drops_entries_that_left_the_list() {
        let mut selected: HashSet<i64> = [1, 2, 3].into_iter().collect();
        retain_selected(&mut selected, &[entry(2, "auto"), entry(3, "auto")]);
        assert_eq!(selected, [2, 3].into_iter().collect());
    }

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(64 * 1024 * 1024), "64.0 MiB");
        assert_eq!(format_bytes(u64::MAX), "16777216.0 TiB");
    }

    #[test]
    fn evidence_known_fields_come_first_in_fixed_order() {
        let value = json!({
            "rule": "excessive",
            "strikes": 2,
            "detail": "ratio 3.7x",
            "deliveredBytes": 3_900_000_000u64,
            "torrentSize": 1_000_000_000u64,
            "ratioMilli": 3_700,
            "dupBytes": 0,
            "rerequests": 0,
            "rewindPermille": 0,
        });
        let fields = evidence_fields(&value);
        let keys: Vec<_> = fields.iter().map(|f| f.raw_key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "torrentSize",
                "deliveredBytes",
                "ratioMilli",
                "strikes",
                "detail"
            ]
        );
        assert_eq!(fields[2].value.text(), "370%");
        assert_eq!(fields[0].label_key, Some("btGuardEvTorrentSize"));
    }

    #[test]
    fn evidence_formats_rewind_permille_and_unknown_keys() {
        let value = json!({"rewindPermille": 125, "sequence": 9, "added": 3, "note": null});
        let fields = evidence_fields(&value);
        assert_eq!(fields[0].value.text(), "12.5%");
        let rest: Vec<_> = fields[1..]
            .iter()
            .map(|f| (f.raw_key.as_str(), f.label_key, f.value.text()))
            .collect();
        assert_eq!(
            rest,
            vec![
                ("added", None, "3".to_owned()),
                ("sequence", None, "9".to_owned())
            ]
        );
    }

    #[test]
    fn evidence_of_non_objects_is_empty() {
        for value in [json!(null), json!("x"), json!([1, 2]), json!(3)] {
            assert!(evidence_fields(&value).is_empty());
        }
    }

    #[test]
    fn event_detail_accepts_json_and_plain_text() {
        let json_fields = event_detail_fields(r#"{"sequence":12,"entries":40}"#);
        assert_eq!(json_fields.len(), 2);
        assert_eq!(json_fields[0].raw_key, "entries");
        assert_eq!(json_fields[1].raw_key, "sequence");

        let plain = event_detail_fields("  connection refused  ");
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].value.text(), "connection refused");
        assert_eq!(plain[0].label_key, Some("btGuardEvDetail"));

        assert!(event_detail_fields("   ").is_empty());
        // 合法 JSON 但不是对象：当作纯文本保留原样。
        assert_eq!(event_detail_fields("[1,2]").len(), 1);
    }

    #[test]
    fn attribution_url_finds_the_link_in_a_credit_line() {
        let line = "BTN-Collected-Rules by PBH-BTN, CC BY 4.0, https://github.com/PBH-BTN/BTN-Collected-Rules";
        assert_eq!(
            attribution_url(line),
            Some("https://github.com/PBH-BTN/BTN-Collected-Rules")
        );
        assert_eq!(
            attribution_url("(https://example.org/x)"),
            Some("https://example.org/x")
        );
        assert_eq!(attribution_url("no link here"), None);
        assert_eq!(attribution_url("javascript:alert(1)"), None);
    }
}

#[cfg(test)]
mod pick_tests {
    use fluxdown_protocol::BT_GUARD_LAYERS;

    use super::{RULE_FILTERS, pick_static};

    #[test]
    fn dropdown_values_map_back_to_protocol_constants() {
        assert_eq!(pick_static(BT_GUARD_LAYERS, "cloud"), Some("cloud"));
        assert_eq!(pick_static(RULE_FILTERS, "rewind"), Some("rewind"));
        assert_eq!(pick_static(RULE_FILTERS, "manual"), Some("manual"));
        assert_eq!(pick_static(RULE_FILTERS, "file"), Some("file"));
        assert_eq!(pick_static(BT_GUARD_LAYERS, ""), None);
        assert_eq!(pick_static(BT_GUARD_LAYERS, "Cloud"), None);
        assert_eq!(pick_static(RULE_FILTERS, "nope"), None);
    }
}
