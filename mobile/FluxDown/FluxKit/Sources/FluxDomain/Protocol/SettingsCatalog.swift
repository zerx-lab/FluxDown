import Foundation

// 设置键目录与写入路由（纯逻辑，无 I/O）。
//
// 镜像 `native/protocol/src/{daemon_config,settings}.rs` 与 Web `pages/settings/kit/writeStore.ts`：
// - daemon 键 → `daemon.config.patch`（值为 wire 字符串，先按 `normalize_daemon_config_value` 规范化）；
// - 落在云同步目录（`SYNC_SETTING_SPECS`）里的 daemon 键改走 `agent.preferences.patch`，用同步键名
//   与 JSON 类型值（与 GPUI `set_daemon` 一致）——否则下次拉取会把旧云端值覆盖回来；
// - agent 偏好 → `agent.preferences.patch`：目录内的键默认同步，其余必须 `sync: false`。

/// 一个设置键的存储位置、值域与默认值。
public struct SettingField: Sendable, Hashable {
    public enum Store: Sendable, Hashable {
        /// daemon 配置键（`daemon.config.patch`）。
        case daemon
        /// agent 偏好键（`agent.preferences.patch`）。
        case preference
    }

    public enum Kind: Sendable, Hashable {
        case bool
        case integer(min: Int64, max: Int64)
        case float(min: Double)
        case choice([String])
        case text
        /// 引擎自行维护：可读不可写。
        case readOnly
    }

    public let key: String
    public let store: Store
    public let kind: Kind
    /// 未持久化时的有效默认值（wire 字符串形式）。
    public let defaultWire: String

    public init(key: String, store: Store, kind: Kind, defaultWire: String) {
        self.key = key
        self.store = store
        self.kind = kind
        self.defaultWire = defaultWire
    }
}

/// 写入被拒（不发请求）：键未知 / 只读 / 值非法。`message` 是可展示的英文细节。
public struct SettingsValidationError: Error, Sendable, Hashable {
    public let key: String
    public let message: String

    public init(key: String, message: String) {
        self.key = key
        self.message = message
    }
}

public enum SettingsCatalog {
    // MARK: 字段目录

    private static let maxInt = Int64.max

    private static func daemon(_ key: String, _ kind: SettingField.Kind, _ def: String) -> SettingField {
        SettingField(key: key, store: .daemon, kind: kind, defaultWire: def)
    }

    private static func pref(_ key: String, _ kind: SettingField.Kind, _ def: String) -> SettingField {
        SettingField(key: key, store: .preference, kind: kind, defaultWire: def)
    }

    public static let fileExistsBehaviors = ["rename", "overwrite", "skip", "ask"]
    public static let fileMissingActions = ["keep", "delete"]
    public static let btSeedTimeUnits = ["minutes", "hours", "days"]
    public static let btSeedLimitOperators = ["or", "and"]
    public static let btSeedThenActions = ["stop", "delete", "delete_files"]
    public static let btGuardModes = ["off", "observe", "enforce"]
    public static let btGuardDownloadingActions = ["observe", "throttle", "choke", "ban"]
    public static let btMseModes = ["disabled", "enabled", "forced"]
    public static let proxyModes = ["none", "system", "manual", "auto"]
    public static let proxyTypes = ["http", "https", "socks4", "socks5"]
    public static let themeModes = ["system", "light", "dark"]
    public static let accentSchemes = ["blue", "green", "violet", "rose", "custom"]

    /// 全部 daemon 配置键（`DAEMON_CONFIG_FIELDS`，顺序无语义）。
    public static let daemonFields: [SettingField] = [
        // 下载
        daemon("default_save_dir", .text, ""),
        daemon("default_segments", .integer(min: 0, max: 512), "0"),
        daemon("auto_max_connections", .integer(min: 0, max: 128), "16"),
        daemon("cdn_multi_enabled", .bool, "false"),
        daemon("cdn_max_nodes", .integer(min: 0, max: 8), "0"),
        daemon("multi_nic_enabled", .bool, "false"),
        daemon("max_concurrent_tasks", .integer(min: 1, max: 1024), "5"),
        daemon("speed_limit_bytes", .integer(min: 0, max: maxInt), "0"),
        daemon("upload_limit_bytes", .integer(min: 0, max: maxInt), "0"),
        daemon("max_auto_retries", .integer(min: -1, max: 20), "3"),
        daemon("auto_retry_delay_secs", .integer(min: 0, max: 86_400), "5"),
        daemon("auto_resume_on_start", .bool, "false"),
        daemon("use_server_time", .bool, "false"),
        daemon("dedup_same_url", .bool, "false"),
        daemon("file_exists_behavior", .choice(fileExistsBehaviors), "rename"),
        daemon("file_missing_action", .choice(fileMissingActions), "keep"),
        daemon("completed_record_retention_days", .integer(min: 0, max: 3650), "0"),
        daemon("idle_file_scan", .bool, "false"),
        daemon("global_user_agent", .text, ""),
        daemon("default_queue_id", .text, ""),
        daemon("domain_conn_caps", .readOnly, ""),
        // BT
        daemon("bt_enabled", .bool, "true"),
        daemon("bt_enable_dht", .bool, "true"),
        daemon("bt_enable_upnp", .bool, "true"),
        daemon("bt_enable_utp", .bool, "true"),
        daemon("bt_port_start", .integer(min: 1, max: 65_535), "6881"),
        daemon("bt_port_end", .integer(min: 1, max: 65_535), "6891"),
        daemon("bt_mse_mode", .choice(btMseModes), "enabled"),
        daemon("bt_custom_trackers", .text, ""),
        daemon("bt_tracker_sub_enabled", .bool, "true"),
        daemon("bt_tracker_sub_urls", .text, ""),
        daemon("bt_tracker_sub_cache", .readOnly, ""),
        daemon("bt_tracker_sub_updated_at", .readOnly, "0"),
        daemon("bt_seed_enabled", .bool, "true"),
        daemon("bt_auto_reseed", .bool, "true"),
        daemon("bt_seed_max_active", .integer(min: 0, max: maxInt), "0"),
        daemon("bt_seed_ratio_limit", .float(min: 0), "0"),
        daemon("bt_seed_post_ratio_limit", .float(min: 0), "0"),
        daemon("bt_seed_time_limit_minutes", .integer(min: 0, max: maxInt), "0"),
        daemon("bt_seed_time_limit_unit", .choice(btSeedTimeUnits), "minutes"),
        daemon("bt_seed_inactive_time_limit_minutes", .integer(min: 0, max: maxInt), "0"),
        daemon("bt_seed_inactive_time_limit_unit", .choice(btSeedTimeUnits), "minutes"),
        daemon("bt_seed_limit_operator", .choice(btSeedLimitOperators), "or"),
        daemon("bt_seed_then_action", .choice(btSeedThenActions), "stop"),
        // BT 防吸血（热更新；`bt_guard_report_enabled` 设备本地不同步）
        daemon("bt_guard_enabled", .bool, "true"),
        daemon("bt_guard_rule_excessive", .choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_rerequest", .choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_rewind", .choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_lag", .choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_fingerprint", .choice(btGuardModes), "observe"),
        daemon("bt_guard_cloud_mode", .choice(btGuardModes), "observe"),
        daemon("bt_guard_excess_ratio_pct", .integer(min: 110, max: 1000), "200"),
        daemon("bt_guard_min_torrent_mib", .integer(min: 0, max: 1_048_576), "64"),
        daemon("bt_guard_rewind_tolerance_pct", .integer(min: 0, max: 100), "7"),
        daemon("bt_guard_throttle_kib", .integer(min: 1, max: 1_048_576), "32"),
        daemon("bt_guard_ban_base_hours", .integer(min: 1, max: 720), "24"),
        daemon("bt_guard_ban_max_days", .integer(min: 1, max: 365), "30"),
        daemon("bt_guard_downloading_max_action", .choice(btGuardDownloadingActions), "throttle"),
        daemon("bt_guard_skip_private", .bool, "true"),
        daemon("bt_guard_skip_lan", .bool, "true"),
        daemon("bt_guard_ipv6_prefix", .integer(min: 48, max: 128), "64"),
        daemon("bt_guard_filter_urls", .text, ""),
        daemon("bt_guard_cloud_enabled", .bool, "true"),
        daemon("bt_guard_report_enabled", .bool, "false"),
        // ED2K
        daemon("ed2k_enable_kad", .bool, "true"),
        daemon("ed2k_enable_upnp", .bool, "true"),
        daemon("ed2k_listen_port", .integer(min: 0, max: 65_535), "0"),
        daemon("ed2k_server_list", .text, ""),
        daemon("ed2k_server_sub_enabled", .bool, "true"),
        daemon("ed2k_server_sub_urls", .text, ""),
        daemon("ed2k_server_sub_cache", .readOnly, ""),
        daemon("ed2k_server_sub_updated_at", .readOnly, "0"),
        daemon("ed2k_nodes_dat_url", .text, ""),
        // 代理
        daemon("proxy_mode", .choice(proxyModes), "none"),
        daemon("proxy_type", .choice(proxyTypes), "http"),
        daemon("proxy_host", .text, ""),
        daemon("proxy_port", .text, ""),
        daemon("proxy_username", .text, ""),
        daemon("proxy_password", .text, ""),
        daemon("proxy_no_list", .text, ""),
        daemon("proxy_rules", .text, ""),
        // Webhook / 组件 / 日志
        daemon("webhook.endpoints", .text, ""),
        daemon("component.ffmpeg.path", .text, ""),
        daemon("component.ytdlp.path", .text, ""),
        daemon("component_mirror_base", .text, ""),
        daemon("log_max_size_mb", .integer(min: 1, max: 1024), "10"),
    ]

    /// 设置页读写的 agent 偏好键（带类型与默认值；目录外的键用 `setPreference` 传原始 JSON）。
    public static let preferenceFields: [SettingField] = [
        // 外观（云同步）
        pref("appearance.theme_mode", .choice(themeModes), "system"),
        pref("appearance.color_scheme", .choice(accentSchemes), "blue"),
        // ARGB（Flutter `Color.toARGB32()`，无符号 32 位）。
        pref("appearance.custom_color", .integer(min: 0, max: Int64(UInt32.max)), "4284704497"),
        // 通用
        pref("general.auto_check_update", .bool, "true"),
        pref("general.clipboard_watch", .bool, "false"),
        pref("analytics_enabled", .bool, "true"),
        pref("ui.show_sidebar_status", .bool, "true"),
        pref("ui.show_sidebar_queues", .bool, "true"),
        pref("ui.show_sidebar_category", .bool, "true"),
        pref("ui.show_sidebar_rss", .bool, "true"),
        pref("ui.show_sidebar_devices", .bool, "true"),
        pref("ui.show_activity_rss", .bool, "true"),
        pref("ui.show_activity_webhooks", .bool, "true"),
        pref("ui.show_activity_theme", .bool, "true"),
        // 下载 / 通知
        pref("download.remember_last_save_dir", .bool, "false"),
        pref("download.notify_on_complete", .bool, "true"),
        pref("download.silent_download", .bool, "false"),
        pref("download.silent_skip_selection", .bool, "false"),
        pref("download.keep_awake", .bool, "false"),
    ]

    private static let index: [String: SettingField] = {
        var map: [String: SettingField] = [:]
        for field in daemonFields + preferenceFields { map[field.key] = field }
        return map
    }()

    public static func field(_ key: String) -> SettingField? { index[key] }

    /// 键的有效默认值（wire）；未知键为空串。
    public static func defaultWire(_ key: String) -> String { index[key]?.defaultWire ?? "" }

    // MARK: 云同步目录

    /// 偏好 / agent 所有、参与云同步的键（`SYNC_SETTING_SPECS` 中 owner ≠ Daemon，不含集合范围键）。
    public static let syncedPreferenceKeys: Set<String> = [
        "appearance.theme_mode", "appearance.dark_theme", "appearance.light_theme",
        "appearance.color_scheme", "appearance.custom_color",
        "general.locale", "general.update_channel", "general.auto_check_update",
        "general.clipboard_watch", "general.floating_ball_enabled", "general.floating_ball_active_only",
        "ui.show_sidebar_status", "ui.show_sidebar_queues", "ui.show_sidebar_category",
        "ui.show_sidebar_rss", "ui.show_activity_rss", "ui.show_activity_webhooks",
        "ui.show_activity_theme", "ui.show_titlebar_pause_all", "ui.show_titlebar_resume_all",
        "ui.show_titlebar_settings", "ui.show_titlebar_theme",
        "download.remember_last_save_dir", "download.notify_on_complete", "download.silent_download",
        "download.keep_awake",
        "custom_categories",
    ]

    /// 自定义主题集合的范围键：只承载分组 / 本机专属开关，从不承载值（逐主题键 `appearance.custom_themes.<id>`）。
    public static let customThemesScopeKey = "appearance.custom_themes"

    /// daemon 存储键 → 云同步键（`SYNC_SETTING_SPECS` 中 owner = Daemon，29 项）。
    public static let daemonSyncNames: [String: String] = [
        "max_concurrent_tasks": "download.max_concurrent_tasks",
        "default_segments": "download.default_segments",
        "auto_max_connections": "download.auto_max_connections",
        "cdn_multi_enabled": "download.cdn_multi_enabled",
        "cdn_max_nodes": "download.cdn_max_nodes",
        "speed_limit_bytes": "download.speed_limit_bytes",
        "max_auto_retries": "download.max_auto_retries",
        "auto_retry_delay_secs": "download.auto_retry_delay_secs",
        "auto_resume_on_start": "download.auto_resume_on_start",
        "use_server_time": "download.use_server_time",
        "global_user_agent": "download.global_user_agent",
        "bt_enabled": "bt.enabled",
        "bt_enable_dht": "bt.enable_dht",
        "bt_enable_upnp": "bt.enable_upnp",
        "bt_enable_utp": "bt.enable_utp",
        "bt_custom_trackers": "bt.custom_trackers",
        "bt_tracker_sub_enabled": "bt.tracker_sub_enabled",
        "bt_tracker_sub_urls": "bt.tracker_sub_urls",
        "bt_seed_ratio_limit": "bt.seed_ratio_limit",
        "bt_seed_post_ratio_limit": "bt.seed_post_ratio_limit",
        "bt_seed_time_limit_minutes": "bt.seed_time_limit_minutes",
        "bt_seed_inactive_time_limit_minutes": "bt.seed_inactive_time_limit_minutes",
        "bt_seed_limit_operator": "bt.seed_limit_operator",
        "bt_seed_then_action": "bt.seed_then_action",
        "bt_seed_max_active": "bt.seed_max_active",
        "bt_guard_enabled": "bt.guard.enabled",
        "bt_guard_rule_excessive": "bt.guard.rule_excessive",
        "bt_guard_rule_rerequest": "bt.guard.rule_rerequest",
        "bt_guard_rule_rewind": "bt.guard.rule_rewind",
        "bt_guard_rule_lag": "bt.guard.rule_lag",
        "bt_guard_rule_fingerprint": "bt.guard.rule_fingerprint",
        "bt_guard_cloud_mode": "bt.guard.cloud_mode",
        "bt_guard_excess_ratio_pct": "bt.guard.excess_ratio_pct",
        "bt_guard_min_torrent_mib": "bt.guard.min_torrent_mib",
        "bt_guard_rewind_tolerance_pct": "bt.guard.rewind_tolerance_pct",
        "bt_guard_throttle_kib": "bt.guard.throttle_kib",
        "bt_guard_ban_base_hours": "bt.guard.ban_base_hours",
        "bt_guard_ban_max_days": "bt.guard.ban_max_days",
        "bt_guard_downloading_max_action": "bt.guard.downloading_max_action",
        "bt_guard_skip_private": "bt.guard.skip_private",
        "bt_guard_skip_lan": "bt.guard.skip_lan",
        "bt_guard_ipv6_prefix": "bt.guard.ipv6_prefix",
        "bt_guard_filter_urls": "bt.guard.filter_urls",
        "bt_guard_cloud_enabled": "bt.guard.cloud_enabled",
        "ed2k_enable_kad": "ed2k.enable_kad",
        "ed2k_enable_upnp": "ed2k.enable_upnp",
        "ed2k_server_list": "ed2k.server_list",
        "ed2k_server_sub_enabled": "ed2k.server_sub_enabled",
        "ed2k_server_sub_urls": "ed2k.server_sub_urls",
    ]

    /// 同步键名 → daemon 存储键（反查）。
    public static let syncNameToDaemonKey: [String: String] = {
        var map: [String: String] = [:]
        for (daemonKey, name) in daemonSyncNames { map[name] = daemonKey }
        return map
    }()

    /// 该键（daemon 存储键或偏好键）是否参与云同步（行标题后显示 ☁︎）。
    public static func isSynced(_ key: String) -> Bool {
        daemonSyncNames[key] != nil || syncedPreferenceKeys.contains(key) || isCustomThemeKey(key)
    }

    /// 偏好键是否在同步目录里（决定 `agent.preferences.patch` 的 `sync` 标志）。
    public static func isSyncedPreference(_ key: String) -> Bool {
        syncedPreferenceKeys.contains(key) || isCustomThemeKey(key)
    }

    public static func isCustomThemeKey(_ key: String) -> Bool {
        key.hasPrefix(customThemesScopeKey + ".") && key.count > customThemesScopeKey.count + 1
    }

    // MARK: 值规范化（镜像 `normalize_daemon_config_value`）

    /// 规范化一个 UI 键的 wire 值；非法返回 `SettingsValidationError`（不发请求）。
    ///
    /// daemon 键严格按 `normalize_daemon_config_value`；偏好键对 bool / 整数 / 枚举做同样的校验
    /// （文本不裁剪：偏好值原样保存）。
    public static func normalize(_ key: String, _ value: String) -> Result<String, SettingsValidationError> {
        guard let field = index[key] else {
            return .failure(SettingsValidationError(key: key, message: "unknown config key: \(key)"))
        }
        func fail(_ message: String) -> Result<String, SettingsValidationError> {
            .failure(SettingsValidationError(key: key, message: "\(key): \(message)"))
        }
        switch field.kind {
        case .readOnly:
            return fail("read-only")
        case .bool:
            switch value.trimmingCharacters(in: .whitespacesAndNewlines) {
            case "true", "1": return .success("true")
            case "false", "0": return .success("false")
            default: return fail("expected boolean")
            }
        case let .integer(min, max):
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            guard let parsed = parseInteger(trimmed) else { return fail("expected integer") }
            guard parsed >= min, parsed <= max else { return fail("must be between \(min) and \(max)") }
            return .success(String(parsed))
        case let .float(min):
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            guard let parsed = Double(trimmed), parsed.isFinite, parsed >= min else {
                return fail("must be a finite number >= \(rustFloat(min))")
            }
            return .success(rustFloat(parsed))
        case let .choice(allowed):
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            return allowed.contains(trimmed) ? .success(trimmed) : fail("must be one of \(allowed.joined(separator: ", "))")
        case .text:
            guard field.store == .daemon else { return .success(value) }
            switch key {
            case "component_mirror_base":
                return normalizeMirrorBase(value).map { .success($0) } ?? fail("must be an https:// base URL")
            case "global_user_agent":
                let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
                if trimmed.utf8.contains(where: { ($0 < 32 && $0 != 9) || $0 == 127 }) {
                    return fail("must not contain control characters")
                }
                return .success(trimmed)
            default:
                return .success(value.trimmingCharacters(in: .whitespacesAndNewlines))
            }
        }
    }

    /// 严格整数（可带符号；与 Rust `i64::from_str` 一致：不接受空白、小数、指数）。
    static func parseInteger(_ text: String) -> Int64? {
        guard !text.isEmpty else { return nil }
        var digits = Substring(text)
        if digits.first == "+" || digits.first == "-" { digits = digits.dropFirst() }
        guard !digits.isEmpty, digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        return Int64(text)
    }

    /// 近似 Rust `f64::to_string()`：整数值不带小数点，其余取最短往返表示（不使用指数）。
    static func rustFloat(_ value: Double) -> String {
        if value == value.rounded(), abs(value) < 1e15 { return String(Int64(value)) }
        let text = "\(value)"
        guard text.contains("e") || text.contains("E") else { return text }
        var fixed = String(format: "%.12f", locale: Locale(identifier: "en_US_POSIX"), value)
        while fixed.hasSuffix("0") { fixed.removeLast() }
        if fixed.hasSuffix(".") { fixed.removeLast() }
        return fixed
    }

    /// 组件镜像基址：空串 = 直连；非空必须是 `https://host[/path]`（无查询 / 片段 / 空白），去掉末尾 `/`。
    static func normalizeMirrorBase(_ value: String) -> String? {
        var trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix("/") { trimmed.removeLast() }
        if trimmed.isEmpty { return "" }
        if trimmed.unicodeScalars.contains(where: { $0.properties.isWhitespace || $0.value < 32 || $0.value == 127 }) { return nil }
        let scheme = "https://"
        guard trimmed.lowercased().hasPrefix(scheme) else { return nil }
        let rest = String(trimmed.dropFirst(scheme.count))
        if rest.contains("?") || rest.contains("#") { return nil }
        let authority = rest.split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? ""
        let hostPort = authority.split(separator: "@", omittingEmptySubsequences: false).last.map(String.init) ?? ""
        let host = hostPort.split(separator: ":", omittingEmptySubsequences: false).first.map(String.init) ?? ""
        if host.isEmpty { return nil }
        return scheme + rest
    }

    // MARK: wire → JSON（同步键走偏好通道时的 JSON 类型值）

    /// 规范化后的 wire 值 → 偏好通道的 JSON 值（镜像 `daemonWireToJson`）。
    public static func json(forWire wire: String, field: SettingField) -> JSONValue {
        switch field.kind {
        case .bool:
            return .bool(wire == "true")
        case .integer:
            return Int64(wire).map { .int($0) } ?? .null
        case .float:
            if let number = Double(wire), number.isFinite { return .double(number) }
            return .null
        case .choice, .text, .readOnly:
            return .string(wire)
        }
    }

    /// 偏好 JSON 值 → wire 字符串（UI 表单统一用字符串读取）；null / 数组 / 对象 → nil。
    public static func wire(fromJSON value: JSONValue) -> String? {
        switch value {
        case let .bool(flag): flag ? "true" : "false"
        case let .int(number): String(number)
        case let .uint(number): String(number)
        case let .double(number): number.isFinite ? rustFloat(number) : nil
        case let .string(text): text
        case .null, .array, .object: nil
        }
    }
}

/// 一批 UI 键值编辑落到哪条 RPC（`agent.preferences.patch` 同步 / 本机，`daemon.config.patch`）。
public struct SettingsWritePlan: Sendable, Equatable {
    /// `daemon.config.patch.values`：不在同步目录的 daemon 键（已规范化 wire）。
    public var daemon: [String: String] = [:]
    /// `agent.preferences.patch {values}`（默认 sync）：同步键名 → JSON。含改走偏好通道的 daemon 键。
    public var syncedPreferences: [String: JSONValue] = [:]
    /// `agent.preferences.patch {values, sync: false}`：设备本地偏好。
    public var localPreferences: [String: JSONValue] = [:]
    /// 同步键名 → daemon 存储键（用于把偏好通道的写入对回 daemon 键的乐观值 / 确认）。
    public var syncedDaemonKeys: [String: String] = [:]

    public init() {}

    public var isEmpty: Bool { daemon.isEmpty && syncedPreferences.isEmpty && localPreferences.isEmpty }

    /// 把 UI 键值编辑（daemon 配置键 / 目录内偏好键，值为 wire 字符串）路由到各通道。
    /// 任一键非法则整批拒绝（与 `normalize_daemon_config_patch` 一致）。
    public static func make(_ edits: [String: String]) -> Result<SettingsWritePlan, SettingsValidationError> {
        var plan = SettingsWritePlan()
        for (key, value) in edits.sorted(by: { $0.key < $1.key }) {
            switch SettingsCatalog.normalize(key, value) {
            case let .failure(error):
                return .failure(error)
            case let .success(wire):
                guard let field = SettingsCatalog.field(key) else {
                    return .failure(SettingsValidationError(key: key, message: "unknown config key: \(key)"))
                }
                switch field.store {
                case .daemon:
                    if let syncName = SettingsCatalog.daemonSyncNames[key] {
                        plan.syncedPreferences[syncName] = SettingsCatalog.json(forWire: wire, field: field)
                        plan.syncedDaemonKeys[syncName] = key
                    } else {
                        plan.daemon[key] = wire
                    }
                case .preference:
                    plan.addPreference(key, SettingsCatalog.json(forWire: wire, field: field))
                }
            }
        }
        return .success(plan)
    }

    /// 加入一个原始 JSON 偏好写入；是否同步由目录决定。集合范围键不可直接写。
    public mutating func addPreference(_ key: String, _ value: JSONValue) {
        if SettingsCatalog.isSyncedPreference(key) {
            syncedPreferences[key] = value
        } else {
            localPreferences[key] = value
        }
    }

    /// 校验原始 JSON 偏好写入（集合范围键只承载分组，不接受值）。
    public static func validatePreferenceKey(_ key: String) -> SettingsValidationError? {
        if key == SettingsCatalog.customThemesScopeKey {
            return SettingsValidationError(key: key, message: "\(key) is a collection; write per-theme keys")
        }
        if key.trimmingCharacters(in: .whitespaces).isEmpty {
            return SettingsValidationError(key: key, message: "empty preference key")
        }
        return nil
    }
}
