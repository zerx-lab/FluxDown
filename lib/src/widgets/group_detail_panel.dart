// 任务组详情面板 — 2 Tab（概览 / 成员）。
//
// 行为规格依据：design-proto-spec.md §12「组详情」。布局照搬
// detail_panel.dart 的既有约定（DetailPanel 无 proto 描述的多 Tab 结构，
// 单栏滚动是既定简化；本面板按契约要求实现真实 2 Tab 切换）。

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:url_launcher/url_launcher.dart';

import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/download_task.dart';
import '../models/list_entity.dart';
import '../models/task_group.dart';
import '../services/open_folder.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'task_columns.dart';
import 'task_group_card.dart';

class GroupDetailPanel extends StatefulWidget {
  final DownloadController controller;
  final VoidCallback onClose;

  /// 当前是否为底部布局（决定切换按钮图标方向，同 DetailPanel）。
  final bool isBottom;
  final VoidCallback? onTogglePosition;

  const GroupDetailPanel({
    super.key,
    required this.controller,
    required this.onClose,
    this.isBottom = true,
    this.onTogglePosition,
  });

  @override
  State<GroupDetailPanel> createState() => _GroupDetailPanelState();
}

class _GroupDetailPanelState extends State<GroupDetailPanel> {
  int _tab = 0; // 0=概览 1=成员

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Container(
      color: c.surface1,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _buildHeader(c),
          Expanded(
            child: ListenableBuilder(
              listenable: widget.controller,
              builder: (context, _) {
                final group = widget.controller.selectedGroup;
                if (group == null) return _buildNoSelection(c);
                final entity = buildGroupEntity(
                  group,
                  widget.controller.selectedGroupMembers,
                );
                return Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Padding(
                      padding: const EdgeInsets.fromLTRB(16, 12, 16, 10),
                      child: _buildGroupSummary(context, c, m, group, entity),
                    ),
                    Padding(
                      padding: const EdgeInsets.fromLTRB(16, 0, 16, 0),
                      child: _buildTabBar(context),
                    ),
                    Expanded(
                      child: widget.isBottom
                          ? _buildBottomBody(context, group, entity)
                          : SingleChildScrollView(
                              padding: const EdgeInsets.all(16),
                              child: _tab == 0
                                  ? _buildOverview(context, group, entity)
                                  : _buildMembers(context, group),
                            ),
                    ),
                  ],
                );
              },
            ),
          ),
        ],
      ),
    );
  }

  /// 组图标(40px)+组名+副标题（design-proto-spec §12 `groupDetailHtml`
  /// `.detail-head` 部分）。顶部 42px 通用工具条（详情标题+切换钮+×）与
  /// DetailPanel 同构不变，此行补在其下方，跨 Tab 常驻。
  Widget _buildGroupSummary(
    BuildContext context,
    AppColors c,
    AppMetrics m,
    DownloadGroup group,
    GroupEntity entity,
  ) {
    final s = LocaleScope.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.center,
      children: [
        buildGroupIcon(c, m, 40, entity.members.length),
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                group.name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  fontSize: 13,
                  fontWeight: FontWeight.w600,
                  color: c.textPrimary,
                ),
              ),
              const SizedBox(height: 2),
              Text(
                s.groupDetailSubtitle(groupStatusLabel(entity.statusBucket, s)),
                style: TextStyle(fontSize: 11, color: c.textSecondary),
              ),
            ],
          ),
        ),
      ],
    );
  }

  /// 底部横向布局（design-proto-spec §12：概览 Tab = Row(左 flex2 进度/计数/
  /// 火花条 + 1px 分隔 + 右 flex1 操作/字段)，各自独立滚动；成员 Tab 全宽但
  /// 内容本身收窄 maxWidth 560 居左，避免迷你列表被拉伸过宽）。
  Widget _buildBottomBody(
    BuildContext context,
    DownloadGroup group,
    GroupEntity entity,
  ) {
    final c = AppColors.of(context);
    if (_tab == 1) {
      return Align(
        alignment: Alignment.topLeft,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 560),
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(16),
            child: _buildMembers(context, group),
          ),
        ),
      );
    }
    final counts = GroupMemberCounts.of(entity.members);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Expanded(
          flex: 2,
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(16),
            child: _buildOverviewProgress(context, entity, counts),
          ),
        ),
        Container(width: 1, color: c.border),
        Expanded(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(16),
            child: _buildOverviewFields(context, group, entity, counts),
          ),
        ),
      ],
    );
  }

  Widget _buildHeader(AppColors c) {
    return Container(
      height: 42,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: c.border, width: 1)),
      ),
      child: Row(
        children: [
          Text(
            currentS.detail,
            style: TextStyle(
              fontSize: 13,
              fontWeight: FontWeight.w600,
              color: c.textPrimary,
            ),
          ),
          const Spacer(),
          ShadButton.ghost(
            onPressed: widget.onTogglePosition,
            size: ShadButtonSize.sm,
            width: 28,
            height: 28,
            padding: EdgeInsets.zero,
            child: Icon(
              widget.isBottom ? LucideIcons.panelRight : LucideIcons.panelBottom,
              size: 14,
              color: c.textMuted,
            ),
          ),
          const SizedBox(width: 4),
          ShadButton.ghost(
            onPressed: widget.onClose,
            size: ShadButtonSize.sm,
            width: 28,
            height: 28,
            padding: EdgeInsets.zero,
            child: Icon(LucideIcons.x, size: 14, color: c.textMuted),
          ),
        ],
      ),
    );
  }

  Widget _buildNoSelection(AppColors c) {
    return Center(
      child: Text(
        currentS.selectTaskHint,
        style: TextStyle(fontSize: 12, color: c.textMuted),
      ),
    );
  }

  Widget _buildTabBar(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final labels = [s.groupDetailOverviewTab, s.groupDetailMembersTab];
    return Row(
      children: [
        for (var i = 0; i < labels.length; i++) ...[
          if (i > 0) const SizedBox(width: 4),
          GestureDetector(
            onTap: () => setState(() => _tab = i),
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: AnimatedContainer(
                duration: const Duration(milliseconds: 120),
                padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
                decoration: BoxDecoration(
                  color: _tab == i ? c.accentBg : c.accentBg.withValues(alpha: 0),
                  borderRadius: m.brMd,
                ),
                child: Text(
                  labels[i],
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: _tab == i ? FontWeight.w500 : FontWeight.normal,
                    color: _tab == i ? c.accent : c.textSecondary,
                  ),
                ),
              ),
            ),
          ),
        ],
      ],
    );
  }

  // ---------------------------------------------------------------------------
  // 概览 Tab
  // ---------------------------------------------------------------------------

  Widget _buildOverview(BuildContext context, DownloadGroup group, GroupEntity entity) {
    final counts = GroupMemberCounts.of(entity.members);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _buildOverviewProgress(context, entity, counts),
        const SizedBox(height: 16),
        _buildOverviewFields(context, group, entity, counts),
      ],
    );
  }

  /// 左列内容（底部横向 flex2）：大号 SUM 进度 + 计数行 + 放大火花条。
  Widget _buildOverviewProgress(
    BuildContext context,
    GroupEntity entity,
    GroupMemberCounts counts,
  ) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final pctStr = (entity.progress * 100).toStringAsFixed(1);
    final subLine = entity.speedBytesPerSec > 0
        ? '${DownloadTask.formatBytes(entity.speedBytesPerSec)}/s · ${groupEtaLine(s, entity) ?? ''}'
        : groupStatusLabel(entity.statusBucket, s);
    final hasFailed = counts.failed > 0;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          '$pctStr%',
          style: TextStyle(
            fontSize: 24,
            fontWeight: FontWeight.w600,
            color: c.textPrimary,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
        const SizedBox(height: 2),
        Text(
          '${entity.downloadedBytes > 0 ? DownloadTask.formatBytes(entity.downloadedBytes) : '0 B'}/${DownloadTask.formatBytes(entity.totalBytes)}',
          style: TextStyle(
            fontSize: 11,
            color: c.textMuted,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
        const SizedBox(height: 10),
        buildGroupSumBar(entity, c, height: 4),
        const SizedBox(height: 6),
        Text(
          '$subLine · ${s.groupItemsCount(counts.total)}',
          style: TextStyle(fontSize: 11, color: c.textSecondary),
        ),
        const SizedBox(height: 10),
        GestureDetector(
          onTap: hasFailed ? () => widget.controller.revealGroupMember(
            entity.groupId,
            entity.members.whereType<TaskEntity>().firstWhere(
              (m) => m.task.status == TaskStatus.error,
            ).task.id,
          ) : null,
          child: _GroupCountsLineWrapper(counts: counts, etaLine: groupEtaLine(s, entity)),
        ),
        const SizedBox(height: 16),
        SizedBox(
          height: 26,
          child: _GroupSparklineWrapper(members: entity.members),
        ),
      ],
    );
  }

  /// 右列内容（底部横向 flex1）：组操作行 + 信息字段。
  Widget _buildOverviewFields(
    BuildContext context,
    DownloadGroup group,
    GroupEntity entity,
    GroupMemberCounts counts,
  ) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _buildActionsRow(context, group, entity, counts),
        const SizedBox(height: 16),
        _infoRow(c, s.groupDetailSource, null, link: group.sourceUrl),
        _infoRow(c, s.groupDetailSaveDir, group.saveDir, mono: true),
        _infoRow(c, s.groupDetailCreatedAt, _formatDateTime(group.createdAt)),
        _infoRow(c, s.groupDetailQueue, _queueLabel(entity.queueId)),
        const SizedBox(height: 4),
        Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
          decoration: BoxDecoration(
            color: c.surface2,
            borderRadius: m.brMd,
            border: Border.all(color: c.border, width: 1),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(LucideIcons.plug, size: 12, color: c.textSecondary),
                  const SizedBox(width: 6),
                  Text(
                    s.groupDetailResolverPlugin,
                    style: TextStyle(
                      fontSize: 11.5,
                      fontWeight: FontWeight.w500,
                      color: c.textSecondary,
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 4),
              Text(
                s.groupDetailLazyRenewHint,
                style: TextStyle(fontSize: 10.5, color: c.textMuted),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _buildActionsRow(
    BuildContext context,
    DownloadGroup group,
    GroupEntity entity,
    GroupMemberCounts counts,
  ) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final hasActive = entity.members.any((m) => m.statusBucket.isActiveOrQueued);
    return Row(
      children: [
        Expanded(
          child: ShadButton(
            onPressed: () => hasActive
                ? widget.controller.pauseGroup(group.id)
                : widget.controller.resumeGroup(group.id),
            backgroundColor: c.accent,
            hoverBackgroundColor: c.accentHover,
            child: Text(
              hasActive ? s.groupPauseAll : s.groupResumeAll,
              style: const TextStyle(fontSize: 12.5, color: Colors.white),
            ),
          ),
        ),
        if (counts.failed > 0) ...[
          const SizedBox(width: 8),
          ShadButton.outline(
            onPressed: () => widget.controller.retryGroupFailed(group.id),
            child: Text(s.groupRetryFailed, style: const TextStyle(fontSize: 12.5)),
          ),
        ],
        const SizedBox(width: 8),
        ShadButton.outline(
          onPressed: () => openFolder(group.saveDir),
          child: Icon(LucideIcons.folderOpen, size: 14, color: c.textSecondary),
        ),
      ],
    );
  }

  Widget _infoRow(AppColors c, String label, String? value, {bool mono = false, String? link}) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 70,
            child: Text(label, style: TextStyle(fontSize: 11, color: c.textMuted)),
          ),
          Expanded(
            child: link != null
                ? GestureDetector(
                    onTap: () => launchUrl(Uri.parse(link)),
                    child: Text(
                      link,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(fontSize: 11, color: c.accent),
                    ),
                  )
                : Text(
                    value ?? '',
                    maxLines: 3,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 11, color: c.textSecondary),
                  ),
          ),
        ],
      ),
    );
  }

  String _queueLabel(String queueId) {
    final s = LocaleScope.of(context);
    if (queueId.isEmpty) return s.ungroupedTasks;
    final q = widget.controller.queueById(queueId);
    return q == null ? queueId : queueDisplayName(s, q);
  }

  String _formatDateTime(DateTime dt) {
    String two(int v) => v.toString().padLeft(2, '0');
    return '${dt.year}-${two(dt.month)}-${two(dt.day)} ${two(dt.hour)}:${two(dt.minute)}';
  }

  // ---------------------------------------------------------------------------
  // 成员 Tab
  // ---------------------------------------------------------------------------

  Widget _buildMembers(BuildContext context, DownloadGroup group) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final members = widget.controller.selectedGroupMembers;
    if (members.isEmpty) {
      return Center(
        child: Text(
          s.groupDetailNoMembers,
          style: TextStyle(fontSize: 12, color: c.textMuted),
        ),
      );
    }
    return Column(
      children: [
        for (final task in members) _memberRow(context, task, group),
      ],
    );
  }

  Widget _memberRow(BuildContext context, DownloadTask task, DownloadGroup group) {
    final c = AppColors.of(context);
    final color = taskStatusColor(task.status, c, fileMissing: task.fileMissing);
    final dir = groupMemberDirPath(task, group);
    final displayName = dir.isEmpty ? task.fileName : '$dir/${task.fileName}';
    final trailing = task.status == TaskStatus.error
        ? currentS.statusError
        : '${(task.progress * 100).toStringAsFixed(0)}%';
    return GestureDetector(
      onTap: () => widget.controller.selectTask(task.id),
      behavior: HitTestBehavior.opaque,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 7),
        child: Row(
          children: [
            Container(
              width: 7,
              height: 7,
              decoration: BoxDecoration(color: color, shape: BoxShape.circle),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                displayName,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 11.5, color: c.textSecondary),
              ),
            ),
            const SizedBox(width: 8),
            Text(
              trailing,
              style: TextStyle(
                fontSize: 11,
                color: task.status == TaskStatus.error ? AppColors.red : c.textMuted,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// `_GroupCountsLine` 是 task_group_card.dart 的库私有 Widget，此处用同一套
/// i18n 键 + 计数结构在本文件重建一份只读展示版（放大档不需要可点击失败态，
/// 点击整行已在外层 GestureDetector 处理——保持与折叠行同样的视觉/文案）。
class _GroupCountsLineWrapper extends StatelessWidget {
  final GroupMemberCounts counts;
  final String? etaLine;

  const _GroupCountsLineWrapper({required this.counts, required this.etaLine});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final parts = <String>[
      s.groupItemsCount(counts.total),
      if (counts.done > 0) s.groupDoneCount(counts.done),
      if (counts.downloading > 0) s.groupDownloadingCount(counts.downloading),
      if (counts.pending > 0) s.groupPendingCount(counts.pending),
      if (counts.paused > 0) s.groupPausedCount(counts.paused),
      if (counts.failed > 0) '${s.groupFailedCount(counts.failed)} ⚠',
      s.groupDoneOfTotal(counts.done, counts.total),
      ?etaLine,
    ];
    return Text(
      parts.join(' · '),
      style: TextStyle(
        fontSize: 11.5,
        color: counts.failed > 0 ? AppColors.red : c.textSecondary,
        fontFeatures: const [FontFeature.tabularFigures()],
      ),
    );
  }
}

/// 放大火花条（design-proto-spec §12 `.dspark`：高 26，每根 flex1）。
class _GroupSparklineWrapper extends StatelessWidget {
  final List<ListEntity> members;

  const _GroupSparklineWrapper({required this.members});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final sampled = sampleSparkline(members, maxBars: 24);
    if (sampled.isEmpty) return const SizedBox.shrink();
    return Row(
      children: [
        for (var i = 0; i < sampled.length; i++) ...[
          if (i > 0) const SizedBox(width: 3),
          Expanded(
            child: ShadTooltip(
              builder: (_) => Text(
                '${sampled[i].name} · ${(sampled[i].progress * 100).toStringAsFixed(0)}%',
              ),
              child: FractionallySizedBox(
                alignment: Alignment.bottomCenter,
                heightFactor: math.max(0.14, sampled[i].progress.clamp(0.0, 1.0)),
                child: Container(
                  decoration: BoxDecoration(
                    color: _sparklineColor(sampled[i].statusBucket, c),
                    borderRadius: BorderRadius.circular(2),
                  ),
                ),
              ),
            ),
          ),
        ],
      ],
    );
  }
}


Color _sparklineColor(TaskStatus status, AppColors c) => switch (status) {
  TaskStatus.completed => AppColors.green,
  TaskStatus.error => AppColors.red,
  TaskStatus.paused => AppColors.amber,
  TaskStatus.pending => c.surface3,
  TaskStatus.downloading || TaskStatus.preparing || TaskStatus.resuming => c.accent,
};
