import FluxDomain
import Foundation

// 设置页的纯逻辑（无 UI / 无主机依赖），便于单测。

// MARK: - 限速（1024 进制）

/// 限速单位（`speed_limit_bytes` / `upload_limit_bytes` 存字节/秒）。
nonisolated enum SettingsRateUnit: String, CaseIterable, Identifiable, Sendable {
    case kb, mb, gb

    var id: String { rawValue }

    var factor: Int64 {
        switch self {
        case .kb: 1024
        case .mb: 1024 * 1024
        case .gb: 1024 * 1024 * 1024
        }
    }

    /// 单位符号（走 i18n，便于各语言按习惯书写）。
    var label: String {
        switch self {
        case .kb: L("speedUnitKbps")
        case .mb: L("speedUnitMbps")
        case .gb: L("speedUnitGbps")
        }
    }

    /// `Stepper` 步长：KB = 64，MB / GB = 1（03-settings §2.3）。
    var step: Double { self == .kb ? 64 : 1 }
}

enum SettingsRateLimit {
    /// 快捷档位（`FD.data.speedLimitPresets`）：0 = 不限制，其余 (数值, 单位)。
    static let presets: [(amount: Int, unit: SettingsRateUnit)] = [
        (0, .mb), (512, .kb), (1, .mb), (2, .mb), (5, .mb), (10, .mb), (20, .mb),
    ]

    /// 档位对应的字节/秒。
    static func bytes(amount: Int, unit: SettingsRateUnit) -> Int64 {
        Int64(amount) * unit.factor
    }

    /// `bytes` 在 `unit` 下的显示文本：最多两位小数、去掉多余的 0；≤ 0 → 空串（显示「不限制」占位）。
    static func text(bytes: Int64, unit: SettingsRateUnit) -> String {
        guard bytes > 0 else { return "" }
        var s = String(format: "%.2f", locale: Locale(identifier: "en_US_POSIX"), Double(bytes) / Double(unit.factor))
        if s.contains(".") {
            while s.hasSuffix("0") { s.removeLast() }
            if s.hasSuffix(".") { s.removeLast() }
        }
        return s.isEmpty ? "0" : s
    }

    /// 文本 → 字节/秒。空串 = 0（不限制）；负数 / 非数字 = nil（非法，调用方回滚）。
    static func parse(_ text: String, unit: SettingsRateUnit) -> Int64? {
        let normalized = text.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: ",", with: ".")
        if normalized.isEmpty { return 0 }
        guard let value = Double(normalized), value.isFinite, value >= 0 else { return nil }
        let raw = value * Double(unit.factor)
        if raw >= Double(Int64.max) { return Int64.max }
        return Int64(raw.rounded())
    }

    /// `unit` 下两位小数即可精确表示 `bytes`。
    static func isRepresentable(_ bytes: Int64, in unit: SettingsRateUnit) -> Bool {
        parse(text(bytes: bytes, unit: unit), unit: unit) == bytes
    }

    /// 首选单位能精确表示就用它；否则取能精确表示的最大单位；都不行回落 KB/s。
    static func effectiveUnit(bytes: Int64, preferred: SettingsRateUnit) -> SettingsRateUnit {
        if bytes <= 0 || isRepresentable(bytes, in: preferred) { return preferred }
        return [SettingsRateUnit.gb, .mb, .kb].first { isRepresentable(bytes, in: $0) } ?? .kb
    }

    /// 输入过滤：仅数字与至多一个小数点（`,` 视同 `.`），最多 12 个字符。
    static func sanitize(_ input: String) -> String {
        var seenDot = false
        var result = ""
        for ch in input {
            if ch.isASCII, ch.isNumber {
                result.append(ch)
            } else if (ch == "." || ch == ","), !seenDot {
                seenDot = true
                result.append(".")
            }
            if result.count >= 12 { break }
        }
        return result
    }

    /// 步进后的字节/秒（以 `unit` 的步长，下限 0）。
    static func stepped(bytes: Int64, unit: SettingsRateUnit, up: Bool) -> Int64 {
        let current = Double(bytes) / Double(unit.factor)
        let next = max(0, current + (up ? unit.step : -unit.step))
        return Int64((next * Double(unit.factor)).rounded())
    }
}

// MARK: - 数值钳位

nonisolated enum SettingsNumber {
    /// 钳位到 `range`；`adjusted` = 输入越界被改写（触发「已调整为 n」提示 + 警告触感）。
    static func clamp(_ value: Int, to range: ClosedRange<Int>) -> (value: Int, adjusted: Bool) {
        let clamped = min(max(value, range.lowerBound), range.upperBound)
        return (clamped, clamped != value)
    }

    /// 文本 → 整数。接受前后空白与 Unicode 减号；不可解析 → nil。超出 `Int` 的数字饱和到 `Int.max/min`
    /// 以便随后被钳位（而不是当作非法输入）。
    static func parse(_ text: String) -> Int? {
        var s = text.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "\u{2212}", with: "-")
        guard !s.isEmpty else { return nil }
        var negative = false
        if s.hasPrefix("-") {
            negative = true
            s.removeFirst()
        }
        guard !s.isEmpty, s.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        if let v = Int(s) { return negative ? -v : v }
        return negative ? Int.min : Int.max
    }
}

// MARK: - 配置视图

/// 设置项（行定义）：一个目录键 + 文案键。daemon 配置键与 agent 偏好键共用，读写由 `ConfigEditor` 按目录路由。
nonisolated struct SettingsItem: Identifiable, Hashable {
    /// 搜索定位 / 滚动用的行标识（如 `download.maxConcurrent`）。
    let id: String
    /// `SettingsCatalog` 键。
    let key: String
    let titleKey: String
    let detailKey: String?

    init(id: String, key: String, titleKey: String, detailKey: String? = nil) {
        self.id = id
        self.key = key
        self.titleKey = titleKey
        self.detailKey = detailKey
    }

    var title: String { L(titleKey) }
    var detail: String? { detailKey.map { L($0) } }
    /// 该键参与云同步（标题后显示 ☁︎）。
    var isSynced: Bool { SettingsCatalog.isSynced(key) }
}

/// 一次渲染内的配置视图：显示值 = 乐观值 ?? 主机值 ?? 目录默认（同 Android `Form` / Web `useDaemonValue`）。
nonisolated struct SettingsConfigForm: Equatable {
    /// `daemon.config` 快照值（wire 字符串）。
    var host: [String: String] = [:]
    var optimistic: [String: String] = [:]
    /// `agent.preferences` 当前值。
    var prefs: [String: JSONValue] = [:]
    /// 原始 JSON 偏好的乐观值（如 `custom_categories`）。
    var optimisticPrefs: [String: JSONValue] = [:]

    /// 主机配置已加载（连接前 / 旧主机为空：daemon 行一律不渲染）。
    var isLoaded: Bool { !host.isEmpty }

    /// 键可用：偏好键恒可用（缺省取目录默认）；daemon 键要求配置已加载（目录内的键缺省取默认值，同 Web）。
    func has(_ key: String) -> Bool {
        if SettingsCatalog.field(key)?.store == .preference { return true }
        return isLoaded && (host[key] != nil || SettingsCatalog.field(key) != nil)
    }

    func any(_ keys: [String]) -> Bool { keys.contains { has($0) } }

    /// 显示值（wire 字符串）；不可用键 → nil。
    func value(_ key: String) -> String? {
        if let local = optimistic[key] { return local }
        if SettingsCatalog.field(key)?.store == .preference {
            if let json = optimisticPrefs[key] ?? prefs[key], let wire = SettingsCatalog.wire(fromJSON: json) { return wire }
            return SettingsCatalog.defaultWire(key)
        }
        guard isLoaded else { return nil }
        return host[key] ?? SettingsCatalog.field(key)?.defaultWire
    }

    /// 原始 JSON 偏好（乐观值优先）。
    func pref(_ key: String) -> JSONValue? { optimisticPrefs[key] ?? prefs[key] }

    func bool(_ key: String) -> Bool {
        let v = value(key)
        return v == "true" || v == "1"
    }

    func int(_ key: String, default fallback: Int) -> Int {
        value(key).flatMap { SettingsNumber.parse($0) } ?? fallback
    }

    func long(_ key: String, default fallback: Int64) -> Int64 {
        value(key).flatMap { Int64($0.trimmingCharacters(in: .whitespaces)) } ?? fallback
    }

    func double(_ key: String, default fallback: Double) -> Double {
        value(key).flatMap { Double($0.trimmingCharacters(in: .whitespaces)) } ?? fallback
    }

    func string(_ key: String, default fallback: String = "") -> String { value(key) ?? fallback }

    static func wire(_ flag: Bool) -> String { flag ? "true" : "false" }
}

// MARK: - 保存目录

enum SettingsSaveDirectory {
    /// 远端主机路径：留空（使用主机默认）或绝对路径（POSIX `/…`、Windows `C:\…` / `C:/…`、UNC `\\…`）。
    static func isValid(_ path: String) -> Bool {
        let p = path.trimmingCharacters(in: .whitespacesAndNewlines)
        if p.isEmpty || p.hasPrefix("/") || p.hasPrefix("\\\\") { return true }
        let chars = Array(p)
        return chars.count >= 3 && chars[0].isASCII && chars[0].isLetter && chars[1] == ":" && (chars[2] == "\\" || chars[2] == "/")
    }

    /// 读数用末段目录名（`/a/b/` → `b`；`C:\x\y` → `y`）。
    static func lastComponent(_ path: String) -> String {
        var p = Substring(path)
        while let last = p.last, last == "/" || last == "\\" { p = p.dropLast() }
        guard let cut = p.lastIndex(where: { $0 == "/" || $0 == "\\" }) else { return String(p) }
        return String(p[p.index(after: cut)...])
    }
}

// MARK: - 下载页行目录

/// 下载页分组（顺序即页面顺序）。
nonisolated enum SettingsDownloadSection: CaseIterable {
    case saveLocation, behavior, connection, retry, advanced

    var titleKey: String {
        switch self {
        case .saveLocation: "settingsGroupSaveLocation"
        case .behavior: "settingsGroupBehavior"
        case .connection: "settingsGroupConnection"
        case .retry: "settingsGroupRetry"
        case .advanced: "settingsGroupAdvanced"
        }
    }
}

/// 可见性判定所需的上下文。
nonisolated struct SettingsDownloadContext: Equatable {
    var form: SettingsConfigForm
    var isLocalHost: Bool
}

/// 下载页的每一行（Android `DownloadPage` 编辑的键，一一对应）。
/// 页面渲染与设置搜索共用同一份可见性判定。
nonisolated enum SettingsDownloadRow: String, CaseIterable, Identifiable {
    case saveDir, rememberLastSaveDir
    case silentDownload, silentAutoCategory, silentSkipSelection, useServerTime, fileExistsBehavior
    case fileMissingAction, completedRecordRetention, idleFileScan, defaultQueue, dedupSameUrl
    case defaultSegments, autoMaxConnections, cdnMulti, cdnMaxNodes, multiNic, maxConcurrent
    case speedLimit, uploadLimit
    case autoRetryCount, autoRetryDelay, autoResumeOnStart
    case userAgent

    var id: String { "download.\(rawValue)" }

    var group: SettingsDownloadSection {
        switch self {
        case .saveDir, .rememberLastSaveDir: .saveLocation
        case .silentDownload, .silentAutoCategory, .silentSkipSelection, .useServerTime, .fileExistsBehavior,
             .fileMissingAction, .completedRecordRetention, .idleFileScan, .defaultQueue, .dedupSameUrl: .behavior
        case .defaultSegments, .autoMaxConnections, .cdnMulti, .cdnMaxNodes, .multiNic, .maxConcurrent,
             .speedLimit, .uploadLimit: .connection
        case .autoRetryCount, .autoRetryDelay, .autoResumeOnStart: .retry
        case .userAgent: .advanced
        }
    }

    /// 绑定的主机配置键。
    var configKey: String {
        switch self {
        case .saveDir: "default_save_dir"
        case .rememberLastSaveDir: "download.remember_last_save_dir"
        case .silentDownload: "download.silent_download"
        case .silentAutoCategory: "download.silent_auto_category"
        case .silentSkipSelection: "download.silent_skip_selection"
        case .useServerTime: "use_server_time"
        case .fileExistsBehavior: "file_exists_behavior"
        case .fileMissingAction: "file_missing_action"
        case .completedRecordRetention: "completed_record_retention_days"
        case .idleFileScan: "idle_file_scan"
        case .defaultQueue: "default_queue_id"
        case .dedupSameUrl: "dedup_same_url"
        case .defaultSegments: "default_segments"
        case .autoMaxConnections: "auto_max_connections"
        case .cdnMulti: "cdn_multi_enabled"
        case .cdnMaxNodes: "cdn_max_nodes"
        case .multiNic: "multi_nic_enabled"
        case .maxConcurrent: "max_concurrent_tasks"
        case .speedLimit: "speed_limit_bytes"
        case .uploadLimit: "upload_limit_bytes"
        case .autoRetryCount: "max_auto_retries"
        case .autoRetryDelay: "auto_retry_delay_secs"
        case .autoResumeOnStart: "auto_resume_on_start"
        case .userAgent: "global_user_agent"
        }
    }

    var titleKey: String {
        switch self {
        case .saveDir: "defaultSaveDir"
        case .rememberLastSaveDir: "rememberLastSaveDir"
        case .silentDownload: "silentDownload"
        case .silentAutoCategory: "silentAutoCategory"
        case .silentSkipSelection: "silentSkipSelection"
        case .useServerTime: "useServerTime"
        case .fileExistsBehavior: "fileExistsBehavior"
        case .fileMissingAction: "fileMissingAction"
        case .completedRecordRetention: "completedRecordRetention"
        case .idleFileScan: "idleFileScan"
        case .defaultQueue: "defaultQueueSetting"
        case .dedupSameUrl: "mobileDedupSameUrl"
        case .defaultSegments: "defaultThreads"
        case .autoMaxConnections: "autoMaxConnections"
        case .cdnMulti: "cdnMultiEnabled"
        case .cdnMaxNodes: "cdnMaxNodes"
        case .multiNic: "multiNicEnabled"
        case .maxConcurrent: "maxConcurrent"
        case .speedLimit: "speedLimit"
        case .uploadLimit: "uploadLimit"
        case .autoRetryCount: "autoRetryCount"
        case .autoRetryDelay: "autoRetryDelay"
        case .autoResumeOnStart: "autoResumeOnStart"
        case .userAgent: "userAgent"
        }
    }

    var detailKey: String {
        switch self {
        case .saveDir: "defaultSaveDirDesc"
        case .rememberLastSaveDir: "rememberLastSaveDirDesc"
        case .silentDownload: "silentDownloadDesc"
        case .silentAutoCategory: "silentAutoCategoryDesc"
        case .silentSkipSelection: "silentSkipSelectionDesc"
        case .useServerTime: "useServerTimeDesc"
        case .fileExistsBehavior: "fileExistsBehaviorDesc"
        case .fileMissingAction: "fileMissingActionDesc"
        case .completedRecordRetention: "completedRecordRetentionDesc"
        case .idleFileScan: "idleFileScanDesc"
        case .defaultQueue: "defaultQueueSettingDesc"
        case .dedupSameUrl: "mobileDedupSameUrlDesc"
        case .defaultSegments: "defaultThreadsDesc"
        case .autoMaxConnections: "autoMaxConnectionsDesc"
        case .cdnMulti: "cdnMultiEnabledDesc"
        case .cdnMaxNodes: "cdnMaxNodesDesc"
        case .multiNic: "multiNicEnabledDesc"
        case .maxConcurrent: BuildFlavor.key(full: "maxConcurrentDesc", store: "maxConcurrentDescGeneric")
        case .speedLimit: "speedLimitDesc"
        case .uploadLimit: "uploadLimitDesc"
        case .autoRetryCount: "autoRetryCountDesc"
        case .autoRetryDelay: "autoRetryDelayDesc"
        case .autoResumeOnStart: "autoResumeOnStartDesc"
        case .userAgent: "userAgentDesc"
        }
    }

    /// 绑定的设置项（读写按目录路由；`isSynced` 决定 ☁︎）。
    var item: SettingsItem {
        SettingsItem(id: id, key: configKey, titleKey: titleKey, detailKey: detailKey)
    }

    /// 该行当前是否渲染（主机配置已加载 / 偏好恒可用；条件行依赖前置值）。
    func isVisible(in ctx: SettingsDownloadContext) -> Bool {
        let f = ctx.form
        switch self {
        case .silentAutoCategory, .silentSkipSelection:
            return f.has(configKey) && f.bool(SettingsDownloadRow.silentDownload.configKey)
        case .idleFileScan:
            // 本机不为刷新 UI 周期轮询（03-settings §6.2）：回前台 10s 节流重扫代替；连接 NAS 时保留。
            return f.has(configKey) && !ctx.isLocalHost
        case .autoMaxConnections:
            return f.has(configKey) && f.has(SettingsDownloadRow.defaultSegments.configKey)
                && f.int(SettingsDownloadRow.defaultSegments.configKey, default: 0) == 0
        case .cdnMaxNodes:
            return f.has(configKey) && f.has(SettingsDownloadRow.cdnMulti.configKey) && f.bool(SettingsDownloadRow.cdnMulti.configKey)
        default:
            return f.has(configKey)
        }
    }

    static func visible(in group: SettingsDownloadSection, _ ctx: SettingsDownloadContext) -> [SettingsDownloadRow] {
        allCases.filter { $0.group == group && $0.isVisible(in: ctx) }
    }
}

/// 已完成记录保留天数的下拉模式逻辑（镜像 GPUI `sections/retention.rs` 与 Web `retention.ts`）。
nonisolated enum SettingsRetention {
    /// 预设天数（0 = 从不），与下拉项一一对应。
    static let presets = [0, 1, 7, 30]
    /// 切到「自定义」且没有可复用的自定义值时的起始天数（刻意不等于任何预设）。
    static let customStart = 14
    static let customID = "custom"

    /// 下拉当前项：预设天数的字符串，或 `customID`。
    static func modeID(days: Int, customFlag: Bool) -> String {
        if (customFlag && days > 0) || !presets.contains(days) { return customID }
        return String(days)
    }

    /// 切到「自定义」时写入的天数：当前值已是非预设值则沿用，否则用起始值。
    static func customTarget(current: Int, max: Int) -> Int {
        let base = current > 0 && !presets.contains(current) ? current : customStart
        return min(max, Swift.max(1, base))
    }
}

// MARK: - 设置搜索

/// 搜索索引项。`title` / `detail` / `breadcrumb` / `keywords` 由调用方用 `L(...)` 本地化后传入；
/// `keywords` 是 PC 命令面板的别名词表（`searchKeywords…`），只做子串匹配、排在标题 / 说明之后。
nonisolated struct SettingsEntry: Identifiable, Hashable {
    let id: String
    let route: SettingsRoute
    let title: String
    let detail: String
    let breadcrumb: String
    let symbol: String
    var keywords: [String] = []
}

extension SettingsEntry {
    /// 由设置项生成搜索条目（`breadcrumb` 如 `下载 › 连接与性能`）。
    init(item: SettingsItem, route: SettingsRoute, breadcrumb: String, symbol: String) {
        self.init(
            id: item.id, route: route, title: item.title, detail: item.detail ?? "",
            breadcrumb: breadcrumb, symbol: symbol
        )
    }
}

/// 各设置页向搜索索引贡献条目时的上下文：与页面渲染共用同一份可见性判定，
/// 因此被条件隐藏 / 能力缺失 / 桌面专属的行不会出现在结果里。
struct SettingsSearchContext {
    let form: SettingsConfigForm
    let isLocalHost: Bool
    let capabilities: Set<String>

    func has(_ capability: String) -> Bool { capabilities.contains(capability) }
}

enum SettingsSearch {
    /// 大小写 / 变音不敏感的子串匹配（标题 / 说明 / 别名词）；标题命中排在前，其次说明命中，再次别名命中，同级保持索引序。
    static func filter(_ entries: [SettingsEntry], query: String) -> [SettingsEntry] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return [] }
        let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive, .widthInsensitive]
        var titleHits: [SettingsEntry] = []
        var detailHits: [SettingsEntry] = []
        var keywordHits: [SettingsEntry] = []
        for entry in entries {
            if entry.title.range(of: needle, options: options) != nil {
                titleHits.append(entry)
            } else if entry.detail.range(of: needle, options: options) != nil {
                detailHits.append(entry)
            } else if entry.keywords.contains(where: { $0.range(of: needle, options: options) != nil }) {
                keywordHits.append(entry)
            }
        }
        return titleHits + detailHits + keywordHits
    }
}

// MARK: - 版本

enum SettingsAppVersion {
    /// `1.0` / `1.0 (42)`；缺失的版本号返回空串。
    static func display(short: String?, build: String?) -> String {
        let version = short?.trimmingCharacters(in: .whitespaces) ?? ""
        guard !version.isEmpty else { return "" }
        let number = build?.trimmingCharacters(in: .whitespaces) ?? ""
        return number.isEmpty || number == version ? version : "\(version) (\(number))"
    }

    static var current: String {
        let info = Bundle.main.infoDictionary
        return display(short: info?["CFBundleShortVersionString"] as? String, build: info?["CFBundleVersion"] as? String)
    }
}
