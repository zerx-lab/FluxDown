// NumberSelector（预设 + 自定义数字输入）行为契约测试 —
// 「最大同时下载数」「Auto 模式连接上限」两处设置共用该组件：
// - 预设下拉：点选预设值 → onChanged 上报该值；
// - 「自定义」项：原地切换为数字输入框，输入合法值逐字上报；
// - 超过 max 的输入被钳制为 max 并上报；低于 min（含空）不产生回调；
// - 初始值不在预设列表中（此前自定义持久化过）→ 直接以输入框形态呈现。
//
// 主题管线复用 quick_download_form_append_test 的最小包装（FluxThemeScope +
// ShadTheme + WidgetsApp），额外套 LocaleScope 提供 i18n。
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/i18n/locale_provider.dart';
import 'package:flux_down/src/theme/app_theme.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';
import 'package:flux_down/src/widgets/number_selector.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

Widget _wrap(Widget child) {
  final tokens = FluxThemeTokens.defaultDark();
  final theme = buildThemeFromTokens(tokens);
  return FluxThemeScope(
    tokens: tokens,
    child: ShadTheme(
      data: theme,
      child: LocaleScope(
        s: S.of('zh'),
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: DefaultTextStyle(
            style: theme.textTheme.p.copyWith(
              color: theme.colorScheme.foreground,
            ),
            child: WidgetsApp(
              color: theme.colorScheme.primary,
              debugShowCheckedModeBanner: false,
              home: Center(child: child),
              pageRouteBuilder: <T>(RouteSettings settings, WidgetBuilder b) {
                return PageRouteBuilder<T>(
                  settings: settings,
                  pageBuilder: (ctx, _, _) => b(ctx),
                );
              },
            ),
          ),
        ),
      ),
    ),
  );
}

void main() {
  // NumberSelector 的「自定义」项经 LocaleScope→S.of('zh')→I18nStore 查表，
  // 需先加载翻译表（assets/i18n/*.json），否则文案回退致 find.text 落空。
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(I18nStore.load);

  testWidgets('点选预设值上报该值', (tester) async {
    int? reported;
    await tester.pumpWidget(
      _wrap(
        NumberSelector(
          value: 5,
          presets: const [1, 2, 3, 5, 8, 10],
          min: 1,
          max: 50,
          fallback: 5,
          onChanged: (v) => reported = v,
        ),
      ),
    );

    await tester.tap(find.byType(ShadSelect<int>));
    await tester.pumpAndSettle();
    await tester.tap(find.text('8').last);
    await tester.pumpAndSettle();

    expect(reported, 8);
  });

  testWidgets('选择「自定义」切换为输入框并上报输入值', (tester) async {
    int? reported;
    await tester.pumpWidget(
      _wrap(
        NumberSelector(
          value: 5,
          presets: const [1, 2, 3, 5, 8, 10],
          min: 1,
          max: 50,
          fallback: 5,
          onChanged: (v) => reported = v,
        ),
      ),
    );

    await tester.tap(find.byType(ShadSelect<int>));
    await tester.pumpAndSettle();
    await tester.tap(find.text('自定义').last);
    await tester.pumpAndSettle();

    expect(find.byType(ShadInput), findsOneWidget);

    await tester.enterText(find.byType(ShadInput), '23');
    await tester.pump();
    expect(reported, 23);
  });

  testWidgets('自定义输入超过 max 被钳制为 max', (tester) async {
    int? reported;
    await tester.pumpWidget(
      _wrap(
        NumberSelector(
          value: 7, // 不在预设 → 初始即自定义输入形态
          presets: const [1, 2, 3, 5, 8, 10],
          min: 1,
          max: 50,
          fallback: 5,
          onChanged: (v) => reported = v,
        ),
      ),
    );

    // 初始值不在预设列表 → 直接呈现输入框，预填当前值
    expect(find.byType(ShadInput), findsOneWidget);
    expect(find.text('7'), findsOneWidget);

    await tester.enterText(find.byType(ShadInput), '99');
    await tester.pump();
    expect(reported, 50, reason: '超过 max 的输入应钳制为 max');
  });

  testWidgets('低于 min 的输入不产生回调', (tester) async {
    int? reported;
    await tester.pumpWidget(
      _wrap(
        NumberSelector(
          value: 7,
          presets: const [1, 2, 3, 5, 8, 10],
          min: 1,
          max: 50,
          fallback: 5,
          onChanged: (v) => reported = v,
        ),
      ),
    );

    await tester.enterText(find.byType(ShadInput), '0');
    await tester.pump();
    expect(reported, isNull, reason: '低于 min 的中间态输入不应上报');
  });

  testWidgets('外部值异步变为非预设值时切换到输入框形态', (tester) async {
    Widget build(int value) => _wrap(
      NumberSelector(
        value: value,
        presets: const [1, 2, 3, 5, 8, 10],
        min: 1,
        max: 50,
        fallback: 5,
        onChanged: (_) {},
      ),
    );

    // 初始为预设值（模拟设置尚未从 Rust 加载完成时的默认值）
    await tester.pumpWidget(build(5));
    expect(find.byType(ShadSelect<int>), findsOneWidget);

    // 异步加载完成，真实持久化值是自定义的 23 → 应切换为输入框并显示 23
    await tester.pumpWidget(build(23));
    await tester.pump();
    expect(find.byType(ShadInput), findsOneWidget);
    expect(find.text('23'), findsOneWidget);
  });
}
