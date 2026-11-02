package com.fluxdown.core.protocol

/*
 * 设置键目录与写入路由（纯逻辑，无 I/O），镜像 `native/protocol/src/{daemon_config,settings}.rs`、
 * Web `pages/settings/kit/writeStore.ts` 与 iOS `FluxDomain/Protocol/SettingsCatalog.swift`：
 * - daemon 键 → `daemon.config.patch`（值为 wire 字符串，先按 `normalize_daemon_config_value` 规范化）；
 * - 落在云同步目录（`SYNC_SETTING_SPECS`）里的 daemon 键改走 `agent.preferences.patch`，用同步键名与
 *   JSON 类型值（与 GPUI `set_daemon` 一致）——否则下次拉取会把旧云端值覆盖回来；
 * - agent 偏好 → `agent.preferences.patch`：目录内的键默认同步，其余必须 `sync: false`。
 * 改 Rust 目录时同步这里（`SettingsCatalogTest` 守同步目录项数）。
 */

/** 一个设置键的存储位置、值域与默认值。 */
data class SettingField(
    val key: String,
    val store: Store,
    val kind: Kind,
    /** 未持久化时的有效默认值（wire 字符串形式）。 */
    val defaultWire: String,
) {
    enum class Store {
        /** daemon 配置键（`daemon.config.patch`）。 */
        Daemon,

        /** agent 偏好键（`agent.preferences.patch`）。 */
        Preference,
    }

    sealed interface Kind {
        data object Bool : Kind
        data class Integer(val min: Long, val max: Long) : Kind
        data class Float(val min: Double) : Kind
        data class Choice(val allowed: List<String>) : Kind
        data object Text : Kind

        /** 引擎自行维护：可读不可写。 */
        data object ReadOnly : Kind
    }
}

/** 写入被拒（不发请求）：键未知 / 只读 / 值非法。[message] 是英文细节（日志用，界面显示通用文案）。 */
data class SettingsValidationError(val key: String, val message: String)

/** 规范化结果。 */
sealed interface Normalized {
    data class Ok(val wire: String) : Normalized
    data class Rejected(val error: SettingsValidationError) : Normalized
}

object SettingsCatalog {
    val fileExistsBehaviors = listOf("rename", "overwrite", "skip", "ask")
    val fileMissingActions = listOf("keep", "delete")
    val btSeedTimeUnits = listOf("minutes", "hours", "days")
    val btSeedLimitOperators = listOf("or", "and")
    val btGuardModes = listOf("off", "observe", "enforce")
    val btGuardDownloadingActions = listOf("observe", "throttle", "choke", "ban")
    val btSeedThenActions = listOf("stop", "delete", "delete_files")
    val btMseModes = listOf("disabled", "enabled", "forced")
    val proxyModes = listOf("none", "system", "manual", "auto")
    val proxyTypes = listOf("http", "https", "socks4", "socks5")
    val themeModes = listOf("system", "light", "dark")
    val accentSchemes = listOf("blue", "green", "violet", "rose", "custom")

    private fun daemon(key: String, kind: SettingField.Kind, def: String) =
        SettingField(key, SettingField.Store.Daemon, kind, def)

    private fun pref(key: String, kind: SettingField.Kind, def: String) =
        SettingField(key, SettingField.Store.Preference, kind, def)

    private fun int(min: Long, max: Long) = SettingField.Kind.Integer(min, max)
    private val bool = SettingField.Kind.Bool
    private val text = SettingField.Kind.Text
    private val readOnly = SettingField.Kind.ReadOnly
    private fun choice(allowed: List<String>) = SettingField.Kind.Choice(allowed)
    private const val MAX = Long.MAX_VALUE

    /** 全部 daemon 配置键（`DAEMON_CONFIG_FIELDS`，顺序无语义）。 */
    val daemonFields: List<SettingField> = listOf(
        // 下载
        daemon("default_save_dir", text, ""),
        daemon("default_segments", int(0, 512), "0"),
        daemon("auto_max_connections", int(0, 128), "16"),
        daemon("cdn_multi_enabled", bool, "false"),
        daemon("cdn_max_nodes", int(0, 8), "0"),
        daemon("multi_nic_enabled", bool, "false"),
        daemon("max_concurrent_tasks", int(1, 1024), "5"),
        daemon("speed_limit_bytes", int(0, MAX), "0"),
        daemon("upload_limit_bytes", int(0, MAX), "0"),
        daemon("max_auto_retries", int(-1, 20), "3"),
        daemon("auto_retry_delay_secs", int(0, 86_400), "5"),
        daemon("auto_resume_on_start", bool, "false"),
        daemon("use_server_time", bool, "false"),
        daemon("dedup_same_url", bool, "false"),
        daemon("file_exists_behavior", choice(fileExistsBehaviors), "rename"),
        daemon("file_missing_action", choice(fileMissingActions), "keep"),
        daemon("completed_record_retention_days", int(0, 3650), "0"),
        daemon("idle_file_scan", bool, "false"),
        daemon("global_user_agent", text, ""),
        daemon("default_queue_id", text, ""),
        daemon("domain_conn_caps", readOnly, ""),
        // BT
        daemon("bt_enabled", bool, "true"),
        daemon("bt_enable_dht", bool, "true"),
        daemon("bt_enable_upnp", bool, "true"),
        daemon("bt_enable_utp", bool, "true"),
        daemon("bt_port_start", int(1, 65_535), "6881"),
        daemon("bt_port_end", int(1, 65_535), "6891"),
        daemon("bt_mse_mode", choice(btMseModes), "enabled"),
        daemon("bt_custom_trackers", text, ""),
        daemon("bt_tracker_sub_enabled", bool, "true"),
        daemon("bt_tracker_sub_urls", text, ""),
        daemon("bt_tracker_sub_cache", readOnly, ""),
        daemon("bt_tracker_sub_updated_at", readOnly, "0"),
        daemon("bt_seed_enabled", bool, "true"),
        daemon("bt_auto_reseed", bool, "true"),
        daemon("bt_seed_max_active", int(0, MAX), "0"),
        daemon("bt_seed_ratio_limit", SettingField.Kind.Float(0.0), "0"),
        daemon("bt_seed_post_ratio_limit", SettingField.Kind.Float(0.0), "0"),
        daemon("bt_seed_time_limit_minutes", int(0, MAX), "0"),
        daemon("bt_seed_time_limit_unit", choice(btSeedTimeUnits), "minutes"),
        daemon("bt_seed_inactive_time_limit_minutes", int(0, MAX), "0"),
        daemon("bt_seed_inactive_time_limit_unit", choice(btSeedTimeUnits), "minutes"),
        daemon("bt_seed_limit_operator", choice(btSeedLimitOperators), "or"),
        daemon("bt_seed_then_action", choice(btSeedThenActions), "stop"),
        // BT 防吸血（热更新；`bt_guard_report_enabled` 设备本地不同步）
        daemon("bt_guard_enabled", bool, "true"),
        daemon("bt_guard_rule_excessive", choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_rerequest", choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_rewind", choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_lag", choice(btGuardModes), "observe"),
        daemon("bt_guard_rule_fingerprint", choice(btGuardModes), "observe"),
        daemon("bt_guard_cloud_mode", choice(btGuardModes), "observe"),
        daemon("bt_guard_excess_ratio_pct", int(110, 1000), "200"),
        daemon("bt_guard_min_torrent_mib", int(0, 1_048_576), "64"),
        daemon("bt_guard_rewind_tolerance_pct", int(0, 100), "7"),
        daemon("bt_guard_throttle_kib", int(1, 1_048_576), "32"),
        daemon("bt_guard_ban_base_hours", int(1, 720), "24"),
        daemon("bt_guard_ban_max_days", int(1, 365), "30"),
        daemon("bt_guard_downloading_max_action", choice(btGuardDownloadingActions), "throttle"),
        daemon("bt_guard_skip_private", bool, "true"),
        daemon("bt_guard_skip_lan", bool, "true"),
        daemon("bt_guard_ipv6_prefix", int(48, 128), "64"),
        daemon("bt_guard_filter_urls", text, ""),
        daemon("bt_guard_cloud_enabled", bool, "true"),
        daemon("bt_guard_report_enabled", bool, "false"),
        // ED2K
        daemon("ed2k_enable_kad", bool, "true"),
        daemon("ed2k_enable_upnp", bool, "true"),
        daemon("ed2k_listen_port", int(0, 65_535), "0"),
        daemon("ed2k_server_list", text, ""),
        daemon("ed2k_server_sub_enabled", bool, "true"),
        daemon("ed2k_server_sub_urls", text, ""),
        daemon("ed2k_server_sub_cache", readOnly, ""),
        daemon("ed2k_server_sub_updated_at", readOnly, "0"),
        daemon("ed2k_nodes_dat_url", text, ""),
        // 代理
        daemon("proxy_mode", choice(proxyModes), "none"),
        daemon("proxy_type", choice(proxyTypes), "http"),
        daemon("proxy_host", text, ""),
        daemon("proxy_port", text, ""),
        daemon("proxy_username", text, ""),
        daemon("proxy_password", text, ""),
        daemon("proxy_no_list", text, ""),
        daemon("proxy_rules", text, ""),
        // Webhook / 组件 / 日志
        daemon("webhook.endpoints", text, ""),
        daemon("component.ffmpeg.path", text, ""),
        daemon("component.ytdlp.path", text, ""),
        daemon("component_mirror_base", text, ""),
        daemon("log_max_size_mb", int(1, 1024), "10"),
    )

    /** 设置页读写的 agent 偏好键（带类型与默认值；目录外的键用 `ConfigEditor.setPreference` 传原始 JSON）。 */
    val preferenceFields: List<SettingField> = listOf(
        // 外观（云同步）
        pref("appearance.theme_mode", choice(themeModes), "system"),
        pref("appearance.color_scheme", choice(accentSchemes), "blue"),
        // ARGB（Flutter `Color.toARGB32()`，无符号 32 位）。
        pref("appearance.custom_color", int(0, 0xFFFF_FFFFL), "4284704497"),
        // 通用
        pref("general.auto_check_update", bool, "true"),
        pref("general.clipboard_watch", bool, "false"),
        pref("analytics_enabled", bool, "true"),
        pref("ui.show_sidebar_status", bool, "true"),
        pref("ui.show_sidebar_queues", bool, "true"),
        pref("ui.show_sidebar_category", bool, "true"),
        pref("ui.show_sidebar_rss", bool, "true"),
        pref("ui.show_sidebar_devices", bool, "true"),
        pref("ui.show_activity_rss", bool, "true"),
        pref("ui.show_activity_webhooks", bool, "true"),
        pref("ui.show_activity_theme", bool, "true"),
        // 下载 / 通知
        pref("download.remember_last_save_dir", bool, "false"),
        pref("download.notify_on_complete", bool, "true"),
        pref("download.silent_download", bool, "false"),
        pref("download.silent_skip_selection", bool, "false"),
        pref("download.keep_awake", bool, "false"),
    )

    private val index: Map<String, SettingField> = (daemonFields + preferenceFields).associateBy { it.key }

    fun field(key: String): SettingField? = index[key]

    /** 键的有效默认值（wire）；未知键为空串。 */
    fun defaultWire(key: String): String = index[key]?.defaultWire.orEmpty()

    // ───────────────────────────── 云同步目录 ─────────────────────────────

    /** 偏好 / agent 所有、参与云同步的键（`SYNC_SETTING_SPECS` 中 owner ≠ Daemon，不含集合范围键）。 */
    val syncedPreferenceKeys: Set<String> = setOf(
        "appearance.theme_mode", "appearance.dark_theme", "appearance.light_theme",
        "appearance.color_scheme", "appearance.custom_color", "appearance.file_icon_pack",
        "general.locale", "general.update_channel", "general.auto_check_update",
        "general.clipboard_watch", "general.floating_ball_enabled", "general.floating_ball_active_only",
        "ui.show_sidebar_status", "ui.show_sidebar_queues", "ui.show_sidebar_category",
        "ui.show_sidebar_rss", "ui.show_activity_rss", "ui.show_activity_webhooks",
        "ui.show_activity_theme", "ui.show_activity_account", "ui.show_titlebar_pause_all", "ui.show_titlebar_resume_all",
        "ui.show_titlebar_settings", "ui.show_titlebar_theme",
        "download.remember_last_save_dir", "download.notify_on_complete", "download.silent_download",
        "download.keep_awake",
        "custom_categories",
    )

    /** 自定义主题集合的范围键：只承载分组 / 本机专属开关，从不承载值（逐主题键 `appearance.custom_themes.<id>`）。 */
    const val customThemesScopeKey = "appearance.custom_themes"

    /** daemon 存储键 → 云同步键（`SYNC_SETTING_SPECS` 中 owner = Daemon，49 项，含 19 个 `bt.guard.*`）。 */
    val daemonSyncNames: Map<String, String> = mapOf(
        "max_concurrent_tasks" to "download.max_concurrent_tasks",
        "default_segments" to "download.default_segments",
        "auto_max_connections" to "download.auto_max_connections",
        "cdn_multi_enabled" to "download.cdn_multi_enabled",
        "cdn_max_nodes" to "download.cdn_max_nodes",
        "speed_limit_bytes" to "download.speed_limit_bytes",
        "max_auto_retries" to "download.max_auto_retries",
        "auto_retry_delay_secs" to "download.auto_retry_delay_secs",
        "auto_resume_on_start" to "download.auto_resume_on_start",
        "use_server_time" to "download.use_server_time",
        "global_user_agent" to "download.global_user_agent",
        "bt_enabled" to "bt.enabled",
        "bt_enable_dht" to "bt.enable_dht",
        "bt_enable_upnp" to "bt.enable_upnp",
        "bt_enable_utp" to "bt.enable_utp",
        "bt_custom_trackers" to "bt.custom_trackers",
        "bt_tracker_sub_enabled" to "bt.tracker_sub_enabled",
        "bt_tracker_sub_urls" to "bt.tracker_sub_urls",
        "bt_seed_ratio_limit" to "bt.seed_ratio_limit",
        "bt_seed_post_ratio_limit" to "bt.seed_post_ratio_limit",
        "bt_seed_time_limit_minutes" to "bt.seed_time_limit_minutes",
        "bt_seed_inactive_time_limit_minutes" to "bt.seed_inactive_time_limit_minutes",
        "bt_seed_limit_operator" to "bt.seed_limit_operator",
        "bt_seed_then_action" to "bt.seed_then_action",
        "bt_seed_max_active" to "bt.seed_max_active",
        "bt_guard_enabled" to "bt.guard.enabled",
        "bt_guard_rule_excessive" to "bt.guard.rule_excessive",
        "bt_guard_rule_rerequest" to "bt.guard.rule_rerequest",
        "bt_guard_rule_rewind" to "bt.guard.rule_rewind",
        "bt_guard_rule_lag" to "bt.guard.rule_lag",
        "bt_guard_rule_fingerprint" to "bt.guard.rule_fingerprint",
        "bt_guard_cloud_mode" to "bt.guard.cloud_mode",
        "bt_guard_excess_ratio_pct" to "bt.guard.excess_ratio_pct",
        "bt_guard_min_torrent_mib" to "bt.guard.min_torrent_mib",
        "bt_guard_rewind_tolerance_pct" to "bt.guard.rewind_tolerance_pct",
        "bt_guard_throttle_kib" to "bt.guard.throttle_kib",
        "bt_guard_ban_base_hours" to "bt.guard.ban_base_hours",
        "bt_guard_ban_max_days" to "bt.guard.ban_max_days",
        "bt_guard_downloading_max_action" to "bt.guard.downloading_max_action",
        "bt_guard_skip_private" to "bt.guard.skip_private",
        "bt_guard_skip_lan" to "bt.guard.skip_lan",
        "bt_guard_ipv6_prefix" to "bt.guard.ipv6_prefix",
        "bt_guard_filter_urls" to "bt.guard.filter_urls",
        "bt_guard_cloud_enabled" to "bt.guard.cloud_enabled",
        "ed2k_enable_kad" to "ed2k.enable_kad",
        "ed2k_enable_upnp" to "ed2k.enable_upnp",
        "ed2k_server_list" to "ed2k.server_list",
        "ed2k_server_sub_enabled" to "ed2k.server_sub_enabled",
        "ed2k_server_sub_urls" to "ed2k.server_sub_urls",
    )

    /** 同步键名 → daemon 存储键（反查）。 */
    val syncNameToDaemonKey: Map<String, String> = daemonSyncNames.entries.associate { (k, v) -> v to k }

    /** 该键（daemon 存储键或偏好键）是否参与云同步（行标题后显示 ☁︎）。 */
    fun isSynced(key: String): Boolean =
        key in daemonSyncNames || key in syncedPreferenceKeys || isCustomThemeKey(key)

    /** 偏好键是否在同步目录里（决定 `agent.preferences.patch` 的 `sync` 标志）。 */
    fun isSyncedPreference(key: String): Boolean = key in syncedPreferenceKeys || isCustomThemeKey(key)

    fun isCustomThemeKey(key: String): Boolean =
        key.startsWith("$customThemesScopeKey.") && key.length > customThemesScopeKey.length + 1

    // ───────────────────────────── 值规范化 ─────────────────────────────

    /**
     * 规范化一个 UI 键的 wire 值（镜像 `normalize_daemon_config_value`）；非法 → [Normalized.Rejected]（不发请求）。
     * daemon 键严格按 Rust 规则；偏好键对 bool / 整数 / 枚举做同样校验（文本不裁剪：偏好值原样保存）。
     */
    fun normalize(key: String, value: String): Normalized {
        val field = index[key] ?: return Normalized.Rejected(SettingsValidationError(key, "unknown config key: $key"))
        fun fail(message: String) = Normalized.Rejected(SettingsValidationError(key, "$key: $message"))
        return when (val kind = field.kind) {
            SettingField.Kind.ReadOnly -> fail("read-only")
            SettingField.Kind.Bool -> when (value.trim()) {
                "true", "1" -> Normalized.Ok("true")
                "false", "0" -> Normalized.Ok("false")
                else -> fail("expected boolean")
            }
            is SettingField.Kind.Integer -> {
                val parsed = parseInteger(value.trim()) ?: return fail("expected integer")
                if (parsed < kind.min || parsed > kind.max) fail("must be between ${kind.min} and ${kind.max}")
                else Normalized.Ok(parsed.toString())
            }
            is SettingField.Kind.Float -> {
                val parsed = value.trim().toDoubleOrNull()
                if (parsed == null || !parsed.isFinite() || parsed < kind.min) {
                    fail("must be a finite number >= ${JsonValue.formatDouble(kind.min)}")
                } else {
                    Normalized.Ok(JsonValue.formatDouble(parsed))
                }
            }
            is SettingField.Kind.Choice -> {
                val trimmed = value.trim()
                if (trimmed in kind.allowed) Normalized.Ok(trimmed) else fail("must be one of ${kind.allowed.joinToString()}")
            }
            SettingField.Kind.Text -> when {
                field.store != SettingField.Store.Daemon -> Normalized.Ok(value)
                key == "component_mirror_base" ->
                    normalizeMirrorBase(value)?.let { Normalized.Ok(it) } ?: fail("must be an https:// base URL")
                key == "global_user_agent" -> {
                    val trimmed = value.trim()
                    if (trimmed.any { (it.code < 32 && it != '\t') || it.code == 127 }) fail("must not contain control characters")
                    else Normalized.Ok(trimmed)
                }
                else -> Normalized.Ok(value.trim())
            }
        }
    }

    /** 严格整数（可带符号；同 Rust `i64::from_str`：不接受空白、小数、指数）。 */
    fun parseInteger(text: String): Long? {
        if (text.isEmpty()) return null
        val digits = if (text[0] == '+' || text[0] == '-') text.substring(1) else text
        if (digits.isEmpty() || !digits.all { it in '0'..'9' }) return null
        return text.toLongOrNull()
    }

    /** 组件镜像基址：空串 = 直连；非空必须是 `https://host[/path]`（无查询 / 片段 / 空白），去掉末尾 `/`。 */
    fun normalizeMirrorBase(value: String): String? {
        val trimmed = value.trim().trimEnd('/')
        if (trimmed.isEmpty()) return ""
        if (trimmed.any { it.isWhitespace() || it.code < 32 || it.code == 127 }) return null
        val scheme = "https://"
        if (!trimmed.lowercase().startsWith(scheme)) return null
        val rest = trimmed.substring(scheme.length)
        if ('?' in rest || '#' in rest) return null
        val authority = rest.substringBefore('/')
        val hostPort = authority.substringAfterLast('@')
        val host = hostPort.substringBefore(':')
        if (host.isEmpty()) return null
        return scheme + rest
    }

    // ───────────────────────────── wire ↔ JSON ─────────────────────────────

    /** 规范化后的 wire 值 → 偏好通道的 JSON 值（镜像 `daemonWireToJson`）。 */
    fun jsonForWire(wire: String, field: SettingField): JsonValue = when (field.kind) {
        SettingField.Kind.Bool -> JsonValue.of(wire == "true")
        is SettingField.Kind.Integer -> wire.toLongOrNull()?.let { JsonValue.of(it) } ?: JsonValue.Null
        is SettingField.Kind.Float -> wire.toDoubleOrNull()?.let { JsonValue.of(it) } ?: JsonValue.Null
        else -> JsonValue.Str(wire)
    }

    /** 偏好 JSON 值 → wire 字符串（UI 表单统一用字符串读取）；null / 数组 / 对象 → null。 */
    fun wireFromJson(value: JsonValue?): String? = when (value) {
        is JsonValue.Bool -> if (value.value) "true" else "false"
        is JsonValue.Num -> value.raw.toLongOrNull()?.toString()
            ?: value.raw.toDoubleOrNull()?.takeIf { it.isFinite() }?.let { JsonValue.formatDouble(it) }
        is JsonValue.Str -> value.value
        else -> null
    }
}

/** 一批 UI 键值编辑落到哪条 RPC（`agent.preferences.patch` 同步 / 本机，`daemon.config.patch`）。 */
class SettingsWritePlan {
    /** `daemon.config.patch.values`：不在同步目录的 daemon 键（已规范化 wire）。 */
    val daemon = LinkedHashMap<String, String>()

    /** `agent.preferences.patch {values}`（默认 sync）：同步键名 → JSON。含改走偏好通道的 daemon 键。 */
    val syncedPreferences = LinkedHashMap<String, JsonValue>()

    /** `agent.preferences.patch {values, sync: false}`：设备本地偏好。 */
    val localPreferences = LinkedHashMap<String, JsonValue>()

    /** 同步键名 → daemon 存储键（把偏好通道的写入对回 daemon 键的乐观值 / 确认）。 */
    val syncedDaemonKeys = LinkedHashMap<String, String>()

    val isEmpty: Boolean get() = daemon.isEmpty() && syncedPreferences.isEmpty() && localPreferences.isEmpty()

    /** 加入一个原始 JSON 偏好写入；是否同步由目录决定。 */
    fun addPreference(key: String, value: JsonValue) {
        if (SettingsCatalog.isSyncedPreference(key)) syncedPreferences[key] = value else localPreferences[key] = value
    }

    sealed interface Result {
        data class Ok(val plan: SettingsWritePlan) : Result
        data class Rejected(val error: SettingsValidationError) : Result
    }

    companion object {
        /**
         * 把 UI 键值编辑（daemon 配置键 / 目录内偏好键，值为 wire 字符串）路由到各通道。
         * 任一键非法则整批拒绝（与 `normalize_daemon_config_patch` 一致）。
         */
        fun make(edits: Map<String, String>): Result {
            val plan = SettingsWritePlan()
            for ((key, value) in edits.entries.sortedBy { it.key }) {
                val wire = when (val n = SettingsCatalog.normalize(key, value)) {
                    is Normalized.Rejected -> return Result.Rejected(n.error)
                    is Normalized.Ok -> n.wire
                }
                val field = SettingsCatalog.field(key)
                    ?: return Result.Rejected(SettingsValidationError(key, "unknown config key: $key"))
                when (field.store) {
                    SettingField.Store.Daemon -> {
                        val syncName = SettingsCatalog.daemonSyncNames[key]
                        if (syncName != null) {
                            plan.syncedPreferences[syncName] = SettingsCatalog.jsonForWire(wire, field)
                            plan.syncedDaemonKeys[syncName] = key
                        } else {
                            plan.daemon[key] = wire
                        }
                    }
                    SettingField.Store.Preference -> plan.addPreference(key, SettingsCatalog.jsonForWire(wire, field))
                }
            }
            return Result.Ok(plan)
        }

        /** 校验原始 JSON 偏好写入（集合范围键只承载分组，不接受值）。 */
        fun validatePreferenceKey(key: String): SettingsValidationError? = when {
            key == SettingsCatalog.customThemesScopeKey ->
                SettingsValidationError(key, "$key is a collection; write per-theme keys")
            key.isBlank() -> SettingsValidationError(key, "empty preference key")
            else -> null
        }
    }
}
