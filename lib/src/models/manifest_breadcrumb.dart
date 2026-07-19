// manifest_selection.dart 的面包屑分段模型——深度的唯一去处（design/
// desktop-task-views/DESIGN.md §4.10）：非搜索态由 cwd 路径段推导；
// >4 段折叠为 首段 / ⋯ / 末两段，中间段进 overflowSegments（点击 ⋯ 的
// 隐藏层级菜单数据源）；搜索态整条替换为「搜索结果 · N 项」。
//
// 独立成文件（而非并入 manifest_selection.dart）：面包屑是纯粹的路径字符串
// 分段逻辑，与下钻导航的树/行流计算解耦，单独可测、单独可读。

import '../bindings/bindings.dart' show ManifestItemDto;
import 'manifest_selection.dart' show manifestIsSearching, manifestItemVisible;

enum ManifestCrumbKind { home, segment, ellipsis }

class ManifestCrumbSegment {
  final ManifestCrumbKind kind;
  final String label;
  final String path;
  final bool isLast;

  const ManifestCrumbSegment._(this.kind, this.label, this.path, this.isLast);

  factory ManifestCrumbSegment.home({required bool isLast}) =>
      ManifestCrumbSegment._(ManifestCrumbKind.home, '', '', isLast);

  factory ManifestCrumbSegment.segment({
    required String label,
    required String path,
    required bool isLast,
  }) => ManifestCrumbSegment._(ManifestCrumbKind.segment, label, path, isLast);

  factory ManifestCrumbSegment.ellipsis() =>
      const ManifestCrumbSegment._(ManifestCrumbKind.ellipsis, '⋯', '', false);
}

class ManifestBreadcrumbModel {
  /// true = 搜索态：整条替换为「搜索结果 · N 项」，[segments]/[overflowSegments]
  /// 均为空，[showUp] 恒 false。
  final bool searching;
  final int searchResultCount;

  /// 是否显示「返回上级」按钮（非根且非搜索态）。
  final bool showUp;

  /// 展示用分段（含 home，超过 4 段时已折叠、含 ellipsis 标记）。
  final List<ManifestCrumbSegment> segments;

  /// 折叠时被 ⋯ 隐藏的中间段（点击 ⋯ 弹出的菜单数据），未折叠时为空。
  final List<ManifestCrumbSegment> overflowSegments;

  const ManifestBreadcrumbModel({
    required this.searching,
    required this.searchResultCount,
    required this.showUp,
    required this.segments,
    required this.overflowSegments,
  });
}

/// 构建面包屑模型。[items]/[extFilter]/[search] 仅用于搜索态下的结果计数；
/// 非搜索态下面包屑纯由 [cwd] 路径段推导。
ManifestBreadcrumbModel buildManifestBreadcrumb({
  required List<ManifestItemDto> items,
  required String cwd,
  required Set<String> extFilter,
  required String search,
}) {
  if (manifestIsSearching(search)) {
    final count = items
        .where((it) => manifestItemVisible(it, extFilter: extFilter, search: search))
        .length;
    return ManifestBreadcrumbModel(
      searching: true,
      searchResultCount: count,
      showUp: false,
      segments: const [],
      overflowSegments: const [],
    );
  }

  final segs = cwd.isEmpty ? const <String>[] : cwd.split('/');
  final paths = <String>[];
  var acc = '';
  for (final s in segs) {
    acc = acc.isEmpty ? s : '$acc/$s';
    paths.add(acc);
  }

  final segments = <ManifestCrumbSegment>[
    ManifestCrumbSegment.home(isLast: segs.isEmpty),
  ];
  final overflow = <ManifestCrumbSegment>[];

  if (segs.length <= 4) {
    for (var i = 0; i < segs.length; i++) {
      segments.add(
        ManifestCrumbSegment.segment(
          label: segs[i],
          path: paths[i],
          isLast: i == segs.length - 1,
        ),
      );
    }
  } else {
    segments.add(
      ManifestCrumbSegment.segment(label: segs[0], path: paths[0], isLast: false),
    );
    segments.add(ManifestCrumbSegment.ellipsis());
    for (var i = segs.length - 2; i < segs.length; i++) {
      segments.add(
        ManifestCrumbSegment.segment(
          label: segs[i],
          path: paths[i],
          isLast: i == segs.length - 1,
        ),
      );
    }
    for (var i = 1; i < segs.length - 2; i++) {
      overflow.add(
        ManifestCrumbSegment.segment(label: segs[i], path: paths[i], isLast: false),
      );
    }
  }

  return ManifestBreadcrumbModel(
    searching: false,
    searchResultCount: 0,
    showUp: segs.isNotEmpty,
    segments: segments,
    overflowSegments: overflow,
  );
}
