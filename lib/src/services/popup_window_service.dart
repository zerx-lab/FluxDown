/// 外部唤起独立快速下载小窗 — 主引擎侧服务。
///
/// 经 `fluxdown/popup_host` 通道请求原生宿主显示独立小窗
/// （原生窗口承载第二个 Flutter 引擎，见 popup-contract）：
/// - `show(payloadJson)`：投递表单载荷并显示小窗（置顶、不占任务栏、
///   不抢主窗口前台 — 这正是独立小窗对主窗口内对话框的核心优势）；
/// - `onResult(resultJson)`：用户确认，回填 pending 请求上下文
///   （referrer/fileSize）后经 [submitQuickDownload] 发送下载信号
///   （cookies 已随载荷进表单，由用户编辑后随结果带回）；
/// - `onClosed()`：用户取消/关闭。
///
/// 原生宿主未实现（MissingPluginException）或显示失败时返回 false，
/// 由调用方回退到主窗口内对话框流程。
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
import '../popup/popup_payload.dart';
import '../theme/theme_provider.dart';
import '../widgets/quick_download_form.dart';
import 'log_service.dart';
import 'quick_download_submitter.dart';

const _tag = 'PopupWinSvc';

/// 弹窗可见期间保存的请求上下文 — 结果回传时按 requestId 关联回填。
class _PendingRequest {
  final int requestId;
  final String referrer;
  final int fileSize;
  final String audioUrl;

  const _PendingRequest({
    required this.requestId,
    required this.referrer,
    required this.fileSize,
    this.audioUrl = '',
  });
}

/// 独立小窗服务单例。
class PopupWindowService {
  PopupWindowService._();

  static final PopupWindowService instance = PopupWindowService._();

  static const _channel = MethodChannel('fluxdown/popup_host');

  ThemeProvider? _themeProvider;
  GlobalKey<NavigatorState>? _navigatorKey;
  bool _handlerInstalled = false;

  /// 小窗当前是否可见（与主窗口对话框的 _dialogOpen 同语义，用于去重）
  bool _visible = false;

  /// 请求序号发生器
  int _seq = 0;

  _PendingRequest? _pending;

  /// pending 时效 watchdog — 弹窗引擎异常（渲染卡死/通道断开）时
  /// onResult/onClosed 永不到达，[_visible] 会永久卡 true 导致后续外部
  /// 请求全部被丢弃。超时后主动 close() 复位。append 路径的原生真值
  /// 校验（见 [tryAppend]）是第一道自愈，本定时器是最后兜底。
  Timer? _watchdog;
  static const _watchdogTimeout = Duration(minutes: 15);

  bool get isVisible => _visible;

  /// 在 app 启动时调用一次（主题与 navigator 用于组装载荷）。
  void init({
    required ThemeProvider themeProvider,
    required GlobalKey<NavigatorState> navigatorKey,
  }) {
    _themeProvider = themeProvider;
    _navigatorKey = navigatorKey;
    if (!_handlerInstalled) {
      _channel.setMethodCallHandler(_onCall);
      _handlerInstalled = true;
    }
  }

  /// 请求原生宿主显示独立小窗。返回 false 表示需回退到主窗口内对话框。
  ///
  /// [resolvedSaveDir] 由调用方按"请求方指定 / 分类规则 / 默认目录"预解析。
  Future<bool> tryShow({
    required ExternalDownloadRequest req,
    required String resolvedSaveDir,
  }) async {
    if (!Platform.isWindows && !Platform.isMacOS && !Platform.isLinux) {
      return false;
    }
    final themeProvider = _themeProvider;
    final settings = SettingsProvider.globalInstance;
    final context = _navigatorKey?.currentContext;
    if (themeProvider == null || settings == null || context == null) {
      logError(_tag, 'not initialized, cannot show popup');
      return false;
    }

    final queues = DownloadController.globalInstance?.queues ?? const [];
    final payload = QuickPopupPayload(
      requestId: ++_seq,
      url: req.url,
      filename: req.filename,
      fileSize: req.fileSize,
      mimeType: req.mimeType,
      saveDir: resolvedSaveDir,
      cookies: req.cookies,
      locale: currentLocale,
      tokensJson: themeProvider.activeTokens(context).toJson(),
      defaultSegments: settings.defaultSegments,
      lastDialogThreads: settings.lastDialogThreads,
      defaultQueueId: settings.defaultQueueId,
      queues: [
        for (final q in queues)
          QuickQueueOption(
            queueId: q.queueId,
            name: q.name,
            defaultSegments: q.defaultSegments,
          ),
      ],
    );

    try {
      final shown =
          await _channel.invokeMethod<bool>('show', payload.toJsonString()) ??
          false;
      if (shown) {
        _visible = true;
        _pending = _PendingRequest(
          requestId: payload.requestId,
          referrer: req.referrer,
          fileSize: req.fileSize,
          audioUrl: req.audioUrl,
        );
        _armWatchdog();
        logInfo(_tag, 'popup shown, requestId=${payload.requestId}');
      } else {
        logError(_tag, 'native host refused to show popup');
      }
      return shown;
    } on MissingPluginException {
      // 原生宿主未实现（该平台尚无 popup host）— 静默回退
      logInfo(_tag, 'popup host not implemented on this platform');
      return false;
    } on PlatformException catch (e) {
      logError(_tag, 'failed to show popup', e);
      return false;
    }
  }

  /// 隐藏小窗（若可见）。
  Future<void> close() async {
    if (!_visible) return;
    _visible = false;
    _pending = null;
    _watchdog?.cancel();
    try {
      await _channel.invokeMethod<void>('close');
    } on PlatformException catch (e) {
      logError(_tag, 'failed to close popup', e);
    } on MissingPluginException {
      // 忽略：宿主不存在时也不可能可见
    }
  }

  /// 小窗可见期间新到的外部请求 — 请求原生把新 URL 合入当前表单
  /// （append 模式），不重置用户正在编辑的表单。
  ///
  /// 返回 true = 请求已处置（已合入 / 按策略忽略），调用方不再处理；
  /// 返回 false = 小窗实际已不可见（内存标志失步，已就地复位自愈），
  /// 调用方应继续走正常 tryShow 流程。
  Future<bool> tryAppend(ExternalDownloadRequest req) async {
    if (!_visible) return false;
    // 音视频轨对请求无法作为普通 URL 行合入（audioUrl 依赖 pending 表
    // 独立通道透传）——维持既有"忽略并记日志"语义，不打断用户表单。
    if (req.audioUrl.isNotEmpty) {
      logInfo(_tag, 'popup open, ignoring track-pair request: ${req.url}');
      return true;
    }
    try {
      final ok = await _channel.invokeMethod<bool>('append', req.url) ?? false;
      if (ok) {
        logInfo(_tag, 'appended external request into visible popup');
        return true;
      }
      // 原生报告窗口实际不可见 — 内存标志失步，复位后让调用方走 show。
      logInfo(_tag, 'append refused: popup not visible, resetting state');
      _visible = false;
      _pending = null;
      _watchdog?.cancel();
      return false;
    } on MissingPluginException {
      // 旧版宿主无 append — 维持既有"忽略"语义，避免 show 重置用户表单
      logInfo(_tag, 'append not implemented, ignoring request');
      return true;
    } on PlatformException catch (e) {
      logError(_tag, 'append failed, ignoring request', e);
      return true;
    }
  }

  void _armWatchdog() {
    _watchdog?.cancel();
    _watchdog = Timer(_watchdogTimeout, () {
      logError(_tag, 'popup pending timed out, force closing');
      close();
    });
  }

  Future<dynamic> _onCall(MethodCall call) async {
    switch (call.method) {
      case 'onResult':
        _visible = false;
        _watchdog?.cancel();
        final pending = _pending;
        _pending = null;
        final result = QuickPopupResult.fromJsonString(
          call.arguments as String,
        );
        if (pending == null || pending.requestId != result.requestId) {
          logError(
            _tag,
            'stale popup result ignored: got=${result.requestId}, '
            'expected=${pending?.requestId}',
          );
          return;
        }
        logInfo(_tag, 'popup confirmed, requestId=${result.requestId}');
        submitQuickDownload(
          result: result.form,
          referrer: pending.referrer,
          hintFileSize: pending.fileSize,
          audioUrlOverride: pending.audioUrl,
        );
      case 'onClosed':
        logInfo(_tag, 'popup closed by user');
        _visible = false;
        _pending = null;
        _watchdog?.cancel();
    }
  }
}
