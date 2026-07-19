// 任务列表视图系统 — 偏好模型 + 持久化 store。
//
// 行为规格依据：design/desktop-task-views/DESIGN.md §3 + design-proto-spec.md §1。
// 默认值 = 现状行为快照（列表·舒适·智能分组·智能排序·显示已完成·协议徽标开·
// 默认列 {进度,速度,剩余时间,状态}），保证升级零感知（DESIGN P1）。

import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../i18n/locale_provider.dart';
import '../services/kv_store.dart';

/// 视图形态：列表 / 网格（bento）。
enum ViewForm { list, grid }

/// 列表密度：舒适 64px / 紧凑 44px。网格形态下密度控件禁用（对渲染无效）。
enum ViewDensity { comfortable, compact }

/// 分组维度（7 维，含「不分组」）。
enum ViewGroupBy { smart, date, status, type, queue, site, none }

/// 排序键（6 键）。`smart` 只定桶内行序；显式键同时决定桶间顺序
/// （「排序控全局叙事」，见 download_controller.dart `orderSections`）。
enum ViewSortKey { smart, created, name, size, progress, speed }

/// 排序方向。
enum SortDir { asc, desc }

/// 任务列注册表的列 ID（9 列，仅作用于任务行；组卡片不受列配置影响）。
enum TaskColumnId {
  progress,
  size,
  created,
  protocol,
  source,
  queue,
  speed,
  eta,
  status,
}

/// 每个排序键切换时重置到的默认方向（`SORT_DEFAULT_DIR`，design-proto-spec §3）。
const Map<ViewSortKey, SortDir> kSortKeyDefaultDir = {
  ViewSortKey.smart: SortDir.desc,
  ViewSortKey.created: SortDir.desc,
  ViewSortKey.name: SortDir.asc,
  ViewSortKey.size: SortDir.desc,
  ViewSortKey.progress: SortDir.desc,
  ViewSortKey.speed: SortDir.desc,
};

/// 分组维度循环顺序（`G` 快捷键 / 面板 chips 顺序）。
const List<ViewGroupBy> kGroupByCycle = [
  ViewGroupBy.smart,
  ViewGroupBy.date,
  ViewGroupBy.status,
  ViewGroupBy.type,
  ViewGroupBy.queue,
  ViewGroupBy.site,
  ViewGroupBy.none,
];

/// 排序键循环顺序（`S` 快捷键 / 面板行顺序）。
const List<ViewSortKey> kSortKeyCycle = [
  ViewSortKey.smart,
  ViewSortKey.created,
  ViewSortKey.name,
  ViewSortKey.size,
  ViewSortKey.progress,
  ViewSortKey.speed,
];

extension ViewFormLabel on ViewForm {
  String get label => switch (this) {
    ViewForm.list => currentS.viewFormList,
    ViewForm.grid => currentS.viewFormGrid,
  };
}

extension ViewDensityLabel on ViewDensity {
  String get label => switch (this) {
    ViewDensity.comfortable => currentS.viewDensityComfortable,
    ViewDensity.compact => currentS.viewDensityCompact,
  };
}

extension ViewGroupByLabel on ViewGroupBy {
  String get label => switch (this) {
    ViewGroupBy.smart => currentS.viewGroupSmart,
    ViewGroupBy.date => currentS.viewGroupDate,
    ViewGroupBy.status => currentS.viewGroupStatus,
    ViewGroupBy.type => currentS.viewGroupType,
    ViewGroupBy.queue => currentS.viewGroupQueue,
    ViewGroupBy.site => currentS.viewGroupSite,
    ViewGroupBy.none => currentS.viewGroupNone,
  };
}

extension ViewSortKeyLabel on ViewSortKey {
  String get label => switch (this) {
    ViewSortKey.smart => currentS.viewSortSmart,
    ViewSortKey.created => currentS.viewSortCreated,
    ViewSortKey.name => currentS.viewSortName,
    ViewSortKey.size => currentS.viewSortSize,
    ViewSortKey.progress => currentS.viewSortProgress,
    ViewSortKey.speed => currentS.viewSortSpeed,
  };
}

/// 任务列表视图偏好：形态/密度/分组/排序/显示属性/列，全部不可变值。
class ViewPrefs {
  final ViewForm form;
  final ViewDensity density;
  final ViewGroupBy groupBy;
  final ViewSortKey sortKey;
  final SortDir sortDir;
  final bool showCompleted;
  final bool protocolBadges;
  final Set<TaskColumnId> columns;

  const ViewPrefs({
    required this.form,
    required this.density,
    required this.groupBy,
    required this.sortKey,
    required this.sortDir,
    required this.showCompleted,
    required this.protocolBadges,
    required this.columns,
  });

  /// 出厂默认列集（`BASE_PREFS.columns`）。
  static const Set<TaskColumnId> defaultColumns = {
    TaskColumnId.progress,
    TaskColumnId.speed,
    TaskColumnId.eta,
    TaskColumnId.status,
  };

  /// 出厂默认值（`BASE_PREFS`，design-proto-spec §1）。
  factory ViewPrefs.defaults() => const ViewPrefs(
    form: ViewForm.list,
    density: ViewDensity.comfortable,
    groupBy: ViewGroupBy.smart,
    sortKey: ViewSortKey.smart,
    sortDir: SortDir.desc,
    showCompleted: true,
    protocolBadges: true,
    columns: defaultColumns,
  );

  ViewPrefs copyWith({
    ViewForm? form,
    ViewDensity? density,
    ViewGroupBy? groupBy,
    ViewSortKey? sortKey,
    SortDir? sortDir,
    bool? showCompleted,
    bool? protocolBadges,
    Set<TaskColumnId>? columns,
  }) => ViewPrefs(
    form: form ?? this.form,
    density: density ?? this.density,
    groupBy: groupBy ?? this.groupBy,
    sortKey: sortKey ?? this.sortKey,
    sortDir: sortDir ?? this.sortDir,
    showCompleted: showCompleted ?? this.showCompleted,
    protocolBadges: protocolBadges ?? this.protocolBadges,
    columns: columns ?? this.columns,
  );

  /// 是否偏离出厂默认（顶栏圆点判定依据，design-proto-spec `isDefaultView`）。
  bool get isDefault => this == ViewPrefs.defaults();

  Map<String, Object?> toJson() => {
    'form': form.name,
    'density': density.name,
    'groupBy': groupBy.name,
    'sortKey': sortKey.name,
    'sortDir': sortDir.name,
    'showCompleted': showCompleted,
    'protocolBadges': protocolBadges,
    'columns': columns.map((c) => c.name).toList(),
  };

  /// 从 JSON 反序列化；字段缺失/类型不符/枚举值未知逐项回退默认值
  /// （schema 演进容错，先例 `FluxMetricTokens.fromJson`）。
  factory ViewPrefs.fromJson(Map<String, Object?> json) {
    final d = ViewPrefs.defaults();
    return ViewPrefs(
      form: _enumFrom(ViewForm.values, json['form'], d.form),
      density: _enumFrom(ViewDensity.values, json['density'], d.density),
      groupBy: _enumFrom(ViewGroupBy.values, json['groupBy'], d.groupBy),
      sortKey: _enumFrom(ViewSortKey.values, json['sortKey'], d.sortKey),
      sortDir: _enumFrom(SortDir.values, json['sortDir'], d.sortDir),
      showCompleted: json['showCompleted'] is bool
          ? json['showCompleted'] as bool
          : d.showCompleted,
      protocolBadges: json['protocolBadges'] is bool
          ? json['protocolBadges'] as bool
          : d.protocolBadges,
      columns: _columnsFrom(json['columns'], d.columns),
    );
  }

  static T _enumFrom<T extends Enum>(List<T> values, Object? raw, T fallback) {
    if (raw is! String) return fallback;
    for (final v in values) {
      if (v.name == raw) return v;
    }
    return fallback;
  }

  static Set<TaskColumnId> _columnsFrom(
    Object? raw,
    Set<TaskColumnId> fallback,
  ) {
    if (raw is! List) return fallback;
    final result = <TaskColumnId>{};
    for (final entry in raw) {
      if (entry is! String) continue;
      for (final c in TaskColumnId.values) {
        if (c.name == entry) {
          result.add(c);
          break;
        }
      }
    }
    return result.isEmpty ? fallback : result;
  }

  @override
  bool operator ==(Object other) {
    if (identical(this, other)) return true;
    return other is ViewPrefs &&
        other.form == form &&
        other.density == density &&
        other.groupBy == groupBy &&
        other.sortKey == sortKey &&
        other.sortDir == sortDir &&
        other.showCompleted == showCompleted &&
        other.protocolBadges == protocolBadges &&
        other.columns.length == columns.length &&
        other.columns.containsAll(columns);
  }

  @override
  int get hashCode => Object.hash(
    form,
    density,
    groupBy,
    sortKey,
    sortDir,
    showCompleted,
    protocolBadges,
    // Set 无序 — 用异或聚合各元素哈希，保证与 == 一致（集合内容相同则 hash 相同）。
    columns.fold<int>(0, (acc, c) => acc ^ c.hashCode),
  );
}

/// 组合视图状态文本（顶栏「显示选项」按钮 tooltip / 状态栏右端回显共用）：
/// `<列表[· 密度]/网格> · 按<分组>分组 · <排序>排序`（网格形态无密度段）。
String describeViewState(ViewPrefs prefs) {
  final s = currentS;
  final formPart = prefs.form == ViewForm.list
      ? '${prefs.form.label} · ${prefs.density.label}'
      : prefs.form.label;
  return '$formPart · ${s.statusViewGroupedByLabel(prefs.groupBy.label)} · '
      '${s.statusViewSortedByLabel(prefs.sortKey.label)}';
}

/// KvStore 持久化的视图偏好 store：全局一份 + 按状态页签独立覆盖层。
///
/// 键：`view_prefs`（全局，未覆盖页签的回退值，出厂状态下恒等于
/// [ViewPrefs.defaults]——v1 无「设为全局默认」入口，仅为结构预留）+
/// `view_prefs.<tab>`（页签覆盖层，tab 取值同 `StatusTab.name`：
/// all/downloading/completed/paused/error）。
///
/// 语义对齐 design-proto-spec §1（`loadPrefs`）：**未被用户改动过的页签
/// 恒回退出厂默认，不继承其它页签最近一次的改动** —— 每次 [update] 只写入
/// 当前页签自己的覆盖层，从不触碰全局或其它页签。
class ViewPrefsStore extends ChangeNotifier {
  ViewPrefsStore() {
    _global = _decode(KvStore.instance.getString(_kGlobalKey)) ??
        ViewPrefs.defaults();
    for (final tab in kViewPrefsKnownTabs) {
      final decoded = _decode(KvStore.instance.getString(_tabKey(tab)));
      if (decoded != null) _overrides[tab] = decoded;
    }
  }

  static const _kGlobalKey = 'view_prefs';
  static String _tabKey(String tab) => 'view_prefs.$tab';

  late ViewPrefs _global;
  final Map<String, ViewPrefs> _overrides = {};

  /// 全局默认视图偏好（未被任何页签覆盖时的回退值）。
  ViewPrefs get global => _global;

  /// 解析指定页签的有效视图偏好：有覆盖层用覆盖层，否则回退全局默认。
  ViewPrefs resolve(String tab) => _overrides[tab] ?? _global;

  /// 该页签是否已有独立覆盖层（用过自定义视图）。
  bool hasOverride(String tab) => _overrides.containsKey(tab);

  /// 该页签当前有效视图是否等于出厂默认（顶栏圆点判定依据）。
  bool isDefault(String tab) => resolve(tab) == ViewPrefs.defaults();

  /// 对指定页签应用一次偏好变更：写入该页签的覆盖层并持久化 + 广播。
  void update(String tab, ViewPrefs Function(ViewPrefs current) updater) {
    final next = updater(resolve(tab));
    _overrides[tab] = next;
    unawaited(
      KvStore.instance.setString(_tabKey(tab), jsonEncode(next.toJson())),
    );
    notifyListeners();
  }

  /// 重置指定页签为出厂默认（清除该页签覆盖层，面板「重置为默认」逃生舱）。
  void reset(String tab) {
    if (!_overrides.containsKey(tab)) return;
    _overrides.remove(tab);
    unawaited(KvStore.instance.remove(_tabKey(tab)));
    notifyListeners();
  }

  ViewPrefs? _decode(String? raw) {
    if (raw == null || raw.isEmpty) return null;
    try {
      final decoded = jsonDecode(raw);
      if (decoded is Map) {
        return ViewPrefs.fromJson(Map<String, Object?>.from(decoded));
      }
    } catch (_) {
      // 损坏的 JSON 视作未设置，回退默认；不阻塞启动。
    }
    return null;
  }
}

/// 已知的状态页签 key 集合（对应 `StatusTab.name`），构造时预加载覆盖层用。
const List<String> kViewPrefsKnownTabs = [
  'all',
  'downloading',
  'completed',
  'paused',
  'error',
];
