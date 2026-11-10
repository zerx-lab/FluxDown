import Foundation

// 任务组与清单预解析 wire DTO（镜像 `native/protocol/src/daemon.rs` 的 Group* / ResolvePreview* /
// `agent.rs` 的 Capture{Preview,CreateGroup}Params 与 `web/src/lib/rpc/protocol/queue.ts`）+
// 组详情（D6）聚合与清单选择（N5）的纯逻辑。

// MARK: - Wire

/// `daemon.group.{pause,resume}`。
public struct GroupIdParams: Codable, Sendable, Hashable {
    public var groupId: String

    public init(groupId: String) { self.groupId = groupId }
}

/// `daemon.group.delete`。
public struct GroupDeleteParams: Codable, Sendable, Hashable {
    public var groupId: String
    public var deleteFiles: Bool

    public init(groupId: String, deleteFiles: Bool = false) {
        self.groupId = groupId
        self.deleteFiles = deleteFiles
    }
}

/// `daemon.group.resolvePreview`（慢方法，只读）。
public struct ResolvePreviewRequest: Codable, Sendable, Hashable {
    public var url: String
    public var cookies: String
    public var referrer: String
    public var userAgent: String
    public var extraHeaders: [String: String]

    public init(url: String, cookies: String = "", referrer: String = "", userAgent: String = "", extraHeaders: [String: String] = [:]) {
        self.url = url
        self.cookies = cookies
        self.referrer = referrer
        self.userAgent = userAgent
        self.extraHeaders = extraHeaders
    }
}

/// 清单条目的单个规格（画质 / 格式）。
public struct PreviewVariantDto: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var label: String
    /// 字节，0 = 未知。
    public var size: Int64

    public init(id: String, label: String, size: Int64 = 0) {
        self.id = id
        self.label = label
        self.size = size
    }

    private enum CodingKeys: String, CodingKey { case id, label, size }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? id
        size = try c.decodeIfPresent(Int64.self, forKey: .size) ?? 0
    }
}

/// 清单条目。建组时 `resolverItem` = `<id>` 或 `<id>@<variantId>`。
public struct PreviewItemDto: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var name: String
    /// 相对组根目录的子路径（空 = 根）。
    public var path: String
    /// 字节，0 = 未知。
    public var size: Int64
    public var variants: [PreviewVariantDto]

    public init(id: String, name: String, path: String = "", size: Int64 = 0, variants: [PreviewVariantDto] = []) {
        self.id = id
        self.name = name
        self.path = path
        self.size = size
        self.variants = variants
    }

    private enum CodingKeys: String, CodingKey { case id, name, path, size, variants }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? id
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        size = try c.decodeIfPresent(Int64.self, forKey: .size) ?? 0
        variants = try c.decodeIfPresent([PreviewVariantDto].self, forKey: .variants) ?? []
    }
}


/// Host preference-derived category routing supplied to group creation by Rust.
public struct CategoryRouteDto: Codable, Sendable, Hashable {
    public var rules: [CategoryRuleDto]
    public var otherDir: String

    public init(rules: [CategoryRuleDto] = [], otherDir: String = "") {
        self.rules = rules
        self.otherDir = otherDir
    }
}

public struct CategoryRuleDto: Codable, Sendable, Hashable {
    public var matchMode: String
    public var extensions: [String]
    public var regexPattern: String
    public var urlPattern: String
    public var saveDir: String

    public init(matchMode: String = "extension", extensions: [String] = [], regexPattern: String = "", urlPattern: String = "", saveDir: String = "") {
        self.matchMode = matchMode
        self.extensions = extensions
        self.regexPattern = regexPattern
        self.urlPattern = urlPattern
        self.saveDir = saveDir
    }
}
/// Root source metadata shared by every manifest item.
public struct ArchiveMetadataDto: Codable, Sendable, Hashable {
    public var platform: String
    public var mediaKind: String
    public var creatorId: String
    public var creatorName: String
    public var seriesId: String
    public var seriesTitle: String
    public var collectionId: String
    public var collectionTitle: String

    public init(
        platform: String = "", mediaKind: String = "", creatorId: String = "", creatorName: String = "",
        seriesId: String = "", seriesTitle: String = "", collectionId: String = "", collectionTitle: String = ""
    ) {
        self.platform = platform
        self.mediaKind = mediaKind
        self.creatorId = creatorId
        self.creatorName = creatorName
        self.seriesId = seriesId
        self.seriesTitle = seriesTitle
        self.collectionId = collectionId
        self.collectionTitle = collectionTitle
    }
}

/// `items` 与 `error` 均为空 = 插件未返回清单（回退普通建任务）；`error` 非空 = 预解析失败（同样回退，`error` 供提示）。
public struct ResolvePreviewResponse: Codable, Sendable, Hashable {
    public var name: String
    public var sourceUrl: String
    public var error: String
    public var items: [PreviewItemDto]
    /// 产出清单的插件 id；无清单时为空。建组时回填 `CreateGroupRequest.resolverPluginId`。
    public var pluginId: String
    public var archiveMetadata: ArchiveMetadataDto?

    public init(name: String = "", sourceUrl: String = "", error: String = "", items: [PreviewItemDto] = [], pluginId: String = "", archiveMetadata: ArchiveMetadataDto? = nil) {
        self.name = name
        self.sourceUrl = sourceUrl
        self.error = error
        self.items = items
        self.pluginId = pluginId
        self.archiveMetadata = archiveMetadata
    }

    private enum CodingKeys: String, CodingKey { case name, sourceUrl, error, items, pluginId, archiveMetadata }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        sourceUrl = try c.decodeIfPresent(String.self, forKey: .sourceUrl) ?? ""
        error = try c.decodeIfPresent(String.self, forKey: .error) ?? ""
        items = try c.decodeIfPresent([PreviewItemDto].self, forKey: .items) ?? []
        pluginId = try c.decodeIfPresent(String.self, forKey: .pluginId) ?? ""
        archiveMetadata = try c.decodeIfPresent(ArchiveMetadataDto.self, forKey: .archiveMetadata)
    }

    /// 命中清单：无错误且有条目。
    public var hasManifest: Bool { error.isEmpty && !items.isEmpty }
}

/// 组成员条目。
public struct GroupItemRequest: Codable, Sendable, Hashable {
    public var resolverItem: String
    public var fileName: String
    public var relPath: String
    public var size: Int64

    public init(resolverItem: String, fileName: String, relPath: String = "", size: Int64 = 0) {
        self.resolverItem = resolverItem
        self.fileName = fileName
        self.relPath = relPath
        self.size = size
    }
}

/// `daemon.group.create`；`items` 不可为空。
public struct CreateGroupRequest: Codable, Sendable, Hashable {
    public var sourceUrl: String
    /// 空 = 组根目录直接用 `saveDir`。
    public var groupName: String
    public var saveDir: String
    public var queueId: String
    public var segments: Int32
    public var cookies: String
    public var referrer: String
    public var userAgent: String
    public var proxyUrl: String
    public var extraHeaders: [String: String]
    public var ignoreTlsErrors: Bool
    public var startPaused: Bool
    public var items: [GroupItemRequest]
    /// 预览清单的产出插件（`ResolvePreviewResponse.pluginId`）；空 = 引擎按预览同规则自选。
    public var resolverPluginId: String
    /// Routing is populated by the mobile HostSession from current host preferences.
    public var categoryRoute: CategoryRouteDto?
    /// Root source metadata shared by all group members.
    public var archiveMetadata: ArchiveMetadataDto?

    public init(
        sourceUrl: String = "",
        groupName: String = "",
        saveDir: String = "",
        queueId: String = "",
        segments: Int32 = 0,
        cookies: String = "",
        referrer: String = "",
        userAgent: String = "",
        proxyUrl: String = "",
        extraHeaders: [String: String] = [:],
        ignoreTlsErrors: Bool = false,
        startPaused: Bool = false,
        items: [GroupItemRequest],
        resolverPluginId: String = "",
        archiveMetadata: ArchiveMetadataDto? = nil,
        categoryRoute: CategoryRouteDto? = nil
    ) {
        self.sourceUrl = sourceUrl
        self.groupName = groupName
        self.saveDir = saveDir
        self.queueId = queueId
        self.segments = segments
        self.cookies = cookies
        self.referrer = referrer
        self.userAgent = userAgent
        self.proxyUrl = proxyUrl
        self.extraHeaders = extraHeaders
        self.ignoreTlsErrors = ignoreTlsErrors
        self.startPaused = startPaused
        self.resolverPluginId = resolverPluginId
        self.items = items
        self.categoryRoute = categoryRoute
        self.archiveMetadata = archiveMetadata
    }
}

public struct CreateGroupResponse: Codable, Sendable, Hashable {
    public var groupId: String

    public init(groupId: String) { self.groupId = groupId }
}

/// `agent.capture.preview`（90 s；不消费事务）：表单上下文，捕获原 URL / method / body 不可被覆盖。
public struct CapturePreviewParams: Codable, Sendable, Hashable {
    public var transactionId: String
    public var request: TaskCreateWire

    public init(transactionId: String, request: TaskCreateWire) {
        self.transactionId = transactionId
        self.request = request
    }
}

/// `agent.capture.createGroup`：成功后消费事务；`sourceUrl` 恒取捕获原 URL。
public struct CaptureCreateGroupParams: Codable, Sendable, Hashable {
    public var transactionId: String
    public var request: CreateGroupRequest

    public init(transactionId: String, request: CreateGroupRequest) {
        self.transactionId = transactionId
        self.request = request
    }
}

// MARK: - 组聚合（D6）

/// 组成员聚合：概览卡（完成数 / 总数 · 百分比）、状态计数与底部工具栏可用性。
public struct GroupSummary: Sendable, Hashable {
    public var total = 0
    public var completed = 0
    public var failed = 0
    public var downloading = 0
    public var paused = 0
    public var pending = 0
    public var downloadedBytes: Int64 = 0
    /// 仅统计总大小已知（> 0）的成员。
    public var knownTotalBytes: Int64 = 0
    /// 总大小未知的成员数。
    public var unknownSizeCount = 0
    public var failedIds: [String] = []
    public var pausedIds: [String] = []
    /// 下载中 / 准备中 / 排队中的成员（可“全部暂停”）。
    public var runningIds: [String] = []

    public init(members: [DownloadTask]) {
        for task in members {
            total += 1
            downloadedBytes += max(task.downloadedBytes, 0)
            if task.totalBytes > 0 { knownTotalBytes += task.totalBytes } else { unknownSizeCount += 1 }
            switch task.status {
            case .completed: completed += 1
            case .failed:
                failed += 1
                failedIds.append(task.taskId)
            case .paused:
                paused += 1
                pausedIds.append(task.taskId)
            case .downloading, .preparing:
                downloading += 1
                runningIds.append(task.taskId)
            case .pending:
                pending += 1
                runningIds.append(task.taskId)
            case .unknown: break
            }
        }
    }

    /// 0…1：全部成员大小已知时按字节，否则按完成数；无成员为 nil。
    public var progress: Double? {
        guard total > 0 else { return nil }
        if unknownSizeCount == 0, knownTotalBytes > 0 {
            return min(max(Double(downloadedBytes) / Double(knownTotalBytes), 0), 1)
        }
        return Double(completed) / Double(total)
    }

    public var canPauseAll: Bool { !runningIds.isEmpty }
    public var canResumeAll: Bool { paused > 0 || failed > 0 }
    public var canRetryFailed: Bool { failed > 0 }
    public var isFinished: Bool { total > 0 && completed == total }

    /// 组成员（`groupId` 相同），保持快照顺序。
    public static func members(of groupId: String, in tasks: [DownloadTask]) -> [DownloadTask] {
        guard !groupId.isEmpty else { return [] }
        return tasks.filter { $0.groupId == groupId }
    }
}

// MARK: - 清单选择（N5）

/// 清单勾选 + 规格选择状态；`requestItems()` 投影为 `CreateGroupRequest.items`。
public struct ManifestSelection: Sendable, Hashable {
    public let items: [PreviewItemDto]
    public private(set) var selected: Set<String> = []
    /// 条目 id → 选中的规格 id；缺省 = 插件默认（`resolverItem` 不带 `@variantId`）。
    public private(set) var variants: [String: String] = [:]

    public init(items: [PreviewItemDto]) { self.items = items }

    public var count: Int { selected.count }

    public mutating func toggle(_ id: String) {
        if !selected.insert(id).inserted { selected.remove(id) }
    }

    public mutating func set(_ id: String, selected on: Bool) {
        if on { selected.insert(id) } else { selected.remove(id) }
    }

    /// 全选 / 清空限定在 `ids`（搜索结果内）；`ids` 全部已选时清空，否则全选。
    public mutating func toggleAll(in ids: [String]) {
        if !ids.isEmpty, ids.allSatisfy(selected.contains) {
            selected.subtract(ids)
        } else {
            selected.formUnion(ids)
        }
    }

    public mutating func invert(in ids: [String]) {
        for id in ids { toggle(id) }
    }

    public mutating func clear() { selected = [] }

    /// `variantId` nil = 插件默认。
    public mutating func chooseVariant(_ variantId: String?, for itemId: String) {
        variants[itemId] = variantId
    }

    public func variant(for item: PreviewItemDto) -> PreviewVariantDto? {
        guard let id = variants[item.id] else { return nil }
        return item.variants.first { $0.id == id }
    }

    /// 有效大小：选了规格且规格大小已知用规格，否则条目大小；0 = 未知。
    public func effectiveSize(of item: PreviewItemDto) -> Int64 {
        if let variant = variant(for: item), variant.size > 0 { return variant.size }
        return max(item.size, 0)
    }

    public var selectedItems: [PreviewItemDto] { items.filter { selected.contains($0.id) } }

    /// 已选总大小（字节，未知条目计 0）与未知大小条目数。
    public var selectedSize: (bytes: Int64, unknown: Int) {
        var bytes: Int64 = 0
        var unknown = 0
        for item in selectedItems {
            let size = effectiveSize(of: item)
            if size > 0 { bytes += size } else { unknown += 1 }
        }
        return (bytes, unknown)
    }

    public func requestItems() -> [GroupItemRequest] {
        selectedItems.map { item in
            let variant = variant(for: item)
            return GroupItemRequest(
                resolverItem: variant.map { "\(item.id)@\($0.id)" } ?? item.id,
                fileName: item.name,
                relPath: item.path,
                size: effectiveSize(of: item)
            )
        }
    }

    /// 默认组名：清单名；为空取来源 URL 路径末段（百分号解码）；都没有为空串。
    public static func defaultGroupName(manifestName: String, sourceUrl: String) -> String {
        let trimmed = manifestName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { return trimmed }
        guard let url = URL(string: sourceUrl) else { return "" }
        let last = url.path.split(separator: "/").last.map(String.init) ?? ""
        return last.removingPercentEncoding ?? last
    }

    /// 是否值得先探测多文件清单：仅 http(s)。
    public static func isPreviewable(_ url: String) -> Bool {
        let lower = url.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return lower.hasPrefix("http://") || lower.hasPrefix("https://")
    }
}
