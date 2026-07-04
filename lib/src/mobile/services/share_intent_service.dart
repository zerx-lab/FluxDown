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
/// 分享内容可能夹带描述文字（如“看看这个 https://x/f.zip”），[extractUrl]
/// 从中提取首个可下载的 URL / magnet。
class ShareIntentService {
  ShareIntentService._();

  static const _channel = MethodChannel('com.fluxdown/share');

  /// 当前平台是否支持系统分享接入
  static bool get supported => Platform.isAndroid || Platform.isIOS;

  static void Function(String url)? _onShared;

  /// 注册分享回调，并立即拉取冷启动时暂存的分享内容。
  ///
  /// [onShared] 收到的是已提取的 URL / magnet；提取失败则不回调。
  static Future<void> init(void Function(String url) onShared) async {
    if (!supported) return;
    _onShared = onShared;
    _channel.setMethodCallHandler(_handle);
    try {
      final initial = await _channel.invokeMethod<String>('getInitialShare');
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
      _dispatch(call.arguments as String?);
    }
  }

  static void _dispatch(String? raw) {
    final url = extractUrl(raw);
    if (url == null) {
      if (raw != null && raw.isNotEmpty) {
        logInfo(_tag, 'shared text has no usable url');
      }
      return;
    }
    logInfo(_tag, 'shared url received');
    _onShared?.call(url);
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
