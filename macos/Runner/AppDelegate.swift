import Cocoa
import FlutterMacOS
import UserNotifications

@main
class AppDelegate: FlutterAppDelegate, UNUserNotificationCenterDelegate {
  override func applicationDidFinishLaunching(_ notification: Notification) {
    UNUserNotificationCenter.current().delegate = self
    super.applicationDidFinishLaunching(notification)
  }

  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return false
  }

  /// 点击 Dock 图标时恢复主窗口。
  /// 主窗口可能处于两种「不可见」状态：
  ///   1. miniaturized（黄色按钮最小化到 Dock）— 需要 deminiaturize
  ///   2. orderOut（红色按钮关闭到托盘）— AppKit 默认 reopen 不会重新显示
  /// 两种状态都在此显式恢复，避免用户只能退出重开（issue #420）。
  /// 注意：不遍历 NSApp.windows —— 悬浮球 FloatingBallPanel 也在其中，不能被激活聚焦。
  override func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
    guard let window = mainFlutterWindow else { return true }
    if window.isMiniaturized {
      window.deminiaturize(nil)
    }
    if !window.isVisible {
      window.setIsVisible(true)
    }
    window.makeKeyAndOrderFront(self)
    NSApp.activate(ignoringOtherApps: true)
    return false
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}
