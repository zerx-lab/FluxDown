// FloatingBallPanel.swift
// 悬浮球原生层（macOS）— MethodChannel `com.fluxdown/floating_ball`。
//
// 设计原则（方案评审硬性约束，勿改）：
// - 本文件是"哑"窗口：只贴位图 + 转发输入，不含任何业务逻辑（下载/托盘/设置一律留在 Dart）。
// - 禁止在此创建第二个 FlutterEngine/FlutterViewController/Isolate —— 本仓库有 0xc0000005 崩溃前科。
// - 坐标协议：Dart <-> 原生 一律使用"主屏左上角为原点、Y 向下"的坐标系（与 Windows 实现一致）；
//   AppKit 原生坐标系是"主屏左下角为原点、Y 向上"，本文件内部做换算，绝不把 AppKit 坐标泄漏给 Dart。
//
// MethodChannel 协议（详见 lib/src/services/floating_ball/floating_ball_service.dart）：
// Dart -> 原生: pushBitmap / showBall / hideBall / destroyBall / showContextMenu
// 原生 -> Dart: onDropPayload / onBallClicked / onBallMoved / onDragEnter / onDragLeave /
//               onContextMenuRequested / onMenuAction

import Cocoa
import CoreGraphics
import FlutterMacOS
import QuartzCore

/// 窗口逻辑尺寸（含 8px 阴影出血），与 Dart 侧 kBallWindowSize 保持一致。
private let kBallWindowSize: CGFloat = 72
/// 圆形命中半径（逻辑像素），与 Dart 侧 kBallHitRadius 保持一致。
private let kBallHitRadius: CGFloat = 28
/// 拖动判定阈值（逻辑像素）：累计位移超过此值才视为拖动而非点击。
private let kDragThreshold: CGFloat = 4
/// 贴边留白（逻辑像素）。
private let kDockMargin: CGFloat = 8
/// 默认停靠垂直位置比例（工作区高度的 40%）。
private let kDockVerticalRatio: CGFloat = 0.4
/// 拖放载荷大小上限（字节），超过丢弃。
private let kMaxDropPayloadBytes = 4096
/// 吸附判定阈值（逻辑像素）：拖动释放时球边缘距屏幕工作区左/右/上边 ≤ 此值 → 贴边。
private let kDockSnapThreshold: CGFloat = 12
/// 收起后露出的边条宽度（逻辑像素）。
private let kDockRevealWidth: CGFloat = 14
/// 展开态下光标离开球后，延迟这么久才收起。
private let kDockCollapseDelay: TimeInterval = 0.8
/// 贴边吸附/收起/展开动画时长（秒，ease-out cubic）。
private let kDockAnimDuration: TimeInterval = 0.16

// =============================================================================
// FloatingBallPanel — 单例，持有 NSPanel + MethodChannel，负责协议分发与坐标换算。
// =============================================================================

final class FloatingBallPanel: NSObject {
    static let shared = FloatingBallPanel()

    override private init() {}

    private var panel: NSPanel?
    private var contentView: BallContentView?
    private var channel: FlutterMethodChannel?
    private var backingObserver: NSObjectProtocol?

    // -- 贴边收起状态机（迅雷式）--
    private var dockEdge: DockEdge = .none
    /// 是否处于收起态；BallContentView.hitTest 需要读取，故 fileprivate。
    fileprivate private(set) var isCollapsed = false
    private var collapseWorkItem: DispatchWorkItem?

    private enum DockEdge: Equatable {
        case none, left, right, top
    }

    /// 由 MainFlutterWindow.awakeFromNib 调用一次，完成 channel 注册。
    func register(with messenger: FlutterBinaryMessenger) {
        let channel = FlutterMethodChannel(
            name: "com.fluxdown/floating_ball",
            binaryMessenger: messenger
        )
        channel.setMethodCallHandler { [weak self] call, result in
            self?.handle(call, result: result)
        }
        self.channel = channel
    }

    // -------------------------------------------------------------------
    // MethodChannel 分发
    // -------------------------------------------------------------------

    private func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        switch call.method {
        case "pushBitmap":
            handlePushBitmap(call, result: result)
        case "showBall":
            handleShowBall(call, result: result)
        case "hideBall":
            hideBall()
            result(nil)
        case "destroyBall":
            destroyBall()
            result(nil)
        case "showContextMenu":
            handleShowContextMenu(call, result: result)
        default:
            // queryCapability 等 Linux 专属方法不在 macOS 上处理。
            result(FlutterMethodNotImplemented)
        }
    }

    private func handlePushBitmap(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        guard let args = call.arguments as? [String: Any],
            let typedBytes = args["bytes"] as? FlutterStandardTypedData,
            let width = (args["width"] as? NSNumber)?.intValue,
            let height = (args["height"] as? NSNumber)?.intValue,
            let scale = (args["scale"] as? NSNumber)?.doubleValue,
            width > 0, height > 0, scale > 0
        else {
            result(FlutterError(code: "bad_args", message: "pushBitmap: invalid arguments", details: nil))
            return
        }

        let view = ensurePanel()
        view.setBitmap(rgba: typedBytes.data, width: width, height: height, scale: CGFloat(scale))

        // 窗口逻辑尺寸 = width/scale（若与当前不同则调整，保持左上角锚点不变）。
        let logicalSize = CGFloat(width) / CGFloat(scale)
        if let panel = panel, abs(panel.frame.width - logicalSize) > 0.5 || abs(panel.frame.height - logicalSize) > 0.5 {
            let oldFrame = panel.frame
            let newOriginY = oldFrame.origin.y + oldFrame.height - logicalSize
            let newFrame = NSRect(x: oldFrame.origin.x, y: newOriginY, width: logicalSize, height: logicalSize)
            panel.setFrame(newFrame, display: true)
        }

        result(nil)
    }

    private func handleShowBall(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        let args = call.arguments as? [String: Any]
        let x = (args?["x"] as? NSNumber)?.doubleValue ?? -1
        let y = (args?["y"] as? NSNumber)?.doubleValue ?? -1

        ensurePanel()
        guard let window = panel else {
            result(nil)
            return
        }
        let origin = resolveOrigin(x: x, y: y, size: window.frame.width)
        window.setFrameOrigin(origin)

        // 非激活面板：绝不调用 NSApp.activate，避免抢占前台应用焦点。
        window.orderFrontRegardless()
        // 定位后立即判定吸附：上次会话已贴边 → 直接进入 dock 态（可能触发收起倒计时）。
        evaluateDock()
        result(nil)
    }

    private func hideBall() {
        cancelCollapse()
        panel?.orderOut(nil)
    }

    private func destroyBall() {
        cancelCollapse()
        dockEdge = .none
        isCollapsed = false
        if let observer = backingObserver {
            NotificationCenter.default.removeObserver(observer)
            backingObserver = nil
        }
        panel?.orderOut(nil)
        panel?.contentView = nil
        panel = nil
        contentView = nil
    }

    // -------------------------------------------------------------------
    // 右键菜单（原生检测右键 -> onContextMenuRequested -> Dart 组装 i18n 菜单
    // -> showContextMenu -> 原生弹出 -> onMenuAction 回传选中项）
    // -------------------------------------------------------------------

    private func handleShowContextMenu(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        guard let args = call.arguments as? [String: Any],
            let items = args["items"] as? [[String: Any]]
        else {
            result(FlutterError(code: "bad_args", message: "showContextMenu: invalid arguments", details: nil))
            return
        }

        let menu = NSMenu()
        menu.autoenablesItems = false
        for item in items {
            guard let id = (item["id"] as? NSNumber)?.intValue else { continue }
            if id == 0 {
                menu.addItem(.separator())
                continue
            }
            let label = item["label"] as? String ?? ""
            let menuItem = NSMenuItem(
                title: label,
                action: #selector(handleMenuItemSelected(_:)),
                keyEquivalent: ""
            )
            menuItem.target = self
            menuItem.representedObject = id
            menuItem.isEnabled = true
            menu.addItem(menuItem)
        }

        // nonactivatingPanel 平时不抢前台焦点；但菜单追踪需要 App 处于激活状态才能正常
        // 接收键盘/鼠标事件，此处短暂激活属用户主动右键触发的预期交互，不违背点击穿透约束。
        NSApp.activate(ignoringOtherApps: true)
        menu.popUp(positioning: nil, at: NSEvent.mouseLocation, in: nil)
        result(nil)
    }

    @objc private func handleMenuItemSelected(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? Int else { return }
        channel?.invokeMethod("onMenuAction", arguments: ["id": id])
    }

    // -------------------------------------------------------------------
    // 窗口生命周期
    // -------------------------------------------------------------------

    /// 懒创建 panel + contentView；已存在则直接复用（幂等）。
    @discardableResult
    private func ensurePanel() -> BallContentView {
        if let view = contentView {
            return view
        }

        let initialFrame = NSRect(x: 0, y: 0, width: kBallWindowSize, height: kBallWindowSize)
        let newPanel = NSPanel(
            contentRect: initialFrame,
            styleMask: [.nonactivatingPanel, .borderless],
            backing: .buffered,
            defer: false
        )
        newPanel.isOpaque = false
        newPanel.backgroundColor = .clear
        newPanel.hasShadow = false // 阴影已由 Dart 渲染烘焙进位图，避免与系统阴影叠加
        newPanel.level = .floating
        newPanel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        newPanel.isMovableByWindowBackground = false
        newPanel.isReleasedWhenClosed = false
        newPanel.ignoresMouseEvents = false
        newPanel.hidesOnDeactivate = false

        let view = BallContentView(frame: initialFrame)
        newPanel.contentView = view

        panel = newPanel
        contentView = view

        // DPI 变更通知：Dart 侧已按 devicePixelRatio 自行渲染对应缩放位图，
        // 这里仅预留钩子，供未来需要主动通知 Dart 重新拉取 scale 时使用。
        backingObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeBackingPropertiesNotification,
            object: newPanel,
            queue: .main
        ) { [weak self] _ in
            // TODO: 若未来需要原生主动驱动重绘，可在此调用 self?.channel?.invokeMethod(...)
            // 通知 Dart 当前 backingScaleFactor 变化；目前 Dart 通过 MediaQuery 自行感知，故留空。
            _ = self
        }

        return view
    }

    // -------------------------------------------------------------------
    // 坐标换算（Dart 左上角原点坐标系 <-> AppKit 左下角原点坐标系）
    // -------------------------------------------------------------------

    /// 主屏（screens[0]，全局坐标原点所在屏）可视工作区；理论上运行中的 GUI 进程恒存在。
    private func primaryVisibleFrame() -> NSRect? {
        NSScreen.screens.first?.visibleFrame
    }

    /// 主屏总高度（用于 Y 轴翻转），取 frame（非 visibleFrame）以匹配全局坐标系高度。
    private func primaryScreenHeight() -> CGFloat? {
        NSScreen.screens.first?.frame.height
    }

    /// 解析 showBall 传入坐标 -> AppKit 窗口原点（左下角）。
    /// x/y == -1 时使用默认停靠（主屏工作区右缘、垂直 40%、贴边 8px）。
    private func resolveOrigin(x: Double, y: Double, size: CGFloat) -> NSPoint {
        guard let visible = primaryVisibleFrame(), let screenHeight = primaryScreenHeight() else {
            return .zero
        }

        if x < 0 || y < 0 {
            return defaultDockOrigin(visible: visible, size: size)
        }

        // Dart 坐标：左上角原点，Y 向下 -> AppKit：左下角原点，Y 向上。
        // 窗口左上角 (x, y) -> AppKit 原点(左下角) = (x, screenHeight - y - size)
        let rawOrigin = NSPoint(x: CGFloat(x), y: screenHeight - CGFloat(y) - size)
        return clampToVisibleFrame(rawOrigin, size: size, visible: visible)
    }

    /// 默认停靠位置：主屏工作区右缘、垂直 40%、贴边 8px（AppKit 坐标）。
    private func defaultDockOrigin(visible: NSRect, size: CGFloat) -> NSPoint {
        let x = visible.maxX - size - kDockMargin
        // 垂直 40%：以"距工作区顶部 40%"衡量（与 Dart 左上角坐标系语义一致），
        // AppKit 是左下角原点，故换算为 visible.maxY - ratio*height - size/2 附近；
        // 为与 Windows 实现（y = top + height*0.4，top-left 坐标）保持视觉一致，
        // 这里先在 top-left 坐标系中算出 y，再统一转换到 AppKit 坐标。
        let topLeftY = visible.height * kDockVerticalRatio
        let appKitY = visible.maxY - topLeftY - size
        return NSPoint(x: x, y: appKitY)
    }

    /// 落屏校验：窗口原点 + 尺寸不得越出 visibleFrame，越界则吸附。
    private func clampToVisibleFrame(_ origin: NSPoint, size: CGFloat, visible: NSRect) -> NSPoint {
        let minX = visible.minX
        let maxX = max(visible.minX, visible.maxX - size)
        let minY = visible.minY
        let maxY = max(visible.minY, visible.maxY - size)
        let cx = min(max(origin.x, minX), maxX)
        let cy = min(max(origin.y, minY), maxY)
        return NSPoint(x: cx, y: cy)
    }

    /// AppKit frame -> Dart 坐标系（窗口左上角，主屏左上角为原点）。
    fileprivate func flippedTopLeft(of frame: NSRect) -> NSPoint {
        let screenHeight = primaryScreenHeight() ?? frame.maxY
        return NSPoint(x: frame.origin.x, y: screenHeight - frame.maxY)
    }

    // -------------------------------------------------------------------
    // 贴边收起（迅雷式）—— 吸附判定 / 收起展开动画 / 悬停与拖动状态机
    // -------------------------------------------------------------------

    /// 拖动释放（或 showBall 定位）后判定：球边缘距所在屏工作区左/右/上边 ≤ 阈值 → 吸附贴边。
    /// 未命中任何边 → 解除贴边。收起态与收起延时在此统一复位（同 Windows _evaluateDock）。
    fileprivate func evaluateDock() {
        guard let panel = panel, let visible = dockScreen(for: panel.frame)?.visibleFrame else { return }
        let frame = panel.frame

        var edge = DockEdge.none
        if frame.minX - visible.minX <= kDockSnapThreshold {
            edge = .left
        } else if visible.maxX - frame.maxX <= kDockSnapThreshold {
            edge = .right
        } else if visible.maxY - frame.maxY <= kDockSnapThreshold {
            edge = .top
        }

        dockEdge = edge
        isCollapsed = false
        cancelCollapse()
        guard edge != .none else { return }

        let target = dockedOrigin(edge: edge, collapsed: false, size: frame.width, visible: visible, current: frame.origin)
        animateFrameOrigin(target)

        // 吸附瞬间光标已不在球上（如 showBall 定位后）才立即起算收起倒计时；
        // 仍悬停则等 mouseExited 触发，避免刚贴边就被判定“离开”。
        let targetFrame = NSRect(x: target.x, y: target.y, width: frame.width, height: frame.height)
        if !NSMouseInRect(NSEvent.mouseLocation, targetFrame, false) {
            scheduleCollapse()
        }
    }

    /// 拖离解除贴边（mouseDragged 超阈值时调用，同 Windows「拖离即解除贴边」）。
    fileprivate func dockDidBeginDrag() {
        dockEdge = .none
        cancelCollapse()
    }

    /// 光标进入/离开球区域：进入 → 展开（如已收起）+ 取消收起倒计时；
    /// 离开 → 若已贴边且未收起，起算 800ms 收起倒计时。
    fileprivate func dockHoverChanged(isInside: Bool) {
        guard dockEdge != .none else { return }
        if isInside {
            cancelCollapse()
            expandIfCollapsed()
        } else if !isCollapsed {
            scheduleCollapse()
        }
    }

    /// 强制展开（外部拖放悬停到收起边条 / 光标重新进入时调用）。
    fileprivate func expandIfCollapsed() {
        cancelCollapse()
        guard dockEdge != .none, isCollapsed, let panel = panel else { return }
        guard let visible = dockScreen(for: panel.frame)?.visibleFrame else { return }
        isCollapsed = false
        let target = dockedOrigin(
            edge: dockEdge, collapsed: false, size: panel.frame.width, visible: visible,
            current: panel.frame.origin
        )
        animateFrameOrigin(target)
    }

    private func scheduleCollapse() {
        collapseWorkItem?.cancel()
        let workItem = DispatchWorkItem { [weak self] in
            self?.performCollapse()
        }
        collapseWorkItem = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + kDockCollapseDelay, execute: workItem)
    }

    private func cancelCollapse() {
        collapseWorkItem?.cancel()
        collapseWorkItem = nil
    }

    private func performCollapse() {
        collapseWorkItem = nil
        guard dockEdge != .none, !isCollapsed, let panel = panel else { return }
        guard let visible = dockScreen(for: panel.frame)?.visibleFrame else { return }
        isCollapsed = true
        let target = dockedOrigin(
            edge: dockEdge, collapsed: true, size: panel.frame.width, visible: visible,
            current: panel.frame.origin
        )
        animateFrameOrigin(target)
    }

    /// 贴边目标原点（AppKit 坐标，左下角为原点）。[collapsed]=false 完整贴边可见；
    /// =true 只露 kDockRevealWidth。注意 AppKit Y 轴向上为正：顶边收起 = 窗口向屏幕
    /// 外上方滑出 = origin.y 增大（与 Dart 左上角坐标系的“上”方向相反，勿混淆）。
    private func dockedOrigin(
        edge: DockEdge, collapsed: Bool, size: CGFloat, visible: NSRect, current: NSPoint
    ) -> NSPoint {
        let reveal = kDockRevealWidth
        switch edge {
        case .none:
            return current
        case .left:
            let x = collapsed ? visible.minX - size + reveal : visible.minX
            return NSPoint(x: x, y: current.y)
        case .right:
            let x = collapsed ? visible.maxX - reveal : visible.maxX - size
            return NSPoint(x: x, y: current.y)
        case .top:
            let y = collapsed ? visible.maxY - reveal : visible.maxY - size
            return NSPoint(x: current.x, y: y)
        }
    }

    /// 以 kDockAnimDuration、ease-out cubic 动画平移 panel 到 [origin]（10.9+ SDK：
    /// NSWindow.setFrameOrigin 在隐式动画上下文中可动画，animator() 代理即可驱动）。
    private func animateFrameOrigin(_ origin: NSPoint) {
        guard let panel = panel, panel.frame.origin != origin else { return }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = kDockAnimDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().setFrameOrigin(origin)
        }
    }

    /// [frame] 所在的 NSScreen：中心点落在某屏内则取该屏；否则取重叠面积最大的屏；
    /// 均无重叠时兜底主屏（多屏配置变化的边界情况）。
    private func dockScreen(for frame: NSRect) -> NSScreen? {
        let center = NSPoint(x: frame.midX, y: frame.midY)
        if let hit = NSScreen.screens.first(where: { $0.frame.contains(center) }) {
            return hit
        }
        return NSScreen.screens.max { lhs, rhs in
            overlapArea(lhs.frame, frame) < overlapArea(rhs.frame, frame)
        } ?? NSScreen.screens.first
    }

    private func overlapArea(_ a: NSRect, _ b: NSRect) -> CGFloat {
        let i = a.intersection(b)
        return i.isNull ? 0 : i.width * i.height
    }

    // -------------------------------------------------------------------
    // 原生 -> Dart 通知（由 BallContentView 调用）
    // -------------------------------------------------------------------

    fileprivate func notifyBallClicked() {
        channel?.invokeMethod("onBallClicked", arguments: nil)
    }

    fileprivate func notifyBallMoved(frame: NSRect) {
        let topLeft = flippedTopLeft(of: frame)
        channel?.invokeMethod("onBallMoved", arguments: [
            "x": Double(topLeft.x),
            "y": Double(topLeft.y),
        ])
    }

    fileprivate func notifyDragEnter() {
        channel?.invokeMethod("onDragEnter", arguments: nil)
    }

    fileprivate func notifyDragLeave() {
        channel?.invokeMethod("onDragLeave", arguments: nil)
    }

    fileprivate func notifyDropPayload(kind: String, values: [String]) {
        let totalBytes = values.reduce(0) { $0 + $1.utf8.count }
        guard totalBytes <= kMaxDropPayloadBytes else { return }
        channel?.invokeMethod("onDropPayload", arguments: [
            "kind": kind,
            "values": values,
        ])
    }

    fileprivate func notifyContextMenuRequested() {
        channel?.invokeMethod("onContextMenuRequested", arguments: nil)
    }
}

// =============================================================================
// BallContentView — 圆形命中、拖拽移动、外部拖放的哑内容视图。
// =============================================================================

final class BallContentView: NSView {
    private var dragAnchorScreenPoint: NSPoint = .zero
    private var dragAnchorWindowOrigin: NSPoint = .zero
    private var didExceedDragThreshold = false
    private var trackingArea: NSTrackingArea?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.contentsGravity = .resize
        layer?.masksToBounds = false
        registerForDraggedTypes([.fileURL, .URL, .string])
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        wantsLayer = true
        registerForDraggedTypes([.fileURL, .URL, .string])
    }

    // -------------------------------------------------------------------
    // 位图渲染
    // -------------------------------------------------------------------

    /// 将 straight-alpha RGBA 字节转为 CGImage 并贴到 layer.contents。
    func setBitmap(rgba: Data, width: Int, height: Int, scale: CGFloat) {
        let bytesPerRow = width * 4
        guard rgba.count >= bytesPerRow * height else { return }

        guard let provider = CGDataProvider(data: rgba as CFData) else { return }
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        let bitmapInfo = CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue)

        guard
            let image = CGImage(
                width: width,
                height: height,
                bitsPerComponent: 8,
                bitsPerPixel: 32,
                bytesPerRow: bytesPerRow,
                space: colorSpace,
                bitmapInfo: bitmapInfo,
                provider: provider,
                decode: nil,
                shouldInterpolate: true,
                intent: .defaultIntent
            )
        else { return }

        layer?.contentsScale = scale
        layer?.contents = image
    }

    // -------------------------------------------------------------------
    // 圆形命中测试：距 view 中心 > 28pt 返回 nil（圆外穿透）。
    // -------------------------------------------------------------------

    override func hitTest(_ point: NSPoint) -> NSView? {
        // 收起态：只露一条边条，圆形命中会漏接——改用整矩形命中（同 Windows _hitBall）。
        if FloatingBallPanel.shared.isCollapsed {
            return super.hitTest(point)
        }
        let localPoint: NSPoint
        if let superview = superview {
            localPoint = convert(point, from: superview)
        } else {
            localPoint = point
        }
        let center = NSPoint(x: bounds.midX, y: bounds.midY)
        let dx = localPoint.x - center.x
        let dy = localPoint.y - center.y
        if (dx * dx + dy * dy) > (kBallHitRadius * kBallHitRadius) {
            return nil
        }
        return super.hitTest(point)
    }

    /// AppKit 默认吞非 key 窗口的首次 mouseDown；nonactivatingPanel 几乎永远非 key，
    /// 缺此覆写点击/拖动必然静默失效。
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        true
    }

    // -------------------------------------------------------------------
    // 悬停跟踪（贴边收起/展开的驱动信号）
    // -------------------------------------------------------------------

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea = trackingArea {
            removeTrackingArea(trackingArea)
        }
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeAlways],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        trackingArea = area
    }

    override func mouseEntered(with event: NSEvent) {
        FloatingBallPanel.shared.dockHoverChanged(isInside: true)
    }

    override func mouseExited(with event: NSEvent) {
        FloatingBallPanel.shared.dockHoverChanged(isInside: false)
    }

    // -------------------------------------------------------------------
    // 手动拖动 + 点击判定
    // -------------------------------------------------------------------

    override func mouseDown(with event: NSEvent) {
        dragAnchorScreenPoint = NSEvent.mouseLocation
        dragAnchorWindowOrigin = window?.frame.origin ?? .zero
        didExceedDragThreshold = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard let window = window else { return }
        let current = NSEvent.mouseLocation
        let dx = current.x - dragAnchorScreenPoint.x
        let dy = current.y - dragAnchorScreenPoint.y
        if !didExceedDragThreshold {
            let distanceSquared = dx * dx + dy * dy
            if distanceSquared > (kDragThreshold * kDragThreshold) {
                didExceedDragThreshold = true
                FloatingBallPanel.shared.dockDidBeginDrag() // 拖离即解除贴边
            } else {
                return
            }
        }
        let newOrigin = NSPoint(x: dragAnchorWindowOrigin.x + dx, y: dragAnchorWindowOrigin.y + dy)
        window.setFrameOrigin(newOrigin)
    }

    override func mouseUp(with event: NSEvent) {
        if didExceedDragThreshold {
            if let frame = window?.frame {
                // 先回传拖动释放坐标（既有持久化语义不变），再判定吸附——
                // 吸附/收起动画会改变 frame，不应污染持久化坐标。
                FloatingBallPanel.shared.notifyBallMoved(frame: frame)
            }
            FloatingBallPanel.shared.evaluateDock()
        } else {
            FloatingBallPanel.shared.notifyBallClicked()
        }
        didExceedDragThreshold = false
    }

    /// 右键弹出菜单：通知 Dart 组装 i18n 菜单项，随后经 showContextMenu 原生弹出。
    override func rightMouseUp(with event: NSEvent) {
        // 收起态忽略右键（先悬停展开再右键，同 Windows 实现）。
        guard !FloatingBallPanel.shared.isCollapsed else { return }
        FloatingBallPanel.shared.notifyContextMenuRequested()
    }

    // -------------------------------------------------------------------
    // NSDraggingDestination（外部拖放）
    // -------------------------------------------------------------------

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        guard acceptableDragOperation(sender) else { return [] }
        FloatingBallPanel.shared.expandIfCollapsed() // 外部内容拖到收起边条 → 立即展开
        FloatingBallPanel.shared.notifyDragEnter()
        return .copy
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        acceptableDragOperation(sender) ? .copy : []
    }

    override func draggingExited(_ sender: NSDraggingInfo?) {
        FloatingBallPanel.shared.notifyDragLeave()
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let pasteboard = sender.draggingPasteboard

        // 优先级：本地文件 URL > 通用 URL/文本。同步读取 pasteboard（拖放会话结束后即失效）。
        if let fileURLs = pasteboard.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL], !fileURLs.isEmpty {
            let paths = fileURLs.map { $0.path }
            DispatchQueue.main.async {
                FloatingBallPanel.shared.notifyDropPayload(kind: "files", values: paths)
            }
            return true
        }

        if let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: nil) as? [URL], !urls.isEmpty {
            let strings = urls.map { $0.absoluteString }
            DispatchQueue.main.async {
                FloatingBallPanel.shared.notifyDropPayload(kind: "text", values: strings)
            }
            return true
        }

        if let strings = pasteboard.readObjects(forClasses: [NSString.self], options: nil) as? [String],
            !strings.isEmpty
        {
            DispatchQueue.main.async {
                FloatingBallPanel.shared.notifyDropPayload(kind: "text", values: strings)
            }
            return true
        }

        return false
    }

    private func acceptableDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let pasteboard = sender.draggingPasteboard
        return pasteboard.availableType(from: [.fileURL, .URL, .string]) != nil
    }
}
