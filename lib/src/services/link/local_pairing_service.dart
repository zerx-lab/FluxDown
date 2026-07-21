// FluxDown 本地设备互联（局域网配对）客户端服务 —— 单例 + ChangeNotifier
// （同 CloudAuthService/RemoteTaskService 的单例风格）。
//
// 与 FluxCloud 账户体系无关：不登录账号，双方在同一局域网内即可直接配对，
// 数据面/控制面均走 Rust 端 LinkManager（native/engine/src/link/），本服务
// 只负责：
//   - 发 LinkCommand（发现/探测/配对/名册管理）；
//   - 收 LinkEvent，按 kind 分发更新本地状态并 notifyListeners；
//   - 把生成信号类型转换为 [LocalDevice]/[LocalDiscoveredPeer]/
//     [PairingChallenge] 等领域模型（见 link_models.dart），UI 层不直接
//     依赖 bindings 生成类型。
//
// 传输方式的可扩展性说明见 link_transport.dart（v1 只做局域网直连）。

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:rinf/rinf.dart';

import '../../bindings/bindings.dart';
import '../log_service.dart';
import 'link_models.dart';

const _tag = 'LocalPairing';

/// 本地设备互联服务单例。宿主页面在 providers 就绪后调 [attach] 一次
/// （同 RemoteTaskService.attach 的接线时机）。
class LocalPairingService extends ChangeNotifier {
  LocalPairingService._();

  static final LocalPairingService instance = LocalPairingService._();

  bool _attached = false;
  StreamSubscription<RustSignalPack<LinkEvent>>? _sub;

  /// 局域网内发现的未配对设备（发现阶段增量 upsert，按 [LocalDiscoveredPeer.dedupeKey] 去重）。
  List<LocalDiscoveredPeer> _discoveredPeers = const [];
  List<LocalDiscoveredPeer> get discoveredPeers => _discoveredPeers;

  /// 已配对（受信任）的本地设备名册。
  List<LocalDevice> _localDevices = const [];
  List<LocalDevice> get localDevices => _localDevices;

  /// 是否已有至少一台已配对设备（供侧栏/设置页判断是否展示本地设备区）。
  bool get hasLocalDevices => _localDevices.isNotEmpty;

  /// 当前待用户核验的配对挑战（SAS 短数字），为 null 表示当前没有进行中的配对。
  PairingChallenge? _pendingChallenge;
  PairingChallenge? get pendingChallenge => _pendingChallenge;

  /// 最近一次错误消息（`LinkEvent{kind:"error"}`），供 UI 弹 toast/内联提示。
  String? _lastError;
  String? get lastError => _lastError;

  /// 本机当前生成的配对码（`LinkEvent{kind:"code"}`），供本机作为「被配对方」
  /// 时展示给用户在对端输入。
  String? _generatedCode;
  String? get generatedCode => _generatedCode;

  /// 是否正在进行局域网发现。
  bool _discovering = false;
  bool get discovering => _discovering;

  // ── 接线 ─────────────────────────────────────────────────────────────

  /// 宿主页面在 providers 创建后调用一次：订阅 LinkEvent 信号流。幂等。
  Future<void> attach() async {
    if (_attached) return;
    _attached = true;
    _startListening();
  }

  void _startListening() {
    _sub = LinkEvent.rustSignalStream.listen(_onLinkEvent);
  }

  // ── Dart → Rust 命令 ─────────────────────────────────────────────────

  /// 开始局域网发现（mDNS/广播，具体机制由 Rust 端实现）。
  void startDiscovery() {
    _discovering = true;
    // 进入/重开本地配对：清掉上一轮遗留的错误与挑战态，避免弹窗复用时显示旧内容。
    _lastError = null;
    _pendingChallenge = null;
    notifyListeners();
    _send(action: 'startDiscovery');
  }

  /// 停止局域网发现。
  void stopDiscovery() {
    _discovering = false;
    notifyListeners();
    _send(action: 'stopDiscovery');
  }

  /// 探测指定地址是否为可配对的 FluxDown 设备（手动输入地址场景）。
  void probe({required String host, required int port}) =>
      _send(action: 'probe', host: host, port: port);

  /// 发起配对：向目标设备发送本机身份 + 用户输入的配对码，等待对端下发
  /// `LinkEvent{kind:"pairingChallenge"}`。
  void beginPairing({
    required String host,
    required int port,
    required String code,
  }) {
    _lastError = null;
    notifyListeners();
    _send(action: 'beginPairing', host: host, port: port, code: code);
  }

  /// 确认/拒绝当前挂起的配对挑战（SAS 核验通过后调用）。没有挂起挑战时忽略。
  void confirmPairing(bool accept) {
    final challenge = _pendingChallenge;
    if (challenge == null) {
      logInfo(_tag, 'confirmPairing skipped: no pending challenge');
      return;
    }
    _send(action: 'confirmPairing', token: challenge.token, accept: accept);
  }

  /// 生成本机配对码（本机作为被配对方时调用），结果经
  /// `LinkEvent{kind:"code"}` 回流到 [generatedCode]。
  void generateCode() => _send(action: 'generateCode');

  /// 刷新已配对设备名册。
  void refreshDevices() => _send(action: 'listDevices');

  /// 解除与指定设备的配对关系。
  void removeDevice(String fingerprint) =>
      _send(action: 'removeDevice', fingerprint: fingerprint);

  void _send({
    required String action,
    String host = '',
    int port = 0,
    String code = '',
    String token = '',
    bool accept = false,
    String fingerprint = '',
  }) {
    LinkCommand(
      action: action,
      host: host,
      port: port,
      code: code,
      token: token,
      accept: accept,
      fingerprint: fingerprint,
    ).sendSignalToRust();
  }

  // ── Rust → Dart 事件分发 ─────────────────────────────────────────────

  void _onLinkEvent(RustSignalPack<LinkEvent> pack) {
    final event = pack.message;
    switch (event.kind) {
      case 'code':
        _generatedCode = event.code;
        notifyListeners();
        break;
      case 'discovered':
        _upsertDiscovered(event.discovered);
        break;
      case 'pairingChallenge':
        _pendingChallenge = PairingChallenge.fromEvent(event);
        notifyListeners();
        break;
      case 'paired':
        // 配对成功：清空挑战态与旧错误，并拉一次最新名册（含刚配对的新设备）。
        _pendingChallenge = null;
        _lastError = null;
        refreshDevices();
        notifyListeners();
        break;
      case 'unpaired':
        _localDevices = _localDevices
            .where((d) => d.fingerprint != event.fingerprint)
            .toList(growable: false);
        notifyListeners();
        break;
      case 'devices':
        _localDevices = event.devices
            .map(LocalDevice.fromPiece)
            .toList(growable: false);
        notifyListeners();
        break;
      case 'error':
        // 失败即退出挑战态（如放弃后过期的 SAS / 错码），并展示错误。
        _lastError = event.message;
        _pendingChallenge = null;
        notifyListeners();
        break;
      default:
        logInfo(_tag, 'unhandled LinkEvent kind: ${event.kind}');
    }
  }

  void _upsertDiscovered(LinkDiscoveredPiece? piece) {
    if (piece == null) return;
    final peer = LocalDiscoveredPeer.fromPiece(piece);
    final next = List<LocalDiscoveredPeer>.of(_discoveredPeers);
    final idx = next.indexWhere((p) => p.dedupeKey == peer.dedupeKey);
    if (idx >= 0) {
      next[idx] = peer;
    } else {
      next.add(peer);
    }
    _discoveredPeers = next;
    notifyListeners();
  }

  @override
  void dispose() {
    unawaited(_sub?.cancel());
    super.dispose();
  }
}
