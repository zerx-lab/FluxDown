// 任务列表视图系统 — 统一实体抽象。
//
// 单任务（[TaskEntity]）与任务组（[GroupEntity]，`members` 由
// `DownloadController.buildListSections` 填充真实组成员）共享同一套聚合键
// 接口，令分桶/排序/列渲染管线对两者零分支处理。[GroupMemberEntity]/
// [GroupDirEntity] 是组展开扁平化后处理阶段产出的纯展示性子行（组内成员
// 行 / 目录分段行，design-proto-spec §8），只出现在 `ListSection.entities`
// 里紧跟其所属 [GroupEntity] 之后，不参与分桶/排序（`buildListSections`
// 排序完成后才插入）。

import 'download_task.dart';

/// 任务列表条目的统一视图实体接口。
///
/// 聚合键覆盖 name/sizeBytes/progress/speed/createdAt/statusBucket/
/// categoryKey/queueId/siteKey（contract-dart.md），供 7 维分桶函数表 +
/// 6 键排序比较器表 + 列渲染器统一消费。
sealed class ListEntity {
  const ListEntity();

  String get id;
  bool get isGroup;
  String get name;
  int get totalBytes;
  int get downloadedBytes;

  /// 进度 [0,1]。
  double get progress;
  int get speedBytesPerSec;
  DateTime get createdAt;

  /// 聚合/自身状态；组按 DESIGN §3.2（对齐主文档 §6.4）规则从成员推导。
  TaskStatus get statusBucket;
  FileCategory get categoryKey;
  String get queueId;

  /// 分桶用 key（注册域聚合，如 `baidu.com`）。
  String get siteKey;

  /// 展示用 label（一级子域展示，如 `pan.baidu.com`）。
  String get siteLabel;
}

/// 单任务实体 — 直接委托给 [DownloadTask]，本波 [ListEntity] 的唯一生产者。
final class TaskEntity extends ListEntity {
  final DownloadTask task;

  const TaskEntity(this.task);

  @override
  String get id => task.id;
  @override
  bool get isGroup => false;
  @override
  String get name => task.fileName;
  @override
  int get totalBytes => task.totalBytes;
  @override
  int get downloadedBytes => task.downloadedBytes;
  @override
  double get progress => task.progress;
  @override
  int get speedBytesPerSec => task.speed;
  @override
  DateTime get createdAt => task.createdAt;
  @override
  TaskStatus get statusBucket => task.status;
  @override
  FileCategory get categoryKey => task.fileCategory;
  @override
  String get queueId => task.queueId;
  @override
  String get siteKey => task.siteKey;
  @override
  String get siteLabel => task.siteLabel;

  @override
  bool operator ==(Object other) =>
      other is TaskEntity && identical(other.task, task);
  @override
  int get hashCode => task.id.hashCode;
}

/// 任务组实体——聚合状态/主导类型/进度/速度/时间均从 [members] 派生
/// （design-proto-spec §3.2 组聚合规则）。`members` 由
/// `DownloadController.buildListSections`（经 `buildGroupEntity`）用真实
/// `AllGroups`/`AllTasks` 数据填充。
final class GroupEntity extends ListEntity {
  final String groupId;
  final String groupName;
  final String sourceUrl;
  final String saveDir;
  final DateTime groupCreatedAt;
  final String groupQueueId;

  /// 组成员（本波恒为空集合；下一波由真实 `TaskGroup` 数据填充，
  /// 元素类型仍是 [ListEntity]——通常是 [TaskEntity]，理论上也允许嵌套聚合）。
  final List<ListEntity> members;

  const GroupEntity({
    required this.groupId,
    required this.groupName,
    required this.sourceUrl,
    required this.saveDir,
    required this.groupCreatedAt,
    required this.groupQueueId,
    this.members = const [],
  });

  @override
  String get id => groupId;
  @override
  bool get isGroup => true;
  @override
  String get name => groupName;
  @override
  int get totalBytes => members.fold(0, (sum, m) => sum + m.totalBytes);
  @override
  int get downloadedBytes =>
      members.fold(0, (sum, m) => sum + m.downloadedBytes);
  @override
  double get progress {
    final total = totalBytes;
    if (total <= 0) return 0;
    return (downloadedBytes / total).clamp(0.0, 1.0);
  }

  @override
  int get speedBytesPerSec => members.fold(
    0,
    (sum, m) => sum + (_isActive(m.statusBucket) ? m.speedBytesPerSec : 0),
  );
  @override
  DateTime get createdAt => groupCreatedAt;

  /// 聚合状态推导（DESIGN §3.2 / 主文档 §6.4）：任一成员下载中→下载中；
  /// 否则任一失败→错误；全部完成→已完成；否则→已暂停。空组（本波恒空，
  /// 业务规则上不会真实出现）安全回退为已完成，避免参与「进行中」聚合。
  @override
  TaskStatus get statusBucket {
    if (members.isEmpty) return TaskStatus.completed;
    if (members.any((m) => _isActive(m.statusBucket))) {
      return TaskStatus.downloading;
    }
    if (members.any((m) => m.statusBucket == TaskStatus.error)) {
      return TaskStatus.error;
    }
    if (members.every((m) => m.statusBucket == TaskStatus.completed)) {
      return TaskStatus.completed;
    }
    return TaskStatus.paused;
  }

  /// 主导类型：成员占比最高者，并列取字节数更大者。
  @override
  FileCategory get categoryKey {
    if (members.isEmpty) return FileCategory.other;
    final counts = <FileCategory, int>{};
    final bytes = <FileCategory, int>{};
    for (final m in members) {
      counts[m.categoryKey] = (counts[m.categoryKey] ?? 0) + 1;
      bytes[m.categoryKey] = (bytes[m.categoryKey] ?? 0) + m.totalBytes;
    }
    FileCategory? best;
    for (final entry in counts.entries) {
      if (best == null) {
        best = entry.key;
        continue;
      }
      final cmp = entry.value.compareTo(counts[best]!);
      if (cmp > 0 || (cmp == 0 && bytes[entry.key]! > bytes[best]!)) {
        best = entry.key;
      }
    }
    return best ?? FileCategory.other;
  }

  @override
  String get queueId => groupQueueId;
  @override
  String get siteKey => extractSiteKey(sourceUrl);
  @override
  String get siteLabel => extractSiteLabel(sourceUrl);

  static bool _isActive(TaskStatus s) =>
      s == TaskStatus.downloading ||
      s == TaskStatus.preparing ||
      s == TaskStatus.resuming;

  @override
  bool operator ==(Object other) =>
      other is GroupEntity && other.groupId == groupId;
  @override
  int get hashCode => groupId.hashCode;
}

/// 组内成员在展开扁平列表中的一行（design-proto-spec §8 `.mrow`）。委托给
/// 底层 [DownloadTask]，字段语义同 [TaskEntity]，额外携带 [groupId]/
/// [dirPath] 供 UI 渲染树轨/缩进与 [GroupDirEntity] 聚簇。只在组展开后处理
/// 阶段产出，不参与分桶/排序（bucketize/compareEntities 只消费顶层实体）。
final class GroupMemberEntity extends ListEntity {
  final DownloadTask task;
  final String groupId;

  /// 该成员在组内的相对目录（''=组根目录），见
  /// `download_controller.dart` 的 `groupMemberDirPath`。
  final String dirPath;

  const GroupMemberEntity({
    required this.task,
    required this.groupId,
    required this.dirPath,
  });

  @override
  String get id => task.id;
  @override
  bool get isGroup => false;
  @override
  String get name => task.fileName;
  @override
  int get totalBytes => task.totalBytes;
  @override
  int get downloadedBytes => task.downloadedBytes;
  @override
  double get progress => task.progress;
  @override
  int get speedBytesPerSec => task.speed;
  @override
  DateTime get createdAt => task.createdAt;
  @override
  TaskStatus get statusBucket => task.status;
  @override
  FileCategory get categoryKey => task.fileCategory;
  @override
  String get queueId => task.queueId;
  @override
  String get siteKey => task.siteKey;
  @override
  String get siteLabel => task.siteLabel;

  @override
  bool operator ==(Object other) =>
      other is GroupMemberEntity && identical(other.task, task);
  @override
  int get hashCode => Object.hash(task.id, groupId);
}

/// 组内目录分段行（design-proto-spec §8 `.mdir`/`dirRowHtml`）。纯合成
/// 展示性实体，只在组展开后处理阶段、每个非空去重目录路径产出一次。
final class GroupDirEntity extends ListEntity {
  final String groupId;
  final String path;
  final int fileCount;
  final int totalDirBytes;

  const GroupDirEntity({
    required this.groupId,
    required this.path,
    required this.fileCount,
    required this.totalDirBytes,
  });

  @override
  String get id => 'dir:$groupId:$path';
  @override
  bool get isGroup => false;
  @override
  String get name => path;
  @override
  int get totalBytes => totalDirBytes;
  @override
  int get downloadedBytes => 0;
  @override
  double get progress => 0;
  @override
  int get speedBytesPerSec => 0;
  @override
  DateTime get createdAt => DateTime.fromMillisecondsSinceEpoch(0);
  @override
  TaskStatus get statusBucket => TaskStatus.completed;
  @override
  FileCategory get categoryKey => FileCategory.other;
  @override
  String get queueId => '';
  @override
  String get siteKey => '';
  @override
  String get siteLabel => '';

  @override
  bool operator ==(Object other) => other is GroupDirEntity && other.id == id;
  @override
  int get hashCode => id.hashCode;
}

/// 组计数行统计（design-proto-spec §8 `groupCountsHtml`）：成员按状态分类
/// 计数。纯函数（工厂），供 `task_group_card.dart` 渲染与测试直接复用。
class GroupMemberCounts {
  final int total;
  final int done;
  final int downloading;
  final int pending;
  final int paused;
  final int failed;

  const GroupMemberCounts({
    required this.total,
    required this.done,
    required this.downloading,
    required this.pending,
    required this.paused,
    required this.failed,
  });

  factory GroupMemberCounts.of(List<ListEntity> members) {
    var done = 0, dl = 0, pend = 0, pause = 0, fail = 0;
    for (final m in members) {
      switch (m.statusBucket) {
        case TaskStatus.completed:
          done++;
        case TaskStatus.downloading:
        case TaskStatus.preparing:
        case TaskStatus.resuming:
          dl++;
        case TaskStatus.pending:
          pend++;
        case TaskStatus.paused:
          pause++;
        case TaskStatus.error:
          fail++;
      }
    }
    return GroupMemberCounts(
      total: members.length,
      done: done,
      downloading: dl,
      pending: pend,
      paused: pause,
      failed: fail,
    );
  }
}

/// 分组头需要的聚合展示信息（design-proto-spec §9 `.bmeta`）。
class ListSectionMeta {
  /// 桶内总字节数（Σtotal）。
  final int totalBytes;

  /// 桶内活跃（下载中/准备中/恢复中）实体的速度之和。
  final int activeSpeedBytesPerSec;
  final bool hasActive;
  final bool hasError;

  const ListSectionMeta({
    required this.totalBytes,
    required this.activeSpeedBytesPerSec,
    required this.hasActive,
    required this.hasError,
  });

  factory ListSectionMeta.of(List<ListEntity> entities) {
    var totalBytes = 0;
    var activeSpeed = 0;
    var hasActive = false;
    var hasError = false;
    for (final e in entities) {
      // 组展开扁平化产出的成员/目录行不参与桶聚合——已经由其所属
      // GroupEntity 的聚合字段计入一次，避免展开态下重复计数字节/速度。
      if (e is GroupMemberEntity || e is GroupDirEntity) continue;
      totalBytes += e.totalBytes;
      if (GroupEntity._isActive(e.statusBucket)) {
        hasActive = true;
        activeSpeed += e.speedBytesPerSec;
      }
      if (e.statusBucket == TaskStatus.error) hasError = true;
    }
    return ListSectionMeta(
      totalBytes: totalBytes,
      activeSpeedBytesPerSec: activeSpeed,
      hasActive: hasActive,
      hasError: hasError,
    );
  }
}

/// 一个分桶（分组头 + 桶内实体列表）。`title == null` 时不渲染分组头
/// （`none` 维度，纯平铺，design-proto-spec §9）。
class ListSection {
  /// 稳定桶 id（如 `smart:live`、`date:0`、`status:dl`、`site:baidu_com`）。
  final String key;
  final String? title;
  final List<ListEntity> entities;

  ListSection({required this.key, required this.title, required this.entities});

  ListSectionMeta get meta => ListSectionMeta.of(entities);

  /// 顶层实体数（不含组展开产出的成员/目录行），分组头计数徽标用。
  int get topLevelCount =>
      entities.where((e) => e is! GroupMemberEntity && e is! GroupDirEntity).length;
}
