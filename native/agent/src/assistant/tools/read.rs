//! 只读工具:应用 / 设置 / 运行时 / 网络 / 插件 / 组件 / 体检 / 网卡 / 测速。
//!
//! 每个函数对应云端目录里的一个客户端工具(注册表见 `mod.rs`):参数按目录 Schema 解析,多余字段
//! 一律拒绝;结果是 JSON,脱敏、去主目录、截断由 `mod.rs` 统一收尾。这里仍按字段做最小化:
//! 路径只给 `~` 形式、代理只给不含凭据的端点、设置里的敏感键直接打码。
//!
//! RPC 对应:
//! - `fd_get_app_info`:agent 事件投影(版本 / 更新 / 外壳)+ `daemon.diagnostics.describe`;
//! - `fd_get_settings`:`daemon.config.get` + agent 事件投影里的偏好;
//! - `fd_get_runtime_stats`:`daemon.runtime.stats` + `daemon.config.get` + 事件投影里的队列;
//! - `fd_get_network_status`:`daemon.config.get` + `daemon.config.systemProxy` + 事件投影;
//! - `fd_list_plugins`:`daemon.plugin.list`;
//! - `fd_list_components`:`daemon.component.get` + `daemon.component.listVersions`;
//! - `fd_run_diagnostics`:`agent.diagnostics.run`;
//! - `fd_get_net_interfaces`:`daemon.net.interfaces`;
//! - `fd_measure_routes`:`daemon.net.measureRoutes`。

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use fluxdown_protocol::net_diag::{
    NET_MEASURE_MAX_CONNECTIONS, NET_MEASURE_MAX_MEGABYTES, NET_MEASURE_MAX_ROUTES,
    NET_MEASURE_MAX_SECONDS, NET_MEASURE_MIN_SECONDS,
};
use fluxdown_protocol::proxy_rules::{parse_proxy_rules, proxy_display_endpoint};
use fluxdown_protocol::{
    APP_VERSION, AgentPreferencesDto, ComponentKind, ComponentStatusDto, ComponentVersions,
    DAEMON_CONFIG_FIELDS, DaemonConfigSnapshot, DaemonRuntimeStatsDto, DiagnosticLevel,
    DiagnosticsReportDto, NetInterfacesDto, NetMeasureParams, NetMeasureResultDto,
    NetRouteMeasureDto, PROTOCOL_VERSION, PluginDto, QueueDto, SYNC_SETTING_SPECS, ServiceHello,
    SettingOwner, SystemProxyDto, UpdateStatusDto, daemon_config_default, daemon_config_field,
    method, setting_spec,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::output::{display_path, iso_millis, truncate_chars, url_host};
use super::settings::{denied_setting_reason, display_setting_value, wire_to_json};
use super::tasks::{check_max, invalid};
use super::{ToolContext, ToolError, expect_no_args, parse_args, to_json};

/// 桌面外壳(GPUI)构建带 `desktop` feature,headless / server 构建没有。
const CLIENT_KIND: &str = if cfg!(feature = "desktop") {
    "desktop"
} else {
    "headless"
};
const BUILD_PROFILE: &str = if cfg!(debug_assertions) {
    "debug"
} else {
    "release"
};

/// agent 进程里助手服务的起点:`fd_get_app_info` 的运行时长从这里算(助手在 agent 启动时构造)。
static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);

/// 记下运行时长的起点;重复调用无副作用。
pub(super) fn mark_started() {
    LazyLock::force(&STARTED);
}

/// 设置值被打码时的占位。
const MASKED: &str = "***";
/// `keys` 过滤项上限(云端 Schema `maxItems`)。
const MAX_SETTING_FILTERS: usize = 100;
const MAX_SETTING_KEY_CHARS: usize = 256;
/// 偏好里字符串值的展示上限。
const MAX_PREFERENCE_TEXT_CHARS: usize = 512;
const MAX_SITE_RULES: usize = 20;
const MAX_RULE_TEXT_CHARS: usize = 120;
const MAX_PLUGINS: usize = 200;
const MAX_PLUGIN_RESOLVER_URLS: usize = 8;
const MAX_DIAGNOSTIC_DETAIL_CHARS: usize = 400;
const MAX_ERROR_TEXT_CHARS: usize = 300;
/// 查询托管组件的最新版本(走用户自己的出口访问发布源)的单次时限。
const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(8);

// ───────────────────────── 通用辅助 ─────────────────────────

/// daemon 配置里某键的当前 wire 值(未持久化时取规范目录的默认值)。
pub(super) fn config_text<'a>(config: &'a DaemonConfigSnapshot, key: &str) -> &'a str {
    config
        .values
        .get(key)
        .map_or_else(|| daemon_config_default(key), String::as_str)
}

/// 某键的带类型值(布尔 / 整数 / 小数 / 文本),与 `fd_update_settings` 返回的形态一致。
pub(super) fn config_json(config: &DaemonConfigSnapshot, key: &str) -> Value {
    daemon_config_field(key).map_or(Value::Null, |field| {
        wire_to_json(field.kind, config_text(config, key))
    })
}

pub(super) fn config_flag(config: &DaemonConfigSnapshot, key: &str) -> bool {
    matches!(config_text(config, key), "true" | "1")
}

/// 空串 → `null`,免得结果里堆满 `""`。
pub(super) fn text_or_null(text: &str) -> Value {
    if text.is_empty() {
        Value::Null
    } else {
        Value::String(text.to_owned())
    }
}

/// 错误 / 说明类文本的展示:截断到固定字符数。
pub(super) fn short_text(text: &str) -> Value {
    if text.is_empty() {
        Value::Null
    } else {
        Value::String(truncate_chars(text, MAX_ERROR_TEXT_CHARS))
    }
}

// ───────────────────────── fd_get_app_info ─────────────────────────

/// 从 agent 事件投影里取出的本机事实。
struct LocalFacts {
    daemon_connected: bool,
    tasks: Value,
    update: UpdateStatusDto,
    resident: bool,
    tray_available: bool,
}

/// `daemon.diagnostics.describe` 里用得到的部分。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonDescribe {
    service: ServiceHello,
    #[serde(default)]
    queues: u64,
    #[serde(default)]
    groups: u64,
    #[serde(default)]
    config_revision: u64,
}

pub(super) async fn app_info(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    expect_no_args(args)?;
    let local = ctx.inspect(|snapshot| LocalFacts {
        daemon_connected: snapshot.daemon_connected,
        tasks: snapshot
            .daemon
            .task_total
            .map_or_else(|| Value::from(snapshot.daemon.tasks.len()), Value::from),
        update: snapshot.update.clone(),
        resident: snapshot.shell.resident,
        tray_available: snapshot.shell.tray_available,
    });
    let daemon = match ctx
        .call_as::<DaemonDescribe>(method::DAEMON_DIAGNOSTICS_DESCRIBE, json!({}))
        .await
    {
        Ok(describe) => json!({
            "connected": local.daemon_connected,
            "version": describe.service.service_version,
            "protocolVersion": describe.service.protocol_version,
            "tasks": local.tasks,
            "queues": describe.queues,
            "groups": describe.groups,
            "configRevision": describe.config_revision,
        }),
        Err(error) => json!({
            "connected": local.daemon_connected,
            "tasks": local.tasks,
            "error": error.to_string(),
        }),
    };
    Ok(json!({
        "app": {
            "version": APP_VERSION,
            "protocolVersion": PROTOCOL_VERSION,
            "clientKind": CLIENT_KIND,
            "buildProfile": BUILD_PROFILE,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "agentUptimeSecs": STARTED.elapsed().as_secs(),
            "residentInBackground": local.resident,
            "trayAvailable": local.tray_available,
        },
        "daemon": daemon,
        "update": update_json(&local.update)?,
    }))
}

fn update_json(update: &UpdateStatusDto) -> Result<Value, ToolError> {
    let checked_at_ms = i64::try_from(update.checked_at_ms).unwrap_or(0);
    Ok(json!({
        "phase": to_json("update phase", &update.phase)?,
        "channel": text_or_null(&update.channel),
        "installKind": to_json("update install kind", &update.install_kind)?,
        "latestVersion": text_or_null(&update.latest_version),
        "hasUpdate": update.has_update,
        "manualUpgradeReason": to_json("update manual reason", &update.manual_reason)?,
        "failure": to_json("update failure", &update.failure)?,
        "lastCheckedAt": iso_millis(checked_at_ms),
    }))
}

// ───────────────────────── fd_get_settings ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    #[serde(default)]
    keys: Option<Vec<String>>,
}

/// 一个键过滤项:精确键,或以 `*` 结尾的前缀。
#[derive(Debug, PartialEq, Eq)]
struct KeyFilter {
    pattern: String,
    prefix: bool,
}

impl KeyFilter {
    fn matches(&self, key: &str) -> bool {
        if self.prefix {
            key.starts_with(&self.pattern)
        } else {
            key == self.pattern
        }
    }

    /// 没有命中任何键时给模型的说明;同步目录里的 daemon 键别名给出规范键。
    fn describe_unmatched(&self) -> String {
        if self.prefix {
            return format!("{}*", self.pattern);
        }
        if let Some(spec) = setting_spec(&self.pattern)
            && spec.owner == SettingOwner::Daemon
            && spec.storage_key != self.pattern
        {
            return format!(
                "{} (a download setting; use the key '{}')",
                self.pattern, spec.storage_key
            );
        }
        self.pattern.clone()
    }
}

fn parse_filters(keys: Vec<String>) -> Result<Vec<KeyFilter>, ToolError> {
    if keys.len() > MAX_SETTING_FILTERS {
        return Err(invalid(format!(
            "keys must not contain more than {MAX_SETTING_FILTERS} entries"
        )));
    }
    let mut filters = Vec::with_capacity(keys.len());
    for key in keys {
        let key = key.trim();
        if key.is_empty() {
            return Err(invalid("keys must not contain empty entries"));
        }
        check_max("keys[]", key, MAX_SETTING_KEY_CHARS)?;
        let (pattern, prefix) = match key.strip_suffix('*') {
            Some(pattern) => (pattern, true),
            None => (key, false),
        };
        if pattern.contains('*') {
            return Err(invalid("'*' is only allowed at the end of a key prefix"));
        }
        filters.push(KeyFilter {
            pattern: pattern.to_owned(),
            prefix,
        });
    }
    Ok(filters)
}

/// 命中过滤项的设置(平铺的 键 → 值),敏感键值已打码。
#[derive(Default)]
struct SettingsView {
    settings: BTreeMap<String, Value>,
    redacted: Vec<String>,
}

impl SettingsView {
    fn put(&mut self, key: &str, value: Value) {
        if denied_setting_reason(key).is_some() {
            self.redacted.push(key.to_owned());
            self.settings
                .insert(key.to_owned(), Value::String(MASKED.to_owned()));
        } else {
            self.settings
                .insert(key.to_owned(), display_setting_value(key, value));
        }
    }
}

/// `filters` 为空 = 全选;命中时把命中的过滤项标记到 `hits`。
fn selected(filters: &[KeyFilter], hits: &mut [bool], key: &str) -> bool {
    if filters.is_empty() {
        return true;
    }
    let mut any = false;
    for (filter, hit) in filters.iter().zip(hits.iter_mut()) {
        if filter.matches(key) {
            *hit = true;
            any = true;
        }
    }
    any
}

fn preference_value(prefs: &AgentPreferencesDto, key: &str) -> Value {
    match prefs.values.get(key) {
        Some(Value::String(text)) => Value::String(truncate_chars(text, MAX_PREFERENCE_TEXT_CHARS)),
        Some(other) => other.clone(),
        // 未设置的偏好:应用默认值生效。
        None => Value::Null,
    }
}

fn settings_view(
    config: &DaemonConfigSnapshot,
    prefs: &AgentPreferencesDto,
    filters: &[KeyFilter],
) -> Value {
    let mut view = SettingsView::default();
    let mut hits = vec![false; filters.len()];
    for spec in SYNC_SETTING_SPECS {
        if !matches!(spec.owner, SettingOwner::Agent | SettingOwner::Preferences) {
            continue;
        }
        if selected(filters, &mut hits, spec.key) {
            view.put(spec.key, preference_value(prefs, spec.key));
        }
    }
    for field in DAEMON_CONFIG_FIELDS {
        if selected(filters, &mut hits, field.key) {
            view.put(field.key, config_json(config, field.key));
        }
    }
    let unmatched: Vec<String> = filters
        .iter()
        .zip(&hits)
        .filter(|(_, hit)| !**hit)
        .map(|(filter, _)| filter.describe_unmatched())
        .collect();
    let mut result = json!({
        "count": view.settings.len(),
        "settings": view.settings,
        "note": "null = not set, the app default applies; \"***\" = sensitive, or not changeable by the assistant",
    });
    if let Some(object) = result.as_object_mut() {
        if !view.redacted.is_empty() {
            object.insert("maskedKeys".to_owned(), json!(view.redacted));
        }
        if !unmatched.is_empty() {
            object.insert("unmatchedFilters".to_owned(), json!(unmatched));
        }
    }
    result
}

pub(super) async fn settings(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    let SettingsArgs { keys } = parse_args(args)?;
    let filters = parse_filters(keys.unwrap_or_default())?;
    let config = ctx.daemon_config().await?;
    let prefs = ctx.preferences().await?;
    Ok(settings_view(&config, &prefs, &filters))
}

// ───────────────────────── fd_get_runtime_stats ─────────────────────────

fn queue_json(queue: &QueueDto) -> Value {
    json!({
        "id": text_or_null(&queue.queue_id),
        "name": queue.name,
        "running": queue.is_running,
        "maxConcurrent": queue.max_concurrent,
        "speedLimitKBps": queue.speed_limit_kbps,
        "uploadLimitKBps": queue.upload_limit_kbps,
        "defaultSaveDir": text_or_null(&display_path(&queue.default_save_dir)),
        "scheduleEnabled": queue.schedule_enabled,
        "scheduleStart": text_or_null(&queue.schedule_start),
        "scheduleStop": text_or_null(&queue.schedule_stop),
    })
}

pub(super) async fn runtime_stats(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    expect_no_args(args)?;
    let stats: DaemonRuntimeStatsDto = ctx.call_as(method::DAEMON_RUNTIME_STATS, json!({})).await?;
    let config = ctx.daemon_config().await?;
    let queues: Vec<Value> =
        ctx.inspect(|snapshot| snapshot.daemon.queues.iter().map(queue_json).collect());
    Ok(json!({
        "activeTasks": stats.active_tasks,
        "pendingTasks": stats.pending_tasks,
        "retryPendingTasks": stats.retry_pending_tasks,
        "localWorkCount": stats.local_work_count,
        "downloadBytesPerSec": stats.total_download_bps,
        "uploadBytesPerSec": stats.total_upload_bps,
        "diskFreeBytes": stats.disk_free_bytes,
        "saveDir": text_or_null(&display_path(&stats.save_dir)),
        "limits": {
            "globalDownloadBytesPerSec": config_json(&config, "speed_limit_bytes"),
            "globalUploadBytesPerSec": config_json(&config, "upload_limit_bytes"),
            "maxConcurrentTasks": config_json(&config, "max_concurrent_tasks"),
            "defaultSegments": config_json(&config, "default_segments"),
            "note": "a limit of 0 means unlimited",
        },
        "queues": queues,
    }))
}

// ───────────────────────── fd_get_network_status ─────────────────────────

/// 协议级代理覆盖键 → 展示名。
const PROTOCOL_PROXY_KEYS: [(&str, &str); 6] = [
    ("proxy_protocol_http", "http"),
    ("proxy_protocol_ftp", "ftp"),
    ("proxy_protocol_hls", "hls"),
    ("proxy_protocol_dash", "dash"),
    ("proxy_protocol_bt", "bt"),
    ("proxy_protocol_ed2k", "ed2k"),
];

fn site_rules_json(raw: &str) -> Value {
    let rules = parse_proxy_rules(raw);
    let enabled = rules.iter().filter(|rule| rule.enabled).count();
    let items: Vec<Value> = rules
        .iter()
        .take(MAX_SITE_RULES)
        .map(|rule| {
            json!({
                "name": text_or_null(&truncate_chars(&rule.name, MAX_RULE_TEXT_CHARS)),
                "enabled": rule.enabled,
                "match": rule.match_kind,
                "pattern": truncate_chars(&rule.pattern, MAX_RULE_TEXT_CHARS),
                "action": rule.action,
                "proxy": text_or_null(&proxy_display_endpoint(&rule.proxy)),
            })
        })
        .collect();
    json!({
        "total": rules.len(),
        "enabled": enabled,
        "shown": items.len(),
        "rules": items,
    })
}

fn global_proxy_json(config: &DaemonConfigSnapshot) -> Value {
    let mode = config_text(config, "proxy_mode");
    let mut per_protocol = serde_json::Map::new();
    for (key, name) in PROTOCOL_PROXY_KEYS {
        let value = config_text(config, key);
        if !value.is_empty() {
            per_protocol.insert(
                name.to_owned(),
                Value::String(proxy_display_endpoint(value)),
            );
        }
    }
    json!({
        "mode": mode,
        "autoRouteEnabled": mode == "auto",
        "manualType": text_or_null(config_text(config, "proxy_type")),
        "manualHost": text_or_null(config_text(config, "proxy_host")),
        "manualPort": text_or_null(config_text(config, "proxy_port")),
        "manualHasLogin": !config_text(config, "proxy_username").is_empty(),
        "noProxyList": text_or_null(&truncate_chars(config_text(config, "proxy_no_list"), MAX_RULE_TEXT_CHARS * 2)),
        "perProtocolOverrides": per_protocol,
        "btProxy": {
            "enabled": config_flag(config, "bt_proxy_enabled"),
            "peersThroughProxy": config_flag(config, "bt_proxy_peers"),
        },
        "multiNicEnabled": config_flag(config, "multi_nic_enabled"),
        "cdnMultiEnabled": config_flag(config, "cdn_multi_enabled"),
    })
}

fn system_proxy_json(system: Result<SystemProxyDto, ToolError>) -> Value {
    match system {
        Ok(dto) if dto.detected => json!({
            "detected": true,
            "type": dto.proxy_type,
            "endpoint": format!("{}:{}", dto.host, dto.port),
            "noProxyList": text_or_null(&truncate_chars(&dto.no_list, MAX_RULE_TEXT_CHARS * 2)),
        }),
        Ok(_) => json!({ "detected": false }),
        Err(error) => json!({ "detected": null, "error": error.to_string() }),
    }
}

pub(super) async fn network_status(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    expect_no_args(args)?;
    let config = ctx.daemon_config().await?;
    let system = ctx
        .call_as::<SystemProxyDto>(method::DAEMON_CONFIG_SYSTEM_PROXY, json!({}))
        .await;
    let (gateway, cloud, daemon_connected) = ctx.inspect(|snapshot| {
        (
            snapshot.gateway.clone(),
            snapshot.cloud_connection.clone(),
            snapshot.daemon_connected,
        )
    });
    Ok(json!({
        "globalProxy": global_proxy_json(&config),
        "systemProxy": system_proxy_json(system),
        "siteProxyRules": site_rules_json(config_text(&config, "proxy_rules")),
        "localGateway": {
            "port": gateway.port,
            "jsonRpcEnabled": gateway.jsonrpc_enabled,
            "apiEnabled": gateway.api_enabled,
            "mcpEnabled": gateway.mcp_enabled,
            "lanEnabled": gateway.lan_enabled,
            "lanActive": gateway.lan_active,
        },
        "connectivity": {
            "daemonConnected": daemon_connected,
            "fluxCloud": {
                "state": to_json("cloud connection state", &cloud.state)?,
                "lastError": cloud.last_error.as_deref().map_or(Value::Null, short_text),
            },
            "note": "use fd_run_diagnostics for service / port / environment checks and fd_measure_routes for real throughput",
        },
    }))
}

// ───────────────────────── fd_list_plugins ─────────────────────────

/// 插件来源:开发模式 → `dev`;FluxCloud 私有商店 → `private`;其余(公开市场 / 本机包)→ `market`。
fn plugin_source(plugin: &PluginDto) -> &'static str {
    if plugin.dev_mode {
        "dev"
    } else if plugin.source == "private" {
        "private"
    } else {
        "market"
    }
}

fn plugin_json(plugin: &PluginDto) -> Result<Value, ToolError> {
    let disabled_reason = match plugin.disabled_reason.as_str() {
        "" | "None" => Value::Null,
        other => Value::String(other.to_owned()),
    };
    let resolver_urls: Vec<String> = plugin
        .resolver_urls
        .iter()
        .take(MAX_PLUGIN_RESOLVER_URLS)
        .map(|url| truncate_chars(url, MAX_RULE_TEXT_CHARS))
        .collect();
    Ok(json!({
        "id": plugin.identity,
        "name": plugin.name,
        "version": plugin.version,
        "enabled": plugin.enabled,
        "disabledReason": disabled_reason,
        "source": plugin_source(plugin),
        "loadStatus": plugin.load_status,
        "lastError": short_text(&plugin.load_error),
        "errorDetail": to_json("plugin load detail", &plugin.load_detail)?,
        "permissions": plugin.permissions,
        "risk": plugin.risk,
        "resolverUrls": resolver_urls,
        "loginSupported": plugin.auth_supported,
    }))
}

pub(super) async fn plugins(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    expect_no_args(args)?;
    let plugins: Vec<PluginDto> = ctx.call_as(method::DAEMON_PLUGIN_LIST, json!({})).await?;
    let items = plugins
        .iter()
        .take(MAX_PLUGINS)
        .map(plugin_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "total": plugins.len(),
        "returned": items.len(),
        "plugins": items,
    }))
}

// ───────────────────────── fd_list_components ─────────────────────────

/// ffmpeg / yt-dlp 两个状态结构同形,统一成一份便于输出。
struct ComponentFacts {
    name: &'static str,
    source: String,
    path: String,
    version: String,
    managed_version: String,
    system_path: String,
    managed_supported: bool,
}

fn component_facts(status: ComponentStatusDto) -> ComponentFacts {
    match status {
        ComponentStatusDto::Ffmpeg(status) => ComponentFacts {
            name: "ffmpeg",
            source: status.source,
            path: status.path,
            version: status.version,
            managed_version: status.managed_version,
            system_path: status.system_path,
            managed_supported: status.managed_supported,
        },
        ComponentStatusDto::Ytdlp(status) => ComponentFacts {
            name: "ytdlp",
            source: status.source,
            path: status.path,
            version: status.version,
            managed_version: status.managed_version,
            system_path: status.system_path,
            managed_supported: status.managed_supported,
        },
    }
}

/// 最新稳定版查询的结果。
enum LatestVersion {
    NotApplicable,
    Known(String),
    Failed(String),
}

fn component_json(facts: &ComponentFacts, latest: &LatestVersion) -> Value {
    let mut value = json!({
        "name": facts.name,
        "installed": facts.source != "none",
        "source": facts.source,
        "version": text_or_null(&facts.version),
        "managedVersion": text_or_null(&facts.managed_version),
        "path": text_or_null(&display_path(&facts.path)),
        "foundOnSystemPath": !facts.system_path.is_empty(),
        "managedInstallSupported": facts.managed_supported,
    });
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    match latest {
        LatestVersion::NotApplicable => {}
        LatestVersion::Known(latest) => {
            object.insert("latestStableVersion".to_owned(), text_or_null(latest));
            // 只有托管安装才谈得上「更新」:系统 / 手动指定的程序由用户自己维护。
            let update_available = !facts.managed_version.is_empty()
                && !latest.is_empty()
                && facts.managed_version != *latest;
            object.insert("updateAvailable".to_owned(), json!(update_available));
        }
        LatestVersion::Failed(message) => {
            object.insert(
                "updateCheckError".to_owned(),
                Value::String(truncate_chars(message, MAX_ERROR_TEXT_CHARS)),
            );
        }
    }
    value
}

async fn component_report(ctx: &ToolContext, kind: ComponentKind) -> Result<Value, ToolError> {
    let params = json!({ "component": kind });
    let status: ComponentStatusDto = ctx
        .call_as(method::DAEMON_COMPONENT_GET, params.clone())
        .await?;
    let facts = component_facts(status);
    let latest = if facts.managed_supported {
        match tokio::time::timeout(
            VERSION_CHECK_TIMEOUT,
            ctx.call_as::<ComponentVersions>(method::DAEMON_COMPONENT_LIST_VERSIONS, params),
        )
        .await
        {
            Ok(Ok(versions)) => LatestVersion::Known(versions.latest_stable),
            Ok(Err(error)) => LatestVersion::Failed(error.to_string()),
            Err(_) => LatestVersion::Failed(format!(
                "version lookup timed out after {} seconds",
                VERSION_CHECK_TIMEOUT.as_secs()
            )),
        }
    } else {
        LatestVersion::NotApplicable
    };
    Ok(component_json(&facts, &latest))
}

pub(super) async fn components(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    expect_no_args(args)?;
    let (ffmpeg, ytdlp) = tokio::join!(
        component_report(ctx, ComponentKind::Ffmpeg),
        component_report(ctx, ComponentKind::Ytdlp)
    );
    Ok(json!({ "components": [ffmpeg?, ytdlp?] }))
}

// ───────────────────────── fd_run_diagnostics ─────────────────────────

fn level_name(level: &DiagnosticLevel) -> &'static str {
    match level {
        DiagnosticLevel::Info => "info",
        DiagnosticLevel::Ok => "ok",
        DiagnosticLevel::Warn => "warn",
        DiagnosticLevel::Error => "error",
    }
}

/// 排序权重:错误、警告在前,其次提示,最后是通过项。
fn level_rank(level: &DiagnosticLevel) -> u8 {
    match level {
        DiagnosticLevel::Error => 0,
        DiagnosticLevel::Warn => 1,
        DiagnosticLevel::Info => 2,
        DiagnosticLevel::Ok => 3,
    }
}

fn diagnostics_json(report: &DiagnosticsReportDto) -> Value {
    let mut checks: Vec<&fluxdown_protocol::DiagnosticCheckDto> = report.checks.iter().collect();
    checks.sort_by_key(|check| level_rank(&check.level));
    let count = |wanted: &DiagnosticLevel| {
        report
            .checks
            .iter()
            .filter(|check| level_name(&check.level) == level_name(wanted))
            .count()
    };
    let items: Vec<Value> = checks
        .iter()
        .map(|check| {
            let mut item = json!({
                "id": check.id,
                "target": text_or_null(&check.target),
                "level": level_name(&check.level),
                "detail": truncate_chars(&check.detail, MAX_DIAGNOSTIC_DETAIL_CHARS),
                "hint": text_or_null(&check.hint),
            });
            if let (Some(object), Some(repair)) = (item.as_object_mut(), &check.repair) {
                object.insert(
                    "repairAction".to_owned(),
                    json!({ "action": repair.action, "target": text_or_null(&repair.target) }),
                );
            }
            item
        })
        .collect();
    json!({
        "generatedAt": iso_millis(report.generated_at_unix_ms),
        "appVersion": report.app_version,
        "platform": report.platform,
        "daemonConnected": report.daemon_connected,
        "summary": {
            "error": count(&DiagnosticLevel::Error),
            "warn": count(&DiagnosticLevel::Warn),
            "info": count(&DiagnosticLevel::Info),
            "ok": count(&DiagnosticLevel::Ok),
        },
        "checks": items,
        "note": "repairAction entries can be applied with fd_repair (the user must approve each call); 'open_settings' / 'open_log_dir' / 'test_notification' stay in the app's diagnostics page",
    })
}

pub(super) async fn diagnostics(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    expect_no_args(args)?;
    let report: DiagnosticsReportDto = ctx
        .call_as(method::AGENT_DIAGNOSTICS_RUN, json!({}))
        .await?;
    Ok(diagnostics_json(&report))
}

// ───────────────────────── fd_get_net_interfaces ─────────────────────────

pub(super) async fn net_interfaces(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    expect_no_args(args)?;
    let interfaces: NetInterfacesDto = ctx
        .call_as(method::DAEMON_NET_INTERFACES, json!({}))
        .await?;
    to_json("net interfaces", &interfaces)
}

// ───────────────────────── fd_measure_routes ─────────────────────────

const MIB: u64 = 1024 * 1024;
const MAX_MEASURE_TASK_ID_CHARS: usize = 128;
const MAX_MEASURE_URL_CHARS: usize = 8192;
const MAX_MEASURE_ROUTE_CHARS: usize = 300;
/// 测速不得指向 FluxDown 自己的服务器:官网 / 下载分发 / FluxCloud 的域名(含全部子域)。
const FLUXDOWN_DOMAINS: [&str; 3] = ["fluxdown.com", "fluxdown.zerx.dev", "zerx.dev"];
/// 源站与运维服务器的 IP。
const FLUXDOWN_IPS: [[u8; 4]; 2] = [[123, 57, 37, 142], [8, 216, 57, 80]];
/// 阿里云 OSS 发布桶:`zerx-lab.oss-<region>[-internal | -accelerate].aliyuncs.com`。
const FLUXDOWN_OSS_PREFIX: &str = "zerx-lab.oss-";
const ALIYUN_SUFFIX: &str = ".aliyuncs.com";
/// 123 云盘发布空间:直链形如 `<UID>.cdn.123clouddisk.cn`。
const PAN123_UID: &str = "1860494902";
const PAN123_SUFFIXES: [&str; 4] = [
    ".123clouddisk.cn",
    ".123clouddisk.com",
    ".123pan.cn",
    ".123pan.com",
];
/// `routes` 里的关键字路线;其余必须是不含凭据的代理 URL。
const ROUTE_KEYWORDS: [&str; 4] = ["current", "direct", "system", "manual"];
const ROUTE_PROXY_SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];
const MEASURE_URL_SCHEMES: [&str; 3] = ["http", "https", "ftp"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct MeasureArgs {
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) url: Option<String>,
    #[serde(default)]
    pub(super) routes: Option<Vec<String>>,
    #[serde(default)]
    pub(super) connections: Option<u32>,
    #[serde(default)]
    pub(super) max_megabytes: Option<u32>,
    #[serde(default)]
    pub(super) max_seconds: Option<u32>,
}

/// 主机是否属于 FluxDown 自己的服务器(域名及子域、OSS 发布桶、源站 IP、123 云盘发布直链)。
fn is_fluxdown_host(host: &str) -> bool {
    let host = host
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let v4 = match ip {
            std::net::IpAddr::V4(v4) => Some(v4),
            std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped(),
        };
        return v4.is_some_and(|v4| FLUXDOWN_IPS.contains(&v4.octets()));
    }
    if FLUXDOWN_DOMAINS
        .iter()
        .any(|own| host == *own || host.ends_with(&format!(".{own}")))
    {
        return true;
    }
    if host.starts_with(FLUXDOWN_OSS_PREFIX) && host.ends_with(ALIYUN_SUFFIX) {
        return true;
    }
    PAN123_SUFFIXES.iter().any(|suffix| host.ends_with(suffix))
        && host.split('.').any(|label| label.contains(PAN123_UID))
}

fn check_measure_url(url: &str) -> Result<(), ToolError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid("url is not a valid URL"))?;
    if !MEASURE_URL_SCHEMES.contains(&parsed.scheme()) {
        return Err(invalid("url must be an http, https or ftp URL"));
    }
    match url_host(url) {
        Some(host) if is_fluxdown_host(&host) => Err(ToolError::Refused(
            "measuring FluxDown's own servers is not allowed".to_owned(),
        )),
        Some(_) => Ok(()),
        None => Err(invalid("url has no host")),
    }
}

fn check_route(route: &str) -> Result<(), ToolError> {
    if route.is_empty() {
        return Err(invalid("routes must not contain empty entries"));
    }
    check_max("routes[]", route, MAX_MEASURE_ROUTE_CHARS)?;
    if ROUTE_KEYWORDS.contains(&route) {
        return Ok(());
    }
    let parsed = reqwest::Url::parse(route).map_err(|_| {
        invalid("each route must be 'current', 'direct', 'system', 'manual' or a proxy URL")
    })?;
    if !ROUTE_PROXY_SCHEMES.contains(&parsed.scheme()) || parsed.host_str().is_none() {
        return Err(invalid(
            "a proxy route must be an http, https, socks5 or socks5h URL with a host",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid("a proxy route must not contain credentials"));
    }
    Ok(())
}

fn check_range(field: &str, value: Option<u32>, min: u32, max: u32) -> Result<(), ToolError> {
    match value {
        Some(value) if !(min..=max).contains(&value) => {
            Err(invalid(format!("{field} must be between {min} and {max}")))
        }
        _ => Ok(()),
    }
}

/// 把参数映射成 `daemon.net.measureRoutes` 的参数:`taskId` 与 `url` 恰好给一个,其余按目录 Schema 校验。
fn measure_params(args: MeasureArgs) -> Result<NetMeasureParams, ToolError> {
    let MeasureArgs {
        task_id,
        url,
        routes,
        connections,
        max_megabytes,
        max_seconds,
    } = args;
    if task_id.is_some() == url.is_some() {
        return Err(invalid("give exactly one of taskId / url"));
    }
    if let Some(task_id) = &task_id {
        if task_id.is_empty() {
            return Err(invalid("taskId must not be empty"));
        }
        check_max("taskId", task_id, MAX_MEASURE_TASK_ID_CHARS)?;
    }
    if let Some(url) = &url {
        if url.is_empty() {
            return Err(invalid("url must not be empty"));
        }
        check_max("url", url, MAX_MEASURE_URL_CHARS)?;
        check_measure_url(url)?;
    }
    let routes = match routes {
        None => Vec::new(),
        Some(routes) if routes.is_empty() || routes.len() > NET_MEASURE_MAX_ROUTES => {
            return Err(invalid(format!(
                "routes must contain 1 to {NET_MEASURE_MAX_ROUTES} entries"
            )));
        }
        Some(routes) => {
            for route in &routes {
                check_route(route)?;
            }
            routes
        }
    };
    check_range("connections", connections, 1, NET_MEASURE_MAX_CONNECTIONS)?;
    check_range("maxMegabytes", max_megabytes, 1, NET_MEASURE_MAX_MEGABYTES)?;
    check_range(
        "maxSeconds",
        max_seconds,
        NET_MEASURE_MIN_SECONDS,
        NET_MEASURE_MAX_SECONDS,
    )?;
    Ok(NetMeasureParams {
        task_id,
        url,
        routes,
        connections,
        max_megabytes,
        max_seconds,
    })
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// 字节 / 秒 → Mbps(十进制,与运营商标称带宽同口径)。
fn mbps(bytes_per_sec: u64) -> f64 {
    round2(bytes_per_sec as f64 * 8.0 / 1_000_000.0)
}

fn route_report(route: &NetRouteMeasureDto) -> Value {
    let per_connection: Vec<f64> = route
        .per_connection_bytes_per_sec
        .iter()
        .copied()
        .map(mbps)
        .collect();
    json!({
        "route": route.route,
        "egress": route.egress,
        "routeLabel": text_or_null(&route.route_label),
        "ok": route.ok,
        "error": route.error.as_deref().map_or(Value::Null, short_text),
        "httpStatus": route.http_status,
        "timingTarget": text_or_null(&route.timing_target),
        "dnsMs": route.dns_ms,
        "connectMs": route.connect_ms,
        "tlsMs": route.tls_ms,
        "firstByteMs": route.first_byte_ms,
        "rangeSupported": route.range_supported,
        "totalBytes": route.total_bytes,
        "bytes": route.bytes,
        "seconds": round2(route.seconds),
        "avgMbps": mbps(route.avg_bytes_per_sec),
        "peakMbps": mbps(route.peak_bytes_per_sec),
        "connectionsUsed": route.connections_used,
        "perConnectionAvgMbps": per_connection,
        "stoppedBy": text_or_null(&route.stopped_by),
    })
}

fn measure_report(result: &NetMeasureResultDto) -> Value {
    let routes: Vec<Value> = result.routes.iter().map(route_report).collect();
    json!({
        "source": {
            "scheme": result.source.scheme,
            "host": result.source.host,
            "taskId": result.source.task_id,
            "plannedConnections": result.source.connections,
            "maxMegabytes": result.source.max_bytes / MIB,
            "maxSeconds": result.source.max_seconds,
        },
        "routes": routes,
    })
}

pub(super) async fn measure_routes(ctx: &ToolContext, args: Value) -> Result<Value, ToolError> {
    let params = measure_params(parse_args(args)?)?;
    // 按任务测速时,任务地址同样不得指向 FluxDown 自己的服务器。
    if let Some(task_url) = params.task_id.as_deref().and_then(|id| {
        ctx.inspect(|snapshot| snapshot.daemon.task(id).map(|task| task.url.clone()))
    }) && url_host(&task_url).is_some_and(|host| is_fluxdown_host(&host))
    {
        return Err(ToolError::Refused(
            "measuring FluxDown's own servers is not allowed".to_owned(),
        ));
    }
    let request = to_json("net measure params", &params)?;
    let result: NetMeasureResultDto = ctx
        .call_as(method::DAEMON_NET_MEASURE_ROUTES, request)
        .await?;
    Ok(measure_report(&result))
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::net_diag::NetMeasureSourceDto;
    use serde_json::json;

    use super::*;

    fn measure(args: Value) -> Result<NetMeasureParams, ToolError> {
        measure_params(parse_args(args)?)
    }

    fn message(error: ToolError) -> String {
        error.to_string()
    }

    #[test]
    fn measure_maps_task_with_defaults() {
        let params = measure(json!({ "taskId": "t-1" })).expect("valid");
        assert_eq!(params.task_id.as_deref(), Some("t-1"));
        assert_eq!(params.url, None);
        assert!(
            params.routes.is_empty(),
            "daemon applies the default routes"
        );
        assert_eq!(params.connections, None);
        assert_eq!(params.max_megabytes, None);
        assert_eq!(params.max_seconds, None);
    }

    #[test]
    fn measure_maps_every_field() {
        let params = measure(json!({
            "url": "https://cdn.example.com/big.bin",
            "routes": ["current", "direct", "system", "manual"],
            "connections": 8,
            "maxMegabytes": 64,
            "maxSeconds": 20,
        }))
        .expect("valid");
        assert_eq!(
            params.url.as_deref(),
            Some("https://cdn.example.com/big.bin")
        );
        assert_eq!(params.task_id, None);
        assert_eq!(params.routes, ["current", "direct", "system", "manual"]);
        assert_eq!(params.connections, Some(8));
        assert_eq!(params.max_megabytes, Some(64));
        assert_eq!(params.max_seconds, Some(20));
    }

    #[test]
    fn measure_accepts_credential_free_proxy_routes() {
        let params = measure(json!({
            "taskId": "t",
            "routes": ["socks5://127.0.0.1:1080", "http://proxy.local:8080"],
        }))
        .expect("valid");
        assert_eq!(params.routes.len(), 2);
    }

    #[test]
    fn measure_needs_exactly_one_source() {
        assert!(message(measure(json!({})).unwrap_err()).contains("exactly one"));
        let both = measure(json!({ "taskId": "t", "url": "https://a.example/x" }));
        assert!(message(both.unwrap_err()).contains("exactly one"));
    }

    #[test]
    fn measure_rejects_unknown_fields() {
        let error = measure(json!({ "taskId": "t", "proxy": "http://x:1" })).unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)), "{error}");
    }

    #[test]
    fn measure_rejects_bad_routes() {
        for routes in [
            json!([]),
            json!(["current", "direct", "system", "manual", "current"]),
            json!(["http://user:pw@proxy:8080"]),
            json!(["ftp://proxy:21"]),
            json!(["fastest"]),
            json!([""]),
        ] {
            let result = measure(json!({ "taskId": "t", "routes": routes }));
            assert!(result.is_err(), "{routes}");
        }
    }

    #[test]
    fn measure_rejects_out_of_range_limits() {
        for patch in [
            json!({ "connections": 0 }),
            json!({ "connections": 33 }),
            json!({ "maxMegabytes": 0 }),
            json!({ "maxMegabytes": 513 }),
            json!({ "maxSeconds": 2 }),
            json!({ "maxSeconds": 61 }),
        ] {
            let mut args = json!({ "taskId": "t" });
            if let (Some(args), Some(patch)) = (args.as_object_mut(), patch.as_object()) {
                args.extend(patch.clone());
            }
            assert!(measure(args).is_err(), "{patch}");
        }
    }

    #[test]
    fn measure_rejects_non_transfer_urls_and_own_servers() {
        for url in ["not a url", "file:///etc/passwd", "magnet:?xt=urn:btih:abc"] {
            assert!(measure(json!({ "url": url })).is_err(), "{url}");
        }
        for url in [
            "https://fluxdown.com/download",
            "https://dl.fluxdown.com/app.zip",
            "https://fluxdown.zerx.dev/x",
            "https://api.zerx.dev/",
        ] {
            let error = measure(json!({ "url": url })).unwrap_err();
            assert!(matches!(error, ToolError::Refused(_)), "{url}: {error}");
        }
        assert!(measure(json!({ "url": "https://evilfluxdown.com/x" })).is_ok());
        assert!(measure(json!({ "url": "https://fluxdown.com.evil.example/x" })).is_ok());
    }

    #[test]
    fn measure_report_converts_units() {
        let result = NetMeasureResultDto {
            source: NetMeasureSourceDto {
                scheme: "https".to_owned(),
                host: "cdn.example.com".to_owned(),
                task_id: Some("t".to_owned()),
                connections: 4,
                max_bytes: 32 * MIB,
                max_seconds: 15,
            },
            routes: vec![NetRouteMeasureDto {
                route: "direct".to_owned(),
                egress: "direct".to_owned(),
                ok: true,
                bytes: 10_000_000,
                seconds: 2.0,
                avg_bytes_per_sec: 5_000_000,
                peak_bytes_per_sec: 6_250_000,
                connections_used: 2,
                per_connection_bytes_per_sec: vec![2_500_000, 2_500_000],
                stopped_by: "bytes".to_owned(),
                ..NetRouteMeasureDto::default()
            }],
        };
        let report = measure_report(&result);
        assert_eq!(report["source"]["maxMegabytes"], 32);
        let route = &report["routes"][0];
        assert_eq!(route["avgMbps"], 40.0);
        assert_eq!(route["peakMbps"], 50.0);
        assert_eq!(route["perConnectionAvgMbps"], json!([20.0, 20.0]));
        assert_eq!(route["seconds"], 2.0);
        assert_eq!(route["error"], Value::Null);
    }

    #[test]
    fn settings_filters_parse_and_match() {
        let filters =
            parse_filters(vec!["proxy*".to_owned(), "default_segments".to_owned()]).expect("valid");
        assert!(filters[0].matches("proxy_mode"));
        assert!(!filters[0].matches("bt_proxy_enabled"));
        assert!(filters[1].matches("default_segments"));
        assert!(!filters[1].matches("default_segments2"));
        for bad in ["", "a*b", "*x*"] {
            assert!(parse_filters(vec![bad.to_owned()]).is_err(), "{bad}");
        }
        assert!(parse_filters(vec!["k".to_owned(); MAX_SETTING_FILTERS + 1]).is_err());
    }

    #[test]
    fn settings_view_masks_sensitive_keys_and_reports_unmatched() {
        let mut config = DaemonConfigSnapshot::default();
        config
            .values
            .insert("proxy_password".to_owned(), "hunter2".to_owned());
        config
            .values
            .insert("proxy_host".to_owned(), "10.0.0.1".to_owned());
        config
            .values
            .insert("default_segments".to_owned(), "8".to_owned());
        let prefs = AgentPreferencesDto::default();
        let filters = parse_filters(vec![
            "proxy_*".to_owned(),
            "default_segments".to_owned(),
            "no_such_key".to_owned(),
        ])
        .expect("valid");
        let view = settings_view(&config, &prefs, &filters);
        assert_eq!(view["settings"]["proxy_password"], MASKED);
        assert_eq!(view["settings"]["proxy_host"], "10.0.0.1");
        assert_eq!(view["settings"]["default_segments"], 8);
        assert!(!view.to_string().contains("hunter2"));
        assert_eq!(view["unmatchedFilters"], json!(["no_such_key"]));
        let masked = view["maskedKeys"].as_array().expect("masked keys");
        assert!(masked.iter().any(|key| key == "proxy_password"));
    }

    #[test]
    fn settings_view_shows_tracker_origins_without_passkey_paths() {
        let mut config = DaemonConfigSnapshot::default();
        config.values.insert(
            "bt_custom_trackers".to_owned(),
            "https://tracker.example/SECRETPASSKEY/announce\n\n  udp://t2.example:6969/announce?k=SECRET2 \n"
                .to_owned(),
        );
        config.values.insert(
            "proxy_protocol_http".to_owned(),
            "http://user:hunter2@proxy.example:8080/".to_owned(),
        );
        let filters = parse_filters(vec![
            "bt_custom_trackers".to_owned(),
            "proxy_protocol_http".to_owned(),
        ])
        .expect("valid");
        let view = settings_view(&config, &AgentPreferencesDto::default(), &filters);
        assert_eq!(
            view["settings"]["bt_custom_trackers"],
            "https://tracker.example\nudp://t2.example:6969"
        );
        let text = view.to_string();
        for leaked in ["SECRETPASSKEY", "SECRET2", "hunter2", "user:"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        assert!(text.contains("proxy.example"));
    }

    #[test]
    fn settings_view_without_filters_lists_every_daemon_key() {
        let view = settings_view(
            &DaemonConfigSnapshot::default(),
            &AgentPreferencesDto::default(),
            &[],
        );
        let settings = view["settings"].as_object().expect("settings object");
        for field in DAEMON_CONFIG_FIELDS {
            assert!(settings.contains_key(field.key), "{}", field.key);
        }
    }

    #[test]
    fn site_rules_summary_hides_proxy_credentials() {
        let raw = r#"[{"id":"a","enabled":true,"name":"work","match":"domain","pattern":"corp.example","action":"custom","proxy":"socks5://u:p@10.1.1.1:1080/"}]"#;
        let summary = site_rules_json(raw);
        assert_eq!(summary["total"], 1);
        assert_eq!(summary["enabled"], 1);
        assert_eq!(summary["rules"][0]["proxy"], "socks5://10.1.1.1:1080");
        assert!(!summary.to_string().contains("u:p"));
    }

    #[test]
    fn component_update_requires_a_managed_install() {
        let facts = ComponentFacts {
            name: "ffmpeg",
            source: "managed".to_owned(),
            path: String::new(),
            version: "7.0".to_owned(),
            managed_version: "7.0".to_owned(),
            system_path: String::new(),
            managed_supported: true,
        };
        let value = component_json(&facts, &LatestVersion::Known("7.1".to_owned()));
        assert_eq!(value["updateAvailable"], true);
        let value = component_json(&facts, &LatestVersion::Known("7.0".to_owned()));
        assert_eq!(value["updateAvailable"], false);
        let system = ComponentFacts {
            managed_version: String::new(),
            source: "system".to_owned(),
            ..facts
        };
        let value = component_json(&system, &LatestVersion::Known("7.1".to_owned()));
        assert_eq!(value["updateAvailable"], false);
    }

    #[test]
    fn diagnostics_lists_problems_first() {
        let check = |id: &str, level: DiagnosticLevel| fluxdown_protocol::DiagnosticCheckDto {
            id: id.to_owned(),
            target: String::new(),
            level,
            detail: "detail".to_owned(),
            hint: String::new(),
            repair: None,
        };
        let report = DiagnosticsReportDto {
            checks: vec![
                check("a", DiagnosticLevel::Ok),
                check("b", DiagnosticLevel::Error),
                check("c", DiagnosticLevel::Warn),
            ],
            ..DiagnosticsReportDto::default()
        };
        let value = diagnostics_json(&report);
        assert_eq!(value["checks"][0]["id"], "b");
        assert_eq!(value["checks"][1]["id"], "c");
        assert_eq!(value["summary"]["ok"], 1);
        assert_eq!(value["summary"]["error"], 1);
    }
}
