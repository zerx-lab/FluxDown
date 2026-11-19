import FluxDomain
import FluxUI
import Foundation

// 下载列表的派生逻辑（同 Android `DownloadsDerive.kt`）：筛选 → 计数 → 排序（含智能档位与进度 / 速度键限频重排）→
// 分区 / 分组 → 行视图模型。`DownloadsDeriver` 持有跨次派生的缓存，只在主 actor 上调用。

struct DeriveInput {
    let state: HostState
    let order: ViewOrder
    let filter: DownloadsFilter
    /// 已折叠的分组键（仅在分组视图生效）。
    let collapsed: Set<String>
    /// 其他设备上执行的远程任务（已去掉目标为本机的镜像）；组详情等本机专属视图不传。
    var remote: [RemoteTaskDto] = []
}

struct DeriveResult {
    let list: DownloadsList
    let facets: Facets
    /// 重排被延后时，建议的重新派生时刻（Unix 毫秒）；0 = 无需重试。
    let retryAtMs: Int64
}

struct DownloadsDeriver {
    /// 动态排序键（进度 / 速度）两次重排的最短间隔；手指 / 指针活动后的静默期（§3.7 数据绑定）。
    static let reorderMinMs: Int64 = 2_000
    static let reorderIdleMs: Int64 = 1_500

    private struct Entry {
        let task: DownloadTask
        let runtime: TaskRuntime?
        let down: Int64
        let up: Int64
        let queueOrder: Int64?
        let boosted: Bool
        let awaitingDecision: Bool
        let category: TaskCategory?
        let queue: TaskQueue?
        let item: TaskItem
    }

    private var entries: [String: Entry] = [:]
    /// `createdAt`（ISO 字符串）→ Unix 秒的解析缓存（字符串不变则结果不变）。
    private var createdCache: [String: Int64] = [:]
    private var categories: [TaskCategory] = []
    private var index = CategoryIndex(TaskCategory.builtin)
    /// 远程任务的分类缓存（网址规则让分类不再只取决于文件名，键为 文件名 + 来源地址）。
    private var categoryByKey: [CategoryKey: TaskCategory?] = [:]
    private var sortSignature = 0
    private var lastReorderMs: Int64 = 0
    private var lastPositions: [String: Int] = [:]

    init() {}

    mutating func derive(
        _ input: DeriveInput,
        nowMs: Int64,
        interactionMs: Int64,
        calendar: Calendar = .current
    ) -> DeriveResult {
        let s = input.state
        let f = input.filter
        let order = input.order

        if categories != s.categories {
            categories = s.categories
            index = CategoryIndex(s.categories)
            categoryByKey.removeAll()
            entries.removeAll()
        }
        var queuesById: [String: TaskQueue] = [:]
        queuesById.reserveCapacity(s.queues.count)
        for queue in s.queues { queuesById[normalizedQueueId(queue.queueId)] = queue }

        // 1. 行模型（输入未变化的复用同一值）
        let conflictTasks = Set(s.selections.fileConflicts.map(\.taskId))
        var all: [TaskItem] = []
        all.reserveCapacity(s.tasks.count)
        for task in s.tasks {
            all.append(item(for: task, state: s, queuesById: queuesById, awaitingDecision: conflictTasks.contains(task.taskId)))
        }
        if entries.count > all.count {
            let ids = Set(all.map(\.id))
            entries = entries.filter { ids.contains($0.key) }
        }

        // 2. 范围（队列 / 搜索）→ 文件夹计数
        let query = f.query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        var scoped: [TaskItem] = []
        scoped.reserveCapacity(all.count)
        for it in all {
            if let groupId = f.groupId, it.task.groupId != groupId { continue }
            if let queueId = f.queueId, normalizedQueueId(it.task.queueId) != queueId { continue }
            if !query.isEmpty, !Self.matches(it, query: query) { continue }
            scoped.append(it)
        }
        // 远程任务：队列 / 任务组是本机概念，限定时不出现；搜索、文件夹与分类同本地行（同 GPUI 下载页）。
        let remoteScoped = f.groupId == nil && f.queueId == nil
            ? input.remote.filter { query.isEmpty || Self.matches($0, query: query) }
            : []
        var folderCounts = Array(repeating: 0, count: StatusFolder.visible.count)
        for it in scoped {
            for (i, folder) in StatusFolder.visible.enumerated() where folder.accepts(it.task) {
                folderCounts[i] += 1
            }
        }
        for task in remoteScoped {
            for (i, folder) in StatusFolder.visible.enumerated() where folder.accepts(task.status) {
                folderCounts[i] += 1
            }
        }

        // 3. 文件夹内 → 分类计数 → 分类 / 仅远程筛选
        let inFolder = scoped.filter { f.folder.accepts($0.task) }
        let remoteInFolder = remoteRows(remoteScoped.filter { f.folder.accepts($0.status) })
        var categoryCounts: [String: Int] = [:]
        for it in inFolder {
            if let category = it.category { categoryCounts[category.id, default: 0] += 1 }
        }
        for remote in remoteInFolder {
            if let id = remote.categoryId { categoryCounts[id, default: 0] += 1 }
        }
        let selectedCategory = f.categoryId.flatMap { id in index.ordered.first { $0.id == id && !$0.isAll }?.id }
        var pills: [CategoryPill] = []
        for category in index.ordered where !category.isAll && category.visible {
            let n = categoryCounts[category.id] ?? 0
            if n > 0 || category.id == selectedCategory { pills.append(CategoryPill(category: category, count: n)) }
        }
        // 行集合：仅远程 → 只有远程行；选了分类 → 本机 + 远程中属于该分类的；否则两者都在。
        var rows: [DownloadsRow] = []
        rows.reserveCapacity(inFolder.count + remoteInFolder.count)
        if f.remoteOnly {
            rows.append(contentsOf: remoteInFolder.map(DownloadsRow.remote))
        } else if let id = selectedCategory {
            rows.append(contentsOf: inFolder.lazy.filter { $0.category?.id == id }.map(DownloadsRow.task))
            rows.append(contentsOf: remoteInFolder.lazy.filter { $0.categoryId == id }.map(DownloadsRow.remote))
        } else {
            rows.append(contentsOf: inFolder.map(DownloadsRow.task))
            rows.append(contentsOf: remoteInFolder.map(DownloadsRow.remote))
        }

        // 4. 排序（本机与远程同一比较器；进度 / 速度键节流重排）
        var retryAt: Int64 = 0
        var ordered = rows.map(Self.sortEntry)
            .sorted { Self.compare($0.key, $1.key, order: order) < 0 }
            .map(\.row)
        let dynamic = order.sortKey == .progress || order.sortKey == .speed
        var hasher = Hasher()
        hasher.combine(f)
        hasher.combine(order)
        let signature = hasher.finalize()
        if dynamic, sortSignature == signature, !lastPositions.isEmpty {
            let sinceReorder = nowMs - lastReorderMs
            let sinceTouch = nowMs - interactionMs
            if sinceReorder < Self.reorderMinMs || sinceTouch < Self.reorderIdleMs {
                let positions = lastPositions
                ordered = ordered.enumerated()
                    .sorted { lhs, rhs in
                        let l = positions[lhs.element.id] ?? .max
                        let r = positions[rhs.element.id] ?? .max
                        return l != r ? l < r : lhs.offset < rhs.offset
                    }
                    .map(\.element)
                retryAt = max(lastReorderMs + Self.reorderMinMs, interactionMs + Self.reorderIdleMs)
            } else {
                lastReorderMs = nowMs
            }
        } else {
            lastReorderMs = nowMs
        }
        sortSignature = signature
        var positions: [String: Int] = [:]
        positions.reserveCapacity(ordered.count)
        for (i, row) in ordered.enumerated() { positions[row.id] = i }
        lastPositions = positions

        // 5. 分区 / 分组
        var sections: [DownloadsSection] = []
        var visible: [String] = []
        visible.reserveCapacity(ordered.count)
        if order.groupBy == .none {
            Self.sectioned(ordered, folder: f.folder, into: &sections, visible: &visible)
        } else {
            for group in groups(for: ordered, by: order.groupBy, state: s, now: nowMs, calendar: calendar) {
                let closed = input.collapsed.contains(group.key)
                sections.append(DownloadsSection(
                    id: group.key,
                    kind: .group,
                    title: group.title,
                    count: group.rows.count,
                    doneOfTotal: group.doneOfTotal,
                    downSpeed: 0,
                    collapsed: closed,
                    rows: closed ? [] : group.rows
                ))
                if !closed { visible.append(contentsOf: group.rows.map(\.id)) }
            }
        }

        // 6. 队列分面
        var queueCounts: [String: Int] = [:]
        for it in all { queueCounts[normalizedQueueId(it.task.queueId), default: 0] += 1 }
        let queueFacets = s.queues
            .sorted { $0.position < $1.position }
            .map { QueueFacet(queue: $0, count: queueCounts[normalizedQueueId($0.queueId)] ?? 0) }

        return DeriveResult(
            list: DownloadsList(
                sections: sections,
                visibleIds: visible,
                taskTotal: s.tasks.count + input.remote.count,
                loaded: !s.tasksLoading,
                loadingTotal: s.tasksLoading ? s.taskTotal : 0
            ),
            facets: Facets(
                folderCounts: folderCounts,
                categories: pills,
                queues: queueFacets,
                remoteCount: remoteInFolder.count,
                matching: ordered.count
            ),
            retryAtMs: retryAt
        )
    }

    /// 远程任务 → 行：一次算好创建时间（Unix 秒）/ 分类 / 站点，供排序、分类与分组共用。
    private mutating func remoteRows(_ tasks: [RemoteTaskDto]) -> [RemoteRow] {
        if createdCache.count > 4096 { createdCache.removeAll() }
        return tasks.map { task in
            let created: Int64
            if let cached = createdCache[task.createdAt] {
                created = cached
            } else {
                created = Self.createdSeconds(task.createdAt)
                createdCache[task.createdAt] = created
            }
            return RemoteRow(
                task: task,
                createdAt: created,
                categoryId: resolveCategory(task.displayName, url: task.url)?.id,
                site: Self.site(ofUrl: task.url)
            )
        }
    }

    /// ISO-8601（带 / 不带小数秒）→ Unix 秒；解析失败 = 0。
    static func createdSeconds(_ iso: String) -> Int64 {
        guard let date = Self.isoFractional.date(from: iso) ?? Self.isoPlain.date(from: iso) else { return 0 }
        return Int64(date.timeIntervalSince1970.rounded(.down))
    }

    private static let isoFractional: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    private static let isoPlain: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()

    private struct CategoryKey: Hashable {
        let name: String
        let url: String
    }

    /// 按文件名 + 来源地址解析分类（远程行用；本地行优先沿用上一行的分类）。
    private mutating func resolveCategory(_ name: String, url: String) -> TaskCategory? {
        let key = CategoryKey(name: name, url: url)
        if let cached = categoryByKey[key] { return cached }
        let category = index.categoryOf(name, url: url)
        categoryByKey[key] = .some(category)
        return category
    }

    // MARK: 行模型

    private mutating func item(
        for task: DownloadTask, state s: HostState, queuesById: [String: TaskQueue], awaitingDecision: Bool
    ) -> TaskItem {
        let speed = s.speeds[task.taskId]
        let down = speed?.down ?? 0
        let up = speed?.up ?? 0
        let runtime = s.runtime[task.taskId]
        let queueOrder = s.queueOrder.order(of: task.taskId)
        let boosted = s.priorityTaskId == task.taskId
        let queue = queuesById[normalizedQueueId(task.queueId)]

        let previous = entries[task.taskId]
        // 分类只取决于文件名与来源地址（网址规则）：二者未变就沿用上一行的分类，不在 10 Hz 派生里重算规则。
        let category: TaskCategory?
        if let previous, previous.task.fileName == task.fileName, previous.task.url == task.url,
           previous.task.originUrl == task.originUrl {
            category = previous.category
        } else {
            category = index.categoryOf(task.fileName, url: task.shareUrl)
        }

        if let previous = previous,
           previous.task == task, previous.runtime == runtime, previous.down == down, previous.up == up,
           previous.queueOrder == queueOrder, previous.boosted == boosted, previous.awaitingDecision == awaitingDecision,
           previous.category == category, previous.queue == queue {
            return previous.item
        }

        let visual = TaskVisual(task, queued: queueOrder != nil)
        var kind = FileKind.from(fileName: task.fileName)
        if kind == .other, task.protocol == .bt { kind = .torrent }
        let built = TaskItem(
            task: task,
            speedDown: down,
            speedUp: up,
            queueOrder: queueOrder,
            boosted: boosted,
            awaitingDecision: awaitingDecision,
            category: category,
            queue: queue,
            site: Self.site(of: task),
            visual: visual,
            kind: kind,
            progressBar: Self.progressBar(task: task, visual: visual, runtime: runtime),
            ring: Self.ring(visual: visual, status: task.status),
            ringProgress: Self.ringProgress(task: task, visual: visual),
            eta: visual == .downloading ? Format.etaSeconds(downloaded: task.downloadedBytes, total: task.totalBytes, speed: down) : nil,
            activeTransfers: runtime?.activeTransfers.map { Int($0) },
            connectedPeers: runtime?.connectedPeers.map { Int($0) }
        )
        entries[task.taskId] = Entry(
            task: task, runtime: runtime, down: down, up: up, queueOrder: queueOrder,
            boosted: boosted, awaitingDecision: awaitingDecision, category: category, queue: queue, item: built
        )
        return built
    }

    /// 来源站点：originUrl ‖ url 的 host（去 `www.` 与端口）；BT / eD2K 哨兵无 host → 空串。
    static func site(of task: DownloadTask) -> String {
        site(ofUrl: task.originUrl.isEmpty ? task.url : task.originUrl)
    }

    /// url 的 host（去 `www.` 与端口）；BT / eD2K 哨兵与非 URL → 空串。
    static func site(ofUrl raw: String) -> String {
        if raw.hasPrefix("magnet:") || raw.hasPrefix("torrent-file://") || raw.hasPrefix("ed2k://") { return "" }
        guard let scheme = raw.range(of: "://") else { return "" }
        var rest = raw[scheme.upperBound...].prefix { $0 != "/" }
        rest = rest.prefix { $0 != "?" }
        if let at = rest.lastIndex(of: "@") { rest = rest[rest.index(after: at)...] }
        var host = String(rest.prefix { $0 != ":" })
        if host.hasPrefix("www.") { host.removeFirst(4) }
        return host
    }

    private static func matches(_ item: TaskItem, query: String) -> Bool {
        let task = item.task
        return task.fileName.lowercased().contains(query)
            || task.url.lowercased().contains(query)
            || task.originUrl.lowercased().contains(query)
            || item.site.lowercased().contains(query)
    }

    private static func matches(_ task: RemoteTaskDto, query: String) -> Bool {
        task.displayName.lowercased().contains(query) || task.url.lowercased().contains(query)
    }

    /// 进度条：按真实字节区间投影；无分段退化为单条总进度。完成 / 做种 / 文件已删除行不显示（§3.5）。
    static func progressBar(task: DownloadTask, visual: TaskVisual, runtime: TaskRuntime?) -> ProgressBarModel? {
        let tone: SegmentTone
        switch visual {
        case .completed, .missing, .seeding:
            return nil
        case .downloading, .preparing, .verifying:
            tone = .downloading
        case .queued, .pending:
            guard task.totalBytes > 0 || task.downloadedBytes > 0 else { return nil }
            tone = .queued
        case .paused:
            guard task.totalBytes > 0 || task.downloadedBytes > 0 else { return nil }
            tone = .paused
        case .failed:
            guard task.totalBytes > 0 || task.downloadedBytes > 0 else { return nil }
            tone = .failed
        }
        // 准备中（总大小未知）= 不定进度条（`SegmentMapView` 在 progress == nil + .downloading 时横扫）。
        let progress: Double? = visual == .preparing ? nil : task.progress
        let usesSegments = task.totalBytes > 0 && (visual == .downloading || visual == .paused || visual == .failed)
        var spans: [SegmentSpan] = []
        if usesSegments, let segments = runtime?.segments, !segments.isEmpty {
            let live = visual == .downloading
            spans = segments.compactMap { segment in
                guard segment.endByte >= segment.startByte else { return nil }
                // 协议里 `endByte` 是闭区间终点；`SegmentSpan.endByte` 不含。
                return SegmentSpan(
                    startByte: segment.startByte,
                    endByte: segment.endByte + 1,
                    downloadedBytes: segment.downloadedBytes,
                    active: live ? segment.active : false
                )
            }
        }
        return ProgressBarModel(spans: spans, totalBytes: spans.isEmpty ? 0 : task.totalBytes, progress: progress, tone: tone)
    }

    /// 行尾圆环字形（§1.3）；未知状态不可控，无圆环。
    static func ring(visual: TaskVisual, status: TaskStatus) -> RingGlyph? {
        guard status != .unknown else { return nil }
        switch visual {
        case .downloading, .pending, .queued: return .pause
        case .preparing, .verifying: return .preparing
        case .paused: return .resume
        case .failed: return .retry
        case .missing: return .redownload
        case .seeding, .completed: return .open
        }
    }

    static func ringProgress(task: DownloadTask, visual: TaskVisual) -> Double? {
        switch visual {
        case .completed, .missing, .seeding, .preparing, .verifying: nil
        default: task.progress
        }
    }

    // MARK: 排序

    /// 本机与远程行共用的排序投影：比较器只看这些字段（远程行按契约投影，见 `sortKey(_: RemoteRow)`）。
    private struct SortProjection {
        let id: String
        let tier: Int
        /// Unix 秒。
        let created: Int64
        let name: String
        /// 未知大小 = 0。
        let size: Int64
        /// nil = 无进度（排序时视为 -1）。
        let progress: Double?
        let speed: Int64
    }

    private struct SortEntry {
        let row: DownloadsRow
        let key: SortProjection
    }

    private static func sortEntry(_ row: DownloadsRow) -> SortEntry {
        switch row {
        case let .task(item): SortEntry(row: row, key: sortKey(item))
        case let .remote(remote): SortEntry(row: row, key: sortKey(remote))
        }
    }

    private static func sortKey(_ item: TaskItem) -> SortProjection {
        SortProjection(
            id: item.id,
            tier: tier(item),
            created: item.task.createdAt,
            name: item.task.fileName,
            size: item.task.totalBytes,
            progress: item.task.progress,
            speed: item.speedDown
        )
    }

    /// 远程投影：档位见 `tier(_: RemoteTaskStatus)`；名称 = 显示名；大小 = `totalBytes ?? 0`；
    /// 进度 = `progress`（未知状态无进度）；速度 = 非终态时的 `speed`，终态 0。
    private static func sortKey(_ remote: RemoteRow) -> SortProjection {
        let task = remote.task
        return SortProjection(
            id: task.id,
            tier: tier(task.status),
            created: remote.createdAt,
            name: task.displayName,
            size: task.totalBytes ?? 0,
            progress: task.status.isUnknown ? nil : task.progress,
            speed: task.status.isTerminal ? 0 : task.speed
        )
    }

    /// 智能排序档（§1.2）：优先下载 → 活跃 → 排队 → 失败 → 暂停 → 完成。
    static func tier(_ item: TaskItem) -> Int {
        let status = item.task.status
        if item.boosted, status == .downloading || status == .pending { return 0 }
        if status == .downloading || status == .preparing { return 1 }
        if status == .pending { return 2 }
        if status == .failed { return 3 }
        if status == .paused { return 4 }
        return 5
    }

    /// 远程任务的智能排序档（永不 boosted）：下载中 1 · 等待接单 / 已接单 / 未知 2 · 失败 / 已取消 3 · 暂停 4 · 完成 5。
    static func tier(_ status: RemoteTaskStatus) -> Int {
        switch status {
        case .downloading: 1
        case .pending, .accepted, .unknown: 2
        case .failed, .canceled: 3
        case .paused: 4
        case .completed: 5
        }
    }

    private static func three<T: Comparable>(_ a: T, _ b: T) -> Int { a < b ? -1 : (a > b ? 1 : 0) }

    /// 三态比较：<0 → a 在前。
    private static func compare(_ a: SortProjection, _ b: SortProjection, order: ViewOrder) -> Int {
        if order.sortKey == .smart {
            if a.tier != b.tier { return three(a.tier, b.tier) }
            let byCreated = three(a.created, b.created)
            // 活跃 / 排队档按添加顺序正序，历史档新 → 旧。
            let directed = a.tier <= 2 ? byCreated : -byCreated
            return directed != 0 ? directed : three(a.id, b.id)
        }
        var key: Int
        switch order.sortKey {
        case .created, .smart:
            key = three(a.created, b.created)
        case .name:
            switch a.name.caseInsensitiveCompare(b.name) {
            case .orderedAscending: key = -1
            case .orderedDescending: key = 1
            case .orderedSame: key = 0
            }
        case .size:
            key = three(a.size, b.size)
        case .progress:
            key = three(a.progress ?? -1, b.progress ?? -1)
        case .speed:
            key = three(a.speed, b.speed)
        case .status:
            key = three(a.tier, b.tier)
        }
        if !order.ascending { key = -key }
        if key != 0 { return key }
        let newestFirst = three(b.created, a.created)
        return newestFirst != 0 ? newestFirst : three(a.id, b.id)
    }

    // MARK: 分区 / 分组

    /// 「传输中」：本机 pending / downloading / preparing + 远程 pending / accepted / downloading / unknown
    /// （同「下载中」文件夹的成员）。
    private static func isInflight(_ row: DownloadsRow) -> Bool {
        switch row {
        case let .task(item): StatusFolder.active.accepts(item.task.status)
        case let .remote(remote): StatusFolder.active.accepts(remote.task.status)
        }
    }

    private static func sectioned(
        _ rows: [DownloadsRow],
        folder: StatusFolder,
        into sections: inout [DownloadsSection],
        visible: inout [String]
    ) {
        let inflight = rows.filter { isInflight($0) }
        let history = rows.filter { !isInflight($0) }
        if !inflight.isEmpty {
            sections.append(DownloadsSection(
                id: "flow", kind: .inFlight, title: .inFlight, count: inflight.count, doneOfTotal: nil,
                // 汇总速度只算本机：远程速度是别的设备的吞吐。
                downSpeed: inflight.reduce(0) { $0 + ($1.item?.speedDown ?? 0) }, collapsed: false, rows: inflight
            ))
            visible.append(contentsOf: inflight.map(\.id))
        }
        if !history.isEmpty {
            let named: StatusFolder? = (folder == .completed || folder == .seeding || folder == .failed || folder == .paused) ? folder : nil
            sections.append(DownloadsSection(
                id: "history", kind: .history, title: .history(named), count: history.count, doneOfTotal: nil,
                downSpeed: 0, collapsed: false, rows: history
            ))
            visible.append(contentsOf: history.map(\.id))
        }
    }

    private struct Group {
        let key: String
        let title: SectionTitle
        let rows: [DownloadsRow]
        var doneOfTotal: DoneOfTotal?

        init(key: String, title: SectionTitle, rows: [DownloadsRow], doneOfTotal: DoneOfTotal? = nil) {
            self.key = key
            self.title = title
            self.rows = rows
            self.doneOfTotal = doneOfTotal
        }
    }

    /// 按首次出现顺序分桶。
    private static func bucketed(_ rows: [DownloadsRow], key: (DownloadsRow) -> String) -> [(key: String, rows: [DownloadsRow])] {
        var order: [String] = []
        var buckets: [String: [DownloadsRow]] = [:]
        for row in rows {
            let k = key(row)
            if buckets[k] == nil { order.append(k) }
            buckets[k, default: []].append(row)
        }
        return order.map { ($0, buckets[$0] ?? []) }
    }

    /// 状态分组键：本机 downloading → 1 · pending / preparing → 0 · failed → 4 · paused → 2 · completed / unknown → 3；
    /// 远程 downloading → 1 · pending / accepted / unknown → 0 · failed / canceled → 4 · paused → 2 · completed → 3。
    private static func statusKey(_ row: DownloadsRow) -> String {
        switch row {
        case let .task(item):
            switch item.task.status {
            case .downloading: "status:1"
            case .pending, .preparing: "status:0"
            case .failed: "status:4"
            case .paused: "status:2"
            case .completed, .unknown: "status:3"
            }
        case let .remote(remote):
            switch remote.task.status {
            case .downloading: "status:1"
            case .pending, .accepted, .unknown: "status:0"
            case .failed, .canceled: "status:4"
            case .paused: "status:2"
            case .completed: "status:3"
            }
        }
    }

    private static func createdAt(_ row: DownloadsRow) -> Int64 {
        switch row {
        case let .task(item): item.task.createdAt
        case let .remote(remote): remote.createdAt
        }
    }

    private static func categoryId(_ row: DownloadsRow) -> String {
        switch row {
        case let .task(item): item.category?.id ?? ""
        case let .remote(remote): remote.categoryId ?? ""
        }
    }

    private static func site(_ row: DownloadsRow) -> String {
        switch row {
        case let .task(item): item.site
        case let .remote(remote): remote.site
        }
    }

    #if FULL_FLAVOR
    /// 无 host 的行是否属于 BT / 磁力（本机看协议，远程看链接形态）。
    private static func isBt(_ row: DownloadsRow) -> Bool {
        switch row {
        case let .task(item): item.task.protocol == .bt
        case let .remote(remote): remote.task.url.hasPrefix("magnet:") || remote.task.url.hasPrefix("torrent-file://")
        }
    }
    #endif

    private func groups(
        for rows: [DownloadsRow],
        by groupBy: GroupBy,
        state s: HostState,
        now nowMs: Int64,
        calendar: Calendar
    ) -> [Group] {
        switch groupBy {
        case .none:
            return []
        case .status:
            let buckets: [(key: String, title: String)] = [
                ("status:1", "statusDownloading"),
                ("status:0", "statusPending"),
                ("status:4", "statusError"),
                ("status:2", "statusPaused"),
                ("status:3", "statusCompleted"),
            ]
            let byStatus = Dictionary(grouping: rows, by: Self.statusKey)
            return buckets.compactMap { bucket in
                guard let items = byStatus[bucket.key] else { return nil }
                return Group(key: bucket.key, title: .key(bucket.title), rows: items)
            }
        case .date:
            let now = Date(timeIntervalSince1970: TimeInterval(nowMs) / 1000)
            let today = calendar.startOfDay(for: now)
            func start(daysAgo: Int) -> Int64 {
                Int64(calendar.date(byAdding: .day, value: -daysAgo, to: today)?.timeIntervalSince1970 ?? 0)
            }
            let bounds = [Int64(today.timeIntervalSince1970), start(daysAgo: 1), start(daysAgo: 7), start(daysAgo: 30)]
            let labels = ["today", "yesterday", "thisWeek", "thisMonth", "older"]
            var buckets = Array(repeating: [DownloadsRow](), count: labels.count)
            for row in rows {
                let created = Self.createdAt(row)
                let b = bounds.firstIndex { created >= $0 } ?? labels.count - 1
                buckets[b].append(row)
            }
            return buckets.enumerated().compactMap { i, items in
                items.isEmpty ? nil : Group(key: "date:\(i)", title: .key(labels[i]), rows: items)
            }
        case .type:
            let byCategory = Dictionary(grouping: rows, by: Self.categoryId)
            var out: [Group] = []
            for category in index.ordered where !category.isAll {
                guard let items = byCategory[category.id] else { continue }
                out.append(Group(key: "type:\(category.id)", title: .category(category), rows: items))
            }
            if let none = byCategory[""] { out.append(Group(key: "type:none", title: .key("categoryOther"), rows: none)) }
            return out
        case .queue:
            // 远程任务没有本机队列：单独一组放最后。
            let local = rows.filter { $0.item != nil }
            let remote = rows.filter { $0.item == nil }
            let byQueue = Self.bucketed(local) { normalizedQueueId($0.item?.task.queueId ?? "") }
            let lookup = Dictionary(uniqueKeysWithValues: byQueue.map { ($0.key, $0.rows) })
            var used = Set<String>()
            var out: [Group] = []
            for queue in s.queues.sorted(by: { $0.position < $1.position }) {
                let key = normalizedQueueId(queue.queueId)
                guard let items = lookup[key] else { continue }
                used.insert(key)
                out.append(Group(key: "queue:\(key)", title: .queue(queue), rows: items))
            }
            for bucket in byQueue where !used.contains(bucket.key) {
                out.append(Group(key: "queue:\(bucket.key)", title: .text(bucket.key), rows: bucket.rows))
            }
            if !remote.isEmpty { out.append(Group(key: "queue:remote", title: .key("remoteTasksGroup"), rows: remote)) }
            return out
        case .site:
            let bySite = Dictionary(grouping: rows, by: Self.site)
            var out: [Group] = []
            for site in bySite.keys.filter({ !$0.isEmpty }).sorted(by: { $0.caseInsensitiveCompare($1) == .orderedAscending }) {
                out.append(Group(key: "site:\(site)", title: .text(site), rows: bySite[site] ?? []))
            }
            if let hostless = bySite[""] {
                #if FULL_FLAVOR
                // 无 host：磁力 / 种子归「BT · 磁力」，其余（eD2K 等）归「—」。
                let bt = hostless.filter(Self.isBt)
                let other = hostless.filter { !Self.isBt($0) }
                if !bt.isEmpty { out.append(Group(key: "site:", title: .key("viewSiteBt"), rows: bt)) }
                if !other.isEmpty { out.append(Group(key: "site:-", title: .text(Format.dash), rows: other)) }
                #else
                // store 不含 P2P：无 host 的行一律归「—」。
                out.append(Group(key: "site:-", title: .text(Format.dash), rows: hostless))
                #endif
            }
            return out
        case .group:
            // 远程任务不属于任何本机任务组：归「未分组」。
            let byGroup = Dictionary(grouping: rows.compactMap(\.item)) { $0.task.groupId }
            var out: [Group] = []
            for group in s.groups.sorted(by: { $0.name.lowercased() < $1.name.lowercased() }) {
                guard let members = byGroup[group.groupId] else { continue }
                let done = members.filter { $0.task.status == .completed }.count
                out.append(Group(
                    key: "group:\(group.groupId)", title: .text(group.name), rows: members.map(DownloadsRow.task),
                    doneOfTotal: DoneOfTotal(done: done, total: members.count)
                ))
            }
            let known = Set(s.groups.map(\.groupId))
            let loose = rows.filter { row in
                guard let item = row.item else { return true }
                return item.task.groupId.isEmpty || !known.contains(item.task.groupId)
            }
            if !loose.isEmpty { out.append(Group(key: "group:none", title: .key("ungroupedTasks"), rows: loose)) }
            return out
        }
    }
}
