import FluxDomain
import FluxUI
import Foundation

/// 订阅行的纯文案规则（同 Android `RssScreen.kt` 的 `relativeAgo` / 抓取间隔 / 状态行）。
enum RssFormat {
    /// 相对时间档位：< 1 分钟 / 分钟 / 小时 / 天（负值按 0 处理）。
    nonisolated enum Ago: Equatable {
        case justNow
        case minutes(Int64)
        case hours(Int64)
        case days(Int64)
    }

    static func ago(deltaSeconds: Int64) -> Ago {
        let d = max(deltaSeconds, 0)
        switch d {
        case ..<60: return .justNow
        case ..<3_600: return .minutes(d / 60)
        case ..<86_400: return .hours(d / 3_600)
        default: return .days(d / 86_400)
        }
    }

    static func agoText(_ ago: Ago) -> String {
        switch ago {
        case .justNow: L("rssJustNow")
        case let .minutes(n): L("rssMinutesAgo", ["n": n])
        case let .hours(n): L("rssHoursAgo", ["n": n])
        case let .days(n): L("rssDaysAgo", ["n": n])
        }
    }

    /// 整小时（≥ 60 且被 60 整除）显示「每 N 小时」，否则「每 N 分钟」。
    static func intervalText(minutes: Int32) -> String {
        if minutes >= 60, minutes % 60 == 0 {
            return L("rssEveryHours", ["n": minutes / 60])
        }
        return L("rssEveryMinutes", ["n": minutes])
    }

    /// 未读徽标文字：> 99 显示 `99+`。
    static func badgeText(unread: Int32) -> String {
        unread > 99 ? "99+" : String(unread)
    }

    /// 订阅显示名：名称为空白时回退到 URL。
    static func title(of source: RssSource) -> String {
        source.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? source.url : source.name
    }

    /// 订阅站点主机名（解析失败回退到原 URL）。
    static func host(of source: RssSource) -> String {
        URL(string: source.url)?.host ?? source.url
    }

    /// 状态行首段的种类（决定颜色）。
    nonisolated enum Status: Equatable {
        case refreshing
        case failed(count: Int32)
        case lastFetch(Ago)
        case neverFetched
    }

    static func status(of source: RssSource, refreshing: Bool, now: Date) -> Status {
        if refreshing { return .refreshing }
        if source.failCount > 0 { return .failed(count: source.failCount) }
        if source.lastSuccessAt > 0 {
            return .lastFetch(ago(deltaSeconds: Int64(now.timeIntervalSince1970) - source.lastSuccessAt))
        }
        return .neverFetched
    }

    static func statusText(_ status: Status) -> String {
        switch status {
        case .refreshing: L("rssRefreshing")
        case let .failed(count): L("rssFailedTimes", ["n": count])
        case let .lastFetch(ago): L("rssLastFetch", ["when": agoText(ago)])
        case .neverFetched: L("rssNeverFetched")
        }
    }

    /// 状态行后半段：间隔 · 模式（自动下载 / 收集）· 已停用。
    static func detailText(of source: RssSource) -> String {
        var parts = [
            intervalText(minutes: source.intervalMinutes),
            L(source.autoDownload ? "rssAutoDownloadOn" : "rssCollectMode"),
        ]
        if !source.enabled { parts.append(L("mobileRssDisabled")) }
        return parts.joined(separator: " · ")
    }
}

// MARK: - 条目（R2）

/// 条目关联任务的实时状态（来自 `HostState.tasks`，按 `taskId` 联动）。
nonisolated struct RssLinkedTask: Equatable {
    var status: TaskStatus
    var fileMissing: Bool
    /// 0...1；总大小未知为 0。
    var fraction: Double

    init(status: TaskStatus, fileMissing: Bool = false, fraction: Double = 0) {
        self.status = status
        self.fileMissing = fileMissing
        self.fraction = fraction
    }

    init(_ task: DownloadTask) {
        status = task.status
        fileMissing = task.fileMissing
        fraction = task.totalBytes > 0 ? min(max(Double(task.downloadedBytes) / Double(task.totalBytes), 0), 1) : 0
    }
}

/// 条目状态 chip：文案键 + 图标 + 语气（形状 + 颜色双通道）。
nonisolated struct RssItemChip: Equatable {
    var titleKey: String
    var systemImage: String
    var tone: BadgeTone
    /// 非空：下载中，显示进度环。
    var progress: Double?
}

extension RssFormat {
    /// 本地时区 + 系统区域格式的发布时间；无时间戳为空串。
    static func dateText(_ timestamp: Int64) -> String {
        guard timestamp > 0 else { return "" }
        return Date(timeIntervalSince1970: TimeInterval(timestamp)).formatted(date: .abbreviated, time: .shortened)
    }

    /// 体积（与全 App 一致的 `Format.bytes`）；未知为空串。
    static func bytesText(_ bytes: Int64) -> String {
        guard bytes > 0 else { return "" }
        return Format.bytes(bytes).description
    }

    /// 条目元信息：`发布时间 · 日期 · 大小`。
    static func metaText(of item: RssItemDto) -> String {
        let date = dateText(item.pubDate)
        let size = bytesText(item.enclosureLength)
        return [date.isEmpty ? "" : "\(L("rssPublishedAt")) · \(date)", size].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    /// 状态 chip：已下载条目以关联任务的真实状态为准，任务已删除 = 「关联任务已删除 · 可重新下载」。
    /// `tasksLoading`：任务表还在分页载入，查不到关联任务不代表已删除，不得显示「任务已删除」。
    static func chip(for item: RssItemDto, task: RssLinkedTask?, tasksLoading: Bool = false) -> RssItemChip {
        switch item.state {
        case .downloaded:
            if task == nil, tasksLoading, !item.taskId.isEmpty {
                return RssItemChip(titleKey: "rssTaskCreated", systemImage: "tray.and.arrow.down", tone: .neutral)
            }
            guard !item.taskId.isEmpty, let task else {
                return RssItemChip(titleKey: "rssTaskMissing", systemImage: "exclamationmark.circle", tone: .failure)
            }
            switch task.status {
            case .pending: return RssItemChip(titleKey: "statusPending", systemImage: "clock", tone: .neutral)
            case .downloading:
                return RssItemChip(titleKey: "statusDownloading", systemImage: "arrow.down.circle", tone: .accent, progress: task.fraction)
            case .paused: return RssItemChip(titleKey: "statusPaused", systemImage: "pause.circle", tone: .neutral)
            case .completed:
                return task.fileMissing
                    ? RssItemChip(titleKey: "statusIncomplete", systemImage: "exclamationmark.triangle", tone: .warning)
                    : RssItemChip(titleKey: "statusCompleted", systemImage: "checkmark.circle", tone: .success)
            case .failed: return RssItemChip(titleKey: "statusError", systemImage: "xmark.circle", tone: .failure)
            case .preparing: return RssItemChip(titleKey: "statusPreparing", systemImage: "hourglass", tone: .accent)
            case .unknown: return RssItemChip(titleKey: "rssTaskCreated", systemImage: "tray.and.arrow.down", tone: .neutral)
            }
        case .ignored:
            let title = switch item.reason {
            case "duplicate_torrent": "rssStatusDuplicateTorrent"
            case "task_removed": "rssStatusTaskRemoved"
            default: "rssStatusIgnored"
            }
            return RssItemChip(titleKey: title, systemImage: "eye.slash", tone: .neutral)
        case .filtered: return RssItemChip(titleKey: "rssStatusFiltered", systemImage: "line.3.horizontal.decrease", tone: .neutral)
        case .duplicateEpisode: return RssItemChip(titleKey: "rssStatusDuplicate", systemImage: "square.on.square", tone: .neutral)
        case .seeded: return RssItemChip(titleKey: "rssStatusHistory", systemImage: "clock.arrow.trianglehead.counterclockwise.rotate.90", tone: .neutral)
        case .new, .unknown: return RssItemChip(titleKey: "rssStatusNew", systemImage: "circle.fill", tone: .accent)
        }
    }

    /// 可见条目：标题包含（不区分大小写）→ 无发布时间的沉底 → 按发布时间排序 → 稳定回退到原顺序
    /// （GPUI `visible_indices` / Web `visibleIndices`）。
    static func visible(_ items: [RssItemDto], query: String, oldestFirst: Bool) -> [RssItemDto] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let matched = items.enumerated().filter { needle.isEmpty || $0.element.title.lowercased().contains(needle) }
        return matched.sorted { lhs, rhs in
            let lhsMissing = lhs.element.pubDate <= 0
            let rhsMissing = rhs.element.pubDate <= 0
            if lhsMissing != rhsMissing { return rhsMissing }
            if lhs.element.pubDate != rhs.element.pubDate {
                return oldestFirst ? lhs.element.pubDate < rhs.element.pubDate : lhs.element.pubDate > rhs.element.pubDate
            }
            return lhs.offset < rhs.offset
        }.map(\.element)
    }

    /// 队列显示名：主队列 / 稍后下载用本地化名，其余取队列自带名称。
    static func queueLabel(_ queue: TaskQueue) -> String {
        queueLabel(id: queue.queueId, name: queue.name)
    }

    static func queueLabel(id: String, name: String) -> String {
        switch id {
        case "", TaskQueue.main: L("mainQueue")
        case TaskQueue.later: L("laterQueue")
        default: name.isEmpty ? id : name
        }
    }

    /// 抓取间隔候选（分钟）；当前值不在其中时追加。
    static let intervalChoices: [Int32] = [10, 30, 60, 120, 360, 720, 1440]
}
