import 'dart:async';
import 'dart:io';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:window_manager/window_manager.dart';
import 'src/bindings/bindings.dart';
import 'src/services/window_state_service.dart';
import 'src/models/download_controller.dart';
import 'src/models/settings_provider.dart';
import 'src/pages/home_page.dart';
import 'src/mobile/mobile_app.dart';
import 'src/services/external_download_service.dart';
import 'src/services/popup_window_service.dart';
import 'src/popup/popup_app.dart';
import 'src/services/floating_ball/floating_ball_service.dart';
import 'src/services/floating_ball/wayland_degradation_service.dart';
import 'src/services/hls_quality_service.dart';
import 'src/services/bt_file_selection_service.dart';
import 'src/services/app_icon_service.dart';
import 'src/services/log_service.dart';
import 'src/services/kv_store.dart';
import 'src/services/notification_service.dart';
import 'src/services/power_service.dart';
import 'src/services/tray_service.dart';
import 'src/i18n/locale_provider.dart';
import 'src/services/update_service.dart';
import 'src/theme/app_theme.dart';
import 'src/theme/flux_theme_tokens.dart';
import 'src/theme/theme_provider.dart';
import 'src/widgets/feedback_dialog.dart';
import 'src/widgets/ui_scale_widget.dart';
import 'src/widgets/update_changelog_dialog.dart';

/// 启动阶段的非关键步骤统一加超时保护和日志，
/// 防止某一步卡住导致整个应用白屏。
Future<void> _runStartupStep(
  String name,
  Future<void> Function() action, {
  Duration timeout = const Duration(seconds: 3),
}) async {
  final sw = Stopwatch()..start();
  logInfo('startup', 'starting $name');
  try {
    await action().timeout(timeout);
    logInfo('startup', 'completed $name in ${sw.elapsedMilliseconds}ms');
  } catch (e, stack) {
    logError(
      'startup',
      '$name failed after ${sw.elapsedMilliseconds}ms, continuing with defaults',
      e,
      stack,
    );
  }
}

Future<void> main(List<String> args) async {
  // 独立快速下载小窗引擎入口：原生宿主以 --quick-popup 参数启动第二引擎。
  // 该引擎零插件注册、不初始化 Rust，所有环境数据经 fluxdown/popup_child
  // 通道注入（见 lib/src/popup/popup_app.dart）。必须在任何插件调用之前分发。
  if (args.contains('--quick-popup')) {
    runQuickPopupApp();
    return;
  }

  WidgetsFlutterBinding.ensureInitialized();

  // 初始化日志服务 — 必须尽早执行。
  // 预览版 Windows 上若 SharedPreferences / 插件初始化卡住，
  // 需要保证这些启动前故障也能写入日志，而不是只剩白屏。
  LogService.instance.init();
  logInfo('main', 'bootstrap start, args=$args');

  // 初始化键值存储门面 — 必须早于任何 provider/service 读取（locale/theme/
  // 窗口状态）。便携模式下改写 exe 目录 settings.json，消除首次打开写 C 盘。
  await _runStartupStep('kv store init', () => KvStore.instance.init());

  // 初始化 i18n — 创建 LocaleNotifier 并从 SharedPreferences 恢复语言偏好
  localeNotifier = LocaleNotifier();
  await _runStartupStep('locale init', () => localeNotifier.init());

  // 注：已移除 desktop_multi_window 子窗口入口。
  // 下载完成通知现在通过主窗口内 OverlayEntry 实现，
  // 不再创建独立子窗口/Isolate，彻底消除并发 Isolate 崩溃。

  // ===== 主窗口正常启动流程 =====

  // 提取启动参数中的 .torrent 文件路径（文件关联双击打开）。
  // Linux 文件管理器通过 %U 传入 file:// URI，需解码为本地路径。
  final torrentFilePaths = args
      .where((a) => !a.startsWith('-'))
      .map(_decodeFilePath)
      .where((a) => a.toLowerCase().endsWith('.torrent'))
      .toList();

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
  await _runStartupStep('theme init', () => themeProvider.init());
  logInfo('main', 'theme init step finished');

  // ===== 移动端启动流程 =====
  // Android / iOS 走精简初始化：无窗口管理、托盘、开机启动等桌面服务。
  if (Platform.isAndroid || Platform.isIOS) {
    logInfo('main', 'initializing Rust runtime (mobile)...');
    await initializeRust(assignRustSignal);
    logInfo('main', 'starting mobile shell');
    runApp(
      FluxDownMobileApp(
        themeProvider: themeProvider,
        localeNotifier: localeNotifier,
      ),
    );
    return;
  }

  logInfo('main', 'initializing windowManager...');
  await windowManager.ensureInitialized();

  // 从 SharedPreferences 读取上次保存的窗口状态（纯读取，不调用 windowManager API）
  logInfo('main', 'loading saved window state...');
  await _runStartupStep(
    'load saved window state',
    () => WindowStateService.instance.loadState(),
  );

  // 不使用 waitUntilReadyToShow —— 它的回调参数类型是 VoidCallback，
  // async 回调中的 await 全部变成 fire-and-forget，与原生层 first_frame_cb
  // 的 gtk_widget_show / Win32 Show() 竞争，导致窗口以默认大小先显示再跳变。
  // 且其内部会无条件执行 unmaximize()，破坏已恢复的最大化状态。
  //
  // 改为在 runApp 之前直接 await 逐步设置窗口属性，
  // 所有 method-channel 调用同步完成后才进入 Flutter 渲染循环，
  // first_frame_cb 触发 show 时窗口属性已就位，不会闪烁跳变。
  // 窗口显示由原生层 first_frame_cb 控制（已处理 silentStart 逻辑）。
  await windowManager.setTitleBarStyle(
    TitleBarStyle.hidden,
    windowButtonVisibility: Platform.isMacOS,
  );
  // macOS：设置 NSWindow 背景色为浅灰，使失焦时 traffic light 按钮（灰色圆圈）
  // 在白色侧边栏背景上有足够对比度，不会"消失"。
  if (Platform.isMacOS) {
    await windowManager.setBackgroundColor(const Color(0xFFE5E5E5));
  }
  await windowManager.setMinimumSize(const Size(900, 500));
  await _runStartupStep(
    'apply saved window state',
    () => WindowStateService.instance.applyState(),
  );
  logInfo('main', 'window state apply step finished before runApp');

  // 初始化开机启动支持（注册时附带 --silentStart 参数，开机自启免打扰）
  // Windows 下路径加引号，防止含空格的安装路径（如 C:\Program Files\...）被 CreateProcess 截断解析失败。
  launchAtStartup.setup(
    appName: 'FluxDown',
    appPath: Platform.isWindows
        ? '"${Platform.resolvedExecutable}"'
        : Platform.resolvedExecutable,
    args: ['--silentStart'],
  );
  // 确保注册表条目包含 --silentStart 参数，处理两种迁移场景：
  // 1. 旧版本自行写入的条目（路径未加引号或缺少 --silentStart）
  // 2. Windows 安装程序写入的条目（无 --silentStart，与 launchAtStartup 期望值不匹配）
  try {
    bool needsReEnable = await launchAtStartup.isEnabled();
    if (!needsReEnable && Platform.isWindows) {
      // launchAtStartup.isEnabled() 做精确值匹配，检测不到安装程序写入的旧条目。
      // 用 reg query 直接检查注册表中是否存在任意值（含安装程序创建的条目）。
      final regResult = await Process.run('reg', [
        'query',
        r'HKCU\Software\Microsoft\Windows\CurrentVersion\Run',
        '/v',
        'FluxDown',
      ]);
      if (regResult.exitCode == 0) {
        needsReEnable = true;
        logInfo(
          'main',
          'found legacy/installer autostart entry, migrating to --silentStart',
        );
      }
    }
    if (needsReEnable) {
      await launchAtStartup.enable();
      logInfo('main', 'launchAtStartup re-enabled with --silentStart arg');
    }
  } catch (e) {
    logInfo('main', 'launchAtStartup refresh skipped: $e');
  }
  logInfo('main', 'launchAtStartup setup done');

  // 初始化系统托盘
  logInfo('main', 'initializing tray...');
  await _runStartupStep(
    'tray init',
    () => TrayService.instance.init(),
    timeout: const Duration(seconds: 5),
  );
  logInfo('main', 'tray init step finished');

  // themeProvider 已加载完毕，立即将托盘图标修正为 app 当前生效主题
  // （init() 默认使用系统亮度作为初始值，这里覆盖为 app 的显式设置）
  if (Platform.isWindows) {
    final mode = themeProvider.themeMode;
    final bool trayIsDark;
    if (mode == ThemeMode.dark) {
      trayIsDark = true;
    } else if (mode == ThemeMode.light) {
      trayIsDark = false;
    } else {
      trayIsDark =
          WidgetsBinding.instance.platformDispatcher.platformBrightness ==
          Brightness.dark;
    }
    await _runStartupStep(
      'tray theme sync',
      () => TrayService.instance.setIsDark(trayIsDark),
    );
    logInfo('main', 'tray isDark=$trayIsDark (from app theme: $mode)');
  }

  // 恢复用户自定义的应用图标（窗口/任务栏/托盘）。
  // WM_SETICON 仅对当前进程生效，需每次启动重新应用；默认图标来自 exe 资源，无需处理。
  await _runStartupStep(
    'app icon init',
    () => AppIconService.instance.init(),
    timeout: const Duration(seconds: 5),
  );
  logInfo('main', 'app icon init step finished');

  logInfo('main', 'initializing Rust runtime...');
  await initializeRust(assignRustSignal);
  logInfo('main', 'Rust runtime initialized');

  logInfo('main', 'calling runApp...');

  runApp(
    FluxDownApp(
      themeProvider: themeProvider,
      localeNotifier: localeNotifier,
      initialTorrentFiles: torrentFilePaths,
    ),
  );
}

/// Normalize a file argument to a plain filesystem path.
/// Linux file managers pass file URIs via `%U` (e.g. `file:///home/user/foo.torrent`).
/// Returns the decoded local path for `file://` URIs, or the original string otherwise.
String _decodeFilePath(String arg) {
  if (arg.startsWith('file://')) {
    try {
      return Uri.parse(arg).toFilePath();
    } catch (_) {}
  }
  return arg;
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

class _FluxDownAppState extends State<FluxDownApp>
    with WindowListener, WidgetsBindingObserver {
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
    WidgetsBinding.instance.addObserver(this);
    // 阻止默认关闭行为，由 onWindowClose 接管
    windowManager.setPreventClose(true);

    // 初始化通知服务 — 传入主题信息（Windows Toast 深浅色绘制用）
    NotificationService.instance.init();
    NotificationService.instance.setThemeProvider(themeProvider);

    // 设置托盘退出回调 — 统一走优雅退出流程
    TrayService.instance.onExitApp = _performGracefulExit;

    // 初始化外部下载服务 — 监听浏览器扩展的下载请求
    ExternalDownloadService.init(
      settingsProvider: _settingsForExternal,
      navigatorKey: _navigatorKey,
    );

    // 初始化独立小窗服务 — 外部下载请求的首选确认入口
    // （原生窗口承载第二 Flutter 引擎，theme/navigator 用于组装载荷）
    PopupWindowService.instance.init(
      themeProvider: themeProvider,
      navigatorKey: _navigatorKey,
    );

    // 初始化 HLS 画质选择服务 — 监听 Rust 端的画质选项信号
    HlsQualityService.init(navigatorKey: _navigatorKey);

    // 初始化 BT 文件选择服务 — 监听 Rust 端的 BtFilesInfo 信号
    BtFileSelectionService.init(navigatorKey: _navigatorKey);
    // 请求加载配置，确保 settingsProvider 有默认保存目录等数据
    _settingsForExternal.requestConfig();

    // 悬浮球服务 — 配置加载完成后初始化（S0.5 初始化钩子）
    _initFloatingBallAfterConfigLoad();

    // 启动时最小化到托盘：配置加载完成后按设置决定是否隐藏主窗口
    // （原生层 first_frame_cb 默认会显示窗口，此处按用户设置补做隐藏）
    _applyStartMinimizedToTrayAfterConfigLoad();

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
    // 主动消费一次：若失败标记响应在监听器注册前就已到达（notifyListeners
    // 已触发但当时无监听者），此处补偿一次，避免错过更新失败提示。
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _onUpdateServiceChanged();
    });

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
    BtFileSelectionService.shutdown();
    ExternalDownloadService.shutdown();
    _settingsForExternal.dispose();
    WindowStateService.instance.dispose();
    WidgetsBinding.instance.removeObserver(this);
    windowManager.removeListener(this);
    _localeNotifier.removeListener(_onLocaleChanged);
    themeProvider.removeListener(_onThemeChanged);
    themeProvider.dispose();
    super.dispose();
    logInfo('FluxDownApp', 'dispose done');
  }

  void _onThemeChanged() {
    logInfo('FluxDownApp', 'themeChanged, mounted=$mounted');
    if (mounted) {
      setState(() {});
      _updateTrayTheme();
    }
  }

  /// 系统亮度变化（深/浅色模式切换）时触发
  /// 仅当 app 主题设为「跟随系统」时才会实际影响托盘图标
  @override
  void didChangePlatformBrightness() {
    _updateTrayTheme();
  }

  /// 根据 app 当前生效主题更新 Windows 托盘图标
  /// 优先级：app 显式设置 > 系统亮度（仅 ThemeMode.system 时回退到系统）
  void _updateTrayTheme() {
    if (!Platform.isWindows) return;
    final mode = themeProvider.themeMode;
    final bool isDark;
    if (mode == ThemeMode.dark) {
      isDark = true;
    } else if (mode == ThemeMode.light) {
      isDark = false;
    } else {
      // ThemeMode.system → 跟随系统亮度
      isDark =
          WidgetsBinding.instance.platformDispatcher.platformBrightness ==
          Brightness.dark;
    }
    TrayService.instance.setIsDark(isDark);
  }

  void _onLocaleChanged() {
    logInfo('FluxDownApp', 'localeChanged to $currentLocale, mounted=$mounted');
    if (mounted) setState(() {});
    // 语言变更后刷新托盘菜单
    TrayService.instance.refreshMenu();
  }

  /// 当 UpdateService 状态变化时，检查是否应该弹出更新日志弹窗 / 更新失败提示。
  void _onUpdateServiceChanged() {
    final svc = UpdateService.instance;

    // 优先处理「上次更新失败」标记（便携版覆盖文件失败等）。
    if (svc.pendingFailureMessage.isNotEmpty) {
      _showUpdateFailureDialog(svc.pendingFailureMessage);
      // 立刻确认，避免 notifyListeners 再次触发重复弹窗。
      svc.acknowledgeFailureMarker();
      return;
    }

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

  /// 弹出「上次更新失败」提示对话框，引导用户手动恢复 / 重新下载。
  void _showUpdateFailureDialog(String message) {
    if (!mounted) return;
    final ctx = _navigatorKey.currentContext;
    if (ctx == null) return;

    final s = S.of(currentLocale);
    logInfo('FluxDownApp', 'showing update failure dialog');

    showShadDialog<void>(
      context: ctx,
      builder: (dialogCtx) => ShadDialog.alert(
        title: Text(s.updateFailedTitle),
        description: Padding(
          padding: const EdgeInsets.only(top: 8),
          child: Text(message),
        ),
        actions: [
          ShadButton.outline(
            onPressed: () => launchUrl(Uri.parse('https://fluxdown.zerx.dev')),
            child: Text(s.updateFailedOpenSite),
          ),
          ShadButton(
            onPressed: () => Navigator.of(dialogCtx).pop(),
            child: Text(s.confirm),
          ),
        ],
      ),
    );
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

  /// 等配置加载完成后初始化悬浮球服务（S0.5：floatingBallEnabled/坐标
  /// 均来自 Rust config，须先就绪；与 torrent 关联处理同款监听模式）。
  void _initFloatingBallAfterConfigLoad() {
    void doInit() {
      FloatingBallService.instance.init(
        settings: _settingsForExternal,
        theme: themeProvider,
        navigatorKey: _navigatorKey,
      );
    }

    if (_settingsForExternal.loaded) {
      doInit();
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
        if (mounted) doInit();
      }
    };
    _settingsForExternal.addListener(listener);
    timeout = Timer(const Duration(seconds: 10), () {
      cleanup();
      if (mounted) doInit();
    });
  }

  /// 启动时最小化到托盘：等配置加载完成后，若用户开启该项则隐藏主窗口
  /// （与 torrent 关联处理 / 悬浮球初始化同款「等待配置加载」监听模式）。
  ///
  /// 主窗口的初始显示由原生层 first_frame_cb 控制（Win32 `Show()` /
  /// GTK `gtk_widget_show`），在 Flutter 首帧渲染完成时同步触发，早于
  /// Dart 侧异步的 Rust 配置加载完成。因此这里不「跳过」原生层的显示，
  /// 而是在配置到达后立即补一次隐藏——由于 Rust 引擎在 `runApp` 之前已
  /// 完成初始化，配置请求/响应往返通常快于首帧上屏，实际观感等同于
  /// 跳过显示；仅托盘驻留，窗口可随时从托盘唤出。监听器仅触发一次
  /// （命中后立即移除），不会在运行期后续设置变更时重复隐藏窗口。
  void _applyStartMinimizedToTrayAfterConfigLoad() {
    void apply() {
      if (!_settingsForExternal.startMinimizedToTray) return;
      logInfo(
        'FluxDownApp',
        'startMinimizedToTray enabled, hiding main window',
      );
      windowManager.hide();
    }

    if (_settingsForExternal.loaded) {
      apply();
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
        if (mounted) apply();
      }
    };
    _settingsForExternal.addListener(listener);
    timeout = Timer(const Duration(seconds: 10), () {
      cleanup();
      if (mounted) apply();
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
      // Linux forwards file:// URIs via GApplication open signal — decode them.
      final torrentPaths = args
          .where((a) => !a.startsWith('-'))
          .map(_decodeFilePath)
          .where((a) => a.toLowerCase().endsWith('.torrent'))
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
      // 隐藏前保存窗口状态（托盘退出不经过 onWindowClose，需在此保存）
      await WindowStateService.instance.saveNow();

      // 立即隐藏窗口，给用户「秒退」的视觉反馈
      logInfo('FluxDownApp', 'hiding window immediately...');
      await windowManager.hide();

      // 释放唤醒锁（Windows 线程级状态随进程退出也会清除，此处保证子进程回收）
      logInfo('FluxDownApp', 'shutting down PowerService...');
      await PowerService.instance.shutdown();

      // 后台清理：通知服务 → 托盘图标
      logInfo('FluxDownApp', 'shutting down NotificationService...');
      NotificationService.instance.shutdown();
      logInfo('FluxDownApp', 'waiting for pending notifications...');
      await NotificationService.instance.waitForPending();
      logInfo('FluxDownApp', 'destroying floating ball...');
      FloatingBallService.instance.destroy();
      logInfo('FluxDownApp', 'destroying tray...');
      await TrayService.instance.destroy();

      // 便携模式下 KvStore 写入有防抖，退出前强制落盘，避免刚改的设置丢失。
      await KvStore.instance.flush();
      logInfo('FluxDownApp', 'destroying window...');
      await LogService.instance.dispose();
      await windowManager.destroy();
    } catch (e, stack) {
      logError('FluxDownApp', '_performGracefulExit error', e, stack);
      // 兜底：无论如何都尝试销毁窗口
      try {
        await windowManager.destroy();
      } catch (_) {}
    } finally {
      // Linux 上 windowManager.destroy() 只销毁 GTK 窗口，进程不会自动退出
      // 需要显式终止 Dart 进程（含 Rust 线程）
      exit(0);
    }
  }

  @override
  void onWindowClose() async {
    logInfo('FluxDownApp', 'onWindowClose called, _isExiting=$_isExiting');
    // 已经在退出流程中，不再重复处理
    if (_isExiting) return;

    // 隐藏/关闭前立即保存窗口状态，确保最新位置/大小被持久化
    await WindowStateService.instance.saveNow();

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
    // Wayland 降级形态③：主窗获焦时读一次剪贴板（失焦读取被协议门控）
    unawaited(WaylandDegradationService.instance.checkClipboardOnRestore());
    // 文件跟踪：主窗获焦时用户可能刚在资源管理器删/移了文件，触发一次重扫。
    RescanFiles().sendSignalToRust();
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

  @override
  void onWindowMoved() {
    WindowStateService.instance.onMoved();
  }

  @override
  void onWindowResized() {
    WindowStateService.instance.onResized();
  }

  @override
  void onWindowMaximize() {
    WindowStateService.instance.onMaximized();
  }

  @override
  void onWindowUnmaximize() {
    WindowStateService.instance.onUnmaximized();
  }

  FluxThemeTokens _resolveTokens(BuildContext context) {
    return themeProvider.activeTokens(context);
  }

  /// macOS：每次主题变更后同步 NSWindow 背景色，让失焦的 traffic light
  /// 灰色圆圈在侧边栏背景上有足够对比度。
  void _syncMacOsWindowBackground(FluxThemeTokens tokens) {
    if (!Platform.isMacOS) return;
    // surface1 是侧边栏背景色，traffic light 按钮就在其上方
    final bg = tokens.surface1;
    // 深色主题 surface1 已经足够深，浅色主题 surface1 可能是纯白，
    // 稍微加深一点让灰色按钮有对比度
    final windowBg = tokens.appearance == Brightness.light
        ? Color.fromARGB(
            255,
            (bg.r * 255 * 0.88).round().clamp(0, 255),
            (bg.g * 255 * 0.88).round().clamp(0, 255),
            (bg.b * 255 * 0.88).round().clamp(0, 255),
          )
        : bg;
    windowManager.setBackgroundColor(windowBg);
  }

  @override
  Widget build(BuildContext context) {
    // 手动组合 ShadTheme + WidgetsApp，跳过 ShadApp 内部的：
    // - ShadAnimatedTheme（200ms 色彩 tween 插值）
    // - AnimatedTheme（200ms Material 主题动画）
    // - materialTheme() 每帧重建 ThemeData + applyGoogleFontToTextTheme
    final tokens = _resolveTokens(context);
    final theme = buildThemeFromTokens(tokens);
    WidgetsBinding.instance.addPostFrameCallback(
      (_) => _syncMacOsWindowBackground(tokens),
    );

    Widget app = LocaleScope(
      s: _localeNotifier.s,
      child: FluxThemeScope(
        tokens: tokens,
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
                      builder: (context, child) {
                        final scale = themeProvider.uiScale;
                        if (scale == 1.0) return child!;
                        final mq = MediaQuery.of(context);
                        return MediaQuery(
                          data: mq.copyWith(size: mq.size / scale),
                          child: UiScaleWidget(scale: scale, child: child!),
                        );
                      },
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
      ),
    );

    // macOS: 自定义应用菜单栏（替换默认 Flutter 模板菜单）
    if (Platform.isMacOS) {
      app = PlatformMenuBar(menus: _buildMacMenus(), child: app);
    }

    return app;
  }

  /// 构建 macOS 应用菜单栏。
  /// [PlatformMenuItemGroup] 将相关菜单项分组，组与组之间自动插入分隔线。
  List<PlatformMenuItem> _buildMacMenus() {
    final s = _localeNotifier.s;
    return [
      // ── FluxDown (应用菜单) ──
      PlatformMenu(
        label: 'FluxDown',
        menus: [
          // About + Check for Updates
          PlatformMenuItemGroup(
            members: [
              const PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.about,
              ),
              PlatformMenuItem(
                label: s.menuCheckForUpdates,
                onSelected: () => UpdateService.instance.checkForUpdate(),
              ),
            ],
          ),
          // Settings
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuSettings,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.comma,
                  meta: true,
                ),
                onSelected: () => AppMenuCallbacks.openSettings?.call(),
              ),
            ],
          ),
          // Services
          const PlatformMenuItemGroup(
            members: [
              PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.servicesSubmenu,
              ),
            ],
          ),
          // Hide / Hide Others / Show All
          const PlatformMenuItemGroup(
            members: [
              PlatformProvidedMenuItem(type: PlatformProvidedMenuItemType.hide),
              PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.hideOtherApplications,
              ),
              PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.showAllApplications,
              ),
            ],
          ),
          // Quit
          const PlatformMenuItemGroup(
            members: [
              PlatformProvidedMenuItem(type: PlatformProvidedMenuItemType.quit),
            ],
          ),
        ],
      ),

      // ── 文件 ──
      PlatformMenu(
        label: s.menuFile,
        menus: [
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuNewDownload,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyN,
                  meta: true,
                ),
                onSelected: () => AppMenuCallbacks.newDownload?.call(),
              ),
            ],
          ),
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuCloseWindow,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyW,
                  meta: true,
                ),
                onSelected: () => windowManager.close(),
              ),
            ],
          ),
        ],
      ),

      // ── 编辑 ──
      PlatformMenu(
        label: s.menuEdit,
        menus: [
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuSelectAll,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyA,
                  meta: true,
                ),
                onSelected: () => AppMenuCallbacks.selectAll?.call(),
              ),
            ],
          ),
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuFind,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyF,
                  meta: true,
                ),
                onSelected: () => AppMenuCallbacks.find?.call(),
              ),
            ],
          ),
        ],
      ),

      // ── 视图 ──
      PlatformMenu(
        label: s.menuView,
        menus: [
          const PlatformProvidedMenuItem(
            type: PlatformProvidedMenuItemType.toggleFullScreen,
          ),
        ],
      ),

      // ── 窗口 ──
      PlatformMenu(
        label: s.menuWindow,
        menus: [
          const PlatformMenuItemGroup(
            members: [
              PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.minimizeWindow,
              ),
              PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.zoomWindow,
              ),
            ],
          ),
          const PlatformMenuItemGroup(
            members: [
              PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.arrangeWindowsInFront,
              ),
            ],
          ),
        ],
      ),

      // ── 帮助 ──
      PlatformMenu(
        label: s.menuHelp,
        menus: [
          PlatformMenuItem(
            label: s.menuWebsite,
            onSelected: () => launchUrl(Uri.parse('https://fluxdown.zerx.dev')),
          ),
          PlatformMenuItem(
            label: s.menuFeedback,
            onSelected: () {
              final ctx = _navigatorKey.currentContext;
              if (ctx != null) showFeedbackDialog(ctx);
            },
          ),
        ],
      ),
    ];
  }
}

// 界面缩放 RenderObject 已提取到 src/widgets/ui_scale_widget.dart
