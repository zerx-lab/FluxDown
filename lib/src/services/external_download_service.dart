import 'dart:async';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';
import 'package:flutter/material.dart';
import 'package:rinf/rinf.dart';
import 'package:window_manager/window_manager.dart';

import '../bindings/bindings.dart';
import '../models/settings_provider.dart';
import '../widgets/quick_download_dialog.dart';
import 'log_service.dart';

const _tag = 'ExtDownSvc';

/// 监听来自浏览器扩展的外部下载请求，弹出主窗口内的快速下载确认对话框。
///
/// 架构：
/// 1. Rust HTTP server 收到浏览器扩展的下载请求
/// 2. Rust 发送 ExternalDownloadRequest 信号到 Dart
/// 3. 本服务监听该信号，在主窗口内弹出 Dialog（无需创建独立子窗口）
/// 4. 用户在 Dialog 中确认下载参数
/// 5. Dialog 直接发送 ConfirmExternalDownload 信号到 Rust
class ExternalDownloadService {
  static ExternalDownloadService? _instance;

  /// HomePage 注册此回调，收到外部下载请求时自动从设置页切回首页。
  static VoidCallback? onNavigateToHome;

  final SettingsProvider settingsProvider;
  final GlobalKey<NavigatorState> navigatorKey;
  StreamSubscription<RustSignalPack<ExternalDownloadRequest>>? _sub;
  bool _dialogOpen = false;

  ExternalDownloadService._({
    required this.settingsProvider,
    required this.navigatorKey,
  });

  /// 初始化单例。应在 app 启动时调用一次。
  static void init({
    required SettingsProvider settingsProvider,
    required GlobalKey<NavigatorState> navigatorKey,
  }) {
    logInfo(_tag, 'init');
    _instance?._teardown();
    _instance = ExternalDownloadService._(
      settingsProvider: settingsProvider,
      navigatorKey: navigatorKey,
    );
    _instance!._startListening();
  }

  static void shutdown() {
    logInfo(_tag, 'shutdown');
    _instance?._teardown();
    _instance = null;
  }

  void _teardown() {
    logInfo(_tag, '_teardown');
    _sub?.cancel();
  }

  void _startListening() {
    _sub = ExternalDownloadRequest.rustSignalStream.listen(_onRequest);
  }

  void _onRequest(RustSignalPack<ExternalDownloadRequest> pack) async {
    final req = pack.message;
    logInfo(
      _tag,
      'received request: url=${req.url}, filename=${req.filename}, size=${req.fileSize}',
    );

    // 防止重复弹窗 — 检查标志并验证 Navigator 上是否仍有弹窗路由
    if (_dialogOpen) {
      bool stillOpen = false;
      final nav = navigatorKey.currentState;
      if (nav != null) {
        try {
          nav.popUntil((route) {
            stillOpen = route is PopupRoute;
            return true; // 不实际 pop，仅检测最顶层路由类型
          });
        } catch (_) {}
      }
      if (stillOpen) {
        logInfo(_tag, 'dialog still open, ignoring request');
        return;
      }
      // 弹窗已关闭但标志未重置，清除后继续处理新请求
      _dialogOpen = false;
    }

    final context = navigatorKey.currentContext;
    if (context == null) {
      logError(_tag, 'navigatorKey has no context, cannot show dialog');
      return;
    }

    _dialogOpen = true;

    try {
      // 如果当前在设置页，先切回首页
      onNavigateToHome?.call();

      // 确保主窗口可见并强制前台激活
      await _bringWindowToFront();

      if (!context.mounted) {
        logError(_tag, 'context not mounted after window restore');
        _dialogOpen = false;
        return;
      }

      logInfo(_tag, 'showing quick download dialog...');
      // 优先使用 globalInstance（HomePage 的主 SettingsProvider，始终反映用户最新设置）。
      // ExternalDownloadService 持有的 settingsProvider 是启动时创建的独立实例，
      // 不会收到用户在 UI 中修改设置后的变更通知，仅作为 fallback。
      final effectiveSettings =
          SettingsProvider.globalInstance ?? settingsProvider;
      showQuickDownloadDialog(
        context,
        url: req.url,
        filename: req.filename,
        fileSize: req.fileSize.toInt(),
        mimeType: req.mimeType,
        cookies: req.cookies,
        referrer: req.referrer,
        defaultSaveDir: effectiveSettings.effectiveDefaultSaveDir,
        defaultQueueId: effectiveSettings.defaultQueueId,
      );
      logInfo(_tag, 'dialog shown');
    } catch (e, stack) {
      logError(_tag, 'failed to show dialog', e, stack);
      _dialogOpen = false;
    }
  }

  /// 强制将主窗口带到前台。
  ///
  /// Windows 限制后台进程调用 SetForegroundWindow，单纯的
  /// windowManager.show() + focus() 在窗口被其他应用遮挡时
  /// 可能只闪烁任务栏图标而不真正弹到前台。
  ///
  /// 使用经典 Win32 技巧：先 HWND_TOPMOST 再 HWND_NOTOPMOST，
  /// 强制窗口到最上层后立刻取消置顶，效果等价于用户手动点击任务栏。
  Future<void> _bringWindowToFront() async {
    // 先确保窗口可见（从托盘/最小化恢复）
    await windowManager.show();
    await windowManager.restore();

    if (Platform.isWindows) {
      _forceActivateWindow();
    }

    await windowManager.focus();
  }
}

// ---------------------------------------------------------------------------
// Win32 FFI — 仅用于强制前台激活
// ---------------------------------------------------------------------------

const int _swpNoMove = 0x0002;
const int _swpNoSize = 0x0001;
const int _hwndTopmost = -1;
const int _hwndNotopmost = -2;
const int _swpShowWindow = 0x0040;

typedef _FindWindowNative =
    IntPtr Function(Pointer<Int8> lpClassName, Pointer<Int8> lpWindowName);
typedef _FindWindowDart =
    int Function(Pointer<Int8> lpClassName, Pointer<Int8> lpWindowName);

typedef _GetForegroundWindowNative = IntPtr Function();
typedef _GetForegroundWindowDart = int Function();

typedef _GetWindowThreadProcessIdNative =
    Uint32 Function(IntPtr hWnd, Pointer<Uint32> lpdwProcessId);
typedef _GetWindowThreadProcessIdDart =
    int Function(int hWnd, Pointer<Uint32> lpdwProcessId);

typedef _AttachThreadInputNative =
    Int32 Function(Uint32 idAttach, Uint32 idAttachTo, Int32 fAttach);
typedef _AttachThreadInputDart =
    int Function(int idAttach, int idAttachTo, int fAttach);

typedef _SetForegroundWindowNative = Int32 Function(IntPtr hWnd);
typedef _SetForegroundWindowDart = int Function(int hWnd);

typedef _SetWindowPosNative =
    Int32 Function(
      IntPtr hWnd,
      IntPtr hWndInsertAfter,
      Int32 x,
      Int32 y,
      Int32 cx,
      Int32 cy,
      Uint32 uFlags,
    );
typedef _SetWindowPosDart =
    int Function(
      int hWnd,
      int hWndInsertAfter,
      int x,
      int y,
      int cx,
      int cy,
      int uFlags,
    );

typedef _GetCurrentThreadIdNative = Uint32 Function();
typedef _GetCurrentThreadIdDart = int Function();

final _user32 = DynamicLibrary.open('user32.dll');
final _kernel32 = DynamicLibrary.open('kernel32.dll');

final _getForegroundWindow = _user32
    .lookupFunction<_GetForegroundWindowNative, _GetForegroundWindowDart>(
      'GetForegroundWindow',
    );

final _getWindowThreadProcessId = _user32
    .lookupFunction<
      _GetWindowThreadProcessIdNative,
      _GetWindowThreadProcessIdDart
    >('GetWindowThreadProcessId');

final _attachThreadInput = _user32
    .lookupFunction<_AttachThreadInputNative, _AttachThreadInputDart>(
      'AttachThreadInput',
    );

final _setForegroundWindow = _user32
    .lookupFunction<_SetForegroundWindowNative, _SetForegroundWindowDart>(
      'SetForegroundWindow',
    );

final _setWindowPos = _user32
    .lookupFunction<_SetWindowPosNative, _SetWindowPosDart>('SetWindowPos');

final _getCurrentThreadId = _kernel32
    .lookupFunction<_GetCurrentThreadIdNative, _GetCurrentThreadIdDart>(
      'GetCurrentThreadId',
    );

final _findWindow = _user32.lookupFunction<_FindWindowNative, _FindWindowDart>(
  'FindWindowA',
);

/// 使用 Win32 API 强制将当前应用窗口激活到前台。
///
/// 两层策略：
/// 1. AttachThreadInput — 将当前线程附加到前台线程的输入队列，
///    使 SetForegroundWindow 不被系统拒绝。
/// 2. TOPMOST → NOTOPMOST — 先置顶再取消，强制 Z-order 到最上层。
void _forceActivateWindow() {
  // 找到 Flutter 主窗口句柄（FlutterWindow 类名固定为 FLUTTER_RUNNER_WIN32_WINDOW）
  final className = 'FLUTTER_RUNNER_WIN32_WINDOW'.toNativeUtf8();
  final hwnd = _findWindow(className.cast(), Pointer.fromAddress(0));
  calloc.free(className);

  if (hwnd == 0) return;

  final foregroundHwnd = _getForegroundWindow();

  if (foregroundHwnd != 0 && foregroundHwnd != hwnd) {
    // 获取前台窗口的线程 ID
    final pidPtr = calloc<Uint32>();
    final foregroundThreadId = _getWindowThreadProcessId(
      foregroundHwnd,
      pidPtr,
    );
    final currentThreadId = _getCurrentThreadId();
    calloc.free(pidPtr);

    // 附加到前台线程的输入队列，使 SetForegroundWindow 被系统接受
    if (foregroundThreadId != currentThreadId) {
      _attachThreadInput(currentThreadId, foregroundThreadId, 1);
      _setForegroundWindow(hwnd);
      _attachThreadInput(currentThreadId, foregroundThreadId, 0);
    } else {
      _setForegroundWindow(hwnd);
    }
  }

  // 兜底：TOPMOST → NOTOPMOST 强制 Z-order 刷新
  _setWindowPos(
    hwnd,
    _hwndTopmost,
    0,
    0,
    0,
    0,
    _swpNoMove | _swpNoSize | _swpShowWindow,
  );
  _setWindowPos(
    hwnd,
    _hwndNotopmost,
    0,
    0,
    0,
    0,
    _swpNoMove | _swpNoSize | _swpShowWindow,
  );
}
