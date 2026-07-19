// 外部唤起独立小窗的载荷/结果 JSON 编解码回归测试。
//
// 主引擎与弹窗引擎运行同一 Dart bundle，经 JSON 字符串传递表单载荷/结果，
// 该 JSON 是两个引擎间的 wire 契约。本测试只断言往返语义（roundtrip +
// 缺省字段容错），不断言实现细节（如 JSON 键顺序 / 内部字段名）。
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/popup/popup_payload.dart';
import 'package:flux_down/src/widgets/manifest_select_view.dart';
import 'package:flux_down/src/widgets/quick_download_form.dart';

/// 递归深度比较，用于校验 tokensJson 这类任意嵌套 Map/List 结构在
/// jsonEncode → jsonDecode 之后语义不变（值相等，不要求同一实例）。
bool _deepEquals(dynamic a, dynamic b) {
  if (a is Map && b is Map) {
    if (a.length != b.length) return false;
    for (final key in a.keys) {
      if (!b.containsKey(key)) return false;
      if (!_deepEquals(a[key], b[key])) return false;
    }
    return true;
  }
  if (a is List && b is List) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (!_deepEquals(a[i], b[i])) return false;
    }
    return true;
  }
  return a == b;
}

void main() {
  group('QuickPopupPayload', () {
    test(
      '全字段 roundtrip：多队列 / 中文文件名 / 多行 URL / 嵌套主题 tokens',
      () {
        const original = QuickPopupPayload(
          requestId: 42,
          url:
              'https://example.com/a.zip\nhttps://example.com/b.zip\nmagnet:?xt=urn:btih:abc',
          filename: '测试文件名 中文 (1).zip',
          fileSize: 123456789,
          mimeType: 'application/zip',
          saveDir: r'C:\Users\zero\Downloads\子目录',
          cookies: 'session=abc123; token=中文值',
          locale: 'zh',
          tokensJson: {
            'colors': {
              'primary': '#FF0000',
              'nested': {'deep': 1, 'deeper': true},
            },
            'radius': 8.5,
            'flag': true,
            'list': [1, 2, 3, '四'],
          },
          defaultSegments: 4,
          lastDialogThreads: '8',
          defaultQueueId: 'queue-1',
          queues: [
            QuickQueueOption(
              queueId: 'queue-1',
              name: '默认队列',
              defaultSegments: 4,
            ),
            QuickQueueOption(
              queueId: 'queue-2',
              name: 'Queue Two',
              defaultSegments: 0,
            ),
          ],
        );

        final decoded = QuickPopupPayload.fromJsonString(
          original.toJsonString(),
        );

        expect(decoded.requestId, original.requestId);
        expect(decoded.url, original.url);
        expect(decoded.filename, original.filename);
        expect(decoded.fileSize, original.fileSize);
        expect(decoded.mimeType, original.mimeType);
        expect(decoded.saveDir, original.saveDir);
        expect(decoded.cookies, original.cookies);
        expect(decoded.locale, original.locale);
        expect(
          _deepEquals(decoded.tokensJson, original.tokensJson),
          isTrue,
          reason: 'tokensJson 嵌套结构应在往返后语义相等',
        );
        expect(decoded.defaultSegments, original.defaultSegments);
        expect(decoded.lastDialogThreads, original.lastDialogThreads);
        expect(decoded.defaultQueueId, original.defaultQueueId);

        expect(decoded.queues.length, original.queues.length);
        for (var i = 0; i < original.queues.length; i++) {
          expect(
            decoded.queues[i].queueId,
            original.queues[i].queueId,
            reason: 'queues[$i].queueId',
          );
          expect(
            decoded.queues[i].name,
            original.queues[i].name,
            reason: 'queues[$i].name',
          );
          expect(
            decoded.queues[i].defaultSegments,
            original.queues[i].defaultSegments,
            reason: 'queues[$i].defaultSegments',
          );
        }
      },
    );

    test('边界：所有字符串字段为空、segments=0、队列为空列表', () {
      const original = QuickPopupPayload(
        requestId: 0,
        url: '',
        filename: '',
        fileSize: 0,
        mimeType: '',
        saveDir: '',
        cookies: '',
        locale: '',
        tokensJson: {},
        defaultSegments: 0,
        lastDialogThreads: '',
        defaultQueueId: '',
        queues: [],
      );

      final decoded = QuickPopupPayload.fromJsonString(
        original.toJsonString(),
      );

      expect(decoded.requestId, 0);
      expect(decoded.url, '');
      expect(decoded.filename, '');
      expect(decoded.fileSize, 0);
      expect(decoded.mimeType, '');
      expect(decoded.saveDir, '');
      expect(decoded.cookies, '');
      // 空字符串是显式提供的合法值，fromJsonString 不应将其误判为
      // “字段缺失”而回退到 'en' 默认 locale。
      expect(decoded.locale, '');
      expect(decoded.tokensJson, isEmpty);
      expect(decoded.defaultSegments, 0);
      expect(decoded.lastDialogThreads, '');
      expect(decoded.defaultQueueId, '');
      expect(decoded.queues, isEmpty);
    });

    test('容错：手工构造最小 JSON，缺失 queues/lastDialogThreads/defaultQueueId 等可选字段不抛异常', () {
      // 仅保留必需的 requestId / req.url / env.tokens（tokensJson 本身无回退，
      // 必须提供），其余可选字段全部省略，模拟弹窗引擎收到旧版/精简载荷。
      const minimalJson =
          '{"requestId":7,"req":{"url":"https://example.com/x"},'
          '"env":{"tokens":{}}}';

      late QuickPopupPayload decoded;
      expect(
        () => decoded = QuickPopupPayload.fromJsonString(minimalJson),
        returnsNormally,
      );

      expect(decoded.requestId, 7);
      expect(decoded.url, 'https://example.com/x');
      expect(decoded.filename, '');
      expect(decoded.fileSize, 0);
      expect(decoded.mimeType, '');
      expect(decoded.saveDir, '');
      expect(decoded.cookies, '');
      expect(decoded.locale, 'en');
      expect(decoded.tokensJson, isEmpty);
      expect(decoded.defaultSegments, 0);
      expect(decoded.lastDialogThreads, '');
      expect(decoded.defaultQueueId, '');
      expect(decoded.queues, isEmpty);
    });

    test('容错：队列元素缺失 name/defaultSegments 时逐项回退默认值', () {
      const json =
          '{"requestId":1,"req":{},"env":{"tokens":{},'
          '"queues":[{"id":"only-id"},{}]}}';

      final decoded = QuickPopupPayload.fromJsonString(json);

      expect(decoded.queues.length, 2);
      expect(decoded.queues[0].queueId, 'only-id');
      expect(decoded.queues[0].name, '');
      expect(decoded.queues[0].defaultSegments, 0);
      expect(decoded.queues[1].queueId, '');
      expect(decoded.queues[1].name, '');
      expect(decoded.queues[1].defaultSegments, 0);
    });
  });

  group('QuickPopupResult', () {
    test('全字段 roundtrip：中文重命名 / 非零线程数 / threadsUserModified=true', () {
      const original = QuickPopupResult(
        requestId: 99,
        form: QuickDownloadFormResult(
          urlText:
              'https://example.com/a.zip\nhttps://example.com/b.zip',
          saveDir: r'D:\下载\分类目录',
          rename: '重命名后的文件 (最终版).zip',
          segments: 16,
          proxyUrl: 'http://127.0.0.1:7890',
          userAgent: 'Mozilla/5.0 (Test Agent)',
          queueId: 'queue-9',
          cookies: 'sid=xyz; theme=dark',
          checksum: 'sha-256=deadbeef0123456789',
          ignoreTlsErrors: true,
          threadsUserModified: true,
        ),
      );

      final decoded = QuickPopupResult.fromJsonString(
        original.toJsonString(),
      );

      expect(decoded.requestId, original.requestId);
      expect(decoded.form.urlText, original.form.urlText);
      expect(decoded.form.saveDir, original.form.saveDir);
      expect(decoded.form.rename, original.form.rename);
      expect(decoded.form.segments, original.form.segments);
      expect(decoded.form.proxyUrl, original.form.proxyUrl);
      expect(decoded.form.userAgent, original.form.userAgent);
      expect(decoded.form.queueId, original.form.queueId);
      expect(
        decoded.form.threadsUserModified,
        original.form.threadsUserModified,
      );
      expect(decoded.form.cookies, original.form.cookies);
      expect(decoded.form.checksum, original.form.checksum);
      expect(decoded.form.ignoreTlsErrors, isTrue);
    });

    test('边界：segments=0（自动）、threadsUserModified=false、全部字符串字段为空', () {
      const original = QuickPopupResult(
        requestId: 0,
        form: QuickDownloadFormResult(
          urlText: '',
          saveDir: '',
          rename: '',
          segments: 0,
          proxyUrl: '',
          userAgent: '',
          queueId: '',
          cookies: '',
          checksum: '',
          threadsUserModified: false,
        ),
      );

      final decoded = QuickPopupResult.fromJsonString(
        original.toJsonString(),
      );

      expect(decoded.requestId, 0);
      expect(decoded.form.urlText, '');
      expect(decoded.form.saveDir, '');
      expect(decoded.form.rename, '');
      expect(decoded.form.segments, 0);
      expect(decoded.form.proxyUrl, '');
      expect(decoded.form.userAgent, '');
      expect(decoded.form.queueId, '');
      expect(decoded.form.ignoreTlsErrors, isFalse);
      expect(decoded.form.threadsUserModified, isFalse);
    });

    test('容错：手工构造最小 JSON，缺失 rename/segments/threadsUserModified 等可选字段不抛异常', () {
      const minimalJson =
          '{"requestId":9,"urlText":"https://example.com/y"}';

      late QuickPopupResult decoded;
      expect(
        () => decoded = QuickPopupResult.fromJsonString(minimalJson),
        returnsNormally,
      );

      expect(decoded.requestId, 9);
      expect(decoded.form.urlText, 'https://example.com/y');
      expect(decoded.form.saveDir, '');
      expect(decoded.form.rename, '');
      expect(decoded.form.segments, 0);
      expect(decoded.form.proxyUrl, '');
      expect(decoded.form.userAgent, '');
      expect(decoded.form.queueId, '');
      expect(decoded.form.ignoreTlsErrors, isFalse);
      expect(decoded.form.threadsUserModified, isFalse);
    });

    test('roundtrip：extraHeaders 保留全部自定义请求头键值', () {
      const original = QuickPopupResult(
        requestId: 5,
        form: QuickDownloadFormResult(
          urlText: 'https://example.com/z',
          saveDir: '',
          rename: '',
          segments: 0,
          proxyUrl: '',
          userAgent: '',
          queueId: '',
          cookies: '',
          checksum: '',
          threadsUserModified: false,
          extraHeaders: {
            'Authorization': 'Bearer token-123',
            'X-Custom-中文键': '中文值',
          },
        ),
      );

      final decoded = QuickPopupResult.fromJsonString(
        original.toJsonString(),
      );

      expect(decoded.form.extraHeaders, {
        'Authorization': 'Bearer token-123',
        'X-Custom-中文键': '中文值',
      });
    });

    test('容错：手工构造 JSON 缺失 extraHeaders 字段时回退空 map', () {
      const minimalJson =
          '{"requestId":9,"urlText":"https://example.com/y"}';

      final decoded = QuickPopupResult.fromJsonString(minimalJson);

      expect(decoded.form.extraHeaders, isEmpty);
    });
  });

  // 清单预解析 relay 信封（弹窗 ↔ 主引擎 经原生 relay/onRelay 透传）。
  group('PopupRelayMessage', () {
    test('previewRequest roundtrip：表单快照全字段还原', () {
      const form = QuickDownloadFormResult(
        urlText: 'https://example.com/合集页面?id=1',
        saveDir: r'C:\Downloads\影视',
        rename: '',
        segments: 16,
        proxyUrl: 'socks5://127.0.0.1:1080',
        userAgent: 'TestUA/1.0',
        queueId: 'queue-x',
        cookies: 'sid=abc; token=秘',
        checksum: '',
        ignoreTlsErrors: true,
        threadsUserModified: true,
        startLater: false,
        extraHeaders: {'X-Auth': 'b', 'Referer': 'https://example.com'},
      );
      final wire = encodePreviewRequest(
        requestId: 7,
        seq: 3,
        form: form,
      ).toJsonString();
      final msg = PopupRelayMessage.fromJsonString(wire);
      expect(msg.kind, kPopupRelayPreviewRequest);
      expect(msg.requestId, 7);
      expect(msg.seq, 3);
      final decoded = decodePreviewRequestForm(msg);
      expect(decoded.urlText, form.urlText);
      expect(decoded.saveDir, form.saveDir);
      expect(decoded.segments, form.segments);
      expect(decoded.proxyUrl, form.proxyUrl);
      expect(decoded.userAgent, form.userAgent);
      expect(decoded.queueId, form.queueId);
      expect(decoded.cookies, form.cookies);
      expect(decoded.ignoreTlsErrors, form.ignoreTlsErrors);
      expect(decoded.threadsUserModified, form.threadsUserModified);
      expect(decoded.extraHeaders, form.extraHeaders);
    });

    test('previewResult roundtrip：清单 items/variants 语义不变', () {
      const manifest = ResolvePreviewResult(
        previewId: 'pv_1',
        name: '沙丘·第二季 4K',
        sourceUrl: 'https://manifest.test/demo',
        error: '',
        items: [
          ManifestItemDto(
            id: 'ep01',
            name: 'E01.mkv',
            path: '正片',
            size: 6442450944,
            variants: [
              ManifestVariantDto(id: '4k', label: '2160p', size: 6442450944),
              ManifestVariantDto(id: '1080', label: '1080p', size: 0),
            ],
          ),
          ManifestItemDto(
            id: 'nfo',
            name: '沙丘S02.nfo',
            path: '',
            size: 0,
            variants: [],
          ),
        ],
      );
      final wire = encodePreviewResult(
        requestId: 7,
        seq: 3,
        manifest: manifest,
      ).toJsonString();
      final msg = PopupRelayMessage.fromJsonString(wire);
      expect(msg.kind, kPopupRelayPreviewResult);
      final decoded = decodePreviewResultManifest(msg);
      expect(decoded, isNotNull);
      expect(decoded!.name, manifest.name);
      expect(decoded.sourceUrl, manifest.sourceUrl);
      expect(decoded.items.length, 2);
      expect(decoded.items[0].id, 'ep01');
      expect(decoded.items[0].path, '正片');
      expect(decoded.items[0].size, 6442450944);
      expect(decoded.items[0].variants.length, 2);
      expect(decoded.items[0].variants[1].size, 0);
      expect(decoded.items[1].variants, isEmpty);
    });

    test('previewResult：manifest=null 编解码后仍为 null（无清单回退语义）', () {
      final wire = encodePreviewResult(
        requestId: 1,
        seq: 1,
        manifest: null,
      ).toJsonString();
      final msg = PopupRelayMessage.fromJsonString(wire);
      expect(decodePreviewResultManifest(msg), isNull);
    });

    test('groupSubmit roundtrip：建组投影全字段；referrer 恒由主引擎回填（解码为空）', () {
      const sub = ManifestGroupSubmission(
        sourceUrl: 'https://manifest.test/demo',
        groupName: '沙丘·第二季',
        saveDir: r'C:\Downloads',
        queueId: 'later',
        segments: 8,
        cookies: 'sid=abc',
        referrer: 'https://should-not-survive.example',
        userAgent: 'UA/2',
        proxyUrl: '',
        extraHeaders: {'X-K': 'v'},
        ignoreTlsErrors: false,
        startPaused: true,
        items: [
          GroupItemEntry(
            resolverItem: 'ep01@4k',
            fileName: 'E01.mkv',
            relPath: '正片',
            size: 6442450944,
          ),
        ],
      );
      final wire = encodeGroupSubmit(
        requestId: 9,
        seq: 2,
        sub: sub,
      ).toJsonString();
      final msg = PopupRelayMessage.fromJsonString(wire);
      expect(msg.kind, kPopupRelayGroupSubmit);
      final decoded = decodeGroupSubmit(msg);
      expect(decoded.sourceUrl, sub.sourceUrl);
      expect(decoded.groupName, sub.groupName);
      expect(decoded.saveDir, sub.saveDir);
      expect(decoded.queueId, sub.queueId);
      expect(decoded.segments, sub.segments);
      expect(decoded.cookies, sub.cookies);
      // referrer 保存在主引擎 pending 表，不经弹窗流转：解码侧恒为空。
      expect(decoded.referrer, '');
      expect(decoded.userAgent, sub.userAgent);
      expect(decoded.extraHeaders, sub.extraHeaders);
      expect(decoded.startPaused, isTrue);
      expect(decoded.items.length, 1);
      expect(decoded.items[0].resolverItem, 'ep01@4k');
      expect(decoded.items[0].relPath, '正片');
      expect(decoded.items[0].size, 6442450944);
    });

    test('previewCancel：kind/requestId/seq 原样往返，data 为空容错', () {
      final wire = encodePreviewCancel(requestId: 4, seq: 6).toJsonString();
      final msg = PopupRelayMessage.fromJsonString(wire);
      expect(msg.kind, kPopupRelayPreviewCancel);
      expect(msg.requestId, 4);
      expect(msg.seq, 6);
      expect(msg.data, isEmpty);
    });
  });
}
