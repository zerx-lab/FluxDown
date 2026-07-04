import Flutter
import UIKit
import flutter_foreground_task

@main
@objc class AppDelegate: FlutterAppDelegate {
  private var shareChannel: FlutterMethodChannel?
  /// 冷启动时暂存的分享 URL，等 Dart 侧首次 getInitialShare 取走。
  private var pendingShare: String?

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    GeneratedPluginRegistrant.register(with: self)

    // flutter_foreground_task：后台 isolate 注册插件 + 前台通知展示
    SwiftFlutterForegroundTaskPlugin.setPluginRegistrantCallback { registry in
      GeneratedPluginRegistrant.register(with: registry)
    }
    if #available(iOS 10.0, *) {
      UNUserNotificationCenter.current().delegate = self as? UNUserNotificationCenterDelegate
    }

    if let controller = window?.rootViewController as? FlutterViewController {
      let channel = FlutterMethodChannel(
        name: "com.fluxdown/share",
        binaryMessenger: controller.binaryMessenger
      )
      channel.setMethodCallHandler { [weak self] call, result in
        if call.method == "getInitialShare" {
          result(self?.pendingShare)
          self?.pendingShare = nil
        } else {
          result(FlutterMethodNotImplemented)
        }
      }
      shareChannel = channel
    }

    // 冷启动通过 URL scheme 打开时携带的链接
    if let url = launchOptions?[.url] as? URL {
      pendingShare = url.absoluteString
    }

    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  /// 应用运行中经 URL scheme（fluxdown:// / magnet:）唤起。
  override func application(
    _ app: UIApplication,
    open url: URL,
    options: [UIApplication.OpenURLOptionsKey: Any] = [:]
  ) -> Bool {
    let shared = url.absoluteString
    if let channel = shareChannel {
      channel.invokeMethod("onShare", arguments: shared)
    } else {
      pendingShare = shared
    }
    return true
  }
}
