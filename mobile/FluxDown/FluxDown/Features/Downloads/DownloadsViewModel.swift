import FluxDomain
import Foundation
import Observation

/// 与列表派生同步发布的「壳层」信息：只在值变化时才发布，页面 body 不随 10 Hz 的主机状态重算。
nonisolated struct DownloadsChrome: Equatable {
    nonisolated enum Link: Equatable { case connecting, live, stale, failed }

    var link: Link = .connecting
    var diskFree: UInt64?
    /// 第一个待处理的选择请求（含被用户划走的）：横幅可重新打开。
    var pendingSelection: PendingSelection?

    /// 断连宽限后只读（stale / failed）；首次连接中不弹横幅但也不可写。
    var isReadOnly: Bool { link != .live }
    var showsOfflineBanner: Bool { link == .stale || link == .failed }
}

nonisolated struct PendingSelection: Equatable {
    /// 「选择」按钮打开的 Sheet：BT / HLS / 插件规格各自一个请求，文件已存在聚合为 `.fileConflicts`。
    let route: SheetRoute
    /// 标题 i18n 键（BT 文件 / HLS 画质 / 插件规格 / 文件已存在）。
    let titleKey: String
    /// 标题占位符 `{count}`（仅「N 个文件已存在」）。
    let titleCount: Int?
    let taskName: String
    /// 倒计时文案键（占位符 `{seconds}`）：默认选择 / 自动重命名。
    let countdownKey: String
    let deadlineUnixMs: Int64

    /// 横幅对应队首（本构建可处理的）请求；队首是「文件已存在」时聚合全部同类请求（最早的到期时间）。
    static func make(_ selections: [SelectionRequest], taskName: (String) -> String?) -> PendingSelection? {
        let selections = selections.presentable
        guard let request = selections.first else { return nil }
        func plain(_ titleKey: String) -> PendingSelection {
            PendingSelection(
                route: .selection(requestId: request.requestId),
                titleKey: titleKey,
                titleCount: nil,
                taskName: taskName(request.taskId) ?? "",
                countdownKey: "selectionAutoDefaultIn",
                deadlineUnixMs: request.deadlineUnixMs
            )
        }
        switch request.kind {
        #if FULL_FLAVOR
        case .bt: return plain("btFileSelectTitle")
        case .variant: return plain("resolveVariantTitle")
        case .plugin: return plain("pluginChoiceTitle")
        #else
        case .bt, .variant, .plugin: return nil
        #endif
        case .hls: return plain("hlsQualityTitle")
        case let .fileExists(conflict):
            let conflicts = selections.fileConflicts
            let many = conflicts.count > 1
            return PendingSelection(
                route: .fileConflicts,
                titleKey: many ? "fileConflictTitleMany" : "fileConflictTitle",
                titleCount: many ? conflicts.count : nil,
                taskName: many ? "" : conflict.fileName,
                countdownKey: "fileConflictAutoRenameIn",
                deadlineUnixMs: conflicts.map(\.deadlineUnixMs).min() ?? request.deadlineUnixMs
            )
        }
    }
}

/// 多选的批量能力：继续 / 暂停 / 移动 各自的可用性（Resume / Pause 取「存在」）。
/// 远程命令的 id 为云端任务 id（不带行前缀），已按 `RemoteTaskRules.canIssue` 过滤；导出不受命令资格限制。
nonisolated struct SelectionCaps: Equatable {
    var canResume: Bool { !resumeIds.isEmpty || !remoteResumeIds.isEmpty }
    var canPause: Bool { !pauseIds.isEmpty || !remotePauseIds.isEmpty }
    var canMove: Bool { !moveIds.isEmpty }
    var canDelete: Bool { hasLocal || !remoteDeleteIds.isEmpty }
    var canExport: Bool { hasLocal || hasRemote }
    /// 选中里是否有本机任务（删除沿用既有路径，不看状态）。
    var hasLocal = false
    var hasRemote = false
    var resumeIds: [String] = []
    var pauseIds: [String] = []
    var moveIds: [String] = []
    var remoteResumeIds: [String] = []
    var remotePauseIds: [String] = []
    var remoteDeleteIds: [String] = []
}

/// 下载页的视图状态：筛选 / 折叠 / 多选 + 派生列表与分面（同 Android `DownloadsView`）。
///
/// 通过 `withObservationTracking` 观察主机状态与排序偏好：任一变化 → 在主 actor 上重新派生并只在结果不同时发布。
/// 进度 / 速度排序键的重排节流（2s）与指针活动静默期（1.5s）由 `DownloadsDeriver` 处理，到期自动补一次派生。
@MainActor
@Observable
final class DownloadsModel {
    private enum Key {
        static let collapsed = "ui.downloads.collapsed_groups"
    }

    var filter = DownloadsFilter()
    /// regular 分栏右栏正在显示的任务组详情（D6）；非 nil 时优先于 `router.selectedTaskId`。
    var detailGroupId: String?
    private(set) var list = DownloadsList.initial
    private(set) var facets = Facets.initial
    private(set) var chrome = DownloadsChrome()
    /// 排队顺序：可见排队行按稳定键取名次（`QueueOrder.rank`），顺序变化不重建行模型。
    private(set) var queueOrder = QueueOrder.empty
    private(set) var collapsed: Set<String>
    private(set) var selectionCaps = SelectionCaps()

    /// 多选模式；退出时清空选择。
    var isSelecting = false {
        didSet { if !isSelecting { selection = [] } }
    }

    var selection: Set<String> = [] {
        didSet { updateCaps() }
    }

    @ObservationIgnored private var store: HostStore?
    @ObservationIgnored private var prefs: ViewPrefsStore?
    @ObservationIgnored private var deriver = DownloadsDeriver()
    @ObservationIgnored private var lastInteractionMs: Int64 = 0
    @ObservationIgnored private var retryTask: Task<Void, Never>?
    @ObservationIgnored private let defaults: UserDefaults
    /// 远程任务 / 会话分区的缓存解码（只在分区字节变化时重新解码）。
    @ObservationIgnored private let agentSections = AgentSections()

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        collapsed = Set(defaults.stringArray(forKey: Key.collapsed) ?? [])
    }

    // MARK: 绑定与派生

    /// 绑定数据源并开始观察（幂等）。
    func bind(store: HostStore, prefs: ViewPrefsStore) {
        guard self.store == nil else { return }
        self.store = store
        self.prefs = prefs
        recompute()
        track()
    }

    private func track() {
        guard let store, let prefs else { return }
        withObservationTracking {
            _ = store.state
            _ = prefs.groupBy
            _ = prefs.sortKey
            _ = prefs.ascending
            _ = filter
            _ = collapsed
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.recompute()
                self.track()
            }
        }
    }

    private static func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

    func recompute() {
        guard let store, let prefs else { return }
        let state = store.state

        // 选中的队列被删除 → 回退「全部」（对齐 PC）。
        if let queueId = filter.queueId, !state.queues.isEmpty,
           !state.queues.contains(where: { normalizedQueueId($0.queueId) == queueId }) {
            filter.queueId = nil
        }

        let now = Self.nowMs()
        let result = deriver.derive(
            DeriveInput(
                state: state,
                order: ViewOrder(groupBy: prefs.groupBy, sortKey: prefs.sortKey, ascending: prefs.ascending),
                filter: filter,
                collapsed: collapsed,
                remote: remoteTasks(state)
            ),
            nowMs: now,
            interactionMs: lastInteractionMs
        )
        if result.list != list { list = result.list }
        if result.facets != facets { facets = result.facets }
        if state.queueOrder != queueOrder { queueOrder = state.queueOrder }

        let nextChrome = Self.chrome(for: state)
        if nextChrome != chrome { chrome = nextChrome }

        // 任务表分页载入中不能据空表裁剪选择（「已不存在」是删除推断，半份列表不成立）；载入完成再裁。
        if !selection.isEmpty {
            // 远程行随云端投影增删，不受本机任务表分页载入影响；本机任务只在载入完成后裁剪。
            var kept = selection
            let remoteIds = Set(remoteTasks(state).map { DownloadsRow.remoteIdPrefix + $0.id })
            kept = kept.filter { !$0.hasPrefix(DownloadsRow.remoteIdPrefix) || remoteIds.contains($0) }
            if !state.tasksLoading {
                let localIds = Set(state.tasks.lazy.map(\.taskId))
                kept = kept.filter { $0.hasPrefix(DownloadsRow.remoteIdPrefix) || localIds.contains($0) }
            }
            if kept != selection { selection = kept } else { updateCaps() }
        }

        retryTask?.cancel()
        retryTask = nil
        if result.retryAtMs > 0 {
            let delay = max(result.retryAtMs - now, 50)
            retryTask = Task { [weak self] in
                try? await Task.sleep(for: .milliseconds(delay))
                guard !Task.isCancelled else { return }
                self?.recompute()
            }
        }
    }

    /// 其他设备上执行的远程任务（同 PC 下载页的远程行）：主机支持远程任务时才有；目标为本机的云端镜像
    /// 不显示（本地已有真实任务）。本机 id 取会话设备，其次名册里的 `isCurrent`。
    private func remoteTasks(_ state: HostState) -> [RemoteTaskDto] {
        guard state.has(HostCapability.agentRemoteTasks) else { return [] }
        let current = agentSections.session(state)?.device.deviceId
            ?? state.cloudDevices.first(where: \.isCurrent)?.deviceId
        return RemoteTaskRules.visible(agentSections.remoteTasks(state), currentDeviceId: current)
    }

    private static func chrome(for state: HostState) -> DownloadsChrome {
        let link: DownloadsChrome.Link = switch state.connection {
        case .connecting: .connecting
        case .live: .live
        case .stale: .stale
        case .failed: .failed
        }
        let pending = PendingSelection.make(state.selections) { state.task($0)?.fileName }
        return DownloadsChrome(link: link, diskFree: state.stats.diskFreeBytes, pendingSelection: pending)
    }

    /// 手指 / 指针活动：进度 / 速度排序键延后重排（§3.7）。
    func touch() {
        lastInteractionMs = Self.nowMs()
    }

    // MARK: 筛选

    func setFolder(_ folder: StatusFolder) {
        guard filter.folder != folder || filter.categoryId != nil || filter.remoteOnly else { return }
        filter.folder = folder
        filter.categoryId = nil
        filter.remoteOnly = false
    }

    /// 选分类清「远程任务」（二者互斥）；nil 仅清分类。
    func setCategory(_ id: String?) {
        filter.categoryId = id
        if id != nil { filter.remoteOnly = false }
    }

    /// 「远程任务」芯片：开 = 只看远程任务并清分类（二者互斥）。
    func setRemoteOnly(_ on: Bool) {
        filter.remoteOnly = on
        if on { filter.categoryId = nil }
    }

    /// nil 清除队列范围。
    func setQueue(_ id: String?) {
        filter.queueId = id.map(normalizedQueueId)
    }

    func clearFilters() {
        filter = DownloadsFilter()
    }

    func toggleGroup(_ key: String) {
        if collapsed.contains(key) { collapsed.remove(key) } else { collapsed.insert(key) }
        defaults.set(collapsed.sorted(), forKey: Key.collapsed)
    }

    // MARK: 多选

    func beginSelecting(with id: String? = nil) {
        isSelecting = true
        if let id { selection = [id] }
    }

    var allVisibleSelected: Bool {
        !list.visibleIds.isEmpty && list.visibleIds.allSatisfy { selection.contains($0) }
    }

    /// 全选 ↔ 清空（范围 = 可见任务；全选时再点 = 清空）。
    func toggleSelectAll() {
        selection = allVisibleSelected ? [] : Set(list.visibleIds)
    }

    func selectedTasks() -> [DownloadTask] {
        guard let store, !selection.isEmpty else { return [] }
        return store.state.tasks.filter { selection.contains($0.taskId) }
    }

    /// 选中的远程任务（列表顺序无关；按云端投影顺序）。
    func selectedRemoteTasks() -> [RemoteTaskDto] {
        guard let store, !selection.isEmpty else { return [] }
        return selectedRemoteTasks(store.state)
    }

    private func selectedRemoteTasks(_ state: HostState) -> [RemoteTaskDto] {
        remoteTasks(state).filter { selection.contains(DownloadsRow.remoteIdPrefix + $0.id) }
    }

    /// 命令目标的在线状态：presence 未知 = nil（不据此禁用）；与 `DevicesModel.targetOnline` 同口径（快照里的云设备）。
    private func remoteTargetOnline(_ task: RemoteTaskDto, _ state: HostState) -> Bool? {
        guard CloudPresence.isKnown(agentSections.connection(state), localReady: state.connection == .live) else { return nil }
        return state.cloudDevices.first { $0.deviceId == task.toDevice }?.isOnline
    }

    private func updateCaps() {
        guard let store, !selection.isEmpty else {
            if selectionCaps != SelectionCaps() { selectionCaps = SelectionCaps() }
            return
        }
        var caps = SelectionCaps()
        for task in store.state.tasks where selection.contains(task.taskId) {
            caps.hasLocal = true
            switch task.status {
            case .paused, .failed: caps.resumeIds.append(task.taskId)
            case .downloading, .pending, .preparing: caps.pauseIds.append(task.taskId)
            case .completed:
                if task.presentsSeeding {
                    caps.pauseIds.append(task.taskId)
                } else if task.presentsResumeSeeding {
                    caps.resumeIds.append(task.taskId)
                }
            case .unknown: break
            }
            if task.status != .completed, task.status != .unknown { caps.moveIds.append(task.taskId) }
        }
        let state = store.state
        let remote = selectedRemoteTasks(state)
        caps.hasRemote = !remote.isEmpty
        if !remote.isEmpty {
            let online: (RemoteTaskDto) -> Bool? = { [self] in remoteTargetOnline($0, state) }
            caps.remoteResumeIds = RemoteBatch.eligibleIds(.resume, in: remote, targetOnline: online)
            caps.remotePauseIds = RemoteBatch.eligibleIds(.pause, in: remote, targetOnline: online)
            caps.remoteDeleteIds = RemoteBatch.eligibleIds(.delete, in: remote, targetOnline: online)
        }
        if caps != selectionCaps { selectionCaps = caps }
    }
}
