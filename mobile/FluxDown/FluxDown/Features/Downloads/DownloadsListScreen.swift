import FluxDomain
import FluxUI
import SwiftUI
import UIKit

/// D1 下载列表（02-downloads §3）+ D2 多选（§4）：大标题 + 主机切换 `toolbarTitleMenu`，吸顶筛选区，
/// `List(.insetGrouped)` 行（滑动 / 上下文菜单 / 圆环），系统工具栏的选择 / 视图菜单与底部批量工具栏。
///
/// 玻璃只出现在功能层：范围条（`GlassScopeBar`）与系统工具栏；行、横幅、芯片都是内容层。
struct DownloadsListScreen: View {
    @Bindable var model: DownloadsModel
    /// compact 栈：列表 → 详情的 zoom 共享元素；regular 分栏为 nil。
    var zoom: Namespace.ID?
    /// regular 分栏中右栏当前任务（行高亮）。
    var currentDetailId: String?
    let onOpen: (String) -> Void
    /// 打开任务组详情（D6）：compact 推入栈，regular 切换右栏。
    let onOpenGroup: (String) -> Void

    @Environment(AppContainer.self) private var container
    @Environment(TaskActions.self) private var actions
    @Environment(ToastCenter.self) private var toasts
    @Environment(\.hasProminentNewTab) private var hasProminentNewTab
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.fluxAccent) private var accent

    @State private var searchText = ""
    @State private var clearIds: [String] = []
    @State private var confirmingClear = false
    @State private var pendingGroupDelete: GroupDeleteRequest?
    /// 多选底栏「删除」待确认的任务（确认 alert 由该按钮挂载）。
    @State private var pendingSelectionDelete: TaskDeleteRequest?
    #if FULL_FLAVOR
    /// 待确认「忽略插件重试」的任务（`daemon.plugin.ignoreRetry`）。
    @State private var ignoreRetryTaskId: String?
    #endif

    private var prefs: ViewPrefsStore { container.viewPrefs }

    var body: some View {
        taskList
            .navigationTitle(model.isSelecting ? L("selectedCount", ["n": model.selection.count]) : L("mobileNavDownloads"))
            .navigationSubtitle(model.isSelecting ? "" : hostTitleSubtitle(container.host, status: linkStatus))
            // 大字标题与工具栏同处一行（不再单独占一行大标题区）；多选时退回普通内联标题。
            .toolbarTitleDisplayMode(model.isSelecting ? .inline : .inlineLarge)
            .toolbarTitleMenu {
                if !model.isSelecting { hostMenu }
            }
            // 搜索框默认收起，下拉列表时出现（同「邮件」「备忘录」）；placement 必须恒定，动态切换会在输入中丢焦点。
            .searchable(
                text: $searchText,
                placement: .navigationBarDrawer(displayMode: .automatic),
                prompt: L("searchTasksPlaceholder")
            )
            .toolbar { toolbarContent }
            .toolbarVisibility(model.isSelecting ? .hidden : .automatic, for: .tabBar)
            .suppressesBottomAccessory("downloads.selecting", when: model.isSelecting)
            .safeAreaBar(edge: .top, spacing: 0) {
                DownloadsFilterBar(model: model)
            }
            #if FULL_FLAVOR
            .alert(L("taskIgnorePluginRetryTitle"), isPresented: Binding(
                get: { ignoreRetryTaskId != nil },
                set: { if !$0 { ignoreRetryTaskId = nil } }
            )) {
                Button(L("cancel"), role: .cancel) {}
                Button(L("taskIgnorePluginRetry")) {
                    if let id = ignoreRetryTaskId { ignorePluginRetry(id) }
                }
            } message: {
                Text(L("taskIgnorePluginRetryMsg"))
            }
            #endif
            .alert(L("mobileClearFinishedTitle"), isPresented: $confirmingClear) {
                Button(L("cancel"), role: .cancel) {}
                Button(L("mobileClearFinishedAction", ["n": clearIds.count]), role: .destructive) { clearFinished(clearIds) }
            } message: {
                Text(L("mobileClearFinishedMessage", ["n": clearIds.count]))
            }
            .sensoryFeedback(.selection, trigger: model.selection)
            .sensoryFeedback(.warning, trigger: model.chrome.showsOfflineBanner) { @Sendable _, shown in shown }
            .task(id: searchText) {
                // 搜索防抖 150ms（trim + 小写在派生里做）。
                guard searchText != model.filter.query else { return }
                try? await Task.sleep(for: .milliseconds(150))
                if !Task.isCancelled { model.filter.query = searchText }
            }
            .onChange(of: model.filter.query) { _, query in
                if query.isEmpty, !searchText.isEmpty { searchText = "" }
            }
    }

    // MARK: 列表

    private var taskList: some View {
        let list = model.list
        let style = RowStyle(density: prefs.density, fields: Set(prefs.fields))
        let editing = model.isSelecting
        let loading = !list.loaded || (list.taskTotal == 0 && model.chrome.link == .connecting)
        return List(selection: editing ? $model.selection : nil) {
            bannerRows
            if loading {
                Section {
                    ForEach(0 ..< 6, id: \.self) { _ in DownloadRowPlaceholder() }
                } header: {
                    // 首个快照到达前（连接中）没有进度可报；快照头已到、任务表分页载入中才显示。
                    if !list.loaded, model.chrome.link != .connecting { loadingHeader(total: list.loadingTotal) }
                }
            } else if !list.isEmpty {
                ForEach(list.sections) { section in
                    Section {
                        ForEach(section.rows) { entry in
                            switch entry {
                            case let .task(item):
                                self.row(item, style: style, editing: editing)
                            case let .remote(remote):
                                DownloadsRemoteRow(task: remote.task, readOnly: model.chrome.isReadOnly)
                            }
                        }
                    } header: {
                        sectionHeader(section)
                    }
                }
                Section {} footer: {
                    Text(footerText)
                        .font(.footnote)
                        .monospacedDigit()
                        .frame(maxWidth: .infinity)
                        .multilineTextAlignment(.center)
                }
            }
        }
        .listStyle(.insetGrouped)
        // 占位行 → 首个快照的真实行：随系统 List 行插删动画淡入，而不是硬切。
        .fluxAnimation(.smooth, value: loading)
        .overlay {
            // 空态不放进列表行（行内按钮样式 / 行高会把 CUV 的按钮压成无字胶囊）：由系统布局居中。
            if !loading, list.isEmpty {
                emptyState
            }
        }
        .environment(\.editMode, Binding(
            get: { model.isSelecting ? .active : .inactive },
            set: { model.isSelecting = $0.isEditing }
        ))
        .refreshable { await refresh() }
        .onScrollPhaseChange { _, _ in model.touch() }
    }

    /// 任务表分页载入中的进度文案（含服务端任务总数）；占位行之上。
    private func loadingHeader(total: UInt64) -> some View {
        HStack(spacing: 8) {
            ProgressView()
            Text(total > 0 ? L("mobileTasksLoadingProgress", ["n": total]) : L("mobileTasksLoading"))
        }
        .font(.footnote)
        .foregroundStyle(.secondary)
        .accessibilityElement(children: .combine)
    }

    private var footerText: String {
        var text = L("nTasks", ["n": model.facets.matching])
        if let free = model.chrome.diskFree {
            text += " · " + L("diskSpaceFreeLabel", ["size": Format.bytes(unsigned: free).description])
        }
        return text
    }

    private func row(_ item: TaskItem, style: RowStyle, editing: Bool) -> some View {
        let readOnly = model.chrome.isReadOnly
        let queuePosition = item.visual == .queued ? model.queueOrder.rank(of: item.id) : 0
        return DownloadRowView(
            item: item,
            style: style,
            isLocalHost: container.isLocalHost,
            queuePosition: queuePosition,
            showsRing: !editing,
            zoom: zoom,
            onOpen: editing ? nil : { onOpen(item.id) }
        )
        .equatable()
        .listRowBackground(currentDetailId == item.id ? accent.color.opacity(0.12) : nil)
        .taskRowSwipeActions(item, readOnly: readOnly)
        .contextMenu {
            if !editing {
                TaskMenuItems(task: item.task, boosted: item.boosted, onSelect: { model.beginSelecting(with: item.id) })
                #if FULL_FLAVOR
                if PluginFailure.isIgnorable(status: item.task.status, errorMessage: item.task.errorMessage, groupId: item.task.groupId) {
                    Section {
                        Button(L("taskIgnorePluginRetry"), systemImage: "puzzlepiece.extension") {
                            if actions.guardWritable() { ignoreRetryTaskId = item.id }
                        }
                    }
                }
                #endif
            }
        } preview: {
            DownloadRowView(
                item: item, style: style, isLocalHost: container.isLocalHost, queuePosition: queuePosition, showsRing: false
            )
                .padding(.horizontal)
                .frame(minWidth: 320)
                .environment(actions)
                .environment(\.fluxAccent, accent)
        }
        .taskDeleteHost()
    }

    // MARK: 分区头

    @ViewBuilder
    private func sectionHeader(_ section: DownloadsSection) -> some View {
        switch section.kind {
        case .inFlight:
            HStack(spacing: 8) {
                Text(section.title.resolved)
                Spacer(minLength: 0)
                Text("↓ " + Format.speedOrZero(section.downSpeed).description).foregroundStyle(accent.text)
                Text("· " + L("nTasks", ["n": section.count])).foregroundStyle(.secondary)
            }
            .font(.subheadline.weight(.semibold))
            .monospacedDigit()
            .textCase(nil)
        case .history:
            HStack {
                Text(section.title.resolved)
                Spacer(minLength: 0)
                Text(L("nTasks", ["n": section.count])).monospacedDigit()
            }
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.secondary)
            .textCase(nil)
        case .group:
            groupHeader(section)
        }
    }

    /// 「按任务组」分组下的真实任务组（`group:none` 与其它分组方式不算）。
    private func taskGroupId(of section: DownloadsSection) -> String? {
        prefs.groupBy == .group ? section.taskGroupId : nil
    }

    /// 组头：折叠按钮（占满剩余宽度）+ 真实任务组的尾部控件（点按 → 组详情；长按 → 组动作菜单），
    /// 整个组头同样挂上下文菜单。
    private func groupHeader(_ section: DownloadsSection) -> some View {
        let groupId = taskGroupId(of: section)
        let name = section.title.resolved
        return HStack(spacing: 0) {
            Button {
                withFluxAnimation(.smooth, reduceMotion: reduceMotion) { model.toggleGroup(section.id) }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "chevron.down")
                        .imageScale(.small)
                        .rotationEffect(.degrees(section.collapsed ? -90 : 0))
                        .accessibilityHidden(true)
                    Text(name).lineLimit(1)
                    Spacer(minLength: 0)
                    if let progress = section.doneOfTotal {
                        Text(L("groupDoneOfTotal", ["done": progress.done, "total": progress.total]))
                    } else {
                        Text(section.count, format: .number)
                    }
                }
                .monospacedDigit()
                .frame(minHeight: 44)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityHint(section.collapsed ? L("mobileExpand") : L("mobileCollapse"))
            .accessibilityAddTraits(.isHeader)
            if let groupId {
                Menu {
                    groupMenuItems(groupId, name: name)
                } label: {
                    Image(systemName: "info.circle")
                        .foregroundStyle(accent.color)
                        .frame(minWidth: 44, minHeight: 44)
                        .contentShape(.rect)
                } primaryAction: {
                    onOpenGroup(groupId)
                }
                .accessibilityLabel(L("detail") + " · " + name)
            }
        }
        .font(.subheadline.weight(.semibold))
        .foregroundStyle(.secondary)
        .textCase(nil)
        .contextMenu {
            if let groupId { groupMenuItems(groupId, name: name) }
        }
        .groupDeleteConfirmation($pendingGroupDelete, groupId: groupId)
    }

    /// 组动作：详情 · 全部暂停 / 继续 · 删除（各自确认）。
    @ViewBuilder
    private func groupMenuItems(_ groupId: String, name: String) -> some View {
        let readOnly = model.chrome.isReadOnly
        let commands = GroupCommands(actions: actions, toasts: toasts)
        Button(L("detail"), systemImage: "info.circle") { onOpenGroup(groupId) }
        Divider()
        Button(L("groupPauseAll"), systemImage: FluxSymbol.pause) { commands.pauseAll(groupId) }
            .disabled(readOnly)
        Button(L("groupResumeAll"), systemImage: FluxSymbol.resume) { commands.resumeAll(groupId) }
            .disabled(readOnly)
        Divider()
        Button(L("groupDelete"), systemImage: FluxSymbol.delete, role: .destructive) {
            pendingGroupDelete = GroupDeleteRequest(groupId: groupId, name: name, withFiles: false)
        }
        .disabled(readOnly)
        Button(L("groupDeleteWithFiles"), systemImage: "trash.fill", role: .destructive) {
            pendingGroupDelete = GroupDeleteRequest(groupId: groupId, name: name, withFiles: true)
        }
        .disabled(readOnly)
    }

    // MARK: 横幅

    @ViewBuilder
    private var bannerRows: some View {
        let chrome = model.chrome
        let pending = chrome.pendingSelection.flatMap { request in
            container.router.sheet == request.route ? nil : request
        }
        if chrome.showsOfflineBanner || pending != nil {
            Section {
                if chrome.showsOfflineBanner {
                    Banner(
                        text: L("localServiceDisconnected"),
                        tone: .warning,
                        systemImage: FluxSymbol.offline,
                        slim: true,
                        action: chrome.link == .failed ? BannerAction(title: L("mobileRetry")) { switchHost(container.host, force: true) } : nil
                    )
                    .bannerRow()
                }
                if let pending {
                    selectionBanner(pending).bannerRow()
                }
            }
        }
    }

    /// 待处理选择请求：标题 + 任务名 + 实时倒计时（无截止的请求不显示倒计时）；「选择」→ X 面板（含已被划走的请求）。
    private func selectionBanner(_ request: PendingSelection) -> some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let remaining = max(0, Int((Double(request.deadlineUnixMs) / 1000 - context.date.timeIntervalSince1970).rounded(.up)))
            let heading = request.titleCount.map { L(request.titleKey, ["count": $0]) } ?? L(request.titleKey)
            let title = request.taskName.isEmpty ? heading : heading + " · " + request.taskName
            let countdown = request.deadlineUnixMs == SelectionRequest.noDeadline
                ? "" : "\n" + L(request.countdownKey, ["seconds": remaining])
            Banner(
                text: title + countdown,
                tone: .info,
                systemImage: "checklist",
                action: BannerAction(title: L("mobileMenuSelect")) {
                    container.router.sheet = request.route
                }
            )
            .monospacedDigit()
        }
    }

    // MARK: 空态

    @ViewBuilder
    private var emptyState: some View {
        if model.list.taskTotal == 0 {
            EmptyStateView(
                L("emptyTitle"),
                message: L(BuildFlavor.key(full: "iosEmptyDownloadsSub", store: "iosEmptyDownloadsSubGeneric")),
                systemImage: FluxSymbol.downloads
            ) {
                Button(L("newDownload"), systemImage: FluxSymbol.newDownload) { container.router.openNewDownload() }
            }
        } else if !model.filter.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            ContentUnavailableView.search(text: model.filter.query)
        } else {
            ContentUnavailableView {
                Label(L("mobileFilterEmptyTitle"), systemImage: "line.3.horizontal.decrease.circle")
            } description: {
                Text(L("mobileFilterEmptySub"))
            } actions: {
                Button(L("mobileResetFilter")) { model.clearFilters() }
                    .buttonStyle(.bordered)
            }
        }
    }

    // MARK: 工具栏

    /// 当前主机的连接状态文案（在线 = nil）：标题副行与标题菜单共用。
    private var linkStatus: String? {
        switch model.chrome.link {
        case .live: nil
        case .connecting: L("mobileHostConnConnecting")
        case .stale: L("mobileHostConnStale")
        case .failed: L("mobileHostConnFailed")
        }
    }

    /// 顶栏只放两组：全局搜索 ·（iOS 26 回退的新建 +）视图菜单。「选择」在视图菜单里，也可长按行进入，不单独占按钮。
    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        if !model.isSelecting {
            ToolbarItem(placement: .primaryAction) { GlobalSearchButton() }
            ToolbarSpacer(.fixed, placement: .primaryAction)
            ToolbarItemGroup(placement: .primaryAction) {
                if !hasProminentNewTab {
                    Button(L("newDownload"), systemImage: FluxSymbol.newDownload) { container.router.openNewDownload() }
                }
                DownloadsViewMenu(prefs: prefs, model: model, onClearFinished: confirmClearFinished)
            }
        }
        if model.isSelecting {
            ToolbarItem(placement: .topBarLeading) {
                Button(model.allVisibleSelected ? L("deselectAll") : L("selectAll")) { model.toggleSelectAll() }
                    .keyboardShortcut("a", modifiers: .command)
            }
            ToolbarItem(placement: .confirmationAction) {
                Button(L("mobileViewDone"), role: .confirm) { model.isSelecting = false }
                    .keyboardShortcut(.escape, modifiers: [])
            }
            ToolbarItemGroup(placement: .bottomBar) {
                let caps = model.selectionCaps
                Button(L("resume"), systemImage: FluxSymbol.resume) { actions.resume(caps.resumeIds) }
                    .disabled(!caps.canResume)
                Button(L("pause"), systemImage: FluxSymbol.pause) { actions.pause(caps.pauseIds) }
                    .disabled(!caps.canPause)
                Button(L("moveToQueueAction"), systemImage: FluxSymbol.queue) { actions.moveToQueue(caps.moveIds) }
                    .disabled(!caps.canMove)
                #if FULL_FLAVOR
                let picking = PiecePickingMenu.facts(tasks: model.selectedTasks())
                if picking.shown {
                    Menu(L("moreActions"), systemImage: FluxSymbol.more) {
                        PiecePickingMenuItems(facts: picking)
                    }
                }
                #endif
                Button(L("copyUrl"), systemImage: FluxSymbol.copy) { copyLinks(model.selectedTasks()) }
                    .disabled(model.selection.isEmpty)
            }
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItem(placement: .bottomBar) {
                Button(L("delete"), systemImage: FluxSymbol.delete, role: .destructive) {
                    pendingSelectionDelete = actions.deleteRequest(model.selectedTasks()) { model.isSelecting = false }
                }
                .disabled(model.selection.isEmpty)
                .taskDeleteDialog($pendingSelectionDelete)
            }
        }
    }

    // MARK: 主机切换（toolbarTitleMenu）

    private var hostMenu: some View {
        HostTitleMenuItems(status: linkStatus)
    }

    /// 切换主机（toast：切换中 / 已切换 / 失败原因）；`force` = 重连当前主机。
    private func switchHost(_ ref: HostRef, force: Bool = false) {
        guard !container.isSwitching, force || ref.id != container.host.id else { return }
        let name = ref.localizedName
        toasts.show(text: L("mobileHostSwitching", ["name": name]), tone: .info, systemImage: FluxSymbol.remoteHost)
        Task {
            switch await container.switchHost(ref) {
            case .success:
                toasts.show(text: L("mobileHostSwitched", ["name": name]), tone: .success, systemImage: FluxSymbol.done)
            case let .failure(error):
                toasts.show(
                    text: L("mobileHostSwitchFailed", ["name": name, "reason": ErrorText.describe(error)]),
                    tone: .error,
                    systemImage: FluxSymbol.offline
                )
            }
        }
    }

    // MARK: 动作

    #if FULL_FLAVOR
    /// `daemon.plugin.ignoreRetry`：跳过插件，用原始链接重试（仅插件失败任务，已由菜单条件保证）。
    private func ignorePluginRetry(_ taskId: String) {
        actions.run(onSuccess: { [toasts] in
            toasts.show(text: L("taskIgnorePluginRetryDone"), tone: .success, systemImage: FluxSymbol.done)
        }) { session throws(HostError) in
            try await session.callVoid(HostMethod.daemonPluginIgnoreRetry, params: TaskIdParams(taskId: taskId))
        }
    }
    #endif

    private func refresh() async {
        guard actions.guardWritable() else { return }
        do throws(HostError) {
            try await container.session.rescan()
        } catch {
            toasts.show(text: ErrorText.describe(error), tone: .error, systemImage: "exclamationmark.circle")
        }
    }

    private func copyLinks(_ tasks: [DownloadTask]) {
        guard !tasks.isEmpty else { return }
        UIPasteboard.general.string = tasks.map(\.shareUrl).joined(separator: "\n")
        toasts.show(text: L("urlCopied"), tone: .success, systemImage: FluxSymbol.copy)
    }

    /// 清除已完成任务：确认 → `deleteMany(deleteFiles: false)`（只移除记录，保留文件）。
    private func confirmClearFinished() {
        guard actions.guardWritable() else { return }
        // 任务表还在分页载入：列表只是半份，不能据此判定「没有已完成任务」或批量清除。
        guard !container.store.state.tasksLoading else {
            toasts.show(text: L("mobileTasksLoading"), tone: .info, systemImage: "hourglass")
            return
        }
        let ids = container.store.state.tasks.filter { $0.status == .completed }.map(\.taskId)
        guard !ids.isEmpty else {
            toasts.show(text: L("mobileClearFinishedNone"), tone: .info, systemImage: "checkmark.circle")
            return
        }
        clearIds = ids
        confirmingClear = true
    }

    private func clearFinished(_ ids: [String]) {
        actions.run(onSuccess: { [toasts] in
            toasts.show(text: L("mobileToastClearedFinished", ["n": ids.count]), tone: .success, systemImage: FluxSymbol.delete)
        }) { session throws(HostError) in
            try await session.deleteMany(ids, deleteFiles: false)
        }
    }
}

private extension View {
    /// 横幅行：透明底、无分隔线、零内边距（横幅自带圆角卡片底）。
    func bannerRow() -> some View {
        listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
            .listRowBackground(Color.clear)
            .listRowSeparator(.hidden)
    }
}

/// 其他设备上执行的远程任务行（同 PC 下载页的远程行）：状态 / 进度由云端经 agent 实时推送，
/// 控制（暂停 / 继续 / 取消 / 删除）走 `agent.remote.command`，与设备页共用 `DevicesModel` 的在途状态。
/// 单独成视图：只有它读主机状态（目标设备名 / 在线），列表页 body 不随 10 Hz 状态重算。
private struct DownloadsRemoteRow: View {
    let task: RemoteTaskDto
    let readOnly: Bool

    @Environment(AppContainer.self) private var container

    var body: some View {
        let state = container.store.state
        let devices = container.devices
        let target = devices.target(for: task.toDevice, state: state)
        RemoteTaskRow(
            task: task,
            targetName: target?.name ?? task.toDevice,
            targetOnline: devices.presenceKnown(state) ? target?.online : nil,
            busy: devices.isBusy(task),
            readOnly: readOnly
        ) { action, deleteFiles in
            devices.issue(action, to: task, deleteFiles: deleteFiles)
        }
        // 远程行不参与多选（全选范围 / visibleIds 不含远程）。
        .selectionDisabled()
    }
}
