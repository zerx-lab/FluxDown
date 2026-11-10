import FluxDomain
import FluxUI
import SwiftUI
import UIKit

/// 需要确认 / 输入的任务对话框（由根视图的 `.taskActionDialogs()` 统一呈现）。
/// 删除确认不在此列：它由触发视图挂载的 alert 承担（见 `taskDeleteHost()`）。
nonisolated enum TaskDialog: Identifiable {
    case redownload(DownloadTask)
    case rename(DownloadTask)
    case changeUrl(DownloadTask)

    var id: String {
        switch self {
        case let .redownload(task): "redownload:" + task.taskId
        case let .rename(task): "rename:" + task.taskId
        case let .changeUrl(task): "url:" + task.taskId
        }
    }
}

/// 任务动作的唯一分发点（同 Android `TaskActions`）：行圆环、滑动、上下文菜单、多选工具栏、详情页都经此调用，
/// 保证只读拦截、错误文案、Toast 一致；菜单顺序沿用 PC `MenuEntry`（见 `TaskMenuItems`）。
@MainActor
@Observable
final class TaskActions {
    /// 当前待呈现的确认 / 输入对话框。
    var dialog: TaskDialog?
    /// 快速查看（本机已完成文件）。
    var previewURL: URL?

    @ObservationIgnored private unowned let container: AppContainer

    init(container: AppContainer) {
        self.container = container
    }

    private var store: HostStore { container.store }
    private var toasts: ToastCenter { container.toasts }

    // MARK: 只读拦截与执行

    /// 只读（断连宽限后）拒绝写操作。
    @discardableResult
    func guardWritable() -> Bool {
        guard store.state.isReadOnly else { return true }
        toasts.show(text: L("localServiceDisconnected"), tone: .error, systemImage: FluxSymbol.offline)
        return false
    }

    /// 执行一个主机命令：只读拦截 → 调用 → 成功回调 / 失败 Toast。
    func run(
        onSuccess: (@MainActor () -> Void)? = nil,
        _ body: @escaping @MainActor (any HostSession) async throws(HostError) -> Void
    ) {
        guard guardWritable() else { return }
        let session = container.session
        Task {
            do throws(HostError) {
                try await body(session)
                onSuccess?()
            } catch {
                toasts.show(text: ErrorText.describe(error), tone: .error, systemImage: "exclamationmark.circle")
            }
        }
    }

    // MARK: 单任务 / 批量

    func pause(_ ids: [String]) {
        run(onSuccess: { [toasts] in
            if ids.count > 1 { toasts.show(text: L("mobileToastPausedN", ["n": ids.count]), tone: .info, systemImage: FluxSymbol.pause) }
        }) { session throws(HostError) in
            if ids.count == 1 { try await session.pause(ids[0]) } else { try await session.pauseMany(ids) }
        }
    }

    func resume(_ ids: [String]) {
        run(onSuccess: { [toasts] in
            if ids.count > 1 { toasts.show(text: L("mobileToastResumedN", ["n": ids.count]), tone: .info, systemImage: FluxSymbol.resume) }
        }) { session throws(HostError) in
            if ids.count == 1 { try await session.resume(ids[0]) } else { try await session.resumeMany(ids) }
        }
    }

    func pauseAll() {
        run(onSuccess: { [toasts] in toasts.show(text: L("mobilePausedAllToast"), tone: .info, systemImage: FluxSymbol.pause) }) { session throws(HostError) in
            try await session.pauseAll()
        }
    }

    func resumeAll() {
        run(onSuccess: { [toasts] in toasts.show(text: L("mobileResumedAllToast"), tone: .info, systemImage: FluxSymbol.resume) }) { session throws(HostError) in
            try await session.resumeAll()
        }
    }

    /// 优先下载（Boost）：主机侧切换；`boosted` = 当前是否已是优先任务。
    func boost(_ task: DownloadTask, boosted: Bool) {
        run(onSuccess: { [toasts] in
            toasts.show(text: L(boosted ? "mobileBoostOff" : "mobileBoostOn"), tone: .info, systemImage: FluxSymbol.boost)
        }) { session throws(HostError) in
            try await session.boost(task.taskId)
        }
    }

    func moveToQueue(_ ids: [String]) {
        guard guardWritable() else { return }
        container.router.sheet = .moveToQueue(ids)
    }

    /// 重新打开「文件已存在」对话框（任务行「待确认」角标）。
    func openFileConflicts() {
        container.router.sheet = .fileConflicts
    }

    func moveToQueueNow(_ ids: [String], queueId: String, queueName: String) {
        run(onSuccess: { [toasts] in
            toasts.show(text: L("mobileMovedToQueueNamed", ["name": queueName]), tone: .success, systemImage: FluxSymbol.done)
        }) { session throws(HostError) in
            for id in ids { try await session.moveToQueue(id, queueId: queueId) }
        }
    }

    // MARK: 删除 / 重新下载

    /// 构造删除确认请求（只读拦截 + 空集过滤）；由触发视图的 `taskDeleteDialog` 呈现。
    func deleteRequest(_ tasks: [DownloadTask], onDone: (@MainActor () -> Void)? = nil) -> TaskDeleteRequest? {
        guard !tasks.isEmpty, guardWritable() else { return nil }
        return TaskDeleteRequest(tasks: tasks, onDone: onDone)
    }

    func delete(_ ids: [String], withFiles: Bool, onDone: (@MainActor () -> Void)? = nil) {
        run(onSuccess: { [toasts] in
            let text = ids.count > 1
                ? L("mobileToastDeletedN", ["n": ids.count])
                : L(withFiles ? "mobileTaskFileDeleted" : "mobileTaskDeleted")
            toasts.show(text: text, tone: .success, systemImage: FluxSymbol.delete)
            onDone?()
        }) { session throws(HostError) in
            if ids.count == 1 {
                try await session.delete(ids[0], deleteFiles: withFiles)
            } else {
                try await session.deleteMany(ids, deleteFiles: withFiles)
            }
        }
    }

    /// 可重新下载：已完成 / 失败，且 url 不是 `torrent-file://` 哨兵（GPUI `redownload_commands` 同规则）；
    /// store 构建不含 P2P，magnet / ed2k 任务也不可重新下载。
    func canRedownload(_ task: DownloadTask) -> Bool {
        guard task.status == .completed || task.status == .failed, !task.url.hasPrefix("torrent-file://") else { return false }
        #if FULL_FLAVOR
        return true
        #else
        return !P2PLink.isP2P(task.url)
        #endif
    }

    func confirmRedownload(_ task: DownloadTask) {
        guard guardWritable() else { return }
        guard canRedownload(task) else {
            toasts.show(text: L("mobileFileNotFound"), tone: .warning, systemImage: "exclamationmark.triangle")
            return
        }
        dialog = .redownload(task)
    }

    /// 删除任务与已有文件后按原参数重建（GPUI `DownloadsCommand::Redownload` 同语义）。
    func redownload(_ task: DownloadTask) {
        run(onSuccess: { [toasts] in
            toasts.show(text: L("mobileDownloadStarted"), tone: .success, systemImage: FluxSymbol.retry)
        }) { session throws(HostError) in
            try await session.delete(task.taskId, deleteFiles: true)
            _ = try await session.createTask(CreateTaskRequest(
                url: task.url,
                fileName: task.fileName,
                saveDir: task.saveDir,
                queueId: task.queueId
            ))
        }
    }

    // MARK: 重命名 / 更换下载源

    func rename(_ task: DownloadTask) {
        guard guardWritable() else { return }
        dialog = .rename(task)
    }

    func submitRename(_ task: DownloadTask, to name: String) {
        let value = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, value != task.fileName else { return }
        run(onSuccess: { [toasts] in toasts.show(text: L("renameTaskSuccess"), tone: .success, systemImage: FluxSymbol.edit) }) { session throws(HostError) in
            try await session.rename(task.taskId, fileName: value)
        }
    }

    func changeUrl(_ task: DownloadTask) {
        guard guardWritable() else { return }
        dialog = .changeUrl(task)
    }

    func submitChangeUrl(_ task: DownloadTask, to url: String) {
        let value = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, value != task.shareUrl else { return }
        run(onSuccess: { [toasts] in toasts.show(text: L("mobileChangeUrlDone"), tone: .success, systemImage: FluxSymbol.link) }) { session throws(HostError) in
            try await session.changeUrl(task.taskId, url: value)
        }
    }

    /// 每个任务一次 `daemon.task.setPiecePicking`，只带被切换的字段。
    /// 单任务失败不中断其余任务；有失败时最后走统一错误 Toast 一次。
    func setPiecePicking(_ params: [SetTaskPiecePickingParams]) {
        guard !params.isEmpty else { return }
        run { session throws(HostError) in
            var first: HostError?
            for item in params {
                do throws(HostError) {
                    try await session.callVoid(HostMethod.daemonTaskSetPiecePicking, params: item)
                } catch {
                    if first == nil { first = error }
                }
            }
            if let first { throw first }
        }
    }

    // MARK: 本地动作（不经主机）

    func copyLink(_ task: DownloadTask) {
        UIPasteboard.general.string = task.shareUrl
        toasts.show(text: L("urlCopied"), tone: .success, systemImage: FluxSymbol.copy)
    }

    /// 本机已完成且文件仍在。远端主机的文件不在本机，不提供打开。
    func hasLocalFile(_ task: DownloadTask) -> Bool {
        container.isLocalHost && task.status == .completed && !task.fileMissing
    }

    /// 打开已完成文件（快速查看）；不存在 → 警告。
    func open(_ task: DownloadTask) {
        let url = LocalPaths.fileURL(saveDir: task.saveDir, fileName: task.fileName)
        guard hasLocalFile(task), FileManager.default.fileExists(atPath: url.path) else {
            toasts.show(text: L("mobileFileNotFound"), tone: .warning, systemImage: "exclamationmark.triangle")
            return
        }
        previewURL = url
    }

    /// 在「文件」App 中显示保存目录。
    func showInFiles(_ task: DownloadTask) {
        guard hasLocalFile(task), let url = LocalPaths.filesAppURL(forDirectory: task.saveDir) else {
            toasts.show(text: L("mobileFileNotFound"), tone: .warning, systemImage: "exclamationmark.triangle")
            return
        }
        UIApplication.shared.open(url)
    }

    /// 行圆环的主动作（按状态）。
    func primary(_ task: DownloadTask) {
        switch task.status {
        case .completed where task.fileMissing: confirmRedownload(task)
        case .completed where container.isLocalHost: open(task)
        case .completed: container.router.showTask(task.taskId)
        case .paused, .failed: resume([task.taskId])
        case .unknown: break
        default: pause([task.taskId])
        }
    }
}
