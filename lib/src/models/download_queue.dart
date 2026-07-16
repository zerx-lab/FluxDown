import '../bindings/bindings.dart';
import '../i18n/translations.dart';

/// 内置「主队列」的固定 ID（不可删除/重命名，UI 本地化显示名）。
const String kMainQueueId = 'main';

/// 内置「稍后下载」队列的固定 ID（默认停止；稍后下载的默认落点）。
const String kLaterQueueId = 'later';

/// 队列归属未知的占位哨兵：TaskProgress 信号不携带 queue_id，先于
/// AllTasks 快照到达的新任务暂用此值——归属未知不等于「未分组」，
/// 避免侧栏未分组入口瞬时闪现。快照到达后被真实 queue_id 覆盖；
/// 永不发送给引擎。
const String kQueueAttributionPending = '\u0000pending-queue';

/// 命名下载队列的 Dart 侧模型（对应 Rust QueueInfo）
class DownloadQueue {
  final String queueId;
  final String name;

  /// 速度限制（KB/s），0 = 不限制
  final int speedLimitKbps;

  /// 同时下载任务数，0 = 使用全局设置
  final int maxConcurrent;

  /// 默认保存目录，空 = 使用全局设置
  final String defaultSaveDir;

  /// 显示顺序（从小到大）
  final int position;

  /// 每任务默认线程数（HTTP 分段连接数），0 = 自动（全局 segment_advisor）
  final int defaultSegments;

  /// 此队列任务使用的默认 User-Agent，空字符串 = 继承全局 UA
  final String defaultUserAgent;

  /// 队列运行状态。停止的队列不自动启动其中任务（稍后下载/定时的基础）。
  final bool isRunning;

  /// 每日定时计划是否启用。
  final bool scheduleEnabled;

  /// 每日定时启动时间 "HH:MM"（空 = 不定时启动）。
  final String scheduleStart;

  /// 每日定时停止时间 "HH:MM"（空 = 不定时停止）。
  final String scheduleStop;

  /// 定时生效星期位掩码：bit0=周一 … bit6=周日；127 = 每天。
  final int scheduleDays;

  const DownloadQueue({
    required this.queueId,
    required this.name,
    required this.speedLimitKbps,
    required this.maxConcurrent,
    required this.defaultSaveDir,
    required this.position,
    this.defaultSegments = 0,
    this.defaultUserAgent = '',
    this.isRunning = true,
    this.scheduleEnabled = false,
    this.scheduleStart = '',
    this.scheduleStop = '',
    this.scheduleDays = 127,
  });

  /// 是否为内置队列（主队列/稍后下载）：不可删除、不可重命名。
  bool get isBuiltin => queueId == kMainQueueId || queueId == kLaterQueueId;

  factory DownloadQueue.fromQueueInfo(QueueInfo info) {
    return DownloadQueue(
      queueId: info.queueId,
      name: info.name,
      speedLimitKbps: info.speedLimitKbps,
      maxConcurrent: info.maxConcurrent,
      defaultSaveDir: info.defaultSaveDir,
      position: info.position,
      defaultSegments: info.defaultSegments,
      defaultUserAgent: info.defaultUserAgent,
      isRunning: info.isRunning,
      scheduleEnabled: info.scheduleEnabled,
      scheduleStart: info.scheduleStart,
      scheduleStop: info.scheduleStop,
      scheduleDays: info.scheduleDays,
    );
  }

  DownloadQueue copyWith({
    String? queueId,
    String? name,
    int? speedLimitKbps,
    int? maxConcurrent,
    String? defaultSaveDir,
    int? position,
    int? defaultSegments,
    String? defaultUserAgent,
    bool? isRunning,
    bool? scheduleEnabled,
    String? scheduleStart,
    String? scheduleStop,
    int? scheduleDays,
  }) {
    return DownloadQueue(
      queueId: queueId ?? this.queueId,
      name: name ?? this.name,
      speedLimitKbps: speedLimitKbps ?? this.speedLimitKbps,
      maxConcurrent: maxConcurrent ?? this.maxConcurrent,
      defaultSaveDir: defaultSaveDir ?? this.defaultSaveDir,
      position: position ?? this.position,
      defaultSegments: defaultSegments ?? this.defaultSegments,
      defaultUserAgent: defaultUserAgent ?? this.defaultUserAgent,
      isRunning: isRunning ?? this.isRunning,
      scheduleEnabled: scheduleEnabled ?? this.scheduleEnabled,
      scheduleStart: scheduleStart ?? this.scheduleStart,
      scheduleStop: scheduleStop ?? this.scheduleStop,
      scheduleDays: scheduleDays ?? this.scheduleDays,
    );
  }
}

/// 队列显示名：内置队列按固定 ID 显示本地化名称（DB 里的英文 sentinel
/// 名不外显），自定义队列显示用户命名。
String queueDisplayName(S s, DownloadQueue q) => switch (q.queueId) {
  kMainQueueId => s.mainQueue,
  kLaterQueueId => s.laterQueue,
  _ => q.name,
};
