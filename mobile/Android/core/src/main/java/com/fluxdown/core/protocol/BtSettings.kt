package com.fluxdown.core.protocol

import java.util.Locale

/*
 * BitTorrent / eD2K 设置页的纯逻辑（行目录、可见性、时长换算、订阅状态、读数）。无 UI / 无主机依赖，便于单测。
 * 对应 iOS `BtSettingsLogic.swift`。文案由 app 层按 id / 枚举映射到 `R.string`。
 */

// ───────────────────────────── BitTorrent ─────────────────────────────

/** BT 页签（GPUI `bt.rs` 的 basic / tracker / seeding）。 */
enum class BtSettingsTab { General, Tracker, Seeding, Guard }

/**
 * BT 页的每一行（GPUI 顺序）。页面渲染与设置搜索共用同一份可见性判定。
 * 做种「时长 + 单位」两个 PC 行在移动端合成一行（各自仍写回两个键）。
 */
enum class BtSettingsRow(val slug: String, val tab: BtSettingsTab, val configKey: String, val unitKey: String? = null) {
    Enabled("enabled", BtSettingsTab.General, "bt_enabled"),
    Dht("dht", BtSettingsTab.General, "bt_enable_dht"),
    Upnp("upnp", BtSettingsTab.General, "bt_enable_upnp"),
    Utp("utp", BtSettingsTab.General, "bt_enable_utp"),
    PortStart("portStart", BtSettingsTab.General, "bt_port_start"),
    PortEnd("portEnd", BtSettingsTab.General, "bt_port_end"),
    MseMode("mseMode", BtSettingsTab.General, "bt_mse_mode"),
    CustomTrackers("customTrackers", BtSettingsTab.Tracker, "bt_custom_trackers"),
    TrackerSub("trackerSub", BtSettingsTab.Tracker, "bt_tracker_sub_enabled"),
    TrackerSubUrls("trackerSubUrls", BtSettingsTab.Tracker, "bt_tracker_sub_urls"),
    TrackerSubStatus("trackerSubStatus", BtSettingsTab.Tracker, "bt_tracker_sub_cache"),
    SeedEnabled("seedEnabled", BtSettingsTab.Seeding, "bt_seed_enabled"),
    SeedMaxActive("seedMaxActive", BtSettingsTab.Seeding, "bt_seed_max_active"),
    AutoReseed("autoReseed", BtSettingsTab.Seeding, "bt_auto_reseed"),
    SeedRatio("seedRatio", BtSettingsTab.Seeding, "bt_seed_ratio_limit"),
    SeedPostRatio("seedPostRatio", BtSettingsTab.Seeding, "bt_seed_post_ratio_limit"),
    SeedTimeLimit("seedTimeLimit", BtSettingsTab.Seeding, "bt_seed_time_limit_minutes", "bt_seed_time_limit_unit"),
    SeedInactiveTimeLimit(
        "seedInactiveTimeLimit", BtSettingsTab.Seeding, "bt_seed_inactive_time_limit_minutes",
        "bt_seed_inactive_time_limit_unit",
    ),
    SeedOperator("seedOperator", BtSettingsTab.Seeding, "bt_seed_limit_operator"),
    SeedThenAction("seedThenAction", BtSettingsTab.Seeding, "bt_seed_then_action"),
    GuardEnabled("guardEnabled", BtSettingsTab.Guard, "bt_guard_enabled"),
    GuardRuleExcessive("guardRuleExcessive", BtSettingsTab.Guard, "bt_guard_rule_excessive"),
    GuardRuleRerequest("guardRuleRerequest", BtSettingsTab.Guard, "bt_guard_rule_rerequest"),
    GuardRuleRewind("guardRuleRewind", BtSettingsTab.Guard, "bt_guard_rule_rewind"),
    GuardRuleLag("guardRuleLag", BtSettingsTab.Guard, "bt_guard_rule_lag"),
    GuardRuleFingerprint("guardRuleFingerprint", BtSettingsTab.Guard, "bt_guard_rule_fingerprint"),
    GuardExcessRatio("guardExcessRatio", BtSettingsTab.Guard, "bt_guard_excess_ratio_pct"),
    GuardMinTorrent("guardMinTorrent", BtSettingsTab.Guard, "bt_guard_min_torrent_mib"),
    GuardRewindTolerance("guardRewindTolerance", BtSettingsTab.Guard, "bt_guard_rewind_tolerance_pct"),
    GuardThrottle("guardThrottle", BtSettingsTab.Guard, "bt_guard_throttle_kib"),
    GuardBanBase("guardBanBase", BtSettingsTab.Guard, "bt_guard_ban_base_hours"),
    GuardBanMax("guardBanMax", BtSettingsTab.Guard, "bt_guard_ban_max_days"),
    GuardDownloadingMax("guardDownloadingMax", BtSettingsTab.Guard, "bt_guard_downloading_max_action"),
    GuardSkipPrivate("guardSkipPrivate", BtSettingsTab.Guard, "bt_guard_skip_private"),
    GuardSkipLan("guardSkipLan", BtSettingsTab.Guard, "bt_guard_skip_lan"),
    GuardIpv6Prefix("guardIpv6Prefix", BtSettingsTab.Guard, "bt_guard_ipv6_prefix"),
    GuardCloudEnabled("guardCloudEnabled", BtSettingsTab.Guard, "bt_guard_cloud_enabled"),
    GuardCloudMode("guardCloudMode", BtSettingsTab.Guard, "bt_guard_cloud_mode"),
    GuardFilterUrls("guardFilterUrls", BtSettingsTab.Guard, "bt_guard_filter_urls"),
    GuardReportEnabled("guardReportEnabled", BtSettingsTab.Guard, "bt_guard_report_enabled");

    /** 与 iOS 一致的行 id（`bt.<slug>`）。 */
    val id: String get() = "bt.$slug"

    fun isVisible(form: SettingsForm): Boolean {
        if (!form.has(configKey)) return false
        if (this != Enabled && !form.bool(Enabled.configKey)) return false
        return when (tab) {
            BtSettingsTab.Seeding -> this == SeedEnabled || form.bool(SeedEnabled.configKey)
            BtSettingsTab.Guard -> when (this) {
                GuardEnabled -> true
                GuardCloudMode -> form.bool(GuardEnabled.configKey) && form.bool(GuardCloudEnabled.configKey)
                else -> form.bool(GuardEnabled.configKey)
            }
            else -> true
        }
    }

    companion object {
        /** 总开关关闭时仅常规页签（且仅总开关一行）可用。 */
        fun visibleTabs(form: SettingsForm): List<BtSettingsTab> =
            if (form.bool(Enabled.configKey)) BtSettingsTab.entries else listOf(BtSettingsTab.General)

        fun visible(tab: BtSettingsTab, form: SettingsForm): List<BtSettingsRow> =
            entries.filter { it.tab == tab && it.isVisible(form) }

        /** 搜索定位：行 id 所在页签（非 BT 行 → null）。 */
        fun tabForRowId(id: String): BtSettingsTab? = entries.firstOrNull { it.id == id }?.tab
    }
}

/** 监听端口区间。 */
object BtPortRange {
    val bounds: IntRange = 1..65_535

    /** 结束端口不得小于起始端口。 */
    fun isValid(start: Int, end: Int): Boolean = end >= start
}

/** MSE（协议加密）选项的 wire 值；未知值按「启用」处理。 */
object BtMseMode {
    const val DEFAULT = "enabled"
    val all: List<String> get() = SettingsCatalog.btMseModes
    fun normalize(wire: String?): String = wire?.trim()?.takeIf { it in all } ?: DEFAULT
}

// ───────────────────────────── 做种时长 ─────────────────────────────

/**
 * 时长单位（`bt_seed_*_time_limit_unit`）。数值键始终以**分钟**落库，单位键记录设置页的展示单位
 * （daemon `bt_config_from_map` 直接取分钟值）。
 */
enum class BtDurationUnit(val wire: String, val factor: Long) {
    Minutes("minutes", 1),
    Hours("hours", 60),
    Days("days", 1_440);

    companion object {
        /** 未知 / 缺失取分钟（目录默认）。 */
        fun fromWire(wire: String?): BtDurationUnit {
            val w = wire?.trim { it == ' ' || it == '\t' }
            return entries.firstOrNull { it.wire == w } ?: Minutes
        }
    }
}

object BtDuration {
    private val decimal = Regex("""\d+(\.\d*)?|\.\d+""")

    /** `minutes` 在 `unit` 下的显示文本：最多两位小数、去掉多余的 0；≤ 0 → 空串（显示「关闭」占位）。 */
    fun text(minutes: Long, unit: BtDurationUnit): String {
        if (minutes <= 0) return ""
        var s = String.format(Locale.ROOT, "%.2f", minutes.toDouble() / unit.factor)
        if ('.' in s) {
            s = s.trimEnd('0')
            if (s.endsWith('.')) s = s.dropLast(1)
        }
        return s.ifEmpty { "0" }
    }

    /** 文本（`unit` 下的数值）→ 分钟。空串 = 0（关闭）；负数 / 非数字 = null（非法）。 */
    fun minutes(text: String, unit: BtDurationUnit): Long? {
        val normalized = text.trim { it == ' ' || it == '\t' }.replace(',', '.')
        if (normalized.isEmpty()) return 0
        if (!decimal.matches(normalized)) return null
        val value = normalized.toDoubleOrNull() ?: return null
        if (!value.isFinite() || value < 0) return null
        val raw = value * unit.factor
        return if (raw >= Long.MAX_VALUE.toDouble()) Long.MAX_VALUE else Math.round(raw)
    }

    /** 以 `unit` 为步长（1 个单位）步进后的分钟数，下限 0。 */
    fun stepped(minutes: Long, unit: BtDurationUnit, up: Boolean): Long {
        val current = minutes.toDouble() / unit.factor
        val next = maxOf(0.0, current + if (up) 1 else -1)
        return Math.round(next * unit.factor)
    }
}

// ───────────────────────────── eD2K ─────────────────────────────

enum class Ed2kSettingsTab { General, Servers }

/** eD2K 页的每一行。`nodesDatUrl` 为 Android 补充行（GPUI 有、iOS 未做）。 */
enum class Ed2kSettingsRow(val slug: String, val tab: Ed2kSettingsTab, val configKey: String) {
    Kad("kad", Ed2kSettingsTab.General, "ed2k_enable_kad"),
    Upnp("upnp", Ed2kSettingsTab.General, "ed2k_enable_upnp"),
    ListenPort("listenPort", Ed2kSettingsTab.General, "ed2k_listen_port"),
    ServerList("serverList", Ed2kSettingsTab.Servers, "ed2k_server_list"),
    ServerSub("serverSub", Ed2kSettingsTab.Servers, "ed2k_server_sub_enabled"),
    ServerSubUrls("serverSubUrls", Ed2kSettingsTab.Servers, "ed2k_server_sub_urls"),
    NodesDatUrl("nodesDatUrl", Ed2kSettingsTab.Servers, "ed2k_nodes_dat_url"),
    ServerSubStatus("serverSubStatus", Ed2kSettingsTab.Servers, "ed2k_server_sub_cache");

    val id: String get() = "ed2k.$slug"

    fun isVisible(form: SettingsForm): Boolean = form.has(configKey)

    companion object {
        fun visible(tab: Ed2kSettingsTab, form: SettingsForm): List<Ed2kSettingsRow> =
            entries.filter { it.tab == tab && it.isVisible(form) }

        fun tabForRowId(id: String): Ed2kSettingsTab? = entries.firstOrNull { it.id == id }?.tab
    }
}

// ───────────────────────────── 订阅 ─────────────────────────────

/** 订阅种类：存储键与列表格式。 */
enum class SubscriptionKind(
    val cacheKey: String,
    val updatedAtKey: String,
    val format: SubscriptionListFormat,
    val rowId: String,
) {
    BtTrackers("bt_tracker_sub_cache", "bt_tracker_sub_updated_at", SubscriptionListFormat.Lines, BtSettingsRow.TrackerSubStatus.id),
    Ed2kServers("ed2k_server_sub_cache", "ed2k_server_sub_updated_at", SubscriptionListFormat.Comma, Ed2kSettingsRow.ServerSubStatus.id),
}

/** 订阅状态的显示值：配置快照（缓存条数 / 更新时间）与本次刷新结果合并。 */
data class SubscriptionStatusModel(val count: Int, /** Unix 秒；0 = 从未更新。 */ val updatedAt: Long) {
    companion object {
        /** 刷新结果可能先于配置快照到达：更新时间更晚的一方为准；失败的刷新不改变显示值（daemon 沿用旧缓存）。 */
        fun make(kind: SubscriptionKind, form: SettingsForm, fresh: SubscriptionRefreshOutcome?): SubscriptionStatusModel {
            val storedCount = kind.format.count(form.string(kind.cacheKey))
            val storedAt = maxOf(0L, form.long(kind.updatedAtKey, 0))
            if (fresh == null || !fresh.success || fresh.updatedAt <= storedAt) {
                return SubscriptionStatusModel(storedCount, storedAt)
            }
            return SubscriptionStatusModel(fresh.count.coerceIn(0, Int.MAX_VALUE.toLong()).toInt(), fresh.updatedAt)
        }
    }
}

/** 订阅更新时间的展示分档：一周内相对时间，更早显示绝对日期时间（由 app 层按语言格式化）。 */
sealed interface SubscriptionTime {
    data object JustNow : SubscriptionTime
    data class MinutesAgo(val n: Long) : SubscriptionTime
    data class HoursAgo(val n: Long) : SubscriptionTime
    data class DaysAgo(val n: Long) : SubscriptionTime
    data object Absolute : SubscriptionTime

    companion object {
        private const val WEEK = 7 * 86_400L

        fun bucket(unix: Long, now: Long): SubscriptionTime {
            val age = now - unix
            return when {
                age >= WEEK -> Absolute
                age < 60 -> JustNow
                age < 3_600 -> MinutesAgo(age / 60)
                age < 86_400 -> HoursAgo(age / 3_600)
                else -> DaysAgo(age / 86_400)
            }
        }
    }
}

// ───────────────────────────── 读数 ─────────────────────────────

object BtReadout {
    /** 设置首页读数：`DHT · 做种 · 6881–6891`（只列当前生效项）；配置未加载 → null。 */
    fun text(form: SettingsForm, seedingLabel: String): String? {
        if (!form.isLoaded) return null
        val parts = ArrayList<String>()
        if (form.bool(BtSettingsRow.Dht.configKey)) parts += "DHT"
        if (form.bool(BtSettingsRow.SeedEnabled.configKey)) parts += seedingLabel
        val start = form.int(BtSettingsRow.PortStart.configKey, 6881)
        val end = form.int(BtSettingsRow.PortEnd.configKey, 6891)
        parts += if (start == end) "$start" else "$start–$end"
        return parts.joinToString(" · ")
    }
}

object Ed2kReadout {
    /** 手动列表 + 订阅缓存去重（忽略大小写）后的服务器数。 */
    fun serverCount(form: SettingsForm): Int {
        val merged = SubscriptionListFormat.Comma.entries(form.string(Ed2kSettingsRow.ServerList.configKey)) +
            SubscriptionListFormat.Comma.entries(form.string(Ed2kSettingsRow.ServerSubStatus.configKey))
        return merged.map { it.lowercase() }.toSet().size
    }

    /** 设置首页读数：`Kad · 12 个服务器`；配置未加载 / 无可报内容 → null。 */
    fun text(form: SettingsForm, serverCountText: (Int) -> String): String? {
        if (!form.isLoaded) return null
        val parts = ArrayList<String>()
        if (form.bool(Ed2kSettingsRow.Kad.configKey)) parts += "Kad"
        val count = serverCount(form)
        if (count > 0) parts += serverCountText(count)
        return if (parts.isEmpty()) null else parts.joinToString(" · ")
    }
}

/** 分享率输入（`bt_seed_ratio_limit` / `bt_seed_post_ratio_limit`，0 = 关闭）。 */
object BtRatio {
    private val decimal = Regex("""\d+(\.\d*)?|\.\d+""")

    /** 显示文本：0 → 空串（显示「关闭」占位）；否则去掉多余的 0。 */
    fun text(value: Double): String {
        if (!value.isFinite() || value <= 0.0) return ""
        return java.math.BigDecimal.valueOf(value).stripTrailingZeros().toPlainString()
    }

    /** 输入 → wire：空串 = "0"（关闭）；非数字 / 负数 = null（非法）。接受逗号小数点。 */
    fun wire(text: String): String? {
        val normalized = text.trim { it == ' ' || it == '\t' }.replace(',', '.')
        if (normalized.isEmpty()) return "0"
        if (!decimal.matches(normalized)) return null
        val value = normalized.toDoubleOrNull()?.takeIf { it.isFinite() } ?: return null
        return java.math.BigDecimal.valueOf(value).stripTrailingZeros().toPlainString()
    }
}
