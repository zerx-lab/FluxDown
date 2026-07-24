/// 独立快速下载小窗的载荷/结果编解码。
///
/// 主引擎与弹窗引擎运行同一 Dart bundle，两侧共用本文件的
/// 序列化代码，保证 JSON schema 天然一致（契约见 popup-contract）。
library;

import 'dart:convert';

import '../widgets/quick_download_form.dart';

/// 主引擎 → 弹窗引擎的表单载荷。
///
/// cookies/referrer/fileSize 等请求上下文保留在主引擎的 pending 表中，
/// 不进弹窗（弹窗提交后由主引擎按 requestId 关联回填）。
class QuickPopupPayload {
  /// 请求序号 — 主引擎生成，结果回传时用于关联校验
  final int requestId;

  /// 初始 URL（可能多行）
  final String url;

  /// 已知文件名（'' = 未知）
  final String filename;

  /// 提示文件大小（0 = 未知，仅标题栏展示用）
  final int fileSize;

  /// MIME 类型（仅标题栏展示用）
  final String mimeType;

  /// 已按"请求方指定 / 分类规则 / 默认目录"预解析的保存目录
  final String saveDir;

  /// 浏览器扩展捕获的 Cookie（预填高级选项供编辑覆盖）
  final String cookies;

  /// 界面语言 code（'zh' / 'en'）
  final String locale;

  /// 主引擎当前生效的主题令牌（FluxThemeTokens.toJson()）
  final Map<String, dynamic> tokensJson;

  /// 全局默认线程数（0 = 自动）
  final int defaultSegments;

  /// 上次新建下载选择的线程数（'' / 'auto' / 数字串）
  final String lastDialogThreads;

  /// 默认队列 ID
  final String defaultQueueId;

  /// 命名队列列表
  final List<QuickQueueOption> queues;

  const QuickPopupPayload({
    required this.requestId,
    required this.url,
    required this.filename,
    required this.fileSize,
    required this.mimeType,
    required this.saveDir,
    required this.cookies,
    required this.locale,
    required this.tokensJson,
    required this.defaultSegments,
    required this.lastDialogThreads,
    required this.defaultQueueId,
    required this.queues,
  });

  String toJsonString() => jsonEncode({
    'requestId': requestId,
    'req': {
      'url': url,
      'filename': filename,
      'fileSize': fileSize,
      'mimeType': mimeType,
      'saveDir': saveDir,
      'cookies': cookies,
    },
    'env': {
      'locale': locale,
      'tokens': tokensJson,
      'defaultSegments': defaultSegments,
      'lastDialogThreads': lastDialogThreads,
      'defaultQueueId': defaultQueueId,
      'queues': [
        for (final q in queues)
          {
            'id': q.queueId,
            'name': q.name,
            'defaultSegments': q.defaultSegments,
          },
      ],
    },
  });

  factory QuickPopupPayload.fromJsonString(String json) {
    final map = jsonDecode(json) as Map<String, dynamic>;
    final req = map['req'] as Map<String, dynamic>;
    final env = map['env'] as Map<String, dynamic>;
    return QuickPopupPayload(
      requestId: (map['requestId'] as num).toInt(),
      url: req['url'] as String? ?? '',
      filename: req['filename'] as String? ?? '',
      fileSize: (req['fileSize'] as num?)?.toInt() ?? 0,
      mimeType: req['mimeType'] as String? ?? '',
      saveDir: req['saveDir'] as String? ?? '',
      cookies: req['cookies'] as String? ?? '',
      locale: env['locale'] as String? ?? 'en',
      tokensJson: env['tokens'] as Map<String, dynamic>,
      defaultSegments: (env['defaultSegments'] as num?)?.toInt() ?? 0,
      lastDialogThreads: env['lastDialogThreads'] as String? ?? '',
      defaultQueueId: env['defaultQueueId'] as String? ?? '',
      queues: [
        for (final q in (env['queues'] as List? ?? const []))
          QuickQueueOption(
            queueId: (q as Map<String, dynamic>)['id'] as String? ?? '',
            name: q['name'] as String? ?? '',
            defaultSegments: (q['defaultSegments'] as num?)?.toInt() ?? 0,
          ),
      ],
    );
  }
}

/// 弹窗引擎 → 主引擎的提交结果。
class QuickPopupResult {
  final int requestId;
  final QuickDownloadFormResult form;

  const QuickPopupResult({required this.requestId, required this.form});

  String toJsonString() => jsonEncode({
    'requestId': requestId,
    'urlText': form.urlText,
    'saveDir': form.saveDir,
    'rename': form.rename,
    'segments': form.segments,
    'proxyUrl': form.proxyUrl,
    'userAgent': form.userAgent,
    'queueId': form.queueId,
    'cookies': form.cookies,
    'checksum': form.checksum,
    'ignoreTlsErrors': form.ignoreTlsErrors,
    'threadsUserModified': form.threadsUserModified,
    'extraHeaders': form.extraHeaders,
    'startLater': form.startLater,
  });

  factory QuickPopupResult.fromJsonString(String json) {
    final map = jsonDecode(json) as Map<String, dynamic>;
    return QuickPopupResult(
      requestId: (map['requestId'] as num).toInt(),
      form: QuickDownloadFormResult(
        urlText: map['urlText'] as String? ?? '',
        saveDir: map['saveDir'] as String? ?? '',
        rename: map['rename'] as String? ?? '',
        segments: (map['segments'] as num?)?.toInt() ?? 0,
        proxyUrl: map['proxyUrl'] as String? ?? '',
        userAgent: map['userAgent'] as String? ?? '',
        queueId: map['queueId'] as String? ?? '',
        cookies: map['cookies'] as String? ?? '',
        checksum: map['checksum'] as String? ?? '',
        ignoreTlsErrors: map['ignoreTlsErrors'] as bool? ?? false,
        threadsUserModified: map['threadsUserModified'] as bool? ?? false,
        startLater: map['startLater'] as bool? ?? false,
        extraHeaders: {
          for (final e
              in (map['extraHeaders'] as Map<String, dynamic>? ?? const {})
                  .entries)
            e.key: (e.value as String?) ?? '',
        },
      ),
    );
  }
}
