// FluxCloud 跨设备任务协同客户端 —— 单例 + ChangeNotifier（同 ConfigSyncService
// 的单例 + SSE 长连风格）。一个服务承担三种角色，互不冲突：
//
//   查看端：SSE 接收 task.progress（批量增量）/ task.status / presence，增量更新
//           本地 [_remoteTasks] 快照，~300ms 合并后注入 DownloadController，驱动
//           设备区混排 + 进度回流 UI（绝不逐事件重建列表）。
//   接收端：SSE 收到 task.dispatch（目标为本机）→ 经 DownloadController 建本地任务
//           执行，回 reportTaskStatus(accepted)；task.command（目标本机）→ 暂停/
//           恢复/取消对应本地任务。
//   执行端：Timer 1s 采样「由下发产生的本地任务」→ 批量 reportProgress（仅活跃）+
//           状态转换即时 reportTaskStatus。节流批量是性能关键（对标迅雷云中转，
//           进度只走内存 + SSE，绝不高频请求/落库）。
//
// 数据面永远直连本地引擎执行；云端仅做连接与下发/进度中转，不取回文件。

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../../models/download_controller.dart';
import '../../models/download_task.dart';
import '../../models/settings_provider.dart';
import '../log_service.dart';
import 'cloud_auth_service.dart';
import 'cloud_client.dart';
import 'cloud_models.dart';
import 'device_identity.dart';

const _tag = 'RemoteTask';
const _kEventsPath = '/api/v1/tasks/events';
const _kSseIdleTimeout = Duration(seconds: 75);
const _kReportInterval = Duration(seconds: 1);
const _kFlushDebounce = Duration(milliseconds: 300);
const _kPresenceDebounce = Duration(seconds: 2);
const _kRetryDelays = [
  Duration(seconds: 5),
  Duration(seconds: 15),
  Duration(seconds: 60),
];

/// 跨设备任务协同服务单例。home_page 在 providers 就绪后调 [attach] 一次。
class RemoteTaskService extends ChangeNotifier {
  RemoteTaskService._();

  static final RemoteTaskService instance = RemoteTaskService._();

  /// 查看端快照：remoteTaskId → RemoteTask（进度经 SSE 增量 copyWith）。
  final Map<String, RemoteTask> _remoteTasks = {};

  /// 只读快照，供设置页/调试查看（侧栏走 DownloadController 混排，不直接读这里）。
  Map<String, RemoteTask> get remoteTasks => Map.unmodifiable(_remoteTasks);

  /// 执行端映射：本地 taskId → remoteTaskId（下发任务落地本机后建立）。
  final Map<String, String> _localToRemote = {};

  /// 执行端待关联：下发任务 url → remoteTaskId（等本地引擎建出同 url 任务再绑定）。
  final Map<String, String> _awaitingLocal = {};

  /// 执行端已上报的最近状态：remoteTaskId → wire 状态（去重，仅转换才上报）。
  final Map<String, String> _lastStatus = {};

  String get _deviceId => DeviceIdentity.deviceId();

  bool _running = false;
  bool _stopped = true;
  bool _authAttached = false;
  bool _controllerAttached = false;

  HttpClient? _sseHttp;
  StreamSubscription<String>? _sseSub;
  Timer? _sseWatchdog;
  Timer? _reportTimer;
  Timer? _flushTimer;
  Timer? _presenceTimer;
  Timer? _retryTimer;
  int _retryAttempt = 0;

  // ── 接线 ─────────────────────────────────────────────────────────────

  /// home_page 在 providers 创建后调用一次：挂账户/控制器监听，登录即启动。
  Future<void> attach() async {
    if (!_authAttached) {
      _authAttached = true;
      CloudAuthService.instance.addListener(_onAuthChanged);
    }
    final ctrl = DownloadController.globalInstance;
    if (ctrl != null && !_controllerAttached) {
      _controllerAttached = true;
      ctrl.addListener(_onControllerChanged);
    }
    if (CloudAuthService.instance.isLoggedIn) {
      await start();
    }
  }

  void _onAuthChanged() {
    if (CloudAuthService.instance.isLoggedIn) {
      if (!_running) unawaited(start());
    } else {
      stop();
    }
  }

  // ── 生命周期 ─────────────────────────────────────────────────────────

  Future<void> start() async {
    if (_running || !CloudAuthService.instance.isLoggedIn) return;
    _running = true;
    _stopped = false;
    _retryAttempt = 0;
    _reportTimer?.cancel();
    _reportTimer = Timer.periodic(_kReportInterval, (_) => _reportTick());
    await _syncAndConnect();
  }

  void stop() {
    _stopped = true;
    _running = false;
    _cancelRetry();
    _reportTimer?.cancel();
    _reportTimer = null;
    _flushTimer?.cancel();
    _flushTimer = null;
    _presenceTimer?.cancel();
    _presenceTimer = null;
    _closeSse();
    _remoteTasks.clear();
    _localToRemote.clear();
    _awaitingLocal.clear();
    _lastStatus.clear();
    DownloadController.globalInstance?.updateRemoteTasks(const []);
    notifyListeners();
  }

  /// 拉全量（断线重连/首启用）→ 建 SSE 流。任何失败走退避重连。
  Future<void> _syncAndConnect() async {
    if (_stopped) return;
    try {
      await _pullAll();
      if (_stopped) return;
      await _connectSse();
      if (_stopped) {
        _closeSse();
        return;
      }
      _retryAttempt = 0;
    } catch (e, stack) {
      if (_stopped) return;
      logError(_tag, 'sync/connect failed', e, stack);
      _scheduleRetry();
    }
  }

  Future<void> _pullAll() async {
    final list = await CloudClient.instance.remoteTasks();
    _remoteTasks
      ..clear()
      ..addEntries(list.map((r) => MapEntry(r.id, r)));
    _pushToController();
    notifyListeners();
  }

  // ── SSE 事件流（仿 ConfigSyncService._connectSse）─────────────────────

  Future<void> _connectSse() async {
    _closeSse();
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
    final deviceId = Uri.encodeQueryComponent(_deviceId);
    final uri = Uri.parse('${CloudApiConfig.baseUrl}$_kEventsPath?deviceId=$deviceId');
    HttpClientResponse res;
    try {
      final req = await client.getUrl(uri);
      req.headers.set('Accept', 'text/event-stream');
      req.headers.set('Authorization', 'Bearer ${CloudClient.instance.accessToken}');
      res = await req.close();
    } catch (e) {
      client.close(force: true);
      throw CloudApiException(
        code: 'network_error',
        message: 'SSE 连接失败：$e',
        status: 0,
      );
    }
    if (res.statusCode != 200) {
      final body = await res.transform(utf8.decoder).join();
      client.close(force: true);
      throw CloudApiException(
        code: res.statusCode == 401 ? 'unauthorized' : 'sse_error',
        message: body.trim().isNotEmpty ? body.trim() : 'HTTP ${res.statusCode}',
        status: res.statusCode,
      );
    }
    _sseHttp = client;
    _resetWatchdog();
    _sseSub = res
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen(
          _onSseLine,
          onDone: _onSseDisconnected,
          onError: (_) => _onSseDisconnected(),
        );
  }

  void _onSseLine(String line) {
    _resetWatchdog();
    if (!line.startsWith('data:')) return;
    final payload = line.substring('data:'.length).trim();
    if (payload.isEmpty) return;
    try {
      final json = jsonDecode(payload) as Map<String, dynamic>;
      _onEvent(json);
    } catch (e, stack) {
      logError(_tag, 'sse payload parse failed: $payload', e, stack);
    }
  }

  void _onEvent(Map<String, dynamic> json) {
    switch (json['type'] as String?) {
      case 'task.dispatch':
        final r = RemoteTask.fromJson(json);
        _remoteTasks[r.id] = r;
        _scheduleFlush();
        if (r.toDevice == _deviceId && r.status == RemoteTaskStatus.pending) {
          _acceptDispatch(r);
        }
      case 'task.progress':
        final items = json['items'] as List<dynamic>? ?? const [];
        var changed = false;
        for (final it in items) {
          if (it is! Map<String, dynamic>) continue;
          final id = it['taskId'] as String?;
          if (id == null) continue;
          final cur = _remoteTasks[id];
          if (cur == null) continue;
          _remoteTasks[id] = cur.copyWith(
            status: cur.status.isTerminal
                ? cur.status
                : RemoteTaskStatus.downloading,
            downloadedBytes: (it['downloadedBytes'] as num?)?.toInt(),
            speed: (it['speed'] as num?)?.toInt(),
            progress: (it['progress'] as num?)?.toDouble(),
          );
          changed = true;
        }
        if (changed) _scheduleFlush();
      case 'task.status':
        final r = RemoteTask.fromJson(json);
        _remoteTasks[r.id] = r;
        _scheduleFlush();
      case 'task.command':
        final target =
            json['targetDevice'] as String? ?? json['toDevice'] as String?;
        if (target == _deviceId) _applyCommand(json);
      case 'presence':
        _schedulePresenceRefresh();
    }
  }

  void _resetWatchdog() {
    _sseWatchdog?.cancel();
    _sseWatchdog = Timer(_kSseIdleTimeout, () {
      logInfo(_tag, 'sse idle ${_kSseIdleTimeout.inSeconds}s, reconnecting');
      _onSseDisconnected();
    });
  }

  void _onSseDisconnected() {
    if (_stopped) return;
    _closeSse();
    _scheduleRetry();
  }

  void _closeSse() {
    _sseWatchdog?.cancel();
    _sseWatchdog = null;
    _sseSub?.cancel();
    _sseSub = null;
    _sseHttp?.close(force: true);
    _sseHttp = null;
  }

  void _scheduleRetry() {
    _cancelRetry();
    if (_stopped) return;
    final idx = _retryAttempt.clamp(0, _kRetryDelays.length - 1);
    _retryAttempt = idx + 1;
    _retryTimer = Timer(_kRetryDelays[idx], () {
      if (_stopped) return;
      unawaited(_syncAndConnect());
    });
  }

  void _cancelRetry() {
    _retryTimer?.cancel();
    _retryTimer = null;
  }

  // ── 查看端：合并推送给 DownloadController ─────────────────────────────

  /// ~300ms 合并窗口：高频进度事件只触发一次列表重建 + notify（性能关键）。
  void _scheduleFlush() {
    _flushTimer ??= Timer(_kFlushDebounce, () {
      _flushTimer = null;
      _pushToController();
      notifyListeners();
    });
  }

  void _pushToController() {
    final list = _remoteTasks.values.map(_asDownloadTask).toList();
    DownloadController.globalInstance?.updateRemoteTasks(list);
  }

  void _schedulePresenceRefresh() {
    _presenceTimer ??= Timer(_kPresenceDebounce, () {
      _presenceTimer = null;
      unawaited(CloudAuthService.instance.refreshDevices());
    });
  }

  // ── 接收端：把下发任务落到本地引擎执行 ───────────────────────────────

  void _acceptDispatch(RemoteTask r) {
    final ctrl = DownloadController.globalInstance;
    if (ctrl == null) return;
    final saveDir = (r.saveDir != null && r.saveDir!.isNotEmpty)
        ? r.saveDir!
        : (SettingsProvider.globalInstance?.effectiveDefaultSaveDir ?? '');
    ctrl.createTask(url: r.url, saveDir: saveDir, fileName: r.fileName);
    _awaitingLocal[r.url] = r.id;
    unawaited(_safeReportStatus(r.id, 'accepted'));
  }

  void _applyCommand(Map<String, dynamic> json) {
    final rid = json['taskId'] as String? ?? json['id'] as String?;
    final action = json['action'] as String?;
    if (rid == null || action == null) return;
    String? localId;
    for (final e in _localToRemote.entries) {
      if (e.value == rid) {
        localId = e.key;
        break;
      }
    }
    if (localId == null) return;
    final ctrl = DownloadController.globalInstance;
    switch (action) {
      case 'pause':
        ctrl?.pauseTask(localId);
      case 'resume':
        ctrl?.resumeTask(localId);
      case 'cancel':
        ctrl?.cancelTask(localId);
    }
  }

  // ── 执行端：关联本地任务 + 1s 批量上报进度/状态 ──────────────────────

  void _onControllerChanged() {
    if (_awaitingLocal.isEmpty) return;
    final ctrl = DownloadController.globalInstance;
    if (ctrl == null) return;
    for (final t in ctrl.localTasks) {
      if (_localToRemote.containsKey(t.id)) continue;
      final rid = _awaitingLocal[t.url];
      if (rid != null) {
        _localToRemote[t.id] = rid;
        _awaitingLocal.remove(t.url);
      }
    }
  }

  void _reportTick() {
    if (_localToRemote.isEmpty) return;
    final ctrl = DownloadController.globalInstance;
    if (ctrl == null) return;
    final byId = {for (final t in ctrl.localTasks) t.id: t};
    final reports = <ProgressReport>[];
    final settledLocalIds = <String>[];
    for (final entry in _localToRemote.entries) {
      final t = byId[entry.key];
      if (t == null) continue;
      final rid = entry.value;
      final wire = _localStatusToWire(t.status);
      if (_lastStatus[rid] != wire) {
        _lastStatus[rid] = wire;
        unawaited(
          _safeReportStatus(
            rid,
            wire,
            totalBytes: t.totalBytes > 0 ? t.totalBytes : null,
            fileName: t.fileName.isNotEmpty ? t.fileName : null,
            error: t.status == TaskStatus.error ? t.errorMessage : null,
          ),
        );
        if (wire == 'completed' || wire == 'failed') {
          settledLocalIds.add(entry.key);
        }
      }
      if (t.status == TaskStatus.downloading) {
        reports.add(
          ProgressReport(
            taskId: rid,
            downloadedBytes: t.downloadedBytes,
            speed: t.speed,
            progress: t.totalBytes > 0 ? t.downloadedBytes / t.totalBytes : 0,
          ),
        );
      }
    }
    if (reports.isNotEmpty) {
      unawaited(_safeReportProgress(reports));
    }
    for (final id in settledLocalIds) {
      _localToRemote.remove(id);
    }
  }

  Future<void> _safeReportStatus(
    String id,
    String status, {
    int? totalBytes,
    String? fileName,
    String? error,
  }) async {
    try {
      await CloudClient.instance.reportTaskStatus(
        id,
        status: status,
        totalBytes: totalBytes,
        fileName: fileName,
        error: error,
      );
    } catch (e, stack) {
      logError(_tag, 'reportTaskStatus failed: $id', e, stack);
    }
  }

  Future<void> _safeReportProgress(List<ProgressReport> items) async {
    try {
      await CloudClient.instance.reportProgress(items);
    } catch (e, stack) {
      logError(_tag, 'reportProgress failed', e, stack);
    }
  }

  // ── 状态映射 ─────────────────────────────────────────────────────────

  String _localStatusToWire(TaskStatus s) => switch (s) {
    TaskStatus.downloading ||
    TaskStatus.preparing ||
    TaskStatus.resuming => 'downloading',
    TaskStatus.paused => 'paused',
    TaskStatus.completed => 'completed',
    TaskStatus.error => 'failed',
    _ => 'accepted',
  };

  TaskStatus _mapStatus(RemoteTaskStatus s) => switch (s) {
    RemoteTaskStatus.downloading => TaskStatus.downloading,
    RemoteTaskStatus.paused => TaskStatus.paused,
    RemoteTaskStatus.completed => TaskStatus.completed,
    RemoteTaskStatus.failed => TaskStatus.error,
    RemoteTaskStatus.canceled => TaskStatus.error,
    _ => TaskStatus.pending,
  };

  DownloadTask _asDownloadTask(RemoteTask r) => DownloadTask(
    id: 'remote:${r.id}',
    url: r.url,
    fileName: r.fileName.isNotEmpty ? r.fileName : _fileNameFromUrl(r.url),
    saveDir: r.saveDir ?? '',
    status: _mapStatus(r.status),
    downloadedBytes: r.downloadedBytes,
    totalBytes: r.totalBytes ?? 0,
    speed: r.speed,
    errorMessage: r.error ?? '',
    deviceId: r.toDevice,
    isRemote: true,
    createdAt: DateTime.tryParse(r.createdAt),
  );

  String _fileNameFromUrl(String url) {
    try {
      final seg = Uri.parse(url).pathSegments;
      if (seg.isNotEmpty && seg.last.isNotEmpty) return seg.last;
    } catch (_) {}
    return url;
  }
}
