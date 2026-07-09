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
import '../widgets/quick_download_form.dart';
import 'log_service.dart';
import 'popup_window_service.dart';
import 'tray_service.dart';

const _tag = 'ExtDownSvc';

/// 监听来自浏览器扩展/aria2 RPC/管理 API 的外部下载请求。
///
/// 架构：
/// 1. Rust HTTP server 收到浏览器扩展的下载请求
/// 2. Rust 发送 ExternalDownloadRequest 信号到 Dart
/// 3. 本服务监听该信号：
///    - 免打扰开启 → 不弹窗，直接按默认设置创建任务；
///    - 否则首选**独立小窗**（PopupWindowService，原生窗口承载第二引擎，
///      置顶且不抢主窗口前台）；
///    - 原生宿主不可用时回退主窗口内快速下载对话框（恢复主窗口并前台激活）
/// 4. 用户确认后由主引擎发送 ConfirmExternalDownload/BatchCreateTask 信号
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

    // 音视频轨对（浏览器扩展嗅探到离散 video/audio 轨，通用语义，非站点
    // 特判）：browser 侧已完成清晰度确认。免打扰开启时宿主直接建任务
    // （不拆分为多任务，audioUrl 原样传 Rust 走离散轨道下载 + mux 旁路）；
    // 免打扰关闭时落入下方弹窗路径，让用户在 FluxDown 内二次确认（audioUrl
    // 经弹窗/小窗独立通道透传，不进 URL 文本、不被换行拆分）。
    final trackPairSilent =
        (SettingsProvider.globalInstance ?? settingsProvider)
            .silentDownloadEnabled;
    if (req.audioUrl.isNotEmpty && trackPairSilent) {
      final trackSettings = SettingsProvider.globalInstance ?? settingsProvider;
      final requestedDir = req.saveDir.trim();
      final matchedDir = trackSettings.resolveCategorySaveDir(
        req.filename,
        url: req.url,
      );
      final saveDir = requestedDir.isNotEmpty
          ? requestedDir
          : (matchedDir.isNotEmpty
                ? matchedDir
                : trackSettings.effectiveDefaultSaveDir);
      logInfo(
        _tag,
        'track-pair request, creating task directly: '
        'url=${req.url}, audioUrl=${req.audioUrl}, saveDir=$saveDir',
      );
      ConfirmExternalDownload(
        url: req.url,
        saveDir: saveDir,
        fileName: req.filename,
        segments: trackSettings.defaultSegments,
        cookies: req.cookies,
        referrer: req.referrer,
        hintFileSize: req.fileSize,
        proxyUrl: '',
        userAgent: '',
        queueId: trackSettings.defaultQueueId,
        audioUrl: req.audioUrl,
      ).sendSignalToRust();
      return;
    }

    // 免打扰下载：不弹确认框、不抢前台，直接按默认设置创建任务。
    // 优先 globalInstance（始终反映用户最新设置），fallback 到启动时实例。
    final silentSettings = SettingsProvider.globalInstance ?? settingsProvider;
    if (silentSettings.silentDownloadEnabled) {
      // url 可能是换行连接的多条 URL（aria2 addUri 多 URI / 脚本接管批量），
      // 与快速下载对话框共用同一解析器：单条走 ConfirmExternalDownload
      // （保留 Rust 侧按 URL 缓存的请求上下文），多条走 BatchCreateTask。
      final entries = parseQuickDownloadEntries(req.url);
      // 请求方显式指定的目录（aria2 dir / 接管 saveDir）优先于分类匹配。
      final requestedDir = req.saveDir.trim();
      final matchedDir = silentSettings.resolveCategorySaveDir(
        req.filename,
        url: req.url,
      );
      final saveDir = requestedDir.isNotEmpty
          ? requestedDir
          : (matchedDir.isNotEmpty
                ? matchedDir
                : silentSettings.effectiveDefaultSaveDir);
      if (entries.isNotEmpty && saveDir.isNotEmpty) {
        logInfo(
          _tag,
          'silent download enabled, creating ${entries.length} task(s) '
          'directly: saveDir=$saveDir',
        );
        final segments = silentSettings.defaultSegments;
        final queueId = silentSettings.defaultQueueId;
        if (entries.length == 1) {
          final entry = entries.first;
          ConfirmExternalDownload(
            url: entry.url,
            saveDir: saveDir,
            fileName: req.filename.isNotEmpty ? req.filename : entry.fileName,
            segments: segments,
            cookies: req.cookies,
            referrer: req.referrer,
            hintFileSize: req.fileSize,
            proxyUrl: '',
            userAgent: '',
            queueId: queueId,
            audioUrl: entry.audioUrl,
          ).sendSignalToRust();
        } else {
          BatchCreateTask(
            entries: entries
                .map(
                  (e) => UrlEntry(
                    url: e.url,
                    fileName: e.fileName,
                    checksum: e.checksum,
                    audioUrl: e.audioUrl,
                  ),
                )
                .toList(),
            saveDir: saveDir,
            segments: segments,
            proxyUrl: '',
            userAgent: '',
            queueId: queueId,
            cookies: req.cookies,
            referrer: req.referrer,
          ).sendSignalToRust();
        }
        return;
      }
      // 无有效 URL 或无可用保存目录 — 降级为弹框，让用户处理。
      logError(_tag, 'silent download: no entries or save dir, falling back');
    }

    // ── 首选路径：独立小窗（不抢主窗口前台）──
    // 小窗仍可见时忽略新请求（与主窗口对话框的去重语义一致）。
    if (PopupWindowService.instance.isVisible) {
      logInfo(_tag, 'popup still open, ignoring request');
      return;
    }
    if (!_dialogOpen) {
      final popupSettings = SettingsProvider.globalInstance ?? settingsProvider;
      final requestedDir = req.saveDir.trim();
      final matchedDir = popupSettings.resolveCategorySaveDir(
        req.filename,
        url: req.url,
      );
      final resolvedDir = requestedDir.isNotEmpty
          ? requestedDir
          : (matchedDir.isNotEmpty
                ? matchedDir
                : popupSettings.effectiveDefaultSaveDir);
      final popupShown = await PopupWindowService.instance.tryShow(
        req: req,
        resolvedSaveDir: resolvedDir,
      );
      if (popupShown) return;
      logInfo(_tag, 'popup unavailable, falling back to in-window dialog');
    }

    // ── 回退路径：主窗口内快速下载对话框 ──
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
      final requestedDir = req.saveDir.trim();
      showQuickDownloadDialog(
        context,
        url: req.url,
        filename: req.filename,
        fileSize: req.fileSize.toInt(),
        mimeType: req.mimeType,
        cookies: req.cookies,
        referrer: req.referrer,
        defaultSaveDir: requestedDir.isNotEmpty
            ? requestedDir
            : effectiveSettings.effectiveDefaultSaveDir,
        defaultQueueId: effectiveSettings.defaultQueueId,
        saveDirFromRequest: requestedDir.isNotEmpty,
        audioUrl: req.audioUrl,
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
    // macOS：走原生可靠激活序列（见 restoreMainWindow）；其它平台 show()+restore()
    if (Platform.isMacOS) {
      await restoreMainWindow();
      return;
    }
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
