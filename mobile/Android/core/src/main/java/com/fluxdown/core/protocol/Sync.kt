package com.fluxdown.core.protocol

import kotlin.math.max

/*
 * 云端配置同步（`agent.sync.get|enable|disable|now|setLocalOnly`）。镜像 `native/protocol/src/agent.rs::SyncStatusDto`
 * / `SyncLocalOnlyParams`（同 iOS `Sync.swift`）；范围分组与状态优先级镜像 Web `account/syncGroups.ts`
 * （键表 = `settings.rs::SYNC_SETTING_SPECS`，含 19 个 `bt.guard.*`，与 [SettingsCatalog] 的同步目录一一对应）。
 */

/** `agent.sync` 分区 / `agent.sync.get|setLocalOnly` 结果。 */
data class SyncStatusDto(
    val enabled: Boolean = false,
    val revision: Long = 0,
    val dirtyKeys: List<String> = emptyList(),
    /** 诊断用原始错误文本（UI 优先按 [lastErrorReason] 展示本地化文案）。 */
    val lastError: String? = null,
    /** `ErrorReason` wire 名。 */
    val lastErrorReason: String? = null,
    /** 同步事件流当前已连通。 */
    val connected: Boolean = false,
    /** 因不可自动恢复的错误（设备超限 / 设备未受信任）暂停自动重试，需用户处理后重新启用。 */
    val halted: Boolean = false,
    /** 最近一次成功完成拉取 + 推送的时间（Unix 毫秒）。 */
    val lastSyncedAtUnixMs: Long? = null,
    /** 本设备不参与云同步的同步目录键。 */
    val localOnlyKeys: List<String> = emptyList(),
) {
    companion object {
        /** 分区缺失 / `null` / 非对象 → 全默认（未启用）；旧 agent 不下发的字段取默认。 */
        fun fromJson(v: JsonValue?): SyncStatusDto {
            if (v !is JsonValue.Obj) return SyncStatusDto()
            return SyncStatusDto(
                enabled = v.bool("enabled"),
                revision = v.long("revision"),
                dirtyKeys = v.strings("dirtyKeys"),
                lastError = v.strOrNull("lastError"),
                lastErrorReason = v.strOrNull("lastErrorReason"),
                connected = v.bool("connected"),
                halted = v.bool("halted"),
                lastSyncedAtUnixMs = v.longOrNull("lastSyncedAtUnixMs"),
                localOnlyKeys = v.strings("localOnlyKeys"),
            )
        }
    }
}

/** `agent.sync.setLocalOnly` 参数：把一组同步目录键设为本设备专属 / 恢复同步。 */
data class SyncLocalOnlyParams(val keys: List<String>, val localOnly: Boolean) {
    fun toJson(): JsonValue = jsonObject("keys" to keys, "localOnly" to localOnly)
}

/** 同步状态阶段（优先级：未启用 → 暂停 → 失败 → 连接中 → 同步中 → 已同步）。 */
enum class SyncPhase { Off, Halted, Error, Connecting, Syncing, Synced }

/** 分组当前状态：全部参与同步 / 全部本设备专属 / 混合（其他客户端逐键设置过）。 */
enum class SyncGroupState { Sync, Local, Mixed }

/** 「在此设备同步的范围」分组 id（顺序 = 页面顺序）。 */
enum class SyncGroupId(val wire: String) {
    Appearance("appearance"), General("general"), Ui("ui"), Download("download"), Bt("bt"), Ed2k("ed2k"), Categories("categories"),
}

/** 「在此设备同步的范围」分组。 */
class SyncGroup(val id: SyncGroupId, val labelKey: String, val keys: List<String>) {
    fun state(localOnlyKeys: Collection<String>): SyncGroupState {
        val local = localOnlyKeys.toHashSet()
        val count = keys.count { it in local }
        return when (count) {
            0 -> SyncGroupState.Sync
            keys.size -> SyncGroupState.Local
            else -> SyncGroupState.Mixed
        }
    }

    /**
     * 点击分组开关后应发送的 `setLocalOnly` 参数：仅「全部参与同步」时切为本设备专属，
     * 其余（本设备专属 / 混合）一律恢复同步。
     */
    fun toggleParams(from: SyncGroupState): SyncLocalOnlyParams =
        SyncLocalOnlyParams(keys = keys, localOnly = from == SyncGroupState.Sync)
}

object SyncRules {
    val groups: List<SyncGroup> = listOf(
        SyncGroup(
            SyncGroupId.Appearance, "syncScopeAppearance",
            listOf(
                "appearance.theme_mode", "appearance.dark_theme", "appearance.light_theme",
                "appearance.color_scheme", "appearance.custom_color", "appearance.custom_themes",
                "appearance.file_icon_pack",
            ),
        ),
        SyncGroup(
            SyncGroupId.General, "syncScopeGeneral",
            listOf(
                "general.locale", "general.update_channel", "general.auto_check_update", "general.clipboard_watch",
                "general.floating_ball_enabled", "general.floating_ball_active_only",
            ),
        ),
        SyncGroup(
            SyncGroupId.Ui, "syncScopeUi",
            listOf(
                "ui.show_sidebar_status", "ui.show_sidebar_queues", "ui.show_sidebar_category", "ui.show_sidebar_rss",
                "ui.show_activity_rss", "ui.show_activity_webhooks", "ui.show_activity_theme", "ui.show_activity_account",
                "ui.show_titlebar_pause_all", "ui.show_titlebar_resume_all", "ui.show_titlebar_settings",
                "ui.show_titlebar_theme",
            ),
        ),
        SyncGroup(
            SyncGroupId.Download, "syncScopeDownload",
            listOf(
                "download.max_concurrent_tasks", "download.default_segments", "download.auto_max_connections",
                "download.cdn_multi_enabled", "download.cdn_max_nodes", "download.speed_limit_bytes",
                "download.max_auto_retries", "download.auto_retry_delay_secs", "download.auto_resume_on_start",
                "download.remember_last_save_dir", "download.use_server_time", "download.global_user_agent",
                "download.notify_on_complete", "download.silent_download", "download.keep_awake",
            ),
        ),
        SyncGroup(
            SyncGroupId.Bt, "syncScopeBt",
            listOf(
                "bt.enabled", "bt.enable_dht", "bt.enable_upnp", "bt.enable_utp", "bt.custom_trackers", "bt.tracker_sub_enabled",
                "bt.tracker_sub_urls",
                "bt.seed_ratio_limit", "bt.seed_post_ratio_limit", "bt.seed_time_limit_minutes",
                "bt.seed_inactive_time_limit_minutes", "bt.seed_limit_operator", "bt.seed_then_action",
                "bt.seed_max_active",
                "bt.guard.enabled", "bt.guard.rule_excessive", "bt.guard.rule_rerequest", "bt.guard.rule_rewind",
                "bt.guard.rule_lag", "bt.guard.rule_fingerprint", "bt.guard.cloud_mode", "bt.guard.excess_ratio_pct",
                "bt.guard.min_torrent_mib", "bt.guard.rewind_tolerance_pct", "bt.guard.throttle_kib",
                "bt.guard.ban_base_hours", "bt.guard.ban_max_days", "bt.guard.downloading_max_action",
                "bt.guard.skip_private", "bt.guard.skip_lan", "bt.guard.ipv6_prefix", "bt.guard.filter_urls",
                "bt.guard.cloud_enabled",
            ),
        ),
        SyncGroup(
            SyncGroupId.Ed2k, "syncScopeEd2k",
            listOf(
                "ed2k.enable_kad", "ed2k.enable_upnp", "ed2k.server_list", "ed2k.server_sub_enabled", "ed2k.server_sub_urls",
            ),
        ),
        SyncGroup(SyncGroupId.Categories, "syncScopeCategories", listOf("custom_categories")),
    )

    fun phase(sync: SyncStatusDto): SyncPhase = when {
        !sync.enabled -> SyncPhase.Off
        sync.halted -> SyncPhase.Halted
        sync.lastError != null || sync.lastErrorReason != null -> SyncPhase.Error
        !sync.connected -> SyncPhase.Connecting
        sync.dirtyKeys.isEmpty() -> SyncPhase.Synced
        else -> SyncPhase.Syncing
    }

    /** 失败 / 暂停原因文案键：按 reason 本地化，未映射时回退通用文案（不显示服务端诊断原文）。 */
    fun reasonKey(sync: SyncStatusDto): String =
        sync.lastErrorReason?.let { AccountRules.errorKeyForReason(it, AccountErrorContext.Sync) } ?: "cloudSyncErrorGeneric"

    /** 「已同步 · {time}」的相对时间：刚刚 / n 分钟前 / n 小时前 / n 天前。 */
    sealed interface Ago {
        data object JustNow : Ago

        data class Minutes(val n: Int) : Ago

        data class Hours(val n: Int) : Ago

        data class Days(val n: Int) : Ago
    }

    fun ago(syncedAtMs: Long, nowMs: Long): Ago {
        val seconds = max(0L, (nowMs - syncedAtMs) / 1000)
        return when {
            seconds < 60 -> Ago.JustNow
            seconds < 3600 -> Ago.Minutes((seconds / 60).toInt())
            seconds < 86400 -> Ago.Hours((seconds / 3600).toInt())
            else -> Ago.Days((seconds / 86400).toInt())
        }
    }
}
