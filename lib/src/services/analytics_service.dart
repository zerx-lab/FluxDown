import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:ui';

import 'log_service.dart';

const _tag = 'Analytics';

/// 构建时注入的版本号，与 update_service.dart 保持一致。
const _appVersion = String.fromEnvironment('APP_VERSION', defaultValue: 'dev');

/// Countly 分析服务封装 — 基于 HTTP API 直接通信。
///
/// **零侵入保证**：当服务不可用（用户关闭、服务器宕机、网络不通）时，
/// 不持有 HttpClient、不运行定时器、不堆积内存。
///
/// 所有公开方法均为 fire-and-forget 设计，内部捕获全部异常，
/// 绝不会阻塞 UI 或引发未捕获异常。
class AnalyticsService {
  AnalyticsService._();
  static final instance = AnalyticsService._();

  bool _initialized = false;
  bool _enabled = true;

  static const _serverUrl = 'https://countly.zerx.dev';

  /// Countly App Key，构建期通过 --dart-define=ANALYTICS_APP_KEY=xxx 注入。
  /// 未注入（本地开发/第三方构建）时为空，分析功能自动禁用。
  static const _appKey = String.fromEnvironment('ANALYTICS_APP_KEY');

  /// 失败请求重试队列上限
  static const _maxRetryQueue = 20;

  /// 连续失败 N 次后停止重试，直到下次 beginSession 重置
  static const _maxConsecutiveFailures = 5;

  /// Countly 要求的功能同意列表
  static const _consentFeatures = [
    'sessions',
    'events',
    'views',
    'crashes',
    'users',
  ];

  /// 事件缓冲区 — 批量发送，减少网络请求
  final List<Map<String, Object?>> _eventQueue = [];

  /// 失败请求重试队列 — 心跳时重发
  final List<Map<String, String>> _retryQueue = [];

  /// 连续发送失败计数
  int _consecutiveFailures = 0;

  /// 设备唯一标识
  late String _deviceId;

  /// Session 是否处于活跃状态
  bool _sessionActive = false;

  /// HTTP client — 仅在 enabled 时创建，disabled 时立即释放
  HttpClient? _httpClient;

  /// Session 心跳计时
  DateTime? _lastSessionUpdate;

  /// 60 秒定时器：session 心跳 + 定时 flush 事件队列 + 重试失败请求
  Timer? _heartbeatTimer;

  /// 上次心跳以来是否有过 trackEvent 调用，用于跳过纯空闲心跳的 HTTP 请求。
  bool _hadEventSinceLastHeartbeat = false;

  /// 延迟 flush 去重 timer
  Timer? _delayedFlushTimer;

  /// 应用启动时间，用于计算 crash _run 字段
  final DateTime _appStartTime = DateTime.now();

  /// init() 完成的 Completer，供早期 logException 等待
  Completer<void>? _initCompleter;

  // ---------------------------------------------------------------------------
  // 初始化
  // ---------------------------------------------------------------------------

  /// 初始化分析服务。
  ///
  /// [enabled] 从 SettingsProvider 读取的用户同意状态。
  /// 如果 [enabled] 为 false，仅记录 deviceId，不创建 HttpClient/Timer。
  Future<void> init({required bool enabled}) async {
    if (_initialized) return;
    if (_initCompleter != null) return _initCompleter!.future;

    _initCompleter = Completer<void>();
    // 未注入 App Key（本地开发/第三方构建）时强制禁用
    _enabled = enabled && _appKey.isNotEmpty;

    try {
      _deviceId = await _getOrCreateDeviceId();
      _initialized = true;
      logInfo(_tag, 'initialized, consent=$_enabled, deviceId=$_deviceId');

      if (_enabled) {
        _ensureHttpClient();
        await _beginSession();
        _startHeartbeat();
      }
      // disabled 时：不创建 HttpClient，不启动 Timer，零开销

      _initCompleter!.complete();
    } catch (e, stack) {
      logError(_tag, 'init failed', e, stack);
      _initCompleter!.complete();
    }
  }

  /// 优雅关闭 — 应在应用退出流程中调用。
  Future<void> dispose() async {
    if (!_initialized) return;
    try {
      _stopHeartbeat();
      if (_enabled && _httpClient != null) {
        await _flushEvents();
        await _endSession();
      }
      _releaseResources();
      logInfo(_tag, 'disposed');
    } catch (e, stack) {
      logError(_tag, 'dispose failed', e, stack);
    }
  }

  // ---------------------------------------------------------------------------
  // 同意控制
  // ---------------------------------------------------------------------------

  /// 用户在设置中切换数据收集同意状态。
  void setEnabled(bool value) {
    if (_enabled == value) return;
    _enabled = value;
    if (!_initialized) return;

    try {
      if (value) {
        _consecutiveFailures = 0;
        _ensureHttpClient();
        _beginSession();
        _startHeartbeat();
      } else {
        _stopHeartbeat();
        // 关闭时不等待网络（服务器可能不可达），直接释放
        _eventQueue.clear();
        _retryQueue.clear();
        _sessionActive = false;
        _releaseResources();
      }
      logInfo(_tag, 'consent changed to $value');
    } catch (e, stack) {
      logError(_tag, 'setEnabled failed', e, stack);
    }
  }

  // ---------------------------------------------------------------------------
  // 视图追踪
  // ---------------------------------------------------------------------------

  void trackView(String viewName) {
    if (!_enabled || !_initialized) return;
    try {
      _recordEvent(
        '[CLY]_view',
        segmentation: {
          'name': viewName,
          'segment': Platform.operatingSystem,
          'visit': '1',
        },
      );
    } catch (e, stack) {
      logError(_tag, 'trackView failed', e, stack);
    }
  }

  // ---------------------------------------------------------------------------
  // 事件追踪
  // ---------------------------------------------------------------------------

  void trackEvent(
    String key, {
    Map<String, Object>? segmentation,
    int count = 1,
    double sum = 0,
  }) {
    if (!_enabled || !_initialized) return;
    try {
      _hadEventSinceLastHeartbeat = true;
      _recordEvent(key, segmentation: segmentation, count: count, sum: sum);
    } catch (e, stack) {
      logError(_tag, 'trackEvent($key) failed', e, stack);
    }
  }

  void trackDownloadCreated(String protocol) {
    trackEvent('download_created', segmentation: {'protocol': protocol});
  }

  void trackDownloadCompleted(String protocol, int fileSizeBytes) {
    final sizeMb = fileSizeBytes / 1048576;
    final bucket = switch (sizeMb) {
      < 1 => '<1MB',
      < 10 => '1-10MB',
      < 100 => '10-100MB',
      < 1024 => '100MB-1GB',
      _ => '>1GB',
    };
    trackEvent(
      'download_completed',
      segmentation: {'protocol': protocol, 'size_bucket': bucket},
    );
  }

  void trackDownloadFailed(String protocol, String errorType) {
    trackEvent(
      'download_failed',
      segmentation: {'protocol': protocol, 'error_type': errorType},
    );
  }

  void trackExternalDownload() {
    trackEvent('external_download');
  }

  // ---------------------------------------------------------------------------
  // 崩溃上报
  // ---------------------------------------------------------------------------

  /// 手动记录异常到 Countly。
  ///
  /// 如果 SDK 尚未初始化完成，会等待 init() 完成后再尝试（最多 5 秒）。
  /// 如果服务不可用，静默丢弃，不堆积。
  void logException(
    String message,
    bool isFatal, {
    StackTrace? stackTrace,
    Map<String, Object>? segmentation,
  }) {
    _logExceptionAsync(
      message,
      isFatal,
      stackTrace: stackTrace,
      segmentation: segmentation,
    );
  }

  Future<void> _logExceptionAsync(
    String message,
    bool isFatal, {
    StackTrace? stackTrace,
    Map<String, Object>? segmentation,
  }) async {
    try {
      // 等待 init 完成（最多 5 秒）
      if (!_initialized && _initCompleter != null) {
        await _initCompleter!.future.timeout(
          const Duration(seconds: 5),
          onTimeout: () {},
        );
      }
      if (!_enabled || !_initialized) return;

      final crash = <String, Object?>{
        '_os': Platform.operatingSystem,
        '_os_version': Platform.operatingSystemVersion,
        '_app_version': _appVersion,
        '_architecture': _cpuArchitecture,
        '_error': '$message\n${stackTrace ?? ''}',
        '_nonfatal': !isFatal,
        '_run': _runDurationSeconds,
      };
      if (segmentation != null) {
        crash['_custom'] = segmentation;
      }
      // 崩溃立即发送，失败不重试（避免大堆栈 JSON 堆积在重试队列）
      await _sendRequest({'crash': jsonEncode(crash)}, allowRetry: false);
    } catch (e, stack) {
      logError(_tag, 'logException failed', e, stack);
    }
  }

  // ---------------------------------------------------------------------------
  // Session 管理
  // ---------------------------------------------------------------------------

  Future<void> _beginSession() async {
    if (_sessionActive) return;
    _sessionActive = true;
    _lastSessionUpdate = DateTime.now();
    _consecutiveFailures = 0;

    final metrics = {
      '_os': Platform.operatingSystem,
      '_os_version': Platform.operatingSystemVersion,
      '_app_version': _appVersion,
      '_resolution': _screenResolution,
      '_architecture': _cpuArchitecture,
    };

    final consent = <String, bool>{for (final f in _consentFeatures) f: true};

    await _sendRequest({
      'begin_session': '1',
      'metrics': jsonEncode(metrics),
      'consent': jsonEncode(consent),
    });
  }

  Future<void> _endSession() async {
    if (!_sessionActive) return;
    _sessionActive = false;
    final duration = _sessionDurationSinceLastUpdate;
    await _sendRequest({'end_session': '1', 'session_duration': '$duration'});
  }

  void _updateSession() {
    if (!_sessionActive) return;
    final duration = _sessionDurationSinceLastUpdate;
    if (duration <= 0) return;
    // 完全空闲（无 trackEvent 调用、无待重试请求）时跳过本次 session_duration 上报，
    // 避免无意义 HTTP 请求。下次有活动或 _endSession 时仍会正确上报累计时长。
    if (!_hadEventSinceLastHeartbeat && _retryQueue.isEmpty) return;
    _hadEventSinceLastHeartbeat = false;
    _sendRequest({'session_duration': '$duration'});
  }

  int get _sessionDurationSinceLastUpdate {
    if (_lastSessionUpdate == null) return 0;
    final now = DateTime.now();
    final seconds = now.difference(_lastSessionUpdate!).inSeconds;
    _lastSessionUpdate = now;
    return seconds;
  }

  // ---------------------------------------------------------------------------
  // 心跳定时器（每 60 秒）
  // ---------------------------------------------------------------------------

  void _startHeartbeat() {
    _stopHeartbeat();
    _heartbeatTimer = Timer.periodic(const Duration(seconds: 60), (_) {
      if (!_enabled || !_initialized) return;

      // 服务器持续不可达时，停止心跳和重试，释放资源
      if (_consecutiveFailures >= _maxConsecutiveFailures) {
        logInfo(
          _tag,
          'server unreachable ($_consecutiveFailures failures), '
          'suspending heartbeat',
        );
        _stopHeartbeat();
        _eventQueue.clear();
        _retryQueue.clear();
        _releaseResources();
        return;
      }

      _updateSession();
      _flushEvents();
      _processRetryQueue();
    });
  }

  void _stopHeartbeat() {
    _heartbeatTimer?.cancel();
    _heartbeatTimer = null;
    _delayedFlushTimer?.cancel();
    _delayedFlushTimer = null;
  }

  // ---------------------------------------------------------------------------
  // 事件队列
  // ---------------------------------------------------------------------------

  void _recordEvent(
    String key, {
    Map<String, Object>? segmentation,
    int count = 1,
    double sum = 0,
  }) {
    // 服务器已判定不可达，直接丢弃，不堆积内存
    if (_consecutiveFailures >= _maxConsecutiveFailures) return;

    final now = DateTime.now();
    final event = <String, Object?>{
      'key': key,
      'count': count,
      'timestamp': now.millisecondsSinceEpoch,
      'hour': now.hour,
      'dow': now.weekday % 7, // Countly: 0=Sunday
    };
    if (sum != 0) event['sum'] = sum;
    if (segmentation != null) event['segmentation'] = segmentation;
    _eventQueue.add(event);

    if (_eventQueue.length >= 10) {
      _flushEvents();
    } else {
      _delayedFlushTimer?.cancel();
      _delayedFlushTimer = Timer(const Duration(seconds: 30), () {
        if (_eventQueue.isNotEmpty) _flushEvents();
      });
    }
  }

  Future<void> _flushEvents() async {
    _delayedFlushTimer?.cancel();
    _delayedFlushTimer = null;

    if (_eventQueue.isEmpty || !_initialized) return;

    final events = List<Map<String, Object?>>.from(_eventQueue);
    _eventQueue.clear();
    await _sendRequest({'events': jsonEncode(events)});
  }

  // ---------------------------------------------------------------------------
  // 失败请求重试
  // ---------------------------------------------------------------------------

  void _processRetryQueue() {
    if (_retryQueue.isEmpty) return;

    final pending = List<Map<String, String>>.from(_retryQueue);
    _retryQueue.clear();

    for (final params in pending) {
      _sendRequest(params, allowRetry: false);
    }
    logInfo(_tag, 'retried ${pending.length} queued request(s)');
  }

  // ---------------------------------------------------------------------------
  // HTTP 请求（POST body）
  // ---------------------------------------------------------------------------

  Future<void> _sendRequest(
    Map<String, String> params, {
    bool allowRetry = true,
  }) async {
    if (!_initialized || !_enabled) return;

    // 惰性创建 HttpClient（熔断恢复后可能需要重建）
    _ensureHttpClient();

    try {
      final now = DateTime.now();
      final body = {
        'app_key': _appKey,
        'device_id': _deviceId,
        'timestamp': '${now.millisecondsSinceEpoch ~/ 1000}',
        'hour': '${now.hour}',
        'dow': '${now.weekday % 7}',
        'sdk_name': 'dart',
        'sdk_version': '1.0.0',
        ...params,
      };

      final uri = Uri.parse('$_serverUrl/i');
      final request = await _httpClient!.postUrl(uri);
      request.headers.set('Content-Type', 'application/x-www-form-urlencoded');

      final encoded = body.entries
          .map(
            (e) =>
                '${Uri.encodeComponent(e.key)}=${Uri.encodeComponent(e.value)}',
          )
          .join('&');
      request.write(encoded);

      final response = await request.close().timeout(
        const Duration(seconds: 10),
      );
      await response.drain<void>();

      if (response.statusCode == 200) {
        _consecutiveFailures = 0; // 成功，重置计数
      } else {
        _onSendFailed(params, allowRetry);
      }
    } catch (e) {
      logInfo(_tag, 'send failed: $e');
      _onSendFailed(params, allowRetry);
    }
  }

  void _onSendFailed(Map<String, String> params, bool allowRetry) {
    _consecutiveFailures++;
    if (allowRetry && _retryQueue.length < _maxRetryQueue) {
      _retryQueue.add(params);
    }
  }

  // ---------------------------------------------------------------------------
  // 资源管理
  // ---------------------------------------------------------------------------

  void _ensureHttpClient() {
    _httpClient ??= HttpClient()
      ..connectionTimeout = const Duration(seconds: 10)
      ..idleTimeout = const Duration(seconds: 15);
  }

  /// 释放全部网络和内存资源。
  void _releaseResources() {
    _httpClient?.close(force: true);
    _httpClient = null;
  }

  // ---------------------------------------------------------------------------
  // 设备 ID 持久化
  // ---------------------------------------------------------------------------

  Future<String> _getOrCreateDeviceId() async {
    try {
      final dir = _countlyDataDir;
      final file = File('$dir${Platform.pathSeparator}countly_device_id');
      if (file.existsSync()) {
        final id = file.readAsStringSync().trim();
        if (id.isNotEmpty) return id;
      }
      final id = _generateUuid();
      await Directory(dir).create(recursive: true);
      await file.writeAsString(id);
      return id;
    } catch (e) {
      logInfo(_tag, 'failed to persist device id: $e');
      return _generateUuid();
    }
  }

  String get _countlyDataDir {
    if (Platform.isWindows) {
      final appData =
          Platform.environment['APPDATA'] ??
          Platform.environment['USERPROFILE'];
      return '$appData${Platform.pathSeparator}FluxDown';
    }
    if (Platform.isMacOS) {
      final home = Platform.environment['HOME'] ?? '.';
      return '$home/Library/Application Support/fluxdown';
    }
    final home = Platform.environment['HOME'] ?? '.';
    return '$home${Platform.pathSeparator}.fluxdown';
  }

  String _generateUuid() {
    final rng = Random.secure();
    final bytes = List<int>.generate(16, (_) => rng.nextInt(256));
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
    final hex = bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
    return '${hex.substring(0, 8)}-${hex.substring(8, 12)}-'
        '${hex.substring(12, 16)}-${hex.substring(16, 20)}-'
        '${hex.substring(20)}';
  }

  // ---------------------------------------------------------------------------
  // 辅助属性
  // ---------------------------------------------------------------------------

  int get _runDurationSeconds =>
      DateTime.now().difference(_appStartTime).inSeconds;

  String get _cpuArchitecture {
    // Windows: PROCESSOR_ARCHITECTURE = AMD64 | ARM64 | x86
    final arch = Platform.environment['PROCESSOR_ARCHITECTURE'] ?? '';
    return switch (arch.toUpperCase()) {
      'AMD64' => 'x64',
      'ARM64' => 'arm64',
      'X86' => 'x86',
      _ => arch.isNotEmpty ? arch.toLowerCase() : 'unknown',
    };
  }

  String get _screenResolution {
    try {
      final display = PlatformDispatcher.instance.displays.firstOrNull;
      if (display != null) {
        final dpr = PlatformDispatcher.instance.views.first.devicePixelRatio;
        final w = (display.size.width / dpr).round();
        final h = (display.size.height / dpr).round();
        return '${w}x$h';
      }
    } catch (_) {}
    return '0x0';
  }
}
