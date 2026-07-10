import 'dart:convert';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:launch_at_startup/launch_at_startup.dart';

import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/models/settings_provider.dart';
import 'package:flux_down/src/services/popup_window_service.dart';
import 'package:flux_down/src/theme/theme_provider.dart';

/// 复现浏览器扩展批量下载竞态：N 条 ExternalDownloadRequest 短时爆发时，
/// 只允许发出一次原生 show，其余请求必须经 append 合入（含 reveal 前重试）。
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  launchAtStartup.setup(
    appName: 'FluxDownTest',
    appPath: Platform.resolvedExecutable,
  );

  const channel = MethodChannel('fluxdown/popup_host');

  ExternalDownloadRequest req(String url) => ExternalDownloadRequest(
    url: url,
    filename: '',
    fileSize: 0,
    mimeType: '',
    cookies: '',
    referrer: '',
    saveDir: '',
    audioUrl: '',
  );

  Future<GlobalKey<NavigatorState>> pumpHost(WidgetTester tester) async {
    final navKey = GlobalKey<NavigatorState>();
    await tester.pumpWidget(
      WidgetsApp(
        color: const Color(0xFF000000),
        navigatorKey: navKey,
        onGenerateRoute: (_) =>
            PageRouteBuilder(pageBuilder: (_, _, _) => const SizedBox()),
      ),
    );
    return navKey;
  }

  testWidgets('批量爆发只 show 一次，其余排队 append', (tester) async {
    final showPayloads = <String>[];
    final appendUrls = <String>[];
    var appendRefusals = 2; // 模拟 reveal 前原生拒绝两次

    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      switch (call.method) {
        case 'show':
          showPayloads.add(call.arguments as String);
          // 模拟原生 show 握手耗时
          await Future<void>.delayed(const Duration(milliseconds: 50));
          return true;
        case 'append':
          if (appendRefusals > 0) {
            appendRefusals--;
            return false; // 窗口尚未 reveal
          }
          appendUrls.add(call.arguments as String);
          return true;
        case 'close':
          return null;
      }
      return null;
    });

    final navKey = await pumpHost(tester);
    final themeProvider = ThemeProvider();
    final settings = SettingsProvider(enableFileAssoc: false);
    addTearDown(settings.dispose);
    final svc = PopupWindowService.instance;
    svc.init(themeProvider: themeProvider, navigatorKey: navKey);

    // 请求 1 发起 show（不 await，模拟信号回调交错）
    final showFuture = svc.tryShow(
      req: req('http://a/1'),
      resolvedSaveDir: 'D:/dl',
    );
    // show 在途：后续请求必须走 append 排队而不是再 show
    expect(svc.isVisible, isTrue);
    expect(await svc.tryAppend(req('http://a/2')), isTrue);
    expect(await svc.tryAppend(req('http://a/3')), isTrue);

    // 推进 fake 时钟让 mock show 的 50ms 延时完成
    await tester.pump(const Duration(milliseconds: 60));
    expect(await showFuture, isTrue);
    // 等待冲刷重试（reveal 前两次拒绝 + 200ms 退避）
    for (var i = 0; i < 10 && appendUrls.length < 2; i++) {
      await tester.pump(const Duration(milliseconds: 250));
    }

    expect(showPayloads, hasLength(1), reason: '只允许一次 show，载荷不被覆盖');
    final payload = jsonDecode(showPayloads.single) as Map<String, dynamic>;
    expect((payload['req'] as Map<String, dynamic>)['url'], 'http://a/1');
    expect(appendUrls, ['http://a/2', 'http://a/3']);

    await svc.close();
    await tester.pump(const Duration(milliseconds: 250));
  });

  testWidgets('关闭后 append 返回 false 走 show 流程', (tester) async {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      switch (call.method) {
        case 'show':
          return true;
        case 'append':
          return false;
        case 'close':
          return null;
      }
      return null;
    });

    final navKey = await pumpHost(tester);
    final themeProvider = ThemeProvider();
    final settings = SettingsProvider(enableFileAssoc: false);
    addTearDown(settings.dispose);
    final svc = PopupWindowService.instance;
    svc.init(themeProvider: themeProvider, navigatorKey: navKey);

    expect(
      await svc.tryShow(req: req('http://b/1'), resolvedSaveDir: 'D:/dl'),
      isTrue,
    );
    // 宽限期内被拒 → 排队处置（返回 true），不复位不重 show
    expect(await svc.tryAppend(req('http://b/2')), isTrue);
    expect(svc.isVisible, isTrue);
    await svc.close();
    expect(svc.isVisible, isFalse);
    // 关闭后 append 直接返回 false（调用方走 show 流程）
    expect(await svc.tryAppend(req('http://b/3')), isFalse);
    await tester.pump(const Duration(milliseconds: 250));
  });
}
