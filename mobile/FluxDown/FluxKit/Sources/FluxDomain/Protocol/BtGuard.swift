import Foundation

// BT 防吸血（`daemon.bt.guard.*`）：DTO 镜像 `native/protocol/src/daemon.rs` 的 BtGuard 段（同 Web
// `lib/rpc/protocol` 与 `lib/btGuard.ts`）。纯逻辑（CIDR 校验、剩余时长、云名单状态、批量解除分块）
// 与 Web / GPUI / Android 逐条对齐。
//
// 客户端只调用 state / entries.* / events.list / export / filterSources.refresh；
// `cloud.apply` 与 `evidence.drain` 仅 agent 内部使用，移动端不得调用（桥层也会拒绝）。

// MARK: - 取值集合

public enum BtGuardLayer {
    /// 过滤条目所在层（优先级 allow > manual > auto / cloud / file）。
    public static let all = ["allow", "manual", "auto", "cloud", "file"]
}

public enum BtGuardEntryAction {
    public static let all = ["allow", "ban", "choke", "throttle"]
}

public enum BtGuardRule {
    public static let all = ["excessive", "rerequest", "rewind", "lag", "fingerprint", "cloud"]
    /// 条目 `rule` 的全部取值：六条检测规则 + 手动封禁 `manual` + 过滤文件 `file`（均有本地化文案）。
    public static let entryValues = all + ["manual", "file"]
}

/// 单页上限 / 默认页大小 / 单次解除 id 上限（`BT_GUARD_LIST_MAX_LIMIT` / `_DEFAULT_LIMIT` / `BT_GUARD_REMOVE_MAX_IDS`）。
public enum BtGuardLimits {
    public static let maxPage = 500
    public static let defaultPage = 100
    public static let removeMaxIds = 1000
}

// MARK: - DTO

/// 今日（本地日）统计。
public struct BtGuardStats: Sendable, Hashable, Codable {
    public var todayObserved: UInt64
    public var todayEnforced: UInt64
    public var todayBlockedConnections: UInt64
    public var todayRefusedBytes: UInt64

    public init(
        todayObserved: UInt64 = 0, todayEnforced: UInt64 = 0, todayBlockedConnections: UInt64 = 0,
        todayRefusedBytes: UInt64 = 0
    ) {
        self.todayObserved = todayObserved
        self.todayEnforced = todayEnforced
        self.todayBlockedConnections = todayBlockedConnections
        self.todayRefusedBytes = todayRefusedBytes
    }

    private enum CodingKeys: String, CodingKey {
        case todayObserved, todayEnforced, todayBlockedConnections, todayRefusedBytes
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        todayObserved = try c.decodeIfPresent(UInt64.self, forKey: .todayObserved) ?? 0
        todayEnforced = try c.decodeIfPresent(UInt64.self, forKey: .todayEnforced) ?? 0
        todayBlockedConnections = try c.decodeIfPresent(UInt64.self, forKey: .todayBlockedConnections) ?? 0
        todayRefusedBytes = try c.decodeIfPresent(UInt64.self, forKey: .todayRefusedBytes) ?? 0
    }
}

/// 云名单状态（`BtGuardCloudStatusDto`）。
public struct BtGuardCloudStatus: Sendable, Hashable, Codable {
    /// 当前已应用文档序号；0 = 无。
    public var sequence: UInt64
    /// 文档应用时间（unix 秒）；0 = 无。
    public var updatedAt: Int64
    /// 文档过期时间（unix 秒）；0 = 无。
    public var expiresAt: Int64
    public var entries: UInt32
    public var clientRules: UInt32
    /// 文档已过期，云层停用。
    public var stale: Bool
    public var lastError: String?
    /// 最近一份被拒绝文档的原因；成功应用后清空。
    public var rejectedReason: String?
    /// 文档要求展示的署名行（CC BY 4.0 等）。
    public var attribution: [String]

    public init(
        sequence: UInt64 = 0, updatedAt: Int64 = 0, expiresAt: Int64 = 0, entries: UInt32 = 0,
        clientRules: UInt32 = 0, stale: Bool = false, lastError: String? = nil, rejectedReason: String? = nil,
        attribution: [String] = []
    ) {
        self.sequence = sequence
        self.updatedAt = updatedAt
        self.expiresAt = expiresAt
        self.entries = entries
        self.clientRules = clientRules
        self.stale = stale
        self.lastError = lastError
        self.rejectedReason = rejectedReason
        self.attribution = attribution
    }

    private enum CodingKeys: String, CodingKey {
        case sequence, updatedAt, expiresAt, entries, clientRules, stale, lastError, rejectedReason, attribution
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sequence = try c.decodeIfPresent(UInt64.self, forKey: .sequence) ?? 0
        updatedAt = try c.decodeIfPresent(Int64.self, forKey: .updatedAt) ?? 0
        expiresAt = try c.decodeIfPresent(Int64.self, forKey: .expiresAt) ?? 0
        entries = try c.decodeIfPresent(UInt32.self, forKey: .entries) ?? 0
        clientRules = try c.decodeIfPresent(UInt32.self, forKey: .clientRules) ?? 0
        stale = try c.decodeIfPresent(Bool.self, forKey: .stale) ?? false
        lastError = try c.decodeIfPresent(String.self, forKey: .lastError)
        rejectedReason = try c.decodeIfPresent(String.self, forKey: .rejectedReason)
        attribution = try c.decodeIfPresent([String].self, forKey: .attribution) ?? []
    }
}

/// 过滤文件订阅状态（`BtGuardFilterSourceDto`）。
public struct BtGuardFilterSource: Sendable, Hashable, Codable {
    public var url: String
    public var ok: Bool
    public var entries: UInt32
    /// 最近一次成功时间（unix 秒）；0 = 从未成功。
    public var updatedAt: Int64
    public var error: String?

    public init(url: String, ok: Bool = false, entries: UInt32 = 0, updatedAt: Int64 = 0, error: String? = nil) {
        self.url = url
        self.ok = ok
        self.entries = entries
        self.updatedAt = updatedAt
        self.error = error
    }

    private enum CodingKeys: String, CodingKey { case url, ok, entries, updatedAt, error }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        url = try c.decode(String.self, forKey: .url)
        ok = try c.decodeIfPresent(Bool.self, forKey: .ok) ?? false
        entries = try c.decodeIfPresent(UInt32.self, forKey: .entries) ?? 0
        updatedAt = try c.decodeIfPresent(Int64.self, forKey: .updatedAt) ?? 0
        error = try c.decodeIfPresent(String.self, forKey: .error)
    }
}

/// 各层未过期条目数（`BtGuardCountsDto`）。
public struct BtGuardCounts: Sendable, Hashable, Codable {
    public var allow: UInt32
    public var manual: UInt32
    public var auto: UInt32
    public var cloud: UInt32
    public var file: UInt32

    public init(allow: UInt32 = 0, manual: UInt32 = 0, auto: UInt32 = 0, cloud: UInt32 = 0, file: UInt32 = 0) {
        self.allow = allow
        self.manual = manual
        self.auto = auto
        self.cloud = cloud
        self.file = file
    }

    private enum CodingKeys: String, CodingKey { case allow, manual, auto, cloud, file }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        allow = try c.decodeIfPresent(UInt32.self, forKey: .allow) ?? 0
        manual = try c.decodeIfPresent(UInt32.self, forKey: .manual) ?? 0
        auto = try c.decodeIfPresent(UInt32.self, forKey: .auto) ?? 0
        cloud = try c.decodeIfPresent(UInt32.self, forKey: .cloud) ?? 0
        file = try c.decodeIfPresent(UInt32.self, forKey: .file) ?? 0
    }
}

/// `daemon.bt.guard.state` 结果（`BtGuardStateDto`）。
public struct BtGuardState: Sendable, Hashable, Codable {
    /// `bt_guard_enabled`。
    public var enabled: Bool
    /// BT 会话存活（防吸血仅在会话内生效）。
    public var sessionActive: Bool
    public var stats: BtGuardStats
    public var cloud: BtGuardCloudStatus
    public var filterSources: [BtGuardFilterSource]
    public var counts: BtGuardCounts

    public init(
        enabled: Bool = false, sessionActive: Bool = false, stats: BtGuardStats = BtGuardStats(),
        cloud: BtGuardCloudStatus = BtGuardCloudStatus(), filterSources: [BtGuardFilterSource] = [],
        counts: BtGuardCounts = BtGuardCounts()
    ) {
        self.enabled = enabled
        self.sessionActive = sessionActive
        self.stats = stats
        self.cloud = cloud
        self.filterSources = filterSources
        self.counts = counts
    }

    private enum CodingKeys: String, CodingKey { case enabled, sessionActive, stats, cloud, filterSources, counts }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        sessionActive = try c.decodeIfPresent(Bool.self, forKey: .sessionActive) ?? false
        stats = try c.decodeIfPresent(BtGuardStats.self, forKey: .stats) ?? BtGuardStats()
        cloud = try c.decodeIfPresent(BtGuardCloudStatus.self, forKey: .cloud) ?? BtGuardCloudStatus()
        filterSources = try c.decodeIfPresent([BtGuardFilterSource].self, forKey: .filterSources) ?? []
        counts = try c.decodeIfPresent(BtGuardCounts.self, forKey: .counts) ?? BtGuardCounts()
    }
}

/// 一条过滤条目（`BtGuardEntryDto`）。
public struct BtGuardEntry: Sendable, Hashable, Codable, Identifiable {
    public var id: Int64
    /// 见 ``BtGuardLayer``。
    public var layer: String
    /// `1.2.3.4` / `1.2.3.0/24` / `2001:db8::/64`。
    public var cidr: String
    /// 见 ``BtGuardEntryAction``。
    public var action: String
    /// 触发规则 id；手动 / 订阅条目为空。
    public var rule: String
    public var reason: String
    /// 证据摘要（JSON）。
    public var evidence: JSONValue?
    /// 来源（`auto` / `user` / 订阅 URL / `cloud:community` 等）。
    public var source: String
    public var createdAt: Int64
    /// 到期 unix 秒；缺省 = 永久。
    public var expiresAt: Int64?
    /// 距到期的剩余秒数；永久缺省。
    public var remainingSecs: Int64?

    public init(
        id: Int64, layer: String, cidr: String, action: String, rule: String = "", reason: String = "",
        evidence: JSONValue? = nil, source: String = "", createdAt: Int64 = 0, expiresAt: Int64? = nil,
        remainingSecs: Int64? = nil
    ) {
        self.id = id
        self.layer = layer
        self.cidr = cidr
        self.action = action
        self.rule = rule
        self.reason = reason
        self.evidence = evidence
        self.source = source
        self.createdAt = createdAt
        self.expiresAt = expiresAt
        self.remainingSecs = remainingSecs
    }

    private enum CodingKeys: String, CodingKey {
        case id, layer, cidr, action, rule, reason, evidence, source, createdAt, expiresAt, remainingSecs
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int64.self, forKey: .id)
        layer = try c.decode(String.self, forKey: .layer)
        cidr = try c.decode(String.self, forKey: .cidr)
        action = try c.decode(String.self, forKey: .action)
        rule = try c.decodeIfPresent(String.self, forKey: .rule) ?? ""
        reason = try c.decodeIfPresent(String.self, forKey: .reason) ?? ""
        evidence = try c.decodeIfPresent(JSONValue.self, forKey: .evidence)
        source = try c.decodeIfPresent(String.self, forKey: .source) ?? ""
        createdAt = try c.decodeIfPresent(Int64.self, forKey: .createdAt) ?? 0
        expiresAt = try c.decodeIfPresent(Int64.self, forKey: .expiresAt)
        remainingSecs = try c.decodeIfPresent(Int64.self, forKey: .remainingSecs)
    }
}

/// `daemon.bt.guard.entries.list` 参数（`BtGuardEntriesListParams`）。nil 字段不下发。
public struct BtGuardEntriesListParams: Sendable, Hashable, Codable {
    public var layer: String?
    public var rule: String?
    /// 对 cidr / reason 的子串搜索。
    public var query: String?
    public var offset: Int
    public var limit: Int

    public init(
        layer: String? = nil, rule: String? = nil, query: String? = nil, offset: Int = 0,
        limit: Int = BtGuardLimits.defaultPage
    ) {
        self.layer = layer
        self.rule = rule
        self.query = query
        self.offset = offset
        self.limit = limit
    }
}

/// `daemon.bt.guard.entries.list` 结果。
public struct BtGuardEntriesPage: Sendable, Hashable, Codable {
    /// 满足过滤条件的总数（不受分页影响）。
    public var total: UInt64
    public var items: [BtGuardEntry]

    public init(total: UInt64 = 0, items: [BtGuardEntry] = []) {
        self.total = total
        self.items = items
    }

    private enum CodingKeys: String, CodingKey { case total, items }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        total = try c.decodeIfPresent(UInt64.self, forKey: .total) ?? 0
        items = try c.decodeIfPresent([BtGuardEntry].self, forKey: .items) ?? []
    }
}

/// `daemon.bt.guard.entries.add` 参数。`action == "allow"` 进白名单层，其余进手动层。
public struct BtGuardEntriesAddParams: Sendable, Hashable, Codable {
    public var cidr: String
    public var action: String
    /// 有效期秒数；nil = 永久。
    public var durationSecs: Int64?
    public var reason: String

    public init(cidr: String, action: String, durationSecs: Int64? = nil, reason: String = "") {
        self.cidr = cidr
        self.action = action
        self.durationSecs = durationSecs
        self.reason = reason
    }
}

/// `daemon.bt.guard.entries.remove` 参数。云层条目 daemon 拒绝删除（`invalidArgument`），应改为加白。
public struct BtGuardEntriesRemoveParams: Sendable, Hashable, Codable {
    public var ids: [Int64]

    public init(ids: [Int64]) { self.ids = ids }
}

/// `daemon.bt.guard.entries.remove` 结果。
public struct BtGuardEntriesRemoveResult: Sendable, Hashable, Codable {
    public var removed: UInt32

    public init(removed: UInt32 = 0) { self.removed = removed }

    private enum CodingKeys: String, CodingKey { case removed }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        removed = try c.decodeIfPresent(UInt32.self, forKey: .removed) ?? 0
    }
}

/// `daemon.bt.guard.export` 参数（`layer` nil = 全部）与结果。
public struct BtGuardExportParams: Sendable, Hashable, Codable {
    public var layer: String?

    public init(layer: String? = nil) { self.layer = layer }
}

public struct BtGuardExportResult: Sendable, Hashable, Codable {
    /// 每行 `cidr # layer rule reason expires`。
    public var text: String
    public var lines: UInt32

    public init(text: String = "", lines: UInt32 = 0) {
        self.text = text
        self.lines = lines
    }

    private enum CodingKeys: String, CodingKey { case text, lines }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        lines = try c.decodeIfPresent(UInt32.self, forKey: .lines) ?? 0
    }
}

/// `daemon.bt.guard.filterSources.refresh` 结果。
public struct BtGuardFilterSourcesRefresh: Sendable, Hashable, Codable {
    public var sources: [BtGuardFilterSource]

    public init(sources: [BtGuardFilterSource] = []) { self.sources = sources }

    private enum CodingKeys: String, CodingKey { case sources }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sources = try c.decodeIfPresent([BtGuardFilterSource].self, forKey: .sources) ?? []
    }
}

// MARK: - 纯逻辑

public enum BtGuardRules {
    // MARK: CIDR 校验

    public enum CidrIssue: Sendable, Hashable { case empty, address, prefix }

    public enum CidrCheck: Sendable, Hashable {
        case valid(cidr: String, family: Int, prefix: Int?)
        case invalid(CidrIssue)
    }

    private static func isDecimalOctet(_ part: Substring) -> Bool {
        guard (1 ... 3).contains(part.count), part.allSatisfy({ $0.isASCII && $0.isNumber }) else { return false }
        if part.count > 1, part.hasPrefix("0") { return false }
        return Int(part).map { $0 <= 255 } ?? false
    }

    static func isIPv4(_ text: String) -> Bool {
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        return parts.count == 4 && parts.allSatisfy(isDecimalOctet)
    }

    private static func isHexGroup(_ group: Substring) -> Bool {
        (1 ... 4).contains(group.count) && group.allSatisfy { $0.isASCII && $0.isHexDigit }
    }

    static func isIPv6(_ text: String) -> Bool {
        guard text.contains(":"), !text.contains("%") else { return false }
        var body = text
        if text.contains(".") {
            guard let cut = text.lastIndex(of: ":") else { return false }
            guard isIPv4(String(text[text.index(after: cut)...])) else { return false }
            // 内嵌 IPv4 占两个 16 位组。
            body = String(text[...cut]) + "0:0"
        }
        let halves = body.components(separatedBy: "::")
        guard halves.count <= 2 else { return false }
        let groups = halves.map { $0.isEmpty ? [Substring]() : $0.split(separator: ":", omittingEmptySubsequences: false) }
        guard groups.allSatisfy({ $0.allSatisfy(isHexGroup) }) else { return false }
        let total = groups.reduce(0) { $0 + $1.count }
        return halves.count == 1 ? total == 8 : total <= 7
    }

    /// 校验 `1.2.3.4` / `1.2.3.0/24` / `2001:db8::/64`（裸地址等价于单地址）；返回去首尾空白后的原文。
    public static func parseCidr(_ input: String) -> CidrCheck {
        let text = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { return .invalid(.empty) }
        let address: String
        let prefixText: String?
        if let slash = text.firstIndex(of: "/") {
            address = String(text[..<slash])
            prefixText = String(text[text.index(after: slash)...])
        } else {
            address = text
            prefixText = nil
        }
        let family: Int
        if isIPv4(address) {
            family = 4
        } else if isIPv6(address) {
            family = 6
        } else {
            return .invalid(.address)
        }
        guard let prefixText else { return .valid(cidr: text, family: family, prefix: nil) }
        guard (1 ... 3).contains(prefixText.count), prefixText.allSatisfy({ $0.isASCII && $0.isNumber }),
              let prefix = Int(prefixText), prefix <= (family == 4 ? 32 : 128)
        else { return .invalid(.prefix) }
        return .valid(cidr: text, family: family, prefix: prefix)
    }

    /// 条目 cidr 与 peer 的 IP 组文本是否指同一组（IPv4 `/32`、IPv6 `/128` 等价于省略前缀；大小写不敏感）。
    public static func sameGroup(_ cidr: String, _ group: String) -> Bool {
        func norm(_ text: String) -> String {
            let value = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if value.hasSuffix("/32"), !value.contains(":") { return String(value.dropLast(3)) }
            if value.hasSuffix("/128") { return String(value.dropLast(4)) }
            return value
        }
        let left = norm(cidr)
        return !left.isEmpty && left == norm(group)
    }

    // MARK: 时长

    public enum RemainingUnit: String, Sendable, Hashable {
        case seconds = "S", minutes = "M", hours = "H", days = "D"

        /// 文案键 `btGuardDuration{D,H,M,S}`。
        public var textKey: String { "btGuardDuration" + rawValue }
    }

    public struct RemainingParts: Sendable, Hashable {
        public var unit: RemainingUnit
        public var n: Int64

        public init(unit: RemainingUnit, n: Int64) {
            self.unit = unit
            self.n = n
        }
    }

    /// 剩余秒数 → 最大整单位；永久（nil）→ nil；负值按 0。
    public static func remainingParts(_ secs: Int64?) -> RemainingParts? {
        guard let secs else { return nil }
        let s = max(0, secs)
        if s < 60 { return RemainingParts(unit: .seconds, n: s) }
        if s < 3600 { return RemainingParts(unit: .minutes, n: s / 60) }
        if s < 86_400 { return RemainingParts(unit: .hours, n: s / 3600) }
        return RemainingParts(unit: .days, n: s / 86_400)
    }

    /// 手动处置的时长预设（1h / 1d / 7d / 永久）。
    public enum DurationPreset: String, CaseIterable, Sendable, Hashable, Identifiable {
        case oneHour = "1h", oneDay = "1d", sevenDays = "7d", forever

        public var id: String { rawValue }

        /// `durationSecs`；永久为 nil（协议缺省 = 永久）。
        public var seconds: Int64? {
            switch self {
            case .oneHour: 3600
            case .oneDay: 86_400
            case .sevenDays: 604_800
            case .forever: nil
            }
        }
    }

    // MARK: 条目操作

    /// 云层条目禁止直接删除（daemon 返回 invalidArgument），只能加白覆盖。
    public static func canRemove(_ entry: BtGuardEntry) -> Bool { entry.layer != "cloud" }

    /// 白名单层本身无需再加白。
    public static func canWhitelist(_ entry: BtGuardEntry) -> Bool { entry.layer != "allow" }

    /// 选中项中可批量解除的 id（云层条目跳过），保持列表顺序。
    public static func removableIDs(_ entries: [BtGuardEntry], selected: Set<Int64>) -> [Int64] {
        entries.filter { selected.contains($0.id) && canRemove($0) }.map(\.id)
    }

    /// 批量解除按协议单次上限分块。
    public static func chunkIDs(_ ids: [Int64], size: Int = BtGuardLimits.removeMaxIds) -> [[Int64]] {
        guard size > 0 else { return [] }
        return stride(from: 0, to: ids.count, by: size).map { Array(ids[$0 ..< min($0 + size, ids.count)]) }
    }

    /// 加白参数（永久）。
    public static func allowParams(cidr: String) -> BtGuardEntriesAddParams {
        BtGuardEntriesAddParams(cidr: cidr, action: "allow", durationSecs: nil, reason: "")
    }

    /// 筛选 + 页码 → `entries.list` 参数（`nil` / 空白搜索不带字段）。
    public static func entriesQuery(
        layer: String?, rule: String? = nil, query: String = "", offset: Int, limit: Int = BtGuardLimits.defaultPage
    ) -> BtGuardEntriesListParams {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return BtGuardEntriesListParams(
            layer: layer, rule: rule, query: trimmed.isEmpty ? nil : trimmed,
            offset: max(0, offset), limit: min(max(1, limit), BtGuardLimits.maxPage)
        )
    }

    /// 合并分页结果：按 id 去重（先到先得），保持顺序。
    public static func mergePage(_ existing: [BtGuardEntry], _ page: [BtGuardEntry]) -> [BtGuardEntry] {
        var seen = Set(existing.map(\.id))
        var merged = existing
        for item in page where seen.insert(item.id).inserted { merged.append(item) }
        return merged
    }

    // MARK: 云名单 / 上报状态

    public enum CloudStatusKind: Sendable, Hashable { case disabled, none, stale, rejected, error, ok }

    /// 云名单状态卡的主状态（优先级：未启用 > 无文档 > 已过期 > 被拒 > 拉取失败 > 正常）。
    public static func cloudStatusKind(_ cloud: BtGuardCloudStatus, enabled: Bool) -> CloudStatusKind {
        func has(_ text: String?) -> Bool { !(text ?? "").isEmpty }
        if !enabled { return .disabled }
        if cloud.sequence == 0 {
            if has(cloud.rejectedReason) { return .rejected }
            return has(cloud.lastError) ? .error : .none
        }
        if cloud.stale { return .stale }
        if has(cloud.rejectedReason) { return .rejected }
        if has(cloud.lastError) { return .error }
        return .ok
    }

    /// 证据上报仅登录用户可用：开关可以开，但未登录时不会实际上报。
    public static func reportNeedsLogin(loggedIn: Bool) -> Bool { !loggedIn }

    /// 实际是否在上报：开关打开且已登录。
    public static func reportActive(enabled: Bool, loggedIn: Bool) -> Bool { enabled && loggedIn }

    /// 上报开关是否可被点击：已开启时始终可关；未开启时要求已登录。
    public static func reportToggleEnabled(enabled: Bool, loggedIn: Bool) -> Bool { enabled || loggedIn }

    /// 总览状态行文案键：关闭 > 载入中 > 会话运行 / 未运行。
    public static func statusKey(enabled: Bool, state: BtGuardState?) -> String {
        if !enabled { return "btGuardStatusDisabled" }
        guard let state else { return "btGuardLoading" }
        return state.sessionActive ? "btGuardSessionOn" : "btGuardSessionOff"
    }
}

extension BtGuardRules {
    /// 列表变化后把选中集合收敛到仍在列表中的 id。
    public static func pruneSelection(_ selected: Set<Int64>, items: [BtGuardEntry]) -> Set<Int64> {
        selected.intersection(items.map(\.id))
    }

    /// 条目的多行摘要里「原因 · 来源」一行：两者都空为 nil；去掉首尾空白，保留内部空白。
    public static func entryNote(_ entry: BtGuardEntry) -> String? {
        let parts = [entry.reason, entry.source]
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

extension BtGuardRules {
    /// 取值 → 文案键：`prefix` + 首字母大写的取值（`btGuardLayer` + `allow` → `btGuardLayerAllow`）。
    /// 空值返回 nil；取值未必有对应文案（主机新增的值），调用方用 `L10n.shared.has` 判断并回退到原文。
    public static func labelKey(prefix: String, value: String) -> String? {
        guard let first = value.first else { return nil }
        return prefix + first.uppercased() + value.dropFirst()
    }

    /// 云名单状态卡的问题行文案键；`ok` 无问题行。
    public static func cloudProblemKey(_ kind: CloudStatusKind) -> String? {
        switch kind {
        case .disabled: "btGuardCloudDisabled"
        case .none: "btGuardCloudNone"
        case .stale: "btGuardCloudStale"
        case .rejected: "btGuardCloudRejected"
        case .error: "btGuardCloudLastError"
        case .ok: nil
        }
    }

    /// 问题行是否按警示语气显示（未启用 / 暂无文档只是中性说明）。
    public static func cloudProblemIsAlert(_ kind: CloudStatusKind) -> Bool {
        switch kind {
        case .stale, .rejected, .error: true
        case .disabled, .none, .ok: false
        }
    }
}
