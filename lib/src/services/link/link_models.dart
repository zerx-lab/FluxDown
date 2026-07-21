// FluxDown 本地设备互联（局域网配对）—— 客户端领域模型。
//
// 字段严格对照 Rust 端信号 payload（见 ../../bindings/signals/link_*.dart，
// 由 rinf 从 native/hub/src/signals/mod.rs 生成），本文件只做「生成类型 →
// 领域模型」的薄封装：UI 层与 LocalPairingService 只依赖这里的类型，不直接
// 触碰生成类型，方便未来协议字段演进时改动收敛在一处。

import '../../bindings/bindings.dart';

/// 已配对（受信任）的本地设备——对应 `LinkEvent.devices` 列表元素
/// （Rust 端 `LinkDevicePiece`）。
class LocalDevice {
  final String fingerprint;
  final String name;
  final String platform;

  /// 是否在线（Rust 端按最近一次探测/心跳判定）。
  final bool online;

  /// 最近一次上线时间（Unix 秒），离线设备用于展示「最后在线于…」。
  final int lastSeenAt;

  const LocalDevice({
    required this.fingerprint,
    required this.name,
    required this.platform,
    required this.online,
    required this.lastSeenAt,
  });

  factory LocalDevice.fromPiece(LinkDevicePiece piece) => LocalDevice(
    fingerprint: piece.fingerprint,
    name: piece.name,
    platform: piece.platform,
    online: piece.online,
    lastSeenAt: piece.lastSeenAt,
  );
}

/// 局域网发现的未配对设备——对应 `LinkEvent.discovered`
/// （Rust 端 `LinkDiscoveredPiece`）。
class LocalDiscoveredPeer {
  final String fingerprint;
  final String name;
  final String platform;
  final String host;
  final int port;
  final String appVersion;

  /// 发现来源（如 mDNS/广播，Rust 端自由定义，UI 仅用于调试展示）。
  final String source;

  const LocalDiscoveredPeer({
    required this.fingerprint,
    required this.name,
    required this.platform,
    required this.host,
    required this.port,
    required this.appVersion,
    required this.source,
  });

  /// 去重/合并键：优先按指纹（同一设备重启后 host:port 可能变化）；
  /// 指纹尚未取得（如刚广播、还未完成身份交换）时退化为 `host:port`。
  String get dedupeKey => fingerprint.isNotEmpty ? fingerprint : '$host:$port';

  factory LocalDiscoveredPeer.fromPiece(LinkDiscoveredPiece piece) =>
      LocalDiscoveredPeer(
        fingerprint: piece.fingerprint,
        name: piece.name,
        platform: piece.platform,
        host: piece.host,
        port: piece.port,
        appVersion: piece.appVersion,
        source: piece.source,
      );
}

/// 配对挑战（SAS 短数字核验）——由 `LinkEvent{kind:"pairingChallenge"}` 派生。
///
/// [token] 用于 [PairingChallenge] 回传 confirmPairing；[sas] 为双方需在各自
/// 屏幕上人工核对一致的短数字/短语（防中间人）。
class PairingChallenge {
  final String token;
  final String sas;
  final String peerName;
  final String peerFingerprint;

  const PairingChallenge({
    required this.token,
    required this.sas,
    required this.peerName,
    required this.peerFingerprint,
  });

  factory PairingChallenge.fromEvent(LinkEvent event) => PairingChallenge(
    token: event.token,
    sas: event.sas,
    peerName: event.name,
    peerFingerprint: event.fingerprint,
  );
}
