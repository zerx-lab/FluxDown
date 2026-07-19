// 任务组活卡片 — 折叠/展开列表行 + 网格 2× 卡片。
//
// 行为规格依据：design-proto-spec.md §8（组活卡片）。密度/尺寸/火花条/
// 计数行/树轨/目录分段行等均照此规格实现；关于本文件与规格的已知偏离见
// dart-groups-report.md。

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/list_entity.dart';
import '../models/task_group.dart';
import '../models/view_prefs.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import 'flux_sonner.dart';
import 'task_columns.dart';
import 'task_list_item.dart';

/// 组聚合状态 → 文案（同任务状态标签复用同一套 i18n 键）。
String groupStatusLabel(TaskStatus status, S s) => switch (status) {
  TaskStatus.pending => s.statusPending,
  TaskStatus.downloading || TaskStatus.preparing || TaskStatus.resuming =>
    s.statusDownloading,
  TaskStatus.paused => s.statusPaused,
  TaskStatus.completed => s.statusCompleted,
  TaskStatus.error => s.statusError,
};

/// 剩余时间格式化（`DownloadTask.etaText` 的组级等价物：组没有单一
/// `status`/`speed`/`totalBytes` 实例字段可直接复用，逻辑轻量重复）。
String _formatEta(S s, int seconds) {
  if (seconds <= 0) return '—';
  if (seconds < 60) return s.etaSeconds(seconds);
  if (seconds < 3600) return s.etaMinutes(seconds ~/ 60);
  if (seconds < 86400) return s.etaHours((seconds / 3600).toStringAsFixed(1));
  return '—';
}

/// 组「剩余约 xx」文案；无有效 ETA（未在下载或剩余字节不足）时返回 null
/// （design-proto-spec §8 计数行 `speed>0 时` 条件）。
String? groupEtaLine(S s, GroupEntity group) {
  if (group.speedBytesPerSec <= 0) return null;
  final remaining = group.totalBytes - group.downloadedBytes;
  if (remaining <= 0) return null;
  final seconds = remaining / group.speedBytesPerSec;
  if (seconds > 86400) return null;
  return s.groupEtaRemaining(_formatEta(s, seconds.toInt()));
}

// =============================================================================
// 右键 / ⋯ 菜单（design-proto-spec §8「组右键 / ⋯ 菜单」）
// =============================================================================

void showGroupContextMenu(
  BuildContext context,
  Offset globalPosition, {
  required GroupEntity group,
  required VoidCallback onPauseAll,
  required VoidCallback onResumeAll,
  VoidCallback? onRetryFailed,
  required VoidCallback onOpenFolder,
  required VoidCallback onCopySource,
  required void Function({required bool deleteFiles}) onDelete,
}) {
  final c = AppColors.of(context);
  final s = LocaleScope.of(context);
  final hasActive = group.members.any(
    (m) => m.statusBucket.isActiveOrQueued,
  );

  final items = <ContextMenuItem>[
    hasActive
        ? ContextMenuItem(
            icon: LucideIcons.pause,
            label: s.groupPauseAll,
            color: c.textPrimary,
            action: onPauseAll,
          )
        : ContextMenuItem(
            icon: LucideIcons.play,
            label: s.groupResumeAll,
            color: c.textPrimary,
            action: onResumeAll,
          ),
    if (onRetryFailed != null)
      ContextMenuItem(
        icon: LucideIcons.rotateCcw,
        label: s.groupRetryFailed,
        color: c.textPrimary,
        action: onRetryFailed,
      ),
    ContextMenuItem(
      icon: LucideIcons.folderOpen,
      label: s.groupOpenFolder,
      color: c.textPrimary,
      action: onOpenFolder,
    ),
    ContextMenuItem(
      icon: LucideIcons.copy,
      label: s.groupCopySourceLink,
      color: c.textPrimary,
      action: onCopySource,
    ),
    ContextMenuItem(
      icon: LucideIcons.trash2,
      label: s.groupDelete,
      color: c.textPrimary,
      action: () => _confirmDeleteGroup(
        context,
        groupName: group.name,
        deleteFiles: false,
        onConfirm: onDelete,
      ),
    ),
    ContextMenuItem(
      icon: LucideIcons.fileX,
      label: s.groupDeleteWithFiles,
      color: AppColors.red,
      action: () => _confirmDeleteGroup(
        context,
        groupName: group.name,
        deleteFiles: true,
        onConfirm: onDelete,
      ),
    ),
  ];

  showContextMenu(
    context,
    globalPosition,
    items: items,
    dividerAfterIndices: {items.length - 3},
  );
}

/// 删除组二次确认（复用任务删除的通用 i18n 键：`deleteConfirmTitle`/
/// `deleteConfirmDesc` 本就接受任意名称字符串，无需新造重复概念）。
void _confirmDeleteGroup(
  BuildContext context, {
  required String groupName,
  required bool deleteFiles,
  required void Function({required bool deleteFiles}) onConfirm,
}) {
  if (!context.mounted) return;
  final c = AppColors.of(context);
  final s = LocaleScope.of(context);
  showShadDialog(
    context: context,
    barrierColor: c.dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => ShadDialog(
      title: Text(s.deleteConfirmTitle(deleteFiles)),
      description: Text(s.deleteConfirmDesc(groupName, deleteFiles)),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(ctx).pop(),
          child: Text(s.cancel),
        ),
        ShadButton.destructive(
          onPressed: () {
            Navigator.of(ctx).pop();
            onConfirm(deleteFiles: deleteFiles);
          },
          child: Text(s.deleteConfirmTitle(deleteFiles)),
        ),
      ],
    ),
  );
}

void copyGroupSourceLink(BuildContext context, String url) {
  Clipboard.setData(ClipboardData(text: url));
  FluxSonner.of(context).show(
    ShadToast(
      title: Text(LocaleScope.of(context).urlCopied),
      duration: const Duration(seconds: 2),
    ),
  );
}

// =============================================================================
// 火花条（design-proto-spec §8 `.gspark`/`sparkHtml`）
// =============================================================================

Color _sparkBarColor(TaskStatus status, AppColors c) => switch (status) {
  TaskStatus.completed => AppColors.green,
  TaskStatus.error => AppColors.red,
  TaskStatus.paused => AppColors.amber,
  TaskStatus.pending => c.surface3,
  TaskStatus.downloading || TaskStatus.preparing || TaskStatus.resuming =>
    c.accent,
};

class _GroupSparkline extends StatelessWidget {
  final List<ListEntity> members;
  final double height;
  final double barWidth;
  final double gap;

  const _GroupSparkline({
    required this.members,
    required this.height,
    required this.barWidth,
    required this.gap,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final sampled = sampleSparkline(members, maxBars: 24);
    return SizedBox(
      height: height,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          for (var i = 0; i < sampled.length; i++) ...[
            if (i > 0) SizedBox(width: gap),
            _sparkBar(sampled[i], c, s),
          ],
        ],
      ),
    );
  }

  Widget _sparkBar(ListEntity m, AppColors c, S s) {
    final fraction = math.max(0.14, m.progress.clamp(0.0, 1.0));
    var barHeight = height * fraction;
    if (m.statusBucket == TaskStatus.error) barHeight = math.max(barHeight, 6.0);
    return ShadTooltip(
      builder: (_) =>
          Text('${m.name} · ${(m.progress * 100).toStringAsFixed(0)}%'),
      child: Container(
        width: barWidth,
        height: height,
        alignment: Alignment.bottomCenter,
        child: Container(
          width: barWidth,
          height: barHeight.clamp(2.0, height),
          decoration: BoxDecoration(
            color: _sparkBarColor(m.statusBucket, c),
            borderRadius: BorderRadius.circular(1.5),
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 计数行（design-proto-spec §8 `groupCountsHtml`）
// =============================================================================

class _GroupCountsLine extends StatefulWidget {
  final GroupMemberCounts counts;
  final String? etaLine;
  final VoidCallback? onJumpToFail;
  final bool compact;

  const _GroupCountsLine({
    required this.counts,
    required this.etaLine,
    required this.onJumpToFail,
    this.compact = false,
  });

  @override
  State<_GroupCountsLine> createState() => _GroupCountsLineState();
}

class _GroupCountsLineState extends State<_GroupCountsLine> {
  // 单独持有 + 复用 recognizer 实例（而非每次 build 新建），避免悬挂未
  // dispose 的 TapGestureRecognizer（Flutter 已知反模式）。
  final TapGestureRecognizer _failRecognizer = TapGestureRecognizer();

  @override
  void dispose() {
    _failRecognizer.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final counts = widget.counts;
    _failRecognizer.onTap = widget.onJumpToFail;
    final spans = <InlineSpan>[
      TextSpan(text: s.groupItemsCount(counts.total)),
      if (counts.done > 0)
        TextSpan(text: ' · ${s.groupDoneCount(counts.done)}'),
      if (counts.downloading > 0)
        TextSpan(
          text: ' · ${s.groupDownloadingCount(counts.downloading)}',
          style: TextStyle(color: c.accent),
        ),
      if (counts.pending > 0)
        TextSpan(text: ' · ${s.groupPendingCount(counts.pending)}'),
      if (counts.paused > 0)
        TextSpan(text: ' · ${s.groupPausedCount(counts.paused)}'),
      if (counts.failed > 0)
        TextSpan(
          text: ' · ${s.groupFailedCount(counts.failed)}',
          style: TextStyle(color: AppColors.red, fontWeight: FontWeight.w500),
          recognizer: widget.onJumpToFail == null ? null : _failRecognizer,
        ),
      TextSpan(text: ' · ${s.groupDoneOfTotal(counts.done, counts.total)}'),
      if (widget.etaLine != null) TextSpan(text: ' · ${widget.etaLine}'),
    ];
    return Text.rich(
      TextSpan(
        style: TextStyle(
          fontSize: widget.compact ? 10.5 : 11,
          // 行内固定行高（64/52/44px 行预算按字号估算；真实字体行高可达
          // 1.4-1.5 倍字号，不钉死会底部溢出——测试环境 Ahem 字体行高=字号
          // 暴露不了，见 2026-07-19 组卡片溢出修复）。
          height: 1.2,
          color: c.textMuted,
          fontFeatures: const [FontFeature.tabularFigures()],
        ),
        children: spans,
      ),
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
    );
  }
}

// =============================================================================
// 组 SUM 进度条（design-proto-spec §8 `.grow-bar` 3px / 详情面板放大版）
// =============================================================================

Widget buildGroupSumBar(
  GroupEntity group,
  AppColors c, {
  double height = 3,
  double? maxWidth,
}) {
  final bar = Container(
    height: height,
    decoration: BoxDecoration(
      color: c.surface3,
      borderRadius: BorderRadius.circular(height / 2),
    ),
    clipBehavior: Clip.hardEdge,
    child: FractionallySizedBox(
      alignment: Alignment.centerLeft,
      widthFactor: group.progress.clamp(0.0, 1.0),
      child: ColoredBox(
        color: group.statusBucket == TaskStatus.completed
            ? AppColors.green
            : c.accent,
      ),
    ),
  );
  return maxWidth == null
      ? bar
      : ConstrainedBox(
          constraints: BoxConstraints(maxWidth: maxWidth),
          child: bar,
        );
}

// =============================================================================
// 组图标 + 角标（design-proto-spec §8 `.gicon`/`.gnum`）
// =============================================================================

Widget buildGroupIcon(AppColors c, AppMetrics m, double size, int memberCount) {
  final badgeSize = size >= 30 ? 17.0 : 15.0;
  return SizedBox(
    width: size + 6,
    height: size + 6,
    child: Stack(
      clipBehavior: Clip.none,
      children: [
        Container(
          width: size,
          height: size,
          decoration: BoxDecoration(color: c.accentBg, borderRadius: m.brCard),
          child: Icon(LucideIcons.layers, size: size * 0.44, color: c.accent),
        ),
        Positioned(
          right: -5,
          bottom: -5,
          child: Container(
            constraints: BoxConstraints(minWidth: badgeSize, minHeight: badgeSize),
            padding: const EdgeInsets.symmetric(horizontal: 3),
            decoration: BoxDecoration(
              color: c.accent,
              borderRadius: m.brPill,
              border: Border.all(color: c.surface1, width: 2),
            ),
            child: Center(
              child: Text(
                '$memberCount',
                style: const TextStyle(
                  fontSize: 9.5,
                  fontWeight: FontWeight.w600,
                  color: Colors.white,
                  fontFeatures: [FontFeature.tabularFigures()],
                ),
              ),
            ),
          ),
        ),
      ],
    ),
  );
}

// =============================================================================
// 折叠态组行（design-proto-spec §8 `.grow`，64/44px）
// =============================================================================

class TaskGroupRow extends StatefulWidget {
  final GroupEntity group;
  final DownloadGroup downloadGroup;
  final bool expanded;
  final bool isSelected;
  final ViewDensity density;
  final VoidCallback onTap;
  final VoidCallback onToggleExpand;
  final VoidCallback onPauseAll;
  final VoidCallback onResumeAll;
  final VoidCallback? onRetryFailed;
  final VoidCallback onOpenFolder;
  final VoidCallback onCopySource;
  final void Function({required bool deleteFiles}) onDelete;
  final VoidCallback? onJumpToFail;

  const TaskGroupRow({
    super.key,
    required this.group,
    required this.downloadGroup,
    required this.expanded,
    required this.isSelected,
    required this.density,
    required this.onTap,
    required this.onToggleExpand,
    required this.onPauseAll,
    required this.onResumeAll,
    this.onRetryFailed,
    required this.onOpenFolder,
    required this.onCopySource,
    required this.onDelete,
    this.onJumpToFail,
  });

  @override
  State<TaskGroupRow> createState() => _TaskGroupRowState();
}

class _TaskGroupRowState extends State<TaskGroupRow> {
  bool _hovered = false;

  bool get _compact => widget.density == ViewDensity.compact;

  void _showContextMenu(TapDownDetails details) {
    showGroupContextMenu(
      context,
      details.globalPosition,
      group: widget.group,
      onPauseAll: widget.onPauseAll,
      onResumeAll: widget.onResumeAll,
      onRetryFailed: widget.onRetryFailed,
      onOpenFolder: widget.onOpenFolder,
      onCopySource: widget.onCopySource,
      onDelete: widget.onDelete,
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final group = widget.group;
    final compact = _compact;
    final rowHeight = compact ? 44.0 : 64.0;
    final iconSize = compact ? 26.0 : 34.0;
    final counts = GroupMemberCounts.of(group.members);
    final pctStr = (group.progress * 100).toStringAsFixed(1);

    final content = Row(
      children: [
        if (widget.isSelected) ...[
          Container(
            width: 3,
            height: compact ? 20 : 28,
            decoration: BoxDecoration(color: c.accent, borderRadius: m.brProgress),
          ),
          const SizedBox(width: 13),
        ],
        // 展开/收起命中区 = 组图标向左的全部区域（chevron+间隙+图标），
        // 子手势赢得竞技场，不连带行 onTap 选中；高度拉满行高，便于点击。
        GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onToggleExpand,
          child: Container(
            height: double.infinity,
            alignment: Alignment.center,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                SizedBox(
                  width: 21,
                  child: Align(
                    alignment: Alignment.centerLeft,
                    child: AnimatedRotation(
                      turns: widget.expanded ? 0.25 : 0,
                      duration: const Duration(milliseconds: 150),
                      child: Icon(
                        LucideIcons.chevronRight,
                        size: 13,
                        color: c.textMuted,
                      ),
                    ),
                  ),
                ),
                buildGroupIcon(c, m, iconSize, group.members.length),
              ],
            ),
          ),
        ),
        const SizedBox(width: 12),
        Expanded(
          child: compact
              ? Row(
                  children: [
                    Flexible(
                      flex: 2,
                      child: Text(
                        widget.downloadGroup.displayName,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(
                          fontSize: 12.5,
                          height: 1.2,
                          fontWeight: FontWeight.w500,
                          color: c.textPrimary,
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      flex: 3,
                      child: _GroupCountsLine(
                        counts: counts,
                        etaLine: groupEtaLine(s, group),
                        onJumpToFail: widget.onJumpToFail,
                        compact: true,
                      ),
                    ),
                  ],
                )
              : Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisAlignment: MainAxisAlignment.center,
                  children: [
                    Row(
                      children: [
                        Flexible(
                          child: Text(
                            widget.downloadGroup.displayName,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(
                              fontSize: 13,
                              // 钉行高：64px 行内名称+计数+SUM 条纵向预算
                              // 依赖行高确定性（同 _GroupCountsLine 注释）。
                              height: 1.2,
                              fontWeight: FontWeight.w500,
                              color: c.textPrimary,
                            ),
                          ),
                        ),
                        const SizedBox(width: 6),
                        Container(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 5,
                            vertical: 1,
                          ),
                          decoration: BoxDecoration(
                            color: c.surface2,
                            borderRadius: m.brXs,
                          ),
                          child: Text(
                            s.groupPluginBadge,
                            style: TextStyle(
                              fontSize: 9.5,
                              fontWeight: FontWeight.w600,
                              color: c.textMuted,
                            ),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 2),
                    _GroupCountsLine(
                      counts: counts,
                      etaLine: groupEtaLine(s, group),
                      onJumpToFail: widget.onJumpToFail,
                    ),
                    const SizedBox(height: 4),
                    buildGroupSumBar(group, c, maxWidth: 420),
                  ],
                ),
        ),
        const SizedBox(width: 8),
        _GroupSparkline(
          members: group.members,
          height: compact ? 14 : 18,
          barWidth: compact ? 4 : 5,
          gap: 2,
        ),
        const SizedBox(width: 14),
        SizedBox(
          width: 44,
          child: Text(
            '$pctStr%',
            textAlign: TextAlign.right,
            style: TextStyle(
              fontSize: 12.5,
              fontWeight: FontWeight.w500,
              color: c.textPrimary,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
          ),
        ),
        SizedBox(
          width: 80,
          child: Text(
            group.speedBytesPerSec > 0
                ? '${DownloadTask.formatBytes(group.speedBytesPerSec)}/s'
                : '—',
            textAlign: TextAlign.center,
            style: TextStyle(
              fontSize: 12,
              color: group.speedBytesPerSec > 0 ? AppColors.green : c.textMuted,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
          ),
        ),
      ],
    );

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        // onTap：查看组详情（选中组）；展开/收起仅由左侧 chevron 子手势触发，
        // 行内 ⋯/失败直达等子手势赢得竞技场后同样不连带触发。
        onTap: widget.onTap,
        onSecondaryTapDown: _showContextMenu,
        child: Container(
          height: rowHeight,
          decoration: BoxDecoration(
            color: widget.isSelected
                ? c.selectedBg
                : _hovered
                ? c.hoverBg
                : Colors.transparent,
            border: Border(bottom: BorderSide(color: c.border, width: 1)),
          ),
          child: Stack(
            children: [
              Padding(
                padding: EdgeInsets.only(
                  left: widget.isSelected ? 0 : 16,
                  right: 16,
                  top: compact ? 4 : 8,
                  bottom: compact ? 6 : 8,
                ),
                child: Stack(
                  clipBehavior: Clip.none,
                  children: [
                    content,
                    if (_hovered)
                      Positioned(
                        right: 0,
                        top: 0,
                        bottom: 0,
                        child: Center(
                          child: _GroupActionCluster(
                            group: group,
                            onPauseAll: widget.onPauseAll,
                            onResumeAll: widget.onResumeAll,
                            onOpenFolder: widget.onOpenFolder,
                            onMoreTapDown: _showContextMenu,
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              if (compact)
                Positioned(
                  left: 0,
                  right: 0,
                  bottom: 0,
                  height: 2,
                  child: buildGroupSumBar(group, c, height: 2),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _GroupActionCluster extends StatelessWidget {
  final GroupEntity group;
  final VoidCallback onPauseAll;
  final VoidCallback onResumeAll;
  final VoidCallback onOpenFolder;
  final void Function(TapDownDetails) onMoreTapDown;

  const _GroupActionCluster({
    required this.group,
    required this.onPauseAll,
    required this.onResumeAll,
    required this.onOpenFolder,
    required this.onMoreTapDown,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final hasActive = group.members.any(
      (e) => e.statusBucket.isActiveOrQueued,
    );
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brMd,
        border: Border.all(color: c.border, width: 1),
        boxShadow: [
          BoxShadow(
            color: m.shadowSoft(c.shadow),
            blurRadius: 8,
            offset: const Offset(0, 2),
          ),
        ],
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          TaskActionButton(
            icon: hasActive ? LucideIcons.pause : LucideIcons.play,
            primary: true,
            onTap: hasActive ? onPauseAll : onResumeAll,
          ),
          const SizedBox(width: 2),
          TaskActionButton(icon: LucideIcons.folderOpen, onTap: onOpenFolder),
          const SizedBox(width: 2),
          TaskActionButton(
            icon: LucideIcons.moreHorizontal,
            onTapDown: onMoreTapDown,
          ),
        ],
      ),
    );
  }
}

// =============================================================================
// 展开态成员行（design-proto-spec §8 `.mrow`，52/44px）
// =============================================================================

class GroupMemberRow extends StatefulWidget {
  final DownloadTask task;
  final ViewDensity density;
  final bool isSelected;
  final int flashEpoch;
  final VoidCallback onTap;
  final VoidCallback onPause;
  final VoidCallback onResume;
  final VoidCallback onOpenFile;
  final void Function(TapDownDetails) onMoreTapDown;

  const GroupMemberRow({
    super.key,
    required this.task,
    required this.density,
    required this.isSelected,
    this.flashEpoch = 0,
    required this.onTap,
    required this.onPause,
    required this.onResume,
    required this.onOpenFile,
    required this.onMoreTapDown,
  });

  @override
  State<GroupMemberRow> createState() => _GroupMemberRowState();
}

class _GroupMemberRowState extends State<GroupMemberRow> {
  bool _hovered = false;

  String _subtitle(S s) {
    final task = widget.task;
    if (task.status == TaskStatus.error) return s.groupMemberExpiredResolve;
    if (task.status == TaskStatus.downloading) {
      return '${task.speedText} · ${task.downloadedText}/${task.sizeText}';
    }
    return task.sizeText;
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final task = widget.task;
    final compact = widget.density == ViewDensity.compact;
    final rowHeight = compact ? 44.0 : 52.0;
    final iconSize = compact ? 22.0 : 26.0;
    final statusColor = taskStatusColor(task.status, c, fileMissing: task.fileMissing);

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        // onTap：成员行操作按钮点击不连带选中成员行。
        onTap: widget.onTap,
        child: Container(
          height: rowHeight,
          color: widget.isSelected
              ? c.selectedBg
              : _hovered
              ? c.hoverBg
              : m.glassSubtle(c.surface1),
          child: Stack(
            clipBehavior: Clip.none,
            children: [
              // 树轨（design-proto-spec §8 `.mrow::before`）。
              Positioned(
                left: 32,
                top: 0,
                bottom: 0,
                width: 2,
                child: ColoredBox(color: c.accentBg),
              ),
              if (widget.flashEpoch > 0)
                Positioned.fill(
                  child: IgnorePointer(
                    child: TweenAnimationBuilder<double>(
                      key: ValueKey(widget.flashEpoch),
                      tween: Tween(begin: 1.0, end: 0.0),
                      duration: const Duration(milliseconds: 1600),
                      curve: Curves.easeOut,
                      // 以 accentBg 自身 alpha 为峰值淡出（规则：同基底零透明端；
                      // 若用 withValues(alpha: value) 则起点是全饱和 accent 盖脸）。
                      builder: (context, value, _) => ColoredBox(
                        color: c.accentBg.withValues(alpha: c.accentBg.a * value),
                      ),
                    ),
                  ),
                ),
              Padding(
                padding: EdgeInsets.fromLTRB(30, 6, 16, 6),
                child: Row(
                  children: [
                    Container(
                      width: iconSize,
                      height: iconSize,
                      margin: const EdgeInsets.only(left: 16, right: 10),
                      decoration: BoxDecoration(color: c.surface2, borderRadius: m.brSm),
                      child: Center(
                        child: Text(
                          task.fileExtension,
                          style: TextStyle(
                            fontSize: compact ? 8 : 9,
                            fontWeight: FontWeight.w600,
                            color: c.textSecondary,
                            fontFeatures: const [FontFeature.tabularFigures()],
                          ),
                        ),
                      ),
                    ),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisAlignment: MainAxisAlignment.center,
                        children: [
                          Text(
                            task.fileName,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(fontSize: 12.5, height: 1.2, color: c.textPrimary),
                          ),
                          if (!compact) ...[
                            const SizedBox(height: 2),
                            Text(
                              _subtitle(s),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                fontSize: 10.5,
                                // 钉行高：52px 成员行 名称+副标题 预算同理。
                                height: 1.2,
                                color: task.status == TaskStatus.error
                                    ? AppColors.amber
                                    : c.textMuted,
                              ),
                            ),
                          ],
                        ],
                      ),
                    ),
                    SizedBox(
                      width: 130,
                      child: kTaskColumns[TaskColumnId.progress]!.cellBuilder(
                        context,
                        task,
                      ),
                    ),
                    SizedBox(
                      width: 60,
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(taskStatusIcon(task.status), size: 11, color: statusColor),
                          const SizedBox(width: 3),
                          Flexible(
                            child: Text(
                              task.statusText,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(fontSize: 11, color: statusColor),
                            ),
                          ),
                        ],
                      ),
                    ),
                    if (_hovered)
                      Padding(
                        padding: const EdgeInsets.only(left: 8),
                        child: _MemberActionCluster(
                          task: task,
                          onPause: widget.onPause,
                          onResume: widget.onResume,
                          onOpenFile: widget.onOpenFile,
                          onMoreTapDown: widget.onMoreTapDown,
                        ),
                      ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _MemberActionCluster extends StatelessWidget {
  final DownloadTask task;
  final VoidCallback onPause;
  final VoidCallback onResume;
  final VoidCallback onOpenFile;
  final void Function(TapDownDetails) onMoreTapDown;

  const _MemberActionCluster({
    required this.task,
    required this.onPause,
    required this.onResume,
    required this.onOpenFile,
    required this.onMoreTapDown,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final primaryBtn = switch (task.status) {
      TaskStatus.error =>
        TaskActionButton(icon: LucideIcons.rotateCcw, primary: true, onTap: onResume),
      TaskStatus.downloading ||
      TaskStatus.pending ||
      TaskStatus.preparing ||
      TaskStatus.resuming =>
        TaskActionButton(icon: LucideIcons.pause, primary: true, onTap: onPause),
      TaskStatus.paused =>
        TaskActionButton(icon: LucideIcons.play, primary: true, onTap: onResume),
      TaskStatus.completed =>
        TaskActionButton(icon: LucideIcons.externalLink, onTap: onOpenFile),
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brMd,
        border: Border.all(color: c.border, width: 1),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          primaryBtn,
          const SizedBox(width: 2),
          TaskActionButton(icon: LucideIcons.moreHorizontal, onTapDown: onMoreTapDown),
        ],
      ),
    );
  }
}

// =============================================================================
// 目录分段行（design-proto-spec §8 `.mdir`/`dirRowHtml`，28/24px）
// =============================================================================

class GroupDirRow extends StatelessWidget {
  final GroupDirEntity entity;
  final bool collapsed;
  final ViewDensity density;
  final VoidCallback onTap;

  const GroupDirRow({
    super.key,
    required this.entity,
    required this.collapsed,
    required this.density,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final compact = density == ViewDensity.compact;
    final height = compact ? 24.0 : 28.0;
    return ShadTooltip(
      builder: (_) => Text(entity.path),
      child: GestureDetector(
        onTap: onTap,
        behavior: HitTestBehavior.opaque,
        child: Container(
          height: height,
          padding: const EdgeInsets.fromLTRB(46, 0, 16, 0),
          color: m.glassSubtle(c.surface1),
          child: Stack(
            clipBehavior: Clip.none,
            children: [
              Positioned(
                left: -14,
                top: 0,
                bottom: 0,
                width: 2,
                child: ColoredBox(color: c.accentBg),
              ),
              Row(
                children: [
                  AnimatedRotation(
                    turns: collapsed ? -0.25 : 0,
                    duration: const Duration(milliseconds: 150),
                    child: Icon(LucideIcons.chevronDown, size: 11, color: c.textMuted),
                  ),
                  const SizedBox(width: 6),
                  Icon(LucideIcons.folder, size: 12, color: c.textMuted),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      compressPathChain(entity.path),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 11,
                        fontWeight: FontWeight.w500,
                        color: c.textSecondary,
                      ),
                    ),
                  ),
                  Text(
                    s.groupDirMeta(
                      entity.fileCount,
                      DownloadTask.formatBytes(entity.totalDirBytes),
                    ),
                    style: TextStyle(fontSize: 11, color: c.textMuted),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 网格 · 组卡 2×（design-proto-spec §7 `.gcard.group.span2`）
// =============================================================================

class TaskGroupCard extends StatefulWidget {
  final GroupEntity group;
  final DownloadGroup downloadGroup;
  final bool isSelected;
  final VoidCallback onTap;
  final void Function(TapDownDetails) onMoreTapDown;

  const TaskGroupCard({
    super.key,
    required this.group,
    required this.downloadGroup,
    required this.isSelected,
    required this.onTap,
    required this.onMoreTapDown,
  });

  @override
  State<TaskGroupCard> createState() => _TaskGroupCardState();
}

class _TaskGroupCardState extends State<TaskGroupCard> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final group = widget.group;
    final counts = GroupMemberCounts.of(group.members);
    final isDl = group.speedBytesPerSec > 0;

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        // onTap：卡内子手势（⋯ 菜单等）优先，不连带选中组卡。
        onTap: widget.onTap,
        onSecondaryTapDown: widget.onMoreTapDown,
        child: Stack(
          children: [
            // 普通 Container 即时切色（规则 no-lerp-from-transparent：悬浮/
            // 选中属即时状态切换，与侧栏 _NavItem/列表行一致，不加颜色动画）。
            Container(
              padding: const EdgeInsets.fromLTRB(12, 12, 12, 11),
              decoration: BoxDecoration(
                // 不透明合成：同 _TaskGridCard——半透明 selectedBg 会让
                // hover boxShadow 透过卡体渗出成浑浊灰蓝。
                color: widget.isSelected
                    ? Color.alphaBlend(c.selectedBg, c.surface1)
                    : c.surface1,
                borderRadius: m.brCard,
                border: Border.all(
                  color: widget.isSelected ? c.accent : c.border,
                  width: widget.isSelected ? 1.5 : 1,
                ),
                boxShadow: _hovered
                    ? [
                        BoxShadow(
                          color: m.shadowSoft(c.shadow),
                          blurRadius: 10,
                          offset: const Offset(0, 3),
                        ),
                      ]
                    : null,
              ),
              transform: Matrix4.translationValues(0, _hovered ? -1 : 0, 0),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      buildGroupIcon(c, m, 34, group.members.length),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          widget.downloadGroup.displayName,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: 12.5,
                            fontWeight: FontWeight.w500,
                            color: c.textPrimary,
                          ),
                        ),
                      ),
                      const SizedBox(width: 6),
                      Text(
                        '${(group.progress * 100).toStringAsFixed(0)}%',
                        style: TextStyle(
                          fontSize: 11.5,
                          fontWeight: FontWeight.w500,
                          color: c.textSecondary,
                          fontFeatures: const [FontFeature.tabularFigures()],
                        ),
                      ),
                    ],
                  ),
                  const SizedBox(height: 4),
                  _GroupCountsLine(
                    counts: counts,
                    etaLine: null,
                    onJumpToFail: null,
                    compact: true,
                  ),
                  const SizedBox(height: 6),
                  _GroupSparkline(members: group.members, height: 14, barWidth: 4, gap: 2),
                  const SizedBox(height: 6),
                  buildGroupSumBar(group, c, height: 4),
                  // 尾行弹性底对齐：与 _TaskGridCard 同款——卡片受
                  // _gridCardHeight 固定高度约束，固定间距 + 字体行高浮动
                  // 可能溢出，让 Expanded 吸收余量、foot 行贴底。
                  Expanded(
                    child: Align(
                      alignment: Alignment.bottomLeft,
                      child: Padding(
                        padding: const EdgeInsets.only(top: 8),
                        child: Text(
                          isDl
                              ? '↓ ${DownloadTask.formatBytes(group.speedBytesPerSec)}/s'
                              : '${groupStatusLabel(group.statusBucket, s)} · ${counts.done}/${counts.total}',
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: 11,
                            color: isDl ? AppColors.green : c.textMuted,
                            fontFeatures: const [FontFeature.tabularFigures()],
                          ),
                        ),
                      ),
                    ),
                  ),
                ],
              ),
            ),
            if (group.statusBucket == TaskStatus.error)
              Positioned(
                left: 0,
                top: 0,
                bottom: 0,
                width: 3,
                child: ColoredBox(color: AppColors.red),
              ),
          ],
        ),
      ),
    );
  }
}
