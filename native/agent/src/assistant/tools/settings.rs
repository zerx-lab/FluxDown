//! `fd_update_settings`：改 FluxDown 设置。
//!
//! 双重判定：先过显式拒绝表（凭据、访问密钥、隐私 / 遥测 / 数据共享、云端推送、自动更新、
//! 引擎学习键、站点凭据、Webhook、可执行路径与下载源、插件开发模式……），再要求键必须出现在规范目录里：
//! - daemon 键（`native/protocol/src/daemon_config.rs` 的 `DAEMON_CONFIG_FIELDS`，可写项）经
//!   `daemon.config.patch` 写入（带 `expected_revision`，先 `daemon.config.get`）；
//! - 客户端偏好键（`native/protocol/src/settings.rs` 的 `SYNC_SETTING_SPECS` 中归 agent / 偏好所有者的键）
//!   经 `agent.preferences.patch` 写入。
//!
//! 整个调用先全部校验，任何键未知 / 只读 / 非法 / 被拒都不改动任何设置。值只接受字符串、数字、布尔，
//! 且必须符合目录类型。返回每个键的改前 / 改后值。

use std::collections::BTreeMap;

use fluxdown_protocol::{
    DaemonConfigField, DaemonConfigKind, DaemonConfigPatch, SettingOwner, SettingSpec,
    daemon_config_default, daemon_config_field, method, normalize_daemon_config_value,
    setting_spec, validate_value,
};
use serde::Deserialize;
use serde_json::{Map, Number, Value, json};

use super::tasks::{
    ToolRpc, check_abs_dir, check_max, encode, invalid, is_conflict, normalize_proxy_url,
};
use super::{ToolContext, ToolError, parse_args};

/// 单次最多改多少个键（云端 Schema `values.maxProperties`）。
const MAX_KEYS: usize = 30;
/// 文本型 daemon 键的长度上限（字符）。
const MAX_TEXT_CHARS: usize = 8192;
/// 字符串型偏好值的长度上限（字符）。
const MAX_PREF_STRING_CHARS: usize = 256;
/// 错误信息里回显键名的长度上限（字符）：键名来自模型，不能原样无限回显。
const MAX_KEY_ECHO_CHARS: usize = 64;
/// daemon 配置修订冲突时的最多尝试次数。
const MAX_ATTEMPTS: usize = 3;

const REASON_CREDENTIAL: &str =
    "credentials, account names and secrets cannot be changed by the assistant";
const REASON_PRIVACY: &str = "privacy, telemetry, crash / evidence reporting and data-sharing choices must be made by the user in the app";
const REASON_UPDATE: &str =
    "update and automatic-update settings cannot be changed by the assistant";
const REASON_ACCOUNT: &str =
    "account, device, cloud and remote-access state cannot be changed by the assistant";
const REASON_PUSH: &str = "cloud push and notification channels cannot be changed by the assistant";
const REASON_LEARNED: &str = "engine-learned and telemetry state is maintained by the app";
const REASON_WEBHOOK: &str =
    "webhook endpoints may carry secrets and cannot be changed by the assistant";
const REASON_SITE_AUTH: &str = "site credentials cannot be changed by the assistant";
const REASON_PLUGIN: &str = "plugin settings and developer mode cannot be changed by the assistant";
const REASON_EXECUTABLE: &str =
    "paths of executables the app runs cannot be changed by the assistant";
const REASON_SOURCE: &str =
    "remote list / mirror sources the app downloads from cannot be changed by the assistant";
const REASON_RULES: &str =
    "JSON rule documents are edited in the app; the assistant cannot change them";
const REASON_QUEUE: &str = "the default queue is managed through the queue settings in the app";
const REASON_COLLECTION: &str =
    "this is a structured collection, not a single value; edit it in the app";
const REASON_DESTRUCTIVE: &str =
    "automatically deleting downloaded files is not allowed for the assistant";

/// 逐键精确拒绝（按规范键名与原键名都比对）。
const DENIED_KEYS: &[(&str, &str)] = &[
    ("proxy_username", REASON_CREDENTIAL),
    ("proxy_password", REASON_CREDENTIAL),
    ("webhook.endpoints", REASON_WEBHOOK),
    ("component.ffmpeg.path", REASON_EXECUTABLE),
    ("component.ytdlp.path", REASON_EXECUTABLE),
    ("component_mirror_base", REASON_SOURCE),
    ("bt_tracker_sub_urls", REASON_SOURCE),
    ("ed2k_server_sub_urls", REASON_SOURCE),
    ("ed2k_nodes_dat_url", REASON_SOURCE),
    ("bt_guard_filter_urls", REASON_SOURCE),
    ("proxy_rules", REASON_RULES),
    ("bt_guard_client_rules", REASON_RULES),
    ("default_queue_id", REASON_QUEUE),
    ("bt_guard_report_enabled", REASON_PRIVACY),
    ("cdn_node_health", REASON_LEARNED),
    ("auto_route_health", REASON_LEARNED),
    ("cdn_pending_reports", REASON_LEARNED),
    ("domain_conn_caps", REASON_LEARNED),
    ("custom_categories", REASON_COLLECTION),
    ("appearance.custom_themes", REASON_COLLECTION),
];

/// 键名含这些片段即拒绝（小写比对；先抹掉 `updated`，免得 `*_updated_at` 误中 `update`）。
const DENIED_FRAGMENTS: &[(&str, &str)] = &[
    ("password", REASON_CREDENTIAL),
    ("passwd", REASON_CREDENTIAL),
    ("secret", REASON_CREDENTIAL),
    ("token", REASON_CREDENTIAL),
    ("credential", REASON_CREDENTIAL),
    ("cookie", REASON_CREDENTIAL),
    ("access_key", REASON_CREDENTIAL),
    ("api_key", REASON_CREDENTIAL),
    ("private_key", REASON_CREDENTIAL),
    ("username", REASON_CREDENTIAL),
    ("site_auth", REASON_SITE_AUTH),
    ("siteauth", REASON_SITE_AUTH),
    ("auth", REASON_CREDENTIAL),
    ("analytics", REASON_PRIVACY),
    ("telemetry", REASON_PRIVACY),
    ("crash", REASON_PRIVACY),
    ("report", REASON_PRIVACY),
    ("consent", REASON_PRIVACY),
    ("update", REASON_UPDATE),
    ("push", REASON_PUSH),
    ("remote", REASON_ACCOUNT),
    ("webhook", REASON_WEBHOOK),
    ("plugin", REASON_PLUGIN),
    ("dev_mode", REASON_PLUGIN),
    ("developer", REASON_PLUGIN),
];

/// 键名以这些前缀开头即拒绝（小写比对）。
const DENIED_PREFIXES: &[(&str, &str)] = &[
    ("cloud", REASON_ACCOUNT),
    ("agent", REASON_ACCOUNT),
    ("link", REASON_ACCOUNT),
    ("account", REASON_ACCOUNT),
    ("profile", REASON_ACCOUNT),
    ("device", REASON_ACCOUNT),
    ("sync", REASON_ACCOUNT),
];

/// 该键是否永不可由助手写入；`Some(原因)` 命中拒绝表。读取设置的工具也用它遮蔽这些键。
pub(super) fn denied_setting_reason(key: &str) -> Option<&'static str> {
    let canonical = setting_spec(key).map_or(key, |spec| spec.storage_key);
    for candidate in [canonical, key] {
        let lowered = candidate.to_ascii_lowercase();
        if let Some((_, reason)) = DENIED_KEYS.iter().find(|(name, _)| *name == lowered) {
            return Some(*reason);
        }
        if let Some((_, reason)) = DENIED_PREFIXES
            .iter()
            .find(|(prefix, _)| lowered.starts_with(prefix))
        {
            return Some(*reason);
        }
        let scrubbed = lowered.replace("updated", "");
        if let Some((_, reason)) = DENIED_FRAGMENTS
            .iter()
            .find(|(fragment, _)| scrubbed.contains(fragment))
        {
            return Some(*reason);
        }
    }
    None
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    values: BTreeMap<String, Value>,
}

/// 单个键的校验失败。
enum KeyProblem {
    /// 命中拒绝策略：整个调用以 `Refused` 返回。
    Denied(&'static str),
    /// 未知 / 只读 / 值非法：整个调用以 `InvalidArguments` 返回。
    Invalid(String),
}

impl KeyProblem {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

/// 一个 daemon 键的写入计划。
struct DaemonPlan {
    kind: DaemonConfigKind,
    /// 规范化后的 wire 字符串。
    wire: String,
    /// 与 `fd_get_settings` 一致的带类型值。
    after: Value,
}

enum Planned {
    Daemon(DaemonPlan),
    Preference(Value),
}

/// 一个键的改前 / 改后。
struct Change {
    key: String,
    before: Value,
    after: Value,
    changed: bool,
}

pub(super) async fn update_settings(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    update_settings_with(ctx, args).await
}

async fn update_settings_with(rpc: &impl ToolRpc, args: Value) -> Result<Value, ToolError> {
    let args: UpdateArgs = parse_args(args)?;
    if args.values.is_empty() || args.values.len() > MAX_KEYS {
        return Err(invalid(format!(
            "values must contain 1..={MAX_KEYS} settings"
        )));
    }

    let mut denied = Vec::new();
    let mut problems = Vec::new();
    let mut daemon = BTreeMap::new();
    let mut preferences = BTreeMap::new();
    for (key, value) in &args.values {
        match plan_key(key, value) {
            Ok(Planned::Daemon(plan)) => {
                daemon.insert(key.clone(), plan);
            }
            Ok(Planned::Preference(value)) => {
                preferences.insert(key.clone(), value);
            }
            Err(KeyProblem::Denied(reason)) => denied.push(format!("{}: {reason}", echo_key(key))),
            Err(KeyProblem::Invalid(message)) => {
                problems.push(format!("{}: {message}", echo_key(key)));
            }
        }
    }
    if !denied.is_empty() {
        return Err(ToolError::Refused(format!(
            "nothing was changed; the assistant may not change: {}",
            denied.join("; ")
        )));
    }
    if !problems.is_empty() {
        return Err(invalid(format!(
            "nothing was changed; {}",
            problems.join("; ")
        )));
    }

    let mut changes = Vec::with_capacity(args.values.len());
    if !daemon.is_empty() {
        changes.extend(apply_daemon(rpc, &daemon).await?);
    }
    if !preferences.is_empty() {
        match apply_preferences(rpc, &preferences).await {
            Ok(applied) => changes.extend(applied),
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            Err(error) if changes.iter().any(|change| change.changed) => {
                let applied = changes
                    .iter()
                    .filter(|change| change.changed)
                    .map(|change| change.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ToolError::Rpc(format!(
                    "{error}; these settings were already applied: {applied}"
                )));
            }
            Err(error) => return Err(error),
        }
    }

    changes.sort_by(|left, right| left.key.cmp(&right.key));
    let changed = changes.iter().filter(|change| change.changed).count();
    let settings = changes
        .into_iter()
        .map(|change| {
            json!({
                "key": change.key,
                "before": change.before,
                "after": change.after,
                "changed": change.changed,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "changed": changed,
        "unchanged": settings.len() - changed,
        "settings": settings,
    }))
}

fn echo_key(key: &str) -> String {
    if key.chars().count() > MAX_KEY_ECHO_CHARS {
        let head = key.chars().take(MAX_KEY_ECHO_CHARS).collect::<String>();
        format!("{head}…")
    } else {
        key.to_owned()
    }
}

/// 键 → 校验后的写入计划。
fn plan_key(key: &str, value: &Value) -> Result<Planned, KeyProblem> {
    if let Some(reason) = denied_setting_reason(key) {
        return Err(KeyProblem::Denied(reason));
    }
    if let Some(field) = daemon_config_field(key) {
        return plan_daemon(field, value).map(Planned::Daemon);
    }
    match setting_spec(key) {
        Some(spec) if spec.key == key => match spec.owner {
            SettingOwner::Daemon => Err(KeyProblem::invalid(format!(
                "use the download setting key '{}' instead",
                spec.storage_key
            ))),
            SettingOwner::Agent | SettingOwner::Preferences => {
                check_preference(spec, value).map_err(KeyProblem::Invalid)?;
                Ok(Planned::Preference(value.clone()))
            }
            SettingOwner::Excluded => Err(KeyProblem::invalid("this setting cannot be changed")),
        },
        _ => Err(KeyProblem::invalid(
            "unknown setting; use a key returned by fd_get_settings",
        )),
    }
}

// ───────────────────────── 客户端偏好 ─────────────────────────

fn check_preference(spec: &SettingSpec, value: &Value) -> Result<(), String> {
    match value {
        Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        _ => return Err("value must be a string, number or boolean".to_owned()),
    }
    validate_value(spec.key, value)?;
    if let Value::String(text) = value {
        if text.chars().count() > MAX_PREF_STRING_CHARS {
            return Err(format!(
                "value must not exceed {MAX_PREF_STRING_CHARS} characters"
            ));
        }
        if text.chars().any(char::is_control) {
            return Err("value must not contain control characters".to_owned());
        }
    }
    Ok(())
}

async fn apply_preferences(
    rpc: &impl ToolRpc,
    planned: &BTreeMap<String, Value>,
) -> Result<Vec<Change>, ToolError> {
    let current = rpc.preferences().await?;
    let mut patch = Map::new();
    let mut changes = Vec::with_capacity(planned.len());
    for (key, after) in planned {
        // 未设置的偏好以 null 表示（应用默认值生效）。
        let before = current.values.get(key).cloned().unwrap_or(Value::Null);
        let changed = before != *after;
        if changed {
            patch.insert(key.clone(), after.clone());
        }
        changes.push(Change {
            key: key.clone(),
            before,
            after: after.clone(),
            changed,
        });
    }
    if !patch.is_empty() {
        rpc.call(method::AGENT_PREFERENCES_PATCH, json!({ "values": patch }))
            .await?;
    }
    Ok(changes)
}

// ───────────────────────── daemon 配置 ─────────────────────────

fn plan_daemon(field: &DaemonConfigField, value: &Value) -> Result<DaemonPlan, KeyProblem> {
    let key = field.key;
    let wire = match (field.kind, value) {
        (DaemonConfigKind::ReadOnly, _) => {
            return Err(KeyProblem::invalid("read-only; maintained by the app"));
        }
        (DaemonConfigKind::Bool, Value::Bool(flag)) => flag.to_string(),
        (DaemonConfigKind::Bool, _) => return Err(KeyProblem::invalid("must be a boolean")),
        (DaemonConfigKind::Integer { .. }, Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| KeyProblem::invalid("must be an integer"))?
            .to_string(),
        (DaemonConfigKind::Integer { .. }, _) => {
            return Err(KeyProblem::invalid("must be an integer"));
        }
        (DaemonConfigKind::Float { .. }, Value::Number(number)) => number.to_string(),
        (DaemonConfigKind::Float { .. }, _) => return Err(KeyProblem::invalid("must be a number")),
        (DaemonConfigKind::Enum(_) | DaemonConfigKind::Text, Value::String(text)) => text.clone(),
        // 端口号是数字，却以文本存储。
        (DaemonConfigKind::Text, Value::Number(number)) if key == "proxy_port" => number
            .as_u64()
            .ok_or_else(|| KeyProblem::invalid("must be a port number"))?
            .to_string(),
        (DaemonConfigKind::Enum(_) | DaemonConfigKind::Text, _) => {
            return Err(KeyProblem::invalid("must be a string"));
        }
    };
    let normalized = normalize_daemon_config_value(key, &wire)
        .map_err(|error| KeyProblem::invalid(error.to_string()))?;
    let wire = check_daemon_value(key, normalized)?;
    Ok(DaemonPlan {
        kind: field.kind,
        after: wire_to_json(field.kind, &wire),
        wire,
    })
}

/// 目录校验之外的值级策略；返回最终要写入的 wire 值。
fn check_daemon_value(key: &str, value: String) -> Result<String, KeyProblem> {
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(KeyProblem::invalid(format!(
            "value must not exceed {MAX_TEXT_CHARS} characters"
        )));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(KeyProblem::invalid(
            "value must not contain control characters",
        ));
    }
    match key {
        "bt_seed_then_action" if value == "delete_files" => {
            Err(KeyProblem::Denied(REASON_DESTRUCTIVE))
        }
        "default_save_dir" if !value.is_empty() => {
            check_abs_dir(key, &value).map_err(tool_message)?;
            Ok(value)
        }
        "global_user_agent" => {
            check_max(key, &value, 512).map_err(tool_message)?;
            Ok(value)
        }
        "proxy_host" => {
            let bad = value
                .chars()
                .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '\\' | '@' | '?' | '#'));
            if bad || value.chars().count() > 253 {
                return Err(KeyProblem::invalid(
                    "must be a bare host name or IP address without scheme, credentials or path",
                ));
            }
            Ok(value)
        }
        "proxy_port" => {
            let ok = value.is_empty()
                || (value.bytes().all(|byte| byte.is_ascii_digit())
                    && value.parse::<u16>().is_ok_and(|port| port != 0));
            if ok {
                Ok(value)
            } else {
                Err(KeyProblem::invalid(
                    "must be empty or a port number 1-65535",
                ))
            }
        }
        key if key.starts_with("proxy_protocol_") => {
            if value.is_empty() || value == "direct://" {
                return Ok(value);
            }
            normalize_proxy_url(&value).ok_or_else(|| {
                KeyProblem::invalid(
                    "must be '' (keep), 'direct://' or a proxy URL like 'http://host:port' or \
                     'socks5://host:port' without credentials",
                )
            })
        }
        _ => Ok(value),
    }
}

fn tool_message(error: ToolError) -> KeyProblem {
    match error {
        ToolError::InvalidArguments(message)
        | ToolError::Rpc(message)
        | ToolError::Refused(message) => KeyProblem::Invalid(message),
        ToolError::Cancelled => KeyProblem::invalid("cancelled"),
    }
}

/// daemon wire 字符串 → 与 `fd_get_settings` 一致的带类型值。
pub(super) fn wire_to_json(kind: DaemonConfigKind, wire: &str) -> Value {
    match kind {
        DaemonConfigKind::Bool => match wire {
            "true" | "1" => Value::Bool(true),
            "false" | "0" => Value::Bool(false),
            other => Value::String(other.to_owned()),
        },
        DaemonConfigKind::Integer { .. } => wire
            .parse::<i64>()
            .map_or_else(|_| Value::String(wire.to_owned()), Value::from),
        DaemonConfigKind::Float { .. } => wire
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map_or_else(|| Value::String(wire.to_owned()), Value::Number),
        DaemonConfigKind::Enum(_) | DaemonConfigKind::Text | DaemonConfigKind::ReadOnly => {
            Value::String(wire.to_owned())
        }
    }
}

/// 跨键约束：只对本次改到的键做检查（其余键沿用快照里的现值）。
fn check_cross_fields(
    snapshot: &BTreeMap<String, String>,
    planned: &BTreeMap<String, DaemonPlan>,
) -> Result<(), ToolError> {
    if !planned.contains_key("bt_port_start") && !planned.contains_key("bt_port_end") {
        return Ok(());
    }
    let effective = |key: &str| {
        planned
            .get(key)
            .map(|plan| plan.wire.as_str())
            .or_else(|| snapshot.get(key).map(String::as_str))
            .unwrap_or_else(|| daemon_config_default(key))
            .parse::<i64>()
            .unwrap_or(0)
    };
    if effective("bt_port_start") > effective("bt_port_end") {
        return Err(invalid("bt_port_start must not exceed bt_port_end"));
    }
    Ok(())
}

async fn apply_daemon(
    rpc: &impl ToolRpc,
    planned: &BTreeMap<String, DaemonPlan>,
) -> Result<Vec<Change>, ToolError> {
    for attempt in 1..=MAX_ATTEMPTS {
        let snapshot = rpc.daemon_config().await?;
        check_cross_fields(&snapshot.values, planned)?;
        let mut patch = BTreeMap::new();
        let mut changes = Vec::with_capacity(planned.len());
        for (key, plan) in planned {
            let before_wire = snapshot
                .values
                .get(key)
                .map_or_else(|| daemon_config_default(key), String::as_str);
            let before = wire_to_json(plan.kind, before_wire);
            let changed = before != plan.after;
            if changed {
                patch.insert(key.clone(), plan.wire.clone());
            }
            changes.push(Change {
                key: key.clone(),
                before,
                after: plan.after.clone(),
                changed,
            });
        }
        if patch.is_empty() {
            return Ok(changes);
        }
        let params = encode(
            method::DAEMON_CONFIG_PATCH,
            &DaemonConfigPatch {
                expected_revision: snapshot.revision,
                values: patch,
            },
        )?;
        match rpc.call(method::DAEMON_CONFIG_PATCH, params).await {
            Ok(_) => return Ok(changes),
            Err(ToolError::Rpc(message)) if attempt < MAX_ATTEMPTS && is_conflict(&message) => {}
            Err(error) => return Err(error),
        }
    }
    Err(ToolError::Rpc(
        "the settings kept changing while they were being updated; try again".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fluxdown_protocol::{AgentPreferencesDto, DaemonConfigSnapshot, method};
    use serde_json::{Value, json};

    use super::super::ToolError;
    use super::super::tasks::test_support::MockRpc;
    use super::{denied_setting_reason, update_settings_with};

    fn ok_rpc() -> MockRpc {
        MockRpc::new(|_, _| Ok(json!({ "ok": true })))
    }

    fn with_config(rpc: &mut MockRpc, revision: u64, values: &[(&str, &str)]) {
        rpc.config = DaemonConfigSnapshot {
            revision,
            values: values
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        };
    }

    fn call(rpc: &MockRpc, values: Value) -> Result<Value, ToolError> {
        block_on(update_settings_with(rpc, json!({ "values": values })))
    }

    /// 单测用的最小阻塞执行器：替身 RPC 从不真正挂起。
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .map(|runtime| runtime.block_on(future))
            .unwrap_or_else(|error| panic!("cannot build test runtime: {error}"))
    }

    fn assert_no_writes(rpc: &MockRpc) {
        assert!(rpc.calls_of(method::DAEMON_CONFIG_PATCH).is_empty());
        assert!(rpc.calls_of(method::AGENT_PREFERENCES_PATCH).is_empty());
    }

    #[test]
    fn extra_top_level_field_is_rejected() {
        let rpc = ok_rpc();
        let result = block_on(update_settings_with(
            &rpc,
            json!({ "values": { "max_concurrent_tasks": 3 }, "force": true }),
        ));
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
        assert_no_writes(&rpc);
    }

    #[test]
    fn empty_and_oversized_values_are_rejected() {
        let rpc = ok_rpc();
        let result = call(&rpc, json!({}));
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
        let many = (0..31)
            .map(|index| (format!("key_{index}"), json!(1)))
            .collect::<serde_json::Map<_, _>>();
        let result = call(&rpc, Value::Object(many));
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
    }

    #[test]
    fn denied_keys_are_refused_without_writes() {
        for key in [
            "analytics_enabled",
            "general.analytics_enabled",
            "proxy_password",
            "proxy_username",
            "cdn_node_health",
            "auto_route_health",
            "cdn_pending_reports",
            "domain_conn_caps",
            "webhook.endpoints",
            "bt_guard_report_enabled",
            "general.silent_auto_update",
            "general.update_channel",
            "general.auto_check_update",
            "component.ffmpeg.path",
            "component_mirror_base",
            "proxy_rules",
            "plugin.dev_mode",
            "agent.cloud_notify.telegram",
            "site_auth.example.com",
            "remote_access_key",
            "appearance.custom_themes.my_theme",
            "custom_categories",
        ] {
            assert!(denied_setting_reason(key).is_some(), "{key} must be denied");
            let rpc = ok_rpc();
            let result = call(&rpc, json!({ key: "x", "max_concurrent_tasks": 3 }));
            assert!(
                matches!(result, Err(ToolError::Refused(_))),
                "{key}: {result:?}"
            );
            assert!(
                rpc.calls
                    .lock()
                    .map(|calls| calls.is_empty())
                    .unwrap_or(false)
            );
        }
    }

    #[test]
    fn allowed_keys_are_not_denied() {
        for key in [
            "max_concurrent_tasks",
            "default_save_dir",
            "proxy_mode",
            "proxy_host",
            "proxy_port",
            "bt_tracker_sub_updated_at",
            "bt_guard_cloud_enabled",
            "appearance.theme_mode",
            "download.notify_on_complete",
            "download.silent_download",
            "general.clipboard_watch",
        ] {
            assert_eq!(denied_setting_reason(key), None, "{key}");
        }
    }

    #[test]
    fn unknown_read_only_and_dotted_daemon_keys_are_invalid() {
        for (key, needle) in [
            ("no_such_setting", "unknown"),
            ("bt_tracker_sub_cache", "read-only"),
            ("download.max_concurrent_tasks", "max_concurrent_tasks"),
        ] {
            let rpc = ok_rpc();
            let result = call(&rpc, json!({ key: 3 }));
            match result {
                Err(ToolError::InvalidArguments(message)) => {
                    assert!(message.contains(needle), "{key}: {message}");
                }
                other => panic!("{key}: {other:?}"),
            }
            assert_no_writes(&rpc);
        }
    }

    #[test]
    fn type_and_range_mismatches_are_rejected() {
        for values in [
            json!({ "max_concurrent_tasks": "many" }),
            json!({ "max_concurrent_tasks": true }),
            json!({ "max_concurrent_tasks": 2.5 }),
            json!({ "max_concurrent_tasks": 0 }),
            json!({ "max_concurrent_tasks": 5000 }),
            json!({ "bt_enabled": 1 }),
            json!({ "bt_enabled": "true" }),
            json!({ "proxy_mode": "teleport" }),
            json!({ "proxy_mode": 3 }),
            json!({ "bt_seed_ratio_limit": "high" }),
            json!({ "bt_seed_ratio_limit": -1 }),
            json!({ "max_concurrent_tasks": null }),
            json!({ "max_concurrent_tasks": [3] }),
            json!({ "appearance.theme_mode": "neon" }),
            json!({ "appearance.theme_mode": 1 }),
            json!({ "general.clipboard_watch": "yes" }),
            json!({ "appearance.custom_color": 1.5 }),
        ] {
            let rpc = ok_rpc();
            let result = call(&rpc, values.clone());
            assert!(
                matches!(result, Err(ToolError::InvalidArguments(_))),
                "{values}: {result:?}"
            );
            assert_no_writes(&rpc);
        }
    }

    #[test]
    fn one_invalid_key_blocks_the_valid_ones() {
        let rpc = ok_rpc();
        let result = call(
            &rpc,
            json!({ "max_concurrent_tasks": 8, "bt_enabled": "nope" }),
        );
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
        assert_no_writes(&rpc);
    }

    #[test]
    fn destructive_and_credential_like_values_are_rejected() {
        for values in [
            json!({ "bt_seed_then_action": "delete_files" }),
            json!({ "proxy_protocol_http": "socks5://user:pass@127.0.0.1:1080" }),
            json!({ "proxy_host": "user:pass@127.0.0.1" }),
            json!({ "proxy_host": "http://127.0.0.1" }),
            json!({ "proxy_port": 0 }),
            json!({ "proxy_port": "80a" }),
            json!({ "default_save_dir": "relative/dir" }),
            json!({ "bt_port_start": 7000, "bt_port_end": 6000 }),
        ] {
            let rpc = ok_rpc();
            let result = call(&rpc, values.clone());
            assert!(result.is_err(), "{values}: {result:?}");
            assert_no_writes(&rpc);
        }
        let rpc = ok_rpc();
        let result = call(&rpc, json!({ "bt_seed_then_action": "delete_files" }));
        assert!(matches!(result, Err(ToolError::Refused(_))), "{result:?}");
    }

    #[test]
    fn daemon_setting_is_patched_with_expected_revision_and_reports_before_after() {
        let mut rpc = ok_rpc();
        with_config(
            &mut rpc,
            7,
            &[("max_concurrent_tasks", "3"), ("bt_enabled", "true")],
        );
        let result = call(
            &rpc,
            json!({
                "max_concurrent_tasks": 8,
                "bt_enabled": true,
                "proxy_mode": "manual",
                "proxy_port": 7890,
                "bt_seed_ratio_limit": 1.5,
            }),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let patches = rpc.calls_of(method::DAEMON_CONFIG_PATCH);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0]["expectedRevision"], json!(7));
        assert_eq!(
            patches[0]["values"],
            json!({
                "max_concurrent_tasks": "8",
                "proxy_mode": "manual",
                "proxy_port": "7890",
                "bt_seed_ratio_limit": "1.5",
            }),
            "unchanged keys must not be patched"
        );
        assert!(rpc.calls_of(method::AGENT_PREFERENCES_PATCH).is_empty());

        assert_eq!(result["changed"], json!(4));
        assert_eq!(result["unchanged"], json!(1));
        let settings = result["settings"].as_array().cloned().unwrap_or_default();
        let find = |key: &str| settings.iter().find(|item| item["key"] == key).cloned();
        let tasks = find("max_concurrent_tasks").unwrap_or_default();
        assert_eq!(tasks["before"], json!(3));
        assert_eq!(tasks["after"], json!(8));
        assert_eq!(tasks["changed"], json!(true));
        let bt = find("bt_enabled").unwrap_or_default();
        assert_eq!(bt["changed"], json!(false));
        assert_eq!(bt["before"], json!(true));
        let mode = find("proxy_mode").unwrap_or_default();
        assert_eq!(mode["before"], json!("none"));
        assert_eq!(mode["after"], json!("manual"));
        let ratio = find("bt_seed_ratio_limit").unwrap_or_default();
        assert_eq!(ratio["before"], json!(0.0));
        assert_eq!(ratio["after"], json!(1.5));
    }

    #[test]
    fn unchanged_daemon_values_make_no_patch() {
        let mut rpc = ok_rpc();
        with_config(&mut rpc, 2, &[("max_concurrent_tasks", "8")]);
        let result = call(&rpc, json!({ "max_concurrent_tasks": 8 }))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["changed"], json!(0));
        assert_no_writes(&rpc);
    }

    #[test]
    fn revision_conflict_rereads_and_retries() {
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&attempts);
        let mut rpc = MockRpc::new(move |name, _| {
            if name == method::DAEMON_CONFIG_PATCH && seen.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(ToolError::Rpc("config revision conflict".to_owned()));
            }
            Ok(json!({ "ok": true }))
        });
        with_config(&mut rpc, 1, &[]);
        let result = call(&rpc, json!({ "max_concurrent_tasks": 9 }));
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(rpc.calls_of(method::DAEMON_CONFIG_PATCH).len(), 2);
    }

    #[test]
    fn daemon_patch_failure_is_reported() {
        let mut rpc = MockRpc::new(|name, _| {
            if name == method::DAEMON_CONFIG_PATCH {
                Err(ToolError::Rpc("invalid-argument: bad value".to_owned()))
            } else {
                Ok(json!({}))
            }
        });
        with_config(&mut rpc, 1, &[]);
        let result = call(&rpc, json!({ "max_concurrent_tasks": 9 }));
        assert!(matches!(result, Err(ToolError::Rpc(_))), "{result:?}");
    }

    #[test]
    fn preference_is_patched_through_the_agent_and_reports_before_after() {
        let mut rpc = ok_rpc();
        rpc.prefs = AgentPreferencesDto {
            revision: 4,
            values: [
                ("appearance.theme_mode".to_owned(), json!("light")),
                ("download.notify_on_complete".to_owned(), json!(true)),
            ]
            .into_iter()
            .collect(),
        };
        let result = call(
            &rpc,
            json!({
                "appearance.theme_mode": "dark",
                "download.notify_on_complete": true,
                "general.clipboard_watch": false,
            }),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let patches = rpc.calls_of(method::AGENT_PREFERENCES_PATCH);
        assert_eq!(patches.len(), 1);
        assert_eq!(
            patches[0],
            json!({ "values": { "appearance.theme_mode": "dark", "general.clipboard_watch": false } })
        );
        assert!(rpc.calls_of(method::DAEMON_CONFIG_PATCH).is_empty());
        assert_eq!(result["changed"], json!(2));
        let settings = result["settings"].as_array().cloned().unwrap_or_default();
        let theme = settings
            .iter()
            .find(|item| item["key"] == "appearance.theme_mode")
            .cloned()
            .unwrap_or_default();
        assert_eq!(theme["before"], json!("light"));
        assert_eq!(theme["after"], json!("dark"));
        let clipboard = settings
            .iter()
            .find(|item| item["key"] == "general.clipboard_watch")
            .cloned()
            .unwrap_or_default();
        assert_eq!(clipboard["before"], Value::Null);
    }

    #[test]
    fn preference_failure_after_daemon_write_names_applied_settings() {
        let mut rpc = MockRpc::new(|name, _| {
            if name == method::AGENT_PREFERENCES_PATCH {
                Err(ToolError::Rpc("internal".to_owned()))
            } else {
                Ok(json!({}))
            }
        });
        with_config(&mut rpc, 1, &[]);
        let result = call(
            &rpc,
            json!({ "max_concurrent_tasks": 9, "appearance.theme_mode": "dark" }),
        );
        match result {
            Err(ToolError::Rpc(message)) => {
                assert!(message.contains("max_concurrent_tasks"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }
}
