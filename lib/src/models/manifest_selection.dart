// 预解析清单（ResolvePreviewResult.items）→ 建组选择弹窗 的纯逻辑层（v1.6 下钻导航版）。
//
// 不依赖 Flutter（可在 VM 单测里直接跑）：可见性判定（扩展名筛选 + 搜索）、
// 当前层行流生成（单链目录合并 + 目录/文件排序）、返回上级跳级、目录子树
// id 收集/三态勾选、全局选中统计、扩展名频次 top7、CreateTaskGroup.items
// 投影。面包屑分段模型（含 >4 段折叠）独立在 manifest_breadcrumb.dart。
//
// 行为权威：design/desktop-task-views/manifest.js（下钻导航范式的高保真实现，
// 本文件逐条对齐其 mfTree/mfRows/mfUp/mfCrumbHtml/mfDirStat/mfDirFiles/
// mfSelStat/mfExtChips 语义）。设计依据：docs/multi-file-task-group-design.md
// §4.4/§4.6、design/desktop-task-views/DESIGN.md §4.10。
//
// v1.5 裁决已砍除：画质规格策略（variants/ManifestQualityPolicy）、剧集启发式
// 建议条、文件类型意图按钮组、树形缩进渲染——resolver_item 恒为 `<itemId>`
// （不带 @variantId，规格选择留给插件默认档）。
//
// UI 层（manifest_select_dialog.dart / manifest_browse_list.dart /
// manifest_advanced_panel.dart）只负责渲染与持有交互状态（cwd/选中集合/
// 筛选/搜索词/排序键/高级选项控制器），实际计算全部委托本文件。

import '../bindings/bindings.dart'
    show GroupItemEntry, ManifestItemDto;
import 'download_task.dart' show FileCategory;

// =============================================================================
// 1. 可见性（扩展名筛选 ∧ 搜索词匹配）
// =============================================================================

/// 从文件名提取扩展名（不含点号，小写）；无扩展名（或以 `.` 开头的隐藏
/// 文件）返回空串。供 [manifestItemCategory]（文件类型着色）使用。
String manifestFileExtension(String fileName) {
  final idx = fileName.lastIndexOf('.');
  if (idx <= 0 || idx == fileName.length - 1) return '';
  return fileName.substring(idx + 1).toLowerCase();
}

/// 扩展名筛选 chip / 行内展示用：大写，无扩展名回退 `"FILE"`
/// （对齐 manifest.js `mfExt`）。
String manifestExtensionLabel(String fileName) {
  final ext = manifestFileExtension(fileName);
  return ext.isEmpty ? 'FILE' : ext.toUpperCase();
}

/// 文件类型分类（文件行扩展名色块 tile 着色用），复用 [FileCategory] 8 类
/// 判定。
FileCategory manifestItemCategory(ManifestItemDto item) =>
    FileCategory.fromExtension(manifestFileExtension(item.name));

/// 当前搜索词是否处于「搜索态」（非空白）。搜索态下列表切换为全局扁平结果，
/// 面包屑/Backspace 返回上级均不响应（由 UI 层依据本函数结果决定）。
bool manifestIsSearching(String search) => search.trim().isNotEmpty;

/// 条目是否在当前筛选（扩展名 chips）+ 搜索词下可见。
bool manifestItemVisible(
  ManifestItemDto item, {
  required Set<String> extFilter,
  required String search,
}) {
  if (extFilter.isNotEmpty && !extFilter.contains(manifestExtensionLabel(item.name))) {
    return false;
  }
  final q = search.trim().toLowerCase();
  if (q.isEmpty) return true;
  return item.name.toLowerCase().contains(q);
}

// =============================================================================
// 2. 扩展名频次 top7（工具栏 chips，恒对全量条目统计，不随当前筛选变化）
// =============================================================================

class ManifestExtChip {
  final String ext;
  final int count;
  const ManifestExtChip({required this.ext, required this.count});
}

/// 按出现频次取前 [limit] 个扩展名（计数相同时按扩展名本身排序，保证结果
/// 确定性——manifest.js 依赖 JS `Array.sort` 的稳定排序，Dart 版用显式次级键
/// 达到同等确定性，非行为偏离）。
List<ManifestExtChip> manifestTopExtensions(
  List<ManifestItemDto> items, {
  int limit = 7,
}) {
  final counts = <String, int>{};
  for (final it in items) {
    final ext = manifestExtensionLabel(it.name);
    counts[ext] = (counts[ext] ?? 0) + 1;
  }
  final chips = [
    for (final e in counts.entries) ManifestExtChip(ext: e.key, count: e.value),
  ]..sort((a, b) {
    final byCount = b.count.compareTo(a.count);
    return byCount != 0 ? byCount : a.ext.compareTo(b.ext);
  });
  return chips.length > limit ? chips.sublist(0, limit) : chips;
}

// =============================================================================
// 3. 下钻导航：当前层行流（单链合并 + 目录/文件排序，零缩进）
// =============================================================================

enum ManifestSortKey { name, size }

/// 一条当前层的目录行：单链合并后的展示（[labels] = 折叠链各段名，UI 拼接
/// 为 `a / b / c`，末段加粗）；[path] = 链末端真实目录的完整路径（导航目标 /
/// 选中集合 key）。
class ManifestDirRow {
  final String path;
  final List<String> labels;
  final int count;
  final int size;
  final int selCnt;
  final bool unknown;

  const ManifestDirRow({
    required this.path,
    required this.labels,
    required this.count,
    required this.size,
    required this.selCnt,
    required this.unknown,
  });
}

/// 一条文件行；[showPath] 仅搜索模式（全局扁平结果）为 true，UI 据此渲染
/// 灰色「直接父目录/」前缀。
class ManifestFileRow {
  final ManifestItemDto item;
  final bool showPath;
  const ManifestFileRow({required this.item, required this.showPath});
}

sealed class ManifestRow {
  const ManifestRow();
}

class ManifestDirRowEntry extends ManifestRow {
  final ManifestDirRow row;
  const ManifestDirRowEntry(this.row);
}

class ManifestFileRowEntry extends ManifestRow {
  final ManifestFileRow row;
  const ManifestFileRowEntry(this.row);
}

/// [manifestRowsAt] 的结果：[cwd] 是筛选后落地的实际当前目录（未失效时等于
/// 请求的 cwd；请求的层被筛空时回退根 `""`——调用方应据此 setState 同步
/// cwd）。
class ManifestRowsResult {
  final String cwd;
  final List<ManifestRow> rows;
  const ManifestRowsResult({required this.cwd, required this.rows});
}

/// 目录构建的内部节点（仅本文件内使用）：按名分组的子目录 + 该层直属文件。
class _DirBuilder {
  final Map<String, _DirBuilder> dirs = {};
  final List<ManifestItemDto> files = [];
}

/// 由可见 items 构建完整目录树（按 `path` 以 `/` 拆分目录段）。
_DirBuilder _buildVisibleDirTree(
  List<ManifestItemDto> items, {
  required Set<String> extFilter,
  required String search,
}) {
  final root = _DirBuilder();
  for (final item in items) {
    if (!manifestItemVisible(item, extFilter: extFilter, search: search)) {
      continue;
    }
    var node = root;
    final segments = item.path.split('/').where((s) => s.isNotEmpty);
    for (final seg in segments) {
      node = node.dirs.putIfAbsent(seg, () => _DirBuilder());
    }
    node.files.add(item);
  }
  return root;
}

/// 按 `/` 拆分的路径在树中定位节点；筛选后该层不存在时返回 null（调用方
/// 回退根）。
_DirBuilder? _nodeAt(_DirBuilder root, String path) {
  if (path.isEmpty) return root;
  var node = root;
  for (final seg in path.split('/')) {
    final next = node.dirs[seg];
    if (next == null) return null;
    node = next;
  }
  return node;
}

class _DirAggregate {
  final int count;
  final int size;
  final int selCnt;
  final bool unknown;
  const _DirAggregate({
    required this.count,
    required this.size,
    required this.selCnt,
    required this.unknown,
  });
}

/// 子树内全部文件的聚合统计：计数 / Σsize（`size==0` 视为未知，不计入
/// 总和但置 unknown）/ 已选计数 / 是否含未知大小项。
_DirAggregate _dirStat(_DirBuilder node, Set<String> selectedItemIds) {
  var count = 0;
  var size = 0;
  var selCnt = 0;
  var unknown = false;
  void walk(_DirBuilder n) {
    for (final f in n.files) {
      count++;
      if (f.size == 0) {
        unknown = true;
      } else {
        size += f.size;
      }
      if (selectedItemIds.contains(f.id)) selCnt++;
    }
    for (final d in n.dirs.values) {
      walk(d);
    }
  }

  walk(node);
  return _DirAggregate(count: count, size: size, selCnt: selCnt, unknown: unknown);
}

/// 文件排序键：`size` 时未知大小（`size==0`）视为 -1，排到末尾（对齐
/// manifest.js `b.size ?? -1`）。
int _fileSizeRank(ManifestItemDto item) => item.size == 0 ? -1 : item.size;

/// 目录名 / 文件名排序：Dart 无 intl 依赖（禁止新增依赖），用 `String.
/// compareTo` 做序数比较，非 locale-aware collation——记录为对
/// manifest.js `localeCompare('zh-Hans-CN')` 的可移植性偏离（沿用本文件
/// 重写前既有实现的一贯约定）。
List<ManifestItemDto> _sortFiles(List<ManifestItemDto> files, ManifestSortKey key) {
  final list = [...files];
  if (key == ManifestSortKey.size) {
    list.sort((a, b) => _fileSizeRank(b).compareTo(_fileSizeRank(a)));
  } else {
    list.sort((a, b) => a.name.compareTo(b.name));
  }
  return list;
}

/// 当前层行流：搜索态返回全局扁平结果（文件行 `showPath=true`）；否则返回
/// [cwd] 直属子目录（单链合并）+ 直属文件（目录恒在前，各自再按 [sortKey]
/// 排序——目录恒按名排序，只有文件排序受 [sortKey] 影响，对齐 mfRows）。
/// [cwd] 被筛空时结果的 [ManifestRowsResult.cwd] 回退为 `""`。
ManifestRowsResult manifestRowsAt({
  required List<ManifestItemDto> items,
  required String cwd,
  required Set<String> selectedItemIds,
  required Set<String> extFilter,
  required String search,
  required ManifestSortKey sortKey,
}) {
  if (manifestIsSearching(search)) {
    final visible = items
        .where((it) => manifestItemVisible(it, extFilter: extFilter, search: search))
        .toList();
    final sorted = _sortFiles(visible, sortKey);
    return ManifestRowsResult(
      cwd: cwd,
      rows: [
        for (final it in sorted)
          ManifestFileRowEntry(ManifestFileRow(item: it, showPath: true)),
      ],
    );
  }

  final root = _buildVisibleDirTree(items, extFilter: extFilter, search: search);
  var node = _nodeAt(root, cwd);
  var effectiveCwd = cwd;
  if (node == null) {
    node = root;
    effectiveCwd = '';
  }

  final rows = <ManifestRow>[];
  final dirNames = node.dirs.keys.toList()..sort();
  for (final name in dirNames) {
    var childPath = effectiveCwd.isEmpty ? name : '$effectiveCwd/$name';
    var child = node.dirs[name]!;
    final labels = <String>[name];
    // 单链合并：仅单个子目录且无直属文件的链持续下潜合并，直到遇到分叉
    // （>1 子项）或该级出现文件为止。
    while (child.files.isEmpty && child.dirs.length == 1) {
      final nextName = child.dirs.keys.first;
      labels.add(nextName);
      childPath = '$childPath/$nextName';
      child = child.dirs[nextName]!;
    }
    final stat = _dirStat(child, selectedItemIds);
    rows.add(
      ManifestDirRowEntry(
        ManifestDirRow(
          path: childPath,
          labels: labels,
          count: stat.count,
          size: stat.size,
          selCnt: stat.selCnt,
          unknown: stat.unknown,
        ),
      ),
    );
  }
  for (final it in _sortFiles(node.files, sortKey)) {
    rows.add(ManifestFileRowEntry(ManifestFileRow(item: it, showPath: false)));
  }
  return ManifestRowsResult(cwd: effectiveCwd, rows: rows);
}

// =============================================================================
// 4. 目录三态勾选 / 子树选择
// =============================================================================

enum ManifestCheckState { checked, unchecked, indeterminate }

/// 由行内已计算好的 selCnt/count 推导三态（无需重新遍历）。
ManifestCheckState manifestDirRowCheckState(ManifestDirRow row) {
  if (row.selCnt == 0) return ManifestCheckState.unchecked;
  if (row.selCnt == row.count) return ManifestCheckState.checked;
  return ManifestCheckState.indeterminate;
}

/// 目录子树（[dirPath] 或其任意下级路径）下全部可见文件 id（对齐
/// manifest.js `mfDirFiles`——用于目录行勾选框的整树选择/取消）。
Set<String> manifestDirFileIds({
  required List<ManifestItemDto> items,
  required String dirPath,
  required Set<String> extFilter,
  required String search,
}) {
  final result = <String>{};
  for (final it in items) {
    if (!manifestItemVisible(it, extFilter: extFilter, search: search)) continue;
    if (it.path == dirPath || it.path.startsWith('$dirPath/')) result.add(it.id);
  }
  return result;
}

/// 切换目录子树选择：子树内文件全部已选则整体取消，否则整体选中（与
/// manifest.js `all ? delete : add` 一致）。
Set<String> manifestToggleDirSubtree({
  required List<ManifestItemDto> items,
  required String dirPath,
  required Set<String> selectedItemIds,
  required Set<String> extFilter,
  required String search,
}) {
  final ids = manifestDirFileIds(
    items: items,
    dirPath: dirPath,
    extFilter: extFilter,
    search: search,
  );
  final allSelected = ids.every(selectedItemIds.contains);
  final next = Set<String>.from(selectedItemIds);
  if (allSelected) {
    next.removeAll(ids);
  } else {
    next.addAll(ids);
  }
  return next;
}

// =============================================================================
// 5. 返回上级（跳过纯过渡层）
// =============================================================================

/// 返回上级：逐段 pop 直到该层有直属文件、或有 >1 个子目录、或到根——纯
/// 过渡层（单链合并链的中间段）不是独立可停留的层级，去程回程都不经过
/// （对齐 manifest.js `mfUp`）。搜索态不调用本函数（由 UI 层保证）。
String manifestUpPath({
  required List<ManifestItemDto> items,
  required String cwd,
  required Set<String> extFilter,
}) {
  if (cwd.isEmpty) return '';
  final root = _buildVisibleDirTree(items, extFilter: extFilter, search: '');
  final segs = cwd.split('/');
  do {
    segs.removeLast();
    final node = _nodeAt(root, segs.join('/'));
    if (segs.isEmpty || (node != null && (node.files.isNotEmpty || node.dirs.length > 1))) {
      break;
    }
  } while (segs.isNotEmpty);
  return segs.join('/');
}


// =============================================================================
// 6. 全局选择操作（作用域 = 全部可见文件，跨层级）
// =============================================================================

/// 全选：整体替换为当前可见文件集合（对齐 manifest.js `mf.sel = new
/// Set(visible)`——筛选范围外此前已选的条目会被丢弃，作用域纪律的直接结果，
/// 非增量并集）。
Set<String> manifestSelectAllVisible(
  List<ManifestItemDto> items, {
  required Set<String> extFilter,
  required String search,
}) {
  final result = <String>{};
  for (final it in items) {
    if (manifestItemVisible(it, extFilter: extFilter, search: search)) {
      result.add(it.id);
    }
  }
  return result;
}

/// 反选：整体替换为「当前可见且此前未选中」的集合（同上，非增量——可见
/// 范围外的已选条目同样被丢弃）。
Set<String> manifestInvertVisibleSelection(
  List<ManifestItemDto> items,
  Set<String> selectedItemIds, {
  required Set<String> extFilter,
  required String search,
}) {
  final result = <String>{};
  for (final it in items) {
    if (!manifestItemVisible(it, extFilter: extFilter, search: search)) continue;
    if (!selectedItemIds.contains(it.id)) result.add(it.id);
  }
  return result;
}

// =============================================================================
// 7. 选中统计 / 清单汇总
// =============================================================================

class ManifestSelectionStat {
  final int count;
  final int size;
  final int unknownCount;
  const ManifestSelectionStat({
    required this.count,
    required this.size,
    required this.unknownCount,
  });
}

/// 全局已选统计：计数 / Σsize（`size==0` 视为未知，计入 [unknownCount] 但
/// 不计入 [size]）——底栏「已选 N 项 · ≈ 大小 （M 项大小未知）」文案数据源。
ManifestSelectionStat manifestSelectionStat(
  List<ManifestItemDto> items,
  Set<String> selectedItemIds,
) {
  var size = 0;
  var unknown = 0;
  for (final it in items) {
    if (!selectedItemIds.contains(it.id)) continue;
    if (it.size == 0) {
      unknown++;
    } else {
      size += it.size;
    }
  }
  return ManifestSelectionStat(
    count: selectedItemIds.length,
    size: size,
    unknownCount: unknown,
  );
}

/// 清单总大小（摘要区「N 项 · 总大小」，不做未知标注——与 manifest.js
/// `mfBodyHtml` 头部摘要一致）。
int manifestTotalSize(List<ManifestItemDto> items) =>
    items.fold(0, (sum, i) => sum + i.size);

// =============================================================================
// 8. 组名默认值 / 来源站点
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

/// 摘要区副标题「来源站点」：`Uri.parse(sourceUrl).host`；解析失败返回
/// 空串（调用方隐藏该片段）。
String manifestSourceHost(String sourceUrl) {
  try {
    return Uri.parse(sourceUrl).host;
  } catch (_) {
    return '';
  }
}

// =============================================================================
// 9. 高级选项（组级，随 CreateTaskGroup 下发全部子任务）
// =============================================================================

class ManifestHeaderEntry {
  final String key;
  final String value;
  const ManifestHeaderEntry({required this.key, required this.value});
}

/// 组级高级选项快照（纯数据，供 dirty 判定/发送前收集；文本字段在 UI 层用
/// [TextEditingController] 承载，提交时读出 `.text` 组装本类实例）。
class ManifestAdvancedOptions {
  final String proxyUrl;
  final bool ignoreTlsErrors;

  /// true = 继承全局 UA（发送时 userAgent 应发空串）；false = 自定义。
  final bool uaInherit;
  final String userAgent;
  final String cookies;

  /// 每子任务线程数；0 = 自动。
  final int segments;
  final List<ManifestHeaderEntry> headers;

  const ManifestAdvancedOptions({
    required this.proxyUrl,
    required this.ignoreTlsErrors,
    required this.uaInherit,
    required this.userAgent,
    required this.cookies,
    required this.segments,
    required this.headers,
  });
}

/// 高级选项是否偏离默认（折叠条圆点用），对齐 manifest.js `mfAdvDirty`。
bool manifestAdvancedOptionsDirty(ManifestAdvancedOptions options) {
  return options.proxyUrl.trim().isNotEmpty ||
      options.ignoreTlsErrors ||
      (!options.uaInherit && options.userAgent.trim().isNotEmpty) ||
      options.cookies.trim().isNotEmpty ||
      options.segments != 0 ||
      options.headers.any((h) => h.key.trim().isNotEmpty || h.value.trim().isNotEmpty);
}

/// 自定义请求头行 → 生效 Map：丢弃 key 或 value 为空的行，同名 key 后者
/// 覆盖前者（与 new_download_dialog.dart `_extraHeaders` 约定一致）。
Map<String, String> manifestEffectiveHeaders(List<ManifestHeaderEntry> headers) {
  final result = <String, String>{};
  for (final h in headers) {
    final key = h.key.trim();
    final value = h.value.trim();
    if (key.isEmpty || value.isEmpty) continue;
    result[key] = value;
  }
  return result;
}

// =============================================================================
// 10. CreateTaskGroup.items 投影
// =============================================================================

/// 由选中集合构建 [CreateTaskGroup.items]：resolver_item 恒为 `<itemId>`
/// （v1.6 裁决——规格/变体选择不在本弹窗，留给插件默认档）。
List<GroupItemEntry> buildManifestGroupItems(
  List<ManifestItemDto> items,
  Set<String> selectedItemIds,
) {
  final result = <GroupItemEntry>[];
  for (final item in items) {
    if (!selectedItemIds.contains(item.id)) continue;
    result.add(
      GroupItemEntry(
        resolverItem: item.id,
        fileName: item.name,
        relPath: item.path,
        size: item.size,
      ),
    );
  }
  return result;
}
