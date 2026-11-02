//! daemon 持久化配置键的唯一目录：类型、范围、默认值与可写性。
//!
//! daemon 用它校验 `daemon.config.patch`，UI 用它渲染控件与解析快照；
//! 两侧不得各自维护第二份键表。

use std::collections::BTreeMap;

/// 单个 daemon 配置键的值域。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DaemonConfigKind {
    Bool,
    Integer {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
    },
    Enum(&'static [&'static str]),
    Text,
    /// 引擎自行维护的键：可读、不可经 patch 写入。
    ReadOnly,
}

/// daemon 配置键描述。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DaemonConfigField {
    pub key: &'static str,
    pub kind: DaemonConfigKind,
    /// 未持久化时的有效默认值（wire 字符串形式）。
    pub default: &'static str,
}

const fn field(
    key: &'static str,
    kind: DaemonConfigKind,
    default: &'static str,
) -> DaemonConfigField {
    DaemonConfigField { key, kind, default }
}

pub const BT_SEED_TIME_UNITS: &[&str] = &["minutes", "hours", "days"];
pub const FILE_EXISTS_BEHAVIORS: &[&str] = &["rename", "overwrite", "skip", "ask"];
pub const FILE_MISSING_ACTIONS: &[&str] = &["keep", "delete"];
pub const BT_SEED_LIMIT_OPERATORS: &[&str] = &["or", "and"];
pub const BT_SEED_THEN_ACTIONS: &[&str] = &["stop", "delete", "delete_files"];
pub const BT_MSE_MODES: &[&str] = &["disabled", "enabled", "forced"];
/// BT 防吸血规则 / 云名单的执行模式。
pub const BT_GUARD_MODES: &[&str] = &["off", "observe", "enforce"];
/// BT 防吸血：下载中任务的动作上限。
pub const BT_GUARD_DOWNLOADING_ACTIONS: &[&str] = &["observe", "throttle", "choke", "ban"];
pub const PROXY_MODES: &[&str] = &["none", "system", "manual", "auto"];
pub const PROXY_TYPES: &[&str] = &["http", "https", "socks4", "socks5"];

/// 单任务连接（线程）数上限：设置、队列默认值与新建任务共用；
/// 与引擎 `segment_coordinator::MAX_SEGMENTS` 保持一致。
pub const MAX_TASK_SEGMENTS: i32 = 512;
/// 线程数超过该值时 UI 须动态提示可能触发服务器限速或安全风控封禁 IP（提醒档）。
pub const HIGH_SEGMENTS_WARN_ABOVE: i32 = 64;
/// 线程数超过该值时提示升级为高风险档（危险色、更强措辞）。
pub const SEVERE_SEGMENTS_WARN_ABOVE: i32 = 256;

/// 全部 daemon 配置键。顺序无语义。
pub const DAEMON_CONFIG_FIELDS: &[DaemonConfigField] = &[
    // ── 下载 ──
    field("default_save_dir", DaemonConfigKind::Text, ""),
    field(
        "default_segments",
        DaemonConfigKind::Integer {
            min: 0,
            max: MAX_TASK_SEGMENTS as i64,
        },
        "0",
    ),
    field(
        "auto_max_connections",
        DaemonConfigKind::Integer { min: 0, max: 128 },
        "16",
    ),
    field("cdn_multi_enabled", DaemonConfigKind::Bool, "false"),
    field(
        "cdn_max_nodes",
        DaemonConfigKind::Integer { min: 0, max: 8 },
        "0",
    ),
    field("multi_nic_enabled", DaemonConfigKind::Bool, "false"),
    field(
        "max_concurrent_tasks",
        DaemonConfigKind::Integer { min: 1, max: 1024 },
        "5",
    ),
    field(
        "speed_limit_bytes",
        DaemonConfigKind::Integer {
            min: 0,
            max: i64::MAX,
        },
        "0",
    ),
    field(
        "upload_limit_bytes",
        DaemonConfigKind::Integer {
            min: 0,
            max: i64::MAX,
        },
        "0",
    ),
    field(
        "max_auto_retries",
        DaemonConfigKind::Integer { min: -1, max: 20 },
        "3",
    ),
    field(
        "auto_retry_delay_secs",
        DaemonConfigKind::Integer {
            min: 0,
            max: 86_400,
        },
        "5",
    ),
    field("auto_resume_on_start", DaemonConfigKind::Bool, "false"),
    field("use_server_time", DaemonConfigKind::Bool, "false"),
    field("dedup_same_url", DaemonConfigKind::Bool, "false"),
    field(
        "file_exists_behavior",
        DaemonConfigKind::Enum(FILE_EXISTS_BEHAVIORS),
        "rename",
    ),
    field(
        "file_missing_action",
        DaemonConfigKind::Enum(FILE_MISSING_ACTIONS),
        "keep",
    ),
    // 已完成任务记录保留天数：超期的「已完成」记录被自动清理（只删记录，不删文件）；0 = 永不。
    field(
        "completed_record_retention_days",
        DaemonConfigKind::Integer { min: 0, max: 3650 },
        "0",
    ),
    // 空闲（无活动/排队任务）时是否仍执行周期性文件跟踪扫描；默认关闭以免唤醒 NAS 硬盘。
    field("idle_file_scan", DaemonConfigKind::Bool, "false"),
    field("global_user_agent", DaemonConfigKind::Text, ""),
    field("default_queue_id", DaemonConfigKind::Text, ""),
    field("domain_conn_caps", DaemonConfigKind::ReadOnly, ""),
    // ── BT ──
    field("bt_enabled", DaemonConfigKind::Bool, "true"),
    field("bt_enable_dht", DaemonConfigKind::Bool, "true"),
    field("bt_enable_upnp", DaemonConfigKind::Bool, "true"),
    field("bt_enable_utp", DaemonConfigKind::Bool, "true"),
    field(
        "bt_port_start",
        DaemonConfigKind::Integer {
            min: 1,
            max: 65_535,
        },
        "6881",
    ),
    field(
        "bt_port_end",
        DaemonConfigKind::Integer {
            min: 1,
            max: 65_535,
        },
        "6891",
    ),
    field(
        "bt_mse_mode",
        DaemonConfigKind::Enum(BT_MSE_MODES),
        "enabled",
    ),
    field("bt_custom_trackers", DaemonConfigKind::Text, ""),
    field("bt_tracker_sub_enabled", DaemonConfigKind::Bool, "true"),
    field("bt_tracker_sub_urls", DaemonConfigKind::Text, ""),
    field("bt_tracker_sub_cache", DaemonConfigKind::ReadOnly, ""),
    field("bt_tracker_sub_updated_at", DaemonConfigKind::ReadOnly, "0"),
    field("bt_seed_enabled", DaemonConfigKind::Bool, "true"),
    field("bt_auto_reseed", DaemonConfigKind::Bool, "true"),
    field(
        "bt_seed_max_active",
        DaemonConfigKind::Integer {
            min: 0,
            max: i64::MAX,
        },
        "0",
    ),
    field(
        "bt_seed_ratio_limit",
        DaemonConfigKind::Float { min: 0.0 },
        "0",
    ),
    field(
        "bt_seed_post_ratio_limit",
        DaemonConfigKind::Float { min: 0.0 },
        "0",
    ),
    field(
        "bt_seed_time_limit_minutes",
        DaemonConfigKind::Integer {
            min: 0,
            max: i64::MAX,
        },
        "0",
    ),
    field(
        "bt_seed_time_limit_unit",
        DaemonConfigKind::Enum(BT_SEED_TIME_UNITS),
        "minutes",
    ),
    field(
        "bt_seed_inactive_time_limit_minutes",
        DaemonConfigKind::Integer {
            min: 0,
            max: i64::MAX,
        },
        "0",
    ),
    field(
        "bt_seed_inactive_time_limit_unit",
        DaemonConfigKind::Enum(BT_SEED_TIME_UNITS),
        "minutes",
    ),
    field(
        "bt_seed_limit_operator",
        DaemonConfigKind::Enum(BT_SEED_LIMIT_OPERATORS),
        "or",
    ),
    field(
        "bt_seed_then_action",
        DaemonConfigKind::Enum(BT_SEED_THEN_ACTIONS),
        "stop",
    ),
    // ── BT 防吸血（全部热更新，不重建 BT 会话）──
    field("bt_guard_enabled", DaemonConfigKind::Bool, "true"),
    field(
        "bt_guard_rule_excessive",
        DaemonConfigKind::Enum(BT_GUARD_MODES),
        "observe",
    ),
    field(
        "bt_guard_rule_rerequest",
        DaemonConfigKind::Enum(BT_GUARD_MODES),
        "observe",
    ),
    field(
        "bt_guard_rule_rewind",
        DaemonConfigKind::Enum(BT_GUARD_MODES),
        "observe",
    ),
    field(
        "bt_guard_rule_lag",
        DaemonConfigKind::Enum(BT_GUARD_MODES),
        "observe",
    ),
    field(
        "bt_guard_rule_fingerprint",
        DaemonConfigKind::Enum(BT_GUARD_MODES),
        "observe",
    ),
    field(
        "bt_guard_cloud_mode",
        DaemonConfigKind::Enum(BT_GUARD_MODES),
        "observe",
    ),
    field(
        "bt_guard_excess_ratio_pct",
        DaemonConfigKind::Integer {
            min: 110,
            max: 1000,
        },
        "200",
    ),
    field(
        "bt_guard_min_torrent_mib",
        DaemonConfigKind::Integer {
            min: 0,
            max: 1_048_576,
        },
        "64",
    ),
    field(
        "bt_guard_rewind_tolerance_pct",
        DaemonConfigKind::Integer { min: 0, max: 100 },
        "7",
    ),
    field(
        "bt_guard_throttle_kib",
        DaemonConfigKind::Integer {
            min: 1,
            max: 1_048_576,
        },
        "32",
    ),
    field(
        "bt_guard_ban_base_hours",
        DaemonConfigKind::Integer { min: 1, max: 720 },
        "24",
    ),
    field(
        "bt_guard_ban_max_days",
        DaemonConfigKind::Integer { min: 1, max: 365 },
        "30",
    ),
    field(
        "bt_guard_downloading_max_action",
        DaemonConfigKind::Enum(BT_GUARD_DOWNLOADING_ACTIONS),
        "throttle",
    ),
    field("bt_guard_skip_private", DaemonConfigKind::Bool, "true"),
    field("bt_guard_skip_lan", DaemonConfigKind::Bool, "true"),
    field(
        "bt_guard_ipv6_prefix",
        DaemonConfigKind::Integer { min: 48, max: 128 },
        "64",
    ),
    field("bt_guard_filter_urls", DaemonConfigKind::Text, ""),
    field("bt_guard_cloud_enabled", DaemonConfigKind::Bool, "true"),
    // 设备本地（不进云同步）：是否向 FluxCloud 上报命中规则的对端证据。
    field("bt_guard_report_enabled", DaemonConfigKind::Bool, "false"),
    // ── ED2K ──
    field("ed2k_enable_kad", DaemonConfigKind::Bool, "true"),
    field("ed2k_enable_upnp", DaemonConfigKind::Bool, "true"),
    field(
        "ed2k_listen_port",
        DaemonConfigKind::Integer {
            min: 0,
            max: 65_535,
        },
        "0",
    ),
    field("ed2k_server_list", DaemonConfigKind::Text, ""),
    field("ed2k_server_sub_enabled", DaemonConfigKind::Bool, "true"),
    field("ed2k_server_sub_urls", DaemonConfigKind::Text, ""),
    field("ed2k_server_sub_cache", DaemonConfigKind::ReadOnly, ""),
    field(
        "ed2k_server_sub_updated_at",
        DaemonConfigKind::ReadOnly,
        "0",
    ),
    field("ed2k_nodes_dat_url", DaemonConfigKind::Text, ""),
    // ── 代理 ──
    field("proxy_mode", DaemonConfigKind::Enum(PROXY_MODES), "none"),
    field("proxy_type", DaemonConfigKind::Enum(PROXY_TYPES), "http"),
    field("proxy_host", DaemonConfigKind::Text, ""),
    field("proxy_port", DaemonConfigKind::Text, ""),
    field("proxy_username", DaemonConfigKind::Text, ""),
    field("proxy_password", DaemonConfigKind::Text, ""),
    field("proxy_no_list", DaemonConfigKind::Text, ""),
    // 站点代理规则：JSON 数组（顺序即优先级），模型见 `crate::proxy_rules`。
    field("proxy_rules", DaemonConfigKind::Text, ""),
    // ── Webhook ──
    field("webhook.endpoints", DaemonConfigKind::Text, ""),
    // ── 受管组件手动路径（空 = 自动解析）──
    field("component.ffmpeg.path", DaemonConfigKind::Text, ""),
    field("component.ytdlp.path", DaemonConfigKind::Text, ""),
    // 组件下载镜像基址（空 = 直连 GitHub）；只接受 https。
    field("component_mirror_base", DaemonConfigKind::Text, ""),
    // ── 日志 ──
    // 日志目录总大小上限（MB），daemon 启动与每次配置提交后应用到引擎 logger。
    field(
        "log_max_size_mb",
        DaemonConfigKind::Integer { min: 1, max: 1024 },
        "10",
    ),
];

/// 校验失败原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonConfigError {
    UnknownField(String),
    ReadOnly(String),
    InvalidValue { field: String, message: String },
}

impl std::fmt::Display for DaemonConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownField(key) => write!(f, "unknown daemon config field: {key}"),
            Self::ReadOnly(key) => write!(f, "daemon config field is read-only: {key}"),
            Self::InvalidValue { field, message } => {
                write!(f, "invalid daemon config value for {field}: {message}")
            }
        }
    }
}

impl std::error::Error for DaemonConfigError {}

#[must_use]
pub fn daemon_config_field(key: &str) -> Option<&'static DaemonConfigField> {
    DAEMON_CONFIG_FIELDS.iter().find(|field| field.key == key)
}

/// 键在 `daemon.config.get` 投影中是否可见（含只读键）。
#[must_use]
pub fn is_public_daemon_config_key(key: &str) -> bool {
    daemon_config_field(key).is_some()
}

/// 未持久化时的默认值；未知键返回空串。
#[must_use]
pub fn daemon_config_default(key: &str) -> &'static str {
    daemon_config_field(key).map_or("", |field| field.default)
}

/// 规范化单个可写键的值。
pub fn normalize_daemon_config_value(key: &str, value: &str) -> Result<String, DaemonConfigError> {
    let field =
        daemon_config_field(key).ok_or_else(|| DaemonConfigError::UnknownField(key.to_owned()))?;
    let invalid = |message: String| DaemonConfigError::InvalidValue {
        field: key.to_owned(),
        message,
    };
    match field.kind {
        DaemonConfigKind::ReadOnly => Err(DaemonConfigError::ReadOnly(key.to_owned())),
        DaemonConfigKind::Bool => match value.trim() {
            "true" | "1" => Ok("true".to_owned()),
            "false" | "0" => Ok("false".to_owned()),
            other => Err(invalid(format!("expected boolean, got {other:?}"))),
        },
        DaemonConfigKind::Integer { min, max } => {
            let parsed = value
                .trim()
                .parse::<i64>()
                .map_err(|error| invalid(error.to_string()))?;
            if parsed < min || parsed > max {
                return Err(invalid(format!("must be between {min} and {max}")));
            }
            Ok(parsed.to_string())
        }
        DaemonConfigKind::Float { min } => {
            let parsed = value
                .trim()
                .parse::<f64>()
                .map_err(|error| invalid(error.to_string()))?;
            if !parsed.is_finite() || parsed < min {
                return Err(invalid(format!("must be a finite number >= {min}")));
            }
            Ok(parsed.to_string())
        }
        DaemonConfigKind::Enum(allowed) => {
            let trimmed = value.trim();
            if allowed.contains(&trimmed) {
                Ok(trimmed.to_owned())
            } else {
                Err(invalid(format!("must be one of {}", allowed.join(", "))))
            }
        }
        DaemonConfigKind::Text if key == "component_mirror_base" => {
            normalize_mirror_base(value).map_err(invalid)
        }
        DaemonConfigKind::Text if key == "global_user_agent" => {
            let trimmed = value.trim();
            // 与 HTTP HeaderValue 规则一致：可见字符、空格与制表符；拒绝控制字符。
            if trimmed.bytes().any(|b| (b < 32 && b != b'\t') || b == 127) {
                return Err(invalid("must not contain control characters".to_owned()));
            }
            Ok(trimmed.to_owned())
        }
        DaemonConfigKind::Text => Ok(value.trim().to_owned()),
    }
}

/// 组件镜像基址：空串表示直连；非空必须是 `https://host[/path]`，不带查询 / 片段 /
/// 空白（基址会与 GitHub 路径直接拼接）。末尾 `/` 被去掉。
fn normalize_mirror_base(value: &str) -> Result<String, String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err("must not contain whitespace or control characters".to_owned());
    }
    let scheme_len = "https://".len();
    let Some(scheme) = trimmed.get(..scheme_len) else {
        return Err("must start with https://".to_owned());
    };
    if !scheme.eq_ignore_ascii_case("https://") {
        return Err("must start with https://".to_owned());
    }
    let rest = trimmed.get(scheme_len..).unwrap_or_default();
    if rest.contains(['?', '#']) {
        return Err("must not contain a query string or fragment".to_owned());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = host_port.split(':').next().unwrap_or_default();
    if host.is_empty() {
        return Err("must include a host".to_owned());
    }
    Ok(format!("https://{rest}"))
}

/// 校验并规范化一整个 patch；任一键失败即整体失败。
pub fn normalize_daemon_config_patch(
    values: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, DaemonConfigError> {
    values
        .iter()
        .map(|(key, value)| normalize_daemon_config_value(key, value).map(|v| (key.clone(), v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_keys_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for field in DAEMON_CONFIG_FIELDS {
            assert!(seen.insert(field.key), "duplicate key {}", field.key);
        }
    }

    #[test]
    fn sync_catalog_daemon_keys_exist_in_config_catalog() {
        for spec in crate::settings::SYNC_SETTING_SPECS {
            if spec.owner == crate::settings::SettingOwner::Daemon {
                let field = daemon_config_field(spec.storage_key);
                assert!(field.is_some(), "missing daemon field {}", spec.storage_key);
                assert_ne!(
                    field.map(|f| f.kind),
                    Some(DaemonConfigKind::ReadOnly),
                    "{} must be writable",
                    spec.storage_key
                );
            }
        }
    }

    #[test]
    fn defaults_are_valid_for_writable_fields() {
        for field in DAEMON_CONFIG_FIELDS {
            if field.kind == DaemonConfigKind::ReadOnly {
                continue;
            }
            normalize_daemon_config_value(field.key, field.default)
                .unwrap_or_else(|error| panic!("{}: {error}", field.key));
        }
    }

    #[test]
    fn global_user_agent_rejects_control_characters() {
        assert_eq!(
            normalize_daemon_config_value("global_user_agent", "  Mozilla/5.0\t(X) 中文 ")
                .as_deref(),
            Ok("Mozilla/5.0\t(X) 中文")
        );
        for bad in ["a\nb", "a\rb", "a\u{1}b", "a\u{7f}b"] {
            assert!(
                normalize_daemon_config_value("global_user_agent", bad).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn normalizes_and_rejects() {
        assert_eq!(
            normalize_daemon_config_value("bt_enable_dht", "1").as_deref(),
            Ok("true")
        );
        assert!(normalize_daemon_config_value("cdn_max_nodes", "9").is_err());
        assert!(normalize_daemon_config_value("bt_tracker_sub_cache", "x").is_err());
        assert!(normalize_daemon_config_value("nope", "x").is_err());
        assert_eq!(
            normalize_daemon_config_value("proxy_mode", " manual ").as_deref(),
            Ok("manual")
        );
    }

    #[test]
    fn default_segments_accepts_up_to_task_cap() {
        assert_eq!(
            normalize_daemon_config_value("default_segments", "512").as_deref(),
            Ok("512")
        );
        assert!(normalize_daemon_config_value("default_segments", "513").is_err());
        assert!(
            crate::settings::validate_value(
                "download.default_segments",
                &serde_json::json!(MAX_TASK_SEGMENTS)
            )
            .is_ok()
        );
    }

    #[test]
    fn component_mirror_base_only_accepts_https() {
        let normalize = |value: &str| normalize_daemon_config_value("component_mirror_base", value);
        assert_eq!(normalize("").as_deref(), Ok(""));
        assert_eq!(normalize("  ").as_deref(), Ok(""));
        assert_eq!(
            normalize(" HTTPS://gh.example.com/proxy/ ").as_deref(),
            Ok("https://gh.example.com/proxy")
        );
        for bad in [
            "http://gh.example.com",
            "gh.example.com",
            "ftp://gh.example.com",
            "https://",
            "https:///path",
            "https://gh.example.com/a b",
            "https://gh.example.com/?x=1",
            "https://gh.example.com/#frag",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?} must be rejected");
        }
        let Err(DaemonConfigError::InvalidValue { field, .. }) = normalize("http://x.test") else {
            panic!("http mirror must be an invalid-value error");
        };
        assert_eq!(field, "component_mirror_base");
    }

    #[test]
    fn log_max_size_is_bounded_in_megabytes() {
        assert_eq!(
            normalize_daemon_config_value("log_max_size_mb", " 64 ").as_deref(),
            Ok("64")
        );
        assert!(normalize_daemon_config_value("log_max_size_mb", "0").is_err());
        assert!(normalize_daemon_config_value("log_max_size_mb", "1025").is_err());
    }

    #[test]
    fn bt_guard_keys_have_contract_ranges_and_defaults() {
        let ok = |k: &str, v: &str| normalize_daemon_config_value(k, v).is_ok();
        for (key, lo, hi) in [
            ("bt_guard_excess_ratio_pct", 110, 1000),
            ("bt_guard_min_torrent_mib", 0, 1_048_576),
            ("bt_guard_rewind_tolerance_pct", 0, 100),
            ("bt_guard_throttle_kib", 1, 1_048_576),
            ("bt_guard_ban_base_hours", 1, 720),
            ("bt_guard_ban_max_days", 1, 365),
            ("bt_guard_ipv6_prefix", 48, 128),
        ] {
            assert!(ok(key, &lo.to_string()), "{key} min");
            assert!(ok(key, &hi.to_string()), "{key} max");
            assert!(!ok(key, &(lo - 1).to_string()), "{key} below min");
            assert!(!ok(key, &(hi + 1).to_string()), "{key} above max");
            assert!(!ok(key, "abc"), "{key} non-numeric");
        }
        for (key, default) in [
            ("bt_guard_enabled", "true"),
            ("bt_guard_rule_excessive", "observe"),
            ("bt_guard_rule_rerequest", "observe"),
            ("bt_guard_rule_rewind", "observe"),
            ("bt_guard_rule_lag", "observe"),
            ("bt_guard_rule_fingerprint", "observe"),
            ("bt_guard_cloud_mode", "observe"),
            ("bt_guard_excess_ratio_pct", "200"),
            ("bt_guard_min_torrent_mib", "64"),
            ("bt_guard_rewind_tolerance_pct", "7"),
            ("bt_guard_throttle_kib", "32"),
            ("bt_guard_ban_base_hours", "24"),
            ("bt_guard_ban_max_days", "30"),
            ("bt_guard_downloading_max_action", "throttle"),
            ("bt_guard_skip_private", "true"),
            ("bt_guard_skip_lan", "true"),
            ("bt_guard_ipv6_prefix", "64"),
            ("bt_guard_filter_urls", ""),
            ("bt_guard_cloud_enabled", "true"),
            ("bt_guard_report_enabled", "false"),
        ] {
            assert_eq!(daemon_config_default(key), default, "{key}");
        }
    }

    #[test]
    fn bt_guard_enums_are_validated() {
        for mode in ["off", "observe", "enforce"] {
            assert_eq!(
                normalize_daemon_config_value("bt_guard_rule_lag", &format!(" {mode} ")).as_deref(),
                Ok(mode)
            );
            assert!(normalize_daemon_config_value("bt_guard_cloud_mode", mode).is_ok());
        }
        assert!(normalize_daemon_config_value("bt_guard_rule_lag", "ban").is_err());
        assert!(normalize_daemon_config_value("bt_guard_cloud_mode", "").is_err());
        for action in ["observe", "throttle", "choke", "ban"] {
            assert!(
                normalize_daemon_config_value("bt_guard_downloading_max_action", action).is_ok()
            );
        }
        assert!(normalize_daemon_config_value("bt_guard_downloading_max_action", "off").is_err());
        assert!(is_public_daemon_config_key("bt_guard_filter_urls"));
    }

    #[test]
    fn bt_guard_report_flag_is_device_local() {
        assert!(
            crate::settings::SYNC_SETTING_SPECS
                .iter()
                .all(|spec| spec.storage_key != "bt_guard_report_enabled")
        );
    }
}
