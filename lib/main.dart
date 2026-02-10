import 'dart:convert';
import 'dart:io';
import 'dart:ui';

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/material.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:window_manager/window_manager.dart';
import 'src/bindings/bindings.dart';
import 'src/models/settings_provider.dart';
import 'src/pages/home_page.dart';
import 'src/services/external_download_service.dart';
import 'src/services/log_service.dart';
import 'src/services/notification_service.dart';
import 'src/services/tray_service.dart';
import 'src/i18n/locale_provider.dart';
import 'src/services/update_service.dart';
import 'src/theme/app_theme.dart';
import 'src/theme/theme_provider.dart';
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

  // 初始化日志服务 — 最先初始化，以捕获后续所有日志
  LogService.instance.init();
  logInfo('main', 'FluxDown starting, args=$args');

  // 设置全局异常捕获 — Flutter 框架异常
  FlutterError.onError = (details) {
    logError(
      'FlutterError',
      details.exceptionAsString(),
      details.exception,
      details.stack,
    );
  };

  // 设置全局异常捕获 — Dart 未捕获异步异常
  // 使用 PlatformDispatcher.onError 而非 runZonedGuarded，
  // 避免 Zone mismatch（ensureInitialized 和 runApp 必须在同一 Zone）
  PlatformDispatcher.instance.onError = (error, stack) {
    logError('PlatformError', 'Uncaught async error', error, stack);
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
  logInfo('main', 'Rust runtime initialized, calling runApp...');
  runApp(
    FluxDownApp(themeProvider: themeProvider, localeNotifier: localeNotifier),
  );
}

class FluxDownApp extends StatefulWidget {
  final ThemeProvider themeProvider;
  final LocaleNotifier localeNotifier;

  const FluxDownApp({
    super.key,
    required this.themeProvider,
    required this.localeNotifier,
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
  final _settingsForExternal = SettingsProvider();

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
    // 请求加载配置，确保 settingsProvider 有默认保存目录等数据
    _settingsForExternal.requestConfig();

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

    logInfo('FluxDownApp', 'initState done');
  }

  @override
  void dispose() {
    logInfo('FluxDownApp', 'dispose called');
    TrayService.instance.onExitApp = null;
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

  /// 优雅退出 — 等待待处理通知完成 → 销毁托盘 → 销毁窗口。
  /// 由托盘「退出」菜单和窗口关闭（closeToTray=false）共用。
  Future<void> _performGracefulExit() async {
    logInfo(
      'FluxDownApp',
      '_performGracefulExit called, _isExiting=$_isExiting',
    );
    // 防止重入：快速双击关闭或托盘退出+窗口关闭同时触发
    if (_isExiting) return;
    _isExiting = true;

    try {
      // 标记通知服务停止接受新请求
      logInfo('FluxDownApp', 'shutting down NotificationService...');
      NotificationService.instance.shutdown();
      // 等待所有正在创建中的通知窗口完成，避免通知丢失
      logInfo('FluxDownApp', 'waiting for pending notifications...');
      await NotificationService.instance.waitForPending();
      logInfo('FluxDownApp', 'destroying tray...');
      await TrayService.instance.destroy();
      logInfo('FluxDownApp', 'destroying window...');
      await windowManager.destroy();
      logInfo('FluxDownApp', 'graceful exit complete');
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
