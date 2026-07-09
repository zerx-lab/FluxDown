import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'log_service.dart';

const _tag = 'Feedback';

/// 应用版本（构建时注入，同 update_service）。作为 source=app 反馈的 appVersion 上报。
const _appVersion = String.fromEnvironment('APP_VERSION', defaultValue: 'dev');

/// 反馈类型，对应 website API 的 type 字段。
enum FeedbackType {
  feature,
  bug,
  other;

  String get value => name;
}

/// 提交反馈的结果。
class FeedbackResult {
  final bool success;
  final String? message;
  final int? issueNumber;

  const FeedbackResult({required this.success, this.message, this.issueNumber});
}

/// 调用 FluxDown website API 提交用户反馈。
///
/// API 端点: POST https://fluxdown.zerx.dev/api/feedback
/// 服务端持有 GITHUB_TOKEN，客户端无需暴露凭据。
class FeedbackService {
  FeedbackService._();
  static final instance = FeedbackService._();

  /// 构建时注入的应用版本号（供反馈表单展示，提交时自动上报）。
  static const String appVersion = _appVersion;

  static const _apiBase = 'https://fluxdown.zerx.dev';
  static const _feedbackPath = '/api/feedback';
  static const _timeout = Duration(seconds: 15);

  HttpClient? _httpClient;

  void _ensureHttpClient() {
    _httpClient ??= HttpClient()
      ..connectionTimeout = const Duration(seconds: 10)
      ..idleTimeout = const Duration(seconds: 15);
  }

  /// 提交反馈到 website API。
  ///
  /// [type] 反馈类型：feature / bug / other
  /// [title] 标题（最长 200 字符）
  /// [description] 详细描述（最长 5000 字符）
  /// [contact] 可选的联系方式
  /// [logs] 当天日志文本，非空时作为独立 logs 字段提交（服务端折叠展示）
  Future<FeedbackResult> submit({
    required FeedbackType type,
    required String title,
    required String description,
    String? contact,
    String? logs,
  }) async {
    _ensureHttpClient();

    final body = <String, dynamic>{
      'type': type.value,
      'title': title,
      'description': description,
      // 标记来源为桌面应用，服务端据此打 App 标签并套用应用反馈 body 模板。
      'source': 'app',
      'appVersion': _appVersion,
    };
    if (contact != null && contact.trim().isNotEmpty) {
      body['contact'] = contact.trim();
    }
    if (logs != null && logs.trim().isNotEmpty) {
      body['logs'] = logs.trim();
    }

    final jsonBody = utf8.encode(json.encode(body));

    try {
      final uri = Uri.parse('$_apiBase$_feedbackPath');
      final request = await _httpClient!.postUrl(uri).timeout(_timeout);
      request.headers.set('Content-Type', 'application/json; charset=utf-8');
      request.headers.set('Accept', 'application/json');
      request.contentLength = jsonBody.length;
      request.add(jsonBody);

      final response = await request.close().timeout(_timeout);
      final responseBody = await response.transform(utf8.decoder).join();

      if (response.statusCode == 201) {
        final data = json.decode(responseBody) as Map<String, dynamic>;
        logInfo(_tag, 'Feedback submitted, issue #${data['issueNumber']}');
        return FeedbackResult(
          success: true,
          message: data['message'] as String?,
          issueNumber: data['issueNumber'] as int?,
        );
      }

      if (response.statusCode == 429) {
        logInfo(_tag, 'Rate limited');
        return const FeedbackResult(success: false, message: 'rate_limited');
      }

      // 其他错误
      String errorMsg = 'HTTP ${response.statusCode}';
      try {
        final data = json.decode(responseBody) as Map<String, dynamic>;
        errorMsg = (data['error'] as String?) ?? errorMsg;
      } catch (_) {
        // 解析失败使用默认错误消息
      }
      logError(_tag, 'Submit failed: $errorMsg');
      return FeedbackResult(success: false, message: errorMsg);
    } on TimeoutException {
      logError(_tag, 'Submit timeout');
      return const FeedbackResult(success: false, message: 'timeout');
    } catch (e) {
      logError(_tag, 'Submit error: $e');
      return FeedbackResult(success: false, message: e.toString());
    }
  }

  /// 释放 HTTP 客户端资源。
  void dispose() {
    _httpClient?.close(force: true);
    _httpClient = null;
  }
}
