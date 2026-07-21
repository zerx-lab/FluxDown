/// 外部唤起独立快速下载小窗 — 主引擎侧服务。
///
/// 经 `fluxdown/popup_host` 通道请求原生宿主显示独立小窗
/// （原生窗口承载第二个 Flutter 引擎，见 popup-contract）：
/// - `show(payloadJson)`：投递表单载荷并显示小窗（置顶、不占任务栏、
///   不抢主窗口前台 — 这正是独立小窗对主窗口内对话框的核心优势）；
/// - `onResult(resultJson)`：用户确认，回填 pending 请求上下文
///   （referrer/fileSize）后经 [submitQuickDownload] 发送下载信号
///   （cookies 已随载荷进表单，由用户编辑后随结果带回）；
/// - `onClosed()`：用户取消/关闭；
/// - `relay`/`onRelay`：通用透传（清单预解析流程——弹窗提交单条 http(s)
///   链接时先经本服务做 ResolvePreview，命中清单则弹窗切清单选择视图，
///   确认后经 groupSubmit 中继回来发 CreateTaskGroup，见 popup_payload.dart
///   的 relay 信封注释）。
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
import 'cloud/cloud_auth_service.dart';
import 'log_service.dart';
import 'quick_download_submitter.dart';
import 'resolve_preview_client.dart';

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

  /// 当前在途的清单预解析（relay previewRequest 触发）；等待期间弹窗
  /// 保持表单 loading 态。会话复位（[_reset]）时一并取消。
  ResolvePreviewHandle? _previewHandle;

  /// 在途预解析对应的 relay seq（同一 show 会话内取消后重试会递增，
  /// previewCancel 按 seq 匹配，防误杀新一轮等待）。
  int _previewSeq = -1;

  /// 清单视图正在小窗中展示（previewResult 携清单成功中继后置位；
  /// manifestClosed / 建组 / 会话复位 / 新一轮 previewRequest 清位）。
  /// 置位期间外部请求不得 append 进（Offstage 的）表单——那是把请求
  /// 托付给确认建组后注定丢弃的 UI 状态。
  bool _manifestShowing = false;

  /// 托管缓冲的外部请求：清单视图期间到达的请求 + 小窗可见期间的音视频
  /// 轨对请求（原为忽略丢弃）。清单退出/会话结束时经 [redispatch] 重新
  /// 走正常分发链——受理即拥有，拥有到用户决策为止。
  final List<ExternalDownloadRequest> _deferredRequests = [];
  static const int _maxDeferredRequests = 20;
  bool _redispatchScheduled = false;

  /// 外部请求重分发入口（ExternalDownloadService.init 注入其完整分发链：
  /// 免打扰/独立小窗/回退对话框策略统一适用）。未注入时缓冲滞留、
  /// 下个冲刷时机重试，绝不静默丢弃。
  void Function(ExternalDownloadRequest req)? redispatch;

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
    final devices = CloudAuthService.instance.remoteDevices;
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
      devices: [
        for (final d in devices)
          QuickDeviceOption(
            deviceId: d.deviceId,
            name: d.name,
            platform: d.platform,
            isOnline: d.isOnline,
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
    // 会话已结束：冲刷托管缓冲（首个请求将获得自己的确认会话）。
    _scheduleRedispatch();
  }

  /// 小窗可见期间新到的外部请求 — 请求原生把新 URL 合入当前表单
  /// （append 模式），不重置用户正在编辑的表单。
  ///
  /// 返回 true = 请求已处置（已合入活表单 / 已托管缓冲待重分发），
  /// 调用方不再处理；返回 false = 小窗实际已不可见（内存标志失步，
  /// 已就地复位自愈），调用方应继续走正常 tryShow 流程。
  Future<bool> tryAppend(ExternalDownloadRequest req) async {
    // 清单视图期间：表单在 Offstage 中且确认建组会关窗——请求完整托管
    // 进缓冲区，清单退出（实时冲刷回表单）或会话结束（新会话）后重分发。
    if (_manifestShowing && isVisible) {
      _defer(req);
      return true;
    }
    // 音视频轨对请求无法作为普通 URL 行合入（audioUrl 依赖 pending 表
    // 独立通道透传）——同样托管缓冲，会话结束后获得自己的确认会话
    // （原为「忽略并记日志」的静默丢弃）。
    if (req.audioUrl.isNotEmpty) {
      if (isVisible) {
        _defer(req);
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

  /// 托管缓冲一条外部请求（带上限：满则丢最旧并记错误日志）。
  void _defer(ExternalDownloadRequest req) {
    if (_deferredRequests.length >= _maxDeferredRequests) {
      final dropped = _deferredRequests.removeAt(0);
      logError(_tag, 'deferred buffer full, dropping oldest: ${dropped.url}');
    }
    _deferredRequests.add(req);
    logInfo(
      _tag,
      'request deferred (${_deferredRequests.length} pending): ${req.url}',
    );
  }

  /// 冲刷托管缓冲：按到达顺序重新交给 [redispatch]（正常分发链）。
  ///
  /// 经 event-loop 异步隔离，避免在 _reset/onRelay 调用栈内重入
  /// tryShow/tryAppend。冲刷时若小窗已回到表单态（manifestClosed 路径），
  /// 重分发会命中 tryAppend 合入活表单；若会话已结束，首个请求 tryShow
  /// 新会话（携带自身 filename/size/mime/cookies 完整载荷），其余落入
  /// 既有 show 在途排队机制。
  void _scheduleRedispatch() {
    if (_redispatchScheduled || _deferredRequests.isEmpty) return;
    final handler = redispatch;
    if (handler == null) {
      logError(
        _tag,
        'no redispatch handler, keeping ${_deferredRequests.length} '
        'deferred request(s)',
      );
      return;
    }
    _redispatchScheduled = true;
    scheduleMicrotask(() {
      _redispatchScheduled = false;
      if (_deferredRequests.isEmpty) return;
      final batch = List<ExternalDownloadRequest>.of(_deferredRequests);
      _deferredRequests.clear();
      logInfo(_tag, 'redispatching ${batch.length} deferred request(s)');
      for (final r in batch) {
        handler(r);
      }
    });
  }

  /// 仅测试用：直接置清单视图标志（生产路径经 previewResult 中继置位）。
  @visibleForTesting
  void debugSetManifestShowing(bool value) {
    _manifestShowing = value;
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
  /// 注意：[_deferredRequests] 不清——托管缓冲必须跨会话存活到冲刷。
  void _reset() {
    _visible = false;
    _pending = null;
    _revealDeadline = null;
    _queuedAppendUrls.clear();
    _watchdog?.cancel();
    _manifestShowing = false;
    // 在途预解析随会话作废：cancel 让挂起的等待协程按 cancelled 语义
    // 退出（绝不在窗口已关后代提交任务）。
    _previewHandle?.cancel();
    _previewHandle = null;
    _previewSeq = -1;
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
        _scheduleRedispatch();
      case 'onClosed':
        logInfo(_tag, 'popup closed by user');
        _reset();
        _scheduleRedispatch();
      case 'onRelay':
        await _onRelay(call.arguments as String);
    }
  }

  // ── 清单预解析中继（relay 信封，见 popup_payload.dart） ─────────────────

  Future<bool> _invokeRelay(PopupRelayMessage msg) async {
    try {
      return await _channel.invokeMethod<bool>('relay', msg.toJsonString()) ??
          false;
    } on MissingPluginException {
      logInfo(_tag, 'relay not implemented by native host');
      return false;
    } on PlatformException catch (e) {
      logError(_tag, 'relay invoke failed', e);
      return false;
    }
  }

  Future<void> _onRelay(String json) async {
    final msg = PopupRelayMessage.fromJsonString(json);
    final pending = _pending;
    if (pending == null || pending.requestId != msg.requestId) {
      logError(
        _tag,
        'stale relay ${msg.kind} ignored: got=${msg.requestId}, '
        'expected=${pending?.requestId}',
      );
      return;
    }
    // relay 活动 = 用户在积极交互（预解析/清单浏览确认），重置 pending
    // 时效 watchdog，避免长时间挑选文件被 15min 兜底误关。
    _armWatchdog();
    switch (msg.kind) {
      case kPopupRelayPreviewRequest:
        await _onPreviewRequest(msg, pending);
      case kPopupRelayPreviewCancel:
        if (msg.seq == _previewSeq) {
          logInfo(_tag, 'preview cancelled by popup, seq=${msg.seq}');
          _previewHandle?.cancel();
          _previewHandle = null;
        }
      case kPopupRelayManifestClosed:
        // 弹窗退出清单视图回到表单：恢复 append 合入语义，托管缓冲实时
        // 冲刷（重分发命中 tryAppend → 合入用户眼前的活表单）。
        logInfo(_tag, 'manifest view closed, flushing deferred requests');
        _manifestShowing = false;
        _scheduleRedispatch();
      case kPopupRelayGroupSubmit:
        _onGroupSubmit(msg, pending);
      default:
        logError(_tag, 'unknown relay kind: ${msg.kind}');
    }
  }

  /// 弹窗提交了单条 http(s) 链接：先探测多文件清单。
  ///
  /// - 无清单/超时 → 主引擎直接代提交（与 onResult 路径同一语义）并关窗；
  /// - 命中清单 → previewResult 中继给弹窗切清单选择视图；
  /// - 轨对请求/门控不满足 → 直接代提交（弹窗侧已判，此处兜底）。
  Future<void> _onPreviewRequest(
    PopupRelayMessage msg,
    _PendingRequest pending,
  ) async {
    final form = decodePreviewRequestForm(msg);
    // previewRequest 只可能来自表单视图：清单标志复位（覆盖 Esc 退出后
    // manifestClosed 消息极端丢失的情形，防止 append 永久走缓冲）。
    _manifestShowing = false;
    // 镜像 new_download_dialog 的提交点偏好记录（清单路径确认后不再经过
    // submitQuickDownload；无清单回退时那里会以同值重记，幂等）。
    SettingsProvider.globalInstance?.recordLastSaveDir(form.saveDir.trim());
    if (form.threadsUserModified) {
      SettingsProvider.globalInstance?.setLastDialogThreads(
        form.segments > 0 ? form.segments.toString() : 'auto',
      );
    }
    final entries = parseQuickDownloadEntries(form.urlText);
    if (pending.audioUrl.isNotEmpty ||
        entries.length != 1 ||
        !isManifestPreviewableUrl(entries.first.url)) {
      _finishSubmit(form, pending);
      return;
    }
    _previewHandle?.cancel();
    _previewSeq = msg.seq;
    final handle = ResolvePreviewClient.start(
      url: entries.first.url,
      cookies: form.cookies,
      referrer: pending.referrer,
      userAgent: form.userAgent,
      extraHeaders: form.extraHeaders,
    );
    _previewHandle = handle;
    logInfo(_tag, 'preview started for popup, seq=${msg.seq}');
    final manifest = await handle.future;
    if (!identical(_previewHandle, handle)) return; // 已被取消/取代
    _previewHandle = null;
    if (handle.cancelled) return;
    if (!identical(_pending, pending)) return; // 会话已复位（窗口已关）
    if (manifest == null) {
      // 无清单/error/超时 → 零差异回退：代提交 + 关窗。
      logInfo(_tag, 'no manifest, submitting directly, seq=${msg.seq}');
      _finishSubmit(form, pending);
      return;
    }
    logInfo(
      _tag,
      'manifest hit (${manifest.items.length} items), relaying to popup',
    );
    final ok = await _invokeRelay(
      encodePreviewResult(
        requestId: msg.requestId,
        seq: msg.seq,
        manifest: manifest,
      ),
    );
    if (ok) {
      // 弹窗已切清单视图：此后到达的外部请求托管缓冲，不进 Offstage 表单。
      _manifestShowing = true;
    } else {
      // 弹窗引擎已不可达：不代建组（未经选择的清单绝不全量下载），
      // 交由 watchdog / onClosed 复位会话。
      logError(_tag, 'popup unreachable, dropping manifest preview');
    }
  }

  /// 清单视图确认：回填 referrer 后发 CreateTaskGroup，关窗复位。
  void _onGroupSubmit(PopupRelayMessage msg, _PendingRequest pending) {
    final sub = decodeGroupSubmit(msg);
    logInfo(
      _tag,
      'popup group confirmed, requestId=${msg.requestId}, '
      'items=${sub.items.length}',
    );
    CreateTaskGroup(
      sourceUrl: sub.sourceUrl,
      groupName: sub.groupName,
      saveDir: sub.saveDir,
      queueId: sub.queueId,
      segments: sub.segments,
      cookies: sub.cookies,
      referrer: pending.referrer,
      userAgent: sub.userAgent,
      proxyUrl: sub.proxyUrl,
      extraHeaders: sub.extraHeaders,
      ignoreTlsErrors: sub.ignoreTlsErrors,
      startPaused: sub.startPaused,
      items: sub.items,
    ).sendSignalToRust();
    close();
  }

  /// 主引擎代提交（无清单回退/门控不满足）：语义与 onResult 路径一致。
  void _finishSubmit(QuickDownloadFormResult form, _PendingRequest pending) {
    submitQuickDownload(
      result: form,
      referrer: pending.referrer,
      hintFileSize: pending.fileSize,
      audioUrlOverride: pending.audioUrl,
    );
    close();
  }
}
