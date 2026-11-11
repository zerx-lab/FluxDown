package com.fluxdown.core.protocol

import java.util.Locale

/*
 * BitTorrent / eD2K 设置页的纯逻辑（行目录、可见性、时长换算、订阅状态、读数）。无 UI / 无主机依赖，便于单测。
 * 对应 iOS `BtSettingsLogic.swift`。文案由 app 层按 id / 枚举映射到 `R.string`。
 */

// ───────────────────────────── BitTorrent ─────────────────────────────

/** BT 页签（GPUI `bt.rs` 的 basic / connection / tracker / seeding + 防吸血）。 */
enum class BtSettingsTab { General, Connection, Tracker, Seeding, Guard }

/**
 * BT 页的每一行（GPUI 顺序：页签 → 分组 → 行）。页面渲染与设置搜索共用同一份可见性判定。
 * 做种「时长 + 单位」两个 PC 行在移动端合成一行（各自仍写回两个键）。
 * [PortMappingStatus] 是只读状态行（`daemon.bt.portStatus`），挂在 `bt_enable_upnp` 上：仅映射开启时显示。
 */
enum class BtSettingsRow(val slug: String, val tab: BtSettingsTab, val configKey: String, val unitKey: String? = null) {
    // 常规：总开关 · 新任务
    Enabled("enabled", BtSettingsTab.General, "bt_enabled"),
    PieceOrder("pieceOrder", BtSettingsTab.General, "bt_piece_order"),
    FirstLastPiecesFirst("firstLastPiecesFirst", BtSettingsTab.General, "bt_first_last_pieces_first"),
    SelectionAutoDefault("selectionAutoDefault", BtSettingsTab.General, "bt_selection_auto_default"),
    // 连接：监听
    PortStart("portStart", BtSettingsTab.Connection, "bt_port_start"),
    PortEnd("portEnd", BtSettingsTab.Connection, "bt_port_end"),
    Upnp("upnp", BtSettingsTab.Connection, "bt_enable_upnp"),
    PortMappingStatus("portMappingStatus", BtSettingsTab.Connection, "bt_enable_upnp"),
    // 连接：传输
    Utp("utp", BtSettingsTab.Connection, "bt_enable_utp"),
    UtpCongestion("utpCongestion", BtSettingsTab.Connection, "bt_utp_congestion"),
    MseMode("mseMode", BtSettingsTab.Connection, "bt_mse_mode"),
    UploadSlots("uploadSlots", BtSettingsTab.Connection, "bt_upload_slots"),
    // 连接：节点与数据来源
    Dht("dht", BtSettingsTab.Connection, "bt_enable_dht"),
    Lsd("lsd", BtSettingsTab.Connection, "bt_enable_lsd"),
    Webseed("webseed", BtSettingsTab.Connection, "bt_enable_webseed"),
    // 连接：连接上限
    MaxConnections("maxConnections", BtSettingsTab.Connection, "bt_max_connections"),
    MaxConnectionsPerTorrent("maxConnectionsPerTorrent", BtSettingsTab.Connection, "bt_max_connections_per_torrent"),
    // 连接：I2P
    I2pEnabled("i2pEnabled", BtSettingsTab.Connection, "bt_i2p_enabled"),
    I2pSamHost("i2pSamHost", BtSettingsTab.Connection, "bt_i2p_sam_host"),
    I2pSamPort("i2pSamPort", BtSettingsTab.Connection, "bt_i2p_sam_port"),
    I2pMixed("i2pMixed", BtSettingsTab.Connection, "bt_i2p_mixed"),
    // Tracker：列表与订阅 · 汇报策略
    CustomTrackers("customTrackers", BtSettingsTab.Tracker, "bt_custom_trackers"),
    TrackerSub("trackerSub", BtSettingsTab.Tracker, "bt_tracker_sub_enabled"),
    TrackerSubUrls("trackerSubUrls", BtSettingsTab.Tracker, "bt_tracker_sub_urls"),
    TrackerSubStatus("trackerSubStatus", BtSettingsTab.Tracker, "bt_tracker_sub_cache"),
    AnnounceAllTiers("announceAllTiers", BtSettingsTab.Tracker, "bt_announce_all_tiers"),
    AnnounceAllTrackers("announceAllTrackers", BtSettingsTab.Tracker, "bt_announce_all_trackers"),
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
            BtSettingsTab.Connection -> when (this) {
                PortMappingStatus -> form.bool(Upnp.configKey)
                UtpCongestion -> form.bool(Utp.configKey)
                I2pSamHost, I2pSamPort, I2pMixed -> form.bool(I2pEnabled.configKey)
                else -> true
            }
            BtSettingsTab.Seeding -> this == SeedEnabled || form.bool(SeedEnabled.configKey)
            BtSettingsTab.Guard -> when (this) {
                GuardEnabled -> true
                GuardCloudMode -> form.bool(GuardEnabled.configKey) && form.bool(GuardCloudEnabled.configKey)
                else -> form.bool(GuardEnabled.configKey)
            }
            else -> true
        }
    }

    /** 页内分组（GPUI 的 section）；页面按组渲染、搜索按组定位。 */
    val group: BtSettingsGroup
        get() = when (this) {
            Enabled -> BtSettingsGroup.Basic
            PieceOrder, FirstLastPiecesFirst, SelectionAutoDefault -> BtSettingsGroup.NewTasks
            PortStart, PortEnd, Upnp, PortMappingStatus -> BtSettingsGroup.Listen
            Utp, UtpCongestion, MseMode, UploadSlots -> BtSettingsGroup.Transport
            Dht, Lsd, Webseed -> BtSettingsGroup.Sources
            MaxConnections, MaxConnectionsPerTorrent -> BtSettingsGroup.Limits
            I2pEnabled, I2pSamHost, I2pSamPort, I2pMixed -> BtSettingsGroup.I2p
            CustomTrackers -> BtSettingsGroup.TrackerList
            TrackerSub, TrackerSubUrls, TrackerSubStatus -> BtSettingsGroup.TrackerSub
            AnnounceAllTiers, AnnounceAllTrackers -> BtSettingsGroup.Announce
            SeedEnabled, SeedMaxActive, AutoReseed -> BtSettingsGroup.SeedMain
            SeedRatio, SeedPostRatio, SeedTimeLimit, SeedInactiveTimeLimit, SeedOperator, SeedThenAction ->
                BtSettingsGroup.SeedLimits
            else -> BtSettingsGroup.Guard
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

/** 新建任务选片顺序的 wire 值；未知值按「最稀有优先」处理。 */
object BtPieceOrder {
    const val DEFAULT = "rarest"
    val all: List<String> get() = SettingsCatalog.btPieceOrders
    fun normalize(wire: String?): String = wire?.trim()?.takeIf { it in all } ?: DEFAULT
}

/** uTP 拥塞控制的 wire 值；未知值按 LEDBAT 处理。 */
object BtUtpCongestion {
    const val DEFAULT = "ledbat"
    val all: List<String> get() = SettingsCatalog.btUtpCongestions
    fun normalize(wire: String?): String = wire?.trim()?.takeIf { it in all } ?: DEFAULT
}

/** BT 页内分组（GPUI `bt.rs` 的 section 顺序；页面按组渲染，搜索按组定位）。 */
enum class BtSettingsGroup {
    Basic, NewTasks,
    Listen, Transport, Sources, Limits, I2p,
    TrackerList, TrackerSub, Announce,
    SeedMain, SeedLimits,
    Guard,
}

// ───────────────────────────── 端口映射状态 ─────────────────────────────

/** `daemon.bt.portStatus` 结果（`BtPortStatusDto`）；旧 daemon 缺省字段取 0 / 空。 */
data class BtPortStatus(val running: Boolean, val listenPort: Long, val mappings: List<BtPortMapping>) {
    companion object {
        fun fromJson(v: JsonValue?): BtPortStatus = BtPortStatus(
            running = v.bool("running"),
            listenPort = v.long("listenPort"),
            mappings = v.list("mappings").map(BtPortMapping::fromJson),
        )
    }
}

/** 一个端口的映射状态（`BtPortMappingDto`）。 */
data class BtPortMapping(
    val protocol: String,
    val internalPort: Long,
    val externalPort: Long,
    val method: String,
    val externalIp: String,
    val state: String,
    val message: String,
) {
    companion object {
        fun fromJson(v: JsonValue?): BtPortMapping = BtPortMapping(
            protocol = v.str("protocol"),
            internalPort = v.long("internalPort"),
            externalPort = v.long("externalPort"),
            method = v.str("method"),
            externalIp = v.str("externalIp"),
            state = v.str("state"),
            message = v.str("message"),
        )
    }
}

/** 端口映射状态行的一段（纯数据，文案由 app 层按语言查表；同 GPUI `PortSegment`）。 */
sealed interface BtPortSegment {
    /** BT 会话未运行：尚未请求映射。 */
    data object Idle : BtPortSegment

    /** [port] 形如 `TCP 6881` / `UDP 6881 → 50123`。 */
    data class Mapping(val port: String, val state: BtPortSegmentState) : BtPortSegment

    data class ExternalIp(val ip: String) : BtPortSegment
}

sealed interface BtPortSegmentState {
    /** 附显示用方法名（`UPnP` / `NAT-PMP` / `PCP`，未知取原值）。 */
    data class Mapped(val method: String) : BtPortSegmentState
    data object Pending : BtPortSegmentState

    /** [message] 已去首尾空白；显示时压成单行，长按看全文。 */
    data class Failed(val message: String) : BtPortSegmentState

    /** 未识别的 wire 状态，原样显示。 */
    data class Other(val raw: String) : BtPortSegmentState
}

object BtPortMappingView {
    /** 未运行一段「空闲」；否则每个映射一段，任一映射带外网地址时末尾追加一段（取第一个非空地址）。 */
    fun segments(status: BtPortStatus): List<BtPortSegment> {
        if (!status.running) return listOf(BtPortSegment.Idle)
        val out = ArrayList<BtPortSegment>(status.mappings.size + 1)
        for (m in status.mappings) {
            var port = "${m.protocol.uppercase(Locale.ROOT)} ${m.internalPort}"
            if (m.state == "mapped" && m.externalPort != 0L && m.externalPort != m.internalPort) {
                port += " → ${m.externalPort}"
            }
            val state = when (m.state) {
                "mapped" -> BtPortSegmentState.Mapped(methodLabel(m.method))
                "pending" -> BtPortSegmentState.Pending
                "failed" -> BtPortSegmentState.Failed(m.message.trim())
                else -> BtPortSegmentState.Other(m.state)
            }
            out += BtPortSegment.Mapping(port, state)
        }
        status.mappings.map { it.externalIp.trim() }.firstOrNull { it.isNotEmpty() }?.let {
            out += BtPortSegment.ExternalIp(it)
        }
        return out
    }

    /** wire 方法名 → 显示名；未知值原样返回。 */
    fun methodLabel(method: String): String = when (method) {
        "upnp" -> "UPnP"
        "natPmp" -> "NAT-PMP"
        "pcp" -> "PCP"
        else -> method
    }

    /** 失败原因压成单行（多行 / 连续空白合成一个空格）。 */
    fun singleLine(message: String): String = message.split(WHITESPACE).filter { it.isNotEmpty() }.joinToString(" ")

    /** 设置页「连接」页签可见时的轮询间隔。 */
    const val POLL_INTERVAL_MS = 5_000L

    private val WHITESPACE = Regex("\\s+")
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

enum class Ed2kSettingsTab { General, Servers, Kad }

/**
 * eD2K 页的每一行（与 GPUI / Web 同序同键）。`IdCheck` 是动作行，不写配置：借监听端口键判定可见性
 * （daemon 配置已加载才可检测）。
 */
enum class Ed2kSettingsRow(val slug: String, val tab: Ed2kSettingsTab, val configKey: String) {
    Kad("kad", Ed2kSettingsTab.General, "ed2k_enable_kad"),
    Upnp("upnp", Ed2kSettingsTab.General, "ed2k_enable_upnp"),
    ListenPort("listenPort", Ed2kSettingsTab.General, "ed2k_listen_port"),
    IdCheck("idCheck", Ed2kSettingsTab.General, "ed2k_listen_port"),
    ServerList("serverList", Ed2kSettingsTab.Servers, "ed2k_server_list"),
    ServerSub("serverSub", Ed2kSettingsTab.Servers, "ed2k_server_sub_enabled"),
    ServerSubUrls("serverSubUrls", Ed2kSettingsTab.Servers, "ed2k_server_sub_urls"),
    ServerSubStatus("serverSubStatus", Ed2kSettingsTab.Servers, "ed2k_server_sub_cache"),
    NodesSubUrls("nodesSubUrls", Ed2kSettingsTab.Kad, "ed2k_nodes_dat_url"),
    NodesSubStatus("nodesSubStatus", Ed2kSettingsTab.Kad, "ed2k_nodes_dat_count");

    val id: String get() = "ed2k.$slug"

    fun isVisible(form: SettingsForm): Boolean = form.has(configKey)

    companion object {
        fun visible(tab: Ed2kSettingsTab, form: SettingsForm): List<Ed2kSettingsRow> =
            entries.filter { it.tab == tab && it.isVisible(form) }

        fun tabForRowId(id: String): Ed2kSettingsTab? = entries.firstOrNull { it.id == id }?.tab
    }
}

// ───────────────────────────── 订阅 ─────────────────────────────

/**
 * 订阅种类：存储键与列表格式。`format == null` 表示 [cacheKey] 直接存整数条数（Kad 节点：daemon 缓存的是
 * 二进制 nodes.dat，只公开引导节点数）。
 */
enum class SubscriptionKind(
    val cacheKey: String,
    val updatedAtKey: String,
    val format: SubscriptionListFormat?,
    val rowId: String,
) {
    BtTrackers("bt_tracker_sub_cache", "bt_tracker_sub_updated_at", SubscriptionListFormat.Lines, BtSettingsRow.TrackerSubStatus.id),
    Ed2kServers("ed2k_server_sub_cache", "ed2k_server_sub_updated_at", SubscriptionListFormat.Comma, Ed2kSettingsRow.ServerSubStatus.id),
    Ed2kNodes("ed2k_nodes_dat_count", "ed2k_nodes_dat_updated_at", null, Ed2kSettingsRow.NodesSubStatus.id),
}

/** 订阅状态的显示值：配置快照（缓存条数 / 更新时间）与本次刷新结果合并。 */
data class SubscriptionStatusModel(val count: Int, /** Unix 秒；0 = 从未更新。 */ val updatedAt: Long) {
    companion object {
        /** 刷新结果可能先于配置快照到达：更新时间更晚的一方为准；失败的刷新不改变显示值（daemon 沿用旧缓存）。 */
        fun make(kind: SubscriptionKind, form: SettingsForm, fresh: SubscriptionRefreshOutcome?): SubscriptionStatusModel {
            val storedCount = kind.format?.count(form.string(kind.cacheKey))
                ?: form.int(kind.cacheKey, 0).coerceAtLeast(0)
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
