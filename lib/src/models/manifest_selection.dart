// 预解析清单（ResolvePreviewResult.items）→ 建组选择弹窗 的纯逻辑层。
//
// 不依赖 Flutter（可在 VM 单测里直接跑）：树构建 + 单链目录折叠、扁平化
// 可见行（缩进封顶）、三态目录勾选、扩展名筛选/意图聚合、规格策略（含
// 精确档回退计数）、resolver_item 拼接、剧集智能建议启发式。
//
// 契约来源：local://contract-dart.md §选择弹窗、local://hub-final-signals.md
// §3（resolver_item 拼接规则）。UI 层（manifest_select_dialog.dart）只负责
// 渲染与持有交互状态（选中集合/折叠集合/覆盖表），实际计算全部委托本文件。

import '../bindings/bindings.dart'
    show GroupItemEntry, ManifestItemDto, ManifestVariantDto;
import 'download_task.dart' show FileCategory;

// =============================================================================
// 1. 树结构 + 单链折叠
// =============================================================================

/// 清单条目的树节点：目录或文件。
sealed class ManifestNode {
  /// 展示名（目录节点在单链折叠后可能是 "a/b/c" 这样的合并段）。
  String get name;

  /// 从虚拟根出发的完整路径（不含前导/尾随 `/`），用于折叠集合的稳定 key。
  String get path;

  /// 折叠后的树深度（根的直接子节点 depth=0）。
  int get depth;
}

final class ManifestDirNode extends ManifestNode {
  @override
  final String name;
  @override
  final String path;
  @override
  final int depth;
  final List<ManifestNode> children;

  ManifestDirNode({
    required this.name,
    required this.path,
    required this.depth,
    required this.children,
  });
}

final class ManifestFileNode extends ManifestNode {
  final ManifestItemDto item;
  @override
  String get name => item.name;
  @override
  final String path;
  @override
  final int depth;

  ManifestFileNode({
    required this.item,
    required this.path,
    required this.depth,
  });
}

class _DirBuilder {
  final String name;
  final String path;
  final Map<String, _DirBuilder> dirs = {};
  final List<ManifestItemDto> files = [];
  _DirBuilder({required this.name, required this.path});
}

/// 由 [items] 构建树（按 `item.path` 以 `/` 拆分目录段），并对单链目录
/// （一路只有一个子目录、且该级没有文件）做合并折叠——`a/b/c` 只出现一行，
/// 直到遇到分叉（多子项）或该级出现文件为止。
List<ManifestNode> buildManifestTree(List<ManifestItemDto> items) {
  final root = _DirBuilder(name: '', path: '');
  for (final item in items) {
    final segments = item.path.split('/').where((s) => s.isNotEmpty).toList();
    if (segments.isEmpty) {
      // 极端兜底：空 path 时把文件直接挂根下（理论上引擎不会发这种数据）。
      root.files.add(item);
      continue;
    }
    var cursor = root;
    for (var i = 0; i < segments.length - 1; i++) {
      final seg = segments[i];
      cursor = cursor.dirs.putIfAbsent(
        seg,
        () => _DirBuilder(
          name: seg,
          path: cursor.path.isEmpty ? seg : '${cursor.path}/$seg',
        ),
      );
    }
    cursor.files.add(item);
  }
  return _materializeChildren(root, depth: 0);
}

List<ManifestNode> _materializeChildren(_DirBuilder dir, {required int depth}) {
  // 目录先按名排序、后接按名排序的文件——先看结构、再看叶子，符合阅读习惯。
  final dirNames = dir.dirs.keys.toList()..sort();
  final fileList = [...dir.files]..sort((a, b) => a.name.compareTo(b.name));

  final nodes = <ManifestNode>[];
  for (final name in dirNames) {
    nodes.add(_materializeDir(dir.dirs[name]!, depth: depth));
  }
  for (final item in fileList) {
    nodes.add(ManifestFileNode(item: item, path: item.path, depth: depth));
  }
  return nodes;
}

/// 单链折叠：只要 [dir] 唯一子项是「无文件的单一子目录」就持续下潜合并
/// name 段，直到遇到分叉（>1 子项）或该级出现文件为止。折叠后行的 [path]
/// 取链末端真实目录的完整路径（供折叠状态集合/选中集合使用的稳定 key）。
ManifestDirNode _materializeDir(_DirBuilder dir, {required int depth}) {
  var current = dir;
  final nameChain = [dir.name];
  while (current.files.isEmpty && current.dirs.length == 1) {
    final only = current.dirs.values.first;
    nameChain.add(only.name);
    current = only;
  }
  return ManifestDirNode(
    name: nameChain.join('/'),
    path: current.path,
    depth: depth,
    children: _materializeChildren(current, depth: depth + 1),
  );
}

// =============================================================================
// 2. 扁平化可见行（缩进封顶 + 灰色父目录前缀）
// =============================================================================

/// 缩进封顶级数：超过此深度不再增加视觉缩进，改用 [ManifestVisibleRow.greyPrefix]
/// 压缩显示路径上下文。
const int kManifestIndentCap = 4;

/// 一条可见行：对应树节点 + 计算好的展示用缩进/灰色前缀。
class ManifestVisibleRow {
  final ManifestNode node;

  /// 视觉缩进级数，`min(depth, kManifestIndentCap)`。
  final int indent;

  /// 深度超过封顶级数时，名称列前缀的灰色路径提示 `"…/<父目录名>/"`；
  /// 未超限时为空串。
  final String greyPrefix;

  const ManifestVisibleRow({
    required this.node,
    required this.indent,
    required this.greyPrefix,
  });
}

/// 把树展平为当前可见（未被折叠隐藏）的行列表。[collapsedDirPaths] 内的
/// 目录自身仍展示一行，但其子树不展开（E4：缩进只吃名称列，右列固定宽由
/// UI 层负责，本函数只产出结构数据）。
List<ManifestVisibleRow> flattenManifestTree(
  List<ManifestNode> roots,
  Set<String> collapsedDirPaths,
) {
  final rows = <ManifestVisibleRow>[];

  void walk(ManifestNode node, int depth, String? immediateParentName) {
    final indent = depth > kManifestIndentCap ? kManifestIndentCap : depth;
    final greyPrefix = depth > kManifestIndentCap && immediateParentName != null
        ? '…/$immediateParentName/'
        : '';
    rows.add(
      ManifestVisibleRow(node: node, indent: indent, greyPrefix: greyPrefix),
    );
    if (node is ManifestDirNode && !collapsedDirPaths.contains(node.path)) {
      for (final child in node.children) {
        walk(child, depth + 1, node.name);
      }
    }
  }

  for (final root in roots) {
    walk(root, 0, null);
  }
  return rows;
}

// =============================================================================
// 3. 三态目录勾选
// =============================================================================

enum ManifestCheckState { checked, unchecked, indeterminate }

/// 收集 [node] 子树下全部叶子条目的 id。
Set<String> collectManifestItemIds(ManifestNode node) {
  return switch (node) {
    ManifestFileNode(:final item) => {item.id},
    ManifestDirNode(:final children) => {
      for (final c in children) ...collectManifestItemIds(c),
    },
  };
}

/// 节点子树内全部条目的 Σsize（目录行的聚合大小展示用）。
int manifestNodeTotalSize(ManifestNode node) {
  return switch (node) {
    ManifestFileNode(:final item) => item.size,
    ManifestDirNode(:final children) => children.fold(
      0,
      (sum, c) => sum + manifestNodeTotalSize(c),
    ),
  };
}

/// 目录节点的三态勾选状态：由其全部叶子 item 是否被选中推导。
ManifestCheckState manifestDirCheckState(
  ManifestDirNode dir,
  Set<String> selectedItemIds,
) {
  final ids = collectManifestItemIds(dir);
  if (ids.isEmpty) return ManifestCheckState.unchecked;
  final selected = ids.where(selectedItemIds.contains).length;
  if (selected == 0) return ManifestCheckState.unchecked;
  if (selected == ids.length) return ManifestCheckState.checked;
  return ManifestCheckState.indeterminate;
}

/// 勾选/取消勾选一个目录：递归对其全部叶子 item 应用同一选中态，返回新集合
/// （不改动入参，UI 层直接拿返回值 setState）。
Set<String> toggleManifestDirSelection(
  ManifestDirNode dir,
  Set<String> selectedItemIds,
  bool select,
) {
  final ids = collectManifestItemIds(dir);
  final next = Set<String>.from(selectedItemIds);
  if (select) {
    next.addAll(ids);
  } else {
    next.removeAll(ids);
  }
  return next;
}

// =============================================================================
// 4. 扩展名筛选 / 意图聚合（复用 FileCategory.fromExtension 8 类）
// =============================================================================

/// 从文件名提取扩展名（不含点号，小写）；无扩展名（或以 `.` 开头的隐藏文件）
/// 返回空串。
String manifestFileExtension(String fileName) {
  final idx = fileName.lastIndexOf('.');
  if (idx <= 0 || idx == fileName.length - 1) return '';
  return fileName.substring(idx + 1).toLowerCase();
}

FileCategory manifestItemCategory(ManifestItemDto item) =>
    FileCategory.fromExtension(manifestFileExtension(item.name));

/// 按 [category] 过滤清单条目；`FileCategory.all` 不过滤（原样返回）。
List<ManifestItemDto> filterManifestItemsByCategory(
  List<ManifestItemDto> items,
  FileCategory category,
) {
  if (category == FileCategory.all) return items;
  return items.where((i) => manifestItemCategory(i) == category).toList();
}

/// 一个类型意图的聚合结果（意图按钮组：计数 + Σsize，点击即把选中集合
/// 整体替换为 [itemIds]）。
class ManifestCategoryAggregate {
  final FileCategory category;
  final int count;
  final int totalSize;
  final Set<String> itemIds;

  const ManifestCategoryAggregate({
    required this.category,
    required this.count,
    required this.totalSize,
    required this.itemIds,
  });
}

/// 按类型聚合，仅返回清单中实际出现过的类型（`all` 不含在内——「全选」由
/// 调用方用 [allManifestItemIds] 单独提供，不是一个类型意图）。按
/// [FileCategory.values] 声明顺序排列。
List<ManifestCategoryAggregate> aggregateManifestByCategory(
  List<ManifestItemDto> items,
) {
  final byCategory = <FileCategory, List<ManifestItemDto>>{};
  for (final item in items) {
    byCategory.putIfAbsent(manifestItemCategory(item), () => []).add(item);
  }
  final result = <ManifestCategoryAggregate>[];
  for (final category in FileCategory.values) {
    if (category == FileCategory.all) continue;
    final bucket = byCategory[category];
    if (bucket == null || bucket.isEmpty) continue;
    result.add(
      ManifestCategoryAggregate(
        category: category,
        count: bucket.length,
        totalSize: bucket.fold(0, (sum, i) => sum + i.size),
        itemIds: bucket.map((i) => i.id).toSet(),
      ),
    );
  }
  return result;
}

/// 清单内全部条目 id（「全选」用）。
Set<String> allManifestItemIds(List<ManifestItemDto> items) =>
    items.map((i) => i.id).toSet();

/// 反选：未选中的变为选中，已选中的变为未选中。
Set<String> invertManifestSelection(
  List<ManifestItemDto> items,
  Set<String> selectedItemIds,
) => allManifestItemIds(items).difference(selectedItemIds);

int manifestTotalSize(List<ManifestItemDto> items) =>
    items.fold(0, (sum, i) => sum + i.size);

int manifestSelectedSize(
  List<ManifestItemDto> items,
  Set<String> selectedIds,
) => items
    .where((i) => selectedIds.contains(i.id))
    .fold(0, (sum, i) => sum + i.size);

// =============================================================================
// 5. 规格策略（最高 / 1080P / 720P / 最省），含精确档回退计数
// =============================================================================

enum ManifestQualityPolicy { highest, p1080, p720, lowest }

/// 从规格 label 中解析一个可比较的"质量分"：优先取分辨率高度
/// （"1080p"/"2160p"/4K/2K/8K 等常见简写），否则退化用带宽数值
/// （"5000kbps"/"3.5mbps"），都取不到时退化取字符串里第一个数字；完全没有
/// 数字则返回 null（策略计算会跳过该 variant 的评分，但它仍可被选中——
/// 见 [_pickBest]/[_pickNearestTier] 的回退逻辑）。
int? manifestVariantQualityScore(String label) {
  final lower = label.toLowerCase();
  final res = RegExp(r'(\d{3,4})\s*p\b').firstMatch(lower);
  if (res != null) return int.parse(res.group(1)!);
  if (RegExp(r'\b8k\b').hasMatch(lower)) return 4320;
  if (RegExp(r'\b4k\b').hasMatch(lower)) return 2160;
  if (RegExp(r'\b2k\b').hasMatch(lower)) return 1440;
  final mbps = RegExp(r'([\d.]+)\s*mbps').firstMatch(lower);
  if (mbps != null) return (double.parse(mbps.group(1)!) * 1000).round();
  final kbps = RegExp(r'([\d.]+)\s*kbps').firstMatch(lower);
  if (kbps != null) return double.parse(kbps.group(1)!).round();
  final anyNum = RegExp(r'(\d+)').firstMatch(lower);
  if (anyNum != null) return int.parse(anyNum.group(1)!);
  return null;
}

/// 单个条目按策略选出的规格结果。
class ManifestItemVariantChoice {
  final String itemId;

  /// 选中的规格 id；null = 该条目没有 variants（单文件直传，不受策略影响）。
  final String? variantId;

  /// true = 精确档（1080p/720p）没有命中，回退到最接近的规格。
  final bool isFallback;

  const ManifestItemVariantChoice({
    required this.itemId,
    required this.variantId,
    required this.isFallback,
  });
}

class ManifestPolicyResult {
  final List<ManifestItemVariantChoice> choices;

  /// 发生回退（仅 [ManifestQualityPolicy.p1080]/[p720] 精确档未命中时计数；
  /// `highest`/`lowest` 本身没有"精确档"概念，恒为 0）的条目数。
  final int fallbackCount;

  const ManifestPolicyResult({
    required this.choices,
    required this.fallbackCount,
  });

  Map<String, String?> get asMap => {
    for (final c in choices) c.itemId: c.variantId,
  };
}

/// 对全部条目应用规格策略；无 variants 的条目原样跳过（variantId=null，
/// 不计入回退统计）。
ManifestPolicyResult applyManifestQualityPolicy(
  List<ManifestItemDto> items,
  ManifestQualityPolicy policy,
) {
  final choices = <ManifestItemVariantChoice>[];
  var fallbackCount = 0;
  for (final item in items) {
    if (item.variants.isEmpty) {
      choices.add(
        ManifestItemVariantChoice(
          itemId: item.id,
          variantId: null,
          isFallback: false,
        ),
      );
      continue;
    }
    final scored = <(ManifestVariantDto, int?)>[
      for (final v in item.variants) (v, manifestVariantQualityScore(v.label)),
    ];

    final ManifestVariantDto chosen;
    var fallback = false;
    if (policy == ManifestQualityPolicy.highest) {
      chosen = _pickBest(scored, higherIsBetter: true) ?? item.variants.first;
    } else if (policy == ManifestQualityPolicy.lowest) {
      chosen = _pickBest(scored, higherIsBetter: false) ?? item.variants.first;
    } else {
      final target = policy == ManifestQualityPolicy.p1080 ? 1080 : 720;
      final (v, fb) = _pickNearestTier(scored, target, item.variants.first);
      chosen = v;
      fallback = fb;
    }
    if (fallback) fallbackCount++;
    choices.add(
      ManifestItemVariantChoice(
        itemId: item.id,
        variantId: chosen.id,
        isFallback: fallback,
      ),
    );
  }
  return ManifestPolicyResult(choices: choices, fallbackCount: fallbackCount);
}

/// 取分数最优（[higherIsBetter] 定义"更优"方向）的 variant；全部未解析出
/// 分数则返回 null（调用方回退取 `variants.first`）。
ManifestVariantDto? _pickBest(
  List<(ManifestVariantDto, int?)> scored, {
  required bool higherIsBetter,
}) {
  ManifestVariantDto? best;
  int? bestScore;
  for (final (variant, score) in scored) {
    if (score == null) continue;
    final better =
        bestScore == null ||
        (higherIsBetter ? score > bestScore : score < bestScore);
    if (better) {
      bestScore = score;
      best = variant;
    }
  }
  return best;
}

/// 精确档策略：优先取分数恰好等于 [target] 的 variant；没有则取分数差
/// 绝对值最小的（次优回退，返回 fallback=true）；全部未解析出分数时回退取
/// [fallbackDefault]（同样标记 fallback=true）。
(ManifestVariantDto, bool) _pickNearestTier(
  List<(ManifestVariantDto, int?)> scored,
  int target,
  ManifestVariantDto fallbackDefault,
) {
  for (final (variant, score) in scored) {
    if (score == target) return (variant, false);
  }
  ManifestVariantDto? nearest;
  int? nearestDiff;
  for (final (variant, score) in scored) {
    if (score == null) continue;
    final diff = (score - target).abs();
    if (nearestDiff == null || diff < nearestDiff) {
      nearestDiff = diff;
      nearest = variant;
    }
  }
  if (nearest != null) return (nearest, true);
  return (fallbackDefault, true);
}

/// 合并策略基准选择与 per-item 覆盖（用户在"逐项调整"里为单个条目手选了
/// 别的 variantId）。覆盖优先；`overrides` 里只应含有 variants 非空的条目。
Map<String, String?> resolveEffectiveManifestVariants(
  ManifestPolicyResult policyResult,
  Map<String, String> overrides,
) {
  return {
    for (final choice in policyResult.choices)
      choice.itemId: overrides[choice.itemId] ?? choice.variantId,
  };
}

// =============================================================================
// 6. resolver_item 拼接
// =============================================================================

/// 拼接 `resolver_item`：无 variantId（或空串）→ `<itemId>`；否则
/// `<itemId>@<variantId>`（与引擎 `manifest_item_resolver_token` 拼接规则
/// 一致，见 hub-final-signals.md §3）。
String buildManifestResolverItem(String itemId, String? variantId) {
  if (variantId == null || variantId.isEmpty) return itemId;
  return '$itemId@$variantId';
}

/// 由当前选中集合 + 生效 variant 表构建 [CreateTaskGroup.items]。variant
/// 命中时按该 variant 的 size 透传（画质不同体积不同），否则用 item.size。
List<GroupItemEntry> buildManifestGroupItems(
  List<ManifestItemDto> items,
  Set<String> selectedItemIds,
  Map<String, String?> effectiveVariants,
) {
  final result = <GroupItemEntry>[];
  for (final item in items) {
    if (!selectedItemIds.contains(item.id)) continue;
    final variantId = effectiveVariants[item.id];
    result.add(
      GroupItemEntry(
        resolverItem: buildManifestResolverItem(item.id, variantId),
        fileName: item.name,
        relPath: item.path,
        size: _effectiveItemSize(item, variantId),
      ),
    );
  }
  return result;
}

int _effectiveItemSize(ManifestItemDto item, String? variantId) {
  if (variantId == null) return item.size;
  for (final v in item.variants) {
    if (v.id == variantId) return v.size;
  }
  return item.size;
}

// =============================================================================
// 7. 组名默认值
// =============================================================================

/// 组名默认值：优先用清单自带的 `name`；为空时退化用来源 URL 最后一段
/// （去查询串/末尾斜杠）；全部拿不到时返回空串——交调用方套用本地化占位符
/// （保持本文件零 i18n 依赖）。
String manifestDefaultGroupName(String manifestName, String sourceUrl) {
  final trimmed = manifestName.trim();
  if (trimmed.isNotEmpty) return trimmed;
  try {
    final uri = Uri.parse(sourceUrl);
    final segments = uri.pathSegments.where((s) => s.isNotEmpty).toList();
    if (segments.isNotEmpty) return segments.last;
  } catch (_) {
    // 解析失败：忽略，落到下方空串兜底。
  }
  return '';
}

// =============================================================================
// 8. 剧集 / 合集智能建议启发式
// =============================================================================

/// 建议条结果：选中集合 + 计数（供"正片+字幕 N 项"文案）。
class ManifestEpisodeSuggestion {
  final Set<String> itemIds;
  final int count;
  const ManifestEpisodeSuggestion({required this.itemIds, required this.count});
}

final RegExp _kEpisodeNumberPattern = RegExp(
  r'(?:s\d{1,2}\s*e\d{1,3})' // S01E02
  r'|(?:\bep?\.?\s?\d{1,3}\b)' // EP01 / E01 / Ep.1
  r'|(?:第\s?\d{1,3}\s?[集话回])' // 第01集/话/回
  r'|(?:[-_\s]\d{1,3}(?=[-_\s.]|$))', // "- 01 -" / "_01."
  caseSensitive: false,
);

const _kExtraKeywords = ['sample', 'trailer', '预告', '花絮', 'nfo', 'poster'];
const _kSubtitleExts = {'srt', 'ass', 'ssa', 'vtt', 'sub', 'idx'};

/// 去扩展名的文件名主干（用于正片-字幕文件名匹配）。
String _manifestFileStem(String fileName) {
  final ext = manifestFileExtension(fileName);
  if (ext.isEmpty) return fileName;
  return fileName.substring(0, fileName.length - ext.length - 1);
}

/// 检测文件名里的编号模式（SxxEyy / EPxx / 第xx集 / 分隔符包裹的纯数字序号
/// 等），当足够多视频条目命中时，建议"正片 + 匹配字幕"的选集。
///
/// 置信度不足（视频条目数 < 2、或命中比例 < 60%、或排除花絮/样片后无剩余
/// 正片）返回 null——调用方隐藏建议条。
ManifestEpisodeSuggestion? detectManifestEpisodeSuggestion(
  List<ManifestItemDto> items,
) {
  final videoItems = items
      .where((i) => manifestItemCategory(i) == FileCategory.video)
      .toList();
  if (videoItems.length < 2) return null;

  final matched = videoItems
      .where((i) => _kEpisodeNumberPattern.hasMatch(i.name))
      .toList();
  if (matched.isEmpty) return null;
  final confidence = matched.length / videoItems.length;
  if (confidence < 0.6) return null;

  final mains = matched
      .where(
        (i) => !_kExtraKeywords.any((w) => i.name.toLowerCase().contains(w)),
      )
      .toList();
  if (mains.isEmpty) return null;

  final mainIds = mains.map((i) => i.id).toSet();
  final mainStems = mains.map((i) => _manifestFileStem(i.name)).toSet();

  final subtitleIds = items
      .where((i) {
        final ext = manifestFileExtension(i.name);
        if (!_kSubtitleExts.contains(ext)) return false;
        final stem = _manifestFileStem(i.name);
        return mainStems.any((s) => stem.contains(s) || s.contains(stem));
      })
      .map((i) => i.id);

  final ids = {...mainIds, ...subtitleIds};
  return ManifestEpisodeSuggestion(itemIds: ids, count: ids.length);
}
