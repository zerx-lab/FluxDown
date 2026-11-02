import FluxDomain
import Foundation

#if FULL_FLAVOR
// BitTorrent / eD2K 设置页的纯逻辑（行目录、可见性、时长换算、读数）。无 UI / 无主机依赖，便于单测。

// MARK: - BitTorrent

/// BT 页签（GPUI `bt.rs` 的 basic / tracker / seeding）。
nonisolated enum BtSettingsTab: String, CaseIterable, Identifiable {
    case general, tracker, seeding, antiLeech

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .general: "settingsTabGeneral"
        case .tracker: "settingsTabTracker"
        case .seeding: "settingsTabSeeding"
        case .antiLeech: "btGuardTab"
        }
    }
}

/// BT 页的每一行（GPUI 顺序）。页面渲染与设置搜索共用同一份可见性判定。
/// 做种「时长 + 单位」两个 PC 行在移动端合成一行（各自仍写回两个键）。
nonisolated enum BtSettingsRow: String, CaseIterable, Identifiable {
    case enabled
    case dht, upnp, utp, portStart, portEnd, mseMode
    case customTrackers, trackerSub, trackerSubUrls, trackerSubStatus
    case seedEnabled, seedMaxActive, autoReseed, seedRatio, seedPostRatio
    case seedTimeLimit, seedInactiveTimeLimit, seedOperator, seedThenAction

    var id: String { "bt.\(rawValue)" }

    var tab: BtSettingsTab {
        switch self {
        case .enabled, .dht, .upnp, .utp, .portStart, .portEnd, .mseMode: .general
        case .customTrackers, .trackerSub, .trackerSubUrls, .trackerSubStatus: .tracker
        default: .seeding
        }
    }

    var configKey: String {
        switch self {
        case .enabled: "bt_enabled"
        case .dht: "bt_enable_dht"
        case .upnp: "bt_enable_upnp"
        case .utp: "bt_enable_utp"
        case .portStart: "bt_port_start"
        case .portEnd: "bt_port_end"
        case .mseMode: "bt_mse_mode"
        case .customTrackers: "bt_custom_trackers"
        case .trackerSub: "bt_tracker_sub_enabled"
        case .trackerSubUrls: "bt_tracker_sub_urls"
        case .trackerSubStatus: "bt_tracker_sub_cache"
        case .seedEnabled: "bt_seed_enabled"
        case .seedMaxActive: "bt_seed_max_active"
        case .autoReseed: "bt_auto_reseed"
        case .seedRatio: "bt_seed_ratio_limit"
        case .seedPostRatio: "bt_seed_post_ratio_limit"
        case .seedTimeLimit: "bt_seed_time_limit_minutes"
        case .seedInactiveTimeLimit: "bt_seed_inactive_time_limit_minutes"
        case .seedOperator: "bt_seed_limit_operator"
        case .seedThenAction: "bt_seed_then_action"
        }
    }

    /// 时长行的单位键（仅 `seedTimeLimit` / `seedInactiveTimeLimit`）。
    var unitKey: String? {
        switch self {
        case .seedTimeLimit: "bt_seed_time_limit_unit"
        case .seedInactiveTimeLimit: "bt_seed_inactive_time_limit_unit"
        default: nil
        }
    }

    /// 单位菜单的无障碍标题键。
    var unitTitleKey: String? {
        switch self {
        case .seedTimeLimit: "btSeedTimeLimitUnit"
        case .seedInactiveTimeLimit: "btSeedInactiveTimeLimitUnit"
        default: nil
        }
    }

    var titleKey: String {
        switch self {
        case .enabled: "btEnabled"
        case .dht: "btEnableDht"
        case .upnp: "btEnableUpnp"
        case .utp: "btEnableUtp"
        case .portStart: "btListenPortStart"
        case .portEnd: "btListenPortEnd"
        case .mseMode: "btMseMode"
        case .customTrackers: "btTrackerList"
        case .trackerSub: "btTrackerSub"
        case .trackerSubUrls: "btTrackerSubUrls"
        case .trackerSubStatus: "btTrackerSubUpdateNow"
        case .seedEnabled: "btSeedEnabled"
        case .seedMaxActive: "btSeedMaxActive"
        case .autoReseed: "btAutoReseed"
        case .seedRatio: "btSeedRatioLimit"
        case .seedPostRatio: "btSeedPostRatioLimit"
        case .seedTimeLimit: "btSeedTimeLimit"
        case .seedInactiveTimeLimit: "btSeedInactiveTimeLimit"
        case .seedOperator: "btSeedConditionsOperator"
        case .seedThenAction: "btSeedThenAction"
        }
    }

    /// 说明文案键。时长行的「说明」只用于搜索（命中单位键标题），页面不渲染。
    var detailKey: String? {
        switch self {
        case .enabled: "btEnabledDesc"
        case .dht: "btEnableDhtDesc"
        case .upnp: "btEnableUpnpDesc"
        case .utp: "btEnableUtpDesc"
        case .portStart: "btListenPortDesc"
        case .mseMode: "btMseModeDesc"
        case .customTrackers: "btTrackerListDesc"
        case .trackerSub: "btTrackerSubDesc"
        case .trackerSubUrls: "btTrackerSubUrlsDesc"
        case .seedEnabled: "btSeedEnabledDesc"
        case .seedMaxActive: "btSeedMaxActiveDesc"
        case .autoReseed: "btAutoReseedDesc"
        case .seedTimeLimit: "btSeedTimeLimitUnit"
        case .seedInactiveTimeLimit: "btSeedInactiveTimeLimitUnit"
        default: nil
        }
    }

    var item: SettingsItem {
        SettingsItem(id: id, key: configKey, titleKey: titleKey, detailKey: detailKey)
    }

    /// 页面里实际渲染的说明（时长行不渲染）。
    var renderedItem: SettingsItem {
        switch self {
        case .seedTimeLimit, .seedInactiveTimeLimit:
            SettingsItem(id: id, key: configKey, titleKey: titleKey, detailKey: nil)
        default:
            item
        }
    }

    /// BT 总开关；缺键（旧主机）按开启处理。
    static func isEnabled(in form: SettingsConfigForm) -> Bool {
        form.bool(BtSettingsRow.enabled.configKey)
    }

    func isVisible(in form: SettingsConfigForm) -> Bool {
        guard form.has(configKey) else { return false }
        if self == .enabled { return true }
        guard BtSettingsRow.isEnabled(in: form) else { return false }
        switch tab {
        case .seeding where self != .seedEnabled:
            return form.bool(BtSettingsRow.seedEnabled.configKey)
        default:
            return true
        }
    }

    static func visible(in tab: BtSettingsTab, _ form: SettingsConfigForm) -> [BtSettingsRow] {
        allCases.filter { $0.tab == tab && $0.isVisible(in: form) }
    }

    /// 当前可选页签：BT 关闭时只剩常规。
    static func availableTabs(in form: SettingsConfigForm) -> [BtSettingsTab] {
        isEnabled(in: form) ? BtSettingsTab.allCases : [.general]
    }

    /// 搜索定位：行 id 所在页签（防吸血行属于防吸血页签）。
    static func tab(forRowID id: String) -> BtSettingsTab? {
        if BtGuardRow.allCases.contains(where: { $0.id == id }) { return .antiLeech }
        return allCases.first { $0.id == id }?.tab
    }
}

// MARK: - 防吸血（移动端子集）

/// 防吸血页签的设置行：总开关 · 各规则模式 · 云名单（开关 / 模式）· 证据上报开关 · 封禁名单入口。
/// 阈值 / 处置阶梯 / 豁免 / 过滤文件订阅 / 事件日志只在桌面与 Web 提供。
nonisolated enum BtGuardRow: String, CaseIterable, Identifiable {
    case enabled
    case ruleExcessive, ruleRerequest, ruleRewind, ruleLag, ruleFingerprint
    case cloudEnabled, cloudMode
    case reportEnabled
    case bans

    var id: String { "btGuard.\(rawValue)" }

    /// 规则模式行（off / observe / enforce）。
    static let ruleRows: [BtGuardRow] = [.ruleExcessive, .ruleRerequest, .ruleRewind, .ruleLag, .ruleFingerprint]

    /// 对应的 daemon 配置键；封禁名单入口不读写配置，借总开关键参与可见性判定。
    var configKey: String {
        switch self {
        case .enabled, .bans: "bt_guard_enabled"
        case .ruleExcessive: "bt_guard_rule_excessive"
        case .ruleRerequest: "bt_guard_rule_rerequest"
        case .ruleRewind: "bt_guard_rule_rewind"
        case .ruleLag: "bt_guard_rule_lag"
        case .ruleFingerprint: "bt_guard_rule_fingerprint"
        case .cloudEnabled: "bt_guard_cloud_enabled"
        case .cloudMode: "bt_guard_cloud_mode"
        case .reportEnabled: "bt_guard_report_enabled"
        }
    }

    var titleKey: String {
        switch self {
        case .enabled: "btGuardEnabled"
        case .ruleExcessive: "btGuardRuleExcessive"
        case .ruleRerequest: "btGuardRuleRerequest"
        case .ruleRewind: "btGuardRuleRewind"
        case .ruleLag: "btGuardRuleLag"
        case .ruleFingerprint: "btGuardRuleFingerprint"
        case .cloudEnabled: "btGuardCloudEnabled"
        case .cloudMode: "btGuardCloudMode"
        case .reportEnabled: "btGuardReportEnabled"
        case .bans: "btGuardTabBans"
        }
    }

    var detailKey: String? {
        switch self {
        case .enabled: "btGuardEnabledDesc"
        case .ruleExcessive: "btGuardRuleExcessiveDesc"
        case .ruleRerequest: "btGuardRuleRerequestDesc"
        case .ruleRewind: "btGuardRuleRewindDesc"
        case .ruleLag: "btGuardRuleLagDesc"
        case .ruleFingerprint: "btGuardRuleFingerprintDesc"
        case .cloudEnabled: "btGuardCloudEnabledDesc"
        case .reportEnabled: "btGuardReportEnabledDesc"
        case .cloudMode, .bans: nil
        }
    }

    var item: SettingsItem {
        SettingsItem(id: id, key: configKey, titleKey: titleKey, detailKey: detailKey)
    }

    /// 规则模式选项（`btGuardModeOff` / `Observe` / `Enforce`）。
    static let modeOptions: [(id: String, labelKey: String)] = [
        ("off", "btGuardModeOff"), ("observe", "btGuardModeObserve"), ("enforce", "btGuardModeEnforce"),
    ]

    /// 防吸血页签的行可见：配置已加载且 BT 总开关开启（旧主机缺键同样走目录默认值，数据区另行报错）。
    func isVisible(in form: SettingsConfigForm) -> Bool {
        form.has(configKey) && BtSettingsRow.isEnabled(in: form)
    }

    static func visible(_ form: SettingsConfigForm) -> [BtGuardRow] {
        allCases.filter { $0.isVisible(in: form) }
    }

    /// 防吸血总开关；缺键按目录默认（开启）。
    static func isGuardEnabled(in form: SettingsConfigForm) -> Bool {
        form.bool(BtGuardRow.enabled.configKey)
    }

    /// 云名单模式行是否可编辑：总开关与云名单开关都开启。
    static func cloudModeEditable(in form: SettingsConfigForm) -> Bool {
        isGuardEnabled(in: form) && form.bool(BtGuardRow.cloudEnabled.configKey)
    }
}

/// 监听端口区间。
enum BtPortRange {
    static let bounds = 1 ... 65_535

    /// 结束端口不得小于起始端口。
    static func isValid(start: Int, end: Int) -> Bool { end >= start }
}

/// MSE（协议加密）选项：值 → 标题 / 说明。
nonisolated enum BtMseMode: String, CaseIterable, Identifiable {
    case disabled, enabled, forced

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .disabled: "btMseModeDisabled"
        case .enabled: "btMseModeEnabled"
        case .forced: "btMseModeForced"
        }
    }

    var detailKey: String {
        switch self {
        case .disabled: "btMseModeDisabledDesc"
        case .enabled: "btMseModeEnabledDesc"
        case .forced: "btMseModeForcedDesc"
        }
    }
}

// MARK: - 做种时长

/// 时长单位（`bt_seed_*_time_limit_unit`）。数值键始终以**分钟**落库，单位键记录设置页的展示单位
/// （daemon `bt_config_from_map` 直接取分钟值）。
nonisolated enum BtDurationUnit: String, CaseIterable, Identifiable {
    case minutes, hours, days

    var id: String { rawValue }

    var factor: Int64 {
        switch self {
        case .minutes: 1
        case .hours: 60
        case .days: 1_440
        }
    }

    var titleKey: String {
        switch self {
        case .minutes: "timeUnitMinutes"
        case .hours: "timeUnitHours"
        case .days: "timeUnitDays"
        }
    }

    /// 未知 / 缺失取分钟（目录默认）。
    init(wire: String?) {
        self = wire.flatMap { BtDurationUnit(rawValue: $0.trimmingCharacters(in: .whitespaces)) } ?? .minutes
    }
}

enum BtDuration {
    private static let posix = Locale(identifier: "en_US_POSIX")

    /// `minutes` 在 `unit` 下的显示文本：最多两位小数、去掉多余的 0；≤ 0 → 空串（显示「关闭」占位）。
    static func text(minutes: Int64, unit: BtDurationUnit) -> String {
        guard minutes > 0 else { return "" }
        var s = String(format: "%.2f", locale: posix, Double(minutes) / Double(unit.factor))
        if s.contains(".") {
            while s.hasSuffix("0") { s.removeLast() }
            if s.hasSuffix(".") { s.removeLast() }
        }
        return s.isEmpty ? "0" : s
    }

    /// 文本（`unit` 下的数值）→ 分钟。空串 = 0（关闭）；负数 / 非数字 = nil（非法）。
    static func minutes(from text: String, unit: BtDurationUnit) -> Int64? {
        let normalized = text.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: ",", with: ".")
        if normalized.isEmpty { return 0 }
        guard let value = Double(normalized), value.isFinite, value >= 0 else { return nil }
        let raw = value * Double(unit.factor)
        if raw >= Double(Int64.max) { return Int64.max }
        return Int64(raw.rounded())
    }

    /// 以 `unit` 为步长（1 个单位）步进后的分钟数，下限 0。
    static func stepped(minutes: Int64, unit: BtDurationUnit, up: Bool) -> Int64 {
        let current = Double(minutes) / Double(unit.factor)
        let next = max(0, current + (up ? 1 : -1))
        return Int64((next * Double(unit.factor)).rounded())
    }
}

// MARK: - 读数

extension BtSettingsRow {
    /// 设置首页读数：`DHT · 做种 · 6881–6891`（只列当前生效项）；配置未加载 / 全部关闭 → nil。
    static func readout(_ form: SettingsConfigForm) -> String? {
        guard form.isLoaded else { return nil }
        guard BtSettingsRow.isEnabled(in: form) else { return nil }
        var parts: [String] = []
        if form.bool(BtSettingsRow.dht.configKey) { parts.append("DHT") }
        if form.bool(BtSettingsRow.seedEnabled.configKey) { parts.append(L("btSeedingTitle")) }
        let start = form.int(BtSettingsRow.portStart.configKey, default: 6881)
        let end = form.int(BtSettingsRow.portEnd.configKey, default: 6891)
        parts.append(start == end ? "\(start)" : "\(start)–\(end)")
        return parts.joined(separator: " · ")
    }
}

// MARK: - eD2K

nonisolated enum Ed2kSettingsTab: String, CaseIterable, Identifiable {
    case general, servers

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .general: "settingsTabGeneral"
        case .servers: "settingsTabServers"
        }
    }
}

nonisolated enum Ed2kSettingsRow: String, CaseIterable, Identifiable {
    case kad, upnp, listenPort
    case serverList, serverSub, serverSubUrls, serverSubStatus

    var id: String { "ed2k.\(rawValue)" }

    var tab: Ed2kSettingsTab {
        switch self {
        case .kad, .upnp, .listenPort: .general
        default: .servers
        }
    }

    var configKey: String {
        switch self {
        case .kad: "ed2k_enable_kad"
        case .upnp: "ed2k_enable_upnp"
        case .listenPort: "ed2k_listen_port"
        case .serverList: "ed2k_server_list"
        case .serverSub: "ed2k_server_sub_enabled"
        case .serverSubUrls: "ed2k_server_sub_urls"
        case .serverSubStatus: "ed2k_server_sub_cache"
        }
    }

    var titleKey: String {
        switch self {
        case .kad: "ed2kEnableKad"
        case .upnp: "ed2kEnableUpnp"
        case .listenPort: "ed2kListenPort"
        case .serverList: "ed2kServerList"
        case .serverSub: "ed2kServerSub"
        case .serverSubUrls: "ed2kServerSubUrls"
        case .serverSubStatus: "ed2kServerSubUpdateNow"
        }
    }

    var detailKey: String? {
        switch self {
        case .kad: "ed2kEnableKadDesc"
        case .upnp: "ed2kEnableUpnpDesc"
        case .listenPort: "ed2kListenPortDesc"
        case .serverList: "ed2kServerListDesc"
        case .serverSub: "ed2kServerSubDesc"
        case .serverSubUrls: "ed2kServerSubUrlsDesc"
        case .serverSubStatus: nil
        }
    }

    var item: SettingsItem {
        SettingsItem(id: id, key: configKey, titleKey: titleKey, detailKey: detailKey)
    }

    func isVisible(in form: SettingsConfigForm) -> Bool { form.has(configKey) }

    static func visible(in tab: Ed2kSettingsTab, _ form: SettingsConfigForm) -> [Ed2kSettingsRow] {
        allCases.filter { $0.tab == tab && $0.isVisible(in: form) }
    }

    static func tab(forRowID id: String) -> Ed2kSettingsTab? {
        allCases.first { $0.id == id }?.tab
    }

    /// 设置首页读数：`Kad · 12 个服务器`（手动列表 + 订阅缓存去重后的条数）；配置未加载 / 无可报内容 → nil。
    static func readout(_ form: SettingsConfigForm) -> String? {
        guard form.isLoaded else { return nil }
        var parts: [String] = []
        if form.bool(Ed2kSettingsRow.kad.configKey) { parts.append("Kad") }
        let merged = SubscriptionListFormat.comma.entries(form.string(Ed2kSettingsRow.serverList.configKey))
            + SubscriptionListFormat.comma.entries(form.string(Ed2kSettingsRow.serverSubStatus.configKey))
        var seen = Set<String>()
        let count = merged.filter { seen.insert($0.lowercased()).inserted }.count
        if count > 0 { parts.append(L("ed2kServerCount", ["n": count])) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

// MARK: - 订阅

/// 订阅种类：存储键、列表格式与文案键（`btTrackerSub…` / `ed2kServerSub…`）。
nonisolated enum SubscriptionKind {
    case btTrackers, ed2kServers

    var cacheKey: String {
        switch self {
        case .btTrackers: "bt_tracker_sub_cache"
        case .ed2kServers: "ed2k_server_sub_cache"
        }
    }

    var updatedAtKey: String {
        switch self {
        case .btTrackers: "bt_tracker_sub_updated_at"
        case .ed2kServers: "ed2k_server_sub_updated_at"
        }
    }

    var format: SubscriptionListFormat {
        switch self {
        case .btTrackers: .lines
        case .ed2kServers: .comma
        }
    }

    var rowID: String {
        switch self {
        case .btTrackers: BtSettingsRow.trackerSubStatus.id
        case .ed2kServers: Ed2kSettingsRow.serverSubStatus.id
        }
    }

    var statusKey: String {
        switch self {
        case .btTrackers: "btTrackerSubStatus"
        case .ed2kServers: "ed2kServerSubStatus"
        }
    }

    var updatedAtTextKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdatedAt"
        case .ed2kServers: "ed2kServerSubUpdatedAt"
        }
    }

    var neverKey: String {
        switch self {
        case .btTrackers: "btTrackerSubNeverUpdated"
        case .ed2kServers: "ed2kServerSubNeverUpdated"
        }
    }

    var updateNowKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdateNow"
        case .ed2kServers: "ed2kServerSubUpdateNow"
        }
    }

    var updatingKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdating"
        case .ed2kServers: "ed2kServerSubUpdating"
        }
    }

    var failedKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdateFailed"
        case .ed2kServers: "ed2kServerSubUpdateFailed"
        }
    }
}

/// 订阅状态的显示值：配置快照（缓存条数 / 更新时间）与本次刷新结果合并。
nonisolated struct SubscriptionStatusModel: Equatable {
    var count: Int
    /// Unix 秒；0 = 从未更新。
    var updatedAt: Int64

    /// 刷新结果可能先于配置快照到达：更新时间更晚的一方为准；失败的刷新不改变显示值（daemon 沿用旧缓存）。
    static func make(
        kind: SubscriptionKind, form: SettingsConfigForm, fresh: SubscriptionRefreshOutcome?
    ) -> SubscriptionStatusModel {
        let storedCount = kind.format.count(form.string(kind.cacheKey))
        let storedAt = max(0, form.long(kind.updatedAtKey, default: 0))
        guard let fresh, fresh.success, fresh.updatedAt > storedAt else {
            return SubscriptionStatusModel(count: storedCount, updatedAt: storedAt)
        }
        return SubscriptionStatusModel(count: Int(clamping: fresh.count), updatedAt: fresh.updatedAt)
    }
}

enum SubscriptionTime {
    /// 一周内相对时间（「3 分钟前」，语言同界面文案），更早显示绝对日期时间。
    static func text(unix: Int64, now: Date = Date(), languageCode: String = L10n.shared.locale) -> String {
        let locale = Locale(identifier: languageCode)
        let date = Date(timeIntervalSince1970: TimeInterval(unix))
        let age = now.timeIntervalSince(date)
        if age < 7 * 86_400 {
            let formatter = RelativeDateTimeFormatter()
            formatter.locale = locale
            formatter.unitsStyle = .full
            formatter.dateTimeStyle = .named
            return formatter.localizedString(for: min(date, now), relativeTo: now)
        }
        return date.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened).locale(locale))
    }

    /// 「有效期至」：未来时间显示绝对日期时间（不夹成 now）；已过期沿用 ``text(unix:now:languageCode:)`` 的相对文案。
    static func expiryText(unix: Int64, now: Date = Date(), languageCode: String = L10n.shared.locale) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unix))
        guard date > now else { return text(unix: unix, now: now, languageCode: languageCode) }
        let locale = Locale(identifier: languageCode)
        return date.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened).locale(locale))
    }
}
#endif
