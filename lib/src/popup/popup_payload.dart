/// 独立快速下载小窗的载荷/结果编解码。
///
/// 主引擎与弹窗引擎运行同一 Dart bundle，两侧共用本文件的
/// 序列化代码，保证 JSON schema 天然一致（契约见 popup-contract）。
library;

import 'dart:convert';

import '../bindings/bindings.dart';
import '../widgets/manifest_select_view.dart' show ManifestGroupSubmission;
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

  /// 设备名册（云账户已登录且有远程设备时非空；渐进披露判定源）。
  final List<QuickDeviceOption> devices;

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
    this.devices = const [],
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
      'devices': [
        for (final d in devices)
          {
            'deviceId': d.deviceId,
            'name': d.name,
            'platform': d.platform,
            'isOnline': d.isOnline,
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
      devices: [
        for (final d in (env['devices'] as List? ?? const []))
          QuickDeviceOption(
            deviceId: (d as Map<String, dynamic>)['deviceId'] as String? ?? '',
            name: d['name'] as String? ?? '',
            platform: d['platform'] as String?,
            isOnline: d['isOnline'] as bool? ?? false,
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
    ...quickFormResultToJson(form),
  });

  factory QuickPopupResult.fromJsonString(String json) {
    final map = jsonDecode(json) as Map<String, dynamic>;
    return QuickPopupResult(
      requestId: (map['requestId'] as num).toInt(),
      form: quickFormResultFromJson(map),
    );
  }
}

/// [QuickDownloadFormResult] 的 JSON 投影（[QuickPopupResult] 与 relay
/// previewRequest 共用，schema 天然一致）。
Map<String, dynamic> quickFormResultToJson(QuickDownloadFormResult form) => {
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
  'targetDeviceId': form.targetDeviceId,
};

QuickDownloadFormResult quickFormResultFromJson(Map<String, dynamic> map) =>
    QuickDownloadFormResult(
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
      targetDeviceId: map['targetDeviceId'] as String? ?? '',
      extraHeaders: {
        for (final e
            in (map['extraHeaders'] as Map<String, dynamic>? ?? const {})
                .entries)
          e.key: (e.value as String?) ?? '',
      },
    );

// ─────────────────────────────────────────────────────────────────────────
// relay 信封（原生通用透传：popup_child.relay ↔ popup_host.onRelay 及反向）
//
// 承载小窗清单选择流程的五类消息（kind）：
// - previewRequest（弹窗→主）：表单提交快照，请求主引擎做清单预解析；
// - previewResult（主→弹窗）：预解析结果（null = 无清单，弹窗回退普通提交；
//   注：无清单时主引擎通常直接代提交并关窗，本消息仅在需要弹窗侧回退时发）；
// - previewCancel（弹窗→主）：用户取消等待，主引擎中止预解析；
// - groupSubmit（弹窗→主）：清单视图确认结果，主引擎发 CreateTaskGroup；
// - manifestClosed（弹窗→主）：清单视图退出回表单，主引擎恢复 append
//   合入语义并冲刷托管缓冲的外部请求。
//
// requestId 关联 show 会话（QuickPopupPayload.requestId），seq 区分同一会话
// 内的多次预解析尝试（取消后重新提交），迟到消息按两者共同判弃。
// ─────────────────────────────────────────────────────────────────────────

const String kPopupRelayPreviewRequest = 'previewRequest';
const String kPopupRelayPreviewResult = 'previewResult';
const String kPopupRelayPreviewCancel = 'previewCancel';
const String kPopupRelayGroupSubmit = 'groupSubmit';
const String kPopupRelayManifestClosed = 'manifestClosed';

class PopupRelayMessage {
  final String kind;
  final int requestId;
  final int seq;
  final Map<String, dynamic> data;

  const PopupRelayMessage({
    required this.kind,
    required this.requestId,
    required this.seq,
    this.data = const {},
  });

  String toJsonString() => jsonEncode({
    'kind': kind,
    'requestId': requestId,
    'seq': seq,
    'data': data,
  });

  factory PopupRelayMessage.fromJsonString(String json) {
    final map = jsonDecode(json) as Map<String, dynamic>;
    return PopupRelayMessage(
      kind: map['kind'] as String? ?? '',
      requestId: (map['requestId'] as num?)?.toInt() ?? 0,
      seq: (map['seq'] as num?)?.toInt() ?? 0,
      data: map['data'] as Map<String, dynamic>? ?? const {},
    );
  }
}

/// previewRequest：表单结果快照进 data（主引擎无清单时可直接代提交）。
PopupRelayMessage encodePreviewRequest({
  required int requestId,
  required int seq,
  required QuickDownloadFormResult form,
}) => PopupRelayMessage(
  kind: kPopupRelayPreviewRequest,
  requestId: requestId,
  seq: seq,
  data: {'form': quickFormResultToJson(form)},
);

QuickDownloadFormResult decodePreviewRequestForm(PopupRelayMessage msg) =>
    quickFormResultFromJson(msg.data['form'] as Map<String, dynamic>? ?? const {});

/// previewResult：`manifest == null` = 无清单（弹窗应走普通提交回退）。
PopupRelayMessage encodePreviewResult({
  required int requestId,
  required int seq,
  required ResolvePreviewResult? manifest,
}) => PopupRelayMessage(
  kind: kPopupRelayPreviewResult,
  requestId: requestId,
  seq: seq,
  data: {
    'manifest': manifest == null
        ? null
        : {
            'name': manifest.name,
            'sourceUrl': manifest.sourceUrl,
            'items': [
              for (final it in manifest.items)
                {
                  'id': it.id,
                  'name': it.name,
                  'path': it.path,
                  'size': it.size,
                  'variants': [
                    for (final v in it.variants)
                      {'id': v.id, 'label': v.label, 'size': v.size},
                  ],
                },
            ],
          },
  },
);

/// 弹窗侧还原清单（bindings 数据类纯构造，不触 Rust FFI）。
ResolvePreviewResult? decodePreviewResultManifest(PopupRelayMessage msg) {
  final m = msg.data['manifest'] as Map<String, dynamic>?;
  if (m == null) return null;
  return ResolvePreviewResult(
    previewId: '',
    name: m['name'] as String? ?? '',
    sourceUrl: m['sourceUrl'] as String? ?? '',
    error: '',
    items: [
      for (final it in (m['items'] as List? ?? const []))
        ManifestItemDto(
          id: (it as Map<String, dynamic>)['id'] as String? ?? '',
          name: it['name'] as String? ?? '',
          path: it['path'] as String? ?? '',
          size: (it['size'] as num?)?.toInt() ?? 0,
          variants: [
            for (final v in (it['variants'] as List? ?? const []))
              ManifestVariantDto(
                id: (v as Map<String, dynamic>)['id'] as String? ?? '',
                label: v['label'] as String? ?? '',
                size: (v['size'] as num?)?.toInt() ?? 0,
              ),
          ],
        ),
    ],
  );
}

PopupRelayMessage encodePreviewCancel({
  required int requestId,
  required int seq,
}) => PopupRelayMessage(
  kind: kPopupRelayPreviewCancel,
  requestId: requestId,
  seq: seq,
);

/// manifestClosed：弹窗侧退出清单视图回到表单（Esc/取消/标题栏 X）。
/// 主引擎据此恢复「append 合入表单」语义并冲刷托管缓冲的外部请求。
PopupRelayMessage encodeManifestClosed({
  required int requestId,
  required int seq,
}) => PopupRelayMessage(
  kind: kPopupRelayManifestClosed,
  requestId: requestId,
  seq: seq,
);

/// groupSubmit：清单视图确认的建组投影（referrer 由主引擎按 pending
/// 请求上下文回填，弹窗侧恒为 ''）。
PopupRelayMessage encodeGroupSubmit({
  required int requestId,
  required int seq,
  required ManifestGroupSubmission sub,
}) => PopupRelayMessage(
  kind: kPopupRelayGroupSubmit,
  requestId: requestId,
  seq: seq,
  data: {
    'sourceUrl': sub.sourceUrl,
    'groupName': sub.groupName,
    'saveDir': sub.saveDir,
    'queueId': sub.queueId,
    'segments': sub.segments,
    'cookies': sub.cookies,
    'userAgent': sub.userAgent,
    'proxyUrl': sub.proxyUrl,
    'extraHeaders': sub.extraHeaders,
    'ignoreTlsErrors': sub.ignoreTlsErrors,
    'startPaused': sub.startPaused,
    'items': [
      for (final it in sub.items)
        {
          'resolverItem': it.resolverItem,
          'fileName': it.fileName,
          'relPath': it.relPath,
          'size': it.size,
        },
    ],
  },
);

ManifestGroupSubmission decodeGroupSubmit(PopupRelayMessage msg) {
  final d = msg.data;
  return ManifestGroupSubmission(
    sourceUrl: d['sourceUrl'] as String? ?? '',
    groupName: d['groupName'] as String? ?? '',
    saveDir: d['saveDir'] as String? ?? '',
    queueId: d['queueId'] as String? ?? '',
    segments: (d['segments'] as num?)?.toInt() ?? 0,
    cookies: d['cookies'] as String? ?? '',
    referrer: '',
    userAgent: d['userAgent'] as String? ?? '',
    proxyUrl: d['proxyUrl'] as String? ?? '',
    extraHeaders: {
      for (final e
          in (d['extraHeaders'] as Map<String, dynamic>? ?? const {}).entries)
        e.key: (e.value as String?) ?? '',
    },
    ignoreTlsErrors: d['ignoreTlsErrors'] as bool? ?? false,
    startPaused: d['startPaused'] as bool? ?? false,
    items: [
      for (final it in (d['items'] as List? ?? const []))
        GroupItemEntry(
          resolverItem:
              (it as Map<String, dynamic>)['resolverItem'] as String? ?? '',
          fileName: it['fileName'] as String? ?? '',
          relPath: it['relPath'] as String? ?? '',
          size: (it['size'] as num?)?.toInt() ?? 0,
        ),
    ],
  );
}
