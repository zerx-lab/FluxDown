/// 清单预解析共享客户端（主 isolate）。
///
/// 「提交单条 http(s) 链接前先探测是否为多文件清单」的等待/超时/取消
/// 逻辑的唯一实现，三个入口共用：
/// - 新建下载对话框（new_download_dialog.dart）
/// - 快速下载回退对话框（quick_download_dialog.dart）
/// - 独立快速下载小窗（popup_window_service.dart，经原生通道中继）
///
/// 语义（与最初 new_download_dialog 内联实现逐条一致）：
/// - 发 [ResolvePreviewRequest]，等待同 previewId 的 [ResolvePreviewResult]；
/// - 90s 超时视同无清单（future 完成 null）；
/// - `error` 非空或 `items` 为空 → 无清单（null）；
/// - [ResolvePreviewHandle.cancel] 后 future 立即完成 null 且
///   [ResolvePreviewHandle.cancelled] 为 true，迟到结果按 previewId 丢弃。
library;

import 'dart:async';

import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';

/// 单条 URL 是否可能是多文件清单（值得预解析探测）。
///
/// 与 new_download_dialog 的原 `_isPreviewableUrl` 同义：只有 http(s)
/// 链接可能命中插件 resolver 清单；磁力/ed2k/种子路径由各自分支提前处理。
bool isManifestPreviewableUrl(String url) {
  final lower = url.toLowerCase();
  return lower.startsWith('http://') || lower.startsWith('https://');
}

/// 一次在途的预解析等待。由 [ResolvePreviewClient.start] 创建。
class ResolvePreviewHandle {
  final String previewId;
  final Completer<ResolvePreviewResult?> _completer =
      Completer<ResolvePreviewResult?>();
  Timer? _timeout;
  bool _cancelled = false;

  ResolvePreviewHandle._(this.previewId);

  /// 完成于：结果到达 / 超时（null）/ 取消（null）。
  Future<ResolvePreviewResult?> get future => _completer.future;

  /// 用户主动取消了等待（调用方据此区分「取消提交」与「无清单回退」）。
  bool get cancelled => _cancelled;

  /// 取消等待：future 立即完成 null，迟到结果被丢弃。幂等。
  void cancel() {
    if (_completer.isCompleted) {
      _cancelled = true;
      return;
    }
    _cancelled = true;
    ResolvePreviewClient._finish(previewId, null);
  }

  void _complete(ResolvePreviewResult? result) {
    _timeout?.cancel();
    _timeout = null;
    if (!_completer.isCompleted) _completer.complete(result);
  }
}

/// 预解析客户端：单一 [ResolvePreviewResult] 订阅 + previewId 分发表。
class ResolvePreviewClient {
  ResolvePreviewClient._();

  static const Duration _kTimeout = Duration(seconds: 90);

  static final Map<String, ResolvePreviewHandle> _pending = {};
  static StreamSubscription<RustSignalPack<ResolvePreviewResult>>? _sub;
  static int _seq = 0;

  /// 发起一次预解析。[referrer] 为外部请求上下文（浏览器扩展捕获），
  /// 应用内入口传 ''。
  static ResolvePreviewHandle start({
    required String url,
    required String cookies,
    required String referrer,
    required String userAgent,
    required Map<String, String> extraHeaders,
  }) {
    _sub ??= ResolvePreviewResult.rustSignalStream.listen(_onResult);
    final previewId =
        'pv_${++_seq}_${DateTime.now().millisecondsSinceEpoch}';
    final handle = ResolvePreviewHandle._(previewId);
    _pending[previewId] = handle;
    ResolvePreviewRequest(
      previewId: previewId,
      url: url,
      cookies: cookies,
      referrer: referrer,
      userAgent: userAgent,
      extraHeaders: extraHeaders,
    ).sendSignalToRust();
    handle._timeout = Timer(_kTimeout, () => _finish(previewId, null));
    return handle;
  }

  static void _onResult(RustSignalPack<ResolvePreviewResult> pack) {
    final msg = pack.message;
    // 无清单/失败统一折叠为 null（回退语义），有清单原样交付。
    final manifest =
        (msg.error.isNotEmpty || msg.items.isEmpty) ? null : msg;
    _finish(msg.previewId, manifest);
  }

  static void _finish(String previewId, ResolvePreviewResult? result) {
    final handle = _pending.remove(previewId);
    handle?._complete(result);
  }
}
