// PopupWindowHost.swift
// 外部唤起独立下载小窗（原生宿主，macOS）。
//
// 完整契约见 FluxDown 跨端契约文档（外部唤起独立小窗 v1）：
// - 主引擎通道 fluxdown/popup_host（注册在主引擎 messenger 上）：show / close，
//   回调 onResult / onClosed。
// - 弹窗引擎通道 fluxdown/popup_child（注册在弹窗引擎 messenger 上）：ready / submit /
//   cancel / pickFolder / startDrag / resize，回调 setPayload。
//
// 设计原则（方案评审硬性约束，勿改）：
// - 弹窗窗口 + 弹窗引擎懒创建、常驻复用：首次 show 创建，之后仅 hide/show，
//   进程退出前绝不销毁 —— 本仓库对 desktop_multi_window 频繁建销 isolate 曾致
//   0xc0000005 崩溃有前科（见 commit 39d6c74），弹窗引擎生命周期必须与主进程等长。
// - 弹窗引擎零插件注册（不得调用 GeneratedPluginRegistrant/RegisterGeneratedPlugins）、
//   不初始化 Rust；所有环境数据经载荷 JSON 注入。
// - 系统级关闭（performClose / windowShouldClose）语义等价于 cancel：隐藏 + 中继
//   onClosed，禁止真正销毁窗口。

import Cocoa
import CoreGraphics
import FlutterMacOS

/// 弹窗逻辑宽度（固定，逻辑像素），需与 Dart 侧、Windows/Linux 原生实现保持一致。
private let kPopupWidth: CGFloat = 520
/// 弹窗初始逻辑高度；resize() 会动态调高。
private let kPopupInitialHeight: CGFloat = 600
/// reveal 兜底超时（秒）——show 后弹窗 Dart 迟迟不发 reveal（引擎冷启动
/// 异常/卡死）时按当前尺寸强制显示，保证窗口永远弹得出来。
private let kRevealFallbackTimeout: TimeInterval = 3.0
/// resize() 允许的最大高度占所在屏幕工作区高度的比例。
private let kPopupMaxHeightRatio: CGFloat = 0.9
/// 弹窗圆角半径（逻辑像素）；Dart 侧已自绘卡片圆角，这里仅作兜底裁剪，避免四角露出窗口透明背景外的直角瑕疵。
private let kPopupCornerRadius: CGFloat = 12

// =============================================================================
// QuickPopupWindow —— borderless 窗口默认无法成为 key/main window，
// 但表单需要文本输入焦点，必须显式 override 为 true。
// 参考：https://developer.apple.com/documentation/appkit/nswindow/canbecomekey
//      （borderless 窗口默认 canBecomeKey = false，是本类存在的唯一原因）
// =============================================================================

private final class QuickPopupWindow: NSWindow {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

// =============================================================================
// PopupWindowHost —— 单例，持有弹窗 NSWindow + 弹窗 FlutterViewController，
// 负责两条 MethodChannel 的协议中继与载荷投递时序。
// =============================================================================

final class PopupWindowHost: NSObject, NSWindowDelegate {
    static let shared = PopupWindowHost()

    override private init() {}

    private var window: QuickPopupWindow?
    private var popupController: FlutterViewController?

    /// fluxdown/popup_host —— 注册在主引擎 messenger 上，用于回调 onResult/onClosed。
    private var hostChannel: FlutterMethodChannel?
    /// fluxdown/popup_child —— 注册在弹窗引擎 messenger 上，用于投递 setPayload。
    private var childChannel: FlutterMethodChannel?

    /// 弹窗 Dart 是否已完成首帧并调用 ready()。
    private var childReady = false
    /// show() 早于 ready() 到达时的暂存载荷；ready() 到达后立即投递并清空。
    private var pendingPayload: String?
    /// reveal 兜底定时器（show 时武装；reveal 到达 / 隐藏时作废）。
    private var revealFallback: DispatchWorkItem?

    // -------------------------------------------------------------------
    // 主引擎侧接线
    // -------------------------------------------------------------------

    /// 由 MainFlutterWindow.awakeFromNib 调用一次，完成主引擎侧 channel 注册。
    func register(with messenger: FlutterBinaryMessenger) {
        let channel = FlutterMethodChannel(name: "fluxdown/popup_host", binaryMessenger: messenger)
        channel.setMethodCallHandler { [weak self] call, result in
            self?.handleHostCall(call, result: result)
        }
        hostChannel = channel
    }

    // -------------------------------------------------------------------
    // fluxdown/popup_host 分发（Dart -> Native，主引擎侧）
    // -------------------------------------------------------------------

    private func handleHostCall(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        switch call.method {
        case "show":
            guard let payload = call.arguments as? String else {
                result(FlutterError(code: "bad_args", message: "show: expected payload JSON string", details: nil))
                return
            }
            result(showPopup(payloadJson: payload))
        case "append":
            // 小窗可见期间新到的外部请求 — 转发给弹窗引擎合入当前表单。
            // 窗口实际不可见/引擎未就绪时返回 false，Dart 侧复位失步状态。
            guard let urlText = call.arguments as? String else {
                result(FlutterError(code: "bad_args", message: "append: expected URL text string", details: nil))
                return
            }
            let canAppend = (window?.isVisible ?? false) && childReady && childChannel != nil
            if canAppend {
                childChannel?.invokeMethod("appendPayload", arguments: urlText)
            }
            result(canAppend)
        case "close":
            // 主引擎主动收起（例如用户在主窗口取消了外部请求），不回调 onClosed。
            hidePopup()
            result(nil)
        default:
            result(FlutterMethodNotImplemented)
        }
    }

    /// 懒创建（或复用）弹窗窗口，按载荷投递时序处理 payload 并重置定位。
    ///
    /// 显示时序（reveal 握手）：窗口保持隐藏（复用时先藏起旧表单画面），
    /// 由弹窗 Dart 在新载荷首帧就绪后经 reveal 一次到位「设高 + 显示」——
    /// 消除旧表单闪现与默认高度→内容高度的二段跳。同时武装兜底定时器：
    /// reveal 超时未到达时按当前尺寸强制显示，保证窗口永远弹得出来。
    private func showPopup(payloadJson: String) -> Bool {
        let win = ensurePopupWindow()

        hidePopup()

        if childReady {
            childChannel?.invokeMethod("setPayload", arguments: payloadJson)
        } else {
            pendingPayload = payloadJson
        }

        // 重置为初始尺寸并居中：避免复用窗口残留上一次请求 resize() 后的
        // 高度；reveal 到达时会按内容实高覆盖。
        repositionCentered(win, width: kPopupWidth, height: kPopupInitialHeight)

        let fallback = DispatchWorkItem { [weak self] in
            self?.revealFallback = nil
            NSLog("[popup-host] reveal timed out, force presenting popup")
            self?.presentPopup()
        }
        revealFallback = fallback
        DispatchQueue.main.asyncAfter(deadline: .now() + kRevealFallbackTimeout, execute: fallback)
        return true
    }

    /// 显示并激活（reveal 握手的显示端；也是兜底定时器的强制显示入口）。
    private func presentPopup() {
        revealFallback?.cancel()
        revealFallback = nil
        guard let win = window else { return }
        win.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// 统一隐藏入口：隐藏窗口并作废 pending 的 reveal 兜底。
    private func hidePopup() {
        revealFallback?.cancel()
        revealFallback = nil
        window?.orderOut(nil)
    }

    // -------------------------------------------------------------------
    // 弹窗引擎懒创建
    // -------------------------------------------------------------------

    /// 首次调用时创建弹窗窗口 + 弹窗 FlutterEngine/ViewController；此后直接复用（幂等）。
    @discardableResult
    private func ensurePopupWindow() -> QuickPopupWindow {
        if let win = window {
            return win
        }

        // 弹窗引擎：独立 FlutterDartProject + dartEntrypointArguments = ["--quick-popup"]，
        // 令 Dart main() 走独立分支（runQuickPopupApp），零插件注册、不初始化 Rust。
        //
        // 查证结论：
        // - FlutterDartProject 在 macOS embedder 上只暴露 `init`（无参）为可用初始化器，
        //   `initWithPrecompiledDartBundle:` 头文件标注同样可用，但另有遗留初始化器被标记
        //   `FLUTTER_UNAVAILABLE("Use -init instead.")`；`FlutterDartProject()` 即官方推荐用法。
        //   来源：https://api.flutter.dev/macos-embedder/class_flutter_dart_project.html
        // - dartEntrypointArguments 为 `NSArray<NSString*>*` 可读写属性，未显式设置时默认取
        //   进程启动参数；仅在 iOS embedder 上标注 API_UNAVAILABLE，macOS 上可用。
        //   来源：https://api.flutter.dev/macos-embedder/class_flutter_dart_project.html
        // - FlutterViewController(project:) 内部会隐式创建一个新的 FlutterEngine 并运行该
        //   project，是"第一个 FlutterViewController"场景的推荐初始化器（区别于
        //   initWithEngine:nibName:bundle: 复用既有引擎的场景），恰好符合"第二个独立引擎"需求。
        //   来源：https://api.flutter.dev/macos-embedder/interface_flutter_view_controller.html
        let project = FlutterDartProject()
        project.dartEntrypointArguments = ["--quick-popup"]

        let controller = FlutterViewController(project: project)
        popupController = controller

        // 圆角裁剪：contentViewController.view 即 FlutterView 所在容器，layer 化后裁剪即可
        // 让窗口物理边界随 Dart 自绘卡片一起呈现圆角，避免透明背景窗口露出直角容器。
        controller.view.wantsLayer = true
        controller.view.layer?.cornerRadius = kPopupCornerRadius
        controller.view.layer?.masksToBounds = true

        let initialFrame = NSRect(x: 0, y: 0, width: kPopupWidth, height: kPopupInitialHeight)
        // styleMask 选择 .borderless（而非 .titled + .fullSizeContentView 方案）：
        // 本窗口完全无意呈现系统标题栏/交通灯按钮，Dart 侧自绘标题拖动区与关闭按钮；
        // .fullSizeContentView 仍需搭配 .titled 才能生效，随之带出系统交通灯按钮，
        // 还得额外隐藏（titlebarAppearsTransparent + 逐个隐藏按钮），不如 .borderless 直接干净。
        let win = QuickPopupWindow(
            contentRect: initialFrame,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        // 透明背景 + 非不透明：配合圆角裁剪，窗口四角之外区域完全透明，不露出方形背景。
        win.isOpaque = false
        win.backgroundColor = .clear
        win.hasShadow = true
        win.level = .floating
        win.isReleasedWhenClosed = false
        win.isMovableByWindowBackground = false
        win.delegate = self
        // 不进 Dock/窗口列表：从应用 Window 菜单剔除 + 从 Mission Control/Cmd-` 窗口循环中隐藏。
        // 来源：https://developer.apple.com/documentation/appkit/nswindow/isexcludedfromwindowsmenu
        //      https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct
        win.isExcludedFromWindowsMenu = true
        win.collectionBehavior = [.transient, .ignoresCycle, .fullScreenAuxiliary]
        win.contentViewController = controller

        // 弹窗引擎 channel：fluxdown/popup_child，注册在弹窗引擎（非主引擎）messenger 上。
        let channel = FlutterMethodChannel(
            name: "fluxdown/popup_child",
            binaryMessenger: controller.engine.binaryMessenger
        )
        channel.setMethodCallHandler { [weak self] call, result in
            self?.handleChildCall(call, result: result)
        }
        childChannel = channel

        window = win
        return win
    }

    // -------------------------------------------------------------------
    // fluxdown/popup_child 分发（Dart -> Native，弹窗引擎侧）
    // -------------------------------------------------------------------

    private func handleChildCall(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        switch call.method {
        case "ready":
            childReady = true
            if let payload = pendingPayload {
                pendingPayload = nil
                childChannel?.invokeMethod("setPayload", arguments: payload)
            }
            result(nil)
        case "submit":
            guard let payload = call.arguments as? String else {
                result(FlutterError(code: "bad_args", message: "submit: expected result JSON string", details: nil))
                return
            }
            hidePopup()
            hostChannel?.invokeMethod("onResult", arguments: payload)
            result(nil)
        case "cancel":
            hidePopup()
            hostChannel?.invokeMethod("onClosed", arguments: nil)
            result(nil)
        case "pickFolder":
            handlePickFolder(call, result: result)
        case "startDrag":
            handleStartDrag(result: result)
        case "resize":
            handleResize(call, result: result)
        case "reveal":
            // reveal 握手：新载荷首帧已就绪 — 设高完成后显示并激活
            // （presentPopup 内部解除兜底定时器）。已可见时等价一次 resize。
            if let args = call.arguments as? [String: Any],
                let height = (args["height"] as? NSNumber)?.doubleValue, height > 0 {
                applyLogicalHeight(CGFloat(height))
            }
            presentPopup()
            result(nil)
        default:
            result(FlutterMethodNotImplemented)
        }
    }

    /// NSOpenPanel 目录选择；parent = 弹窗窗口，取消返回 nil。
    private func handlePickFolder(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        guard let win = window else {
            result(nil)
            return
        }
        let args = call.arguments as? [String: Any]
        let title = args?["title"] as? String
        let initialDir = args?["initialDir"] as? String

        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.resolvesAliases = true
        // NSOpenPanel 以 sheet 形式呈现时不显示系统标题栏，`title` 属性不生效；
        // 面板顶部说明文案须使用 `message`。
        // 来源：https://developer.apple.com/documentation/appkit/nssavepanel/message
        if let title = title, !title.isEmpty {
            panel.message = title
        }
        panel.directoryURL = resolveInitialDirectoryURL(initialDir)

        panel.beginSheetModal(for: win) { response in
            if response == .OK, let url = panel.urls.first {
                result(url.path)
            } else {
                result(nil)
            }
        }
    }

    /// initialDir 不存在时逐级向上查找存在的祖先目录；查到系统根仍不存在则返回 nil
    /// （NSOpenPanel 会自行回退到用户上次选择过的目录）。
    private func resolveInitialDirectoryURL(_ path: String?) -> URL? {
        guard let path = path, !path.isEmpty else { return nil }
        let fileManager = FileManager.default
        var url = URL(fileURLWithPath: path)
        while !fileManager.fileExists(atPath: url.path) {
            let parent = url.deletingLastPathComponent()
            if parent.path == url.path {
                return nil
            }
            url = parent
        }
        return url
    }

    /// 开始拖动窗口：window.performDrag(with:) 需要触发拖动手势的原始鼠标事件。
    private func handleStartDrag(result: @escaping FlutterResult) {
        guard let win = window else {
            result(nil)
            return
        }
        // NSApp.currentEvent 是应用当前正在处理的事件；Dart 侧标题拖动区在收到
        // mouseDown/pan 手势起始的这一帧内同步调用 startDrag，此时 currentEvent 通常
        // 仍是驱动该手势的原始鼠标事件。事件可能因系统吞掉或时序错位而拿不到，
        // 此时静默忽略（不崩溃），窗口只是这次没跟手拖动，用户可再次尝试。
        if let event = NSApp.currentEvent {
            win.performDrag(with: event)
        }
        result(nil)
    }

    /// 按逻辑像素高度调整窗口：宽度不变，顶边不动（AppKit 坐标系 Y 向上，
    /// 需以旧 frame 的 maxY 作为锚点反推新 origin.y），并 clamp 到所在屏幕
    /// 工作区高度的 90%。resize / reveal 共用。
    private func applyLogicalHeight(_ height: CGFloat) {
        guard let win = window else { return }
        let visibleHeight = win.screen?.visibleFrame.height
            ?? NSScreen.screens.first?.visibleFrame.height
            ?? win.frame.height
        let clampedHeight = min(height, visibleHeight * kPopupMaxHeightRatio)

        let oldFrame = win.frame
        let topY = oldFrame.origin.y + oldFrame.height
        let newFrame = NSRect(
            x: oldFrame.origin.x,
            y: topY - clampedHeight,
            width: oldFrame.width,
            height: clampedHeight
        )
        win.setFrame(newFrame, display: true, animate: false)
    }

    private func handleResize(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        guard window != nil else {
            result(nil)
            return
        }
        guard let args = call.arguments as? [String: Any],
            let height = (args["height"] as? NSNumber)?.doubleValue, height > 0
        else {
            result(FlutterError(code: "bad_args", message: "resize: invalid height", details: nil))
            return
        }
        applyLogicalHeight(CGFloat(height))
        result(nil)
    }

    // -------------------------------------------------------------------
    // 窗口定位
    // -------------------------------------------------------------------

    /// 以给定逻辑尺寸居中于主屏（screens.first，全局坐标原点所在屏，与 FloatingBallPanel
    /// 的主屏约定一致）。取不到屏幕信息时退化为仅设置尺寸，不改变原点。
    /// 仅重定位/设尺寸，不改变窗口可见性（显示由 presentPopup 负责）。
    private func repositionCentered(_ win: NSWindow, width: CGFloat, height: CGFloat) {
        guard let visible = NSScreen.screens.first?.visibleFrame else {
            let old = win.frame
            win.setFrame(NSRect(x: old.origin.x, y: old.origin.y, width: width, height: height), display: true)
            return
        }
        let x = visible.origin.x + (visible.width - width) / 2
        let y = visible.origin.y + (visible.height - height) / 2
        win.setFrame(NSRect(x: x, y: y, width: width, height: height), display: true)
    }

    // -------------------------------------------------------------------
    // NSWindowDelegate —— 系统级关闭 = cancel 语义，禁止销毁
    // -------------------------------------------------------------------

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        hidePopup()
        hostChannel?.invokeMethod("onClosed", arguments: nil)
        // 返回 false 阻止 AppKit 真正销毁窗口：弹窗引擎常驻复用，禁止随窗口关闭而销毁。
        return false
    }
}
