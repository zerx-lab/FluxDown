import FluxDomain
import FluxUI
import QuickLook
import SwiftUI

/// 任务上下文菜单 / 详情「更多」菜单的内容，顺序沿用 PC `MenuEntry`（同 Android `TaskActions.menuItems`）：
/// 打开 / 继续·暂停 / 优先 ─ 重命名 / 更换下载源 ─（未完成 BT）选片 ─ 重新下载 / 复制链接 / 分享 / 移动到队列 / 选择 ─ 删除。
struct TaskMenuItems: View {
    let task: DownloadTask
    /// 当前是否为优先任务（`HostState.priorityTaskId`）。
    let boosted: Bool
    /// 列表多选入口（详情页不传）。
    var onSelect: (() -> Void)?

    @Environment(AppContainer.self) private var container
    @Environment(TaskActions.self) private var actions
    @Environment(\.confirmTaskDelete) private var confirmDelete

    var body: some View {
        let status = task.status
        Section {
            if actions.hasLocalFile(task) {
                Button(L("openFile"), systemImage: FluxSymbol.openFile) { actions.open(task) }
                Button(L("mobileShowInFiles"), systemImage: FluxSymbol.folder) { actions.showInFiles(task) }
            }
            if status == .paused || status == .failed || task.presentsResumeSeeding {
                Button(L("resume"), systemImage: FluxSymbol.resume) { actions.resume([task.taskId]) }
            }
            if status.isActive || status == .pending || task.presentsSeeding {
                Button(L("pause"), systemImage: FluxSymbol.pause) { actions.pause([task.taskId]) }
            }
            // 只有未结束的任务能优先下载（主机对已完成 / 失败任务拒绝 Boost）。
            if status != .completed && status != .failed {
                Toggle(isOn: Binding(get: { boosted }, set: { _ in actions.boost(task, boosted: boosted) })) {
                    Label(L(boosted ? "cancelBoost" : "boostDownload"), systemImage: FluxSymbol.boost)
                }
            }
        }
        if showsEditItems {
            Section {
                if !status.isActive, task.protocol != .bt {
                    Button(L("renameTask"), systemImage: FluxSymbol.edit) { actions.rename(task) }
                }
                if status == .failed || status == .paused {
                    Button(L("mobileChangeUrl"), systemImage: FluxSymbol.link) { actions.changeUrl(task) }
                }
            }
        }
        piecePickingSection
        Section {
            if actions.canRedownload(task) {
                Button(L("redownloadTask"), systemImage: FluxSymbol.retry) { actions.confirmRedownload(task) }
            }
            Button(L("copyUrl"), systemImage: FluxSymbol.copy) { actions.copyLink(task) }
            ShareLink(item: task.shareUrl) { Label(L("mobileShareLink"), systemImage: FluxSymbol.share) }
            if status != .completed {
                Button(L("moveToQueueAction"), systemImage: FluxSymbol.queue) { actions.moveToQueue([task.taskId]) }
            }
            if let onSelect {
                Button(L("mobileMenuSelect"), systemImage: "checkmark.circle") { onSelect() }
            }
        }
        Section {
            Button(L("delete"), systemImage: FluxSymbol.delete, role: .destructive) { confirmDelete?.confirm([task]) }
        }
    }

    /// 重命名 / 换链都没有时不留空段（与 Android 编辑项分隔线同条件）。
    private var showsEditItems: Bool {
        let status = task.status
        return (!status.isActive && task.protocol != .bt) || status == .failed || status == .paused
    }

    #if FULL_FLAVOR
    @ViewBuilder
    private var piecePickingSection: some View {
        let facts = PiecePickingMenu.facts(tasks: [task])
        if facts.shown {
            Section {
                PiecePickingMenuItems(facts: facts)
            }
        }
    }
    #else
    private var piecePickingSection: some View { EmptyView() }
    #endif
}

#if FULL_FLAVOR
/// 「选片方式」子菜单：最稀有优先 / 按顺序下载（单选）+ 先下载首尾文件块（独立勾选）。
/// 单选态来自 `PiecePickingMenu.order`（混选两项都不点亮）；关态也画空圆 / 空框。
/// 点顺序只发状态不同的任务的 `sequential`；点首尾只发该字段。
struct PiecePickingMenuItems: View {
    let facts: PiecePickingMenu

    @Environment(TaskActions.self) private var actions

    var body: some View {
        Menu(L("btPiecePicking"), systemImage: "square.grid.3x3") {
            Button { actions.setPiecePicking(facts.settingSequential(false)) } label: {
                Label(L("btRarestFirst"), systemImage: facts.order == .rarest ? "largecircle.fill.circle" : "circle")
            }
            Button { actions.setPiecePicking(facts.settingSequential(true)) } label: {
                Label(L("btSequentialDownload"), systemImage: facts.order == .sequential ? "largecircle.fill.circle" : "circle")
            }
            Button { actions.setPiecePicking(facts.togglingFirstLast()) } label: {
                Label(L("btFirstLastPiecesFirst"), systemImage: facts.firstLastOn ? "checkmark.square" : "square")
            }
        }
    }
}
#endif

// MARK: - 删除确认

/// 待确认的任务删除：保留文件 / 连同文件（PC 同两档）。
nonisolated struct TaskDeleteRequest {
    let tasks: [DownloadTask]
    let onDone: (@MainActor () -> Void)?
}

/// 触发视图（行 / 菜单）向最近的 `taskDeleteHost()` 申请删除确认。
struct TaskDeleteAction {
    let request: @MainActor ([DownloadTask], (@MainActor () -> Void)?) -> Void

    func confirm(_ tasks: [DownloadTask], onDone: (@MainActor () -> Void)? = nil) {
        request(tasks, onDone)
    }
}

extension EnvironmentValues {
    @Entry var confirmTaskDelete: TaskDeleteAction?
}

extension View {
    /// 根视图挂载：呈现 `TaskActions.dialog`（重新下载确认、重命名 / 更换下载源输入）与快速查看。
    func taskActionDialogs(_ actions: TaskActions) -> some View {
        modifier(TaskActionDialogs(actions: actions))
    }

    /// 任务删除确认 alert 挂在本视图上：
    /// 本视图子树内的 `TaskMenuItems` / 行滑动 / 行无障碍菜单经环境 `confirmTaskDelete` 申请。
    /// 挂在**单个任务行 / 菜单按钮**上。
    func taskDeleteHost() -> some View {
        modifier(TaskDeleteHost())
    }

    /// 直接由持有者驱动的任务删除确认框（工具栏按钮等自带状态的触发点）：`request` 非 nil 时呈现。
    func taskDeleteDialog(_ request: Binding<TaskDeleteRequest?>) -> some View {
        modifier(TaskDeleteDialog(request: request))
    }
}

private struct TaskDeleteHost: ViewModifier {
    @Environment(TaskActions.self) private var actions
    @State private var pending: TaskDeleteRequest?

    func body(content: Content) -> some View {
        content
            .taskDeleteDialog($pending)
            .environment(\.confirmTaskDelete, TaskDeleteAction { tasks, onDone in
                pending = actions.deleteRequest(tasks, onDone: onDone)
            })
    }
}

private struct TaskDeleteDialog: ViewModifier {
    @Binding var request: TaskDeleteRequest?
    @Environment(TaskActions.self) private var actions

    func body(content: Content) -> some View {
        content.alert(
            request.map { $0.tasks.count == 1 ? L("deleteTask") : L("mobileDeleteNTitle", ["n": $0.tasks.count]) } ?? "",
            isPresented: Binding(get: { request != nil }, set: { if !$0 { request = nil } }),
            presenting: request
        ) { pending in
            let ids = pending.tasks.map(\.taskId)
            Button(L("deleteTaskAndFile"), role: .destructive) { actions.delete(ids, withFiles: true, onDone: pending.onDone) }
            Button(L("deleteTask"), role: .destructive) { actions.delete(ids, withFiles: false, onDone: pending.onDone) }
            Button(L("cancel"), role: .cancel) {}
        } message: { pending in
            Text(pending.tasks.count == 1 ? L("deleteConfirmDescKeepFile", ["fileName": pending.tasks[0].fileName]) : L("mobileDeleteNMessage"))
        }
    }
}

private struct TaskActionDialogs: ViewModifier {
    @Bindable var actions: TaskActions
    @State private var text = ""

    private func binding(_ match: @escaping (TaskDialog) -> Bool) -> Binding<Bool> {
        Binding(
            get: { actions.dialog.map(match) ?? false },
            set: { if !$0 { actions.dialog = nil } }
        )
    }

    func body(content: Content) -> some View {
        content
            .alert(
                L("redownloadTask"),
                isPresented: binding { if case .redownload = $0 { return true } else { return false } },
                presenting: actions.dialog
            ) { dialog in
                if case let .redownload(task) = dialog {
                    Button(L("cancel"), role: .cancel) {}
                    Button(L("redownloadTask")) { actions.redownload(task) }
                }
            } message: { dialog in
                if case let .redownload(task) = dialog {
                    Text(L("mobileRedownloadMessage", ["fileName": task.fileName]))
                }
            }
            .alert(
                L("renameTaskTitle"),
                isPresented: binding { if case .rename = $0 { return true } else { return false } },
                presenting: actions.dialog
            ) { dialog in
                if case let .rename(task) = dialog {
                    TextField(L("renameTaskPlaceholder"), text: $text)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button(L("cancel"), role: .cancel) {}
                    Button(L("confirm")) { actions.submitRename(task, to: text) }
                }
            }
            .alert(
                L("mobileChangeUrl"),
                isPresented: binding { if case .changeUrl = $0 { return true } else { return false } },
                presenting: actions.dialog
            ) { dialog in
                if case let .changeUrl(task) = dialog {
                    TextField(L("urlPlaceholder"), text: $text)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button(L("cancel"), role: .cancel) {}
                    Button(L("confirm")) { actions.submitChangeUrl(task, to: text) }
                }
            }
            .onChange(of: actions.dialog?.id) {
                switch actions.dialog {
                case let .rename(task)?: text = task.fileName
                case let .changeUrl(task)?: text = task.shareUrl
                default: break
                }
            }
            .quickLookPreview($actions.previewURL)
    }
}
