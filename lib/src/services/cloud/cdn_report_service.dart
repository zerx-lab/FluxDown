// FluxCloud CDN 众包遥测上报服务 —— 单例（同 CdnConfigService 的接线/周期范式：
// 无 UI 状态，纯后台任务）。P2 §五契约：登录态下每 30min + 启动时读取引擎侧
// 缓冲的待上传样本（config 键 `cdn_pending_reports`，JSON 数组），按 ≤64 条
// 分批 POST /api/v1/cdn/report；全部批次成功后写空串清空，任一批次失败则
// 整轮静默保留（下次周期/启动重试，绝不重复上报——见 telemetry.rs 文件头
// 的"读取与清空之间新增样本会丢、不会重复"设计）。
//
// 常开：本服务与引擎侧采样均无用户开关，登录后自动上报——语义对齐既有
// analytics（匿名使用统计）：仅上报域名、节点 IP、连接耗时与吞吐，不含
// URL、文件名等任何内容信息。
//
// 读取通道：复用应用启动时已有的 `RequestConfig` → `ConfigLoaded` 批量读取
// 信号对（见 SettingsProvider.requestConfig()/ComponentController 的用法），
// 每次上报前主动发一次 RequestConfig 触发引擎重新落盘归并后的最新快照，
// 避免读到 12h 前的陈旧缓存；不新开任何 Rinf 信号。
// 写入通道：与 CdnConfigService 一致的 SaveConfig 信号，Rust 侧
// apply_config_key（hub/src/actors/download_actor.rs、server/src/actor.rs）
// 收到 `cdn_pending_reports` 空串即转调 `clear_cdn_pending_reports()`。
//
// 触发时机：
//   1. home_page 在 providers 就绪后调 [attach]：已登录则立即上报一次；
//   2. 登录状态由未登录 → 已登录（监听 CloudAuthService）；
//   3. 登录期间 30min 周期定时（兜底：覆盖失败重试与仅有失败样本的场景）；
//   4. 任务下载完成事件（[notifyTaskCompleted]，10s 去抖合批）——主同步
//      通道：任务一完成、样本齐了就上传，不必等周期。依赖引擎侧
//      RequestConfig 处理点先 flush 内存缓冲（telemetry::flush），上报
//      才能读到本次任务的全部样本。
// 未登录 / 登出：停止定时器，不清空引擎侧已缓冲的样本（登出不代表放弃
// 已采集的遥测，下次登录继续上报；且未登录时该接口本就需要鉴权）。

import 'dart:async';
import 'dart:convert';

import 'package:rinf/rinf.dart';

import '../../bindings/bindings.dart';
import '../log_service.dart';
import 'cloud_auth_service.dart';
import 'cloud_client.dart';

const _tag = 'CdnReport';

/// 上报周期。P2 契约「30min」。
const _kPeriod = Duration(minutes: 30);

/// 单批上传上限（服务端契约：≤64 条/次），超量本端分批。
const _kBatchSize = 64;

/// 批间间隔：服务端 `POST /cdn/report` 有 per-user 1s 限频窗口
/// （FluxCloud `CDN_REPORT_RATE_LIMIT_WINDOW`），发完一批后不等待就发下一批
/// 会被 429 拒绝并中止整轮。留 0.2s 余量吸收时钟偏差。
const _kInterBatchGap = Duration(milliseconds: 1200);
/// 任务完成事件的上报去抖：合并短时间内连续完成的多个任务（批量下载），
/// 也给引擎侧段完成样本的收尾留出余量。
const _kCompletionDebounce = Duration(seconds: 10);

/// 等待一次 RequestConfig 往返的超时保护——宿主异常时不让上报任务永久挂起。
const _kConfigRequestTimeout = Duration(seconds: 10);

/// FluxCloud CDN 众包遥测上报服务单例。
class CdnReportService {
  CdnReportService._();

  static final CdnReportService instance = CdnReportService._();

  bool _authAttached = false;
  bool _running = false;
  Timer? _periodicTimer;
  Timer? _completionDebounce;
  StreamSubscription<RustSignalPack<ConfigLoaded>>? _configSub;

  /// 当前待响应的 RequestConfig 请求（同一时刻至多一个上报任务在跑，见
  /// [_inflight]，因此无需按请求 id 区分多路并发）。
  Completer<List<ConfigEntry>>? _pendingConfigRequest;

  /// 正在进行的上报（避免登录瞬间的 attach 上报与周期定时器重叠触发并发上传）。
  Future<void>? _inflight;

  // ── 接线 ─────────────────────────────────────────────────────────────

  /// home_page 在 providers 创建后调用一次：挂账户监听 + 订阅 ConfigLoaded，
  /// 已登录则立即启动。
  Future<void> attach() async {
    _configSub ??= ConfigLoaded.rustSignalStream.listen(_onConfigLoaded);
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

  /// 启动：立即上报一次 + 建立 30min 周期定时。
  void start() {
    if (_running) return;
    _running = true;
    _scheduleDrain();
    _periodicTimer?.cancel();
    _periodicTimer = Timer.periodic(_kPeriod, (_) => _scheduleDrain());
  }

  /// 停止：仅取消定时器，不清空引擎侧已缓冲的样本（见文件头说明）。
  void stop() {
    _running = false;
    _periodicTimer?.cancel();
    _periodicTimer = null;
    _completionDebounce?.cancel();
    _completionDebounce = null;
  }

  /// 任务下载完成 → 10s 去抖后上报一轮（事件驱动主通道）。
  /// 未登录/未启动时忽略：样本留在引擎缓冲，登录后由 start() 上报。
  void notifyTaskCompleted() {
    if (!_running) return;
    _completionDebounce?.cancel();
    _completionDebounce = Timer(_kCompletionDebounce, _scheduleDrain);
  }

  void _scheduleDrain() {
    _inflight ??= _drain().whenComplete(() => _inflight = null);
  }

  // ── 配置读取（复用 RequestConfig/ConfigLoaded，不新开信号）──────────────

  void _onConfigLoaded(RustSignalPack<ConfigLoaded> pack) {
    final completer = _pendingConfigRequest;
    if (completer == null || completer.isCompleted) return;
    _pendingConfigRequest = null;
    completer.complete(pack.message.entries);
  }

  Future<List<ConfigEntry>> _requestConfigEntries() {
    final completer = Completer<List<ConfigEntry>>();
    _pendingConfigRequest = completer;
    const RequestConfig().sendSignalToRust();
    return completer.future.timeout(
      _kConfigRequestTimeout,
      onTimeout: () {
        if (identical(_pendingConfigRequest, completer)) {
          _pendingConfigRequest = null;
        }
        return const <ConfigEntry>[];
      },
    );
  }

  // ── 上报 / 上传 ──────────────────────────────────────────────────────

  Future<void> _drain() async {
    if (!CloudAuthService.instance.isLoggedIn) return;

    final entries = await _requestConfigEntries();
    String? raw;
    for (final e in entries) {
      if (e.key == 'cdn_pending_reports') {
        raw = e.value;
        break;
      }
    }
    if (raw == null || raw.isEmpty) return;

    List<dynamic> samples;
    try {
      final decoded = jsonDecode(raw);
      if (decoded is! List) throw const FormatException('not a JSON array');
      samples = decoded;
    } catch (e, stack) {
      logError(_tag, 'cdn_pending_reports is not a JSON array, dropping', e, stack);
      return;
    }
    if (samples.isEmpty) return;

    try {
      for (var i = 0; i < samples.length; i += _kBatchSize) {
        if (i > 0) await Future.delayed(_kInterBatchGap);
        final end = (i + _kBatchSize < samples.length) ? i + _kBatchSize : samples.length;
        final batch = samples.sublist(i, end).cast<Map<String, dynamic>>();
        await CloudClient.instance.reportCdnSamples(batch);
      }
      // 全部批次成功：写空串清空（Rust 侧 apply_config_key 空值分支转调
      // clear_cdn_pending_reports()）。
      SaveConfig(key: 'cdn_pending_reports', value: '').sendSignalToRust();
      logInfo(_tag, 'drained ${samples.length} cdn telemetry sample(s)');
    } catch (e, stack) {
      // 失败静默：整轮保留待下次周期/启动重试，不向上抛出、不弹 UI 提示。
      logError(_tag, 'cdn report upload failed, keeping pending samples', e, stack);
    }
  }
}
