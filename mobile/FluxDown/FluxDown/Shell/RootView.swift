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

    var body: some View {
        if let actions {
            shell
                .environment(actions)
                .taskActionDialogs(actions)
        } else {
            Color.clear.onAppear { actions = TaskActions(container: container) }
        }
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
            #if FULL_FLAVOR
            .pluginAutoDisabledToasts()
            #endif
            .onOpenURL(perform: handle)
            .onChange(of: router.tab) { _, new in
                if new == .newTask {
                    router.tab = previousTab
                    router.openNewDownload()
                } else {
                    previousTab = new
                }
            }
            .onChange(of: state.selections.presentable.map(\.requestId), initial: true) { presentNextSelection() }
            .onChange(of: router.sheet == nil) { presentNextSelection() }
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

    /// 选择请求（X1–X3）排队：无其他 Sheet 时呈现第一个未被用户划走、且本构建可处理的请求。
    private func presentNextSelection() {
        let router = container.router
        guard router.sheet == nil else { return }
        let pending = container.store.state.selections.presentable
        router.dismissedSelections.formIntersection(pending.map(\.requestId))
        if let next = pending.first(where: { !router.dismissedSelections.contains($0.requestId) }) {
            router.sheet = next.fileConflict == nil ? .selection(requestId: next.requestId) : .fileConflicts
        }
    }

    /// 选择请求 Sheet 被划走而主机仍在等待：不再自动弹出（下载页横幅可重新打开；到期按默认处理）。
    private func sheetDismissed() {
        defer { presented = nil }
        let pending = container.store.state.selections
        switch presented {
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

    /// N4：`fluxdown://…?url=` 唤起 → 预填「新建下载」（full 另接 `magnet:` / `ed2k://`）；
    /// N3：「文件」App 里「用 FluxDown 打开」文件 → full 直接导入 `.torrent`，store 与不支持的文件一律给出提示。
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
        var link = url.absoluteString
        if url.scheme?.lowercased() == "fluxdown",
           let item = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "url" }),
           let value = item.value, !value.isEmpty {
            link = value
        }
        container.router.tab = .downloads
        container.router.openNewDownload(prefill: link)
    }
}

private extension View {
    /// 底部活动附件：页面自带底栏（多选批量栏、任务详情操作栏）时用 `isEnabled` 隐藏，避免遮挡。
    /// 最低部署 iOS 26.1 正是为了这个重载（26.0 只能常驻）。
    func bottomAccessory(suppressed: Bool) -> some View {
        tabViewBottomAccessory(isEnabled: !suppressed) { ActivityAccessory() }
    }
}
