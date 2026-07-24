// FluxCloud CDN 聚合配置云拉取服务 —— 单例（同 RemoteTaskService 的登录接线
// 风格：无 UI 状态，纯后台任务）。P1 §四契约：GET /api/v1/cdn/config，
// If-None-Match 条件请求 + 12h 周期刷新；失败静默保留旧值——多 CDN 聚合
// 下载本身有本地兜底默认值（内置 resolver baseline / cdn_max_nodes 自动档），
// 云端不可达不影响下载功能。
//
// 触发时机（对应契约「客户端行为」节）：
//   1. home_page 在 providers 就绪后调 [attach]：已登录则立即拉一次；
//   2. 登录状态由未登录 → 已登录（监听 CloudAuthService）；
//   3. 登录期间 12h 周期定时。
// 未登录 / 登出：停止定时器，不清空已落库的旧值（引擎侧配置是持久化的，
// 登出不代表云端配置作废，且未登录时也不该反复请求需要鉴权的接口）。
//
// 落库通道：与 [SettingsProvider] 完全一致的 [SaveConfig] 信号（见
// settings_provider.dart `_saveToRust`），Rust 侧 apply_config_key 分支
// （hub/src/actors/download_actor.rs、server/src/actor.rs）据 key 分发给
// 对应 engine setter，无需新开一条落库路径。

import 'dart:async';
import 'dart:convert';

import '../../bindings/bindings.dart';
import '../kv_store.dart';
import '../log_service.dart';
import 'cloud_auth_service.dart';
import 'cloud_client.dart';
import 'cloud_models.dart';

const _tag = 'CdnConfig';

/// ETag 持久化键前缀，按登录用户 id 隔离（同 ConfigSyncService 水位线的
/// `cloud_sync_rev.$uid` 惯例）——换号登录不会误用另一账号的缓存 ETag。
const _kEtagKeyPrefix = 'cdn_config_etag';

/// 云端拉取周期。P1 契约「12h 周期」。
const _kPeriod = Duration(hours: 12);

/// FluxCloud CDN 聚合配置云拉取服务单例。
class CdnConfigService {
  CdnConfigService._();

  static final CdnConfigService instance = CdnConfigService._();

  bool _authAttached = false;
  bool _running = false;
  Timer? _periodicTimer;

  /// 正在进行的拉取（避免登录瞬间的 attach 拉取与周期定时器重叠触发并发请求）。
  Future<void>? _inflight;

  // ── 接线 ─────────────────────────────────────────────────────────────

  /// home_page 在 providers 创建后调用一次：挂账户监听，已登录则立即启动。
  Future<void> attach() async {
    if (!_authAttached) {
      _authAttached = true;
      CloudAuthService.instance.addListener(_onAuthChanged);
    }
    if (CloudAuthService.instance.isLoggedIn) {
      start();
    }
  }

  void _onAuthChanged() {
    if (CloudAuthService.instance.isLoggedIn) {
      if (!_running) start();
    } else {
      stop();
    }
  }

  // ── 生命周期 ─────────────────────────────────────────────────────────

  /// 启动：立即拉一次 + 建立 12h 周期定时。
  void start() {
    if (_running) return;
    _running = true;
    _scheduleFetch();
    _periodicTimer?.cancel();
    _periodicTimer = Timer.periodic(_kPeriod, (_) => _scheduleFetch());
  }

  /// 停止：仅取消定时器，不清空已落库的引擎配置（见文件头说明）。
  void stop() {
    _running = false;
    _periodicTimer?.cancel();
    _periodicTimer = null;
  }

  /// SSE 收到全局 CDN 配置 revision 变更事件（`kind=cdn_config`，见
  /// ConfigSyncService._onSseLine）时立即重拉，不等 12h 周期。未登录时
  /// [_fetch] 内部会自行跳过；不影响 [start]/[stop] 的运行状态与定时器。
  void refreshNow() {
    _scheduleFetch();
  }

  void _scheduleFetch() {
    _inflight ??= _fetch().whenComplete(() => _inflight = null);
  }

  // ── 拉取 / 应用 ──────────────────────────────────────────────────────

  String? get _etagKvKey {
    final uid = CloudAuthService.instance.user?.id;
    return uid == null ? null : '$_kEtagKeyPrefix.$uid';
  }

  Future<void> _fetch() async {
    if (!CloudAuthService.instance.isLoggedIn) return;
    final kvKey = _etagKvKey;
    final etag = kvKey == null ? null : KvStore.instance.getString(kvKey);
    try {
      final result = await CloudClient.instance.fetchCdnConfig(ifNoneMatch: etag);
      if (result.notModified) {
        logInfo(_tag, 'cdn config not modified (etag hit)');
        return;
      }
      final config = result.config;
      if (config == null) return;
      if (kvKey != null && result.etag != null) {
        await KvStore.instance.setString(kvKey, result.etag!);
      }
      _apply(config);
      logInfo(
        _tag,
        'cdn config applied: revision=${config.revision} enabled=${config.enabled} '
        'maxNodes=${config.maxNodes} resolvers=${config.resolvers.length}',
      );
    } catch (e, stack) {
      // 失败静默：保留本地已落库的旧值，不向上抛出、不弹 UI 提示。
      logError(_tag, 'fetch cdn config failed, keeping last value', e, stack);
    }
  }

  /// 写入引擎 config 表：走 [SaveConfig] 信号，Rust 侧 apply_config_key 分发
  /// 给 `set_cdn_resolver_endpoints` / `set_cdn_cloud_max_nodes` /
  /// `set_cdn_ecs_subnets` / `set_cdn_hints_base`（P2 §五扩展）。
  void _apply(CdnConfig config) {
    if (config.enabled) {
      // 对象形式 `[{"url":...,"ecs":bool}]`：保留云端下发的 ECS 标志——引擎
      // 对纯字符串数组（旧格式）与对象数组均兼容，向后兼容。
      final endpoints = jsonEncode(
        config.resolvers.map((r) => {'url': r.url, 'ecs': r.ecs}).toList(),
      );
      SaveConfig(key: 'cdn_resolver_endpoints', value: endpoints).sendSignalToRust();
      SaveConfig(
        key: 'cdn_cloud_max_nodes',
        value: config.maxNodes.clamp(0, 8).toString(),
      ).sendSignalToRust();
      final ecsSubnets = jsonEncode(config.ecsSubnets.map((s) => s.subnet).toList());
      SaveConfig(key: 'cdn_ecs_subnets', value: ecsSubnets).sendSignalToRust();
      // hints 与聚合下载同源，走 CloudClient 当前生效的服务地址。
      SaveConfig(key: 'cdn_hints_base', value: CloudApiConfig.baseUrl).sendSignalToRust();
    } else {
      // 套餐未开通聚合能力：回退内置 resolver baseline，云端上限归零
      // （不再对本地/自动上限设限）；ECS 先验与 hints 查询一并禁用。
      SaveConfig(key: 'cdn_resolver_endpoints', value: '[]').sendSignalToRust();
      SaveConfig(key: 'cdn_cloud_max_nodes', value: '0').sendSignalToRust();
      SaveConfig(key: 'cdn_ecs_subnets', value: '[]').sendSignalToRust();
      SaveConfig(key: 'cdn_hints_base', value: '').sendSignalToRust();
    }
  }
}
