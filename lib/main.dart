import 'dart:async';
import 'dart:io';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'src/widgets/flux_sonner.dart';
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
import 'src/services/quick_download_submitter.dart';
import 'src/popup/popup_app.dart';
import 'src/services/floating_ball/floating_ball_service.dart';
import 'src/services/floating_ball/wayland_degradation_service.dart';
import 'src/services/foreground_service.dart';
import 'src/services/hls_quality_service.dart';
import 'src/services/resolve_variant_service.dart';
import 'src/services/bt_file_selection_service.dart';
import 'src/services/analytics_service.dart';
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

/// 记录 Flutter 官方定义的启动边界：引擎完成首帧栅格化。
void _logFirstFrameWhenRasterized(Stopwatch startupStopwatch) {
  unawaited(
    WidgetsBinding.instance.waitUntilFirstFrameRasterized.then((_) {
      startupStopwatch.stop();
      logInfo(
        'startup',
        'first frame rasterized in ${startupStopwatch.elapsedMilliseconds}ms',
      );
    }),
  );
}

Future<void> main(List<String> args) async {
  // 独立快速下载小窗引擎入口：原生宿主以 --quick-popup 参数启动第二引擎。
  // 该引擎零插件注册、不初始化 Rust，所有环境数据经 fluxdown/popup_child
  // 通道注入（见 lib/src/popup/popup_app.dart）。必须在任何插件调用之前分发。
  if (args.contains('--quick-popup')) {
    await runQuickPopupApp();
    return;
  }

  WidgetsFlutterBinding.ensureInitialized();
  final startupStopwatch = Stopwatch()..start();

  // 初始化日志服务 — 必须尽早执行。
  // 预览版 Windows 上若 SharedPreferences / 插件初始化卡住，
  // 需要保证这些启动前故障也能写入日志，而不是只剩白屏。
  LogService.instance.init();
  logInfo('main', 'bootstrap start, args=$args');

  // 必须早于所有 provider/service 读取。便携模式使用 exe 目录 settings.json。
  await _runStartupStep('kv store init', () => KvStore.instance.init());

  // 主题只依赖已载入的 KvStore，与翻译资源加载互不依赖。并发等待可让
  // AssetBundle I/O 与本地主题解析重叠，同时仍保证 runApp 前主题和语言
  // 都已就绪，不引入首帧闪烁。
  final themeProvider = ThemeProvider();
  await Future.wait<void>([
    _runStartupStep('i18n load', I18nStore.load),
    _runStartupStep('theme init', () => themeProvider.init()),
  ]);
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

  // 提取启动参数中的协议 URL（系统协议处理器：浏览器扩展协议模式 /
  // 网页 <a href="fluxdown://..."> / ed2k:// 电驴链接唤起本 exe）。
  final protocolRequests = args.map(_parseProtocolArg).nonNulls.toList();

  logInfo(
    'main',
    'FluxDown starting, args=$args, torrentFiles=${torrentFilePaths.length}, '
        'protocolRequests=${protocolRequests.length}',
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

  logInfo('main', 'theme and locale init steps finished');

  // ===== 移动端启动流程 =====
  // Android / iOS 走精简初始化：无窗口管理、托盘、开机启动等桌面服务。
  if (Platform.isAndroid || Platform.isIOS) {
    ForegroundServiceManager.initCommunicationPort();
    logInfo('main', 'initializing Rust runtime (mobile)...');
    await initializeRust(assignRustSignal);
    logInfo('main', 'starting mobile shell');
    runApp(
      FluxDownMobileApp(
        themeProvider: themeProvider,
        localeNotifier: localeNotifier,
      ),
    );
    _logFirstFrameWhenRasterized(startupStopwatch);
    return;
  }

  // Rust、窗口、开机启动和托盘互不依赖。旧流程将四条 I/O 链完全串行，
  // 任何一条慢路径都会直接累加到首帧时间。这里在同一 isolate 上并发等待
  // I/O（不是多线程执行 Dart），仍在 runApp 前汇合，确保首帧契约不变。
  final windowManagerReady = windowManager.ensureInitialized();

  final rustInit = () async {
    logInfo('main', 'initializing Rust runtime...');
    await initializeRust(assignRustSignal);
    logInfo('main', 'Rust runtime initialized');
  }();

  final windowInit = () async {
    logInfo('main', 'initializing windowManager...');
    await windowManagerReady;

    // 从 KvStore 读取上次保存的窗口状态（纯读取，不调用 windowManager API）。
    await _runStartupStep(
      'load saved window state',
      () => WindowStateService.instance.loadState(),
    );

    // 不使用 waitUntilReadyToShow：其 VoidCallback 无法等待异步窗口设置，
    // 会与原生 first_frame_cb 竞争并产生默认尺寸闪烁。
    await windowManager.setTitleBarStyle(
      TitleBarStyle.hidden,
      windowButtonVisibility: Platform.isMacOS,
    );
    if (Platform.isMacOS) {
      await windowManager.setBackgroundColor(const Color(0xFFE5E5E5));
    }
    await windowManager.setMinimumSize(const Size(900, 500));
    await _runStartupStep(
      'apply saved window state',
      () => WindowStateService.instance.applyState(),
    );
    logInfo('main', 'window state apply step finished before runApp');
  }();

  final autostartInit = () async {
    // 注册时附带 --silentStart；Windows 路径加引号，避免空格截断。
    launchAtStartup.setup(
      appName: 'FluxDown',
      appPath: Platform.isWindows
          ? '"${Platform.resolvedExecutable}"'
          : Platform.resolvedExecutable,
      args: ['--silentStart'],
    );
    try {
      bool needsReEnable = await launchAtStartup.isEnabled();
      if (!needsReEnable && Platform.isWindows) {
        // 精确值匹配检测不到安装程序或旧版本写入的旧条目，直接查注册表。
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
  }();

  final trayInit = () async {
    await _runStartupStep(
      'tray init',
      () => TrayService.instance.init(),
      timeout: const Duration(seconds: 5),
    );

    // 将 Windows 托盘图标修正为 app 当前主题，而非系统默认亮度。
    if (Platform.isWindows) {
      final mode = themeProvider.themeMode;
      final trayIsDark = switch (mode) {
        ThemeMode.dark => true,
        ThemeMode.light => false,
        ThemeMode.system =>
          WidgetsBinding.instance.platformDispatcher.platformBrightness ==
              Brightness.dark,
      };
      await _runStartupStep(
        'tray theme sync',
        () => TrayService.instance.setIsDark(trayIsDark),
      );
      logInfo('main', 'tray isDark=$trayIsDark (from app theme: $mode)');
    }
    logInfo('main', 'tray init step finished');
  }();

  await Future.wait<void>([rustInit, windowInit, autostartInit, trayInit]);

  // AppIconService 依赖 windowManager 与 TrayService，必须在上面的汇合点之后。
  await _runStartupStep(
    'app icon init',
    () => AppIconService.instance.init(),
    timeout: const Duration(seconds: 5),
  );
  logInfo('main', 'app icon init step finished');

  logInfo('main', 'calling runApp...');

  runApp(
    FluxDownApp(
      themeProvider: themeProvider,
      localeNotifier: localeNotifier,
      initialTorrentFiles: torrentFilePaths,
      initialProtocolRequests: protocolRequests,
    ),
  );

  _logFirstFrameWhenRasterized(startupStopwatch);
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

/// 解析协议启动参数（系统协议处理器唤起本 exe 时经启动参数传入）。
///
/// 三种形态：
/// - `fluxdown://download?url=<encoded-url>&filename=<name>`——自有深链，
///   拆出内层真实 URL；host 不是 download 或缺 url 参数时忽略。
/// - `ed2k://|file|<name>|<size>|<hash>|/`——电驴链接本身就是下载地址，
///   原样透传（`|` 不是合法 URI 字符，不能过 [Uri.tryParse]）。
/// - `magnet:?xt=urn:btih:…`——磁力链接本身就是下载地址，原样透传
///   （系统 magnet 协议处理器唤起，见 protocol_registry::MAGNET）。
({String url, String filename})? _parseProtocolArg(String arg) {
  final lower = arg.toLowerCase();
  if (lower.startsWith('ed2k://') || lower.startsWith('magnet:')) {
    return (url: arg.trim(), filename: '');
  }
  if (!lower.startsWith('fluxdown://')) return null;
  final uri = Uri.tryParse(arg);
  if (uri == null || uri.host.toLowerCase() != 'download') {
    logInfo('main', 'ignoring malformed fluxdown:// arg: $arg');
    return null;
  }
  final url = uri.queryParameters['url']?.trim() ?? '';
  if (url.isEmpty) {
    logInfo('main', 'fluxdown:// arg missing url parameter: $arg');
    return null;
  }
  final filename = uri.queryParameters['filename']?.trim() ?? '';
  return (url: url, filename: filename);
}

class FluxDownApp extends StatefulWidget {
  final ThemeProvider themeProvider;
  final LocaleNotifier localeNotifier;

  /// .torrent file paths passed via command-line args (Windows file association).
  final List<String> initialTorrentFiles;

  /// fluxdown:// 协议请求（Windows 注册表协议处理器经启动参数传入）。
  final List<({String url, String filename})> initialProtocolRequests;

  const FluxDownApp({
    super.key,
    required this.themeProvider,
    required this.localeNotifier,
    this.initialTorrentFiles = const [],
    this.initialProtocolRequests = const [],
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
  // App 级服务与 HomePage 共享同一个配置实例，避免重复订阅五条 Rust 信号、
  // 重复读取完整配置，并维持 SettingsProvider.globalInstance 单一所有者。
  final _settingsForExternal = SettingsProvider();

  /// MethodChannel for receiving args from second instances (single-instance).
  static const _singleInstanceChannel = MethodChannel(
    'com.fluxdown/single_instance',
  );

  /// 防止 _performGracefulExit 被并发调用多次
  bool _isExiting = false;

  /// 文件跟踪重扫的最小间隔。主窗获焦是高频事件（alt-tab、从别的程序切回），
  /// 而一次重扫要遍历全部已完成任务逐个 stat，几万任务时代价可观。
  static const _rescanMinInterval = Duration(seconds: 30);
  DateTime? _lastRescanAt;
  Timer? _pendingRescan;

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

    // 快速下载提交器的 toast 反馈通道 — 本地/云端设备下发是异步的，
    // 独立小窗场景下下发完成时弹窗多半已关闭，结果提示必须走主窗口。
    initQuickDownloadSubmitter(navigatorKey: _navigatorKey);

    // 初始化 HLS 画质选择服务 — 监听 Rust 端的画质选项信号
    HlsQualityService.init(navigatorKey: _navigatorKey);

    // 初始化插件 resolve 变体选择服务 — 监听 Rust 端的变体选项信号
    ResolveVariantService.init(navigatorKey: _navigatorKey);

    // 初始化 BT 文件选择服务 — 监听 Rust 端的 BtFilesInfo 信号
    BtFileSelectionService.init(navigatorKey: _navigatorKey);
    // 请求加载配置，确保 settingsProvider 有默认保存目录等数据
    _settingsForExternal.requestConfig();

    // 匿名统计 — 配置加载完成后上报首装/每日活跃事件（不含任何下载任务信息）
    AnalyticsService.instance.init(_settingsForExternal);

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

    // Handle .torrent files and fluxdown:// protocol URLs passed via
    // command-line args (Windows file association / protocol handler).
    // Wait for SettingsProvider to finish loading config from Rust so we have
    // a valid defaultSaveDir, instead of a fragile fixed delay.
    if (widget.initialTorrentFiles.isNotEmpty ||
        widget.initialProtocolRequests.isNotEmpty) {
      logInfo(
        'FluxDownApp',
        'will process ${widget.initialTorrentFiles.length} torrent file(s) and '
            '${widget.initialProtocolRequests.length} protocol request(s) after config loads',
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
    _pendingRescan?.cancel();
    UpdateService.instance.removeListener(_onUpdateServiceChanged);
    _singleInstanceChannel.setMethodCallHandler(null);
    TrayService.instance.onExitApp = null;
    HlsQualityService.shutdown();
    ResolveVariantService.shutdown();
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
    // （桌面端默认目录只由 Rust 经系统 API 解析后下发，Dart 侧无拼接兜底：
    //  配置真的没到时 defaultSaveDir 为空，下游会记日志并跳过，不写错路径）。
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
      unawaited(TrayService.instance.hideToTray());
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

  /// Handle .torrent files and fluxdown:// protocol URLs passed via
  /// command-line args. Torrent files create tasks directly with the default
  /// save directory; protocol URLs route into the same external download
  /// flow as browser-extension requests (silent / popup / dialog).
  void _handleInitialTorrentFiles() {
    _dispatchInitialProtocolRequests();
    final saveDir = _settingsForExternal.defaultSaveDir;
    if (saveDir.isEmpty) {
      if (widget.initialTorrentFiles.isNotEmpty) {
        logInfo(
          'FluxDownApp',
          'default save dir not ready, skipping torrent file handling',
        );
      }
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

  /// 分发启动参数中的协议请求（fluxdown:// / ed2k://；幂等：配置加载监听器
  /// 与超时兜底可能双触发 _handleInitialTorrentFiles）。
  bool _protocolRequestsDispatched = false;

  void _dispatchInitialProtocolRequests() {
    if (_protocolRequestsDispatched) return;
    _protocolRequestsDispatched = true;
    for (final req in widget.initialProtocolRequests) {
      logInfo('FluxDownApp', 'dispatching protocol arg: ${req.url}');
      ExternalDownloadService.handleLocalRequest(
        url: req.url,
        filename: req.filename,
      );
    }
  }

  /// Called when a second instance sends its command-line args via WM_COPYDATA.
  /// Extracts .torrent file paths / protocol URLs (fluxdown:// deep links,
  /// ed2k:// links), dispatches
  /// them, then brings the window to the foreground.
  Future<dynamic> _handleSecondInstance(MethodCall call) async {
    if (call.method == 'onSecondInstance') {
      final args = (call.arguments as List<dynamic>).cast<String>();
      logInfo('FluxDownApp', 'received second-instance args: $args');

      // Bring window to foreground.
      await restoreMainWindow();

      // 协议 URL（浏览器扩展协议模式 / 网页链接 / ed2k 链接唤起时，
      // 系统启动第二实例，参数经 WM_COPYDATA 转发到本主实例）。
      final protocolRequests = args.map(_parseProtocolArg).nonNulls.toList();
      for (final req in protocolRequests) {
        logInfo('FluxDownApp', 'second-instance protocol arg: ${req.url}');
        ExternalDownloadService.handleLocalRequest(
          url: req.url,
          filename: req.filename,
        );
      }

      // Extract .torrent file paths from the args.
      // Linux forwards file:// URIs via GApplication open signal — decode them.
      final torrentPaths = args
          .where((a) => !a.startsWith('-'))
          .map(_decodeFilePath)
          .where((a) => a.toLowerCase().endsWith('.torrent'))
          .toList();

      if (torrentPaths.isEmpty) {
        if (protocolRequests.isEmpty) {
          logInfo('FluxDownApp', 'no actionable second-instance args');
        }
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
    _requestRescanFiles();
  }

  /// 发一次文件跟踪重扫请求，最小间隔 [_rescanMinInterval]。冷却期内的触发
  /// 不丢弃而是折叠成一次尾沿补发——否则「获焦 → 切出去删文件 → 再获焦」
  /// 会被直接吞掉，而桌面端没有定时器兜底（只有 headless server 有）。
  void _requestRescanFiles() {
    final last = _lastRescanAt;
    final elapsed = last == null ? null : DateTime.now().difference(last);
    if (elapsed != null && elapsed < _rescanMinInterval) {
      _pendingRescan ??= Timer(_rescanMinInterval - elapsed, () {
        _pendingRescan = null;
        _lastRescanAt = DateTime.now();
        RescanFiles().sendSignalToRust();
      });
      return;
    }
    _pendingRescan?.cancel();
    _pendingRescan = null;
    _lastRescanAt = DateTime.now();
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
                child: FluxSonner(
                  child: ExcludeSemantics(
                    child: WidgetsApp(
                      navigatorKey: _navigatorKey,
                      color: theme.colorScheme.primary,
                      debugShowCheckedModeBanner: false,
                      home: HomePage(settingsProvider: _settingsForExternal),
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
                            // 快速淡入替代 Material 默认 300ms 转场，降低动效感知
                            return PageRouteBuilder<T>(
                              settings: settings,
                              transitionDuration: const Duration(
                                milliseconds: 120,
                              ),
                              reverseTransitionDuration: const Duration(
                                milliseconds: 100,
                              ),
                              pageBuilder: (context, _, _) => builder(context),
                              transitionsBuilder:
                                  (context, animation, _, child) {
                                    return FadeTransition(
                                      opacity: animation,
                                      child: child,
                                    );
                                  },
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
          // About 不用 PlatformProvidedMenuItemType.about（系统标准 About 面板，
          // 见 issue #47），改为导航到主窗口「设置 > 关于」页。
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuAbout,
                onSelected: () => AppMenuCallbacks.openAbout?.call(),
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
          // Services（唯一保留的系统提供项：子菜单内容由 macOS 动态填充，
          // 无法在 Flutter 侧复刻；标题本地化受限于 flutter/flutter#120097）
          const PlatformMenuItemGroup(
            members: [
              PlatformProvidedMenuItem(
                type: PlatformProvidedMenuItemType.servicesSubmenu,
              ),
            ],
          ),
          // Hide / Hide Others / Show All
          // 不用 PlatformProvidedMenuItem：其 label 由 engine 硬编码英文无法
          // 本地化（flutter/flutter#120097），且 macOS 26 会给标准 selector
          // 自动配图标，与自定义项混排导致图标/语言不统一。以下经
          // com.fluxdown/window 通道调用等效 AppKit API。
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuHide,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyH,
                  meta: true,
                ),
                onSelected: () => macMenuAction('hide'),
              ),
              PlatformMenuItem(
                label: s.menuHideOthers,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyH,
                  meta: true,
                  alt: true,
                ),
                onSelected: () => macMenuAction('hideOthers'),
              ),
              PlatformMenuItem(
                label: s.menuShowAll,
                onSelected: () => macMenuAction('showAll'),
              ),
            ],
          ),
          // Quit — 走托盘「退出」同一优雅退出流程（完整清理 + 防重入）
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuQuit,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyQ,
                  meta: true,
                ),
                onSelected: () => TrayService.instance.requestExit(),
              ),
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
          PlatformMenuItem(
            label: s.menuToggleFullScreen,
            shortcut: const SingleActivator(
              LogicalKeyboardKey.keyF,
              meta: true,
              control: true,
            ),
            onSelected: () => macMenuAction('toggleFullScreen'),
          ),
        ],
      ),

      // ── 窗口 ──
      PlatformMenu(
        label: s.menuWindow,
        menus: [
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuMinimize,
                shortcut: const SingleActivator(
                  LogicalKeyboardKey.keyM,
                  meta: true,
                ),
                onSelected: () => windowManager.minimize(),
              ),
              PlatformMenuItem(
                label: s.menuZoom,
                onSelected: () => macMenuAction('zoom'),
              ),
            ],
          ),
          PlatformMenuItemGroup(
            members: [
              PlatformMenuItem(
                label: s.menuBringAllToFront,
                onSelected: () => macMenuAction('front'),
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
