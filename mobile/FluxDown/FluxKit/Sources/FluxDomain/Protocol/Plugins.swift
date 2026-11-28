import Foundation

// 插件（`daemon.plugin.*`）DTO 与纯逻辑：镜像 `native/protocol/src/daemon.rs` 的插件段
// （同 `web/src/lib/rpc/protocol/plugin.ts` + `params.ts`），校验 / 市场 / 登录挑战规则对齐
// `web/src/pages/settings/sections/extensions/logic.ts` 与 GPUI `crates/extensions`。

// MARK: - DTO

/// select 控件选项。
public struct PluginSettingOption: Codable, Sendable, Hashable {
    public var value: String
    public var label: String

    public init(value: String, label: String) {
        self.value = value
        self.label = label
    }
}

/// 声明式设置项（`SettingFieldDto`）。
public struct PluginSettingField: Codable, Sendable, Hashable, Identifiable {
    public var key: String
    public var title: String
    public var description: String
    /// wire 字段名 `type`：`string` / `number` / `boolean`。
    public var settingType: String
    /// `text` / `password` / `textarea` / `select` / `toggle` / `number` / `folder`。
    public var widget: String
    public var options: [PluginSettingOption]
    /// wire 字段名 `default`。
    public var defaultValue: String?
    public var required: Bool
    public var min: Double?
    public var max: Double?
    public var pattern: String?
    /// 非空时 UI 在字段旁渲染复制按钮（仅复制文本，绝不执行）。
    public var helperScript: String?
    public var helperLabel: String?

    public var id: String { key }

    private enum CodingKeys: String, CodingKey {
        case key, title, description, widget, options, required, min, max, pattern, helperScript, helperLabel
        case settingType = "type"
        case defaultValue = "default"
    }

    public init(
        key: String,
        title: String = "",
        description: String = "",
        settingType: String = "string",
        widget: String = "text",
        options: [PluginSettingOption] = [],
        defaultValue: String? = nil,
        required: Bool = false,
        min: Double? = nil,
        max: Double? = nil,
        pattern: String? = nil,
        helperScript: String? = nil,
        helperLabel: String? = nil
    ) {
        self.key = key
        self.title = title
        self.description = description
        self.settingType = settingType
        self.widget = widget
        self.options = options
        self.defaultValue = defaultValue
        self.required = required
        self.min = min
        self.max = max
        self.pattern = pattern
        self.helperScript = helperScript
        self.helperLabel = helperLabel
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        key = try c.decode(String.self, forKey: .key)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        settingType = try c.decodeIfPresent(String.self, forKey: .settingType) ?? "string"
        widget = try c.decodeIfPresent(String.self, forKey: .widget) ?? "text"
        options = try c.decodeIfPresent([PluginSettingOption].self, forKey: .options) ?? []
        defaultValue = try c.decodeIfPresent(String.self, forKey: .defaultValue)
        required = try c.decodeIfPresent(Bool.self, forKey: .required) ?? false
        min = try c.decodeIfPresent(Double.self, forKey: .min)
        max = try c.decodeIfPresent(Double.self, forKey: .max)
        pattern = try c.decodeIfPresent(String.self, forKey: .pattern)
        helperScript = try c.decodeIfPresent(String.self, forKey: .helperScript)
        helperLabel = try c.decodeIfPresent(String.self, forKey: .helperLabel)
    }
}

public struct PluginResolverUrlConflict: Codable, Sendable, Hashable {
    public let pattern: String
    public let otherIdentity: String
    public let otherPattern: String
}

/// 已安装插件视图（`PluginDto`）。
public struct PluginDto: Codable, Sendable, Hashable, Identifiable {
    public var identity: String
    public var name: String
    public var version: String
    public var description: String
    public var homepage: String
    public var enabled: Bool
    public var devMode: Bool
    /// Empty for public/local/legacy installs; `private` for FluxCloud private installs.
    public var source: String
    /// `None` / `Manual` / `CircuitBreaker`。
    public var disabledReason: String
    public var settings: [PluginSettingField]
    /// 当前设置值（key → 字符串）。
    public var settingsValues: [String: String]
    /// manifest 声明的能力权限（如 `["ffmpeg"]`）。
    public var permissions: [String]
    /// 是否声明平台登录入口。
    public var authSupported: Bool
    public var subscriptionProviderIds: [String]
    /// `Loaded` / `Failed`；与 `enabled` 独立。
    public var loadStatus: String
    /// Legacy wire fallback; clients render localized `loadDetail` instead.
    public var loadError: String
    public var loadDetail: PluginDiagnostic?
    public var resolverUrlConflicts: [PluginResolverUrlConflict]

    public var id: String { identity }
    public var loadFailed: Bool { loadStatus == "Failed" }

    public init(
        identity: String,
        name: String,
        version: String,
        description: String = "",
        homepage: String = "",
        enabled: Bool = true,
        devMode: Bool = false,
        source: String = "",
        disabledReason: String = "None",
        settings: [PluginSettingField] = [],
        settingsValues: [String: String] = [:],
        permissions: [String] = [],
        authSupported: Bool = false,
        subscriptionProviderIds: [String] = [],
        loadStatus: String = "Loaded",
        loadError: String = "",
        loadDetail: PluginDiagnostic? = nil,
        resolverUrlConflicts: [PluginResolverUrlConflict] = []
    ) {
        self.identity = identity
        self.name = name
        self.version = version
        self.description = description
        self.homepage = homepage
        self.enabled = enabled
        self.devMode = devMode
        self.source = source
        self.disabledReason = disabledReason
        self.settings = settings
        self.settingsValues = settingsValues
        self.permissions = permissions
        self.authSupported = authSupported
        self.subscriptionProviderIds = subscriptionProviderIds
        self.loadStatus = loadStatus
        self.loadError = loadError
        self.loadDetail = loadDetail
        self.resolverUrlConflicts = resolverUrlConflicts
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        identity = try c.decode(String.self, forKey: .identity)
        name = try c.decode(String.self, forKey: .name)
        version = try c.decode(String.self, forKey: .version)
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        homepage = try c.decodeIfPresent(String.self, forKey: .homepage) ?? ""
        enabled = try c.decode(Bool.self, forKey: .enabled)
        devMode = try c.decodeIfPresent(Bool.self, forKey: .devMode) ?? false
        source = try c.decodeIfPresent(String.self, forKey: .source) ?? ""
        disabledReason = try c.decodeIfPresent(String.self, forKey: .disabledReason) ?? "None"
        settings = try c.decodeIfPresent([PluginSettingField].self, forKey: .settings) ?? []
        settingsValues = try c.decodeIfPresent([String: String].self, forKey: .settingsValues) ?? [:]
        permissions = try c.decodeIfPresent([String].self, forKey: .permissions) ?? []
        authSupported = try c.decodeIfPresent(Bool.self, forKey: .authSupported) ?? false
        subscriptionProviderIds = try c.decodeIfPresent([String].self, forKey: .subscriptionProviderIds) ?? []
        loadStatus = try c.decodeIfPresent(String.self, forKey: .loadStatus) ?? "Loaded"
        loadError = try c.decodeIfPresent(String.self, forKey: .loadError) ?? ""
        loadDetail = try? c.decodeIfPresent(PluginDiagnostic.self, forKey: .loadDetail)
        resolverUrlConflicts = try c.decodeIfPresent([PluginResolverUrlConflict].self, forKey: .resolverUrlConflicts) ?? []
    }
}

/// `daemon.plugin.auth` 请求（`PluginAuthRequest`）；省略字段由主机按 `#[serde(default)]` 补空串。
public struct PluginAuthRequest: Codable, Sendable, Hashable {
    public var identity: String
    /// `begin` / `poll` / `cancel` / `logout` / `status`。
    public var action: String
    public var site: String
    public var authRef: String
    public var sessionId: String
    public var input: String

    public init(
        identity: String,
        action: String,
        site: String = "",
        authRef: String = "",
        sessionId: String = "",
        input: String = ""
    ) {
        self.identity = identity
        self.action = action
        self.site = site
        self.authRef = authRef
        self.sessionId = sessionId
        self.input = input
    }
}

/// `daemon.plugin.auth` 响应（`PluginAuthResponse`）。
public struct PluginAuthResponse: Codable, Sendable, Hashable {
    /// `pending` / `success` / `error`。
    public var status: String
    public var sessionId: String
    /// 二维码文本、data URL 或其他挑战内容。
    public var challenge: String?
    public var challengeType: String?
    public var message: String
    public var authRef: String?

    public init(
        status: String,
        sessionId: String = "",
        challenge: String? = nil,
        challengeType: String? = nil,
        message: String = "",
        authRef: String? = nil
    ) {
        self.status = status
        self.sessionId = sessionId
        self.challenge = challenge
        self.challengeType = challengeType
        self.message = message
        self.authRef = authRef
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = try c.decode(String.self, forKey: .status)
        sessionId = try c.decodeIfPresent(String.self, forKey: .sessionId) ?? ""
        challenge = try c.decodeIfPresent(String.self, forKey: .challenge)
        challengeType = try c.decodeIfPresent(String.self, forKey: .challengeType)
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        authRef = try c.decodeIfPresent(String.self, forKey: .authRef)
    }
}

/// 安装成功结果（`InstalledPlugin`）；`missingComponents` 为所需但未安装的基础组件（提醒式，不阻断）。
public struct InstalledPlugin: Codable, Sendable, Hashable {
    public var identity: String
    public var missingComponents: [String]

    public init(identity: String, missingComponents: [String] = []) {
        self.identity = identity
        self.missingComponents = missingComponents
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        identity = try c.decode(String.self, forKey: .identity)
        missingComponents = try c.decodeIfPresent([String].self, forKey: .missingComponents) ?? []
    }
}

/// 市场索引条目（`MarketEntryDto`）。
public struct MarketEntry: Codable, Sendable, Hashable, Identifiable {
    public var pluginId: String
    public var version: String
    public var sequence: UInt64
    public var contentHash: String
    public var minAppVersion: String
    public var name: String
    public var description: String
    public var author: String
    public var homepage: String
    public var mirrors: [String]
    public var publishTime: String
    /// `none` = 可安装；`deprecated` / `vulnerable` / `malicious` = 已被发布者撤回。
    public var yanked: String
    public var tags: [String]
    public var permissions: [String]

    public var id: String { "\(pluginId)@\(version)" }
    public var displayName: String { name.isEmpty ? pluginId : name }
    /// 引擎只安装 `yanked == "none"` 的条目。
    public var installable: Bool { yanked == "none" }

    public init(
        pluginId: String,
        version: String,
        sequence: UInt64 = 0,
        contentHash: String = "",
        minAppVersion: String = "",
        name: String = "",
        description: String = "",
        author: String = "",
        homepage: String = "",
        mirrors: [String] = [],
        publishTime: String = "",
        yanked: String = "none",
        tags: [String] = [],
        permissions: [String] = []
    ) {
        self.pluginId = pluginId
        self.version = version
        self.sequence = sequence
        self.contentHash = contentHash
        self.minAppVersion = minAppVersion
        self.name = name
        self.description = description
        self.author = author
        self.homepage = homepage
        self.mirrors = mirrors
        self.publishTime = publishTime
        self.yanked = yanked
        self.tags = tags
        self.permissions = permissions
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        pluginId = try c.decode(String.self, forKey: .pluginId)
        version = try c.decode(String.self, forKey: .version)
        sequence = try c.decodeIfPresent(UInt64.self, forKey: .sequence) ?? 0
        contentHash = try c.decodeIfPresent(String.self, forKey: .contentHash) ?? ""
        minAppVersion = try c.decodeIfPresent(String.self, forKey: .minAppVersion) ?? ""
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        author = try c.decodeIfPresent(String.self, forKey: .author) ?? ""
        homepage = try c.decodeIfPresent(String.self, forKey: .homepage) ?? ""
        mirrors = try c.decodeIfPresent([String].self, forKey: .mirrors) ?? []
        publishTime = try c.decodeIfPresent(String.self, forKey: .publishTime) ?? ""
        yanked = try c.decodeIfPresent(String.self, forKey: .yanked) ?? ""
        tags = try c.decodeIfPresent([String].self, forKey: .tags) ?? []
        permissions = try c.decodeIfPresent([String].self, forKey: .permissions) ?? []
    }
}

// MARK: - 参数

/// `daemon.plugin.{uninstall,reloadDev}`。
public struct PluginIdentityParams: Codable, Sendable, Hashable {
    public var identity: String

    public init(identity: String) {
        self.identity = identity
    }
}

public struct PluginSetEnabledParams: Codable, Sendable, Hashable {
    public var identity: String
    public var enabled: Bool

    public init(identity: String, enabled: Bool) {
        self.identity = identity
        self.enabled = enabled
    }
}

public struct PluginUpdateSettingsParams: Codable, Sendable, Hashable {
    public var identity: String
    /// 设置键 → 字符串值。
    public var entries: [String: String]

    public init(identity: String, entries: [String: String]) {
        self.identity = identity
        self.entries = entries
    }
}

/// `daemon.plugin.install`：`blobId` 是经 `/api/web/blobs/plugins` 上传得到的一次性引用。
public struct PluginInstallParams: Codable, Sendable, Hashable {
    public var blobId: String

    public init(blobId: String) {
        self.blobId = blobId
    }
}

public struct PluginMarketInstallParams: Codable, Sendable, Hashable {
    public var pluginId: String
    /// 用户确认权限时看到的版本；最新可装版本不一致时主机拒绝（`marketVersionChanged`）。
    public var version: String?

    public init(pluginId: String, version: String?) {
        self.pluginId = pluginId
        self.version = version
    }
}

/// `daemon.plugin.ignoreRetry`（与 `TaskIdParams` 同形）。
public struct PluginIgnoreRetryParams: Codable, Sendable, Hashable {
    public var taskId: String

    public init(taskId: String) {
        self.taskId = taskId
    }
}

/// `daemon.plugin.siteDefaults` 的一行（`PluginSiteDefaultDto`）。
/// `automatic == true`：无人值守下载自动选定、尚未被用户确认。
public struct PluginSiteDefault: Codable, Sendable, Hashable, Identifiable {
    public var site: String
    public var identity: String
    public var candidates: [String]
    public var automatic: Bool

    public var id: String { site }

    public init(site: String, identity: String, candidates: [String], automatic: Bool) {
        self.site = site
        self.identity = identity
        self.candidates = candidates
        self.automatic = automatic
    }
}

/// `daemon.plugin.setSiteDefault`：`identity == nil` 移除条目（编码为 JSON `null`，不能省略键）。
public struct PluginSetSiteDefaultParams: Codable, Sendable, Hashable {
    public var site: String
    public var identity: String?

    public init(site: String, identity: String?) {
        self.site = site
        self.identity = identity
    }

    private enum CodingKeys: String, CodingKey { case site, identity }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(site, forKey: .site)
        try container.encode(identity, forKey: .identity)
    }
}

/// `WsServerMsg::PluginAutoDisabled`（`HostNoticeName.pluginAutoDisabled` 的载荷）。
public struct PluginAutoDisabledNotice: Codable, Sendable, Hashable {
    public var identity: String
    public var reason: String

    public init(identity: String, reason: String = "") {
        self.identity = identity
        self.reason = reason
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        identity = try c.decode(String.self, forKey: .identity)
        reason = try c.decodeIfPresent(String.self, forKey: .reason) ?? ""
    }
}

// MARK: - 设置校验

/// 前置校验失败原因；文案映射留给 UI 层。
public enum PluginFieldError: Sendable, Hashable {
    case required
    case number
    case min(String)
    case max(String)
    case select
    /// 主机校验失败（如 `pattern` 不匹配）；文案已本地化。
    case server(String)
}

/// 插件设置表单规则（GPUI `plugin_settings.rs` / Web `logic.ts` 一一对应）。
public enum PluginSettings {
    /// 单条前置校验：required → number / min / max → select 成员。`pattern` 由主机校验。
    public static func validate(_ field: PluginSettingField, raw: String) -> PluginFieldError? {
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if field.required, value.isEmpty { return .required }
        if value.isEmpty { return nil }
        if field.settingType == "number" {
            guard let number = parseDecimal(value), number.isFinite else { return .number }
            if let min = field.min, number < min { return .min(formatNumber(min)) }
            if let max = field.max, number > max { return .max(formatNumber(max)) }
        }
        if field.widget == "select", !field.options.isEmpty, !field.options.contains(where: { $0.value == value }) {
            return .select
        }
        return nil
    }

    /// 校验整张表单，返回 key → 错误。
    public static func validateAll(_ fields: [PluginSettingField], values: [String: String]) -> [String: PluginFieldError] {
        var errors: [String: PluginFieldError] = [:]
        for field in fields {
            if let error = validate(field, raw: values[field.key] ?? "") { errors[field.key] = error }
        }
        return errors
    }

    /// 实际发给引擎的值（GPUI `submit_value`）：引擎对数字按原串解析、对 select 要求成员、对 pattern 做
    /// 正则匹配，因此数字去掉首尾空白，可选且留空的这三类字段不上报（引擎会拒绝空串）。
    public static func submitValue(_ field: PluginSettingField, raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if field.settingType == "number" { return trimmed.isEmpty ? nil : trimmed }
        let rejectsEmpty = field.pattern != nil
            || (field.widget == "select" && !field.options.contains(where: { $0.value.isEmpty }))
        if trimmed.isEmpty, !field.required, rejectsEmpty { return nil }
        return raw
    }

    /// `daemon.plugin.updateSettings` 的 `entries`。
    public static func submitEntries(_ fields: [PluginSettingField], values: [String: String]) -> [String: String] {
        var entries: [String: String] = [:]
        for field in fields {
            if let value = submitValue(field, raw: values[field.key] ?? "") { entries[field.key] = value }
        }
        return entries
    }

    /// 初始值：已保存 > manifest 默认值 > toggle 的 `false`。
    public static func initialValue(_ field: PluginSettingField, saved: [String: String]) -> String {
        if let stored = saved[field.key] { return stored }
        if let fallback = field.defaultValue, !fallback.isEmpty { return fallback }
        return field.widget == "toggle" ? "false" : ""
    }

    public static func initialValues(_ fields: [PluginSettingField], saved: [String: String]) -> [String: String] {
        Dictionary(uniqueKeysWithValues: fields.map { ($0.key, initialValue($0, saved: saved)) })
    }

    /// 数字占位提示：`1 – 10` / `≥ 1` / `≤ 10`。
    public static func rangeHint(_ field: PluginSettingField) -> String? {
        switch (field.min, field.max) {
        case let (min?, max?): "\(formatNumber(min)) – \(formatNumber(max))"
        case let (min?, nil): "≥ \(formatNumber(min))"
        case let (nil, max?): "≤ \(formatNumber(max))"
        case (nil, nil): nil
        }
    }

    /// JS `String(number)` 的等价输出：整数不带小数点。
    public static func formatNumber(_ value: Double) -> String {
        if value.rounded() == value, abs(value) < 1e15 { return String(Int64(value)) }
        return String(value)
    }

    /// 十进制浮点（与 Rust `f64::from_str` 对常规输入的接受范围一致，不含 hex / inf / NaN）。
    static func parseDecimal(_ text: String) -> Double? {
        var chars = Array(text.unicodeScalars)
        var normalized = ""
        if let first = chars.first, first == "+" || first == "-" {
            normalized.unicodeScalars.append(first)
            chars.removeFirst()
        }
        var index = 0
        var intDigits = 0
        var fracDigits = 0
        var mantissa = ""
        while index < chars.count, isDigit(chars[index]) {
            mantissa.unicodeScalars.append(chars[index])
            index += 1
            intDigits += 1
        }
        var sawDot = false
        if index < chars.count, chars[index] == "." {
            sawDot = true
            mantissa += "."
            index += 1
            while index < chars.count, isDigit(chars[index]) {
                mantissa.unicodeScalars.append(chars[index])
                index += 1
                fracDigits += 1
            }
        }
        guard intDigits + fracDigits > 0 else { return nil }
        if intDigits == 0 { mantissa = "0" + mantissa }
        if sawDot, fracDigits == 0 { mantissa += "0" }
        normalized += mantissa
        if index < chars.count {
            guard chars[index] == "e" || chars[index] == "E" else { return nil }
            index += 1
            var exponent = "e"
            if index < chars.count, chars[index] == "+" || chars[index] == "-" {
                exponent.unicodeScalars.append(chars[index])
                index += 1
            }
            var expDigits = 0
            while index < chars.count, isDigit(chars[index]) {
                exponent.unicodeScalars.append(chars[index])
                index += 1
                expDigits += 1
            }
            guard expDigits > 0, index == chars.count else { return nil }
            normalized += exponent
        }
        return Double(normalized)
    }

    private static func isDigit(_ scalar: Unicode.Scalar) -> Bool {
        scalar.value >= 0x30 && scalar.value <= 0x39
    }
}

// MARK: - 市场

public enum MarketAction: Sendable, Hashable {
    case install, update, installed, unavailable
}

public enum PluginMarket {
    /// 市场列表每次展开的条数。
    public static let pageSize = 50

    /// 关键字过滤：名称 / id / 描述 / 作者 / 标签任一命中（大小写不敏感）。
    public static func filter(_ entries: [MarketEntry], query: String) -> [MarketEntry] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if needle.isEmpty { return entries }
        func hit(_ value: String) -> Bool { value.lowercased().contains(needle) }
        return entries.filter { entry in
            hit(entry.name) || hit(entry.pluginId) || hit(entry.description) || hit(entry.author) || entry.tags.contains(where: hit)
        }
    }

    /// 每个插件一条（`crates/extensions/src/market.rs::latest_per_plugin`）：最新可安装版本；
    /// 全部撤回（下架）的插件不出现。顺序为各插件在索引中首次出现的位置。
    public static func latestPerPlugin(_ entries: [MarketEntry]) -> [MarketEntry] {
        var order: [String] = []
        var best: [String: MarketEntry] = [:]
        for entry in entries {
            guard let current = best[entry.pluginId] else {
                best[entry.pluginId] = entry
                order.append(entry.pluginId)
                continue
            }
            let a = entry.installable
            let b = current.installable
            let better = a != b ? a : entry.sequence > current.sequence
            if better { best[entry.pluginId] = entry }
        }
        return order.compactMap { best[$0] }.filter(\.installable)
    }

    /// `MAJOR.MINOR.PATCH`（可带前导 `v`，忽略预发布 / 构建后缀）；无法解析为 nil。
    static func parseSemver(_ version: String) -> [Int]? {
        var text = version.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.hasPrefix("v") { text.removeFirst() }
        let core = text.split(maxSplits: 1, omittingEmptySubsequences: false, whereSeparator: { $0 == "-" || $0 == "+" }).first ?? ""
        let parts = core.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 3 else { return nil }
        var numbers: [Int] = []
        for part in parts {
            guard !part.isEmpty, part.allSatisfy({ $0.isASCII && $0.isNumber }), let number = Int(part) else { return nil }
            numbers.append(number)
        }
        return numbers
    }

    /// `candidate` 严格新于 `current`；任一侧无法解析视为不可比较。
    public static func versionNewer(_ candidate: String, than current: String) -> Bool {
        guard let a = parseSemver(candidate), let b = parseSemver(current) else { return false }
        for i in 0 ..< 3 where a[i] != b[i] { return a[i] > b[i] }
        return false
    }

    /// 市场条目相对本机安装状态的动作；开发模式插件不被市场覆盖。
    public static func action(for entry: MarketEntry, installed: PluginDto?) -> MarketAction {
        guard let installed else { return entry.installable ? .install : .unavailable }
        if !installed.devMode, entry.installable, versionNewer(entry.version, than: installed.version) { return .update }
        return .installed
    }

    /// 需要用户确认的权限：新装取全部权限，更新只取已安装版本没有的新增权限。
    public static func permissionsToConfirm(_ entry: MarketEntry, installed: PluginDto?) -> [String] {
        let granted = installed?.permissions ?? []
        return entry.permissions.filter { !granted.contains($0) }
    }

    /// 已安装版本在市场中的撤回标记；未撤回 / 市场无此版本为 nil。
    public static func installedVersionYanked(_ entries: [MarketEntry], plugin: PluginDto) -> String? {
        if plugin.devMode { return nil }
        guard let hit = entries.first(where: { $0.pluginId == plugin.identity && $0.version == plugin.version }),
              !hit.yanked.isEmpty, hit.yanked != "none"
        else { return nil }
        return hit.yanked
    }

    /// 撤回标记 → i18n 键；空 / 未知值不展示。
    public static func yankedLabelKey(_ yanked: String) -> String? {
        switch yanked {
        case "deprecated": "marketYankedDeprecated"
        case "vulnerable": "marketYankedVulnerable"
        case "malicious": "marketYankedMalicious"
        default: nil
        }
    }

    /// 权限 → i18n 键（名称、说明）；未知权限为 nil（调用侧回退原名 + 未知说明）。
    public static func permissionKeys(_ permission: String) -> (name: String, desc: String)? {
        switch permission {
        case "ffmpeg": ("pluginPermFfmpegName", "pluginPermFfmpegDesc")
        case "ytdlp": ("pluginPermYtdlpName", "pluginPermYtdlpDesc")
        case "auth": ("pluginPermAuthName", "pluginPermAuthDesc")
        default: nil
        }
    }
}

// MARK: - 登录挑战

/// 插件平台登录对话的本地状态。
public struct PluginAuthState: Sendable, Hashable {
    public var status: String
    public var sessionId: String
    public var authRef: String
    public var challenge: String?
    public var challengeType: String?
    public var message: String?

    public init(
        status: String = "",
        sessionId: String = "",
        authRef: String = "",
        challenge: String? = nil,
        challengeType: String? = nil,
        message: String? = nil
    ) {
        self.status = status
        self.sessionId = sessionId
        self.authRef = authRef
        self.challenge = challenge
        self.challengeType = challengeType
        self.message = message
    }

    /// 已登录：status 在不同 action 下语义不同（logout 的 success 是「注销成功」），故额外要求 authRef 非空。
    public var loggedIn: Bool { status == "success" && !authRef.isEmpty }
    public var sessionPending: Bool { status == "pending" && !sessionId.isEmpty }
    public var isQrChallenge: Bool { PluginAuth.isQrcode(challengeType) }
}

public enum PluginAuth {
    /// 二维码轮询间隔。
    public static let pollInterval: Duration = .seconds(2)
    /// 挑战文本超过该长度（按码点）即截断显示（复制始终给完整原文）。
    public static let challengeTextLimit = 512
    /// `data:image/...;base64,` 挑战超过该长度直接放弃图片渲染，退化为文本 + 复制。
    public static let maxChallengeDataURLLength = 256 * 1024
    /// QR Model 2 即使最低纠错级别也最多容纳 7089 个数字；更大的不可信载荷不交给编码器。
    public static let maxQRTextLength = 7089

    private static let imageMimes: Set<String> = [
        "image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/svg+xml",
    ]

    public static func isQrcode(_ type: String?) -> Bool {
        type?.lowercased() == "qrcode"
    }

    /// pending 的 poll 可省略挑战（沿用旧值）；终态不沿用旧挑战；logout 始终清空本地登录态。
    public static func apply(_ previous: PluginAuthState, response: PluginAuthResponse, wasLogout: Bool) -> PluginAuthState {
        let pending = response.status == "pending"
        let next = PluginAuthState(
            status: response.status,
            sessionId: response.sessionId,
            authRef: response.authRef ?? "",
            challenge: pending ? (response.challenge ?? previous.challenge) : response.challenge,
            challengeType: pending ? (response.challengeType ?? previous.challengeType) : response.challengeType,
            message: response.message.isEmpty ? nil : response.message
        )
        guard wasLogout else { return next }
        var cleared = next
        cleared.authRef = ""
        cleared.sessionId = ""
        cleared.challenge = nil
        cleared.challengeType = nil
        return cleared
    }

    /// 挑战不可信：只有形如 `data:image/<mime>[;..];base64,<payload>`、mime 已知、载荷是合法 base64 且
    /// 长度未超限的值才当图片；返回解码后的图片字节，其余为 nil（调用侧退化为截断文本 + 复制）。
    public static func dataImageBytes(_ value: String) -> Data? {
        if value.count > maxChallengeDataURLLength { return nil }
        guard value.lowercased().hasPrefix("data:"), let comma = value.firstIndex(of: ",") else { return nil }
        let header = value[value.index(value.startIndex, offsetBy: 5) ..< comma]
        let pieces = header.split(separator: ";", omittingEmptySubsequences: false).map { $0.lowercased() }
        guard let mime = pieces.first, imageMimes.contains(mime), pieces.dropFirst().contains("base64") else { return nil }
        let payload = value[value.index(after: comma)...].filter { !$0.isWhitespace }
        guard !payload.isEmpty, payload.count % 4 != 1,
              payload.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "+" || $0 == "/" || $0 == "=") })
        else { return nil }
        var padded = String(payload)
        while padded.count % 4 != 0 { padded += "=" }
        return Data(base64Encoded: padded)
    }

    /// 挑战文本超过 ``challengeTextLimit`` 个字符时截断并追加省略号。
    public static func truncate(_ value: String) -> String {
        guard value.count > challengeTextLimit else { return value }
        return String(value.prefix(challengeTextLimit)) + "…"
    }

    /// 仅 http(s) 链接可点击（挑战 / 主页均不可信，拒绝 `javascript:` 等）。
    public static func safeHTTPURL(_ value: String) -> URL? {
        guard let url = URL(string: value), let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https", url.host?.isEmpty == false
        else { return nil }
        return url
    }
}

// MARK: - 详情

/// 详情页数据：已安装插件与市场条目映射到同一形状。
public struct PluginDetail: Sendable, Hashable {
    public var name: String
    public var version: String
    public var identity: String
    public var description: String
    public var homepage: String
    public var author: String
    public var tags: [String]
    public var publishTime: String
    public var minAppVersion: String
    public var settingsCount: Int
    public var permissions: [String]
    public var yanked: String

    public init(plugin: PluginDto) {
        name = plugin.name
        version = plugin.version
        identity = plugin.identity
        description = plugin.description
        homepage = plugin.homepage
        author = ""
        tags = []
        publishTime = ""
        minAppVersion = ""
        settingsCount = plugin.settings.count
        permissions = plugin.permissions
        yanked = ""
    }

    public init(market entry: MarketEntry) {
        name = entry.displayName
        version = entry.version
        identity = entry.pluginId
        description = entry.description
        homepage = entry.homepage
        author = entry.author
        tags = entry.tags
        publishTime = entry.publishTime
        minAppVersion = entry.minAppVersion
        settingsCount = 0
        permissions = entry.permissions
        yanked = entry.yanked
    }
}
