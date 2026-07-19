import 'dart:convert';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:launch_at_startup/launch_at_startup.dart';

import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/models/settings_provider.dart';
import 'package:flux_down/src/popup/popup_payload.dart';
import 'package:flux_down/src/services/popup_window_service.dart';
import 'package:flux_down/src/theme/theme_provider.dart';

/// 复现浏览器扩展批量下载竞态：N 条 ExternalDownloadRequest 短时爆发时，
/// 只允许发出一次原生 show，其余请求必须经 append 合入（含 reveal 前重试）。
/// 另覆盖清单视图期间的请求托管缓冲：受理即拥有，会话结束/清单退出后
/// 经 redispatch 重新分发，任何路径都不静默丢请求。
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

  testWidgets('清单视图期间请求托管缓冲，关窗后按序冲刷重分发', (tester) async {
    final appendUrls = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      switch (call.method) {
        case 'show':
          return true;
        case 'append':
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
    final redispatched = <ExternalDownloadRequest>[];
    svc.redispatch = redispatched.add;
    addTearDown(() => svc.redispatch = null);

    expect(
      await svc.tryShow(req: req('http://m/src'), resolvedSaveDir: 'D:/dl'),
      isTrue,
    );
    svc.debugSetManifestShowing(true);

    // 清单视图期间：受理（true）但绝不 append 进 Offstage 表单
    expect(await svc.tryAppend(req('http://m/b1')), isTrue);
    expect(await svc.tryAppend(req('http://m/b2')), isTrue);
    expect(appendUrls, isEmpty, reason: '清单态请求不得托付给注定丢弃的表单');
    expect(redispatched, isEmpty, reason: '会话未结束不得提前重分发');

    // 会话结束（确认建组/关闭同路径 close()）→ 按到达顺序冲刷
    await svc.close();
    await tester.pump();
    expect(redispatched.map((r) => r.url), ['http://m/b1', 'http://m/b2']);
  });

  testWidgets('可见期间的音视频轨对请求托管缓冲（原为静默丢弃），冲刷保留 audioUrl', (
    tester,
  ) async {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      switch (call.method) {
        case 'show':
          return true;
        case 'append':
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
    final redispatched = <ExternalDownloadRequest>[];
    svc.redispatch = redispatched.add;
    addTearDown(() => svc.redispatch = null);

    expect(
      await svc.tryShow(req: req('http://t/main'), resolvedSaveDir: 'D:/dl'),
      isTrue,
    );
    final trackPair = ExternalDownloadRequest(
      url: 'http://t/video',
      filename: 'v.mp4',
      fileSize: 0,
      mimeType: 'video/mp4',
      cookies: '',
      referrer: 'http://t/page',
      saveDir: '',
      audioUrl: 'http://t/audio',
    );
    expect(await svc.tryAppend(trackPair), isTrue);

    await svc.close();
    await tester.pump();
    expect(redispatched, hasLength(1));
    expect(redispatched.single.url, 'http://t/video');
    expect(
      redispatched.single.audioUrl,
      'http://t/audio',
      reason: '托管缓冲保留完整请求载荷（audioUrl 不丢）',
    );
  });

  testWidgets('onRelay manifestClosed：清单退出即冲刷，窗口保持可见', (tester) async {
    final showPayloads = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      switch (call.method) {
        case 'show':
          showPayloads.add(call.arguments as String);
          return true;
        case 'append':
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
    final redispatched = <ExternalDownloadRequest>[];
    svc.redispatch = redispatched.add;
    addTearDown(() => svc.redispatch = null);

    expect(
      await svc.tryShow(req: req('http://c/src'), resolvedSaveDir: 'D:/dl'),
      isTrue,
    );
    svc.debugSetManifestShowing(true);
    expect(await svc.tryAppend(req('http://c/late')), isTrue);

    // 弹窗经原生中继上报清单视图退出（真实通道路径：onRelay 平台消息）
    final requestId =
        (jsonDecode(showPayloads.single) as Map<String, dynamic>)['requestId']
            as int;
    final closedMsg = PopupRelayMessage(
      kind: kPopupRelayManifestClosed,
      requestId: requestId,
      seq: 1,
    ).toJsonString();
    await tester.binding.defaultBinaryMessenger.handlePlatformMessage(
      'fluxdown/popup_host',
      const StandardMethodCodec().encodeMethodCall(
        MethodCall('onRelay', closedMsg),
      ),
      (_) {},
    );
    await tester.pump();

    expect(redispatched.map((r) => r.url), ['http://c/late']);
    expect(svc.isVisible, isTrue, reason: '清单退出回表单，会话不结束');

    await svc.close();
    await tester.pump(const Duration(milliseconds: 250));
  });
}
