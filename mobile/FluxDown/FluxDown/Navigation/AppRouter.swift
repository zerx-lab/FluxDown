import Foundation
import Observation
import SwiftUI

/// 顶层标签（README §4.1）：下载 / 订阅 / 设备 / 设置。全局搜索不是标签：各根页右上角按钮以 `SheetRoute.search` 呈现。
/// `newTask` 仅 iOS 27 的 `Tab(role: .prominent)` 使用：选中即弹新建 Sheet 并回弹到原标签。
nonisolated enum AppTab: Hashable {
    case downloads, rss, devices, settings, newTask
}

/// 下载栈推入页。iPad（regular 宽度）下任务详情进入右栏，由 `selectedTaskId` 驱动。
nonisolated enum DownloadsRoute: Hashable {
    case task(String)
    /// D6：任务组详情。
    case group(String)
}

/// 设置栈推入页（顺序见 03-settings §1.1；对齐 PC `build_pages`，桌面专属项省略）。
nonisolated enum SettingsRoute: Hashable {
    case account, general, appearance, notify
    case download, network, webhook, api
    #if FULL_FLAVOR
    /// 插件 / 组件（仅 full flavor；store 不含插件功能）。
    case extensions
    case bt, ed2k
    #endif
    case diagnostics, about
}

/// 一次性的跨页意图：全局搜索命令等发起，目标页就绪（所在标签被选中、无其它 Sheet）后消费并清空。
nonisolated enum AppIntent: Hashable {
    /// 订阅页：新建订阅（R3 创建）。
    case newRssSource
    /// 设备页：添加设备（配对）。
    case addDevice

    /// 该意图的执行页所在的标签。
    var tab: AppTab {
        switch self {
        case .newRssSource: .rss
        case .addDevice: .devices
        }
    }
}

/// 全局 Sheet（同一时刻最多一个；SwiftUI 只允许单一呈现）。
nonisolated enum SheetRoute: Identifiable, Hashable {
    /// N1：`prefill` = 预填链接（粘贴 / 分享 / magnet:// 唤起）。
    case newDownload(prefill: String)
    /// D9：全局活动面板。
    case activity
    /// V2：添加远程（`--server`）主机。
    case addHost
    /// D5：移动到队列。
    case moveToQueue([String])
    /// X1–X3：引擎发起的选择请求（由 shell 依据 `HostState.selections` 排队呈现）。
    case selection(requestId: String)
    /// X4：文件已存在询问（所有待答的 `fileExists` 请求聚合在同一个 Sheet）。
    case fileConflicts
    /// G1：全局搜索（各根页右上角按钮）。
    case search
    /// D7 / D8：队列管理。
    case queues
    /// BT 任务级限速（任务菜单「任务限速…」）。
    case btTaskLimits(taskId: String)

    var id: String {
        switch self {
        case let .newDownload(prefill): "new:\(prefill)"
        case .activity: "activity"
        case .addHost: "addHost"
        case let .moveToQueue(ids): "move:\(ids.joined(separator: ","))"
        case let .selection(requestId): "selection:\(requestId)"
        case .fileConflicts: "fileConflicts"
        case .search: "search"
        case .queues: "queues"
        case let .btTaskLimits(taskId): "btLimits:\(taskId)"
        }
    }
}

/// 应用导航状态（单一事实源，同 Android `AppNavigator`）。
@MainActor
@Observable
final class AppRouter {
    var tab: AppTab = .downloads
    var downloadsPath: [DownloadsRoute] = []
    var settingsPath: [SettingsRoute] = []
    var sheet: SheetRoute?
    /// 正在显示底部工具栏 / 隐藏标签栏的页面登记的键；非空时壳层隐藏底部活动附件，避免盖住页面自己的底栏。
    var bottomAccessorySuppressors: Set<String> = []
    var isBottomAccessorySuppressed: Bool { !bottomAccessorySuppressors.isEmpty }
    /// regular 宽度三栏布局中当前详情任务（compact 下走 `downloadsPath`）。
    var selectedTaskId: String?
    /// 已呈现过、被用户关闭但主机尚未 resolve 的选择请求不再自动弹出（避免关了又弹）。
    var dismissedSelections: Set<String> = []
    /// 待目标页执行的一次性意图（见 `AppIntent`）。
    var pendingIntent: AppIntent?
    /// 「重新配对」入口给添加主机面板预填的主机地址（面板出现时一次性消费，见 `AddHostSheet`）。
    var addHostRepairAddress: String?

    /// 已配对主机被拒后重新配对：打开添加主机面板（配对模式）并预填该主机的地址。
    func presentRepair(address: String) {
        addHostRepairAddress = address
        sheet = .addHost
    }

    /// 收起当前 Sheet、切到意图所在的标签，并登记意图（目标页在标签被选中且无 Sheet 后消费）。
    func perform(_ intent: AppIntent) {
        sheet = nil
        tab = intent.tab
        pendingIntent = intent
    }

    /// 目标页消费意图的等待：所在标签已被选中且没有 Sheet 正在呈现 / 收起（再留出 Sheet 收起动画的时间）。
    /// 返回 true = 可以执行（并已清空意图）；false = 意图已被取代 / 超时。
    func claim(_ intent: AppIntent) async -> Bool {
        var waited = 0
        while pendingIntent == intent, tab != intent.tab || sheet != nil {
            guard waited < 60 else { return false } // 约 3 秒
            waited += 1
            do {
                try await Task.sleep(for: .milliseconds(50))
            } catch {
                return false
            }
        }
        guard pendingIntent == intent else { return false }
        do {
            try await Task.sleep(for: .milliseconds(400)) // 全局搜索 Sheet 的收起动画
        } catch {
            return false
        }
        guard pendingIntent == intent, tab == intent.tab, sheet == nil else { return false }
        pendingIntent = nil
        return true
    }

    /// 打开「新建下载」：已有 Sheet 时先替换为新建（链接唤起优先）。
    func openNewDownload(prefill: String = "") {
        sheet = .newDownload(prefill: prefill)
    }

    /// 打开任务详情（全局搜索 / 通知 / 选择请求跳转）。
    func showTask(_ id: String) {
        tab = .downloads
        selectedTaskId = id
        downloadsPath = [.task(id)]
    }

    func showSettings(_ route: SettingsRoute) {
        tab = .settings
        settingsPath = [route]
    }
}

extension View {
    /// 页面显示自己的底部工具栏（或隐藏标签栏）期间压制底部活动附件。`key` 在同一时刻需唯一；视图消失即撤销。
    func suppressesBottomAccessory(_ key: String, when active: Bool = true) -> some View {
        modifier(BottomAccessorySuppression(key: key, active: active))
    }
}

private struct BottomAccessorySuppression: ViewModifier {
    @Environment(AppContainer.self) private var container
    let key: String
    let active: Bool

    func body(content: Content) -> some View {
        content
            .onAppear { apply(active) }
            .onChange(of: active) { _, now in apply(now) }
            .onDisappear { apply(false) }
    }

    private func apply(_ on: Bool) {
        if on {
            container.router.bottomAccessorySuppressors.insert(key)
        } else {
            container.router.bottomAccessorySuppressors.remove(key)
        }
    }
}
