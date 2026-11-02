import Foundation

// 云端配置同步（`agent.sync.*`）。镜像 `native/protocol/src/agent.rs::SyncStatusDto` / `SyncLocalOnlyParams`；
// 范围分组与状态优先级镜像 Web `account/syncGroups.ts`（键表 = `settings.rs::SYNC_SETTING_SPECS` 共 56 键）。

/// `agent.sync` 分区 / `agent.sync.get|setLocalOnly` 结果。
public struct SyncStatusDto: Sendable, Hashable, Codable {
    public var enabled: Bool
    public var revision: UInt64
    public var dirtyKeys: [String]
    /// 诊断用原始错误文本（UI 优先按 `lastErrorReason` 展示本地化文案）。
    public var lastError: String?
    /// `ErrorReason` wire 名。
    public var lastErrorReason: String?
    /// 同步事件流当前已连通。
    public var connected: Bool
    /// 因不可自动恢复的错误（设备超限 / 设备未受信任）暂停自动重试，需用户处理后重新启用。
    public var halted: Bool
    /// 最近一次成功完成拉取 + 推送的时间（Unix 毫秒）。
    public var lastSyncedAtUnixMs: Int64?
    /// 本设备不参与云同步的同步目录键。
    public var localOnlyKeys: [String]

    public init(
        enabled: Bool = false, revision: UInt64 = 0, dirtyKeys: [String] = [], lastError: String? = nil,
        lastErrorReason: String? = nil, connected: Bool = false, halted: Bool = false,
        lastSyncedAtUnixMs: Int64? = nil, localOnlyKeys: [String] = []
    ) {
        self.enabled = enabled
        self.revision = revision
        self.dirtyKeys = dirtyKeys
        self.lastError = lastError
        self.lastErrorReason = lastErrorReason
        self.connected = connected
        self.halted = halted
        self.lastSyncedAtUnixMs = lastSyncedAtUnixMs
        self.localOnlyKeys = localOnlyKeys
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        revision = try c.decodeIfPresent(UInt64.self, forKey: .revision) ?? 0
        dirtyKeys = try c.decodeIfPresent([String].self, forKey: .dirtyKeys) ?? []
        lastError = try c.decodeIfPresent(String.self, forKey: .lastError)
        lastErrorReason = try c.decodeIfPresent(String.self, forKey: .lastErrorReason)
        connected = try c.decodeIfPresent(Bool.self, forKey: .connected) ?? false
        halted = try c.decodeIfPresent(Bool.self, forKey: .halted) ?? false
        lastSyncedAtUnixMs = try c.decodeIfPresent(Int64.self, forKey: .lastSyncedAtUnixMs)
        localOnlyKeys = try c.decodeIfPresent([String].self, forKey: .localOnlyKeys) ?? []
    }
}

/// `agent.sync.setLocalOnly` 参数：把一组同步目录键设为本设备专属 / 恢复同步。
public struct SyncLocalOnlyParams: Sendable, Hashable, Codable {
    public var keys: [String]
    public var localOnly: Bool

    public init(keys: [String], localOnly: Bool) {
        self.keys = keys
        self.localOnly = localOnly
    }
}

/// 同步状态阶段（优先级：未启用 → 暂停 → 失败 → 连接中 → 同步中 → 已同步）。
public enum SyncPhase: Sendable, Hashable {
    case off, halted, error, connecting, syncing, synced
}

/// 「在此设备同步的范围」分组。
public struct SyncGroup: Sendable, Hashable, Identifiable {
    public enum ID: String, Sendable, Hashable, CaseIterable {
        case appearance, general, ui, download, bt, ed2k, categories
    }

    public let id: ID
    /// 分组标题 i18n 键。
    public let labelKey: String
    public let keys: [String]

    /// 分组当前状态：全部参与同步 / 全部本设备专属 / 混合（其他客户端逐键设置过）。
    public enum State: Sendable, Hashable {
        case sync, local, mixed
    }

    public func state(localOnlyKeys: [String]) -> State {
        let local = Set(localOnlyKeys)
        let count = keys.filter(local.contains).count
        if count == 0 { return .sync }
        return count == keys.count ? .local : .mixed
    }

    /// 点击分组开关后应发送的 `setLocalOnly` 参数：仅「全部参与同步」时切为本设备专属，
    /// 其余（本设备专属 / 混合）一律恢复同步。
    public func toggleParams(from state: State) -> SyncLocalOnlyParams {
        SyncLocalOnlyParams(keys: keys, localOnly: state == .sync)
    }
}

public enum SyncRules {
    public static let groups: [SyncGroup] = [
        SyncGroup(id: .appearance, labelKey: "syncScopeAppearance", keys: [
            "appearance.theme_mode", "appearance.dark_theme", "appearance.light_theme",
            "appearance.color_scheme", "appearance.custom_color", "appearance.custom_themes",
        ]),
        SyncGroup(id: .general, labelKey: "syncScopeGeneral", keys: [
            "general.locale", "general.update_channel", "general.auto_check_update", "general.clipboard_watch",
            "general.floating_ball_enabled", "general.floating_ball_active_only",
        ]),
        SyncGroup(id: .ui, labelKey: "syncScopeUi", keys: [
            "ui.show_sidebar_status", "ui.show_sidebar_queues", "ui.show_sidebar_category", "ui.show_sidebar_rss",
            "ui.show_activity_rss", "ui.show_activity_webhooks", "ui.show_activity_theme",
            "ui.show_titlebar_pause_all", "ui.show_titlebar_resume_all", "ui.show_titlebar_settings",
            "ui.show_titlebar_theme",
        ]),
        SyncGroup(id: .download, labelKey: "syncScopeDownload", keys: [
            "download.max_concurrent_tasks", "download.default_segments", "download.auto_max_connections",
            "download.cdn_multi_enabled", "download.cdn_max_nodes", "download.speed_limit_bytes",
            "download.max_auto_retries", "download.auto_retry_delay_secs", "download.auto_resume_on_start",
            "download.remember_last_save_dir", "download.use_server_time", "download.global_user_agent",
            "download.notify_on_complete", "download.silent_download", "download.keep_awake",
        ]),
        SyncGroup(id: .bt, labelKey: "syncScopeBt", keys: [
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
        ]),
        SyncGroup(id: .ed2k, labelKey: "syncScopeEd2k", keys: [
            "ed2k.enable_kad", "ed2k.enable_upnp", "ed2k.server_list", "ed2k.server_sub_enabled", "ed2k.server_sub_urls",
        ]),
        SyncGroup(id: .categories, labelKey: "syncScopeCategories", keys: ["custom_categories"]),
    ]

    public static func phase(_ sync: SyncStatusDto) -> SyncPhase {
        if !sync.enabled { return .off }
        if sync.halted { return .halted }
        if sync.lastError != nil || sync.lastErrorReason != nil { return .error }
        if !sync.connected { return .connecting }
        return sync.dirtyKeys.isEmpty ? .synced : .syncing
    }

    /// 失败 / 暂停原因文案键：按 reason 本地化，未映射时回退通用文案（不显示服务端诊断原文）。
    public static func reasonKey(_ sync: SyncStatusDto) -> String {
        sync.lastErrorReason.flatMap { AccountRules.errorKey(forReason: $0, context: .sync) } ?? "cloudSyncErrorGeneric"
    }

    /// 「已同步 · {time}」的相对时间：刚刚 / n 分钟前 / n 小时前 / n 天前。
    public enum Ago: Sendable, Hashable {
        case justNow
        case minutes(Int)
        case hours(Int)
        case days(Int)
    }

    public static func ago(syncedAtMs: Int64, nowMs: Int64) -> Ago {
        let seconds = max(0, (nowMs - syncedAtMs) / 1000)
        if seconds < 60 { return .justNow }
        if seconds < 3600 { return .minutes(Int(seconds / 60)) }
        if seconds < 86400 { return .hours(Int(seconds / 3600)) }
        return .days(Int(seconds / 86400))
    }
}
