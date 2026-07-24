// FluxCloud 配置同步客户端 —— 单例 + ChangeNotifier（同 CloudAuthService/
// UpdateService 的单例风格），按契约 v1「客户端同步流程」节实现全链路：
// pull / push / SSE 事件通知 / 防抖批量推送 / 快照+脏键防回环 / 断线退避重连。
//
// 推送通道用 Server-Sent Events（GET /sync/events，见契约），本类直接持有独立
// [HttpClient] 长驻读取事件流，不经 [CloudClient]——CloudClient._request 给每次
// 请求套了 15s 超时，SSE 是长驻连接不能复用那条路径。pull/push 是短请求，走
// CloudClient 默认超时即可。
//
// 状态机：disabled → connecting → syncing ↔ synced；error 可重试（退避
// 5s/15s/60s/5min 封顶），403 sync_device_limit/sync_device_untrusted 除外——
// 那两种不自动重试，等用户手动「立即同步」或重新登录。
//
// 防回环三道闸（修复「本机连点设置被自己的回显翻回去 / 误报来自其他设备」）：
// 1. push 回包 revision 恰为水位线+1 ⇒ 快进水位线，自己的写不再触发回显 pull；
// 2. pull 应用时跳过脏键（本机未推送的编辑优先），且只有 deviceId ≠ 本机的
//    条目才计入「来自其他设备的配置同步」toast；
// 3. 全部同步入口经 _gate 串行，杜绝并发 pull 交错应用新旧批次远端值。
// 服务端配合：值未变的 push 不写库不涨 revision 不广播（FluxCloud sync.rs）。

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../../i18n/locale_provider.dart';
import '../../models/settings_provider.dart';
import '../../theme/theme_provider.dart';
import '../kv_store.dart';
import '../log_service.dart';
import 'cdn_config_service.dart';
import 'cloud_auth_service.dart';
import 'cloud_client.dart';
import 'cloud_models.dart';
import 'device_identity.dart';
import 'sync_catalog.dart';

const _tag = 'ConfigSync';
const _kEnabledKey = 'cloud_sync_enabled';
const _kEventsPath = '/api/v1/sync/events';
const _kSseIdleTimeout = Duration(seconds: 75);
const _kDebounce = Duration(milliseconds: 600);
const _kRetryDelays = [
  Duration(seconds: 5),
  Duration(seconds: 15),
  Duration(seconds: 60),
  Duration(minutes: 5),
];

enum CloudSyncStatus { disabled, connecting, syncing, synced, error }

/// FluxCloud 配置同步客户端单例。home_page.dart 在 providers 就绪后调 [attach]
/// 一次；设置页「配置同步」块经 ListenableBuilder 监听本类展示状态与开关。
class ConfigSyncService extends ChangeNotifier {
  ConfigSyncService._();

  static final ConfigSyncService instance = ConfigSyncService._();

  CloudSyncStatus _status = CloudSyncStatus.disabled;
  CloudSyncStatus get status => _status;

  String? _lastError;
  String? get lastError => _lastError;

  DateTime? _lastSyncAt;
  DateTime? get lastSyncAt => _lastSyncAt;

  bool _enabled = KvStore.instance.getBool(_kEnabledKey) ?? true;
  bool get enabled => _enabled;

  /// 来自其他设备的条目被实际应用后回调（count>0 才触发），供 home_page 弹
  /// toast。本机自身推送的回显（重启补拉旧值等场景）静默应用，不计数。
  void Function(int count, String? deviceName)? onRemoteApplied;

  List<SyncEntry>? _catalog;
  Map<String, SyncEntry>? _catalogByKey;
  bool _baselined = false;
  bool _authListenerAttached = false;

  /// key → jsonEncode(value) 快照，用于本地变更 diff 与远端应用防回环。
  final Map<String, String> _snapshot = {};
  final Set<String> _dirty = {};
  Timer? _debounceTimer;
  Timer? _retryTimer;
  int _retryAttempt = 0;
  bool _applyingRemote = false;
  bool _stopped = true;

  /// 同步操作串行门：SSE 触发的 pull、防抖 push、手动同步、断线重连一律排队
  /// 执行。并发 pull 会交错应用不同批次的远端值（主题曾在 34ms 内 dark/light
  /// 来回翻转），串行化后天然消除。
  Future<void> _gate = Future.value();

  HttpClient? _sseHttp;
  StreamSubscription<String>? _sseSub;
  Timer? _sseWatchdog;

  // ── 接线 ─────────────────────────────────────────────────────────────

  /// home_page.dart 在 providers 创建后调用一次。等 [SettingsProvider.loaded]
  /// 后建立基线快照，再按登录/开关状态决定是否启动。
  Future<void> attach({
    required SettingsProvider settings,
    required ThemeProvider theme,
    required LocaleNotifier locale,
  }) async {
    _catalog = buildSyncCatalog(settings: settings, theme: theme, locale: locale);
    _catalogByKey = {for (final e in _catalog!) e.key: e};

    if (!_authListenerAttached) {
      _authListenerAttached = true;
      CloudAuthService.instance.addListener(_onAuthChanged);
    }

    settings.addListener(_onLocalChange);
    theme.addListener(_onLocalChange);
    locale.addListener(_onLocalChange);

    if (settings.loaded) {
      await _bootstrap();
    } else {
      void onLoaded() {
        if (!settings.loaded) return;
        settings.removeListener(onLoaded);
        unawaited(_bootstrap());
      }

      settings.addListener(onLoaded);
    }
  }

  Future<void> _bootstrap() async {
    for (final entry in _catalog!) {
      _snapshot[entry.key] = _encode(entry.read());
    }
    _baselined = true;
    if (_enabled && CloudAuthService.instance.isLoggedIn) {
      await start();
    } else {
      _status = CloudSyncStatus.disabled;
      notifyListeners();
    }
  }

  void _onAuthChanged() {
    if (!_baselined) return;
    if (CloudAuthService.instance.isLoggedIn) {
      if (_enabled) unawaited(start());
    } else {
      stop();
    }
  }

  // ── 开关 ─────────────────────────────────────────────────────────────

  Future<void> setEnabled(bool value) async {
    if (_enabled == value) return;
    _enabled = value;
    await KvStore.instance.setBool(_kEnabledKey, value);
    if (!value) {
      stop();
      return;
    }
    if (_baselined && CloudAuthService.instance.isLoggedIn) {
      await start();
    } else {
      notifyListeners();
    }
  }

  /// 所有同步入口经此串行执行；入口自身已捕获异常，链上再兜底一次防断链。
  Future<void> _serialized(Future<void> Function() op) {
    final run = _gate.then((_) => op());
    _gate = run.then((_) {}, onError: (_) {});
    return run;
  }

  // ── 生命周期 ─────────────────────────────────────────────────────────

  Future<void> start() async {
    if (!_enabled || !CloudAuthService.instance.isLoggedIn || _catalog == null) {
      _status = CloudSyncStatus.disabled;
      notifyListeners();
      return;
    }
    _stopped = false;
    _retryAttempt = 0;
    _cancelRetryTimer();
    _loadDirty();
    // 重新基线化快照：登出/开关关闭期间本地可能已变，快照若过期，pull 的
    // remote-vs-snapshot diff 就不再等价 remote-vs-current，行为变成碰运气。
    // 以「当前本地值」为基线，重连后语义确定：云端有的键云端胜，云端没有的键
    // 保持本地（首次/resync 场景由 _pull 标脏播种回云端）。
    for (final entry in _catalog!) {
      _snapshot[entry.key] = _encode(entry.read());
    }
    _status = CloudSyncStatus.connecting;
    notifyListeners();
    await _syncAndConnect();
  }

  /// 登出 / 开关关闭时调用：断流、停止防抖与重试、状态归 disabled。本地设置
  /// 值不受影响（契约「开关关→断流停监听，本地不动」）。
  void stop() {
    _stopped = true;
    _cancelRetryTimer();
    _debounceTimer?.cancel();
    _debounceTimer = null;
    _closeSse();
    _status = CloudSyncStatus.disabled;
    _lastError = null;
    notifyListeners();
  }

  /// 手动触发（设置页「立即同步」按钮）：pull + push 脏键；若 SSE 当前未连接
  /// （例如此前因设备超限等致命错误从未连上）则顺带补连一次。
  Future<void> syncNow() async {
    if (!_enabled || !CloudAuthService.instance.isLoggedIn || _catalog == null) return;
    _cancelRetryTimer();
    _status = CloudSyncStatus.syncing;
    notifyListeners();
    await _serialized(() async {
      try {
        await _pull();
        if (_stopped) return;
        if (_dirty.isNotEmpty) await _pushDirtyNow();
        if (_stopped) return;
        if (_sseHttp == null) await _connectSse();
        if (_stopped) return;
        _onSyncSucceeded();
      } catch (e, stack) {
        if (_stopped) return;
        logError(_tag, 'syncNow failed', e, stack);
        _handleSyncFailure(e, _syncAndConnect);
      }
    });
  }

  // ── 启动 / 重连统一路径 ───────────────────────────────────────────────

  /// pull → （首次/resync）标脏 → push 脏键 → 建 SSE 流 → synced。
  /// 也是 SSE 断线/超时重连的统一入口——契约要求"任何连接失败都走这条退避
  /// 路径，重试前先 pull（顺带经 CloudClient._authed 刷新 token）"。
  Future<void> _syncAndConnect() => _serialized(() async {
    if (_stopped) return;
    try {
      _status = CloudSyncStatus.syncing;
      notifyListeners();
      await _pull();
      if (_stopped) return;
      if (_dirty.isNotEmpty) await _pushDirtyNow();
      if (_stopped) return;
      await _connectSse();
      if (_stopped) {
        _closeSse();
        return;
      }
      _onSyncSucceeded();
    } catch (e, stack) {
      if (_stopped) return;
      logError(_tag, 'sync/connect failed', e, stack);
      _handleSyncFailure(e, _syncAndConnect);
    }
  });

  void _onSyncSucceeded() {
    _status = CloudSyncStatus.synced;
    _lastError = null;
    _lastSyncAt = DateTime.now();
    _retryAttempt = 0;
    notifyListeners();
  }

  void _handleSyncFailure(Object error, Future<void> Function() retry) {
    if (_stopped) return;
    final fatal = _fatalMessage(error);
    _lastError = fatal ?? _friendlyMessage(error);
    _status = CloudSyncStatus.error;
    notifyListeners();
    if (fatal != null) return; // 403 设备超限/未信任：不自动重试，等手动/重登
    _scheduleRetry(retry);
  }

  String? _fatalMessage(Object error) {
    if (error is CloudApiException) {
      if (error.code == 'sync_device_limit') return currentS.cloudSyncErrorDeviceLimit;
      if (error.code == 'sync_device_untrusted') return currentS.cloudSyncErrorDeviceUntrusted;
    }
    return null;
  }

  String _friendlyMessage(Object error) {
    if (error is CloudApiException && error.message.isNotEmpty) return error.message;
    return currentS.cloudSyncErrorNetwork;
  }

  void _scheduleRetry(Future<void> Function() retry) {
    _cancelRetryTimer();
    final idx = _retryAttempt.clamp(0, _kRetryDelays.length - 1);
    final delay = _kRetryDelays[idx];
    _retryAttempt = idx + 1;
    _retryTimer = Timer(delay, () {
      if (_stopped) return;
      unawaited(retry());
    });
  }

  void _cancelRetryTimer() {
    _retryTimer?.cancel();
    _retryTimer = null;
  }

  // ── pull / push ──────────────────────────────────────────────────────

  Future<void> _pull() async {
    final deviceId = DeviceIdentity.deviceId();
    final since = _watermark();
    final result = await CloudClient.instance.syncPull(since: since, deviceId: deviceId);

    var applied = 0;
    String? sourceDeviceName;
    _applyingRemote = true;
    try {
      for (final item in result.items) {
        final entry = _catalogByKey![item.key];
        if (entry == null || item.deleted) continue;
        // 脏键让位：该键在本机有尚未推送的编辑，pull 回来的必然是旧值（自己
        // 上一轮的回显或别机更早的写入），应用它会把刚点的开关翻回去、把
        // 「跟随系统」踢回 light/dark。跳过且不动快照——快照必须始终等于本地
        // 当前值；脏键随后 push 成为服务端最新，收敛方向正确。
        if (_dirty.contains(item.key)) continue;
        final encoded = _encode(item.value);
        if (_snapshot[item.key] == encoded) continue;
        // 先更新快照再 apply：apply 触发的 provider notifyListeners 会经
        // _onLocalChange 重新 diff 全目录，快照已经等于新值即可判定"未变"。
        _snapshot[item.key] = encoded;
        // 逐条防护：value 是同账号其他客户端的输入，单条毒值若让 apply 抛错，
        // 会中断整轮 pull 且水位线不前进——重试永远卡在同一条，所有设备的同步
        // 都会被卡死。跳过该条并记日志，让其余条目与水位线正常推进。
        try {
          entry.apply(item.value);
          // 只统计真正来自其他设备的条目：本机条目的回显静默应用，不弹
          // 「来自其他设备的配置同步」toast。
          if (item.deviceId != deviceId) {
            applied++;
            sourceDeviceName = item.deviceName ?? sourceDeviceName;
          }
        } catch (e, stack) {
          logError(_tag, 'apply remote item failed, skipped: ${item.key}', e, stack);
        }
      }
    } finally {
      _applyingRemote = false;
    }

    // 首次同步（since==0）或强制重同步（resync）：本地目录中云端没有的键
    // 全部标脏重传，把本机当前值播种回云端。
    if (since == 0 || result.resync) {
      final missing = _catalogByKey!.keys.toSet()
        ..removeAll(result.items.map((e) => e.key));
      _markDirty(missing);
    }
    await _setWatermark(result.revision);

    if (applied > 0) onRemoteApplied?.call(applied, sourceDeviceName);
  }

  Future<void> _pushDirtyNow() async {
    if (_dirty.isEmpty) return;
    final keys = _dirty.toList();
    final items = <Map<String, dynamic>>[];
    for (final key in keys) {
      final entry = _catalogByKey![key];
      if (entry == null) continue;
      items.add({'key': key, 'value': entry.read()});
    }
    if (items.isEmpty) {
      _dirty.clear();
      await _persistDirty();
      return;
    }
    // 契约限额单批 ≤128 条；目录固定 41 键，永远不会超限，无需分批。
    final before = _watermark();
    final revision = await CloudClient.instance
        .syncPush(deviceId: DeviceIdentity.deviceId(), items: items);
    _dirty.removeAll(keys);
    await _persistDirty();
    // 自回显消除：本次 push 恰好把 revision 推进一格 ⇒ 期间无其他设备写入，
    // 直接快进水位线到回包值，随后 SSE 送达的同号事件（≤ 水位线）被忽略，
    // 不再发起纯回显 pull——那轮 pull 与用户的下一次点击竞争，正是「点一下
    // 被翻回去」的根源之一。间隔 >1 说明有并发外部写入，水位线不动，照常
    // 走 SSE→pull 拉取别机变更。
    if (revision == before + 1 && _watermark() == before) {
      await _setWatermark(revision);
    }
  }

  // ── 本地变更（防抖批量推送） ─────────────────────────────────────────

  void _onLocalChange() {
    if (!_baselined || _applyingRemote) return;
    if (!_enabled || !CloudAuthService.instance.isLoggedIn) return;
    final changed = <String>[];
    for (final entry in _catalog!) {
      final encoded = _encode(entry.read());
      if (_snapshot[entry.key] != encoded) {
        _snapshot[entry.key] = encoded;
        changed.add(entry.key);
      }
    }
    if (changed.isEmpty) return;
    _markDirty(changed);
    _debounceTimer?.cancel();
    _debounceTimer = Timer(_kDebounce, () => unawaited(_flushDirty()));
  }

  Future<void> _flushDirty() => _serialized(() async {
    if (_stopped || !_enabled || !CloudAuthService.instance.isLoggedIn) return;
    _status = CloudSyncStatus.syncing;
    notifyListeners();
    try {
      await _pushDirtyNow();
      if (_stopped) return;
      _onSyncSucceeded();
    } catch (e, stack) {
      if (_stopped) return;
      logError(_tag, 'debounced push failed', e, stack);
      // 推送失败保留脏键，走标准退避重试（同一条路径也会顺带重新拉取/重连）。
      _handleSyncFailure(e, _syncAndConnect);
    }
  });

  void _markDirty(Iterable<String> keys) {
    if (keys.isEmpty) return;
    _dirty.addAll(keys);
    unawaited(_persistDirty());
  }

  // ── 水位线 / 脏集合持久化（按登录用户 id 隔离） ─────────────────────────

  String? get _uid => CloudAuthService.instance.user?.id;

  int _watermark() {
    final uid = _uid;
    if (uid == null) return 0;
    return int.tryParse(KvStore.instance.getString('cloud_sync_rev.$uid') ?? '') ?? 0;
  }

  Future<void> _setWatermark(int revision) async {
    final uid = _uid;
    if (uid == null) return;
    await KvStore.instance.setString('cloud_sync_rev.$uid', revision.toString());
  }

  void _loadDirty() {
    _dirty.clear();
    final uid = _uid;
    if (uid == null) return;
    final raw = KvStore.instance.getString('cloud_sync_dirty.$uid');
    if (raw == null) return;
    try {
      final list = jsonDecode(raw) as List<dynamic>;
      _dirty.addAll(list.cast<String>());
    } catch (e, stack) {
      logError(_tag, 'failed to load persisted dirty set', e, stack);
    }
  }

  Future<void> _persistDirty() async {
    final uid = _uid;
    if (uid == null) return;
    await KvStore.instance.setString('cloud_sync_dirty.$uid', jsonEncode(_dirty.toList()));
  }

  String _encode(dynamic value) => jsonEncode(value);

  // ── SSE 事件流 ───────────────────────────────────────────────────────

  /// GET /sync/events：长驻响应体，不设超时。连接建立后服务端立即回发一条
  /// 当前 revision（内建 catch-up），此后每次该用户数据变更再发一条；每 30s
  /// 一条注释心跳行。本方法只建流；收到的行交给 [_onSseLine] 处理。
  Future<void> _connectSse() async {
    _closeSse();
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
    final deviceId = Uri.encodeQueryComponent(DeviceIdentity.deviceId());
    final uri = Uri.parse('${CloudApiConfig.baseUrl}$_kEventsPath?deviceId=$deviceId');
    HttpClientResponse res;
    try {
      final req = await client.getUrl(uri);
      req.headers.set('Accept', 'text/event-stream');
      req.headers.set('Authorization', 'Bearer ${CloudClient.instance.accessToken}');
      res = await req.close();
    } catch (e) {
      client.close(force: true);
      throw CloudApiException(code: 'network_error', message: 'SSE 连接失败：$e', status: 0);
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
    _resetSseWatchdog();
    _sseSub = res
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen(_onSseLine, onDone: _onSseDisconnected, onError: (_) => _onSseDisconnected());
  }

  /// 解析 `data: {...}` 行：无 `kind` 字段沿用既有 per-user sync revision
  /// 处理；`kind: "cdn_config"` 是合入本流的全局 CDN 配置变更事件（P2 §九，
  /// FluxDownCloud 管理端保存 CDN 设置后 bump），与本账号 sync revision 是
  /// 两套独立计数——不参与水位线比较，只触发 [CdnConfigService] 立即重拉。
  /// `:` 开头的注释心跳与空行仅用于喂看门狗。
  void _onSseLine(String line) {
    _resetSseWatchdog();
    if (!line.startsWith('data:')) return;
    final payload = line.substring('data:'.length).trim();
    if (payload.isEmpty) return;
    try {
      final json = jsonDecode(payload) as Map<String, dynamic>;
      if (json['kind'] == 'cdn_config') {
        logInfo(_tag, 'cdn config revision changed via sse, refreshing');
        CdnConfigService.instance.refreshNow();
        return;
      }
      final revision = (json['revision'] as num?)?.toInt();
      // 严格相等才跳过：push 回包快进后自回显事件恰等于水位线；revision
      // 倒退（如管理端清空同步数据后 publish(0)）必须照常 pull 以触发 resync。
      if (revision == null || revision == _watermark()) return;
      unawaited(_onRevisionChanged(revision));
    } catch (e, stack) {
      logError(_tag, 'sse payload parse failed: $payload', e, stack);
    }
  }

  Future<void> _onRevisionChanged([int? revision]) => _serialized(() async {
    if (_stopped) return;
    // 在串行门里排队期间，水位线可能已被前一轮 pull 或 push 快进推到本事件
    // 的 revision——那就没什么可拉的了。倒退事件（管理端清空）与失败重试
    //（不带参）恒重拉。
    if (revision != null && revision == _watermark()) return;
    try {
      _status = CloudSyncStatus.syncing;
      notifyListeners();
      await _pull();
      if (_stopped) return;
      _onSyncSucceeded();
    } catch (e, stack) {
      if (_stopped) return;
      logError(_tag, 'pull on sse event failed', e, stack);
      _handleSyncFailure(e, _onRevisionChanged);
    }
  });

  void _resetSseWatchdog() {
    _sseWatchdog?.cancel();
    _sseWatchdog = Timer(_kSseIdleTimeout, _onSseTimeout);
  }

  void _onSseTimeout() {
    logInfo(_tag, 'sse idle for ${_kSseIdleTimeout.inSeconds}s, reconnecting');
    _onSseDisconnected();
  }

  void _onSseDisconnected() {
    if (_stopped) return;
    _closeSse();
    _status = CloudSyncStatus.connecting;
    notifyListeners();
    _scheduleRetry(_syncAndConnect);
  }

  void _closeSse() {
    _sseWatchdog?.cancel();
    _sseWatchdog = null;
    _sseSub?.cancel();
    _sseSub = null;
    try {
      _sseHttp?.close(force: true);
    } catch (_) {}
    _sseHttp = null;
  }
}
