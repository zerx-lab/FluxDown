/// 悬浮球统一服务（方案 S0.5）— 进程生命周期，与 TrayService 同级。
///
/// ## 职责
/// - enable()/disable() 状态机（幂等，布尔在途锁）
/// - 平台分发：Windows = FFI 分层窗口；macOS/Linux = MethodChannel 推位图
/// - 载荷分发：URL → QuickDownloadDialog 预填；.torrent → torrent 流程；
///   其他本地文件 → 丢弃并 logInfo
/// - 挂起/唤醒：Dart 心跳 Timer 检测墙钟跳变 >30s → 全量重绘 + 坐标校验
///
/// ## MethodChannel 协议（A6，`com.fluxdown/floating_ball`）
/// Dart→原生：pushBitmap / showBall / hideBall / destroyBall / registerDropTarget
/// 原生→Dart：onDropPayload / onBallClicked / onBallMoved / onCapability
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:window_manager/window_manager.dart';

import '../../i18n/locale_provider.dart';
import '../../models/download_controller.dart';
import '../../models/settings_provider.dart';
import '../../theme/theme_provider.dart';
import '../../widgets/quick_download_dialog.dart';
import '../app_icon_service.dart';
import '../log_service.dart';
import '../tray_service.dart';
import 'floating_ball_controller.dart';
import 'floating_ball_renderer.dart';
import 'wayland_degradation_service.dart';
import 'win32_ball_window.dart';

const _tag = 'FloatBall';

/// URL 识别正则（同 new_download_dialog.dart 先例）
final _urlRegex = RegExp(r'(https?|ftp)://\S+', caseSensitive: false);

/// Linux 能力探测结果
enum LinuxBallCapability { unknown, x11, wayland }

class FloatingBallService {
  FloatingBallService._();
  static final instance = FloatingBallService._();

  static const _channel = MethodChannel('com.fluxdown/floating_ball');

  SettingsProvider? _settings;
  ThemeProvider? _theme;
  GlobalKey<NavigatorState>? _navigatorKey;
  FloatingBallController? _controller;

  bool _enabled = false;
  bool _transitioning = false; // 在途锁（防快速反复切换交叠）
  LinuxBallCapability _linuxCapability = LinuxBallCapability.unknown;

  // 唤醒检测（S0.5：墙钟跳变 >30s = 唤醒）
  Timer? _heartbeat;
  DateTime _lastBeat = DateTime.now();

  // 位图缓存（A7 缓存键：variant + themeGeneration + dpiScale）
  final Map<String, BallImage> _staticCache = {};

  /// 有效设置实例 — 优先 globalInstance（HomePage 主实例，设置页读写的
  /// 就是它）；init 注入的 _settingsForExternal 仅作 fallback。
  /// 双实例不同步是设置页 switch 不刷新的根因（用户实测反馈）。
  SettingsProvider? get _effectiveSettings =>
      SettingsProvider.globalInstance ?? _settings;

  /// Linux Wayland 会话下悬浮球不可用（S3.4 降级）。
  bool get isDegraded =>
      Platform.isLinux && _linuxCapability == LinuxBallCapability.wayland;

  LinuxBallCapability get linuxCapability => _linuxCapability;

  /// 初始化 — 必须在 SettingsProvider 配置加载完成后调用（S0.5 初始化钩子）。
  void init({
    required SettingsProvider settings,
    required ThemeProvider theme,
    required GlobalKey<NavigatorState> navigatorKey,
  }) {
    _settings = settings;
    _theme = theme;
    _navigatorKey = navigatorKey;
    _channel.setMethodCallHandler(_onNativeCall);

    if (settings.floatingBallEnabled) {
      // Linux 需先等 onCapability；Windows/macOS 直接启用
      if (Platform.isLinux) {
        _requestLinuxCapability();
      } else {
        enable();
      }
    }
    logInfo(_tag, 'init done, enabled=${settings.floatingBallEnabled}');
  }

  // ===========================================================================
  // enable / disable 状态机
  // ===========================================================================

  Future<void> enable() async {
    if (_enabled || _transitioning) return;
    final settings = _settings;
    final theme = _theme;
    if (settings == null || theme == null) {
      logError(_tag, 'enable() before init()');
      return;
    }
    final downloads = DownloadController.globalInstance;
    if (downloads == null) {
      logInfo(_tag, 'enable() deferred: DownloadController not ready');
      return;
    }
    if (isDegraded) {
      logInfo(_tag, 'enable() skipped: wayland degraded mode');
      return;
    }
    _transitioning = true;
    try {
      // 0. 预解码 logo（幂等；idle 态球心图标，跟随应用图标自定义）
      await ensureBallLogoLoaded();
      AppIconService.instance.addListener(_onAppIconChanged);

      // 1. 订阅数据层
      _controller = FloatingBallController(downloads: downloads, theme: theme)
        ..addListener(_onDataChanged);

      // 2. 读坐标（哨兵 -1 → 默认停靠）→ 校验
      final (x, y) = _resolvePosition();

      // 3. 创建窗口
      if (Platform.isWindows) {
        final win = Win32BallWindow.instance
          ..onClicked = _onBallClicked
          ..onMoved = _onBallMoved
          ..onContextMenu = _onBallContextMenu
          ..onDpiChanged = (_) => _rerenderAll();
        win.create(x: x, y: y);
        await _renderAndPush();
        win.show();
        // 4. C++ 侧注册 IDropTarget（S1.2）。失败仅降级（球保留展示/
        // 点击/拖动，仅拖放不可用）—— 不拆整球。
        try {
          await _channel.invokeMethod('registerDropTarget', {
            'hwnd': Win32BallWindow.instance.hwnd,
          });
        } catch (e) {
          logError(_tag, 'registerDropTarget failed (drop disabled)', e);
        }
      } else {
        await _channel.invokeMethod('showBall', {
          'x': x.toDouble(),
          'y': y.toDouble(),
        });
        await _renderAndPush();
      }

      // 5. 心跳（唤醒检测）
      _startHeartbeat();

      _enabled = true;
      logInfo(_tag, 'enabled at ($x,$y)');
    } catch (e, stack) {
      logError(_tag, 'enable failed', e, stack);
      _teardown();
    } finally {
      _transitioning = false;
    }
  }

  Future<void> disable() async {
    if (!_enabled || _transitioning) return;
    _transitioning = true;
    try {
      _teardown();
      logInfo(_tag, 'disabled');
    } finally {
      _transitioning = false;
    }
  }

  /// 应用退出收口（S4.4 — _performGracefulExit 调用）。
  void destroy() {
    _teardown();
    _channel.setMethodCallHandler(null);
    logInfo(_tag, 'destroy: done');
  }

  void _teardown() {
    _heartbeat?.cancel();
    _heartbeat = null;
    AppIconService.instance.removeListener(_onAppIconChanged);
    _controller?.removeListener(_onDataChanged);
    _controller?.dispose();
    _controller = null;
    _staticCache.clear();
    if (Platform.isWindows) {
      // RevokeDragDrop 在 C++ destroyBall 分支处理
      if (Win32BallWindow.instance.isCreated) {
        unawaited(
          _channel.invokeMethod('unregisterDropTarget').catchError((Object e) {
            logError(_tag, 'unregisterDropTarget failed', e);
            return null;
          }),
        );
      }
      Win32BallWindow.instance.destroy();
    } else {
      unawaited(
        _channel.invokeMethod('destroyBall').catchError((Object e) {
          logError(_tag, 'destroyBall failed', e);
          return null;
        }),
      );
    }
    _enabled = false;
  }

  /// 设置开关变更入口（设置页/托盘菜单调用）。
  void setEnabled(bool value) {
    _effectiveSettings?.setFloatingBallEnabled(value);
    if (value) {
      if (Platform.isLinux && _linuxCapability == LinuxBallCapability.unknown) {
        _requestLinuxCapability();
      } else {
        unawaited(enable());
      }
    } else {
      unawaited(disable());
    }
  }

  // ===========================================================================
  // 渲染管线
  // ===========================================================================

  void _onDataChanged() {
    if (!_enabled) return;
    unawaited(_renderAndPush());
  }

  /// 应用图标切换（设置-外观）→ 重载球心 logo 并整体重绘
  void _onAppIconChanged() {
    unawaited(_refreshLogo());
  }

  Future<void> _refreshLogo() async {
    await ensureBallLogoLoaded();
    if (!_enabled) return;
    await _rerenderAll();
  }

  Future<void> _rerenderAll() async {
    _staticCache.clear();
    await _renderAndPush();
  }

  bool _renderInFlight = false;
  bool _renderQueued = false;

  /// 渲染当前状态一帧并推送（串行化：进行中则排队一次）。
  Future<void> _renderAndPush() async {
    if (_renderInFlight) {
      _renderQueued = true;
      return;
    }
    _renderInFlight = true;
    try {
      do {
        _renderQueued = false;
        await _renderOnce();
      } while (_renderQueued);
    } finally {
      _renderInFlight = false;
    }
  }

  Future<void> _renderOnce() async {
    final controller = _controller;
    final theme = _theme;
    if (controller == null || theme == null) return;

    final scale = Platform.isWindows
        ? Win32BallWindow.instance.scale
        : (_navigatorKey?.currentContext != null
              ? MediaQuery.of(_navigatorKey!.currentContext!).devicePixelRatio
              : 1.0);
    final dark = _isDarkNoContext();
    final tokens = theme.tokensFor(dark: dark);
    final st = controller.state;

    final BallImage image;
    if (_dragHover) {
      image = await _staticVariant(
        BallVariant.dragTarget,
        tokens,
        scale,
        controller.themeGeneration,
      );
    } else if (st.isActive) {
      // 动态层不缓存（数据驱动，每帧内容不同）
      image = await renderBallImage(
        variant: BallVariant.active,
        tokens: tokens,
        scale: scale,
        activeSpec: st.activeSpec,
      );
    } else {
      image = await _staticVariant(
        BallVariant.idle,
        tokens,
        scale,
        controller.themeGeneration,
      );
    }

    if (Platform.isWindows) {
      Win32BallWindow.instance.pushImage(image);
    } else {
      await _channel.invokeMethod('pushBitmap', {
        'bytes': image.rgba,
        'width': image.width,
        'height': image.height,
        'scale': scale,
      });
    }
  }

  Future<BallImage> _staticVariant(
    BallVariant variant,
    tokens,
    double scale,
    int themeGen,
  ) async {
    final key = '${variant.name}#$themeGen@$scale';
    final cached = _staticCache[key];
    if (cached != null) return cached;
    // 主题/DPI 变更 → 整体失效（A7 缓存键裁决）
    _staticCache.removeWhere((k, _) => !k.endsWith('#$themeGen@$scale'));
    final image = await renderBallImage(
      variant: variant,
      tokens: tokens,
      scale: scale,
    );
    _staticCache[key] = image;
    return image;
  }

  bool _isDarkNoContext() {
    final ctx = _navigatorKey?.currentContext;
    if (ctx != null && ctx.mounted) {
      return _theme?.isDark(ctx) ?? true;
    }
    return true;
  }

  // ===========================================================================
  // 交互回调
  // ===========================================================================

  bool _dragHover = false;

  void _onBallClicked() {
    unawaited(_restoreMainWindow());
  }

  /// 右键菜单（Windows：原生 TrackPopupMenuEx；macOS/Linux：原生层自行弹出，
  /// 经 onMenuAction 回传）。
  void _onBallContextMenu() {
    if (!Platform.isWindows) return;
    final s = currentS;
    final selected = Win32BallWindow.instance.showContextMenu([
      (1, s.resumeAll),
      (2, s.pauseAll),
      (0, ''),
      (3, s.trayShowWindow),
      (4, s.hideFloatingBall),
      (0, ''),
      (5, s.trayExit),
    ]);
    _dispatchMenuAction(selected);
  }

  void _dispatchMenuAction(int id) {
    final downloads = DownloadController.globalInstance;
    switch (id) {
      case 1:
        downloads?.resumeAll();
      case 2:
        downloads?.pauseAll();
      case 3:
        unawaited(_restoreMainWindow());
      case 4:
        setEnabled(false);
        unawaited(TrayService.instance.refreshMenu()); // 同步托盘复选状态
      case 5:
        // 复用托盘退出链路（onExitApp → _performGracefulExit）
        final exit = TrayService.instance.onExitApp;
        if (exit != null) unawaited(exit());
      default:
        break; // 0 = 取消
    }
  }

  Future<void> _restoreMainWindow() async {
    try {
      final visible = await windowManager.isVisible();
      await windowManager.show();
      await windowManager.focus();
      logInfo(_tag, 'main window restored (wasVisible=$visible)');
    } catch (e, stack) {
      logError(_tag, 'restore main window failed', e, stack);
    }
  }

  void _onBallMoved(double x, double y) {
    // 转场期垃圾坐标防护（S0.5：-32000 类值不落盘）
    if (x < -500 || x > 20000 || y < -500 || y > 20000) {
      logInfo(_tag, 'onBallMoved: rejected garbage coords ($x,$y)');
      return;
    }
    _effectiveSettings?.setFloatingBallPosition(x, y);
    logInfo(_tag, 'ball moved to ($x,$y)');
  }

  (int, int) _resolvePosition() {
    final s = _effectiveSettings!;
    if (Platform.isWindows) {
      if (s.floatingBallX < 0 || s.floatingBallY < 0) {
        return Win32BallWindow.defaultDockPosition();
      }
      return Win32BallWindow.clampToWorkArea(
        s.floatingBallX.round(),
        s.floatingBallY.round(),
      );
    }
    // macOS/Linux：原生层负责落屏校验，Dart 只传原始值（-1 = 原生默认停靠）
    return (s.floatingBallX.round(), s.floatingBallY.round());
  }

  // ===========================================================================
  // 原生 → Dart（A6 协议）
  // ===========================================================================

  Future<dynamic> _onNativeCall(MethodCall call) async {
    switch (call.method) {
      case 'onDropPayload':
        final args = (call.arguments as Map).cast<String, dynamic>();
        final kind = args['kind'] as String? ?? 'text';
        final values = (args['values'] as List<dynamic>? ?? const [])
            .cast<String>();
        _handleDropPayload(kind, values);
      case 'onBallClicked':
        _onBallClicked();
      case 'onBallMoved':
        final args = (call.arguments as Map).cast<String, dynamic>();
        _onBallMoved(
          (args['x'] as num).toDouble(),
          (args['y'] as num).toDouble(),
        );
      case 'onDragEnter':
        _dragHover = true;
        if (Platform.isWindows) {
          Win32BallWindow.instance.expandIfCollapsed();
        }
        unawaited(_renderAndPush());
      case 'onDragLeave':
        _dragHover = false;
        unawaited(_renderAndPush());
      case 'onContextMenuRequested':
        // macOS/Linux：原生检测到右键 → Dart 组装 i18n 菜单 → 原生弹出
        final s = currentS;
        unawaited(
          _channel
              .invokeMethod('showContextMenu', {
                'items': [
                  {'id': 1, 'label': s.resumeAll},
                  {'id': 2, 'label': s.pauseAll},
                  {'id': 0, 'label': ''},
                  {'id': 3, 'label': s.trayShowWindow},
                  {'id': 4, 'label': s.hideFloatingBall},
                  {'id': 0, 'label': ''},
                  {'id': 5, 'label': s.trayExit},
                ],
              })
              .catchError((Object e) {
                logError(_tag, 'showContextMenu failed', e);
                return null;
              }),
        );
      case 'onMenuAction':
        final args = (call.arguments as Map).cast<String, dynamic>();
        _dispatchMenuAction((args['id'] as num?)?.toInt() ?? 0);
      case 'onCapability':
        final args = (call.arguments as Map).cast<String, dynamic>();
        final mode = args['mode'] as String? ?? 'wayland';
        _linuxCapability = mode == 'x11'
            ? LinuxBallCapability.x11
            : LinuxBallCapability.wayland;
        logInfo(_tag, 'linux capability: $mode');
        if (isDegraded) {
          // Wayland：激活降级通道（托盘 setTitle 速度 + 剪贴板形态③）
          final s = _settings;
          final nav = _navigatorKey;
          if (s != null && nav != null) {
            WaylandDegradationService.instance.activate(
              settings: s,
              navigatorKey: nav,
            );
          }
        } else if (_effectiveSettings?.floatingBallEnabled == true) {
          unawaited(enable());
        }
      default:
        logInfo(_tag, 'unknown native call: ${call.method}');
    }
    return null;
  }

  void _requestLinuxCapability() {
    unawaited(
      _channel.invokeMethod('queryCapability').catchError((Object e) {
        logError(_tag, 'queryCapability failed', e);
        return null;
      }),
    );
  }

  // ===========================================================================
  // 载荷分发（S0.5）
  // ===========================================================================

  void _handleDropPayload(String kind, List<String> values) {
    logInfo(_tag, 'drop payload: kind=$kind count=${values.length}');
    _dragHover = false;
    unawaited(_renderAndPush());

    if (kind == 'files') {
      var accepted = 0;
      for (final path in values) {
        if (path.toLowerCase().endsWith('.torrent')) {
          final saveDir = _effectiveSettings?.effectiveDefaultSaveDir ?? '';
          if (saveDir.isEmpty) {
            logError(_tag, 'drop torrent: defaultSaveDir empty, skip');
            continue;
          }
          unawaited(DownloadController.sendTorrentFileSignal(path, saveDir));
          accepted++;
        } else {
          // 明确不支持：非 .torrent 本地文件丢弃（S0.5 裁决）
          logInfo(_tag, 'drop file ignored (not .torrent): $path');
        }
      }
      if (accepted > 0) {
        unawaited(_restoreMainWindow());
      }
      return;
    }

    // kind == text：URL 正则筛选（语义校验留 Dart，A4）
    final urls = <String>[];
    for (final text in values) {
      urls.addAll(_urlRegex.allMatches(text).map((m) => m.group(0)!));
    }
    if (urls.isEmpty) {
      logInfo(_tag, 'drop text contained no URL, ignored');
      return;
    }
    unawaited(_showQuickDialogWithUrls(urls));
  }

  Future<void> _showQuickDialogWithUrls(List<String> urls) async {
    await _restoreMainWindow();
    final ctx = _navigatorKey?.currentContext;
    if (ctx == null || !ctx.mounted) {
      logError(_tag, 'no navigator context for quick dialog');
      return;
    }
    final settings = SettingsProvider.globalInstance ?? _settings!;
    showQuickDownloadDialog(
      ctx,
      url: urls.join('\n'),
      filename: '',
      fileSize: 0,
      mimeType: '',
      cookies: '',
      defaultSaveDir: settings.effectiveDefaultSaveDir,
      defaultQueueId: settings.defaultQueueId,
    );
  }

  // ===========================================================================
  // 唤醒检测（S0.5 — 墙钟跳变）
  // ===========================================================================

  void _startHeartbeat() {
    _lastBeat = DateTime.now();
    _heartbeat?.cancel();
    _heartbeat = Timer.periodic(const Duration(seconds: 10), (_) {
      final now = DateTime.now();
      final gap = now.difference(_lastBeat);
      _lastBeat = now;
      if (gap.inSeconds > 30) {
        logInfo(_tag, 'wake detected (gap=${gap.inSeconds}s), revalidating');
        _onWake();
      }
    });
  }

  void _onWake() {
    if (!_enabled) return;
    // 坐标校验 + 全量重绘
    if (Platform.isWindows && Win32BallWindow.instance.isCreated) {
      final s = _effectiveSettings!;
      final (x, y) = Win32BallWindow.clampToWorkArea(
        s.floatingBallX < 0 ? 0 : s.floatingBallX.round(),
        s.floatingBallY < 0 ? 0 : s.floatingBallY.round(),
      );
      Win32BallWindow.instance.moveTo(x, y);
    }
    unawaited(_rerenderAll());
  }
}
