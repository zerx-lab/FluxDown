import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart' show LucideIcons;

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import '../theme/theme_provider.dart';
import '../services/kv_store.dart';
import '../services/update_service.dart';
import 'screens/mobile_settings_screen.dart';
import 'services/mobile_storage_service.dart';
import 'screens/mobile_tasks_screen.dart';
import 'services/share_intent_service.dart';
import 'mobile_ui.dart';
import 'sheets/mobile_new_download_sheet.dart';

/// 移动端根壳：任务列表主屏 + 右上角设置入口（push 路由进入设置页）
class MobileShell extends StatefulWidget {
  final ThemeProvider themeProvider;
  final LocaleNotifier localeNotifier;

  const MobileShell({
    super.key,
    required this.themeProvider,
    required this.localeNotifier,
  });

  @override
  State<MobileShell> createState() => _MobileShellState();
}

class _MobileShellState extends State<MobileShell> with WidgetsBindingObserver {
  final _controller = DownloadController();
  final _settings = SettingsProvider(enableFileAssoc: false);
  bool _sheetOpen = false;

  /// 新建下载弹层是否正在展示（区别于更新提示等其他弹层）。
  /// 弹层可见期间到达的分享 / 协议 URL 经 [_shareAppendCtrl] 合入表单，
  /// 支撑扩展批量协议唤起（逐条 VIEW intent，间隔 800ms）。
  bool _downloadSheetOpen = false;
  final _shareAppendCtrl = StreamController<String>.broadcast();

  /// 自动更新检查只触发一次（等配置加载完成后）。
  bool _updateCheckScheduled = false;

  /// 更新提示弹层只弹一次；用户点了"立即更新"后进入自动流程，
  /// 下载完成时直接唤起安装。
  bool _updatePromptShown = false;
  bool _updateFlowAccepted = false;

  /// KvStore key：用户选择"跳过此版本"时记录的版本号。
  static const _prefKeySkippedVersion = 'update_skipped_version';

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _settings.requestConfig();
    _ensureAndroidSaveDir();
    // 系统分享 / URL scheme 接入：收到链接切到下载页并弹新建下载弹层
    ShareIntentService.init(_onShared);
    // 启动自动检查更新：等配置加载完成后按 autoCheckUpdate 决定
    _settings.addListener(_maybeScheduleUpdateCheck);
    UpdateService.instance.addListener(_onUpdateChanged);
    _maybeScheduleUpdateCheck();
  }

  /// 配置加载完成且开启了自动检查 → 延迟数秒触发一次版本检查
  /// （避开启动高峰，与桌面端 5s 静默检查策略一致）。
  void _maybeScheduleUpdateCheck() {
    if (_updateCheckScheduled || !_settings.loaded) return;
    _updateCheckScheduled = true;
    _settings.removeListener(_maybeScheduleUpdateCheck);
    if (!_settings.autoCheckUpdate) return;
    Future.delayed(const Duration(seconds: 5), () {
      if (mounted) UpdateService.instance.checkForUpdate();
    });
  }

  /// UpdateService 状态驱动：发现新版本弹提示层；
  /// 用户接受更新后，下载完成自动唤起安装器。
  void _onUpdateChanged() {
    final svc = UpdateService.instance;
    switch (svc.status) {
      case UpdateStatus.available:
        _maybePromptUpdate(svc);
      case UpdateStatus.readyToInstall:
        if (_updateFlowAccepted) {
          _updateFlowAccepted = false;
          svc.installUpdate();
        }
      default:
        break;
    }
  }

  Future<void> _maybePromptUpdate(UpdateService svc) async {
    if (_updatePromptShown || _sheetOpen || !mounted) return;
    final result = svc.checkResult;
    if (result == null || !result.hasUpdate) return;
    final version = result.latestVersion;
    // 用户此前选择过"跳过此版本"→ 不再提示
    if (KvStore.instance.getString(_prefKeySkippedVersion) == version) return;
    _updatePromptShown = true;

    final s = widget.localeNotifier.s;
    _sheetOpen = true;
    try {
      final choice = await showMobileSheet<_UpdateChoice>(
        context,
        builder: (ctx) {
          final c = AppColors.of(ctx);
          return MobileSheetContainer(
            title: s.newVersionFound(version),
            footer: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                MobilePrimaryButton(
                  icon: LucideIcons.download,
                  label: s.updateNow,
                  filled: true,
                  onTap: () => Navigator.of(ctx).pop(_UpdateChoice.update),
                ),
                const SizedBox(height: 10),
                MobilePrimaryButton(
                  label: s.updateLater,
                  filled: false,
                  onTap: () => Navigator.of(ctx).pop(_UpdateChoice.later),
                ),
                const SizedBox(height: 10),
                MobilePrimaryButton(
                  label: s.skipThisVersion,
                  filled: false,
                  onTap: () => Navigator.of(ctx).pop(_UpdateChoice.skip),
                ),
              ],
            ),
            child: Padding(
              padding: const EdgeInsets.only(top: 2, bottom: 4),
              child: Text(
                s.updatePromptBody(
                  version,
                  UpdateService.formatBytes(result.fileSize),
                ),
                style: TextStyle(fontSize: 13, height: 1.5, color: c.textMuted),
              ),
            ),
          );
        },
      );
      switch (choice) {
        case _UpdateChoice.update:
          _updateFlowAccepted = true;
          svc.downloadUpdate();
        case _UpdateChoice.skip:
          await KvStore.instance.setString(_prefKeySkippedVersion, version);
        case _UpdateChoice.later || null:
          break; // 划走 / 点遮罩 / 稍后：本次会话不再提示
      }
    } finally {
      _sheetOpen = false;
    }
  }

  /// 收到系统分享 / URL scheme 唤起的链接：切到下载页，弹新建下载弹层
  /// 并预填 URL（fluxdown:// 协议携带的建议文件名一并预填）。
  /// 新建下载弹层已打开时把 URL 追加进现有表单（批量协议唤起逐条到达）；
  /// 其他弹层（更新提示等）打开时忽略，避免叠层。
  Future<void> _onShared(
    String url,
    String filename,
    String userAgent,
    String cookie,
    String referer,
  ) async {
    if (!mounted) return;
    if (_downloadSheetOpen) {
      _shareAppendCtrl.add(url);
      return;
    }
    if (_sheetOpen) return;
    // 若正在设置页，先回到任务列表
    Navigator.of(context).popUntil((r) => r.isFirst);
    _sheetOpen = true;
    _downloadSheetOpen = true;
    try {
      await showMobileNewDownloadSheet(
        context,
        controller: _controller,
        settings: _settings,
        initialUrl: url,
        initialFileName: filename,
        initialUserAgent: userAgent,
        initialCookie: cookie,
        initialReferer: referer,
        appendUrls: _shareAppendCtrl.stream,
      );
    } finally {
      _sheetOpen = false;
      _downloadSheetOpen = false;
    }
  }

  /// Android：让 framework 创建应用专属外部下载目录
  /// （`Android/data` 层禁止应用自建子树，Rust 引擎写入前必须初始化），
  /// 并在用户未自定义时把默认保存目录同步为 framework 返回的真实路径
  /// （多用户 / 特殊分区场景下与硬编码路径可能不同）。
  Future<void> _ensureAndroidSaveDir() async {
    final dir = await MobileStorageService.appExternalDownloadDir();
    if (dir == null || dir.isEmpty || !mounted) return;
    if (_settings.defaultSaveDir == SettingsProvider.platformDefaultSaveDir &&
        _settings.defaultSaveDir != dir) {
      _settings.setDefaultSaveDir(dir);
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    ShareIntentService.shutdown();
    _shareAppendCtrl.close();
    _settings.removeListener(_maybeScheduleUpdateCheck);
    UpdateService.instance.removeListener(_onUpdateChanged);
    _controller.dispose();
    _settings.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      // 文件跟踪：回到前台时用户可能刚在文件管理器删/移了文件。
      RescanFiles().sendSignalToRust();
      // 同进程引擎重建后内存列表是空的；再要一次快照自愈。
      _controller.requestPersistedState();
    }
  }

  void _openSettings() {
    Navigator.of(context).push(
      PageRouteBuilder<void>(
        transitionDuration: const Duration(milliseconds: 280),
        reverseTransitionDuration: const Duration(milliseconds: 240),
        pageBuilder: (_, _, _) => MobileSettingsScreen(
          settings: _settings,
          themeProvider: widget.themeProvider,
          localeNotifier: widget.localeNotifier,
        ),
        transitionsBuilder: (_, anim, _, child) {
          final curved = CurvedAnimation(
            parent: anim,
            curve: const Cubic(0.32, 0.72, 0.32, 1),
          );
          return SlideTransition(
            position: Tween<Offset>(
              begin: const Offset(1, 0),
              end: Offset.zero,
            ).animate(curved),
            child: child,
          );
        },
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);

    return Container(
      color: c.bg,
      child: Stack(
        children: [
          // 背景氛围光斑（品牌蓝，极低透明度）
          Positioned(
            top: -60,
            right: -40,
            child: _AmbientGlow(color: c.accent, size: 300),
          ),
          Positioned(
            bottom: -80,
            left: -60,
            child: _AmbientGlow(color: c.accent, size: 340),
          ),

          Positioned.fill(
            child: MobileTasksScreen(
              controller: _controller,
              settings: _settings,
              onOpenSettings: _openSettings,
            ),
          ),
        ],
      ),
    );
  }
}

/// 背景氛围光斑
class _AmbientGlow extends StatelessWidget {
  final Color color;
  final double size;

  const _AmbientGlow({required this.color, required this.size});

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return IgnorePointer(
      child: Container(
        width: size,
        height: size,
        decoration: BoxDecoration(
          shape: BoxShape.circle,
          gradient: RadialGradient(
            colors: [m.soft(color), color.withValues(alpha: 0.0)],
          ),
        ),
      ),
    );
  }
}

/// 更新提示弹层的用户选择
enum _UpdateChoice { update, later, skip }
