import 'dart:io';

import 'package:flutter/services.dart';

import '../../services/log_service.dart';

const _tag = 'ShareIntent';

/// 系统分享 / URL scheme 接入桥（对应 Android [MainActivity] 的
/// `com.fluxdown/share` channel 与 iOS [AppDelegate] 的同名 channel）。
///
/// 两端约定：
/// - 原生侧 invoke `onShare`（热启动，应用已在前台/后台收到新分享 intent）；
/// - Dart 侧 invoke `getInitialShare`（冷启动，首帧就绪后主动拉取暂存内容，
///   取走即清空，避免重复触发）。
///
/// 载荷双形态兼容：Android 传 `{url, filename, userAgent, cookie, referer}`
/// Map（filename 仅 fluxdown:// 协议携带，其余为空串；userAgent/cookie/referer
/// 在 X 浏览器 ACTION_VIEW 直链路径附上，缺省为空串）；iOS 仍传纯 String
/// （无协议模式，三字段恒为空）。
///
/// 分享内容可能夹带描述文字（如“看看这个 https://x/f.zip”），[extractUrl]
/// 从中提取首个可下载的 URL / magnet。
class ShareIntentService {
  ShareIntentService._();

  static const _channel = MethodChannel('com.fluxdown/share');

  /// 当前平台是否支持系统分享接入
  static bool get supported => Platform.isAndroid || Platform.isIOS;

  /// 回调形参：url / 建议文件名（仅 fluxdown:// 携带，其余空串）/ UA /
  /// Cookie / Referer（X 浏览器 ACTION_VIEW 直链附上，缺省空串）。
  static void Function(
    String url,
    String filename,
    String userAgent,
    String cookie,
    String referer,
  )?
      _onShared;

  /// 注册分享回调，并立即拉取冷启动时暂存的分享内容。
  ///
  /// [onShared] 收到的是已提取的 URL / magnet 与可选的建议文件名，以及
  /// X 浏览器可能附带的 User-Agent / Cookie / Referer（空串 = 未提供）；
  /// 提取失败则不回调。
  static Future<void> init(
    void Function(
      String url,
      String filename,
      String userAgent,
      String cookie,
      String referer,
    )
        onShared,
  ) async {
    if (!supported) return;
    _onShared = onShared;
    _channel.setMethodCallHandler(_handle);
    try {
      final initial = await _channel.invokeMethod<Object>('getInitialShare');
      _dispatch(initial);
    } catch (e, st) {
      logError(_tag, 'getInitialShare failed', e, st);
    }
  }

  static void shutdown() {
    _onShared = null;
    if (supported) _channel.setMethodCallHandler(null);
  }

  static Future<void> _handle(MethodCall call) async {
    if (call.method == 'onShare') {
      _dispatch(call.arguments);
    }
  }

  static void _dispatch(Object? raw) {
    final String? text;
    var filename = '';
    var userAgent = '';
    var cookie = '';
    var referer = '';
    if (raw is Map) {
      text = raw['url'] as String?;
      filename = (raw['filename'] as String?)?.trim() ?? '';
      userAgent = (raw['userAgent'] as String?)?.trim() ?? '';
      cookie = (raw['cookie'] as String?)?.trim() ?? '';
      referer = (raw['referer'] as String?)?.trim() ?? '';
    } else {
      text = raw as String?;
    }
    final url = extractUrl(text);
    if (url == null) {
      if (text != null && text.isNotEmpty) {
        logInfo(_tag, 'shared text has no usable url');
      }
      return;
    }
    logInfo(_tag, 'shared url received');
    _onShared?.call(url, filename, userAgent, cookie, referer);
  }

  /// 从分享文本中提取首个可下载链接。
  ///
  /// 优先匹配 magnet，其次 http(s)/ftp 直链；整串本身即为链接时直接返回。
  /// 无匹配返回 `null`。
  static String? extractUrl(String? raw) {
    if (raw == null) return null;
    final text = raw.trim();
    if (text.isEmpty) return null;
    final match = _urlPattern.firstMatch(text);
    return match?.group(0);
  }

  static final RegExp _urlPattern = RegExp(
    r'(magnet:\?[^\s]+|(?:https?|ftp)://[^\s]+)',
    caseSensitive: false,
  );
}
