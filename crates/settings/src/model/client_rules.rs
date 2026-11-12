//! BT 防吸血本机客户端名字规则（`bt_guard_client_rules`，JSON 数组）的纯模型。
//!
//! 解析与引擎 `parse_local_client_rules` 逐条一致：整体非法 → 空列表；单条字段非法只丢该条；
//! 动作取 `verdict ?? action`，生效场景取 `when ?? (seedingOnly ? seeding : always)`；
//! 最多 [`MAX_RULES`] 条。写回时只写当前 8 个字段。

use serde::{Deserialize, Serialize};

/// 配置键。
pub(crate) const CLIENT_RULES_KEY: &str = "bt_guard_client_rules";
/// 规则条数上限。
pub(crate) const MAX_RULES: usize = 200;
/// 匹配内容的最大字符数。
pub(crate) const MAX_VALUE_CHARS: usize = 64;
/// 备注的最大字符数。
pub(crate) const MAX_REASON_CHARS: usize = 200;
/// `ban` 的默认时长（3 天）与上限（1 年），单位分钟。
pub(crate) const DEFAULT_BAN_MINUTES: u32 = 3 * 24 * 60;
pub(crate) const MAX_BAN_MINUTES: u32 = 365 * 24 * 60;

pub(crate) const FIELDS: [&str; 2] = ["clientName", "peerIdPrefix"];
pub(crate) const OPS: [&str; 2] = ["contains", "prefix"];
pub(crate) const ACTIONS: [&str; 4] = ["ban", "choke", "throttle", "allow"];
pub(crate) const WHENS: [&str; 3] = ["always", "seeding", "downloading"];

/// 一条规则（已校验；枚举字段取值见上面的常量）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientRule {
    pub id: String,
    pub field: String,
    pub op: String,
    pub value: String,
    pub action: String,
    pub when: String,
    pub ban_minutes: u32,
    pub reason: String,
}

/// wire 形态（含旧字段 `seedingOnly` 与新字段 `verdict` / `when` / `banMinutes`）。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawRule {
    id: String,
    field: String,
    op: String,
    value: String,
    action: String,
    seeding_only: bool,
    reason: String,
    verdict: Option<String>,
    when: Option<String>,
    ban_minutes: Option<u32>,
}

fn known(list: &[&str], value: &str) -> bool {
    list.contains(&value)
}

impl ClientRule {
    fn compile(raw: RawRule) -> Option<Self> {
        if !known(&FIELDS, &raw.field) || !known(&OPS, &raw.op) {
            return None;
        }
        let action = raw.verdict.unwrap_or(raw.action);
        if !known(&ACTIONS, &action) {
            return None;
        }
        let when = match raw.when {
            Some(when) if known(&WHENS, &when) => when,
            Some(_) => return None,
            None if raw.seeding_only => "seeding".to_owned(),
            None => "always".to_owned(),
        };
        if raw.value.is_empty() || raw.id.is_empty() {
            return None;
        }
        Some(Self {
            id: raw.id,
            field: raw.field,
            op: raw.op,
            value: raw.value,
            action,
            when,
            ban_minutes: raw
                .ban_minutes
                .unwrap_or(DEFAULT_BAN_MINUTES)
                .clamp(1, MAX_BAN_MINUTES),
            reason: raw.reason,
        })
    }

    /// 新规则草稿（id 由 [`next_id`] 生成）。
    pub(crate) fn draft(id: String) -> Self {
        Self {
            id,
            field: FIELDS[0].to_owned(),
            op: OPS[0].to_owned(),
            value: String::new(),
            action: ACTIONS[0].to_owned(),
            when: WHENS[0].to_owned(),
            ban_minutes: DEFAULT_BAN_MINUTES,
            reason: String::new(),
        }
    }
}

/// 宽松解析：空串 / 非法 JSON / 非数组 → 空列表。
pub(crate) fn parse(raw: &str) -> Vec<ClientRule> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<RawRule>>(raw)
        .map(|list| {
            list.into_iter()
                .filter_map(ClientRule::compile)
                .take(MAX_RULES)
                .collect()
        })
        .unwrap_or_default()
}

/// 编码为配置值；空列表写空串。
pub(crate) fn encode(list: &[ClientRule]) -> Option<String> {
    if list.is_empty() {
        return Some(String::new());
    }
    serde_json::to_string(list).ok()
}

/// 下一个未被占用的 `local-<n>`（n 从 1 起，取现有最大值 + 1）。
pub(crate) fn next_id(list: &[ClientRule]) -> String {
    let max = list
        .iter()
        .filter_map(|rule| rule.id.strip_prefix("local-")?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("local-{}", max.saturating_add(1))
}

/// 表单校验错误（值为 i18n 键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftError {
    ValueRequired,
}

impl DraftError {
    pub(crate) fn message_key(self) -> &'static str {
        match self {
            Self::ValueRequired => "btGuardClientRuleValueRequired",
        }
    }
}

/// 按字符边界截取前 `max` 个字符。
pub(crate) fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => text.get(..end).unwrap_or(text),
        None => text,
    }
}

/// 规整表单输入：匹配内容去首尾空白后不能为空，超长按字符截断；备注同理截断；时长钳制。
pub(crate) fn normalize(mut rule: ClientRule) -> Result<ClientRule, DraftError> {
    let value = truncate_chars(rule.value.trim(), MAX_VALUE_CHARS).to_owned();
    if value.is_empty() {
        return Err(DraftError::ValueRequired);
    }
    rule.value = value;
    rule.reason = truncate_chars(rule.reason.trim(), MAX_REASON_CHARS).to_owned();
    rule.ban_minutes = rule.ban_minutes.clamp(1, MAX_BAN_MINUTES);
    Ok(rule)
}

/// 按 id 覆盖；不存在则追加（已满 [`MAX_RULES`] 时不追加，返回 `None`）。
pub(crate) fn upsert(mut list: Vec<ClientRule>, draft: ClientRule) -> Option<Vec<ClientRule>> {
    if let Some(slot) = list.iter_mut().find(|rule| rule.id == draft.id) {
        *slot = draft;
    } else if list.len() >= MAX_RULES {
        return None;
    } else {
        list.push(draft);
    }
    Some(list)
}

pub(crate) fn remove(mut list: Vec<ClientRule>, id: &str) -> Option<Vec<ClientRule>> {
    let before = list.len();
    list.retain(|rule| rule.id != id);
    (list.len() != before).then_some(list)
}

/// 上移（`up = true`）或下移一位；已在边界或 id 不存在返回 `None`。
pub(crate) fn shift(mut list: Vec<ClientRule>, id: &str, up: bool) -> Option<Vec<ClientRule>> {
    let index = list.iter().position(|rule| rule.id == id)?;
    let target = if up { index.checked_sub(1)? } else { index + 1 };
    if target >= list.len() {
        return None;
    }
    list.swap(index, target);
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn rule(id: &str, value: &str) -> ClientRule {
        ClientRule {
            value: value.to_owned(),
            ..ClientRule::draft(id.to_owned())
        }
    }

    #[test]
    fn client_rules_parse_is_lenient() {
        assert!(parse("").is_empty());
        assert!(parse("   ").is_empty());
        assert!(parse("{not json").is_empty());
        assert!(parse(r#"{"id":"a"}"#).is_empty());
        let list = parse(
            r#"[
                {"id":"a","field":"clientName","op":"contains","value":"Xunlei","action":"ban"},
                {"id":"b","field":"nope","op":"contains","value":"x","action":"ban"},
                {"id":"c","field":"clientName","op":"regex","value":"x","action":"ban"},
                {"id":"d","field":"clientName","op":"prefix","value":"x","action":"kill"},
                {"id":"e","field":"clientName","op":"prefix","value":"x","action":"ban","when":"never"},
                {"id":"","field":"clientName","op":"prefix","value":"x","action":"ban"},
                {"id":"g","field":"clientName","op":"prefix","value":"","action":"ban"}
            ]"#,
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "a");
        assert_eq!(list[0].when, "always");
        assert_eq!(list[0].ban_minutes, DEFAULT_BAN_MINUTES);
    }

    #[test]
    fn client_rules_legacy_fields_are_honoured() {
        let list = parse(
            r#"[
                {"id":"a","field":"peerIdPrefix","op":"prefix","value":"-XL","action":"throttle","seedingOnly":true},
                {"id":"b","field":"peerIdPrefix","op":"prefix","value":"-XL","action":"throttle","verdict":"ban","seedingOnly":true,"when":"downloading"},
                {"id":"c","field":"peerIdPrefix","op":"prefix","value":"-XL","action":"bogus","verdict":"allow"},
                {"id":"d","field":"peerIdPrefix","op":"prefix","value":"-XL","action":"ban","verdict":"bogus"}
            ]"#,
        );
        let summary: Vec<_> = list
            .iter()
            .map(|r| (r.id.as_str(), r.action.as_str(), r.when.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("a", "throttle", "seeding"),
                ("b", "ban", "downloading"),
                ("c", "allow", "always"),
            ]
        );
    }

    #[test]
    fn client_rules_ban_minutes_clamped_and_capped() {
        let list = parse(
            r#"[
                {"id":"a","field":"clientName","op":"contains","value":"x","action":"ban","banMinutes":0},
                {"id":"b","field":"clientName","op":"contains","value":"x","action":"ban","banMinutes":99999999}
            ]"#,
        );
        assert_eq!(list[0].ban_minutes, 1);
        assert_eq!(list[1].ban_minutes, MAX_BAN_MINUTES);

        let many: Vec<Value> = (0..250)
            .map(|n| json!({"id": format!("local-{n}"), "field":"clientName","op":"contains","value":"x","action":"ban"}))
            .collect();
        let parsed = parse(&Value::Array(many).to_string());
        assert_eq!(parsed.len(), MAX_RULES);
        assert!(upsert(parsed, rule("local-999", "y")).is_none());
    }

    #[test]
    fn client_rules_encode_writes_all_fields() -> Result<(), serde_json::Error> {
        let list = parse(
            r#"[{"id":"a","field":"clientName","op":"contains","value":"x","action":"choke","seedingOnly":true}]"#,
        );
        let encoded = encode(&list).unwrap_or_default();
        let value: Value = serde_json::from_str(&encoded)?;
        assert_eq!(
            value,
            json!([{"id":"a","field":"clientName","op":"contains","value":"x",
                "action":"choke","when":"seeding","banMinutes":DEFAULT_BAN_MINUTES,"reason":""}])
        );
        assert_eq!(encode(&[]).as_deref(), Some(""));
        assert_eq!(parse(&encoded), list);
        Ok(())
    }

    #[test]
    fn client_rules_ids_edits_and_order() {
        assert_eq!(next_id(&[]), "local-1");
        let list = vec![
            rule("local-3", "a"),
            rule("cloud-x", "b"),
            rule("local-1", "c"),
        ];
        assert_eq!(next_id(&list), "local-4");

        let edited = upsert(list.clone(), rule("cloud-x", "B")).unwrap_or_default();
        assert_eq!(edited[1].value, "B");
        assert_eq!(edited.len(), 3);

        let up = shift(list.clone(), "local-1", true).unwrap_or_default();
        assert_eq!(up[1].id, "local-1");
        assert!(shift(list.clone(), "local-3", true).is_none());
        assert!(shift(list.clone(), "local-1", false).is_none());
        assert!(remove(list.clone(), "missing").is_none());
        assert_eq!(remove(list, "cloud-x").map(|l| l.len()), Some(2));
    }

    #[test]
    fn client_rules_normalize_trims_and_truncates_on_char_boundary() {
        assert_eq!(
            normalize(rule("a", "   ")).err(),
            Some(DraftError::ValueRequired)
        );
        let long = "迅雷".repeat(40);
        let normalized =
            normalize(rule("a", &format!("  {long}  "))).unwrap_or_else(|_| rule("a", ""));
        assert_eq!(normalized.value.chars().count(), MAX_VALUE_CHARS);
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("é😀x", 2), "é😀");
    }
}
