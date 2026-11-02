//! daemon 进程环境配置与 loopback 绑定约束。

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

#[cfg(feature = "bt")]
use fluxdown_engine::bt_guard::{BtGuardConfig, DownloadingMaxAction, RuleMode};
use fluxdown_protocol::{
    DaemonConfigError, is_public_daemon_config_key, normalize_daemon_config_patch,
};

/// daemon 默认控制端口。
pub const DEFAULT_DAEMON_BIND: &str = "127.0.0.1:17801";

/// daemon 进程级配置。
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub bind_addr: SocketAddr,
    /// 仅在 `FLUXDOWN_DAEMON_BIND` 未设置（使用默认地址）时为真：默认端口被占用或被系统保留时
    /// 回退到系统分配的 loopback 端口。显式地址始终严格。
    pub bind_fallback: bool,
    /// 是否发布 `daemon-endpoint.json`（独立进程为真，嵌入式为假）。
    pub publish_endpoint: bool,
    pub data_dir_override: Option<PathBuf>,
    pub database_url: Option<String>,
    pub token_file_override: Option<PathBuf>,
    /// `FLUXDOWN_SAVE_DIR`：仅在库中 `default_save_dir` 尚未设置时播种（见 `init_default_config`）。
    pub save_dir_seed: Option<String>,
    /// 演示模式：`Some(url)` 时任务 / 任务组创建只放行该 URL（trim 后精确比较）。
    pub demo_url: Option<String>,
}

/// daemon 配置错误。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("FLUXDOWN_DAEMON_BIND is invalid: {0}")]
    InvalidBind(String),
    #[error("FLUXDOWN_DAEMON_BIND must be loopback, got {0}")]
    NonLoopback(IpAddr),
    #[error("unknown or daemon-private config field: {0}")]
    UnknownField(String),
    #[error("config field is read-only: {0}")]
    ReadOnlyField(String),
    #[error("invalid config value for {field}: {message}")]
    InvalidValue { field: String, message: String },
}

impl DaemonConfig {
    /// 从环境读取配置并拒绝非 loopback 绑定。
    pub fn from_env() -> Result<Self, ConfigError> {
        let (bind_addr, bind_fallback) = resolve_bind(std::env::var("FLUXDOWN_DAEMON_BIND"))?;
        Ok(Self {
            bind_addr,
            bind_fallback,
            publish_endpoint: true,
            data_dir_override: std::env::var_os("FLUXDOWN_DATA_DIR").map(PathBuf::from),
            database_url: std::env::var("FLUXDOWN_DATABASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            token_file_override: std::env::var_os("FLUXDOWN_DAEMON_TOKEN_FILE").map(PathBuf::from),
            save_dir_seed: std::env::var("FLUXDOWN_SAVE_DIR")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            demo_url: demo_url_from_env(),
        })
    }

    /// 嵌入式（进程内）daemon 配置：不读环境，数据目录由嵌入方给出。
    ///
    /// 绑定 `127.0.0.1:0`（系统分配的临时 loopback 端口，真实地址经
    /// [`crate::runtime::DaemonReady`] 报告）；无演示模式，无外部数据库。
    #[must_use]
    pub fn embedded(data_dir: PathBuf, save_dir_seed: Option<String>) -> Self {
        Self {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            bind_fallback: false,
            publish_endpoint: false,
            data_dir_override: Some(data_dir),
            database_url: None,
            token_file_override: None,
            save_dir_seed: save_dir_seed
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            demo_url: None,
        }
    }
}

/// 解析 `FLUXDOWN_DAEMON_BIND`：未设置时用默认地址并允许端口回退；显式设置则严格 loopback、不回退。
fn resolve_bind(
    value: Result<String, std::env::VarError>,
) -> Result<(SocketAddr, bool), ConfigError> {
    let (text, fallback) = match value {
        Ok(text) => (text, false),
        Err(std::env::VarError::NotPresent) => (DEFAULT_DAEMON_BIND.to_owned(), true),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(ConfigError::InvalidBind(
                "value is not valid Unicode".to_owned(),
            ));
        }
    };
    let addr = text
        .parse::<SocketAddr>()
        .map_err(|error| ConfigError::InvalidBind(error.to_string()))?;
    if !addr.ip().is_loopback() {
        return Err(ConfigError::NonLoopback(addr.ip()));
    }
    Ok((addr, fallback))
}

/// 演示模式 URL：`FLUXDOWN_DEMO_URL` 优先；否则 `FLUXDOWN_DEMO` 为真值时用内置演示文件
/// （`http://127.0.0.1:<FLUXDOWN_BIND 端口>/demo/file`，由 agent 的 `/demo/file` 托管）。
fn demo_url_from_env() -> Option<String> {
    let url = std::env::var("FLUXDOWN_DEMO_URL")
        .ok()
        .as_deref()
        .and_then(parse_demo_url);
    url.or_else(|| {
        std::env::var("FLUXDOWN_DEMO")
            .is_ok_and(|value| flag_truthy(&value))
            .then(|| {
                let bind = std::env::var("FLUXDOWN_BIND").unwrap_or_default();
                builtin_demo_url(&bind)
            })
    })
}

/// 归一化 `FLUXDOWN_DEMO_URL`：去掉首尾空白与误带的包裹引号（Windows cmd 的
/// `set X="v" && …` 会把引号和尾部空格一并写进值），归一化后为空视为未开启。
fn parse_demo_url(raw: &str) -> Option<String> {
    let mut value = raw.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            value = value[1..value.len() - 1].trim();
        }
    }
    (!value.is_empty()).then(|| value.to_owned())
}

fn flag_truthy(value: &str) -> bool {
    matches!(
        value
            .trim()
            .trim_matches(['"', '\''])
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// 内置演示 URL：回环访问同进程组托管的 `/demo/file`，端口取 `bind` 末段，缺省 17800。
fn builtin_demo_url(bind: &str) -> String {
    let port = bind
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(17800);
    format!("http://127.0.0.1:{port}/demo/file")
}

/// 演示模式守卫：`demo_url` 已设置且请求 URL 与之不符时返回 `false`（trim 后精确比较，
/// 前缀 / 追加查询串不放行；空 URL 的种子任务在演示模式下同样拒绝）。
#[must_use]
pub fn demo_allows(demo_url: Option<&str>, url: &str) -> bool {
    demo_url.is_none_or(|allowed| url.trim() == allowed)
}

/// 校验并规范化客户端可写的 daemon 设置（键表与值域唯一来源：
/// [`fluxdown_protocol::DAEMON_CONFIG_FIELDS`]）。
///
/// 规范化后的布尔值再按引擎落库编码转写：`bt_seed_enabled` /
/// `bt_auto_reseed` 在引擎侧按 `"0"` 判定关闭（保持既有 `'1'`/`'0'` 编码），
/// 若原样写入 `"false"` 引擎会视为开启。
pub fn validate_config_patch(
    values: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let mut normalized = normalize_daemon_config_patch(values).map_err(|error| match error {
        DaemonConfigError::UnknownField(key) => ConfigError::UnknownField(key),
        DaemonConfigError::ReadOnly(key) => ConfigError::ReadOnlyField(key),
        DaemonConfigError::InvalidValue { field, message } => {
            ConfigError::InvalidValue { field, message }
        }
    })?;
    for key in ENGINE_NUMERIC_BOOL_KEYS {
        if let Some(value) = normalized.get_mut(*key) {
            *value = if value == "true" { "1" } else { "0" }.to_owned();
        }
    }
    Ok(normalized)
}

/// 引擎以 `"1"`/`"0"` 读取的布尔键（见 `bt_downloader` / `download_manager`
/// 中的 `get_config` 判定）。
const ENGINE_NUMERIC_BOOL_KEYS: &[&str] = &["bt_seed_enabled", "bt_auto_reseed"];

/// 从完整 engine config 中只投影 daemon UI 可见的键（含只读键，如
/// `bt_tracker_sub_cache` / `domain_conn_caps`；daemon 私有键与凭据表不投影）。
#[must_use]
pub fn public_config_values(all: &HashMap<String, String>) -> BTreeMap<String, String> {
    all.iter()
        .filter(|(key, _)| is_public_daemon_config_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// 从持久化配置构建完整 BT 运行配置。
///
/// `bt_seed_time_limit_minutes` / `bt_seed_inactive_time_limit_minutes` 落库
/// 时已是分钟；`*_unit` 键仅记录设置页的展示单位，
/// 引擎 [`fluxdown_engine::bt_config::BtConfig`] 直接取分钟值。
/// `bt_seed_enabled` / `bt_auto_reseed` 不在 `BtConfig` 内：引擎在完成 /
/// 启动时实时读库，落库即生效。
#[must_use]
pub fn bt_config_from_map(cfg: &HashMap<String, String>) -> fluxdown_engine::bt_config::BtConfig {
    use fluxdown_engine::bt_config::BtMseMode;
    use fluxdown_engine::bt_seeding::SeedingLimitOperator;

    let subscription_enabled = cfg
        .get("bt_tracker_sub_enabled")
        .map(|value| value == "true")
        .unwrap_or(true);
    fluxdown_engine::bt_config::BtConfig {
        enabled: cfg
            .get("bt_enabled")
            .map(|value| value == "true")
            .unwrap_or(true),
        enable_dht: cfg
            .get("bt_enable_dht")
            .map(|value| value == "true")
            .unwrap_or(true),
        enable_upnp: cfg
            .get("bt_enable_upnp")
            .map(|value| value == "true")
            .unwrap_or(true),
        enable_utp: cfg
            .get("bt_enable_utp")
            .map(|value| value == "true")
            .unwrap_or(true),
        port_start: cfg
            .get("bt_port_start")
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(6881),
        port_end: cfg
            .get("bt_port_end")
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(6891),
        custom_trackers: cfg.get("bt_custom_trackers").cloned().unwrap_or_default(),
        subscription_trackers: if subscription_enabled {
            cfg.get("bt_tracker_sub_cache").cloned().unwrap_or_default()
        } else {
            String::new()
        },
        seed_ratio_limit: cfg
            .get("bt_seed_ratio_limit")
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0),
        seed_post_ratio_limit: cfg
            .get("bt_seed_post_ratio_limit")
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0),
        seed_time_limit_minutes: cfg
            .get("bt_seed_time_limit_minutes")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0),
        seed_inactive_time_limit_minutes: cfg
            .get("bt_seed_inactive_time_limit_minutes")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0),
        seed_limit_operator: cfg
            .get("bt_seed_limit_operator")
            .map(|value| {
                if value.eq_ignore_ascii_case("and") {
                    SeedingLimitOperator::And
                } else {
                    SeedingLimitOperator::Or
                }
            })
            .unwrap_or(SeedingLimitOperator::Or),
        seed_then_action: cfg
            .get("bt_seed_then_action")
            .cloned()
            .unwrap_or_else(|| "stop".to_owned()),
        seed_max_active: cfg
            .get("bt_seed_max_active")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0),
        mse_mode: cfg
            .get("bt_mse_mode")
            .map(String::as_str)
            .map(BtMseMode::from)
            .unwrap_or_default(),
    }
}

/// 从持久化配置构建 BT 防吸血配置（`bt_guard_*` 键，值域见 `fluxdown_protocol::DAEMON_CONFIG_FIELDS`）。
///
/// 缺失或无法解析的值回落到 [`BtGuardConfig::default`]（即契约默认值）；写入路径已由
/// [`validate_config_patch`] 校验，这里只防御旧库 / 手改库里的脏值。全部键热生效：
/// 不在 `invalidate_bt_session` 的监听类键之列，不会重建 BT 会话。
#[cfg(feature = "bt")]
#[must_use]
pub fn bt_guard_config_from_map(cfg: &HashMap<String, String>) -> BtGuardConfig {
    let defaults = BtGuardConfig::default();
    let flag = |key: &str, default: bool| -> bool {
        match cfg.get(key).map(|value| value.trim()) {
            Some("true" | "1") => true,
            Some("false" | "0") => false,
            _ => default,
        }
    };
    let number = |key: &str, min: u64, max: u64| -> Option<u64> {
        cfg.get(key)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| (min..=max).contains(value))
    };
    let rule = |key: &str, default: RuleMode| -> RuleMode {
        cfg.get(key)
            .and_then(|value| value.trim().parse::<RuleMode>().ok())
            .unwrap_or(default)
    };
    // 数值范围与 `daemon_config.rs` 键目录一致；范围外视为脏值回落默认。
    let as_u32 = |value: Option<u64>, default: u32| {
        value
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(default)
    };
    BtGuardConfig {
        enabled: flag("bt_guard_enabled", defaults.enabled),
        rule_excessive: rule("bt_guard_rule_excessive", defaults.rule_excessive),
        rule_rerequest: rule("bt_guard_rule_rerequest", defaults.rule_rerequest),
        rule_rewind: rule("bt_guard_rule_rewind", defaults.rule_rewind),
        rule_lag: rule("bt_guard_rule_lag", defaults.rule_lag),
        rule_fingerprint: rule("bt_guard_rule_fingerprint", defaults.rule_fingerprint),
        cloud_mode: rule("bt_guard_cloud_mode", defaults.cloud_mode),
        excess_ratio_pct: as_u32(
            number("bt_guard_excess_ratio_pct", 110, 1000),
            defaults.excess_ratio_pct,
        ),
        min_torrent_mib: number("bt_guard_min_torrent_mib", 0, 1_048_576)
            .unwrap_or(defaults.min_torrent_mib),
        rewind_tolerance_pct: as_u32(
            number("bt_guard_rewind_tolerance_pct", 0, 100),
            defaults.rewind_tolerance_pct,
        ),
        throttle_kib: as_u32(
            number("bt_guard_throttle_kib", 1, 1_048_576),
            defaults.throttle_kib,
        ),
        ban_base_hours: as_u32(
            number("bt_guard_ban_base_hours", 1, 720),
            defaults.ban_base_hours,
        ),
        ban_max_days: as_u32(
            number("bt_guard_ban_max_days", 1, 365),
            defaults.ban_max_days,
        ),
        downloading_max_action: cfg
            .get("bt_guard_downloading_max_action")
            .and_then(|value| value.trim().parse::<DownloadingMaxAction>().ok())
            .unwrap_or(defaults.downloading_max_action),
        skip_private: flag("bt_guard_skip_private", defaults.skip_private),
        skip_lan: flag("bt_guard_skip_lan", defaults.skip_lan),
        ipv6_prefix: number("bt_guard_ipv6_prefix", 48, 128)
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(defaults.ipv6_prefix),
        filter_urls: cfg.get("bt_guard_filter_urls").cloned().unwrap_or_default(),
        cloud_enabled: flag("bt_guard_cloud_enabled", defaults.cloud_enabled),
        report_enabled: flag("bt_guard_report_enabled", defaults.report_enabled),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::{ConfigError, public_config_values, validate_config_patch};

    #[test]
    fn loopback_predicate_rejects_public_bind() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 17801);
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 17801);
        assert!(loopback.ip().is_loopback());
        assert!(!public.ip().is_loopback());
    }

    #[test]
    fn embedded_config_uses_an_ephemeral_loopback_port_and_no_environment() {
        let dir = std::path::PathBuf::from("data");
        let config = super::DaemonConfig::embedded(dir.clone(), Some("  /downloads ".to_owned()));
        assert!(config.bind_addr.ip().is_loopback());
        assert_eq!(config.bind_addr.port(), 0);
        assert_eq!(config.data_dir_override, Some(dir));
        assert_eq!(config.save_dir_seed.as_deref(), Some("/downloads"));
        assert!(config.database_url.is_none());
        assert!(config.token_file_override.is_none());
        assert!(config.demo_url.is_none());

        let blank =
            super::DaemonConfig::embedded(std::path::PathBuf::from("data"), Some(" ".into()));
        assert!(blank.save_dir_seed.is_none());
    }

    #[test]
    fn unset_bind_uses_default_with_fallback_while_explicit_bind_is_strict() {
        use std::env::VarError;
        let (addr, fallback) =
            super::resolve_bind(Err(VarError::NotPresent)).expect("default bind");
        assert_eq!(addr.to_string(), super::DEFAULT_DAEMON_BIND);
        assert!(fallback);

        let (addr, fallback) =
            super::resolve_bind(Ok("127.0.0.1:17801".to_owned())).expect("explicit bind");
        assert_eq!(addr.port(), 17801);
        assert!(!fallback, "an explicit address must never fall back");

        assert!(matches!(
            super::resolve_bind(Ok("0.0.0.0:17801".to_owned())),
            Err(ConfigError::NonLoopback(_))
        ));
        assert!(matches!(
            super::resolve_bind(Ok("not-an-address".to_owned())),
            Err(ConfigError::InvalidBind(_))
        ));
        assert!(matches!(
            super::resolve_bind(Err(VarError::NotUnicode(std::ffi::OsString::from("x")))),
            Err(ConfigError::InvalidBind(_))
        ));
    }

    #[test]
    fn embedded_config_never_falls_back_or_publishes_an_endpoint() {
        let config = super::DaemonConfig::embedded(std::path::PathBuf::from("data"), None);
        assert!(!config.bind_fallback);
        assert!(!config.publish_endpoint);
    }

    #[test]
    fn sync_catalog_semantics_normalize_without_losing_flutter_values() {
        let values = BTreeMap::from([
            ("auto_max_connections".to_owned(), "0".to_owned()),
            ("max_auto_retries".to_owned(), "-1".to_owned()),
            ("auto_resume_on_start".to_owned(), "true".to_owned()),
            ("ed2k_enable_upnp".to_owned(), "false".to_owned()),
            ("ed2k_server_list".to_owned(), "ed2k://server".to_owned()),
            ("bt_seed_then_action".to_owned(), "delete_files".to_owned()),
            ("bt_mse_mode".to_owned(), "enabled".to_owned()),
        ]);
        let normalized = validate_config_patch(&values).expect("valid synced config");
        assert_eq!(normalized, values);
    }

    #[test]
    fn catalog_keys_new_to_daemon_are_accepted_and_engine_encoded() {
        let values = BTreeMap::from([
            ("bt_seed_enabled".to_owned(), "false".to_owned()),
            ("bt_auto_reseed".to_owned(), "1".to_owned()),
            ("bt_seed_time_limit_unit".to_owned(), "hours".to_owned()),
            (
                "bt_seed_inactive_time_limit_unit".to_owned(),
                "days".to_owned(),
            ),
            ("ed2k_listen_port".to_owned(), " 4662 ".to_owned()),
            ("default_queue_id".to_owned(), "later".to_owned()),
        ]);
        let normalized = validate_config_patch(&values).expect("catalog keys accepted");
        assert_eq!(
            normalized["bt_seed_enabled"], "0",
            "engine reads `!= \"0\"`"
        );
        assert_eq!(normalized["bt_auto_reseed"], "1");
        assert_eq!(normalized["bt_seed_time_limit_unit"], "hours");
        assert_eq!(normalized["bt_seed_inactive_time_limit_unit"], "days");
        assert_eq!(normalized["ed2k_listen_port"], "4662");
        assert_eq!(normalized["default_queue_id"], "later");
    }

    #[test]
    fn read_only_and_unknown_keys_are_rejected_on_patch() {
        for key in [
            "domain_conn_caps",
            "bt_tracker_sub_cache",
            "bt_tracker_sub_updated_at",
            "ed2k_server_sub_cache",
        ] {
            let patch = BTreeMap::from([(key.to_owned(), String::new())]);
            assert!(
                matches!(validate_config_patch(&patch), Err(ConfigError::ReadOnlyField(k)) if k == key),
                "{key} must be rejected as read-only"
            );
        }
        for key in ["site_auth_credentials", "daemon_config_revision", "nope"] {
            let patch = BTreeMap::from([(key.to_owned(), "x".to_owned())]);
            assert!(
                matches!(validate_config_patch(&patch), Err(ConfigError::UnknownField(k)) if k == key),
                "{key} must be rejected as unknown"
            );
        }
        let bad = BTreeMap::from([("ed2k_listen_port".to_owned(), "70000".to_owned())]);
        assert!(matches!(
            validate_config_patch(&bad),
            Err(ConfigError::InvalidValue { field, .. }) if field == "ed2k_listen_port"
        ));
    }

    #[test]
    fn component_mirror_base_patch_rejects_plain_http_with_field_name() {
        let insecure = BTreeMap::from([(
            "component_mirror_base".to_owned(),
            "http://gh-proxy.example.com".to_owned(),
        )]);
        assert!(matches!(
            validate_config_patch(&insecure),
            Err(ConfigError::InvalidValue { field, .. }) if field == "component_mirror_base"
        ));
        let secure = BTreeMap::from([(
            "component_mirror_base".to_owned(),
            "https://gh-proxy.example.com/".to_owned(),
        )]);
        let normalized = validate_config_patch(&secure).expect("https mirror accepted");
        assert_eq!(
            normalized["component_mirror_base"],
            "https://gh-proxy.example.com"
        );
    }

    #[test]
    fn public_projection_keeps_read_only_keys_and_hides_private_ones() {
        let all = HashMap::from([
            ("max_concurrent_tasks".to_owned(), "5".to_owned()),
            ("bt_tracker_sub_cache".to_owned(), "udp://t".to_owned()),
            ("domain_conn_caps".to_owned(), "v3".to_owned()),
            ("proxy_password".to_owned(), "secret".to_owned()),
            ("site_auth_credentials".to_owned(), "{}".to_owned()),
            ("daemon_config_revision".to_owned(), "7".to_owned()),
            ("daemon_migration_link_acked".to_owned(), "1".to_owned()),
        ]);
        let public = public_config_values(&all);
        assert_eq!(public["max_concurrent_tasks"], "5");
        assert_eq!(public["bt_tracker_sub_cache"], "udp://t");
        assert_eq!(public["domain_conn_caps"], "v3");
        assert_eq!(public["proxy_password"], "secret");
        assert!(!public.contains_key("site_auth_credentials"));
        assert!(!public.contains_key("daemon_config_revision"));
        assert!(!public.contains_key("daemon_migration_link_acked"));
    }

    #[test]
    fn bt_config_from_map_parses_bt_enabled() {
        let mut map = std::collections::HashMap::new();
        assert!(super::bt_config_from_map(&map).enabled, "defaults to true");

        map.insert("bt_enabled".to_string(), "false".to_string());
        assert!(!super::bt_config_from_map(&map).enabled, "explicit false");

        map.insert("bt_enabled".to_string(), "true".to_string());
        assert!(super::bt_config_from_map(&map).enabled, "explicit true");
    }

    fn patch(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn guard_patch_is_normalized_but_not_recoded_to_engine_numeric_booleans() {
        let normalized = validate_config_patch(&patch(&[
            ("bt_guard_enabled", "0"),
            ("bt_guard_rule_lag", " enforce "),
            ("bt_guard_ipv6_prefix", " 56 "),
            ("bt_guard_report_enabled", "1"),
            (
                "bt_guard_filter_urls",
                "  https://a.test/x\nhttps://b.test/y  ",
            ),
        ]))
        .expect("valid guard patch");
        assert_eq!(normalized["bt_guard_enabled"], "false");
        assert_eq!(normalized["bt_guard_rule_lag"], "enforce");
        assert_eq!(normalized["bt_guard_ipv6_prefix"], "56");
        assert_eq!(normalized["bt_guard_report_enabled"], "true");
        assert_eq!(
            normalized["bt_guard_filter_urls"],
            "https://a.test/x\nhttps://b.test/y"
        );
    }

    #[test]
    fn guard_patch_rejects_bad_values() {
        for (key, value) in [
            ("bt_guard_excess_ratio_pct", "109"),
            ("bt_guard_ipv6_prefix", "47"),
            ("bt_guard_ipv6_prefix", "129"),
            ("bt_guard_throttle_kib", "0"),
            ("bt_guard_rule_rewind", "ban"),
            ("bt_guard_downloading_max_action", "enforce"),
            ("bt_guard_enabled", "yes"),
        ] {
            assert!(
                matches!(
                    validate_config_patch(&patch(&[(key, value)])),
                    Err(ConfigError::InvalidValue { .. })
                ),
                "{key}={value}"
            );
        }
        // 任一键非法即整体失败，合法键不落库。
        assert!(
            validate_config_patch(&patch(&[
                ("bt_guard_enabled", "true"),
                ("bt_guard_ban_max_days", "366"),
            ]))
            .is_err()
        );
    }

    #[test]
    fn guard_filter_urls_are_visible_in_the_public_config() {
        let mut all = std::collections::HashMap::new();
        all.insert(
            "bt_guard_filter_urls".to_owned(),
            "https://a.test".to_owned(),
        );
        let public = public_config_values(&all);
        assert!(public.contains_key("bt_guard_filter_urls"));
    }

    #[cfg(feature = "bt")]
    #[test]
    fn guard_config_defaults_match_the_contract() {
        let cfg = super::bt_guard_config_from_map(&std::collections::HashMap::new());
        assert_eq!(cfg, fluxdown_engine::bt_guard::BtGuardConfig::default());
        assert!(cfg.enabled);
        assert_eq!(cfg.rule_excessive.as_str(), "observe");
        assert_eq!(cfg.cloud_mode.as_str(), "observe");
        assert_eq!(cfg.excess_ratio_pct, 200);
        assert_eq!(cfg.min_torrent_mib, 64);
        assert_eq!(cfg.rewind_tolerance_pct, 7);
        assert_eq!(cfg.throttle_kib, 32);
        assert_eq!(cfg.ban_base_hours, 24);
        assert_eq!(cfg.ban_max_days, 30);
        assert_eq!(cfg.downloading_max_action.as_str(), "throttle");
        assert!(cfg.skip_private && cfg.skip_lan && cfg.cloud_enabled);
        assert_eq!(cfg.ipv6_prefix, 64);
        assert!(cfg.filter_urls.is_empty());
        assert!(!cfg.report_enabled);
    }

    #[cfg(feature = "bt")]
    #[test]
    fn guard_config_maps_every_key_from_a_validated_patch() {
        let normalized = validate_config_patch(&patch(&[
            ("bt_guard_enabled", "false"),
            ("bt_guard_rule_excessive", "enforce"),
            ("bt_guard_rule_rerequest", "off"),
            ("bt_guard_rule_rewind", "enforce"),
            ("bt_guard_rule_lag", "off"),
            ("bt_guard_rule_fingerprint", "enforce"),
            ("bt_guard_cloud_mode", "enforce"),
            ("bt_guard_excess_ratio_pct", "350"),
            ("bt_guard_min_torrent_mib", "0"),
            ("bt_guard_rewind_tolerance_pct", "15"),
            ("bt_guard_throttle_kib", "128"),
            ("bt_guard_ban_base_hours", "2"),
            ("bt_guard_ban_max_days", "9"),
            ("bt_guard_downloading_max_action", "ban"),
            ("bt_guard_skip_private", "false"),
            ("bt_guard_skip_lan", "false"),
            ("bt_guard_ipv6_prefix", "48"),
            ("bt_guard_filter_urls", "https://a.test/x"),
            ("bt_guard_cloud_enabled", "false"),
            ("bt_guard_report_enabled", "true"),
        ]))
        .expect("valid patch");
        let all: std::collections::HashMap<String, String> = normalized.into_iter().collect();
        let cfg = super::bt_guard_config_from_map(&all);
        assert!(!cfg.enabled);
        assert_eq!(cfg.rule_excessive.as_str(), "enforce");
        assert_eq!(cfg.rule_rerequest.as_str(), "off");
        assert_eq!(cfg.rule_rewind.as_str(), "enforce");
        assert_eq!(cfg.rule_lag.as_str(), "off");
        assert_eq!(cfg.rule_fingerprint.as_str(), "enforce");
        assert_eq!(cfg.cloud_mode.as_str(), "enforce");
        assert_eq!(cfg.excess_ratio_pct, 350);
        assert_eq!(cfg.min_torrent_mib, 0);
        assert_eq!(cfg.rewind_tolerance_pct, 15);
        assert_eq!(cfg.throttle_kib, 128);
        assert_eq!(cfg.ban_base_hours, 2);
        assert_eq!(cfg.ban_max_days, 9);
        assert_eq!(cfg.downloading_max_action.as_str(), "ban");
        assert!(!cfg.skip_private && !cfg.skip_lan && !cfg.cloud_enabled);
        assert_eq!(cfg.ipv6_prefix, 48);
        assert_eq!(cfg.filter_urls, "https://a.test/x");
        assert!(cfg.report_enabled);
    }

    #[cfg(feature = "bt")]
    #[test]
    fn guard_config_falls_back_to_defaults_on_dirty_stored_values() {
        let dirty: std::collections::HashMap<String, String> = [
            ("bt_guard_enabled", "maybe"),
            ("bt_guard_rule_lag", "ban"),
            ("bt_guard_excess_ratio_pct", "5"),
            ("bt_guard_min_torrent_mib", "-1"),
            ("bt_guard_throttle_kib", "0"),
            ("bt_guard_ban_max_days", "99999"),
            ("bt_guard_ipv6_prefix", "300"),
            ("bt_guard_downloading_max_action", "off"),
            ("bt_guard_rewind_tolerance_pct", "abc"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(
            super::bt_guard_config_from_map(&dirty),
            fluxdown_engine::bt_guard::BtGuardConfig::default()
        );
    }
}

#[cfg(test)]
mod demo_tests {
    use super::{builtin_demo_url, demo_allows, parse_demo_url};

    const DEMO: &str = "https://example.com/demo.bin";

    #[test]
    fn demo_url_strips_whitespace_and_wrapping_quotes() {
        let want = Some(DEMO.to_owned());
        assert_eq!(parse_demo_url(DEMO), want);
        assert_eq!(parse_demo_url("\"https://example.com/demo.bin\" "), want);
        assert_eq!(parse_demo_url("'https://example.com/demo.bin'"), want);
        assert_eq!(parse_demo_url("  "), None);
        assert_eq!(parse_demo_url("\"\""), None);
    }

    #[test]
    fn builtin_demo_url_uses_bind_port_over_loopback() {
        assert_eq!(
            builtin_demo_url("0.0.0.0:17800"),
            "http://127.0.0.1:17800/demo/file"
        );
        assert_eq!(
            builtin_demo_url("[::]:9000"),
            "http://127.0.0.1:9000/demo/file"
        );
        assert_eq!(builtin_demo_url(""), "http://127.0.0.1:17800/demo/file");
    }

    #[test]
    fn demo_guard_is_exact_match_after_trim() {
        assert!(demo_allows(None, "https://evil.example/anything.iso"));
        assert!(demo_allows(Some(DEMO), DEMO));
        assert!(demo_allows(Some(DEMO), &format!("  {DEMO}\n")));
        for url in [
            "https://example.com/other.bin",
            "https://example.com/demo.bin?x=1",
            "https://example.com/demo.bin/../secret",
            "",
        ] {
            assert!(!demo_allows(Some(DEMO), url), "{url:?} must be rejected");
        }
    }
}
