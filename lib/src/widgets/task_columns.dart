// 任务列表视图系统 — 列注册表（表头 + 行共用的单一事实源）。
//
// 行为规格依据：design-proto-spec.md §4（`COLUMNS`/`COL_ORDER`/`colBudget`/
// `tryToggleCol`/`effectiveCols`）。宽度/默认开关/canonical 顺序完全对齐
// 现状硬编码列（进度150/速度90/剩余时间80/状态60），保证默认视图零感知。

import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/download_task.dart';
import '../models/view_prefs.dart';
import '../theme/app_colors.dart';

/// 单列的静态定义：宽度 / 标签 / 单元格渲染器，表头与任务行共用同一份。
class TaskColumnDef {
  final TaskColumnId id;
  final double width;
  final String Function(S s) label;
  final Widget Function(BuildContext context, DownloadTask task) cellBuilder;

  const TaskColumnDef({
    required this.id,
    required this.width,
    required this.label,
    required this.cellBuilder,
  });
}

/// canonical 列序（勾选先后不影响列序；design-proto-spec §4 `COL_ORDER`）。
const List<TaskColumnId> kColumnCanonicalOrder = [
  TaskColumnId.progress,
  TaskColumnId.size,
  TaskColumnId.created,
  TaskColumnId.protocol,
  TaskColumnId.source,
  TaskColumnId.queue,
  TaskColumnId.speed,
  TaskColumnId.eta,
  TaskColumnId.status,
];

/// 列宽预算固定量：行首 padding16 + 图标34 + 间距12 + 名称保底90 + 右缘16。
const double kColumnBudgetReserved = 16 + 34 + 12 + 90 + 16; // = 168

/// 给定列表区宽度，计算可用于列的宽度预算（超出即拒绝勾选新列）。
double columnWidthBudget(double listWidth) => listWidth - kColumnBudgetReserved;

/// 一组列 id 的总宽度。
double columnsTotalWidth(Iterable<TaskColumnId> ids) =>
    ids.fold(0.0, (sum, id) => sum + kTaskColumns[id]!.width);

/// 列的「保留重要性」序（高→低）。渲染期宽度不足时按此序**从尾部**裁列
/// （来源/队列/协议/创建时间 等低优列先隐藏，进度/状态最后才放弃）。
const List<TaskColumnId> kColumnKeepPriority = [
  TaskColumnId.progress,
  TaskColumnId.status,
  TaskColumnId.speed,
  TaskColumnId.eta,
  TaskColumnId.size,
  TaskColumnId.created,
  TaskColumnId.protocol,
  TaskColumnId.queue,
  TaskColumnId.source,
];

/// 渲染期自适应裁列：勾选护栏（[tryToggleColumn]）只在切换那一刻校验，
/// 之后窗口变窄/详情面板打开都可能让已勾选组合超出当前预算——固定宽 Row
/// 会直接溢出报错。本函数按 [kColumnKeepPriority] 从低优到高优裁掉放不下
/// 的列（至少保留一列），名称弹性列的 90px 保底因此永不被挤穿
/// （DESIGN §4.1「按优先级自动隐藏低优列，护栏语义保持」）。
/// 返回值保持 [cols] 原有顺序（canonical 序过滤）。
List<TaskColumnId> fitColumnsToWidth(
  List<TaskColumnId> cols,
  double listWidth,
) {
  final budget = columnWidthBudget(listWidth);
  if (columnsTotalWidth(cols) <= budget) return cols;
  final kept = cols.toSet();
  for (final id in kColumnKeepPriority.reversed) {
    if (kept.length <= 1) break;
    if (!kept.remove(id)) continue;
    if (columnsTotalWidth(kept) <= budget) break;
  }
  return [
    for (final id in cols)
      if (kept.contains(id)) id,
  ];
}

/// 尝试切换某列的勾选状态；返回 null 表示成功，否则返回应展示的拒绝提示
/// （i18n key 已解析好的文案）供调用方 toast。
///
/// - 取消勾选：若只剩 1 列则拒绝（至少保留一列护栏）。
/// - 新增勾选：若超出 [listWidth] 对应预算则拒绝。
String? tryToggleColumn({
  required Set<TaskColumnId> current,
  required TaskColumnId toggling,
  required double listWidth,
  required S s,
}) {
  final willEnable = !current.contains(toggling);
  if (!willEnable) {
    if (current.length <= 1) return s.viewColumnsAtLeastOne;
    return null;
  }
  final next = {...current, toggling};
  if (columnsTotalWidth(next) > columnWidthBudget(listWidth)) {
    return s.viewColumnsBudgetExceeded;
  }
  return null;
}

/// 紧凑档「进度→大小」自动切换后的有效列（design-proto-spec §4 `effectiveCols`）。
///
/// 舒适档原样返回（按 canonical 顺序过滤）；紧凑档把 `progress` 映射为
/// `size` 并去重（若用户同时勾了 progress 和 size，紧凑档下只显示一次）。
List<TaskColumnId> effectiveColumns(ViewPrefs prefs) {
  if (prefs.density != ViewDensity.compact) {
    return kColumnCanonicalOrder.where(prefs.columns.contains).toList();
  }
  final mapped = <TaskColumnId>{};
  for (final id in kColumnCanonicalOrder) {
    if (!prefs.columns.contains(id)) continue;
    mapped.add(id == TaskColumnId.progress ? TaskColumnId.size : id);
  }
  return kColumnCanonicalOrder.where(mapped.contains).toList();
}

// =============================================================================
// 共享状态色（进度条/状态列/网格卡/组卡共用，消灭各处重复 switch）
// =============================================================================

/// 状态 → 语义色（design-proto-spec §5 `.ficon.is-*`/`statusCell` 配色表）。
Color taskStatusColor(TaskStatus status, AppColors c, {bool fileMissing = false}) {
  switch (status) {
    case TaskStatus.downloading:
    case TaskStatus.pending:
    case TaskStatus.preparing:
    case TaskStatus.resuming:
      return c.accent;
    case TaskStatus.completed:
      return fileMissing ? AppColors.amber : AppColors.green;
    case TaskStatus.paused:
      return AppColors.amber;
    case TaskStatus.error:
      return AppColors.red;
  }
}

/// 状态图标（design-proto-spec §5 `ST[st].icon`：dl=down,pend=clock,
/// pause=pause,err=alert,done=check；preparing/resuming 视觉上归入下载中）。
IconData taskStatusIcon(TaskStatus status) => switch (status) {
  TaskStatus.downloading || TaskStatus.preparing || TaskStatus.resuming =>
    LucideIcons.arrowDown,
  TaskStatus.pending => LucideIcons.clock,
  TaskStatus.paused => LucideIcons.pause,
  TaskStatus.error => LucideIcons.alertCircle,
  TaskStatus.completed => LucideIcons.check,
};

// =============================================================================
// 单元格渲染辅助
// =============================================================================

Widget _tnumCenter(String text, Color color) => Center(
  child: Text(
    text,
    style: TextStyle(
      fontSize: 12,
      color: color,
      fontFeatures: const [FontFeature.tabularFigures()],
    ),
  ),
);

Widget _ellipsisCenter(BuildContext context, String text) {
  final c = AppColors.of(context);
  return Center(
    child: Text(
      text,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: TextStyle(fontSize: 12, color: c.textSecondary),
    ),
  );
}

/// `今天 16:12` / `昨天 16:12` / `2026-07-18 16:12` 语义化时间格式
/// （design-proto-spec §14 `fmtWhen`）。
String formatWhen(DateTime dt) {
  final now = DateTime.now();
  final today = DateTime(now.year, now.month, now.day);
  final target = DateTime(dt.year, dt.month, dt.day);
  final hm =
      '${dt.hour.toString().padLeft(2, '0')}:${dt.minute.toString().padLeft(2, '0')}';
  final daysDiff = today.difference(target).inDays;
  if (daysDiff <= 0) return '${currentS.today} $hm';
  if (daysDiff == 1) return '${currentS.yesterday} $hm';
  final y = dt.year.toString().padLeft(4, '0');
  final m = dt.month.toString().padLeft(2, '0');
  final d = dt.day.toString().padLeft(2, '0');
  return '$y-$m-$d $hm';
}

// =============================================================================
// 列注册表
// =============================================================================

/// 全部 9 列的静态定义表（表头/行/面板 chips 三入口共用的单一事实源）。
final Map<TaskColumnId, TaskColumnDef> kTaskColumns = {
  TaskColumnId.progress: TaskColumnDef(
    id: TaskColumnId.progress,
    width: 150,
    label: (s) => s.colProgress,
    cellBuilder: (context, task) {
      final c = AppColors.of(context);
      final color = taskStatusColor(
        task.status,
        c,
        fileMissing: task.fileMissing,
      );
      final pct = (task.progress * 100).toStringAsFixed(1);
      return Padding(
        padding: const EdgeInsets.only(right: 12),
        child: Row(
          children: [
            Expanded(
              child: Container(
                height: 3,
                decoration: BoxDecoration(
                  color: c.surface3,
                  borderRadius: const BorderRadius.all(Radius.circular(2)),
                ),
                clipBehavior: Clip.hardEdge,
                child: task.isIndeterminate
                    ? null
                    : FractionallySizedBox(
                        alignment: Alignment.centerLeft,
                        widthFactor: task.progress,
                        child: ColoredBox(color: color),
                      ),
              ),
            ),
            const SizedBox(width: 8),
            Text(
              task.isIndeterminate ? '—' : '$pct%',
              style: TextStyle(
                fontSize: 12,
                color: c.textSecondary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ],
        ),
      );
    },
  ),
  TaskColumnId.size: TaskColumnDef(
    id: TaskColumnId.size,
    width: 80,
    label: (s) => s.colSize,
    cellBuilder: (context, task) =>
        _tnumCenter(task.sizeText, AppColors.of(context).textMuted),
  ),
  TaskColumnId.created: TaskColumnDef(
    id: TaskColumnId.created,
    width: 104,
    label: (s) => s.colCreated,
    cellBuilder: (context, task) =>
        _tnumCenter(formatWhen(task.createdAt), AppColors.of(context).textMuted),
  ),
  TaskColumnId.protocol: TaskColumnDef(
    id: TaskColumnId.protocol,
    width: 64,
    label: (s) => s.colProtocol,
    cellBuilder: (context, task) => _tnumCenter(
      task.siteKey == 'bt' ? 'BT' : task.protocolLabel,
      AppColors.of(context).textMuted,
    ),
  ),
  TaskColumnId.source: TaskColumnDef(
    id: TaskColumnId.source,
    width: 148,
    label: (s) => s.colSource,
    cellBuilder: (context, task) => ShadTooltip(
      builder: (_) => Text(task.siteLabel),
      child: _ellipsisCenter(context, task.siteLabel),
    ),
  ),
  TaskColumnId.queue: TaskColumnDef(
    id: TaskColumnId.queue,
    width: 88,
    label: (s) => s.colQueue,
    cellBuilder: (context, task) {
      final ctrl = DownloadController.globalInstance;
      final label = task.queueId.isEmpty
          ? currentS.ungroupedTasks
          : queueDisplayName(
              currentS,
              ctrl?.queueById(task.queueId) ??
                  DownloadQueue(
                    queueId: task.queueId,
                    name: task.queueId,
                    speedLimitKbps: 0,
                    maxConcurrent: 0,
                    defaultSaveDir: '',
                    position: 0,
                  ),
            );
      return ShadTooltip(
        builder: (_) => Text(label),
        child: _ellipsisCenter(context, label),
      );
    },
  ),
  TaskColumnId.speed: TaskColumnDef(
    id: TaskColumnId.speed,
    width: 90,
    label: (s) => s.colSpeed,
    cellBuilder: (context, task) => _tnumCenter(
      task.speedText,
      task.status == TaskStatus.downloading
          ? AppColors.green
          : AppColors.of(context).textMuted,
    ),
  ),
  TaskColumnId.eta: TaskColumnDef(
    id: TaskColumnId.eta,
    width: 80,
    label: (s) => s.colEta,
    cellBuilder: (context, task) => _tnumCenter(
      task.etaText,
      task.status == TaskStatus.downloading
          ? AppColors.of(context).textSecondary
          : AppColors.of(context).textMuted,
    ),
  ),
  TaskColumnId.status: TaskColumnDef(
    id: TaskColumnId.status,
    width: 60,
    label: (s) => s.colStatus,
    cellBuilder: (context, task) {
      final c = AppColors.of(context);
      final color = taskStatusColor(
        task.status,
        c,
        fileMissing: task.fileMissing,
      );
      return Center(
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(taskStatusIcon(task.status), size: 11, color: color),
            const SizedBox(width: 3),
            Flexible(
              child: Text(
                task.statusText,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 12, color: color),
              ),
            ),
          ],
        ),
      );
    },
  ),
};
