//! 云同步设置的稳定 wire 键、所有权与 daemon 映射。

use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingOwner {
    Daemon,
    Agent,
    Preferences,
    Excluded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettingSpec {
    pub key: &'static str,
    pub owner: SettingOwner,
    pub storage_key: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingValueKind {
    Boolean,
    Integer,
    Float,
    String,
}

macro_rules! spec {
    ($key:literal, $owner:ident) => {
        SettingSpec {
            key: $key,
            owner: SettingOwner::$owner,
            storage_key: $key,
        }
    };
    ($key:literal, $owner:ident, $storage:literal) => {
        SettingSpec {
            key: $key,
            owner: SettingOwner::$owner,
            storage_key: $storage,
        }
    };
}

/// 自定义主题集合的同步范围键：进同步目录只为「在此设备同步的范围」分组与本机专属开关，
/// 本身从不承载值；每个主题是独立键 `appearance.custom_themes.<编码 id>`（见 [`custom_theme_key`]），
/// 值为主题文件原文。逐主题成键让导入 / 删除各自 LWW，并发编辑不会互相覆盖整张列表。
pub const CUSTOM_THEMES_KEY: &str = "appearance.custom_themes";
const CUSTOM_THEME_KEY_PREFIX: &str = "appearance.custom_themes.";
/// 自定义主题 id 的最大字节数（GPUI 主题库的 id 清洗规则同此上限）。
pub const MAX_CUSTOM_THEME_ID_LEN: usize = 64;
/// 单个同步值序列化后的字节上限，镜像 FluxCloud `sync::MAX_VALUE_BYTES`：
/// 超限条目会让整批推送被拒，因此在写入时就拦下。
pub const MAX_SYNC_VALUE_BYTES: usize = 64 * 1024;

/// 保留旧客户端云同步键，另含 GPUI 专属的 `ui.show_activity_*`、
/// `custom_categories`（自定义分类，推送时剥离各设备不同的 `saveDir`）、[`CUSTOM_THEMES_KEY`]
/// （自定义主题集合）与 [`FILE_ICON_PACK_KEY`]（GPUI / Web 文件图标包），共 59 个；
/// 另含 19 个 `bt.guard.*`（BT 防吸血；`bt_guard_report_enabled` 设备本地，不进目录）。
pub const SYNC_SETTING_SPECS: &[SettingSpec] = &[
    spec!("appearance.theme_mode", Preferences),
    spec!("appearance.dark_theme", Preferences),
    spec!("appearance.light_theme", Preferences),
    spec!("appearance.color_scheme", Preferences),
    spec!("appearance.custom_color", Preferences),
    spec!("appearance.custom_themes", Preferences),
    spec!("appearance.file_icon_pack", Preferences),
    spec!("general.locale", Preferences),
    spec!("general.update_channel", Preferences),
    spec!("general.auto_check_update", Preferences),
    spec!("general.clipboard_watch", Preferences),
    spec!("general.floating_ball_enabled", Preferences),
    spec!("general.floating_ball_active_only", Preferences),
    spec!("ui.show_sidebar_status", Preferences),
    spec!("ui.show_sidebar_queues", Preferences),
    spec!("ui.show_sidebar_category", Preferences),
    spec!("ui.show_sidebar_rss", Preferences),
    spec!("ui.show_activity_rss", Preferences),
    spec!("ui.show_activity_webhooks", Preferences),
    spec!("ui.show_activity_theme", Preferences),
    spec!("ui.show_activity_account", Preferences),
    spec!("ui.show_titlebar_pause_all", Preferences),
    spec!("ui.show_titlebar_resume_all", Preferences),
    spec!("ui.show_titlebar_settings", Preferences),
    spec!("ui.show_titlebar_theme", Preferences),
    spec!(
        "download.max_concurrent_tasks",
        Daemon,
        "max_concurrent_tasks"
    ),
    spec!("download.default_segments", Daemon, "default_segments"),
    spec!(
        "download.auto_max_connections",
        Daemon,
        "auto_max_connections"
    ),
    spec!("download.cdn_multi_enabled", Daemon, "cdn_multi_enabled"),
    spec!("download.cdn_max_nodes", Daemon, "cdn_max_nodes"),
    spec!("download.speed_limit_bytes", Daemon, "speed_limit_bytes"),
    spec!("download.max_auto_retries", Daemon, "max_auto_retries"),
    spec!(
        "download.auto_retry_delay_secs",
        Daemon,
        "auto_retry_delay_secs"
    ),
    spec!(
        "download.auto_resume_on_start",
        Daemon,
        "auto_resume_on_start"
    ),
    spec!("download.remember_last_save_dir", Preferences),
    spec!("download.use_server_time", Daemon, "use_server_time"),
    spec!("download.global_user_agent", Daemon, "global_user_agent"),
    spec!("download.notify_on_complete", Agent),
    spec!("download.silent_download", Agent),
    spec!("download.keep_awake", Agent),
    spec!("bt.enabled", Daemon, "bt_enabled"),
    spec!("bt.enable_dht", Daemon, "bt_enable_dht"),
    spec!("bt.enable_upnp", Daemon, "bt_enable_upnp"),
    spec!("bt.enable_utp", Daemon, "bt_enable_utp"),
    spec!("bt.custom_trackers", Daemon, "bt_custom_trackers"),
    spec!("bt.tracker_sub_enabled", Daemon, "bt_tracker_sub_enabled"),
    spec!("bt.tracker_sub_urls", Daemon, "bt_tracker_sub_urls"),
    spec!("bt.seed_ratio_limit", Daemon, "bt_seed_ratio_limit"),
    spec!(
        "bt.seed_post_ratio_limit",
        Daemon,
        "bt_seed_post_ratio_limit"
    ),
    spec!(
        "bt.seed_time_limit_minutes",
        Daemon,
        "bt_seed_time_limit_minutes"
    ),
    spec!(
        "bt.seed_inactive_time_limit_minutes",
        Daemon,
        "bt_seed_inactive_time_limit_minutes"
    ),
    spec!("bt.seed_limit_operator", Daemon, "bt_seed_limit_operator"),
    spec!("bt.seed_then_action", Daemon, "bt_seed_then_action"),
    spec!("bt.seed_max_active", Daemon, "bt_seed_max_active"),
    spec!("bt.guard.enabled", Daemon, "bt_guard_enabled"),
    spec!("bt.guard.rule_excessive", Daemon, "bt_guard_rule_excessive"),
    spec!("bt.guard.rule_rerequest", Daemon, "bt_guard_rule_rerequest"),
    spec!("bt.guard.rule_rewind", Daemon, "bt_guard_rule_rewind"),
    spec!("bt.guard.rule_lag", Daemon, "bt_guard_rule_lag"),
    spec!(
        "bt.guard.rule_fingerprint",
        Daemon,
        "bt_guard_rule_fingerprint"
    ),
    spec!("bt.guard.cloud_mode", Daemon, "bt_guard_cloud_mode"),
    spec!(
        "bt.guard.excess_ratio_pct",
        Daemon,
        "bt_guard_excess_ratio_pct"
    ),
    spec!(
        "bt.guard.min_torrent_mib",
        Daemon,
        "bt_guard_min_torrent_mib"
    ),
    spec!(
        "bt.guard.rewind_tolerance_pct",
        Daemon,
        "bt_guard_rewind_tolerance_pct"
    ),
    spec!("bt.guard.throttle_kib", Daemon, "bt_guard_throttle_kib"),
    spec!("bt.guard.ban_base_hours", Daemon, "bt_guard_ban_base_hours"),
    spec!("bt.guard.ban_max_days", Daemon, "bt_guard_ban_max_days"),
    spec!(
        "bt.guard.downloading_max_action",
        Daemon,
        "bt_guard_downloading_max_action"
    ),
    spec!("bt.guard.skip_private", Daemon, "bt_guard_skip_private"),
    spec!("bt.guard.skip_lan", Daemon, "bt_guard_skip_lan"),
    spec!("bt.guard.ipv6_prefix", Daemon, "bt_guard_ipv6_prefix"),
    spec!("bt.guard.filter_urls", Daemon, "bt_guard_filter_urls"),
    spec!("bt.guard.cloud_enabled", Daemon, "bt_guard_cloud_enabled"),
    spec!("ed2k.enable_kad", Daemon, "ed2k_enable_kad"),
    spec!("ed2k.enable_upnp", Daemon, "ed2k_enable_upnp"),
    spec!("ed2k.server_list", Daemon, "ed2k_server_list"),
    spec!("ed2k.server_sub_enabled", Daemon, "ed2k_server_sub_enabled"),
    spec!("ed2k.server_sub_urls", Daemon, "ed2k_server_sub_urls"),
    spec!("custom_categories", Preferences),
];

/// 键的同步目录条目；自定义主题键（`appearance.custom_themes.<id>`）归入 [`CUSTOM_THEMES_KEY`]。
#[must_use]
pub fn setting_spec(key: &str) -> Option<&'static SettingSpec> {
    let key = sync_scope_key(key);
    SYNC_SETTING_SPECS.iter().find(|spec| spec.key == key)
}

/// 键的同步范围键：自定义主题键归入 [`CUSTOM_THEMES_KEY`]，其余为自身。本机专属按范围键判定。
#[must_use]
pub fn sync_scope_key(key: &str) -> &str {
    if key
        .strip_prefix(CUSTOM_THEME_KEY_PREFIX)
        .is_some_and(|encoded| is_segmented(encoded, '.'))
    {
        CUSTOM_THEMES_KEY
    } else {
        key
    }
}

/// 自定义主题 id → 偏好 / 同步键。id 须为规范形式（段 `[a-z0-9_]+` 以单个 `-` 相连，
/// 不超过 [`MAX_CUSTOM_THEME_ID_LEN`] 字节）；`-` 编码为 `.`，因而可逆且满足云端键名规则
/// `^[a-z0-9_]+(\.[a-z0-9_]+)*$`。
#[must_use]
pub fn custom_theme_key(id: &str) -> Option<String> {
    is_custom_theme_id(id).then(|| format!("{CUSTOM_THEME_KEY_PREFIX}{}", id.replace('-', ".")))
}

/// 偏好 / 同步键 → 自定义主题 id；不是主题键时为 `None`。
#[must_use]
pub fn custom_theme_id(key: &str) -> Option<String> {
    let encoded = key.strip_prefix(CUSTOM_THEME_KEY_PREFIX)?;
    is_segmented(encoded, '.').then(|| encoded.replace('.', "-"))
}

/// 规范的自定义主题 id（见 [`custom_theme_key`]）。
#[must_use]
pub fn is_custom_theme_id(id: &str) -> bool {
    is_segmented(id, '-')
}

/// 主题文件原文作为同步值时不超过 [`MAX_SYNC_VALUE_BYTES`]（按 JSON 字符串转义后计）。
#[must_use]
pub fn custom_theme_fits_sync(text: &str) -> bool {
    serde_json::to_vec(text).is_ok_and(|bytes| bytes.len() <= MAX_SYNC_VALUE_BYTES)
}

/// 文件图标包选择（GPUI / Web 任务列表）：`builtin:<id>` 或 `custom:<id>`，id 规则同自定义主题。
pub const FILE_ICON_PACK_KEY: &str = "appearance.file_icon_pack";

/// 合法的图标包引用：`builtin:<id>` / `custom:<id>`（id 见 [`is_custom_theme_id`]）。
#[must_use]
pub fn is_icon_pack_ref(value: &str) -> bool {
    value.split_once(':').is_some_and(|(source, id)| {
        matches!(source, "builtin" | "custom") && is_custom_theme_id(id)
    })
}

/// 非空、不超过 [`MAX_CUSTOM_THEME_ID_LEN`] 字节，由 `separator` 分隔的 `[a-z0-9_]+` 段。
fn is_segmented(text: &str, separator: char) -> bool {
    !text.is_empty()
        && text.len() <= MAX_CUSTOM_THEME_ID_LEN
        && text.split(separator).all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

#[must_use]
pub fn setting_value_kind(key: &str) -> SettingValueKind {
    if boolean_key(key) {
        SettingValueKind::Boolean
    } else if integer_key(key) {
        SettingValueKind::Integer
    } else if float_key(key) {
        SettingValueKind::Float
    } else {
        SettingValueKind::String
    }
}

/// 校验同步目录键的值；自定义主题键以其范围键（`setting_spec(key).key`）校验。
pub fn validate_value(key: &str, value: &Value) -> Result<(), String> {
    if key == "custom_categories" {
        return validate_custom_categories(value);
    }
    if key == CUSTOM_THEMES_KEY {
        return validate_custom_theme(value);
    }
    if boolean_key(key) {
        return value
            .as_bool()
            .map(|_| ())
            .ok_or_else(|| format!("{key} must be boolean"));
    }
    if integer_key(key) {
        let value = value
            .as_i64()
            .ok_or_else(|| format!("{key} must be integer"))?;
        let (minimum, maximum) = integer_range(key);
        return (minimum..=maximum)
            .contains(&value)
            .then_some(())
            .ok_or_else(|| format!("{key} must be between {minimum} and {maximum}"));
    }
    if float_key(key) {
        return value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|_| ())
            .ok_or_else(|| format!("{key} must be a non-negative number"));
    }
    let value = value
        .as_str()
        .ok_or_else(|| format!("{key} must be string"))?;
    match key {
        "appearance.theme_mode" if !matches!(value, "system" | "light" | "dark") => {
            Err(format!("{key} has unknown value"))
        }
        FILE_ICON_PACK_KEY if !is_icon_pack_ref(value) => {
            Err(format!("{key} must be builtin:<id> or custom:<id>"))
        }
        "general.update_channel" if !matches!(value, "stable" | "frontier") => {
            Err(format!("{key} has unknown value"))
        }
        "bt.seed_limit_operator" if !matches!(value, "or" | "and") => {
            Err(format!("{key} has unknown value"))
        }
        "bt.seed_then_action" if !matches!(value, "stop" | "delete" | "delete_files") => {
            Err(format!("{key} has unknown value"))
        }
        "bt.guard.rule_excessive"
        | "bt.guard.rule_rerequest"
        | "bt.guard.rule_rewind"
        | "bt.guard.rule_lag"
        | "bt.guard.rule_fingerprint"
        | "bt.guard.cloud_mode"
            if !matches!(value, "off" | "observe" | "enforce") =>
        {
            Err(format!("{key} has unknown value"))
        }
        "bt.guard.downloading_max_action"
            if !matches!(value, "observe" | "throttle" | "choke" | "ban") =>
        {
            Err(format!("{key} has unknown value"))
        }
        _ => Ok(()),
    }
}

pub fn value_to_daemon_config(spec: &SettingSpec, value: &Value) -> Result<String, String> {
    validate_value(spec.key, value)?;
    match value {
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(value.clone()),
        _ => Err(format!("{} has unsupported config value", spec.key)),
    }
}

pub fn daemon_config_to_value(spec: &SettingSpec, value: &str) -> Result<Value, String> {
    if boolean_key(spec.key) {
        return match value {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("{} has invalid boolean config", spec.key)),
        };
    }
    if integer_key(spec.key) {
        return value
            .parse::<i64>()
            .map(Value::from)
            .map_err(|error| error.to_string());
    }
    if float_key(spec.key) {
        let value = value.parse::<f64>().map_err(|error| error.to_string())?;
        return serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| format!("{} has invalid float config", spec.key));
    }
    Ok(Value::String(value.to_owned()))
}

/// `custom_categories` 的线上形态：分类对象数组，或与 Flutter 偏好同形的 JSON 数组字符串。
fn validate_custom_categories(value: &Value) -> Result<(), String> {
    let parsed;
    let list = match value {
        Value::Array(list) => list,
        Value::String(text) => {
            parsed = serde_json::from_str::<Value>(text)
                .map_err(|error| format!("custom_categories is not valid JSON: {error}"))?;
            parsed
                .as_array()
                .ok_or_else(|| "custom_categories must be a JSON array".to_owned())?
        }
        _ => return Err("custom_categories must be an array".to_owned()),
    };
    if list.iter().all(Value::is_object) {
        Ok(())
    } else {
        Err("custom_categories entries must be objects".to_owned())
    }
}

/// 单个自定义主题的线上形态：主题文件原文（JSON 对象文本），转义后不超过 [`MAX_SYNC_VALUE_BYTES`]。
fn validate_custom_theme(value: &Value) -> Result<(), String> {
    let text = value
        .as_str()
        .ok_or_else(|| "custom theme must be the theme file text".to_owned())?;
    if !custom_theme_fits_sync(text) {
        return Err(format!("custom theme exceeds {MAX_SYNC_VALUE_BYTES} bytes"));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(_)) => Ok(()),
        Ok(_) => Err("custom theme must be a JSON object".to_owned()),
        Err(error) => Err(format!("custom theme is not valid JSON: {error}")),
    }
}

fn boolean_key(key: &str) -> bool {
    matches!(
        key,
        "general.auto_check_update"
            | "general.clipboard_watch"
            | "general.floating_ball_enabled"
            | "general.floating_ball_active_only"
            | "ui.show_sidebar_status"
            | "ui.show_sidebar_queues"
            | "ui.show_sidebar_category"
            | "ui.show_sidebar_rss"
            | "ui.show_activity_rss"
            | "ui.show_activity_webhooks"
            | "ui.show_activity_theme"
            | "ui.show_activity_account"
            | "ui.show_titlebar_pause_all"
            | "ui.show_titlebar_resume_all"
            | "ui.show_titlebar_settings"
            | "ui.show_titlebar_theme"
            | "download.cdn_multi_enabled"
            | "download.auto_resume_on_start"
            | "download.remember_last_save_dir"
            | "download.use_server_time"
            | "download.notify_on_complete"
            | "download.silent_download"
            | "download.keep_awake"
            | "bt.enabled"
            | "bt.enable_dht"
            | "bt.enable_upnp"
            | "bt.enable_utp"
            | "bt.tracker_sub_enabled"
            | "bt.guard.enabled"
            | "bt.guard.skip_private"
            | "bt.guard.skip_lan"
            | "bt.guard.cloud_enabled"
            | "ed2k.enable_kad"
            | "ed2k.enable_upnp"
            | "ed2k.server_sub_enabled"
    )
}

fn integer_key(key: &str) -> bool {
    matches!(
        key,
        "appearance.custom_color"
            | "download.max_concurrent_tasks"
            | "download.default_segments"
            | "download.auto_max_connections"
            | "download.cdn_max_nodes"
            | "download.speed_limit_bytes"
            | "download.max_auto_retries"
            | "download.auto_retry_delay_secs"
            | "bt.seed_time_limit_minutes"
            | "bt.seed_inactive_time_limit_minutes"
            | "bt.seed_max_active"
            | "bt.guard.excess_ratio_pct"
            | "bt.guard.min_torrent_mib"
            | "bt.guard.rewind_tolerance_pct"
            | "bt.guard.throttle_kib"
            | "bt.guard.ban_base_hours"
            | "bt.guard.ban_max_days"
            | "bt.guard.ipv6_prefix"
    )
}

fn float_key(key: &str) -> bool {
    matches!(key, "bt.seed_ratio_limit" | "bt.seed_post_ratio_limit")
}

fn integer_range(key: &str) -> (i64, i64) {
    match key {
        // Flutter `Color.toARGB32()`：无符号 32 位 ARGB。
        "appearance.custom_color" => (0, i64::from(u32::MAX)),
        "download.max_concurrent_tasks" => (1, 1024),
        "download.default_segments" => (0, crate::daemon_config::MAX_TASK_SEGMENTS as i64),
        "download.auto_max_connections" => (0, 128),
        "download.cdn_max_nodes" => (0, 8),
        "download.max_auto_retries" => (-1, 20),
        "download.auto_retry_delay_secs" => (0, 86_400),
        "bt.guard.excess_ratio_pct" => (110, 1000),
        "bt.guard.min_torrent_mib" => (0, 1_048_576),
        "bt.guard.rewind_tolerance_pct" => (0, 100),
        "bt.guard.throttle_kib" => (1, 1_048_576),
        "bt.guard.ban_base_hours" => (1, 720),
        "bt.guard.ban_max_days" => (1, 365),
        "bt.guard.ipv6_prefix" => (48, 128),
        _ => (0, i64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::{
        CUSTOM_THEMES_KEY, FILE_ICON_PACK_KEY, MAX_SYNC_VALUE_BYTES, SYNC_SETTING_SPECS,
        SettingOwner, custom_theme_id, custom_theme_key, setting_spec, sync_scope_key,
        validate_value,
    };

    #[test]
    fn catalog_has_exact_unique_flutter_count_and_namespaced_daemon_mapping() {
        assert_eq!(SYNC_SETTING_SPECS.len(), 79);
        assert_eq!(
            SYNC_SETTING_SPECS
                .iter()
                .map(|spec| spec.key)
                .collect::<HashSet<_>>()
                .len(),
            79
        );
        let spec = setting_spec("download.max_concurrent_tasks").expect("download spec");
        assert_eq!(spec.owner, SettingOwner::Daemon);
        assert_eq!(spec.storage_key, "max_concurrent_tasks");
        assert!(setting_spec("ui.show_sidebar_status").is_some());
        // 侧栏「设备区」显隐是设备本地偏好，永不入目录。
        assert!(setting_spec("ui.show_sidebar_devices").is_none());
    }

    #[test]
    fn validation_preserves_zero_auto_and_negative_infinite_retry_semantics() {
        assert!(validate_value("download.auto_max_connections", &json!(0)).is_ok());
        assert!(validate_value("download.max_auto_retries", &json!(-1)).is_ok());
        assert!(validate_value("bt.seed_then_action", &json!("delete_files")).is_ok());
        assert!(validate_value("bt.seed_then_action", &json!("remove")).is_err());
    }

    #[test]
    fn custom_color_accepts_flutter_argb32_integer_only() {
        assert!(validate_value("appearance.custom_color", &json!(0xFF11_2233_u32)).is_ok());
        assert!(validate_value("appearance.custom_color", &json!(u32::MAX)).is_ok());
        assert!(validate_value("appearance.custom_color", &json!(-1)).is_err());
        assert!(validate_value("appearance.custom_color", &json!("ff112233")).is_err());
    }

    #[test]
    fn file_icon_pack_accepts_canonical_pack_refs_only() {
        for value in ["builtin:material", "custom:my-pack_2"] {
            assert!(
                validate_value(FILE_ICON_PACK_KEY, &json!(value)).is_ok(),
                "{value}"
            );
        }
        for value in [
            json!("material"),
            json!("theme:x"),
            json!("custom:A"),
            json!(1),
        ] {
            assert!(
                validate_value(FILE_ICON_PACK_KEY, &value).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn custom_categories_accepts_object_array_or_flutter_json_string_only() {
        assert!(validate_value("custom_categories", &json!([{"id": "a"}])).is_ok());
        assert!(validate_value("custom_categories", &json!([])).is_ok());
        assert!(validate_value("custom_categories", &json!(r#"[{"id":"a"}]"#)).is_ok());
        assert!(validate_value("custom_categories", &json!("not json")).is_err());
        assert!(validate_value("custom_categories", &json!(r#"{"id":"a"}"#)).is_err());
        assert!(validate_value("custom_categories", &json!([1, 2])).is_err());
        assert!(validate_value("custom_categories", &json!(true)).is_err());
    }

    #[test]
    fn custom_theme_keys_round_trip_and_satisfy_the_cloud_key_grammar() {
        let key = custom_theme_key("nord-square_2").expect("canonical id");
        assert_eq!(key, "appearance.custom_themes.nord.square_2");
        assert_eq!(custom_theme_id(&key).as_deref(), Some("nord-square_2"));
        assert_eq!(sync_scope_key(&key), CUSTOM_THEMES_KEY);
        assert_eq!(
            setting_spec(&key).map(|spec| spec.key),
            Some(CUSTOM_THEMES_KEY)
        );
        let longest = "a".repeat(64);
        assert!(custom_theme_key(&longest).is_some());
        // 非规范 id（大写、空段、越长、越界字符）不成键，避免与云端键名规则或文件名冲突。
        for id in [
            "",
            "Nord",
            "a--b",
            "-a",
            "a-",
            "a.b",
            "../x",
            &"a".repeat(65),
        ] {
            assert!(custom_theme_key(id).is_none(), "{id:?}");
        }
        // 范围键本身与非规范后缀都不是主题键。
        for key in [
            CUSTOM_THEMES_KEY,
            "appearance.custom_themes.",
            "appearance.custom_themes.a..b",
            "appearance.custom_themes.A",
        ] {
            assert_eq!(custom_theme_id(key), None, "{key}");
            assert_eq!(sync_scope_key(key), key);
        }
    }

    #[test]
    fn custom_theme_values_are_object_text_within_the_cloud_value_limit() {
        let key = custom_theme_key("ocean").expect("key");
        let spec = setting_spec(&key).expect("theme spec");
        assert_eq!(spec.owner, SettingOwner::Preferences);
        assert!(validate_value(spec.key, &json!(r#"{"meta":{"id":"ocean"}}"#)).is_ok());
        assert!(validate_value(spec.key, &json!({"meta": {}})).is_err());
        assert!(validate_value(spec.key, &json!("[1]")).is_err());
        assert!(validate_value(spec.key, &json!("{")).is_err());
        let padded = format!(r#"{{"pad":"{}"}}"#, "x".repeat(MAX_SYNC_VALUE_BYTES));
        assert!(validate_value(spec.key, &json!(padded)).is_err());
        // 转义计入上限：引号在线上翻倍成 `\"`。
        let quotes = format!(r#"{{"pad":"{}"}}"#, "\\\"".repeat(MAX_SYNC_VALUE_BYTES / 3));
        assert!(validate_value(spec.key, &json!(quotes)).is_err());
    }
}

#[cfg(test)]
mod bt_guard_tests {
    use serde_json::json;

    use super::{SYNC_SETTING_SPECS, SettingOwner, setting_spec, validate_value};
    use crate::daemon_config::{
        DaemonConfigKind, daemon_config_field, normalize_daemon_config_value,
    };

    #[test]
    fn guard_specs_map_to_daemon_keys_with_matching_kinds() {
        let specs: Vec<_> = SYNC_SETTING_SPECS
            .iter()
            .filter(|spec| spec.key.starts_with("bt.guard."))
            .collect();
        assert_eq!(specs.len(), 19);
        for spec in specs {
            assert_eq!(spec.owner, SettingOwner::Daemon, "{}", spec.key);
            assert_eq!(
                spec.storage_key,
                format!("bt_guard_{}", &spec.key["bt.guard.".len()..]),
                "{}",
                spec.key
            );
            let field = daemon_config_field(spec.storage_key).expect("daemon field");
            // 同步层与 daemon 目录的值域必须一致：daemon 默认值能过同步校验。
            let wire = match field.kind {
                DaemonConfigKind::Bool => json!(field.default == "true"),
                DaemonConfigKind::Integer { .. } => json!(field.default.parse::<i64>().unwrap()),
                _ => json!(field.default),
            };
            assert!(validate_value(spec.key, &wire).is_ok(), "{}", spec.key);
        }
    }

    #[test]
    fn guard_ranges_match_daemon_catalog() {
        for (key, lo, hi) in [
            ("excess_ratio_pct", 110_i64, 1000_i64),
            ("min_torrent_mib", 0, 1_048_576),
            ("rewind_tolerance_pct", 0, 100),
            ("throttle_kib", 1, 1_048_576),
            ("ban_base_hours", 1, 720),
            ("ban_max_days", 1, 365),
            ("ipv6_prefix", 48, 128),
        ] {
            let sync = format!("bt.guard.{key}");
            let daemon = format!("bt_guard_{key}");
            assert!(validate_value(&sync, &json!(lo)).is_ok(), "{sync}");
            assert!(validate_value(&sync, &json!(hi)).is_ok(), "{sync}");
            assert!(validate_value(&sync, &json!(lo - 1)).is_err(), "{sync}");
            assert!(validate_value(&sync, &json!(hi + 1)).is_err(), "{sync}");
            assert!(validate_value(&sync, &json!("1")).is_err(), "{sync}");
            assert!(normalize_daemon_config_value(&daemon, &lo.to_string()).is_ok());
            assert!(normalize_daemon_config_value(&daemon, &(lo - 1).to_string()).is_err());
            assert!(normalize_daemon_config_value(&daemon, &(hi + 1).to_string()).is_err());
        }
    }

    #[test]
    fn guard_enums_and_booleans_are_validated() {
        for key in [
            "rule_excessive",
            "rule_rerequest",
            "rule_rewind",
            "rule_lag",
            "rule_fingerprint",
            "cloud_mode",
        ] {
            let sync = format!("bt.guard.{key}");
            for ok in ["off", "observe", "enforce"] {
                assert!(validate_value(&sync, &json!(ok)).is_ok(), "{sync}={ok}");
            }
            assert!(validate_value(&sync, &json!("ban")).is_err(), "{sync}");
            assert!(validate_value(&sync, &json!(true)).is_err(), "{sync}");
        }
        for ok in ["observe", "throttle", "choke", "ban"] {
            assert!(validate_value("bt.guard.downloading_max_action", &json!(ok)).is_ok());
        }
        assert!(validate_value("bt.guard.downloading_max_action", &json!("enforce")).is_err());
        for key in ["enabled", "skip_private", "skip_lan", "cloud_enabled"] {
            let sync = format!("bt.guard.{key}");
            assert!(validate_value(&sync, &json!(true)).is_ok(), "{sync}");
            assert!(validate_value(&sync, &json!("true")).is_err(), "{sync}");
        }
        assert!(validate_value("bt.guard.filter_urls", &json!("https://a\nhttps://b")).is_ok());
        assert!(validate_value("bt.guard.filter_urls", &json!(1)).is_err());
    }

    #[test]
    fn device_local_and_read_only_guard_keys_are_not_synced() {
        assert!(setting_spec("bt.guard.report_enabled").is_none());
        assert!(setting_spec("bt.guard.cloud_cache").is_none());
    }
}
