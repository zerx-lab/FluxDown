// manifest_select_dialog 的渲染/交互冒烟测试（v1.6 下钻导航版）。
//
// 纯逻辑已在 manifest_selection_test.dart 覆盖；本文件只验证真实 widget
// 树能装配、渲染且交互不抛异常——弹窗结构复杂（下钻导航 + 高级面板 +
// 双拆分按钮），flutter analyze 无法发现的运行期布局/约束错误（如
// Expanded 缺少有界高度）需要真正 pump 一次才能捉住。
//
// 与 quick_download_form_append_test.dart 同构的最小主题管线（FluxThemeScope
// + ShadTheme + WidgetsApp）：不引入 MaterialApp / 完整 ShadApp。不点击
// 「开始下载」/「稍后下载」——那会触发 `CreateTaskGroup(...).sendSignalToRust()`
// 需要原生 rinf runtime，超出本测试范围。
//
// 大量展示文本经 `Text.rich` 渲染（目录链/文件名/高级面板字段标签），
// `find.text` 默认不匹配富文本——全文件统一传 `findRichText: true`
// （对纯 `Text` 同样有效，是能力超集，不会引入误匹配）。

import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/i18n/locale_provider.dart';
import 'package:flux_down/src/models/download_queue.dart';
import 'package:flux_down/src/theme/app_theme.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';
import 'package:flux_down/src/widgets/manifest_select_dialog.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

ResolvePreviewResult _manifest() => const ResolvePreviewResult(
  previewId: 'p1',
  name: '测试合集',
  sourceUrl: 'https://pan.example.com/s/abc',
  error: '',
  items: [
    ManifestItemDto(id: '1', name: 'a.mkv', path: '正片', size: 1000, variants: []),
    ManifestItemDto(id: '2', name: 'b.srt', path: '字幕', size: 0, variants: []), // 大小未知
    ManifestItemDto(id: '3', name: 'root.nfo', path: '', size: 200, variants: []),
  ],
);

List<DownloadQueue> _queues() => const [
  DownloadQueue(
    queueId: kMainQueueId,
    name: '主队列',
    speedLimitKbps: 0,
    maxConcurrent: 0,
    defaultSaveDir: '',
    position: 0,
  ),
  DownloadQueue(
    queueId: kLaterQueueId,
    name: '稍后下载',
    speedLimitKbps: 0,
    maxConcurrent: 0,
    defaultSaveDir: '',
    position: 1,
  ),
];

/// 与 quick_download_form_append_test.dart `_wrapForm` 同构的最小主题管线：
/// FluxThemeScope 供 AppColors.of/AppMetrics.of 读取 token，ShadTheme 供
/// shadcn_ui 组件读取主题，WidgetsApp 提供 Navigator/Overlay（showShadDialog
/// 依赖）。
Widget _harness(Widget home) {
  final tokens = FluxThemeTokens.defaultDark();
  final theme = buildThemeFromTokens(tokens);
  return LocaleScope(
    // 显式固定语言，不依赖测试运行环境的系统 locale（否则 CI 机器语言不同
    // 会导致中文文案断言失配）。
    s: S.of('zh'),
    child: FluxThemeScope(
      tokens: tokens,
      child: ShadTheme(
        data: theme,
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: DefaultTextStyle(
            style: theme.textTheme.p.copyWith(color: theme.colorScheme.foreground),
            child: ShadToaster(
              child: ShadSonner(
                child: WidgetsApp(
                  color: theme.colorScheme.primary,
                  debugShowCheckedModeBanner: false,
                  home: home,
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
    ),
  );
}

/// 打开对话框；返回尚未 resolve 的 Future——调用方在完成交互（如点取消）
/// 后再 await，拿到真实的确认/取消结果。
Future<Future<bool>> _openDialog(WidgetTester tester) async {
  late Future<bool> pending;
  await tester.pumpWidget(
    _harness(
      Builder(
        builder: (context) => ShadButton(
          onPressed: () {
            pending = showManifestSelectDialog(
              context,
              queues: _queues(),
              manifest: _manifest(),
              sourceUrl: _manifest().sourceUrl,
              initialSaveDir: r'C:\downloads',
              initialQueueId: kMainQueueId,
              segments: 0,
              cookies: 'sid=abc',
              referrer: '',
              userAgent: '',
              proxyUrl: '',
              extraHeaders: const {},
              ignoreTlsErrors: false,
            );
          },
          child: const Text('open'),
        ),
      ),
    ),
  );
  await tester.tap(find.byType(ShadButton));
  await tester.pumpAndSettle();
  return pending;
}

Finder _text(String text) => find.text(text, findRichText: true);
Finder _textContaining(String text) =>
    find.textContaining(text, findRichText: true);

void main() {
  setUpAll(() async {
    await I18nStore.load();
  });

  testWidgets('弹窗打开：摘要区/工具栏/面包屑/文件列表/底栏全部渲染，无异常', (tester) async {
    await _openDialog(tester);

    // 摘要区：组名输入框默认值 = manifest.name。
    expect(_text('测试合集'), findsOneWidget);
    // 底栏：初始 0 选中（openManifestModal 语义）→「未选择文件」文案。
    expect(_text('未选择文件'), findsOneWidget);
  });

  testWidgets('下钻导航：点击目录行进入子层，面包屑显示返回上级；点返回上级回根', (tester) async {
    await _openDialog(tester);

    // 根层：字幕/正片两个目录行（各自只有一个直属文件，无单链合并对象）
    // + 一个根级散件文件行 root.nfo。
    expect(_text('root.nfo'), findsOneWidget);
    await tester.tap(_text('正片'));
    await tester.pumpAndSettle();
    expect(_text('a.mkv'), findsOneWidget);

    // 返回上级：面包屑左侧箭头。
    final upIcon = find.byIcon(LucideIcons.arrowLeft);
    expect(upIcon, findsOneWidget);
    await tester.tap(upIcon);
    await tester.pumpAndSettle();
    expect(_text('root.nfo'), findsOneWidget);
  });

  testWidgets('全选后底栏计数更新，含未知大小项时显示 ≈ 与未知计数提示', (tester) async {
    await _openDialog(tester);

    await tester.tap(_text('全选'));
    await tester.pumpAndSettle();

    // 3 项全选，其中 1 项 size=0（未知）→ 摘要含"≈"与"大小未知"提示。
    expect(_textContaining('已选 3 项'), findsOneWidget);
    expect(_textContaining('大小未知'), findsOneWidget);
  });

  testWidgets('高级选项折叠条可展开，展开后代理/UA/Cookie/请求头字段可见', (tester) async {
    await _openDialog(tester);

    expect(_text('高级选项'), findsOneWidget);
    await tester.tap(_text('高级选项'));
    await tester.pumpAndSettle();

    expect(_textContaining('留空则使用全局设置'), findsOneWidget);
    expect(_textContaining('网盘登录态'), findsOneWidget);
    expect(_textContaining('自定义请求头'), findsOneWidget);
  });

  testWidgets('取消关闭对话框并返回 false', (tester) async {
    final pending = await _openDialog(tester);
    await tester.tap(_text('取消'));
    await tester.pumpAndSettle();
    expect(await pending, isFalse);
    expect(_text('未选择文件'), findsNothing);
  });
}
