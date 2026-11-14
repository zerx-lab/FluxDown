import FluxDomain
import Foundation

#if FULL_FLAVOR
// BitTorrent / eD2K 设置页的纯逻辑（行目录、可见性、时长换算、读数）。无 UI / 无主机依赖，便于单测。

// MARK: - BitTorrent

/// BT 页签（GPUI `bt.rs` 的 basic / connection / tracker / seeding + 防吸血）。
/// `rawValue` 由 `@SceneStorage` 持久化：只能追加，不能改名。
nonisolated enum BtSettingsTab: String, CaseIterable, Identifiable {
    case general, connection, tracker, seeding, antiLeech

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .general: "settingsTabGeneral"
        case .connection: "settingsTabConnection"
        case .tracker: "settingsTabTracker"
        case .seeding: "settingsTabSeeding"
        case .antiLeech: "btGuardTab"
        }
    }
}

/// BT 页的分组（GPUI `bt.rs` 的各 `*_section`，同序）。
nonisolated enum BtSettingsSection: CaseIterable {
    case basic, newTasks
    case listen, transport, sources, limits, i2p
    case trackers, trackerSub, announce
    case seeding, seedLimits

    var tab: BtSettingsTab {
        switch self {
        case .basic, .newTasks: .general
        case .listen, .transport, .sources, .limits, .i2p: .connection
        case .trackers, .trackerSub, .announce: .tracker
        case .seeding, .seedLimits: .seeding
        }
    }

    /// 分组标题键（nil = 无标题）。
    var titleKey: String? {
        switch self {
        case .newTasks: "btNewTasksSection"
        case .listen: "btSectionListen"
        case .transport: "btSectionTransport"
        case .sources: "btSectionSources"
        case .limits: "btSectionLimits"
        case .i2p: "btSectionI2p"
        case .announce: "btSectionAnnounce"
        case .seedLimits: "btSeedLimitsTitle"
        case .basic, .trackers, .trackerSub, .seeding: nil
        }
    }

    var rows: [BtSettingsRow] { BtSettingsRow.allCases.filter { $0.section == self } }

    static func all(in tab: BtSettingsTab) -> [BtSettingsSection] { allCases.filter { $0.tab == tab } }
}

/// BT 页的每一行（GPUI 顺序）。页面渲染与设置搜索共用同一份可见性判定。
/// 做种「时长 + 单位」两个 PC 行在移动端合成一行（各自仍写回两个键）。
/// 端口映射状态行不是配置键，由 ``BtSettingsRow/showsPortMappingStatus(in:)`` 判定。
nonisolated enum BtSettingsRow: String, CaseIterable, Identifiable {
    case enabled
    case pieceOrder, firstLastPiecesFirst, selectionAutoDefault
    case portStart, portEnd, upnp
    case utp, utpCongestion, mseMode, uploadSlots
    case dht, lsd, webseed
    case maxConnections, maxConnectionsPerTorrent
    case i2pEnabled, i2pSamHost, i2pSamPort, i2pMixed
    case customTrackers, trackerSub, trackerSubUrls, trackerSubStatus
    case announceAllTiers, announceAllTrackers
    case seedEnabled, seedMaxActive, autoReseed, seedRatio, seedPostRatio
    case seedTimeLimit, seedInactiveTimeLimit, seedOperator, seedThenAction

    var id: String { "bt.\(rawValue)" }

    var section: BtSettingsSection {
        switch self {
        case .enabled: .basic
        case .pieceOrder, .firstLastPiecesFirst, .selectionAutoDefault: .newTasks
        case .portStart, .portEnd, .upnp: .listen
        case .utp, .utpCongestion, .mseMode, .uploadSlots: .transport
        case .dht, .lsd, .webseed: .sources
        case .maxConnections, .maxConnectionsPerTorrent: .limits
        case .i2pEnabled, .i2pSamHost, .i2pSamPort, .i2pMixed: .i2p
        case .customTrackers: .trackers
        case .trackerSub, .trackerSubUrls, .trackerSubStatus: .trackerSub
        case .announceAllTiers, .announceAllTrackers: .announce
        case .seedEnabled, .seedMaxActive, .autoReseed: .seeding
        case .seedRatio, .seedPostRatio, .seedTimeLimit, .seedInactiveTimeLimit, .seedOperator, .seedThenAction: .seedLimits
        }
    }

    var tab: BtSettingsTab { section.tab }

    var configKey: String {
        switch self {
        case .enabled: "bt_enabled"
        case .pieceOrder: "bt_piece_order"
        case .firstLastPiecesFirst: "bt_first_last_pieces_first"
        case .selectionAutoDefault: "bt_selection_auto_default"
        case .portStart: "bt_port_start"
        case .portEnd: "bt_port_end"
        case .upnp: "bt_enable_upnp"
        case .utp: "bt_enable_utp"
        case .utpCongestion: "bt_utp_congestion"
        case .mseMode: "bt_mse_mode"
        case .uploadSlots: "bt_upload_slots"
        case .dht: "bt_enable_dht"
        case .lsd: "bt_enable_lsd"
        case .webseed: "bt_enable_webseed"
        case .maxConnections: "bt_max_connections"
        case .maxConnectionsPerTorrent: "bt_max_connections_per_torrent"
        case .i2pEnabled: "bt_i2p_enabled"
        case .i2pSamHost: "bt_i2p_sam_host"
        case .i2pSamPort: "bt_i2p_sam_port"
        case .i2pMixed: "bt_i2p_mixed"
        case .customTrackers: "bt_custom_trackers"
        case .trackerSub: "bt_tracker_sub_enabled"
        case .trackerSubUrls: "bt_tracker_sub_urls"
        case .trackerSubStatus: "bt_tracker_sub_cache"
        case .announceAllTiers: "bt_announce_all_tiers"
        case .announceAllTrackers: "bt_announce_all_trackers"
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
        case .pieceOrder: "btPieceOrder"
        case .firstLastPiecesFirst: "btFirstLastPiecesFirstDefault"
        case .selectionAutoDefault: "btSelectionAutoDefault"
        case .portStart: "btListenPortStart"
        case .portEnd: "btListenPortEnd"
        case .upnp: "btEnableUpnp"
        case .utp: "btEnableUtp"
        case .utpCongestion: "btUtpCongestion"
        case .mseMode: "btMseMode"
        case .uploadSlots: "btUploadSlots"
        case .dht: "btEnableDht"
        case .lsd: "btEnableLsd"
        case .webseed: "btEnableWebseed"
        case .maxConnections: "btMaxConnections"
        case .maxConnectionsPerTorrent: "btMaxConnectionsPerTorrent"
        case .i2pEnabled: "btI2pEnabled"
        case .i2pSamHost: "btI2pSamHost"
        case .i2pSamPort: "btI2pSamPort"
        case .i2pMixed: "btI2pMixed"
        case .customTrackers: "btTrackerList"
        case .trackerSub: "btTrackerSub"
        case .trackerSubUrls: "btTrackerSubUrls"
        case .trackerSubStatus: "btTrackerSubUpdateNow"
        case .announceAllTiers: "btAnnounceAllTiers"
        case .announceAllTrackers: "btAnnounceAllTrackers"
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
        case .pieceOrder: "btPieceOrderDesc"
        case .firstLastPiecesFirst: "btFirstLastPiecesFirstDefaultDesc"
        case .selectionAutoDefault: "btSelectionAutoDefaultDesc"
        case .portStart: "btListenPortDesc"
        case .upnp: "btEnableUpnpDesc"
        case .utp: "btEnableUtpDesc"
        case .utpCongestion: "btUtpCongestionDesc"
        case .mseMode: "btMseModeDesc"
        case .uploadSlots: "btUploadSlotsDesc"
        case .dht: "btEnableDhtDesc"
        case .lsd: "btEnableLsdDesc"
        case .webseed: "btEnableWebseedDesc"
        case .maxConnections: "btMaxConnectionsDesc"
        case .maxConnectionsPerTorrent: "btMaxConnectionsPerTorrentDesc"
        case .i2pEnabled: "btI2pEnabledDesc"
        case .i2pMixed: "btI2pMixedDesc"
        case .customTrackers: "btTrackerListDesc"
        case .trackerSub: "btTrackerSubDesc"
        case .trackerSubUrls: "btTrackerSubUrlsDesc"
        case .announceAllTiers: "btAnnounceAllTiersDesc"
        case .announceAllTrackers: "btAnnounceAllTrackersDesc"
        case .seedEnabled: "btSeedEnabledDesc"
        case .seedMaxActive: "btSeedMaxActiveDesc"
        case .autoReseed: "btAutoReseedDesc"
        case .seedTimeLimit: "btSeedTimeLimitUnit"
        case .seedInactiveTimeLimit: "btSeedInactiveTimeLimitUnit"
        default: nil
        }
    }

    /// 数值行的范围（与 `daemon_config.rs` / 目录一致；非数值行为 nil）。
    var range: ClosedRange<Int>? {
        switch self {
        case .portStart, .portEnd: BtPortRange.bounds
        case .uploadSlots: 0 ... 512
        case .maxConnections: 0 ... 65_535
        case .maxConnectionsPerTorrent: 1 ... 4_096
        case .i2pSamPort: 1 ... 65_535
        case .seedMaxActive: 0 ... 1_000_000
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
        switch self {
        case .utpCongestion:
            // 拥塞控制只在 uTP 开启时有意义（GPUI `transport_section`）。
            return form.bool(BtSettingsRow.utp.configKey)
        case .i2pSamHost, .i2pSamPort, .i2pMixed:
            return form.bool(BtSettingsRow.i2pEnabled.configKey)
        default:
            break
        }
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

    static func visible(inSection section: BtSettingsSection, _ form: SettingsConfigForm) -> [BtSettingsRow] {
        section.rows.filter { $0.isVisible(in: form) }
    }

    /// 端口映射状态行：BT 与端口映射都开启时显示在「监听」组末尾（GPUI `listen_section`）。
    static func showsPortMappingStatus(in form: SettingsConfigForm) -> Bool {
        BtSettingsRow.upnp.isVisible(in: form) && form.bool(BtSettingsRow.upnp.configKey)
    }

    /// 当前可选页签：BT 关闭时只剩常规。
    static func availableTabs(in form: SettingsConfigForm) -> [BtSettingsTab] {
        isEnabled(in: form) ? BtSettingsTab.allCases : [.general]
    }

    /// 搜索定位：行 id 所在页签（防吸血行属于防吸血页签）。
    static func tab(forRowID id: String) -> BtSettingsTab? {
        if BtGuardRow.allCases.contains(where: { $0.id == id }) { return .antiLeech }
        if id == portMappingStatusID { return .connection }
        return allCases.first { $0.id == id }?.tab
    }

    /// 端口映射状态行的行 id（只用于搜索定位 / 滚动锚点）。
    static let portMappingStatusID = "bt.portMappingStatus"
}

// MARK: - BitTorrent 代理（代理设置页的 BitTorrent 分组）

/// GPUI `sections/proxy.rs::bt_section`：总开关 + 代理来源 + 节点连接是否也走代理 + 一条提示。
nonisolated enum BtProxyRow: String, CaseIterable, Identifiable {
    case enabled, source, peers

    var id: String { "network.btProxy.\(rawValue)" }

    var configKey: String {
        switch self {
        case .enabled: "bt_proxy_enabled"
        case .source: "bt_proxy_source"
        case .peers: "bt_proxy_peers"
        }
    }

    var titleKey: String {
        switch self {
        case .enabled: "btProxyEnabled"
        case .source: "btProxySource"
        case .peers: "btProxyPeers"
        }
    }

    var detailKey: String {
        switch self {
        case .enabled: "btProxyEnabledDesc"
        case .source: "btProxySourceDesc"
        case .peers: "btProxyPeersDesc"
        }
    }

    var item: SettingsItem {
        SettingsItem(id: id, key: configKey, titleKey: titleKey, detailKey: detailKey)
    }

    /// 分组只在主机配置已加载时出现；「代理来源」「节点连接」只在总开关开启时出现。
    func isVisible(in form: SettingsConfigForm) -> Bool {
        guard form.has(configKey) else { return false }
        switch self {
        case .enabled: return true
        case .source, .peers: return form.bool(BtProxyRow.enabled.configKey)
        }
    }

    static func visible(_ form: SettingsConfigForm) -> [BtProxyRow] {
        allCases.filter { $0.isVisible(in: form) }
    }

    /// 提示行（i18n 键）。系统代理来源：说明检测时机与生效方式。手动代理来源：未配置服务器优先，
    /// 其次是 BT 不支持 / 无法转发 UDP 的类型；SOCKS5 无提示。
    static func hint(source: String, proxyType: String, host: String, port: String) -> String? {
        if source == BtProxySource.system.rawValue { return "btProxySystemNote" }
        if host.trimmingCharacters(in: .whitespaces).isEmpty || port.trimmingCharacters(in: .whitespaces).isEmpty {
            return "btProxyNoServer"
        }
        switch proxyType {
        case "https": return "btProxyUnsupportedType"
        case "http", "socks4": return "btProxyNoUdp"
        default: return nil
        }
    }

    /// 总开关开启时按代理来源与手动代理配置给出的提示；关闭 / 未加载 → nil。
    static func hint(in form: SettingsConfigForm) -> String? {
        guard BtProxyRow.enabled.isVisible(in: form), form.bool(BtProxyRow.enabled.configKey) else { return nil }
        return hint(
            source: form.string(BtProxyRow.source.configKey),
            proxyType: form.string("proxy_type"), host: form.string("proxy_host"), port: form.string("proxy_port")
        )
    }
}

/// BT 代理来源（`bt_proxy_source`）：值 → 标题。
nonisolated enum BtProxySource: String, CaseIterable, Identifiable {
    case manual, system

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .manual: "btProxySourceManual"
        case .system: "btProxySourceSystem"
        }
    }
}

/// uTP 拥塞控制（`bt_utp_congestion`）：值 → 标题。
nonisolated enum BtUtpCongestion: String, CaseIterable, Identifiable {
    case ledbat, cubic

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .ledbat: "btUtpCongestionLedbat"
        case .cubic: "btUtpCongestionCubic"
        }
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
nonisolated enum BtPortRange {
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

/// 新任务选片顺序（`bt_piece_order`）：值 → 标题 / 说明。默认最稀有优先。
nonisolated enum BtPieceOrder: String, CaseIterable, Identifiable {
    case rarest, sequential

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .rarest: "btPieceOrderRarest"
        case .sequential: "btPieceOrderSequential"
        }
    }

    var detailKey: String {
        switch self {
        case .rarest: "btPieceOrderRarestDesc"
        case .sequential: "btPieceOrderSequentialDesc"
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
    case general, servers, kad

    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .general: "settingsTabGeneral"
        case .servers: "settingsTabServers"
        case .kad: "settingsTabKadNodes"
        }
    }
}

nonisolated enum Ed2kSettingsRow: String, CaseIterable, Identifiable {
    case kad, upnp, listenPort, idCheck
    case serverList, serverSub, serverSubUrls, serverSubStatus
    case nodesSubUrls, nodesSubStatus

    var id: String { "ed2k.\(rawValue)" }

    var tab: Ed2kSettingsTab {
        switch self {
        case .kad, .upnp, .listenPort, .idCheck: .general
        case .serverList, .serverSub, .serverSubUrls, .serverSubStatus: .servers
        case .nodesSubUrls, .nodesSubStatus: .kad
        }
    }

    /// 行绑定的配置键；`idCheck` 是动作行，借监听端口键判可见（daemon 支持 eD2K 监听即可检测）。
    var configKey: String {
        switch self {
        case .kad: "ed2k_enable_kad"
        case .upnp: "ed2k_enable_upnp"
        case .listenPort, .idCheck: "ed2k_listen_port"
        case .serverList: "ed2k_server_list"
        case .serverSub: "ed2k_server_sub_enabled"
        case .serverSubUrls: "ed2k_server_sub_urls"
        case .serverSubStatus: "ed2k_server_sub_cache"
        case .nodesSubUrls: "ed2k_nodes_dat_url"
        case .nodesSubStatus: "ed2k_nodes_dat_count"
        }
    }

    var titleKey: String {
        switch self {
        case .kad: "ed2kEnableKad"
        case .upnp: "ed2kEnableUpnp"
        case .listenPort: "ed2kListenPort"
        case .idCheck: "ed2kIdCheck"
        case .serverList: "ed2kServerList"
        case .serverSub: "ed2kServerSub"
        case .serverSubUrls: "ed2kServerSubUrls"
        case .serverSubStatus: "ed2kServerSubUpdateNow"
        case .nodesSubUrls: "ed2kNodesSubUrls"
        case .nodesSubStatus: "ed2kNodesSubUpdateNow"
        }
    }

    var detailKey: String? {
        switch self {
        case .kad: "ed2kEnableKadDesc"
        case .upnp: "ed2kEnableUpnpDesc"
        case .listenPort: "ed2kListenPortDesc"
        case .idCheck: "ed2kIdCheckDesc"
        case .serverList: "ed2kServerListDesc"
        case .serverSub: "ed2kServerSubDesc"
        case .serverSubUrls: "ed2kServerSubUrlsDesc"
        case .nodesSubUrls: "ed2kNodesSubUrlsDesc"
        case .serverSubStatus, .nodesSubStatus: nil
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

/// 订阅种类：存储键、条数来源与文案键（`btTrackerSub…` / `ed2kServerSub…` / `ed2kNodesSub…`）。
nonisolated enum SubscriptionKind {
    case btTrackers, ed2kServers, ed2kNodes

    /// 条数来源键：BT / eD2K 服务器按缓存列表计数；Kad 节点读整数键 `ed2k_nodes_dat_count`。
    var cacheKey: String {
        switch self {
        case .btTrackers: "bt_tracker_sub_cache"
        case .ed2kServers: "ed2k_server_sub_cache"
        case .ed2kNodes: "ed2k_nodes_dat_count"
        }
    }

    var updatedAtKey: String {
        switch self {
        case .btTrackers: "bt_tracker_sub_updated_at"
        case .ed2kServers: "ed2k_server_sub_updated_at"
        case .ed2kNodes: "ed2k_nodes_dat_updated_at"
        }
    }

    /// 配置快照里的已缓存条数。
    func storedCount(_ form: SettingsConfigForm) -> Int {
        switch self {
        case .btTrackers: SubscriptionListFormat.lines.count(form.string(cacheKey))
        case .ed2kServers: SubscriptionListFormat.comma.count(form.string(cacheKey))
        case .ed2kNodes: max(0, form.int(cacheKey, default: 0))
        }
    }

    var rowID: String {
        switch self {
        case .btTrackers: BtSettingsRow.trackerSubStatus.id
        case .ed2kServers: Ed2kSettingsRow.serverSubStatus.id
        case .ed2kNodes: Ed2kSettingsRow.nodesSubStatus.id
        }
    }

    var statusKey: String {
        switch self {
        case .btTrackers: "btTrackerSubStatus"
        case .ed2kServers: "ed2kServerSubStatus"
        case .ed2kNodes: "ed2kNodesSubStatus"
        }
    }

    var updatedAtTextKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdatedAt"
        case .ed2kServers: "ed2kServerSubUpdatedAt"
        case .ed2kNodes: "ed2kNodesSubUpdatedAt"
        }
    }

    var neverKey: String {
        switch self {
        case .btTrackers: "btTrackerSubNeverUpdated"
        case .ed2kServers: "ed2kServerSubNeverUpdated"
        case .ed2kNodes: "ed2kNodesSubNeverUpdated"
        }
    }

    var updateNowKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdateNow"
        case .ed2kServers: "ed2kServerSubUpdateNow"
        case .ed2kNodes: "ed2kNodesSubUpdateNow"
        }
    }

    var updatingKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdating"
        case .ed2kServers: "ed2kServerSubUpdating"
        case .ed2kNodes: "ed2kNodesSubUpdating"
        }
    }

    var failedKey: String {
        switch self {
        case .btTrackers: "btTrackerSubUpdateFailed"
        case .ed2kServers: "ed2kServerSubUpdateFailed"
        case .ed2kNodes: "ed2kNodesSubUpdateFailed"
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
        let storedCount = kind.storedCount(form)
        let storedAt = max(0, form.long(kind.updatedAtKey, default: 0))
        guard let fresh, fresh.success, fresh.updatedAt > storedAt else {
            return SubscriptionStatusModel(count: storedCount, updatedAt: storedAt)
        }
        return SubscriptionStatusModel(count: Int(clamping: fresh.count), updatedAt: fresh.updatedAt)
    }
}

// MARK: - eD2K ID 检测

/// 设置「常规」页 HighID / LowID 检测结果的展示（契约 §4）：结果只存页面状态，不落配置。
nonisolated enum Ed2kIdCheckDisplay: Equatable {
    /// 检测成功：`summary` = `ed2kIdCheckResult`；`lowHint` = LowID 时追加的提示。
    case ok(summary: String, lowHint: String?)
    /// `id == none` 或调用失败：`ed2kIdCheckFailed`（有错误详情时附上）。
    case failed(String)

    static func make(_ network: Ed2kNetworkDto) -> Ed2kIdCheckDisplay {
        let idKey: String
        switch network.id {
        case .none: return failure(network.error)
        case .high: idKey = "ed2kIdHigh"
        case .low: idKey = "ed2kIdLow"
        }
        let server = network.server.trimmingCharacters(in: .whitespacesAndNewlines)
        let summary = L("ed2kIdCheckResult", [
            "id": L(idKey),
            "server": server.isEmpty ? "—" : server,
            "port": network.listenPort == 0 ? "—" : String(network.listenPort),
            "upnp": upnpText(network),
        ])
        return .ok(summary: summary, lowHint: network.id == .low ? L("ed2kIdLowHint") : nil)
    }

    /// 调用失败（传输 / daemon 错误）或未登录：详情为空时只显示失败文案。
    static func failure(_ detail: String) -> Ed2kIdCheckDisplay {
        let detail = detail.trimmingCharacters(in: .whitespacesAndNewlines)
        return .failed(detail.isEmpty ? L("ed2kIdCheckFailed") : "\(L("ed2kIdCheckFailed")): \(detail)")
    }

    /// UPnP 状态文本：失败且有原因时为「失败: 原因」（与 BT 端口映射段拼法一致），否则为状态文案。
    static func upnpText(_ network: Ed2kNetworkDto) -> String {
        let reason = network.upnpError.trimmingCharacters(in: .whitespacesAndNewlines)
        guard network.upnp == .failed, !reason.isEmpty else { return L(upnpKey(network.upnp)) }
        return "\(L("ed2kUpnpFailed")): \(reason)"
    }

    static func upnpKey(_ state: Ed2kNetworkDto.UpnpState) -> String {
        switch state {
        case .disabled: "ed2kUpnpDisabled"
        case .pending: "ed2kUpnpPending"
        case .mapped: "ed2kUpnpMapped"
        case .failed: "ed2kUpnpFailed"
        }
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
