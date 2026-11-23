import FluxDomain
import FluxUI
import Foundation

// 下载列表的纯派生模型（同 Android `DownloadsModel.kt`）：筛选 → 计数 → 排序 → 分区 / 分组 → 行视图模型。
// 全部是值类型，不依赖 SwiftUI，可直接单测（见 `FluxDownTests/DownloadsDeriveTests.swift`）。

/// 状态文件夹（范围条）；顺序即显示顺序（02-downloads §3.2）。
nonisolated enum StatusFolder: CaseIterable, Hashable {
    case all, active, completed, seeding, failed, paused

    /// 范围条显示（也是 `Facets.folderCounts` 的索引序）的文件夹：「做种中」是 BT 专属，商店版不含 BT，隐藏。
    static let visible: [StatusFolder] = {
        #if FULL_FLAVOR
        allCases
        #else
        allCases.filter { $0 != .seeding }
        #endif
    }()

    /// 本机任务是否属于该文件夹：「做种中」按 `isSeeding`（1 / 8）判定，其余按状态；做种任务同时仍在「已完成」。
    func accepts(_ task: DownloadTask) -> Bool {
        self == .seeding ? task.isSeeding : accepts(task.status)
    }

    func accepts(_ status: TaskStatus) -> Bool {
        switch self {
        case .all: true
        case .active: status == .pending || status == .downloading || status == .preparing
        case .completed: status == .completed
        case .seeding: false
        case .failed: status == .failed
        case .paused: status == .paused
        }
    }

    /// 其他设备上的远程任务按 GPUI `DownloadTaskView::remote` 的状态映射归入文件夹：等待接单 / 已接单 /
    /// 未知 / 下载中 → 下载中，失败 / 已取消 → 失败。
    func accepts(_ status: RemoteTaskStatus) -> Bool {
        switch self {
        case .all: true
        case .active:
            switch status {
            case .pending, .accepted, .downloading, .unknown: true
            case .paused, .completed, .failed, .canceled: false
            }
        case .completed: status == .completed
        case .seeding: false
        case .failed: status == .failed || status == .canceled
        case .paused: status == .paused
        }
    }
}

/// 列表筛选：状态文件夹 · 分类 / 仅远程任务（互斥）· 队列范围 · 搜索词。
nonisolated struct DownloadsFilter: Hashable {
    var folder: StatusFolder = .all
    var categoryId: String?
    /// 只看其他设备上的远程任务；与 `categoryId` 互斥（由 `DownloadsModel` 维护）。
    var remoteOnly = false
    /// 已规范化的队列 id（主队列 = `TaskQueue.main`）；nil = 全部队列。
    var queueId: String?
    var query: String = ""
    /// 仅某任务组的成员（组详情页内部使用；下载列表本身不设置）。
    var groupId: String?
}

/// 主队列的任务 `queueId` 可能是 ""（隐式主队列）或 `main`，视为同一个。
func normalizedQueueId(_ id: String) -> String { id.isEmpty ? TaskQueue.main : id }

/// 派生显示状态（§1.3：颜色 + 图形冗余）。
nonisolated enum TaskVisual: Hashable {
    case downloading, queued, pending, preparing, verifying, paused, failed, seeding, missing, completed

    /// `queued`：任务在引擎排队顺序里（有排序键）。
    init(_ task: DownloadTask, queued: Bool) {
        switch task.status {
        case .downloading: self = .downloading
        case .pending: self = queued ? .queued : .pending
        case .preparing: self = task.totalBytes > 0 ? .verifying : .preparing
        case .paused: self = .paused
        case .failed: self = .failed
        case .completed:
            if task.fileMissing {
                self = .missing
            } else if task.presentsSeeding {
                self = .seeding
            } else {
                self = .completed
            }
        case .unknown: self = .pending
        }
    }
}

/// 分组 / 排序 / 过滤之外决定列表顺序的偏好（不含密度与卡片字段：它们只影响行渲染）。
nonisolated struct ViewOrder: Hashable {
    var groupBy: GroupBy
    var sortKey: SortKey
    var ascending: Bool
}

/// 行内进度条的输入：有分段 → 按真实字节区间；否则回退总进度（`SegmentMapView` 的两种形态）。
nonisolated struct ProgressBarModel: Equatable {
    var spans: [SegmentSpan]
    var totalBytes: Int64
    /// 总进度；总大小未知 + 下载中 + 无分段 → 不定进度条。
    var progress: Double?
    var tone: SegmentTone
}

/// 一行任务的全部展示输入（派生时算好并按输入复用实例：未变化的任务得到相等的值，行视图据此跳过重算）。
nonisolated struct TaskItem: Identifiable, Equatable {
    let task: DownloadTask
    let speedDown: Int64
    let speedUp: Int64
    /// 引擎排序键（任务在队列期间不变）；nil = 不在排队。名次不进模型：渲染可见行时按 `QueueOrder` 取。
    let queueOrder: Int64?
    let boosted: Bool
    /// 有待答的「文件已存在」询问（任务在等用户决定，行上显示「待确认」角标）。
    let awaitingDecision: Bool
    let category: TaskCategory?
    let queue: TaskQueue?
    /// 来源站点（去 `www.` 与端口）；BT / eD2K 哨兵无 host → 空串。
    let site: String
    let visual: TaskVisual
    let kind: FileKind
    let progressBar: ProgressBarModel?
    let ring: RingGlyph?
    let ringProgress: Double?
    /// 仅下载中且速度 / 总大小已知。
    let eta: Int64?
    let activeTransfers: Int?
    let connectedPeers: Int?

    var id: String { task.taskId }
}

/// 分区 / 分组标题（视图层解析为文案；与 Android `GroupLabel` 同构）。
nonisolated enum SectionTitle: Equatable {
    case inFlight
    /// 选中 已完成 / 失败 / 已暂停 文件夹时换成该文件夹名。
    case history(StatusFolder?)
    /// i18n 键（状态 / 日期 / 「其他」/「未分组」…）。
    case key(String)
    case text(String)
    case category(TaskCategory)
    case queue(TaskQueue)
}

nonisolated struct DoneOfTotal: Equatable {
    let done: Int
    let total: Int
}

/// 其他设备上的远程任务行（同 PC 下载页的远程行）。排序 / 分组 / 分类用到的投影在派生时一次算好，行视图只读 `task`。
nonisolated struct RemoteRow: Identifiable, Equatable {
    let task: RemoteTaskDto
    /// `createdAt`（ISO-8601）解析出的 Unix 秒；解析失败 = 0。
    let createdAt: Int64
    /// 按显示名解析的分类 id；无分类 = nil。
    let categoryId: String?
    /// 来源站点（url 的 host，去 `www.` 与端口）；无 host = 空串。
    let site: String

    var id: String { task.id }
}

/// 列表里的一行：本机任务或远程任务。`id` 全局唯一（远程行加 `remote:` 前缀，避免与本机任务 id 相撞）。
nonisolated enum DownloadsRow: Identifiable, Equatable {
    case task(TaskItem)
    case remote(RemoteRow)

    /// 远程行 id 前缀（多选集合里用它区分本机 / 远程）。
    static let remoteIdPrefix = "remote:"

    var id: String {
        switch self {
        case let .task(item): item.id
        case let .remote(row): Self.remoteIdPrefix + row.id
        }
    }

    /// 本机任务；远程行 = nil。
    var item: TaskItem? {
        if case let .task(item) = self { item } else { nil }
    }
}

nonisolated struct DownloadsSection: Identifiable, Equatable {
    nonisolated enum Kind: Equatable {
        /// 「传输中」：右侧实时汇总下行速度 + 任务数。
        case inFlight
        case history
        /// 可折叠的分组。
        case group
    }

    let id: String
    let kind: Kind
    let title: SectionTitle
    /// 该分组 / 分区的成员数（折叠时仍是完整成员数）。
    let count: Int
    /// 仅「任务组」分组：完成数 / 总数。
    let doneOfTotal: DoneOfTotal?
    /// 仅 `.inFlight`：汇总下行速度。
    let downSpeed: Int64
    let collapsed: Bool
    /// 折叠时为空。本机行与远程行按排序穿插。
    let rows: [DownloadsRow]
}

/// 派生出的列表：分区 + 可见行 id（全选范围：本机任务 id 与 `remote:` 前缀的远程行 id）+ 总任务数（区分「无任务」与「筛选后为空」）。
nonisolated struct DownloadsList: Equatable {
    let sections: [DownloadsSection]
    let visibleIds: [String]
    /// 本机任务 + 远程任务总数（区分「无任务」与「筛选后为空」）。
    let taskTotal: Int
    /// false = 任务表尚未载入完成（首个快照前 / 分页载入中）：`sections` 与计数不是完整结果，UI 只能显示加载态。
    let loaded: Bool
    /// 载入中的服务端任务总数（进度文案）；载入完成后为 0。
    let loadingTotal: UInt64

    static let initial = DownloadsList(sections: [], visibleIds: [], taskTotal: 0, loaded: false, loadingTotal: 0)

    var isEmpty: Bool { sections.allSatisfy { $0.count == 0 } }
}

nonisolated struct CategoryPill: Equatable, Identifiable {
    let category: TaskCategory
    let count: Int
    var id: String { category.id }
}

nonisolated struct QueueFacet: Equatable, Identifiable {
    let queue: TaskQueue
    let count: Int
    var id: String { queue.queueId }
}

/// 分面：文件夹计数（队列 / 搜索范围内）· 当前文件夹内的分类计数 · 各队列任务数 · 远程任务数。
nonisolated struct Facets: Equatable {
    /// 按 `StatusFolder.visible` 顺序。
    let folderCounts: [Int]
    let categories: [CategoryPill]
    let queues: [QueueFacet]
    /// 范围（队列 / 搜索 / 任务组）与当前文件夹内的远程任务数（分类筛选之前）。
    let remoteCount: Int
    /// 当前筛选结果行数（本机 + 远程）。
    let matching: Int

    static let initial = Facets(
        folderCounts: Array(repeating: 0, count: StatusFolder.visible.count),
        categories: [],
        queues: [],
        remoteCount: 0,
        matching: 0
    )

    func count(_ folder: StatusFolder) -> Int {
        guard let index = StatusFolder.visible.firstIndex(of: folder), folderCounts.indices.contains(index) else { return 0 }
        return folderCounts[index]
    }
}
