// 独立小窗 append 模式回归测试 —
// QuickDownloadFormController.appendUrls 是「小窗可见期间新到的外部
// 下载请求合入当前表单」这条流程的唯一入口，行为契约：
// - 已挂载表单：把尚未出现在 URL 输入框里的 URL 追加（换行分隔），
//   同 URL 去重，返回实际追加条数；带 out= 文件名选项行的 URL 追加时
//   连同选项行一并保留；
// - 未挂载表单（controller 未绑定任何 State，例如小窗尚未渲染完成时
//   宿主就收到新请求）：安全返回 0，不抛异常，不影响后续挂载。
//
// 本测试用与 popup_app.dart 相同的最小主题管线（FluxThemeScope + ShadTheme
// + WidgetsApp）包裹 QuickDownloadForm，复现独立小窗渲染该表单所需的最小
// context 依赖，不引入 MaterialApp / 完整 ShadApp。
import 'package:flutter/material.dart' show TextField;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/theme/app_theme.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';
import 'package:flux_down/src/widgets/quick_download_form.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

/// 最小 QuickDownloadFormHost fake：队列空、默认线程数 0、无历史线程记忆、
/// 目录选择器恒返回 null（本测试不触碰目录选择交互）。
class _FakeHost implements QuickDownloadFormHost {
  @override
  List<QuickQueueOption> get queues => const [];

  @override
  int get defaultSegments => 0;

  @override
  String get lastDialogThreads => '';

  @override
  Future<String?> pickDirectory({
    required String dialogTitle,
    String? initialDirectory,
  }) async => null;
}

/// 用与 popup_app.dart 同构的最小主题管线包裹表单：FluxThemeScope 供
/// AppColors.of/AppMetrics.of 读取 token，ShadTheme 供 shadcn_ui 组件读取
/// 主题，WidgetsApp 提供 Directionality 之外的 Navigator/Overlay（下拉选择
/// 器等 shadcn_ui 组件依赖）。
Widget _wrapForm(QuickDownloadForm form) {
  final tokens = FluxThemeTokens.defaultDark();
  final theme = buildThemeFromTokens(tokens);
  return FluxThemeScope(
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
              child: WidgetsApp(
                color: theme.colorScheme.primary,
                debugShowCheckedModeBanner: false,
                home: form,
                pageRouteBuilder: <T>(
                  RouteSettings settings,
                  WidgetBuilder builder,
                ) {
                  return PageRouteBuilder<T>(
                    settings: settings,
                    pageBuilder: (context, _, _) => builder(context),
                  );
                },
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

/// 取出已挂载表单里 URL 输入框的当前文本。表单里唯一的裸 [TextField] 就是
/// URL 框——其余输入（重命名/代理/UA 等）都是 ShadInput，内部用 EditableText
/// 实现，不会被 find.byType(TextField) 命中，因此定位无歧义。
String _urlBoxText(WidgetTester tester) =>
    tester.widget<TextField>(find.byType(TextField)).controller!.text;

Future<QuickDownloadFormController> _pumpForm(
  WidgetTester tester, {
  required String initialUrl,
}) async {
  final controller = QuickDownloadFormController();
  await tester.pumpWidget(
    _wrapForm(
      QuickDownloadForm(
        initialUrl: initialUrl,
        initialFileName: '',
        initialSaveDir: r'C:\downloads',
        defaultQueueId: '',
        initialCookies: '',
        host: _FakeHost(),
        onSubmit: (_) {},
        onCancel: () {},
        controller: controller,
      ),
    ),
  );
  await tester.pump();
  return controller;
}

void main() {
  group('QuickDownloadFormController.appendUrls', () {
    test('表单未挂载（controller 未绑定 State）时安全返回 0', () {
      final controller = QuickDownloadFormController();
      expect(controller.appendUrls('https://example.com/a.zip'), 0);
    });

    testWidgets('追加尚未出现的新 URL：返回追加条数，换行拼接到既有文本后', (tester) async {
      final controller = await _pumpForm(
        tester,
        initialUrl: 'https://example.com/a.zip',
      );

      final appended = controller.appendUrls('https://example.com/b.zip');

      expect(appended, 1);
      expect(
        _urlBoxText(tester),
        'https://example.com/a.zip\nhttps://example.com/b.zip',
      );
    });

    testWidgets('追加已存在的 URL：去重跳过，返回 0 且文本不变', (tester) async {
      const initialUrl = 'https://example.com/a.zip';
      final controller = await _pumpForm(tester, initialUrl: initialUrl);

      final appended = controller.appendUrls(initialUrl);

      expect(appended, 0);
      expect(_urlBoxText(tester), initialUrl);
    });

    testWidgets('追加带 out= 文件名选项行的 URL：选项行随 URL 一并保留', (tester) async {
      final controller = await _pumpForm(
        tester,
        initialUrl: 'https://example.com/a.zip',
      );

      final appended = controller.appendUrls(
        'https://example.com/c.zip\n out=custom-name.zip',
      );

      expect(appended, 1);
      expect(
        _urlBoxText(tester),
        'https://example.com/a.zip\n'
        'https://example.com/c.zip\n'
        ' out=custom-name.zip',
      );
    });

    testWidgets('混合追加：新 URL 与已存在 URL 并存时，只追加新条目、计数只算新条目', (
      tester,
    ) async {
      final controller = await _pumpForm(
        tester,
        initialUrl: 'https://example.com/a.zip',
      );

      final appended = controller.appendUrls(
        'https://example.com/a.zip\nhttps://example.com/d.zip',
      );

      expect(appended, 1);
      expect(
        _urlBoxText(tester),
        'https://example.com/a.zip\nhttps://example.com/d.zip',
      );
    });

    testWidgets('追加空白/无有效 URL 的文本：返回 0 且不改动输入框文本', (tester) async {
      const initialUrl = 'https://example.com/a.zip';
      final controller = await _pumpForm(tester, initialUrl: initialUrl);

      final appended = controller.appendUrls('   \n# 仅注释，无 URL\n');

      expect(appended, 0);
      expect(_urlBoxText(tester), initialUrl);
    });
  });
}
