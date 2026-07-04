import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';
import '../theme/theme_provider.dart';
import 'mobile_ui.dart';
import 'screens/mobile_settings_screen.dart';
import 'services/mobile_storage_service.dart';
import '../services/foreground_service.dart';
import 'screens/mobile_tasks_screen.dart';
import 'services/share_intent_service.dart';
import 'sheets/mobile_new_download_sheet.dart';

/// 移动端根壳：任务列表 / 设置 两屏切换 + 悬浮玻璃 Dock
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

class _MobileShellState extends State<MobileShell> {
  final _controller = DownloadController();
  final _settings = SettingsProvider(enableFileAssoc: false);
  int _tab = 0;
  bool _sheetOpen = false;

  @override
  void initState() {
    super.initState();
    _settings.requestConfig();
    _ensureAndroidSaveDir();
    // 前台服务：切换应用时保活进程、持续下载，任务栏常驻进度通知（仅移动端生效）
    ForegroundServiceManager.instance.start(
      _controller,
      widget.localeNotifier.s,
    );
    widget.localeNotifier.addListener(_onLocaleChanged);
    // 系统分享 / URL scheme 接入：收到链接切到下载页并弹新建下载弹层
    ShareIntentService.init(_onShared);
  }

  void _onLocaleChanged() {
    ForegroundServiceManager.instance.updateStrings(widget.localeNotifier.s);
  }

  /// 收到系统分享 / URL scheme 唤起的链接：切到下载页，弹新建下载弹层
  /// 并预填 URL。已有弹层打开时忽略，避免叠层。
  Future<void> _onShared(String url) async {
    if (!mounted || _sheetOpen) return;
    setState(() => _tab = 0);
    _sheetOpen = true;
    try {
      await showMobileNewDownloadSheet(
        context,
        controller: _controller,
        settings: _settings,
        initialUrl: url,
      );
    } finally {
      _sheetOpen = false;
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
    ShareIntentService.shutdown();
    widget.localeNotifier.removeListener(_onLocaleChanged);
    ForegroundServiceManager.instance.stop();
    _controller.dispose();
    _settings.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final bottomInset = MediaQuery.paddingOf(context).bottom;

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

          // 两屏
          Positioned.fill(
            child: IndexedStack(
              index: _tab,
              children: [
                MobileTasksScreen(
                  controller: _controller,
                  settings: _settings,
                ),
                MobileSettingsScreen(
                  settings: _settings,
                  themeProvider: widget.themeProvider,
                  localeNotifier: widget.localeNotifier,
                ),
              ],
            ),
          ),

          // 悬浮玻璃 Dock
          Positioned(
            left: 0,
            right: 0,
            bottom: MobileDims.dockBottomGap + bottomInset,
            child: Center(
              child: ClipRRect(
                borderRadius: BorderRadius.circular(999),
                child: BackdropFilter(
                  filter: mobileBlurFilter,
                  child: Container(
                    padding: const EdgeInsets.all(4),
                    decoration: BoxDecoration(
                      color: c.surface1.withValues(alpha: 0.68),
                      borderRadius: BorderRadius.circular(999),
                      border: Border.all(
                        color: c.border.withValues(alpha: 0.8),
                      ),
                      boxShadow: [
                        BoxShadow(
                          color: c.shadow.withValues(alpha: 0.14),
                          blurRadius: 28,
                          offset: const Offset(0, 8),
                        ),
                      ],
                    ),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        _DockItem(
                          icon: LucideIcons.download,
                          label: s.mobileNavDownloads,
                          selected: _tab == 0,
                          onTap: () => setState(() => _tab = 0),
                        ),
                        const SizedBox(width: 2),
                        _DockItem(
                          icon: LucideIcons.settings,
                          label: s.settings,
                          selected: _tab == 1,
                          onTap: () => setState(() => _tab = 1),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _DockItem extends StatelessWidget {
  final IconData icon;
  final String label;
  final bool selected;
  final VoidCallback onTap;

  const _DockItem({
    required this.icon,
    required this.label,
    required this.selected,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final color = selected ? c.accent : c.textSecondary;
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: Container(
        width: 76,
        padding: const EdgeInsets.symmetric(vertical: 6),
        decoration: BoxDecoration(
          color: selected
              ? c.accent.withValues(alpha: 0.10)
              : const Color(0x00000000),
          borderRadius: BorderRadius.circular(999),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 19, color: color),
            const SizedBox(height: 2),
            Text(
              label,
              style: TextStyle(
                fontSize: 10,
                fontWeight: FontWeight.w500,
                color: color,
              ),
            ),
          ],
        ),
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
    return IgnorePointer(
      child: Container(
        width: size,
        height: size,
        decoration: BoxDecoration(
          shape: BoxShape.circle,
          gradient: RadialGradient(
            colors: [
              color.withValues(alpha: 0.10),
              color.withValues(alpha: 0.0),
            ],
          ),
        ),
      ),
    );
  }
}
