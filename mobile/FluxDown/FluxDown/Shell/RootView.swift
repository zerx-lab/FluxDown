import FluxDomain
import FluxUI
import SwiftUI

/// 环境值：壳层是否已提供「新建」入口（iOS 27 `Tab(role: .prominent)`）。
/// false 时下载页导航栏自行放置 `+` 按钮（iOS 26 回退，01-foundations §0.4）。
extension EnvironmentValues {
    @Entry var hasProminentNewTab = false
}

/// 顶层壳（02-downloads §2）：`TabView`（下载 / 订阅 / 设备 / 设置 + iOS 27 新建），全局搜索由各根页右上角按钮以 Sheet 呈现，
/// 底部附件 = 全局活动条（页面显示自己的底部工具栏时由 `router.bottomAccessorySuppressors` 压制），
/// 全局 Sheet 路由、任务对话框、Toast、链接唤起与选择请求排队都挂在这里。
struct RootView: View {
    @Environment(AppContainer.self) private var container
    @Environment(\.scenePhase) private var scenePhase
    @State private var actions: TaskActions?
    /// prominent「新建」被选中时回弹到的标签。
    @State private var previousTab: AppTab = .downloads
    /// 最近一次呈现的 Sheet（`onDismiss` 时判断是否为被划走的选择请求）。
    @State private var presented: SheetRoute?
    @State private var externalRequests: [ExternalDownload] = []
    @State private var externalConfirmations: [ExternalDownloadConfirmation] = []
    @State private var processingExternalRequests = false
    @State private var captureSections = AgentSections()

    var body: some View {
        Group {
            if let actions {
                shell
                    .environment(actions)
                    .taskActionDialogs(actions)
            } else {
                Color.clear.onAppear { actions = TaskActions(container: container) }
            }
        }
        .onOpenURL(perform: handle)
    }

    private var shell: some View {
        @Bindable var router = container.router
        let state = container.store.state
        return tabs(selection: $router.tab)
            .bottomAccessory(suppressed: router.isBottomAccessorySuppressed)
            .sheet(item: $router.sheet, onDismiss: sheetDismissed) { route in
                sheetContent(route)
                    .onAppear { presented = route }
            }
            .toastHost(container.toasts, dismissLabel: L("close"))
            .onAppear { container.startHostRestore() }
            #if FULL_FLAVOR
            .pluginAutoDisabledToasts()
            #endif
            .onChange(of: router.tab) { _, new in
                if new == .newTask {
                    router.tab = previousTab
                    router.openNewDownload()
                } else {
                    previousTab = new
                }
            }
            .onChange(of: state.selections.presentable.map(\.requestId), initial: true) { presentNextSheet() }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { container.rescanOnForeground() }
            }
            .sessionRevokedAlert()
            .linkPairingPrompts()
    }

    @ViewBuilder
    private func tabs(selection: Binding<AppTab>) -> some View {
        if #available(iOS 27, *) {
            TabView(selection: selection) {
                primaryTabs
                Tab(L("newDownload"), systemImage: FluxSymbol.newDownload, value: AppTab.newTask, role: .prominent) {
                    Color.clear
                }
            }
            .tabViewStyle(.sidebarAdaptable)
            .environment(\.hasProminentNewTab, true)
        } else {
            TabView(selection: selection) {
                primaryTabs
            }
            .tabViewStyle(.sidebarAdaptable)
        }
    }

    @TabContentBuilder<AppTab>
    private var primaryTabs: some TabContent<AppTab> {
        Tab(L("mobileNavDownloads"), systemImage: FluxSymbol.downloads, value: AppTab.downloads) {
            DownloadsScreen()
        }
        Tab(L("mobileNavRss"), systemImage: FluxSymbol.subscriptions, value: AppTab.rss) {
            RssScreen()
        }
        // `ui.show_activity_rss`（云同步偏好，通用设置「入口」）关闭 → 标签栏不显示「订阅」；
        // 全局搜索的「前往订阅」等入口仍可程序化进入（03-settings §4.4）。
        .hidden(!container.store.state.preferences.bool("ui.show_activity_rss", default: true))
        Tab(L("mobileNavDevices"), systemImage: FluxSymbol.devices, value: AppTab.devices) {
            DevicesScreen()
        }
        Tab(L("mobileNavSettings"), systemImage: FluxSymbol.settings, value: AppTab.settings) {
            SettingsScreen()
        }
    }

    @ViewBuilder
    private func sheetContent(_ route: SheetRoute) -> some View {
        switch route {
        case let .newDownload(prefill): NewDownloadSheet(prefill: prefill)
        case let .externalDownload(confirmation):
            NewDownloadSheet(prefill: confirmation.request.url, confirmation: confirmation)
        case .activity: ActivitySheet()
        case .addHost: AddHostSheet()
        case let .moveToQueue(ids): MoveToQueueSheet(taskIds: ids)
        case let .selection(requestId): SelectionRequestSheet(requestId: requestId)
        case .fileConflicts: FileConflictSheet()
        case .search: GlobalSearchScreen()
        case .queues: QueueManagerSheet()
        case let .btTaskLimits(taskId):
            #if FULL_FLAVOR
            BtTaskLimitsSheet(taskId: taskId)
            #else
            EmptyView()
            #endif
        }
    }

    /// Keep a capture awaiting confirmation separate from any open form (and from captures for other devices).
    private func presentNextSheet() {
        let router = container.router
        guard router.sheet == nil, presented == nil else { return }
        if !externalConfirmations.isEmpty {
            router.tab = .downloads
            router.sheet = .externalDownload(externalConfirmations.removeFirst())
            return
        }
        let pending = container.store.state.selections.presentable
        router.dismissedSelections.formIntersection(pending.map(\.requestId))
        if let next = pending.first(where: { !router.dismissedSelections.contains($0.requestId) }) {
            router.sheet = next.fileConflict == nil ? .selection(requestId: next.requestId) : .fileConflicts
        }
    }

    /// 选择请求 Sheet 被划走而主机仍在等待：不再自动弹出（下载页横幅可重新打开；到期按默认处理）。
    private func sheetDismissed() {
        let dismissed = presented
        presented = nil
        defer { presentNextSheet() }
        let pending = container.store.state.selections
        switch dismissed {
        case let .selection(requestId)?:
            guard pending.contains(where: { $0.requestId == requestId }) else { return }
            container.router.dismissedSelections.insert(requestId)
        case .fileConflicts?:
            // 「稍后决定」：当前所有待答的文件已存在请求都不再自动弹出（之后新到的会重新弹出整个列表）。
            container.router.dismissedSelections.formUnion(pending.fileConflicts.map(\.requestId))
        default:
            break
        }
    }

    /// External links use device rules; files keep the system torrent import path.
    private func handle(_ url: URL) {
        if url.isFileURL {
            container.router.tab = .downloads
            #if FULL_FLAVOR
            TorrentImport.openFromSystem(url, container: container)
            #else
            FluxHaptic.warning.play()
            container.toasts.show(text: L("unsupportedDropHintGeneric"), tone: .warning)
            #endif
            return
        }
        // Keep an accepted system download on the current host while routing or waiting for its sheet.
        container.supersedeHostRestore()
        externalRequests += ExternalDownload.capture(from: url)
        guard !processingExternalRequests else { return }
        processingExternalRequests = true
        Task { await processExternalRequests() }
    }

    /// A cold launch must not read the empty placeholder snapshot as the user's saved rules.
    private func processExternalRequests() async {
        defer { processingExternalRequests = false }
        let deadline = ContinuousClock.now + .seconds(8)
        while container.store.state.connection == .connecting, ContinuousClock.now < deadline {
            do { try await Task.sleep(for: .milliseconds(50)) } catch { break }
        }
        while !externalRequests.isEmpty {
            let batch = externalRequests
            externalRequests = []
            for request in batch {
                if let confirmation = await routeExternal(request) {
                    externalConfirmations.append(confirmation)
                    presentNextSheet()
                }
            }
        }
    }

    private func routeExternal(_ request: ExternalDownload) async -> ExternalDownloadConfirmation? {
        let state = container.store.state
        let rules = CaptureDeviceRule.parse(state.preferences.string(CaptureDeviceRule.preferenceKey, default: ""))
        let id = CaptureDeviceRule.match(url: request.url, fileName: request.fileName, rules: rules)
        var confirmation = ExternalDownloadConfirmation(request: request, targetId: id)
        guard let id, state.preferences.bool("download.silent_download", default: false) else { return confirmation }
        guard request.canDispatch else { return confirmation }
        guard !state.isReadOnly,
              let target = DeviceRules.resolveTarget(id: id, in: state.downloadDispatchTargets(sections: captureSections)) else {
            confirmation.failure = L("captureRoutingTargetUnavailable")
            return confirmation
        }
        if !target.isCloud, target.isOffline {
            confirmation.failure = L("captureRoutingDispatchFailed") + "\n" + L("errReasonPeerOffline")
            return confirmation
        }
        let item = DispatchItem(
            entry: UrlEntry(url: request.url, fileName: request.fileName),
            fileName: request.fileName.isEmpty ? nil : request.fileName
        )
        let result = await Dispatcher.run([item], to: target, saveDir: nil, agent: container.agent)
        if let details = Dispatcher.announce(result, target: target, toasts: container.toasts) {
            confirmation.failure = L("captureRoutingDispatchFailed") + "\n" + details
            return confirmation
        }
        return nil
    }
}

private extension View {
    /// 底部活动附件：页面自带底栏（多选批量栏、任务详情操作栏）时用 `isEnabled` 隐藏，避免遮挡。
    /// 最低部署 iOS 26.1 正是为了这个重载（26.0 只能常驻）。
    func bottomAccessory(suppressed: Bool) -> some View {
        tabViewBottomAccessory(isEnabled: !suppressed) { ActivityAccessory() }
    }
}
