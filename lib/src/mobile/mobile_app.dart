import 'package:flutter/material.dart' show MaterialPageRoute;
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../services/bt_file_selection_service.dart';
import '../services/hls_quality_service.dart';
import '../services/log_service.dart';
import '../services/resolve_variant_service.dart';
import '../theme/app_theme.dart';
import '../theme/flux_theme_tokens.dart';
import '../theme/theme_provider.dart';
import 'mobile_shell.dart';

/// 移动端应用根组件
///
/// 与桌面 [FluxDownApp] 的差异：
/// - 无窗口管理 / 托盘 / 开机启动 / NMH 等桌面服务
/// - 保留 HLS 画质选择与 BT 文件选择服务（Rust 信号驱动的全局弹窗）
/// - 首页为 [MobileShell]（任务列表 + 设置 双屏 + 悬浮 Dock）
class FluxDownMobileApp extends StatefulWidget {
  final ThemeProvider themeProvider;
  final LocaleNotifier localeNotifier;

  const FluxDownMobileApp({
    super.key,
    required this.themeProvider,
    required this.localeNotifier,
  });

  @override
  State<FluxDownMobileApp> createState() => _FluxDownMobileAppState();
}

class _FluxDownMobileAppState extends State<FluxDownMobileApp> {
  final _navigatorKey = GlobalKey<NavigatorState>();

  @override
  void initState() {
    super.initState();
    logInfo('MobileApp', 'initState');
    widget.themeProvider.addListener(_onChanged);
    widget.localeNotifier.addListener(_onChanged);

    // Rust 信号驱动的全局选择弹窗（HLS 画质 / BT 文件选择 / 插件变体选择）
    HlsQualityService.init(navigatorKey: _navigatorKey);
    ResolveVariantService.init(navigatorKey: _navigatorKey);
    BtFileSelectionService.init(navigatorKey: _navigatorKey);
  }

  @override
  void dispose() {
    HlsQualityService.shutdown();
    ResolveVariantService.shutdown();
    BtFileSelectionService.shutdown();
    widget.localeNotifier.removeListener(_onChanged);
    widget.themeProvider.removeListener(_onChanged);
    super.dispose();
  }

  void _onChanged() {
    if (mounted) setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final FluxThemeTokens tokens = widget.themeProvider.activeTokens(context);
    final theme = buildThemeFromTokens(tokens);

    return LocaleScope(
      s: widget.localeNotifier.s,
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
                  alignment: Alignment.topCenter,
                  padding: EdgeInsets.only(
                    top: MediaQuery.of(context).padding.top + 12,
                    left: 16,
                    right: 16,
                    bottom: 16,
                  ),
                  child: WidgetsApp(
                    navigatorKey: _navigatorKey,
                    color: theme.colorScheme.primary,
                    debugShowCheckedModeBanner: false,
                    home: MobileShell(
                      themeProvider: widget.themeProvider,
                      localeNotifier: widget.localeNotifier,
                    ),
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
