// 任务组桌面 UI — 组元数据模型（对应 Rust `GroupInfo`）。
//
// 命名说明：hub 信号契约里的概念名是「group」（GroupInfo/GroupControl/
// AllGroups/RenameGroup），Dart 侧本应直接叫 `TaskGroup`，但该符号已被
// `download_task.dart` 的移动端时间分桶类占用（`mobile_tasks_screen.dart`
// 仍在使用，按契约保留不动）。Dart 顶层类名同库不可重复，故本文件的引擎
// 任务组模型改名为 [DownloadGroup]（与 [DownloadTask]/[DownloadQueue] 同一
// 命名族），字段/语义与 hub-final-signals.md 完全一致，仅符号名不同。

import '../bindings/bindings.dart';

/// 命名下载组的 Dart 侧模型（对应 Rust `GroupInfo`）。
class DownloadGroup {
  final String id;
  final String name;
  final String sourceUrl;

  /// 组根目录——引擎解析后的实际下载目录（`base_save_dir/sanitize(name)`，
  /// `name` 为空时直接等于用户提交的 base 目录，见 hub-final-signals.md
  /// §3），不是 `CreateTaskGroup.saveDir` 原始提交值。
  final String saveDir;
  final DateTime createdAt;

  const DownloadGroup({
    required this.id,
    required this.name,
    required this.sourceUrl,
    required this.saveDir,
    required this.createdAt,
  });

  /// 从 `AllGroups` 信号中的 `GroupInfo` 构建（`createdAt` 为 Unix seconds
  /// 字符串，同 [DownloadTask.fromTaskInfo] 惯例）。
  factory DownloadGroup.fromSignal(GroupInfo info) {
    final seconds = int.tryParse(info.createdAt) ?? 0;
    return DownloadGroup(
      id: info.groupId,
      name: info.name,
      sourceUrl: info.sourceUrl,
      saveDir: info.saveDir,
      createdAt: seconds > 0
          ? DateTime.fromMillisecondsSinceEpoch(seconds * 1000)
          : DateTime.now(),
    );
  }

  /// 展示用名称：组名为空（用户建组未命名）时回退保存目录末段，保证组行
  /// 永远有非空可读名称。
  String get displayName =>
      name.isNotEmpty ? name : _lastPathSegment(saveDir);

  static String _lastPathSegment(String path) {
    final normalized = path.replaceAll('\\', '/');
    final trimmed = normalized.endsWith('/')
        ? normalized.substring(0, normalized.length - 1)
        : normalized;
    final idx = trimmed.lastIndexOf('/');
    return idx >= 0 ? trimmed.substring(idx + 1) : trimmed;
  }

  @override
  bool operator ==(Object other) => other is DownloadGroup && other.id == id;
  @override
  int get hashCode => id.hashCode;
}

// =============================================================================
// 火花条抽样（design-proto-spec §8 `sparkHtml`）
// =============================================================================

/// 火花条抽样：`items.length > maxBars` 时等距抽样 [maxBars] 根
/// （`step = len / maxBars`，取 `items[(i * step).floor()]`）；否则逐项
/// 返回。纯函数，供 `task_group_card.dart` 渲染与测试直接复用。
List<T> sampleSparkline<T>(List<T> items, {int maxBars = 24}) {
  if (items.length <= maxBars) return items;
  final step = items.length / maxBars;
  return [for (var i = 0; i < maxBars; i++) items[(i * step).floor()]];
}

// =============================================================================
// 路径链压缩（design-proto-spec §8 `dirRowHtml` 路径压缩）
// =============================================================================

/// 目录路径压缩：按 `/`（含反斜杠归一化）分段，`>3` 段压缩为
/// `"首段 / … / 末段 /"`，`<=3` 段整链 `"seg / seg / … /"`。空路径（根目录）
/// 返回空串——调用方不应为根目录渲染目录行。
String compressPathChain(String path) {
  if (path.isEmpty) return '';
  final segments = path
      .replaceAll('\\', '/')
      .split('/')
      .where((s) => s.isNotEmpty)
      .toList();
  if (segments.isEmpty) return '';
  if (segments.length <= 3) return '${segments.join(' / ')} /';
  return '${segments.first} / … / ${segments.last} /';
}
