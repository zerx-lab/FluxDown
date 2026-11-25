import FluxDomain
import FluxUI
import Foundation
import Observation
import UIKit
import UserNotifications
import os

// MARK: - 纯逻辑：状态迁移 → 通知事件

/// 一次需要通知的任务状态迁移。
nonisolated struct NotificationEvent: Equatable, Sendable {
    nonisolated enum Kind: Sendable { case completed, failed }

    var kind: Kind
    var taskId: String
    var fileName: String
    var errorMessage: String
}

/// 一次 `ingest` 的结果。
nonisolated struct NotificationDelta: Equatable, Sendable {
    var events: [NotificationEvent] = []
    /// 本轮新出现的选择请求 id。
    var newSelections: [String] = []
    /// 本轮消失（已被解决 / 过期）的选择请求 id。
    var resolvedSelections: [String] = []

    var isEmpty: Bool { events.isEmpty && newSelections.isEmpty && resolvedSelections.isEmpty }
}

/// 逐主机跟踪任务状态，产出「已完成 / 已失败」迁移（纯值类型，可单测）。
///
/// 规则：
/// - 每台主机的**首个已连上（live）快照只播种、不通知**：附着时已完成 / 已失败的任务永远不会被当作新事件；
/// - 主机变更、断线 / 重连（`isLive == false` 期间）都会重置：重新同步后的整表同样静默播种；
/// - 播种后新出现、且一出现就处于终态的任务（体积极小的任务在一个发布帧内完成）仅当 `completedAt` / `createdAt`
///   落在 `freshWindow` 内才通知，否则视为同步带来的历史任务；
/// - `errorMessage == "deleted"` 的失败是删除任务的标记，不通知；
/// - 同一状态不重复通知；任务重新下载后再次完成会再通知。
nonisolated struct NotificationTracker: Equatable, Sendable {
    /// 新出现即终态任务被视为「刚发生」的时间窗（秒）。
    static let freshWindow: Int64 = 30

    private var hostID: String?
    private var statuses: [String: TaskStatus] = [:]
    private var selections: Set<String> = []
    private var isSeeded = false

    init() {}

    mutating func reset() {
        hostID = nil
        statuses = [:]
        selections = []
        isSeeded = false
    }

    mutating func ingest(
        hostID: String, isLive: Bool, tasks: [DownloadTask], selections requests: [SelectionRequest], now: Int64
    ) -> NotificationDelta {
        if self.hostID != hostID {
            reset()
            self.hostID = hostID
        }
        guard isLive else {
            // 未连上 / 重连中：丢弃基线，恢复后重新静默播种。
            statuses = [:]
            selections = []
            isSeeded = false
            return NotificationDelta()
        }
        guard isSeeded else {
            statuses.reserveCapacity(tasks.count)
            for task in tasks { statuses[task.taskId] = task.status }
            selections = Set(requests.map(\.requestId))
            isSeeded = true
            return NotificationDelta()
        }

        var delta = NotificationDelta()
        var seen = 0
        for task in tasks {
            seen += 1
            let previous = statuses[task.taskId]
            guard previous != task.status else { continue }
            statuses[task.taskId] = task.status
            switch task.status {
            case .completed:
                if previous == nil, !Self.isFresh(task.completedAt, now: now) { continue }
                delta.events.append(NotificationEvent(
                    kind: .completed, taskId: task.taskId, fileName: task.fileName, errorMessage: ""
                ))
            case .failed:
                guard task.errorMessage != "deleted" else { continue }
                if previous == nil, !Self.isFresh(task.createdAt, now: now) { continue }
                delta.events.append(NotificationEvent(
                    kind: .failed, taskId: task.taskId, fileName: task.fileName, errorMessage: task.errorMessage
                ))
            default:
                break
            }
        }
        if statuses.count != seen {
            // 有任务被删除：只在数量不符时重建索引，常态路径不分配。
            let alive = Set(tasks.map(\.taskId))
            statuses = statuses.filter { alive.contains($0.key) }
        }

        let current = Set(requests.map(\.requestId))
        if current != selections {
            delta.newSelections = requests.map(\.requestId).filter { !selections.contains($0) }
            delta.resolvedSelections = selections.subtracting(current).sorted()
            selections = current
        }
        return delta
    }

    private static func isFresh(_ unixSeconds: Int64, now: Int64) -> Bool {
        unixSeconds > 0 && now - unixSeconds <= freshWindow && now - unixSeconds >= -freshWindow
    }
}

/// 一批事件合并后的通知项。
nonisolated enum NotificationItem: Equatable, Sendable {
    case single(NotificationEvent)
    case summary(kind: NotificationEvent.Kind, count: Int, names: [String])
}

/// 去抖合批：窗口内同类事件 ≥ `summaryThreshold` 条合并为一条汇总，否则逐条通知。
nonisolated enum NotificationBatch {
    /// 去抖窗口（自批内第一条事件起算）。
    static let window: Duration = .seconds(2)
    static let summaryThreshold = 3
    /// 汇总里最多列出的文件名数。
    static let summaryNames = 3

    static func plan(_ events: [NotificationEvent]) -> [NotificationItem] {
        var items: [NotificationItem] = []
        for kind in [NotificationEvent.Kind.completed, .failed] {
            let group = events.filter { $0.kind == kind }
            if group.count >= summaryThreshold {
                items.append(.summary(kind: kind, count: group.count, names: group.prefix(summaryNames).map(\.fileName)))
            } else {
                items.append(contentsOf: group.map(NotificationItem.single))
            }
        }
        return items
    }
}

// MARK: - 纯逻辑：通知内容 / 点击意图

/// 通知类别与动作标识。
nonisolated enum NotificationIDs {
    static let completed = "fluxdown.completed"
    static let completedOpen = "fluxdown.completed.open"
    static let completedOpenShare = "fluxdown.completed.openShare"
    static let failed = "fluxdown.failed"
    static let selection = "fluxdown.selection"
    static let fileConflict = "fluxdown.fileConflict"
    static let test = "fluxdown.test"

    static let actionOpen = "fluxdown.action.open"
    static let actionShare = "fluxdown.action.share"
    static let actionUseDefault = "fluxdown.action.useDefault"

    static let keyTask = "task"
    static let keyHost = "host"
    static let keyRequest = "request"
}

/// 待发送的本地通知（已本地化；与 `UNMutableNotificationContent` 一一对应）。
nonisolated struct NotificationSpec: Equatable, Sendable {
    var identifier: String
    var threadID: String
    var title: String
    var body: String
    var category: String
    var userInfo: [String: String]
}

extension NotificationSpec {
    /// 完成 / 失败通知。`actions`：完成通知附「打开」；`canShare`：该任务文件在本机且是普通文件时再附「分享」。
    static func make(
        _ item: NotificationItem, hostID: String, actions: Bool, canShare: (String) -> Bool
    ) -> NotificationSpec {
        switch item {
        case let .single(event):
            let thread = "fluxdown.\(hostID).\(event.taskId)"
            let info = [NotificationIDs.keyTask: event.taskId, NotificationIDs.keyHost: hostID]
            switch event.kind {
            case .completed:
                let category: String
                if !actions {
                    category = NotificationIDs.completed
                } else if canShare(event.taskId) {
                    category = NotificationIDs.completedOpenShare
                } else {
                    category = NotificationIDs.completedOpen
                }
                return NotificationSpec(
                    identifier: "done.\(hostID).\(event.taskId)", threadID: thread,
                    title: L("downloadCompleted"), body: event.fileName, category: category, userInfo: info
                )
            case .failed:
                let reason = TaskErrorMessage.displayText(event.errorMessage).trimmingCharacters(in: .whitespacesAndNewlines)
                return NotificationSpec(
                    identifier: "fail.\(hostID).\(event.taskId)", threadID: thread,
                    title: L("subtitleError"), body: reason.isEmpty ? event.fileName : "\(event.fileName)\n\(reason)",
                    category: NotificationIDs.failed, userInfo: info
                )
            }
        case let .summary(kind, count, names):
            let tail = count > names.count ? "…" : ""
            let body = names.joined(separator: ", ") + tail
            let info = [NotificationIDs.keyHost: hostID]
            switch kind {
            case .completed:
                return NotificationSpec(
                    identifier: "done.\(hostID).summary.\(UUID().uuidString)", threadID: "fluxdown.\(hostID).summary.completed",
                    title: L("mobileNotifCompletedSummary", ["n": count]), body: body,
                    category: NotificationIDs.completed, userInfo: info
                )
            case .failed:
                return NotificationSpec(
                    identifier: "fail.\(hostID).summary.\(UUID().uuidString)", threadID: "fluxdown.\(hostID).summary.failed",
                    title: L("mobileNotifFailedSummary", ["n": count]), body: body,
                    category: NotificationIDs.failed, userInfo: info
                )
            }
        }
    }

    /// 选择请求通知：`fileName` 为空（任务已不在表里）时用通用文案。
    static func selection(requestId: String, taskId: String, fileName: String, hostID: String) -> NotificationSpec {
        NotificationSpec(
            identifier: "sel.\(hostID).\(requestId)", threadID: "fluxdown.\(hostID).\(taskId)",
            title: L("mobileNotifSelectionTitle"),
            body: fileName.isEmpty ? L("mobileNotifSelectionBodyGeneric") : L("mobileNotifSelectionBody", ["name": fileName]),
            category: NotificationIDs.selection,
            userInfo: [
                NotificationIDs.keyRequest: requestId, NotificationIDs.keyTask: taskId, NotificationIDs.keyHost: hostID,
            ]
        )
    }

    /// 文件已存在询问通知：`fileName` 取询问里的文件名（任务不一定还在表里）；动作「自动重命名」= 默认答复。
    static func fileConflict(requestId: String, taskId: String, fileName: String, hostID: String) -> NotificationSpec {
        NotificationSpec(
            identifier: "sel.\(hostID).\(requestId)", threadID: "fluxdown.\(hostID).\(taskId)",
            title: L("fileConflictTitle"),
            body: L("mobileNotifFileConflictBody", ["name": fileName]),
            category: NotificationIDs.fileConflict,
            userInfo: [
                NotificationIDs.keyRequest: requestId, NotificationIDs.keyTask: taskId, NotificationIDs.keyHost: hostID,
            ]
        )
    }
}

/// 用户对通知的响应。
nonisolated enum NotificationIntent: Equatable, Sendable {
    case openDownloads(hostID: String)
    case openTask(taskId: String, hostID: String)
    case openSelection(requestId: String, hostID: String)
    case shareTask(taskId: String, hostID: String)
    case resolveDefault(requestId: String, hostID: String)

    /// 解析点按 / 动作；划掉通知（dismiss）与未知动作返回 nil。
    static func parse(actionIdentifier: String, userInfo: [String: String]) -> NotificationIntent? {
        let task = userInfo[NotificationIDs.keyTask] ?? ""
        let host = userInfo[NotificationIDs.keyHost] ?? ""
        let request = userInfo[NotificationIDs.keyRequest] ?? ""
        switch actionIdentifier {
        case UNNotificationDefaultActionIdentifier, NotificationIDs.actionOpen:
            if !request.isEmpty { return .openSelection(requestId: request, hostID: host) }
            if !task.isEmpty { return .openTask(taskId: task, hostID: host) }
            return .openDownloads(hostID: host)
        case NotificationIDs.actionShare:
            return task.isEmpty ? nil : .shareTask(taskId: task, hostID: host)
        case NotificationIDs.actionUseDefault:
            return request.isEmpty ? nil : .resolveDefault(requestId: request, hostID: host)
        default:
            return nil
        }
    }

    /// 前台展示策略：只有失败 / 选择请求在前台弹横幅（完成已经体现在列表里，前台不重复打扰）。
    static func foregroundPresentation(category: String) -> UNNotificationPresentationOptions {
        switch category {
        case NotificationIDs.failed, NotificationIDs.selection, NotificationIDs.fileConflict, NotificationIDs.test: [.banner, .list, .sound]
        default: []
        }
    }
}

extension UNAuthorizationStatus {
    /// 系统会实际投递通知（含临时 / 安静投递授权）。
    nonisolated var deliversNotifications: Bool {
        switch self {
        case .authorized, .provisional, .ephemeral: true
        case .notDetermined, .denied: false
        @unknown default: false
        }
    }
}

// MARK: - 服务

/// 本机通知服务：观察当前主机的任务状态，在任务完成 / 失败 / 引擎请求选择时发本地通知（`UNUserNotificationCenter`）。
///
/// iOS 内嵌的 agent 不发桌面通知，而本 App 靠 `BGContinuedProcessingTask` 在后台保活，因此由客户端按
/// `HostStore` 状态自己通知。偏好实时读取：`download.notify_on_complete`（agent 偏好，默认开）、
/// `DeviceSettings.notifyOnFailure` / `notifyActions`（设备本地）。
///
/// 授权**懒申请**：启动时只读取状态，从不弹系统授权框；设置页（通知）里显式请求。
/// 只观察**当前主机**——正在查看远端主机时，本机引擎的任务完成不会在这里通知。
/// 代理在 `init` 里设置：冷启动由通知拉起时，响应能在 `attach` 之后补处理。
@MainActor
@Observable
final class NotificationService: NSObject {
    static let shared = NotificationService()

    /// 系统通知授权状态（同步缓存；`attach` / 回到前台 / 申请后刷新）。
    private(set) var authorization: UNAuthorizationStatus = .notDetermined

    @ObservationIgnored private weak var container: AppContainer?
    @ObservationIgnored private var tracker = NotificationTracker()
    @ObservationIgnored private var batch: [NotificationEvent] = []
    @ObservationIgnored private var batchHost: String?
    @ObservationIgnored private var flushTask: Task<Void, Never>?
    @ObservationIgnored private var observers: [any NSObjectProtocol] = []
    @ObservationIgnored private let log = Logger(subsystem: "com.fluxdown.app", category: "notifications")

    private var center: UNUserNotificationCenter { .current() }

    private override init() {
        super.init()
        center.delegate = self
    }

    /// 冷启动接入应用容器；后台通知动作不依赖根视图出现。
    func attach(container: AppContainer) {
        guard self.container == nil else { return }
        self.container = container
        registerCategories()
        observeLifecycle()
        Task { await refreshAuthorization() }
        observeStore()
    }

    // MARK: 授权

    func refreshAuthorization() async {
        let settings = await center.notificationSettings()
        authorization = settings.authorizationStatus
    }

    /// 请求通知授权（提醒 / 声音 / 角标）。返回是否已获得投递权限。
    @discardableResult
    func requestAuthorization() async -> Bool {
        do {
            _ = try await center.requestAuthorization(options: [.alert, .sound, .badge])
        } catch {
            log.error("request authorization failed: \(error.localizedDescription, privacy: .public)")
        }
        await refreshAuthorization()
        return authorization.deliversNotifications
    }

    private func observeLifecycle() {
        let token = NotificationCenter.default.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                Task { await self.refreshAuthorization() }
            }
        }
        observers.append(token)
    }

    // MARK: 类别

    private func registerCategories() {
        let open = UNNotificationAction(identifier: NotificationIDs.actionOpen, title: L("mobileNotifActionOpen"), options: [.foreground])
        let share = UNNotificationAction(identifier: NotificationIDs.actionShare, title: L("mobileNotifActionShare"), options: [.foreground])
        let useDefault = UNNotificationAction(identifier: NotificationIDs.actionUseDefault, title: L("mobileNotifActionUseDefault"), options: [])
        // 文件已存在：默认答复恒为「重命名」，按钮直接写明（同一动作标识，另一类别里用不同标题）。
        let renameDefault = UNNotificationAction(identifier: NotificationIDs.actionUseDefault, title: L("fileExistsRename"), options: [])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: NotificationIDs.completed, actions: [], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: NotificationIDs.completedOpen, actions: [open], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: NotificationIDs.completedOpenShare, actions: [open, share], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: NotificationIDs.failed, actions: [], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: NotificationIDs.selection, actions: [useDefault], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: NotificationIDs.fileConflict, actions: [renameDefault], intentIdentifiers: [], options: []),
        ])
    }

    // MARK: 观察主机状态

    private func observeStore() {
        guard let container else { return }
        // onChange 在变更生效前（willSet）同步触发：下一轮主 actor 再读取并重新登记。
        let (state, hostID) = withObservationTracking {
            (container.store.state, container.host.id)
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.observeStore() }
        }
        process(state: state, hostID: hostID)
    }

    private func process(state: HostState, hostID: String) {
        DiagnosticsSummary.shared.reset(ifHostIsNot: hostID)
        // 任务表分页载入中视同未就绪：空表 / 半份表不是「任务被删除 / 新出现」，丢弃基线，载入完成后静默重新播种。
        let delta = tracker.ingest(
            hostID: hostID, isLive: state.connection == .live && !state.tasksLoading, tasks: state.tasks,
            selections: state.selections, now: Int64(Date().timeIntervalSince1970)
        )
        guard !delta.isEmpty else { return }

        if !delta.resolvedSelections.isEmpty {
            let ids = delta.resolvedSelections.map { "sel.\(hostID).\($0)" }
            center.removeDeliveredNotifications(withIdentifiers: ids)
            center.removePendingNotificationRequests(withIdentifiers: ids)
        }

        if !delta.events.isEmpty {
            let wantsComplete = state.preferences.bool("download.notify_on_complete", default: true)
            let wantsFailure = DeviceSettings.shared.notifyOnFailure
            let wanted = delta.events.filter { $0.kind == .completed ? wantsComplete : wantsFailure }
            enqueue(wanted, hostID: hostID)
        }

        // 选择请求：App 在前台时壳层已自动弹出请求 Sheet，只在不活跃时通知。
        if !delta.newSelections.isEmpty, UIApplication.shared.applicationState != .active {
            for request in state.selections.presentable where delta.newSelections.contains(request.requestId) {
                if let conflict = request.fileConflict {
                    post(NotificationSpec.fileConflict(requestId: request.requestId, taskId: request.taskId, fileName: conflict.fileName, hostID: hostID))
                } else {
                    let name = state.task(request.taskId)?.fileName ?? ""
                    post(NotificationSpec.selection(requestId: request.requestId, taskId: request.taskId, fileName: name, hostID: hostID))
                }
            }
        }
    }

    /// 发送一条测试通知（通知设置页 / 诊断）。前台也会显示横幅；返回是否已提交给系统。
    func sendTest() async -> Bool {
        await refreshAuthorization()
        guard authorization.deliversNotifications else { return false }
        let content = UNMutableNotificationContent()
        content.title = L("doctorTestNotificationTitle")
        content.body = L("doctorTestNotificationBody")
        content.categoryIdentifier = NotificationIDs.test
        content.sound = .default
        do {
            try await center.add(UNNotificationRequest(identifier: "fluxdown.test.\(UUID().uuidString)", content: content, trigger: nil))
            return true
        } catch {
            log.error("test notification failed: \(error.localizedDescription, privacy: .public)")
            return false
        }
    }

    // MARK: 去抖与发送

    private func enqueue(_ events: [NotificationEvent], hostID: String) {
        guard !events.isEmpty else { return }
        if let batchHost, batchHost != hostID { batch = [] }
        batchHost = hostID
        batch.append(contentsOf: events)
        guard flushTask == nil else { return }
        flushTask = Task { [weak self] in
            do {
                try await Task.sleep(for: NotificationBatch.window)
            } catch {
                return
            }
            self?.flush()
        }
    }

    private func flush() {
        flushTask = nil
        let events = batch
        let hostID = batchHost
        batch = []
        batchHost = nil
        guard let hostID, !events.isEmpty, container?.host.id == hostID else { return }
        let actions = DeviceSettings.shared.notifyActions
        let specs = NotificationBatch.plan(events).map { item in
            NotificationSpec.make(item, hostID: hostID, actions: actions) { [weak self] in self?.canShare(taskId: $0) ?? false }
        }
        for spec in specs { post(spec) }
    }

    private func post(_ spec: NotificationSpec) {
        Task {
            await refreshAuthorization()
            guard authorization.deliversNotifications else { return }
            let content = UNMutableNotificationContent()
            content.title = spec.title
            content.body = spec.body
            content.threadIdentifier = spec.threadID
            content.categoryIdentifier = spec.category
            content.userInfo = spec.userInfo
            content.sound = .default
            do {
                try await center.add(UNNotificationRequest(identifier: spec.identifier, content: content, trigger: nil))
            } catch {
                log.error("post notification failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    /// 完成通知能否附「分享」：任务文件在本机且是普通文件（目录 / 远端文件无法分享）。
    private func canShare(taskId: String) -> Bool {
        shareableFile(taskId: taskId) != nil
    }

    private func shareableFile(taskId: String) -> URL? {
        guard let container, container.isLocalHost, let task = container.store.state.task(taskId),
              task.status == .completed, !task.fileName.isEmpty else { return nil }
        let url = LocalPaths.fileURL(saveDir: task.saveDir, fileName: task.fileName)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), !isDirectory.boolValue else { return nil }
        return url
    }

    // MARK: 响应

    fileprivate func receive(_ intent: NotificationIntent) async {
        guard let container else { return }
        // 先让通知接管主机选择，再异步路由；迟到的恢复结果不能抢在路由前接管界面。
        switch intent {
        case let .openDownloads(hostID) where !hostID.isEmpty:
            container.supersedeHostRestore()
        case .openTask, .openSelection, .shareTask:
            container.supersedeHostRestore()
        case .resolveDefault:
            await handle(intent)
            return
        case .openDownloads:
            break
        }
        Task { await handle(intent) }
    }

    private func handle(_ intent: NotificationIntent) async {
        guard let container else { return }
        switch intent {
        case let .openDownloads(hostID):
            guard await ensureHost(hostID) else { return }
            container.router.tab = .downloads
        case let .openTask(taskId, hostID):
            guard await ensureHost(hostID) else { return }
            container.router.showTask(taskId)
        case let .openSelection(requestId, hostID):
            guard await ensureHost(hostID) else { return }
            await waitUntilLive()
            guard let request = container.store.state.selections.first(where: { $0.requestId == requestId }) else {
                container.router.tab = .downloads
                return
            }
            container.router.dismissedSelections.remove(requestId)
            container.router.tab = .downloads
            container.router.sheet = request.fileConflict == nil ? .selection(requestId: requestId) : .fileConflicts
        case let .shareTask(taskId, hostID):
            guard await ensureHost(hostID) else { return }
            await waitUntilLive()
            guard let file = shareableFile(taskId: taskId) else {
                container.router.showTask(taskId)
                return
            }
            await NotifyActivityPresenter.present(items: [file])
        case let .resolveDefault(requestId, hostID):
            guard container.host.id == hostID else { return }
            if container.store.state.connection == .connecting { await waitUntilLive() }
            guard container.host.id == hostID,
                  let request = container.store.state.selections.first(where: { $0.requestId == requestId }) else { return }
            do throws(HostError) {
                try await container.session.resolveSelection(requestId, outcome: request.defaultChoice)
            } catch {
                log.error("resolve default selection failed: \(error.description, privacy: .public)")
                container.toasts.show(text: ErrorText.describe(error), tone: .error)
            }
        }
    }

    /// 通知来自另一台主机：先切换过去（主机已被删除则放弃）。
    private func ensureHost(_ hostID: String) async -> Bool {
        guard let container else { return false }
        guard !hostID.isEmpty, container.host.id != hostID else { return true }
        guard let ref = container.hosts.first(where: { $0.id == hostID }) else { return false }
        switch await container.switchHost(ref, remember: false) {
        case .success: return true
        case .failure: return false
        }
    }

    /// 最多等 5 秒让新会话的首个快照到达（切主机 / 冷启动后）。
    private func waitUntilLive() async {
        guard let container else { return }
        var attempts = 0
        while container.store.state.connection != .live, attempts < 50 {
            attempts += 1
            do {
                try await Task.sleep(for: .milliseconds(100))
            } catch {
                return
            }
        }
    }
}

// MARK: - 通知中心代理

extension NotificationService: UNUserNotificationCenterDelegate {
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        NotificationIntent.foregroundPresentation(category: notification.request.content.categoryIdentifier)
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping @Sendable () -> Void
    ) {
        let info = response.notification.request.content.userInfo.reduce(into: [String: String]()) { result, pair in
            if let key = pair.key as? String, let value = pair.value as? String { result[key] = value }
        }
        let intent = NotificationIntent.parse(actionIdentifier: response.actionIdentifier, userInfo: info)
        // 在主线程应答；后台动作先等写入结果，前台路由不等待连接或分享面板。
        Task { @MainActor in
            if let intent { await NotificationService.shared.receive(intent) }
            completionHandler()
        }
    }
}

// MARK: - 分享面板

/// 以 `UIActivityViewController` 从最上层控制器呈现系统分享面板（通知「分享」动作 / 日志导出共用）。
@MainActor
enum NotifyActivityPresenter {
    /// 呈现并等待面板关闭；无可用窗口（最多等 2 秒场景激活）时返回 false。
    @discardableResult
    static func present(items: [Any]) async -> Bool {
        guard let top = await topViewController() else { return false }
        let controller = UIActivityViewController(activityItems: items, applicationActivities: nil)
        if let popover = controller.popoverPresentationController {
            popover.sourceView = top.view
            popover.sourceRect = CGRect(x: top.view.bounds.midX, y: top.view.bounds.midY, width: 0, height: 0)
            popover.permittedArrowDirections = []
        }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            controller.completionWithItemsHandler = { _, _, _, _ in continuation.resume() }
            top.present(controller, animated: true)
        }
        return true
    }

    private static func topViewController() async -> UIViewController? {
        for _ in 0 ..< 20 {
            if let top = currentTop() { return top }
            do {
                try await Task.sleep(for: .milliseconds(100))
            } catch {
                return nil
            }
        }
        return nil
    }

    private static func currentTop() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        guard let scene = scenes.first(where: { $0.activationState == .foregroundActive }),
              let window = scene.windows.first(where: \.isKeyWindow) ?? scene.windows.first,
              var top = window.rootViewController else { return nil }
        while let presented = top.presentedViewController, !presented.isBeingDismissed { top = presented }
        return top
    }
}
