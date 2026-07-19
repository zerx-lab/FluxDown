// 详情面板钉底 footer 按钮的窄宽防溢出回归。
//
// 根因（2026-07，面板最小宽 240 下「文件夹/复制链接」溢出 12/24px）：
// Flutter Flex 给非 flex 子项的主轴约束无界，ShadButton 内部 Row 把无界
// 宽度传给 child——FittedBox 直接作 child 拿不到有限上界，scaleDown 永不
// 生效。修复 = `expands: true`（child 变 flex 子项获得有界宽度）+ FittedBox。
//
// 本测试 pump detail_panel.dart 的真实公开 widget（DetailFooterActionButton /
// DetailFooterPrimaryButton），按真实 footer 的三按钮 Row 结构在 180–260
// 宽度域扫描，断言零溢出异常；并覆盖 resuming 态（spinner+文字变体曾内嵌
// Flexible，在 FittedBox 无界测量下会断言崩溃——同样必须零异常）。
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/i18n/locale_provider.dart';
import 'package:flux_down/src/models/download_task.dart';
import 'package:flux_down/src/theme/app_theme.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';
import 'package:flux_down/src/widgets/detail_panel.dart';
import 'package:flux_down/src/widgets/group_detail_panel.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

Widget _harness(Widget home) {
  final tokens = FluxThemeTokens.defaultDark();
  final theme = buildThemeFromTokens(tokens);
  return LocaleScope(
    s: S.of('zh'),
    child: FluxThemeScope(
      tokens: tokens,
      child: ShadTheme(
        data: theme,
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: DefaultTextStyle(
            style: theme.textTheme.p,
            child: WidgetsApp(
              color: theme.colorScheme.primary,
              debugShowCheckedModeBanner: false,
              home: home,
              pageRouteBuilder: <T>(RouteSettings s, WidgetBuilder b) =>
                  PageRouteBuilder<T>(
                    settings: s,
                    pageBuilder: (context, _, _) => b(context),
                  ),
            ),
          ),
        ),
      ),
    ),
  );
}

/// 与 `_buildActionsFooter` 相同的三按钮行结构（结构本身是稳定的
/// Expanded 等分，按钮实现直接用真实 widget）。
Widget _footerRow(TaskStatus status) {
  return Container(
    padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
    child: Row(
      children: [
        Expanded(
          child: DetailFooterPrimaryButton(
            status: status,
            onPause: () {},
            onResume: () {},
          ),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: DetailFooterActionButton(
            icon: LucideIcons.folderOpen,
            label: S.of('zh').detailActionFolder,
            onPressed: () {},
          ),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: DetailFooterActionButton(
            icon: LucideIcons.link,
            label: S.of('zh').detailActionCopyLink,
            onPressed: () {},
          ),
        ),
      ],
    ),
  );
}

void main() {
  for (final status in [
    TaskStatus.downloading,
    TaskStatus.resuming,
    TaskStatus.paused,
  ]) {
    for (final width in [180.0, 200.0, 220.0, 240.0, 260.0]) {
      testWidgets('footer 三按钮 status=$status width=$width 零溢出', (
        tester,
      ) async {
        final errors = <FlutterErrorDetails>[];
        final prev = FlutterError.onError;
        FlutterError.onError = errors.add;
        await tester.pumpWidget(
          _harness(
            Center(
              child: SizedBox(
                width: width,
                height: 200,
                child: _footerRow(status),
              ),
            ),
          ),
        );
        await tester.pump();
        FlutterError.onError = prev;

        expect(
          errors.map((e) => e.exceptionAsString()).toList(),
          isEmpty,
          reason: 'status=$status width=$width 不应有布局异常',
        );
      });
    }
  }

  // 组详情面板概览动作行（全部暂停/恢复 + 重试失败项 + 文件夹）——
  // 有失败项时非 flex 子项挤占宽度，主按钮剩余宽最窄（2026-07 溢出
  // 6.5px 的实际场景）。面板最小宽 240，向下多扫两档裕量。
  for (final showRetry in [false, true]) {
    for (final hasActive in [false, true]) {
      for (final width in [200.0, 220.0, 240.0, 260.0]) {
        testWidgets(
          '组动作行 retry=$showRetry active=$hasActive width=$width 零溢出',
          (tester) async {
            final errors = <FlutterErrorDetails>[];
            final prev = FlutterError.onError;
            FlutterError.onError = errors.add;
            await tester.pumpWidget(
              _harness(
                Center(
                  child: SizedBox(
                    width: width,
                    height: 200,
                    child: Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 16),
                      child: GroupDetailActionsRow(
                        hasActive: hasActive,
                        showRetry: showRetry,
                        onPauseAll: () {},
                        onResumeAll: () {},
                        onRetryFailed: () {},
                        onOpenFolder: () {},
                      ),
                    ),
                  ),
                ),
              ),
            );
            await tester.pump();
            FlutterError.onError = prev;

            expect(
              errors.map((e) => e.exceptionAsString()).toList(),
              isEmpty,
              reason: 'retry=$showRetry active=$hasActive width=$width '
                  '不应有布局异常',
            );
          },
        );
      }
    }
  }
}
