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

  /// show 握手在途（invokeMethod('show') 尚未返回）。外部请求短时爆发
  /// （浏览器扩展批量下载会拆成 N 条独立请求）时，后续请求在此期间到达
  /// 必须排队等 append，而不是各自再发 show 互相覆盖载荷。
  bool _showing = false;

  /// show 返回后到弹窗引擎 reveal 之间的宽限期截止时刻。此窗口内原生
  /// append 会因窗口尚未实际可见而返回 false，应重试而非判定失步复位。
  DateTime? _revealDeadline;

  /// 等待合入表单的 URL（show 在途 / reveal 宽限期内被拒时缓冲）。
  final List<String> _queuedAppendUrls = [];
  bool _flushScheduled = false;

  /// 请求序号发生器
  int _seq = 0;

  _PendingRequest? _pending;

  /// pending 时效 watchdog — 弹窗引擎异常（渲染卡死/通道断开）时
  /// onResult/onClosed 永不到达，[_visible] 会永久卡 true 导致后续外部
  /// 请求全部被丢弃。超时后主动 close() 复位。append 路径的原生真值
  /// 校验（见 [tryAppend]）是第一道自愈，本定时器是最后兜底。
  Timer? _watchdog;
  static const _watchdogTimeout = Duration(minutes: 15);

  /// show 返回后到弹窗实际 reveal 的宽限时长（原生兜底 3s 强制显示，
  /// 留冗余覆盖首帧慢的极端情况）。
  static const _revealGrace = Duration(seconds: 6);

  bool get isVisible => _visible || _showing;

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

    _showing = true;
    try {
      final shown =
          await _channel.invokeMethod<bool>('show', payload.toJsonString()) ??
          false;
      if (shown) {
        _visible = true;
        // show 返回后弹窗引擎尚需时间完成首帧 + reveal（原生 3s 兜底），
        // 此窗口内 append 被原生拒绝应视为"尚未 reveal"而非失步。
        _revealDeadline = DateTime.now().add(_revealGrace);
        _pending = _PendingRequest(
          requestId: payload.requestId,
          referrer: req.referrer,
          fileSize: req.fileSize,
          audioUrl: req.audioUrl,
        );
        _armWatchdog();
        logInfo(_tag, 'popup shown, requestId=${payload.requestId}');
        _scheduleFlush();
      } else {
        logError(_tag, 'native host refused to show popup');
        _queuedAppendUrls.clear();
      }
      return shown;
    } on MissingPluginException {
      // 原生宿主未实现（该平台尚无 popup host）— 静默回退
      logInfo(_tag, 'popup host not implemented on this platform');
      _queuedAppendUrls.clear();
      return false;
    } on PlatformException catch (e) {
      logError(_tag, 'failed to show popup', e);
      _queuedAppendUrls.clear();
      return false;
    } finally {
      _showing = false;
    }
  }

  /// 隐藏小窗（若可见）。
  Future<void> close() async {
    if (!_visible) return;
    _reset();
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
    // 音视频轨对请求无法作为普通 URL 行合入（audioUrl 依赖 pending 表
    // 独立通道透传）——维持既有"忽略并记日志"语义，不打断用户表单。
    if (req.audioUrl.isNotEmpty) {
      if (isVisible) {
        logInfo(_tag, 'popup open, ignoring track-pair request: ${req.url}');
        return true;
      }
      return false;
    }
    // show 握手在途（批量请求爆发）— 缓冲，show 成功后统一 append，
    // 避免各请求并发 show 互相覆盖载荷（浏览器扩展批量下载只剩 1 条的根因）。
    if (_showing) {
      _queuedAppendUrls.add(req.url);
      logInfo(_tag, 'show in flight, queued append: ${req.url}');
      return true;
    }
    if (!_visible) return false;
    try {
      final ok = await _channel.invokeMethod<bool>('append', req.url) ?? false;
      if (ok) {
        logInfo(_tag, 'appended external request into visible popup');
        return true;
      }
      // reveal 宽限期内：窗口尚未实际显示（首帧未就绪），不是失步 —
      // 缓冲重试而不是复位再 show（那会重置用户表单/覆盖批量 URL）。
      final deadline = _revealDeadline;
      if (deadline != null && DateTime.now().isBefore(deadline)) {
        _queuedAppendUrls.add(req.url);
        logInfo(_tag, 'append deferred until reveal: ${req.url}');
        _scheduleFlush();
        return true;
      }
      // 原生报告窗口实际不可见 — 内存标志失步，复位后让调用方走 show。
      logInfo(_tag, 'append refused: popup not visible, resetting state');
      _reset();
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

  /// 异步冲刷缓冲的 append URL：reveal 前原生会拒绝，按固定间隔重试
  /// 直至宽限期截止（原生 3s reveal 兜底保证窗口最终可见）。
  void _scheduleFlush() {
    if (_flushScheduled || _queuedAppendUrls.isEmpty) return;
    _flushScheduled = true;
    unawaited(_flushQueued());
  }

  Future<void> _flushQueued() async {
    try {
      while (_queuedAppendUrls.isNotEmpty && _visible) {
        final url = _queuedAppendUrls.first;
        bool ok = false;
        try {
          ok = await _channel.invokeMethod<bool>('append', url) ?? false;
        } on MissingPluginException {
          logInfo(_tag, 'append not implemented, dropping queued urls');
          _queuedAppendUrls.clear();
          return;
        } on PlatformException catch (e) {
          logError(_tag, 'queued append failed, dropping: $url', e);
          _queuedAppendUrls.removeAt(0);
          continue;
        }
        if (ok) {
          _queuedAppendUrls.removeAt(0);
          logInfo(_tag, 'flushed queued append: $url');
          continue;
        }
        final deadline = _revealDeadline;
        if (deadline == null || !DateTime.now().isBefore(deadline)) {
          logError(
            _tag,
            'popup never revealed, dropping ${_queuedAppendUrls.length} queued url(s)',
          );
          _queuedAppendUrls.clear();
          return;
        }
        await Future<void>.delayed(const Duration(milliseconds: 200));
      }
    } finally {
      _flushScheduled = false;
      // 冲刷期间可能又有新缓冲进来
      if (_queuedAppendUrls.isNotEmpty && _visible) _scheduleFlush();
    }
  }

  /// 复位所有会话状态（关闭/提交/失步时调用）。
  void _reset() {
    _visible = false;
    _pending = null;
    _revealDeadline = null;
    _queuedAppendUrls.clear();
    _watchdog?.cancel();
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
        final pending = _pending;
        _reset();
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
        _reset();
    }
  }
}
