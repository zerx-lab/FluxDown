// FluxDown 本地设备互联 —— 客户端传输抽象（镜像 Rust 端 transport seam）。
//
// 背景：Rust 引擎（native/engine/src/link/）按「传输方式」抽象了拨号层——
// v1 只实现「网络可达直连」（同局域网/同网段，TCP 直连，不做 NAT 穿透）；
// 后续计划接入 iroh（QUIC + 打洞 + DERP 中继兜底）等库，让配对在跨网络场景
// 下也能工作，且不需要更换上层协议（LinkCommand/LinkEvent 不变）。
//
// 本文件是**这条 seam 在 Dart 侧的镜像声明**：实际拨号逻辑完全在 Rust 引擎
// 内完成（Dart 只发 LinkCommand、收 LinkEvent），Dart 侧不做任何网络 I/O，
// 因此这里没有 `dial()`/`connect()` 方法。它存在的意义：
//
//   1. 让「本地配对」对话框可以显式展示当前连接走的是哪条路径（如角标/
//      文案「局域网直连」），而不是把「直连」这个假设硬编码进 UI 文案；
//   2. 为未来新增 iroh/relay 传输时，UI 层已有落点（新增枚举值 + 注册项
//      即可渲染新选项），不需要推倒重来；
//   3. 文档化「v1 只做局域网直连，不支持跨网络打洞」这一产品边界，避免
//      调用方误以为配对已支持公网穿透。
//
// 新增传输方式的步骤：
//   1. Rust 端 native/engine/src/link/ 下实现对应 dial 逻辑；
//   2. 这里新增一个 [LinkTransport] 实现类，纳入 [LinkTransportRegistry.all]；
//   3. 该传输方式落地后把 `available` 翻转为 true。

/// 设备互联可用的传输方式。
///
/// 与 Rust 端 transport 实现一一对应（未来新增 Rust transport 时，此处需
/// 同步新增枚举值，两端保持镜像，命名也保持一致）。
enum LinkTransportKind {
  /// v1：网络可达直连——同局域网/同网段内，客户端探测到对端 host:port 后
  /// 直接 TCP 连接，不做任何 NAT 穿透或中继。要求双方在同一二层/路由可达
  /// 网络内（如同一 Wi-Fi、公司内网），跨网络（如两端分属不同 NAT 之后）
  /// 不可用。
  direct,

  /// 规划中：基于 iroh（QUIC + hole punching + DERP 中继兜底）的传输，
  /// 目标是让配对在「双方均在公网/不同内网」场景下也能直连或经中继连通，
  /// 无需用户手动做端口转发。Rust 引擎尚未实现，枚举值先占位以固定
  /// 客户端 API 形状，避免未来新增时改动波及现有调用方。
  iroh,

  /// 规划中：显式中继模式（不尝试直连，直接经中继服务器转发）。用于
  /// iroh 打洞失败时的强制兜底，或网络策略明确禁止 P2P 直连的场景。
  relay,
}

/// 客户端可见的一种传输方式描述。
///
/// 纯声明性对象（无网络行为）——[kind] 用于 UI 判断当前/可选传输，
/// [available] 标记该传输在当前客户端版本是否已经可用（iroh/relay 在
/// Rust 引擎落地前恒为 false，避免 UI 提前展示不可用的选项）。
abstract class LinkTransport {
  const LinkTransport();

  /// 传输方式标识。
  LinkTransportKind get kind;

  /// 该传输方式在当前客户端版本是否可用（供 UI 决定是否展示/可选）。
  bool get available;

  /// 面向用户的简短说明（配对对话框可直接展示，说明连接路径的含义）。
  String get description;
}

/// v1 唯一已落地实现：网络可达直连。探测（probe）与配对
/// （beginPairing/confirmPairing）在 Rust 端均假设双方局域网可达，
/// 不做穿透重试；连不通时直接下发 `LinkEvent{kind:"error"}`。
class DirectLinkTransport extends LinkTransport {
  const DirectLinkTransport();

  @override
  LinkTransportKind get kind => LinkTransportKind.direct;

  @override
  bool get available => true;

  @override
  String get description =>
      '同一局域网内直连（无需公网穿透），要求两台设备处于同一 Wi-Fi/路由器下。';
}

/// 规划中：iroh 传输占位描述（[available] 恒为 false，直到 Rust 引擎接入
/// iroh 后再翻转）。保留在此处是为了让 UI 可以提前渲染「即将支持」的选项，
/// 而不必等 Rust 落地后才补 Dart 侧改动。
class IrohLinkTransport extends LinkTransport {
  const IrohLinkTransport();

  @override
  LinkTransportKind get kind => LinkTransportKind.iroh;

  @override
  bool get available => false;

  @override
  String get description => '跨网络直连/自动打洞（规划中，尚未接入 Rust 引擎）。';
}

/// 规划中：显式中继传输占位描述，语义同 [IrohLinkTransport]，仅连接路径
/// 固定为「经中继转发」。
class RelayLinkTransport extends LinkTransport {
  const RelayLinkTransport();

  @override
  LinkTransportKind get kind => LinkTransportKind.relay;

  @override
  bool get available => false;

  @override
  String get description => '经中继服务器转发（规划中，打洞失败时的兜底方案）。';
}

/// 客户端侧传输注册表——UI 层枚举「当前支持哪些传输方式」的唯一入口。
///
/// 配对对话框只应通过 [available] 过滤后展示选项，不应把
/// [LinkTransportKind.direct] 硬编码为唯一可能值，为后续 iroh/relay 接入
/// 预留位置。
class LinkTransportRegistry {
  LinkTransportRegistry._();

  /// 全部已声明的传输方式（含尚未落地的规划项）。
  static const List<LinkTransport> all = [
    DirectLinkTransport(),
    IrohLinkTransport(),
    RelayLinkTransport(),
  ];

  /// 当前客户端版本实际可用的传输方式（UI 应优先用这个渲染可选项）。
  static List<LinkTransport> get available =>
      all.where((t) => t.available).toList(growable: false);

  /// 按 [kind] 查询对应传输方式描述。
  static LinkTransport byKind(LinkTransportKind kind) =>
      all.firstWhere((t) => t.kind == kind);
}
