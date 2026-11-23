import Foundation

// `daemon.rss.*` 的 wire 类型（镜像 `native/protocol/src/daemon.rs` 的 `RssSourceDto` / `RssItemDto` /
// `RssItemActionRequest` / `RssValidate*`，与 `web/src/lib/rpc/protocol/{rss,params}.ts` 交叉核对）。
//
// 命名：`RssSourceDto` 与 UniFFI 生成的同名 UI 子集类型冲突（FluxBridge 同时可见两者），
// 因此完整订阅用 `RssSourceDetail`；`RssSource`（Models.swift）是快照里的 UI 子集。

// MARK: - 订阅

/// 完整订阅（`RssSourceDto`）。`daemon.rss.updateSource` 需要**整个**订阅 + `sourceId`：
/// 先 `listSources` 取最新副本、只改要改的字段再整体写回（只读运行态字段引擎忽略）。
public struct RssSourceDetail: Codable, Sendable, Hashable, Identifiable {
    public static let builtinProviderId = "rss"
    /// `intervalMinutes == 0` 时引擎的默认抓取间隔。
    public static let defaultIntervalMinutes: Int32 = 30
    /// `maxPerFetch == 0` 时引擎的默认每轮新建任务数。
    public static let defaultMaxPerFetch: Int32 = 20
    public static let maxPerFetchRange: ClosedRange<Int32> = 1...100

    public var sourceId: String
    public var providerId: String
    public var providerConfig: String
    public var url: String
    public var name: String
    public var enabled: Bool
    public var autoDownload: Bool
    public var startPaused: Bool
    /// 空 = 内置主队列。
    public var queueId: String
    /// 空 = 队列目录 → 全局目录。
    public var saveDir: String
    /// 0 = 引擎默认 30。
    public var intervalMinutes: Int32
    public var includePattern: String
    public var excludePattern: String
    public var useRegex: Bool
    public var smartEpisode: Bool
    /// 字节，0 = 不限。
    public var sizeMinBytes: Int64
    public var sizeMaxBytes: Int64
    public var sendReferer: Bool
    public var notifyOnDownload: Bool
    /// 1...100；0 = 引擎默认 20。
    public var maxPerFetch: Int32
    public var cookies: String
    public var userAgent: String
    public var proxyUrl: String
    // 只读运行态
    public var lastFetchAt: Int64
    public var lastSuccessAt: Int64
    public var lastError: String
    public var failCount: Int32
    public var seeded: Bool
    public var position: Int32
    public var unreadCount: Int32

    public var id: String { sourceId }

    public init(
        sourceId: String = "",
        providerId: String = RssSourceDetail.builtinProviderId,
        providerConfig: String = "",
        url: String,
        name: String = "",
        enabled: Bool = true,
        autoDownload: Bool = true,
        startPaused: Bool = false,
        queueId: String = "",
        saveDir: String = "",
        intervalMinutes: Int32 = 0,
        includePattern: String = "",
        excludePattern: String = "",
        useRegex: Bool = false,
        smartEpisode: Bool = false,
        sizeMinBytes: Int64 = 0,
        sizeMaxBytes: Int64 = 0,
        sendReferer: Bool = true,
        notifyOnDownload: Bool = true,
        maxPerFetch: Int32 = 0,
        cookies: String = "",
        userAgent: String = "",
        proxyUrl: String = "",
        lastFetchAt: Int64 = 0,
        lastSuccessAt: Int64 = 0,
        lastError: String = "",
        failCount: Int32 = 0,
        seeded: Bool = false,
        position: Int32 = 0,
        unreadCount: Int32 = 0
    ) {
        self.sourceId = sourceId
        self.providerId = providerId
        self.providerConfig = providerConfig
        self.url = url
        self.name = name
        self.enabled = enabled
        self.autoDownload = autoDownload
        self.startPaused = startPaused
        self.queueId = queueId
        self.saveDir = saveDir
        self.intervalMinutes = intervalMinutes
        self.includePattern = includePattern
        self.excludePattern = excludePattern
        self.useRegex = useRegex
        self.smartEpisode = smartEpisode
        self.sizeMinBytes = sizeMinBytes
        self.sizeMaxBytes = sizeMaxBytes
        self.sendReferer = sendReferer
        self.notifyOnDownload = notifyOnDownload
        self.maxPerFetch = maxPerFetch
        self.cookies = cookies
        self.userAgent = userAgent
        self.proxyUrl = proxyUrl
        self.lastFetchAt = lastFetchAt
        self.lastSuccessAt = lastSuccessAt
        self.lastError = lastError
        self.failCount = failCount
        self.seeded = seeded
        self.position = position
        self.unreadCount = unreadCount
    }

    /// 全字段 `#[serde(default)]`（`url` 必填）：旧主机缺字段按 Rust 默认值处理。
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            sourceId: try c.decodeIfPresent(String.self, forKey: .sourceId) ?? "",
            providerId: try c.decodeIfPresent(String.self, forKey: .providerId) ?? Self.builtinProviderId,
            providerConfig: try c.decodeIfPresent(String.self, forKey: .providerConfig) ?? "",
            url: try c.decode(String.self, forKey: .url),
            name: try c.decodeIfPresent(String.self, forKey: .name) ?? "",
            enabled: try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? true,
            autoDownload: try c.decodeIfPresent(Bool.self, forKey: .autoDownload) ?? true,
            startPaused: try c.decodeIfPresent(Bool.self, forKey: .startPaused) ?? false,
            queueId: try c.decodeIfPresent(String.self, forKey: .queueId) ?? "",
            saveDir: try c.decodeIfPresent(String.self, forKey: .saveDir) ?? "",
            intervalMinutes: try c.decodeIfPresent(Int32.self, forKey: .intervalMinutes) ?? 0,
            includePattern: try c.decodeIfPresent(String.self, forKey: .includePattern) ?? "",
            excludePattern: try c.decodeIfPresent(String.self, forKey: .excludePattern) ?? "",
            useRegex: try c.decodeIfPresent(Bool.self, forKey: .useRegex) ?? false,
            smartEpisode: try c.decodeIfPresent(Bool.self, forKey: .smartEpisode) ?? false,
            sizeMinBytes: try c.decodeIfPresent(Int64.self, forKey: .sizeMinBytes) ?? 0,
            sizeMaxBytes: try c.decodeIfPresent(Int64.self, forKey: .sizeMaxBytes) ?? 0,
            sendReferer: try c.decodeIfPresent(Bool.self, forKey: .sendReferer) ?? true,
            notifyOnDownload: try c.decodeIfPresent(Bool.self, forKey: .notifyOnDownload) ?? true,
            maxPerFetch: try c.decodeIfPresent(Int32.self, forKey: .maxPerFetch) ?? 0,
            cookies: try c.decodeIfPresent(String.self, forKey: .cookies) ?? "",
            userAgent: try c.decodeIfPresent(String.self, forKey: .userAgent) ?? "",
            proxyUrl: try c.decodeIfPresent(String.self, forKey: .proxyUrl) ?? "",
            lastFetchAt: try c.decodeIfPresent(Int64.self, forKey: .lastFetchAt) ?? 0,
            lastSuccessAt: try c.decodeIfPresent(Int64.self, forKey: .lastSuccessAt) ?? 0,
            lastError: try c.decodeIfPresent(String.self, forKey: .lastError) ?? "",
            failCount: try c.decodeIfPresent(Int32.self, forKey: .failCount) ?? 0,
            seeded: try c.decodeIfPresent(Bool.self, forKey: .seeded) ?? false,
            position: try c.decodeIfPresent(Int32.self, forKey: .position) ?? 0,
            unreadCount: try c.decodeIfPresent(Int32.self, forKey: .unreadCount) ?? 0
        )
    }

    /// 有效抓取间隔（分钟）：0 → 引擎默认。
    public var effectiveIntervalMinutes: Int32 {
        intervalMinutes > 0 ? intervalMinutes : Self.defaultIntervalMinutes
    }

    /// 有效每轮上限：0 → 引擎默认。
    public var effectiveMaxPerFetch: Int32 {
        maxPerFetch > 0 ? maxPerFetch : Self.defaultMaxPerFetch
    }
}

// MARK: - 条目

/// `RssItemDto.status`：0 新 / 1 已下载 / 2 已忽略 / 3 规则未命中 / 4 重复剧集 / 5 首轮历史条目。
public enum RssItemStatus: Sendable, Hashable {
    case new, downloaded, ignored, filtered, duplicateEpisode, seeded
    case unknown(Int32)

    public init(wire: Int32) {
        switch wire {
        case 0: self = .new
        case 1: self = .downloaded
        case 2: self = .ignored
        case 3: self = .filtered
        case 4: self = .duplicateEpisode
        case 5: self = .seeded
        default: self = .unknown(wire)
        }
    }
}

/// 条目上的稳定原因码（引擎只产出码，文案由客户端本地化）。
/// `seed_skipped`、空码与未知码都没有对应文案（`i18nKey == nil`）。
public enum RssReason: WireStringEnum {
    case notIncluded, excluded, tooSmall, tooLarge, dupEpisode, torrentFetchFailed
    case seedSkipped
    case none
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "": self = .none
        case "not_included": self = .notIncluded
        case "excluded": self = .excluded
        case "too_small": self = .tooSmall
        case "too_large": self = .tooLarge
        case "dup_episode": self = .dupEpisode
        case "torrent_fetch_failed": self = .torrentFetchFailed
        case "seed_skipped": self = .seedSkipped
        default: self = .unknown(wire)
        }
    }

    public var wire: String {
        switch self {
        case .none: ""
        case .notIncluded: "not_included"
        case .excluded: "excluded"
        case .tooSmall: "too_small"
        case .tooLarge: "too_large"
        case .dupEpisode: "dup_episode"
        case .torrentFetchFailed: "torrent_fetch_failed"
        case .seedSkipped: "seed_skipped"
        case let .unknown(raw): raw
        }
    }

    /// 文案键；无文案（空 / `seed_skipped` / 未知码）为 nil。
    public var i18nKey: String? {
        switch self {
        case .notIncluded: "rssReasonNotIncluded"
        case .excluded: "rssReasonExcluded"
        case .tooSmall: "rssReasonTooSmall"
        case .tooLarge: "rssReasonTooLarge"
        case .dupEpisode: "rssReasonDupEpisode"
        case .torrentFetchFailed: "rssReasonTorrentFetchFailed"
        case .seedSkipped, .none, .unknown: nil
        }
    }
}

/// 订阅流中的一个条目（`RssItemDto`）。
public struct RssItemDto: Codable, Sendable, Hashable, Identifiable {
    public var sourceId: String
    /// 去重主键。
    public var guid: String
    public var title: String
    public var link: String
    /// enclosure 直链（空 = 回退 `link`）。
    public var enclosureUrl: String
    /// enclosure 声明大小（字节，0 = 未知）。
    public var enclosureLength: Int64
    /// 发布时间（Unix 秒，0 = 未知）。
    public var pubDate: Int64
    public var fetchedAt: Int64
    public var status: Int32
    /// `status == 1` 时回链的任务 ID。
    public var taskId: String
    /// 智能剧集归一键（空 = 未识别）。
    public var episodeKey: String
    public var reason: String
    public var imageCount: Int

    public var id: String { sourceId + "\u{0}" + guid }

    public init(
        sourceId: String = "",
        guid: String,
        title: String = "",
        link: String = "",
        enclosureUrl: String = "",
        enclosureLength: Int64 = 0,
        pubDate: Int64 = 0,
        fetchedAt: Int64 = 0,
        status: Int32 = 0,
        taskId: String = "",
        episodeKey: String = "",
        reason: String = "",
        imageCount: Int = 0
    ) {
        self.sourceId = sourceId
        self.guid = guid
        self.title = title
        self.link = link
        self.enclosureUrl = enclosureUrl
        self.enclosureLength = enclosureLength
        self.pubDate = pubDate
        self.fetchedAt = fetchedAt
        self.status = status
        self.taskId = taskId
        self.episodeKey = episodeKey
        self.reason = reason
        self.imageCount = imageCount
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            sourceId: try c.decodeIfPresent(String.self, forKey: .sourceId) ?? "",
            guid: try c.decode(String.self, forKey: .guid),
            title: try c.decodeIfPresent(String.self, forKey: .title) ?? "",
            link: try c.decodeIfPresent(String.self, forKey: .link) ?? "",
            enclosureUrl: try c.decodeIfPresent(String.self, forKey: .enclosureUrl) ?? "",
            enclosureLength: try c.decodeIfPresent(Int64.self, forKey: .enclosureLength) ?? 0,
            pubDate: try c.decodeIfPresent(Int64.self, forKey: .pubDate) ?? 0,
            fetchedAt: try c.decodeIfPresent(Int64.self, forKey: .fetchedAt) ?? 0,
            status: try c.decodeIfPresent(Int32.self, forKey: .status) ?? 0,
            taskId: try c.decodeIfPresent(String.self, forKey: .taskId) ?? "",
            episodeKey: try c.decodeIfPresent(String.self, forKey: .episodeKey) ?? "",
            reason: try c.decodeIfPresent(String.self, forKey: .reason) ?? "",
            imageCount: try c.decodeIfPresent(Int.self, forKey: .imageCount) ?? 0
        )
    }

    public var state: RssItemStatus { RssItemStatus(wire: status) }
    public var reasonCode: RssReason { RssReason(wire: reason) }

    /// 打开 / 复制用的链接：enclosure 优先，空则回退 `link`。
    public var effectiveLink: String { enclosureUrl.isEmpty ? link : enclosureUrl }
}

// MARK: - 方法参数 / 结果

/// `RssSourceIdParams`：`getItems` / `deleteSource` / `refreshSource`。
public struct RssSourceIdParams: Codable, Sendable, Hashable {
    public var sourceId: String

    public init(sourceId: String) {
        self.sourceId = sourceId
    }
}

/// `itemAction` 的动作：`download`（绕过规则强制下载，任何状态都允许重新下载）/ `ignore` / `readAll`。
public enum RssItemAction: WireStringEnum {
    case download, ignore, readAll
    case unknown(String)

    public init(wire: String) {
        switch wire {
        case "download": self = .download
        case "ignore": self = .ignore
        case "readAll": self = .readAll
        default: self = .unknown(wire)
        }
    }

    public var wire: String {
        switch self {
        case .download: "download"
        case .ignore: "ignore"
        case .readAll: "readAll"
        case let .unknown(raw): raw
        }
    }
}

/// `daemon.rss.itemAction` 参数。`readAll` 忽略 `guid`（省略键）。
public struct RssItemActionParams: Codable, Sendable, Hashable {
    public var sourceId: String
    public var guid: String?
    public var action: RssItemAction

    public init(sourceId: String, guid: String? = nil, action: RssItemAction) {
        self.sourceId = sourceId
        self.guid = guid
        self.action = action
    }
}

/// `daemon.rss.createSource` 结果。
public struct RssCreateResult: Codable, Sendable, Hashable {
    public var sourceId: String

    public init(sourceId: String) {
        self.sourceId = sourceId
    }
}

/// `daemon.rss.validate` 参数（只读、不落库；慢方法）。
public struct RssValidateRequest: Codable, Sendable, Hashable {
    public var url: String
    public var cookies: String
    public var userAgent: String
    public var proxyUrl: String

    public init(url: String, cookies: String = "", userAgent: String = "", proxyUrl: String = "") {
        self.url = url
        self.cookies = cookies
        self.userAgent = userAgent
        self.proxyUrl = proxyUrl
    }
}

/// `daemon.rss.validate` 结果。`error` 非空即验证失败——这是诊断载荷，不是传输错误。
public struct RssValidateResponse: Codable, Sendable, Hashable {
    public var url: String
    /// feed 标题（供回填订阅名）。
    public var feedTitle: String
    /// 最近条目预览。
    public var items: [RssItemDto]
    public var error: String

    public init(url: String = "", feedTitle: String = "", items: [RssItemDto] = [], error: String = "") {
        self.url = url
        self.feedTitle = feedTitle
        self.items = items
        self.error = error
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            url: try c.decodeIfPresent(String.self, forKey: .url) ?? "",
            feedTitle: try c.decodeIfPresent(String.self, forKey: .feedTitle) ?? "",
            items: try c.decodeIfPresent([RssItemDto].self, forKey: .items) ?? [],
            error: try c.decodeIfPresent(String.self, forKey: .error) ?? ""
        )
    }
}

/// 一次性通知 `rssItemsChanged`（`WsServerMsg::RssItemsChanged`）：某订阅的条目流快照（新 → 旧）。
/// `notifyTitles` = 本轮自动建任务的条目标题（无自动下载为空）。
public struct RssItemsChangedNotice: Codable, Sendable, Hashable {
    public var sourceId: String
    public var items: [RssItemDto]
    public var notifyTitles: [String]

    public init(sourceId: String, items: [RssItemDto] = [], notifyTitles: [String] = []) {
        self.sourceId = sourceId
        self.items = items
        self.notifyTitles = notifyTitles
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            sourceId: try c.decode(String.self, forKey: .sourceId),
            items: try c.decodeIfPresent([RssItemDto].self, forKey: .items) ?? [],
            notifyTitles: try c.decodeIfPresent([String].self, forKey: .notifyTitles) ?? []
        )
    }
}

// MARK: - 目录浏览（`daemon.fs.list`，订阅保存目录选择器）

/// `FsListParams`：空 / 省略 = 默认保存目录。
public struct RssDirListParams: Codable, Sendable, Hashable {
    public var path: String?

    public init(path: String? = nil) {
        self.path = path
    }
}

/// `FsEntry`。
public struct RssDirEntry: Codable, Sendable, Hashable, Identifiable {
    public var name: String
    public var path: String

    public var id: String { path }

    public init(name: String, path: String) {
        self.name = name
        self.path = path
    }
}

/// `FsListResponse`：仅子目录；`denied` = 服务进程无读取权限（语义是「看不到」而不是「没有」）。
public struct RssDirListing: Codable, Sendable, Hashable {
    public var path: String
    public var parent: String?
    public var dirs: [RssDirEntry]
    public var denied: Bool

    public init(path: String, parent: String? = nil, dirs: [RssDirEntry] = [], denied: Bool = false) {
        self.path = path
        self.parent = parent
        self.dirs = dirs
        self.denied = denied
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            path: try c.decode(String.self, forKey: .path),
            parent: try c.decodeIfPresent(String.self, forKey: .parent),
            dirs: try c.decodeIfPresent([RssDirEntry].self, forKey: .dirs) ?? [],
            denied: try c.decodeIfPresent(Bool.self, forKey: .denied) ?? false
        )
    }
}

// MARK: - 体积字面量（编辑器输入框 ↔ 字节数；不是过滤规则求值）

/// `200M` / `2G` / `1.5 GB` / `1024`（1024 进制，可带小数与 `B` 后缀）↔ 字节数，
/// 与 Web `filter.ts::parseSize/formatSize` 同规则。过滤规则本身由主机引擎求值，客户端不复制。
public enum RssSizeLiteral {
    /// 空串 / 无法解析 / 越界 → nil。
    public static func parse(_ input: String) -> Int64? {
        var text = Substring(input.trimmingCharacters(in: .whitespacesAndNewlines).lowercased())
        guard !text.isEmpty else { return nil }
        if text.last == "b" { text = text.dropLast() }
        var multiplier = 1.0
        if let last = text.last, let scale = scales[last] {
            multiplier = scale
            text = text.dropLast()
        }
        let number = text.trimmingCharacters(in: .whitespaces)
        guard isDecimal(number), let value = Double(number) else { return nil }
        let bytes = value * multiplier
        let limit = 9_223_372_036_854_775_808.0 // 2^63
        guard bytes.isFinite, bytes >= 0, bytes <= limit else { return nil }
        return bytes >= limit ? Int64.max : Int64(bytes)
    }

    /// 字节数 → 字面量（≤ 0 → 空串 = 不限）；与 `parse` 往返。
    public static func format(_ bytes: Int64) -> String {
        guard bytes > 0 else { return "" }
        for (scale, suffix) in [(Int64(1) << 40, "T"), (Int64(1) << 30, "G"), (Int64(1) << 20, "M"), (Int64(1) << 10, "K")]
            where bytes % scale == 0 {
            return "\(bytes / scale)\(suffix)"
        }
        return String(bytes)
    }

    private static let scales: [Character: Double] = [
        "k": 1024, "m": 1024 * 1024, "g": 1024 * 1024 * 1024, "t": 1024 * 1024 * 1024 * 1024,
    ]

    /// `[0-9]+(\.[0-9]+)?`
    private static func isDecimal(_ text: String) -> Bool {
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 1 || parts.count == 2 else { return false }
        return parts.allSatisfy { !$0.isEmpty && $0.allSatisfy { $0 >= "0" && $0 <= "9" } }
    }
}
