import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui';

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:window_manager/window_manager.dart';
import 'src/bindings/bindings.dart';
import 'src/models/download_controller.dart';
import 'src/models/settings_provider.dart';
import 'src/pages/home_page.dart';
import 'src/services/external_download_service.dart';
import 'src/services/hls_quality_service.dart';
import 'src/services/analytics_service.dart';
import 'src/services/log_service.dart';
import 'src/services/notification_service.dart';
import 'src/services/tray_service.dart';
import 'src/i18n/locale_provider.dart';
import 'src/services/update_service.dart';
import 'src/theme/app_theme.dart';
import 'src/theme/theme_provider.dart';
import 'src/widgets/update_changelog_dialog.dart';
import 'src/windows/download_complete_window.dart';

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();

  // 初始化 i18n — 创建 LocaleNotifier 并从 SharedPreferences 恢复语言偏好
  localeNotifier = LocaleNotifier();
  await localeNotifier.init();

  // desktop_multi_window 子窗口入口：
  // 当子窗口被创建时，同一个 main() 会再次调用，args 包含 ['multi_window', windowId, argument]
  if (args.firstOrNull == 'multi_window') {
    final windowId = args[1];
    final argument = args.length > 2 ? args[2] : '{}';

    final windowController = WindowController.fromWindowId(windowId);

    Map<String, dynamic> windowArgs;
    try {
      windowArgs = jsonDecode(argument) as Map<String, dynamic>;
    } catch (_) {
      windowArgs = {};
    }

    final windowType = windowArgs['windowType'] as String? ?? '';

    // 注意：子窗口不能调用 windowManager.ensureInitialized()，
    // 因为 windowManager 是全局单例，会干扰主窗口的 native handle，
    // 导致主窗口隐藏到托盘后恢复时崩溃。
    // 子窗口应通过 WindowController 管理自身。

    if (windowType == 'download_complete') {
      runApp(
        DownloadCompleteApp(
          windowController: windowController,
          args: windowArgs,
        ),
      );
    }
    return;
  }

  // ===== 主窗口正常启动流程 =====

  // 提取启动参数中的 .torrent 文件路径（Windows 文件关联双击打开）
  final torrentFilePaths = args
      .where((a) => a.toLowerCase().endsWith('.torrent') && !a.startsWith('-'))
      .toList();

  // 初始化日志服务 — 最先初始化，以捕获后续所有日志
  LogService.instance.init();
  logInfo(
    'main',
    'FluxDown starting, args=$args, torrentFiles=${torrentFilePaths.length}',
  );

  // 设置全局异常捕获 — Flutter 框架异常
  FlutterError.onError = (details) {
    logError(
      'FlutterError',
      details.exceptionAsString(),
      details.exception,
      details.stack,
    );
    AnalyticsService.instance.logException(
      details.exceptionAsString(),
      true,
      stackTrace: details.stack,
    );
  };

  // 设置全局异常捕获 — Dart 未捕获异步异常
  // 使用 PlatformDispatcher.onError 而非 runZonedGuarded，
  // 避免 Zone mismatch（ensureInitialized 和 runApp 必须在同一 Zone）
  PlatformDispatcher.instance.onError = (error, stack) {
    logError('PlatformError', 'Uncaught async error', error, stack);
    AnalyticsService.instance.logException(
      error.toString(),
      true,
      stackTrace: stack,
    );
    return true; // 已处理，不再向上传播
  };

  logInfo('main', 'initializing theme...');
  // 在 runApp 之前恢复主题设置，避免启动时主题闪烁
  final themeProvider = ThemeProvider();
  await themeProvider.init();
  logInfo('main', 'theme initialized');

  logInfo('main', 'initializing windowManager...');
  await windowManager.ensureInitialized();

  const windowOptions = WindowOptions(
    size: Size(1280, 720),
    minimumSize: Size(900, 500),
    center: true,
    titleBarStyle: TitleBarStyle.hidden,
    windowButtonVisibility: false,
  );

  windowManager.waitUntilReadyToShow(windowOptions, () async {
    logInfo('main', 'window ready to show');
    await windowManager.show();
    await windowManager.focus();
    logInfo('main', 'window shown and focused');
  });

  // 初始化开机启动支持
  launchAtStartup.setup(
    appName: 'FluxDown',
    appPath: Platform.resolvedExecutable,
  );
  logInfo('main', 'launchAtStartup setup done');

  // 初始化系统托盘
  logInfo('main', 'initializing tray...');
  await TrayService.instance.init();
  logInfo('main', 'tray initialized');

  logInfo('main', 'initializing Rust runtime...');
  await initializeRust(assignRustSignal);
  logInfo('main', 'Rust runtime initialized');

  // 初始化数据分析服务（异步，不阻塞启动）
  // 此时 SettingsProvider 尚未加载配置，先以默认值(true)启动。
  // _FluxDownAppState.initState 中 SettingsProvider 配置加载后会同步实际状态。
  AnalyticsService.instance.init(enabled: true);
  logInfo('main', 'analytics init dispatched, calling runApp...');

  runApp(
    FluxDownApp(
      themeProvider: themeProvider,
      localeNotifier: localeNotifier,
      initialTorrentFiles: torrentFilePaths,
    ),
  );
}

class FluxDownApp extends StatefulWidget {
  final ThemeProvider themeProvider;
  final LocaleNotifier localeNotifier;

  /// .torrent file paths passed via command-line args (Windows file association).
  final List<String> initialTorrentFiles;

  const FluxDownApp({
    super.key,
    required this.themeProvider,
    required this.localeNotifier,
    this.initialTorrentFiles = const [],
  });

  /// 允许子组件通过 context 访问 ThemeProvider
  static ThemeProvider of(BuildContext context) {
    final state = context.findAncestorStateOfType<_FluxDownAppState>();
    return state!.themeProvider;
  }

  @override
  State<FluxDownApp> createState() => _FluxDownAppState();
}

class _FluxDownAppState extends State<FluxDownApp> with WindowListener {
  late final ThemeProvider themeProvider;
  late final LocaleNotifier _localeNotifier;
  final _navigatorKey = GlobalKey<NavigatorState>();
  final _settingsForExternal = SettingsProvider(enableFileAssoc: false);

  /// MethodChannel for receiving args from second instances (single-instance).
  static const _singleInstanceChannel = MethodChannel(
    'com.fluxdown/single_instance',
  );

  /// 防止 _performGracefulExit 被并发调用多次
  bool _isExiting = false;

  @override
  void initState() {
    super.initState();
    logInfo('FluxDownApp', 'initState');
    themeProvider = widget.themeProvider;
    _localeNotifier = widget.localeNotifier;
    themeProvider.addListener(_onThemeChanged);
    _localeNotifier.addListener(_onLocaleChanged);
    windowManager.addListener(this);
    // 阻止默认关闭行为，由 onWindowClose 接管
    windowManager.setPreventClose(true);

    // 初始化通知服务 — 传递主题信息给通知窗口
    NotificationService.instance.setThemeProvider(themeProvider);

    // 设置托盘退出回调 — 统一走优雅退出流程
    TrayService.instance.onExitApp = _performGracefulExit;

    // 初始化外部下载服务 — 监听浏览器扩展的下载请求
    ExternalDownloadService.init(
      settingsProvider: _settingsForExternal,
      navigatorKey: _navigatorKey,
    );

    // 初始化 HLS 画质选择服务 — 监听 Rust 端的画质选项信号
    HlsQualityService.init(navigatorKey: _navigatorKey);
    // 请求加载配置，确保 settingsProvider 有默认保存目录等数据
    _settingsForExternal.requestConfig();

    // 配置加载完成后，同步 analytics 的实际同意状态
    _syncAnalyticsAfterConfigLoad();

    // 延迟 5 秒后台静默检查更新（避免阻塞启动流程）
    Future.delayed(const Duration(seconds: 5), () {
      if (!mounted) return;
      if (!_settingsForExternal.autoCheckUpdate) {
        logInfo('FluxDownApp', 'auto check for updates skipped (disabled)');
        return;
      }
      logInfo('FluxDownApp', 'auto check for updates');
      UpdateService.instance.checkForUpdate();
    });

    // Handle .torrent files passed via command-line args (Windows file association).
    // Wait for SettingsProvider to finish loading config from Rust so we have
    // a valid defaultSaveDir, instead of a fragile fixed delay.
    if (widget.initialTorrentFiles.isNotEmpty) {
      logInfo(
        'FluxDownApp',
        'will process ${widget.initialTorrentFiles.length} initial torrent file(s) after config loads',
      );
      _waitForConfigAndHandleTorrentFiles();
    }

    // 监听更新服务 — changelog 就绪后自动弹出更新日志弹窗
    UpdateService.instance.addListener(_onUpdateServiceChanged);

    // Listen for args from second instances (single-instance enforcement).
    // When a second instance is launched (e.g. double-clicking a .torrent
    // file while the app is already running), the native C++ layer sends
    // the command-line args here via MethodChannel.
    _singleInstanceChannel.setMethodCallHandler(_handleSecondInstance);

    logInfo('FluxDownApp', 'initState done');
  }

  @override
  void dispose() {
    logInfo('FluxDownApp', 'dispose called');
    UpdateService.instance.removeListener(_onUpdateServiceChanged);
    _singleInstanceChannel.setMethodCallHandler(null);
    TrayService.instance.onExitApp = null;
    HlsQualityService.shutdown();
    ExternalDownloadService.shutdown();
    _settingsForExternal.dispose();
    windowManager.removeListener(this);
    _localeNotifier.removeListener(_onLocaleChanged);
    themeProvider.removeListener(_onThemeChanged);
    themeProvider.dispose();
    super.dispose();
    logInfo('FluxDownApp', 'dispose done');
  }

  void _onThemeChanged() {
    logInfo('FluxDownApp', 'themeChanged, mounted=$mounted');
    if (mounted) setState(() {});
  }

  void _onLocaleChanged() {
    logInfo('FluxDownApp', 'localeChanged to $currentLocale, mounted=$mounted');
    if (mounted) setState(() {});
    // 语言变更后刷新托盘菜单
    TrayService.instance.refreshMenu();
  }

  /// 当 UpdateService 状态变化时，检查是否应该弹出更新日志弹窗。
  void _onUpdateServiceChanged() {
    final svc = UpdateService.instance;
    if (!svc.shouldShowChangelog) return;
    if (!mounted) return;

    final ctx = _navigatorKey.currentContext;
    if (ctx == null) return;

    logInfo('FluxDownApp', 'showing update changelog dialog');
    svc.markChangelogShown();

    showUpdateChangelogDialog(
      ctx,
      releases: svc.changelogReleases,
      latestVersion: svc.checkResult?.latestVersion ?? '',
      currentVersion: svc.currentVersion,
      onUpdate: () => svc.downloadUpdate(),
      onLater: () {
        // No-op — dialog already dismissed, changelog marked as shown.
      },
    );
  }

  /// 等配置从 Rust 加载完成后，同步 analytics 的真实同意状态。
  /// 如果用户之前关闭了分析，这里会及时撤销同意。
  void _syncAnalyticsAfterConfigLoad() {
    void applyConsent() {
      AnalyticsService.instance.setEnabled(
        _settingsForExternal.analyticsEnabled,
      );
    }

    if (_settingsForExternal.loaded) {
      applyConsent();
      return;
    }
    late final void Function() listener;
    Timer? timeout;
    void cleanup() {
      timeout?.cancel();
      _settingsForExternal.removeListener(listener);
    }

    listener = () {
      if (_settingsForExternal.loaded) {
        cleanup();
        applyConsent();
      }
    };
    _settingsForExternal.addListener(listener);
    // 兜底：10 秒后仍未加载则以默认值（enabled=true）继续
    timeout = Timer(const Duration(seconds: 10), cleanup);
  }

  /// Wait for SettingsProvider to finish loading config from Rust, then handle
  /// the initial .torrent files. Uses a listener instead of a fixed delay so
  /// we react as soon as the config arrives, with a 10-second timeout fallback.
  void _waitForConfigAndHandleTorrentFiles() {
    // Already loaded (unlikely but possible if Rust responds before initState completes)
    if (_settingsForExternal.loaded) {
      _handleInitialTorrentFiles();
      return;
    }

    late final void Function() listener;
    Timer? timeout;

    void cleanup() {
      timeout?.cancel();
      _settingsForExternal.removeListener(listener);
    }

    listener = () {
      if (_settingsForExternal.loaded) {
        cleanup();
        if (mounted) _handleInitialTorrentFiles();
      }
    };

    _settingsForExternal.addListener(listener);

    // Timeout fallback — if config never arrives within 10s, try anyway
    // (defaultSaveDir has a platform fallback so it won't be empty).
    timeout = Timer(const Duration(seconds: 10), () {
      logInfo(
        'FluxDownApp',
        'config load timed out after 10s, handling torrent files with fallback dir',
      );
      cleanup();
      if (mounted) _handleInitialTorrentFiles();
    });
  }

  /// Handle .torrent files passed via command-line args.
  /// Creates download tasks using the default save directory from settings.
  void _handleInitialTorrentFiles() {
    final saveDir = _settingsForExternal.defaultSaveDir;
    if (saveDir.isEmpty) {
      logInfo(
        'FluxDownApp',
        'default save dir not ready, skipping torrent file handling',
      );
      return;
    }
    for (final path in widget.initialTorrentFiles) {
      logInfo('FluxDownApp', 'creating task from torrent file: $path');
      // Reuse the static helper from DownloadController — avoids duplicating
      // the file-read + signal-send logic. DownloadController in HomePage
      // will pick up the resulting task via Rust signal stream.
      DownloadController.sendTorrentFileSignal(path, saveDir);
    }
  }

  /// Called when a second instance sends its command-line args via WM_COPYDATA.
  /// Extracts .torrent file paths and creates download tasks, then brings
  /// the window to the foreground.
  Future<dynamic> _handleSecondInstance(MethodCall call) async {
    if (call.method == 'onSecondInstance') {
      final args = (call.arguments as List<dynamic>).cast<String>();
      logInfo('FluxDownApp', 'received second-instance args: $args');

      // Bring window to foreground.
      await windowManager.show();
      await windowManager.focus();

      // Extract .torrent file paths from the args.
      final torrentPaths = args
          .where(
            (a) => a.toLowerCase().endsWith('.torrent') && !a.startsWith('-'),
          )
          .toList();

      if (torrentPaths.isEmpty) {
        logInfo('FluxDownApp', 'no torrent files in second-instance args');
        return;
      }

      logInfo(
        'FluxDownApp',
        'second-instance torrent files: ${torrentPaths.length}',
      );

      // Wait for config if not yet loaded.
      final saveDir = _settingsForExternal.defaultSaveDir;
      if (saveDir.isEmpty) {
        logInfo(
          'FluxDownApp',
          'config not loaded yet, waiting before handling second-instance torrents',
        );
        // Use a completer to wait for config.
        final completer = Completer<void>();
        late final void Function() listener;
        Timer? timeout;

        listener = () {
          if (_settingsForExternal.loaded) {
            timeout?.cancel();
            _settingsForExternal.removeListener(listener);
            completer.complete();
          }
        };

        _settingsForExternal.addListener(listener);
        timeout = Timer(const Duration(seconds: 10), () {
          _settingsForExternal.removeListener(listener);
          completer.complete();
        });

        await completer.future;
      }

      final dir = _settingsForExternal.defaultSaveDir;
      if (dir.isEmpty) return;

      for (final path in torrentPaths) {
        logInfo(
          'FluxDownApp',
          'creating task from second-instance torrent: $path',
        );
        DownloadController.sendTorrentFileSignal(path, dir);
      }
    }
  }

  /// 优雅退出 — 隐藏窗口 → 清理资源 → 销毁窗口。
  /// 由托盘「退出」菜单和窗口关闭（closeToTray=false）共用。
  /// 先隐藏窗口让用户感知「秒退」，再后台执行清理。
  Future<void> _performGracefulExit() async {
    logInfo(
      'FluxDownApp',
      '_performGracefulExit called, _isExiting=$_isExiting',
    );
    // 防止重入：快速双击关闭或托盘退出+窗口关闭同时触发
    if (_isExiting) return;
    _isExiting = true;

    try {
      // 立即隐藏窗口，给用户「秒退」的视觉反馈
      logInfo('FluxDownApp', 'hiding window immediately...');
      await windowManager.hide();

      // 后台清理：通知服务 → 托盘图标
      logInfo('FluxDownApp', 'shutting down NotificationService...');
      NotificationService.instance.shutdown();
      logInfo('FluxDownApp', 'waiting for pending notifications...');
      await NotificationService.instance.waitForPending();
      logInfo('FluxDownApp', 'destroying tray...');
      await TrayService.instance.destroy();

      logInfo('FluxDownApp', 'disposing analytics...');
      await AnalyticsService.instance.dispose();

      logInfo('FluxDownApp', 'destroying window...');
      await LogService.instance.dispose();
      await windowManager.destroy();
    } catch (e, stack) {
      logError('FluxDownApp', '_performGracefulExit error', e, stack);
      // 兜底：无论如何都尝试销毁窗口
      try {
        await windowManager.destroy();
      } catch (_) {}
    }
  }

  @override
  void onWindowClose() async {
    logInfo('FluxDownApp', 'onWindowClose called, _isExiting=$_isExiting');
    // 已经在退出流程中，不再重复处理
    if (_isExiting) return;

    final closeToTray = SettingsProvider.globalInstance?.closeToTray ?? true;
    logInfo('FluxDownApp', 'closeToTray=$closeToTray');

    // 当用户设置了「关闭到托盘」时，隐藏窗口而非退出
    if (closeToTray) {
      logInfo('FluxDownApp', 'hiding to tray...');
      await TrayService.instance.hideToTray();
      logInfo('FluxDownApp', 'hidden to tray');
    } else {
      await _performGracefulExit();
    }
  }

  @override
  void onWindowFocus() {
    logInfo('FluxDownApp', 'onWindowFocus');
  }

  @override
  void onWindowBlur() {
    logInfo('FluxDownApp', 'onWindowBlur');
  }

  @override
  void onWindowRestore() {
    logInfo('FluxDownApp', 'onWindowRestore');
  }

  @override
  void onWindowMinimize() {
    logInfo('FluxDownApp', 'onWindowMinimize');
  }

  ShadThemeData _resolveTheme(BuildContext context) {
    final mode = themeProvider.themeMode;
    final scheme = themeProvider.colorScheme;
    final platformBrightness = MediaQuery.platformBrightnessOf(context);
    final useDark =
        mode == ThemeMode.dark ||
        (mode == ThemeMode.system && platformBrightness == Brightness.dark);
    return useDark ? buildDarkTheme(scheme) : buildLightTheme(scheme);
  }

  @override
  Widget build(BuildContext context) {
    // 手动组合 ShadTheme + WidgetsApp，跳过 ShadApp 内部的：
    // - ShadAnimatedTheme（200ms 色彩 tween 插值）
    // - AnimatedTheme（200ms Material 主题动画）
    // - materialTheme() 每帧重建 ThemeData + applyGoogleFontToTextTheme
    final theme = _resolveTheme(context);
    return LocaleScope(
      s: _localeNotifier.s,
      child: ShadTheme(
        data: theme,
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: DefaultTextStyle(
            style: theme.textTheme.p.copyWith(
              color: theme.colorScheme.foreground,
            ),
            child: ShadToaster(
              child: ShadSonner(
                child: ExcludeSemantics(
                  child: WidgetsApp(
                    navigatorKey: _navigatorKey,
                    color: theme.colorScheme.primary,
                    debugShowCheckedModeBanner: false,
                    home: const HomePage(),
                    pageRouteBuilder:
                        <T>(RouteSettings settings, WidgetBuilder builder) {
                          return MaterialPageRoute<T>(
                            settings: settings,
                            builder: builder,
                          );
                        },
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
