import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../services/open_folder.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import 'task_list_item.dart';

class TaskList extends StatefulWidget {
  final DownloadController controller;
  final ValueChanged<String>? onTaskTap;
  final VoidCallback? onNewDownload;

  const TaskList({
    super.key,
    required this.controller,
    this.onTaskTap,
    this.onNewDownload,
  });

  @override
  State<TaskList> createState() => _TaskListState();
}

class _TaskListState extends State<TaskList> {
  static const double _scrollToTopThreshold = 400.0;

  final ScrollController _scrollController = ScrollController();
  bool _showScrollToTop = false;

  @override
  void initState() {
    super.initState();
    _scrollController.addListener(_onScroll);
  }

  @override
  void dispose() {
    _scrollController.removeListener(_onScroll);
    _scrollController.dispose();
    super.dispose();
  }

  void _onScroll() {
    final show = _scrollController.offset > _scrollToTopThreshold;
    if (show != _showScrollToTop) {
      setState(() => _showScrollToTop = show);
    }
  }

  void _scrollToTop() {
    if (!_scrollController.hasClients) return;
    _scrollController.animateTo(
      0,
      duration: const Duration(milliseconds: 300),
      curve: Curves.easeOut,
    );
  }

  void _onDoubleTap(DownloadTask task) {
    switch (task.status) {
      case TaskStatus.downloading:
      case TaskStatus.pending:
      case TaskStatus.preparing:
      case TaskStatus.resuming:
        widget.controller.pauseTask(task.id);
      case TaskStatus.paused:
      case TaskStatus.error:
        widget.controller.resumeTask(task.id);
      case TaskStatus.completed:
        if (task.fileMissing) return;
        final filePath = task.filePath;
        openFile(filePath);
    }
  }

  void _showBlankAreaMenu(BuildContext context, TapDownDetails details) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final hasActive = widget.controller.activeCount > 0;
    final hasPausedOrError =
        widget.controller.pausedCount + widget.controller.errorCount > 0;

    final items = <ContextMenuItem>[
      ContextMenuItem(
        icon: LucideIcons.plus,
        label: s.newDownload,
        color: c.textPrimary,
        action: () => widget.onNewDownload?.call(),
      ),
      // 全部开始 / 全部暂停 常驻显示，不可用时置灰
      ContextMenuItem(
        icon: LucideIcons.play,
        label: s.startAll,
        color: c.textPrimary,
        enabled: hasPausedOrError,
        action: () => widget.controller.resumeAll(),
      ),
      ContextMenuItem(
        icon: LucideIcons.pause,
        label: s.pauseAll,
        color: c.textPrimary,
        enabled: hasActive,
        action: () => widget.controller.pauseAll(),
      ),
    ];

    showContextMenu(
      context,
      details.globalPosition,
      items: items,
      dividerAfterIndices: const {0}, // 新建下载后加分隔线
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return ListenableBuilder(
      listenable: widget.controller,
      builder: (context, _) {
        final tasks = widget.controller.filteredTasks;
        return ColoredBox(
          color: c.bg,
          child: Column(
            children: [
              _buildHeader(context),
              Expanded(
                child: Stack(
                  children: [
                    tasks.isEmpty
                        ? _buildEmpty(context)
                        : _buildListWithBlankArea(context, tasks),
                    if (_showScrollToTop)
                      Positioned(
                        right: 16,
                        bottom: 16,
                        child: _ScrollToTopButton(onTap: _scrollToTop),
                      ),
                  ],
                ),
              ),
            ],
          ),
        );
      },
    );
  }

  /// 列表 + 列表下方空白区域均支持右键菜单
  Widget _buildListWithBlankArea(BuildContext context, List tasks) {
    final isManage = widget.controller.isManageMode;
    final groups = widget.controller.groupedTasks;

    return CustomScrollView(
      controller: _scrollController,
      slivers: [
        for (final group in groups) ...[
          // 分组头：活跃组用专属 header，时间分组用可折叠 header
          SliverToBoxAdapter(
            child: group.isActiveGroup
                ? _ActiveGroupHeader(
                    taskCount: group.tasks.length,
                    hasDownloading: group.tasks.any(
                      (t) => t.status == TaskStatus.downloading,
                    ),
                    onPauseAll: () => widget.controller.pauseAll(),
                  )
                : _GroupHeader(
                    group: group.group!,
                    taskCount: group.tasks.length,
                    isCollapsed:
                        widget.controller.isGroupCollapsed(group.group),
                    onToggle: () =>
                        widget.controller.toggleGroupCollapsed(group.group!),
                  ),
          ),
          // 活跃组永不折叠；时间分组支持折叠
          if (group.isActiveGroup ||
              !widget.controller.isGroupCollapsed(group.group))
            SliverList(
              delegate: SliverChildBuilderDelegate((context, index) {
                final task = group.tasks[index];
                return RepaintBoundary(
                  key: ValueKey(task.id),
                  child: TaskListItem(
                    task: task,
                    isSelected: task.id == widget.controller.selectedTaskId,
                    onTap: () => widget.onTaskTap?.call(task.id),
                    onDoubleTap: () => _onDoubleTap(task),
                    onPause: () => widget.controller.pauseTask(task.id),
                    onResume: () => widget.controller.resumeTask(task.id),
                    onDelete: ({required bool deleteFiles}) =>
                        widget.controller
                            .deleteTask(task.id, deleteFiles: deleteFiles),
                    isPriority: widget.controller.priorityTaskId == task.id,
                    onBoost: () => widget.controller.setPriorityTask(task.id),
                    isManageMode: isManage,
                    isChecked:
                        widget.controller.checkedTaskIds.contains(task.id),
                    onToggleChecked: () =>
                        widget.controller.toggleTaskChecked(task.id),
                  ),
                );
              }, childCount: group.tasks.length),
            ),
        ],
        // 填满剩余空间的空白区域，仅此区域响应右键
        SliverFillRemaining(
          hasScrollBody: false,
          child: GestureDetector(
            onSecondaryTapDown: isManage
                ? null
                : (details) => _showBlankAreaMenu(context, details),
            behavior: HitTestBehavior.opaque,
            child: const SizedBox.expand(),
          ),
        ),
      ],
    );
  }

  Widget _buildEmpty(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return GestureDetector(
      onSecondaryTapDown: widget.controller.isManageMode
          ? null
          : (details) => _showBlankAreaMenu(context, details),
      behavior: HitTestBehavior.opaque,
      child: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(LucideIcons.download, size: 48, color: c.textMuted),
            const SizedBox(height: 12),
            Text(
              s.emptyTitle,
              style: TextStyle(fontSize: 14, color: c.textMuted),
            ),
            const SizedBox(height: 4),
            Text(
              s.emptySubtitle,
              style: TextStyle(fontSize: 12, color: c.textMuted),
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildHeader(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final isManage = widget.controller.isManageMode;
    final hasTasks = widget.controller.filteredTasks.isNotEmpty;

    return Container(
      height: 36,
      padding: const EdgeInsets.symmetric(horizontal: 16),
      decoration: BoxDecoration(
        color: c.surface1,
        border: Border(bottom: BorderSide(color: c.border, width: 1)),
      ),
      child: Row(
        children: [
          // 管理模式下列头显示全选复选框
          if (isManage) ...[
            _HeaderCheckbox(controller: widget.controller),
            const SizedBox(width: 10),
          ],
          // 管理按钮（放在文件名列之前）
          if (hasTasks && !isManage) ...[
            _ManageToggleButton(
              onTap: () => widget.controller.toggleManageMode(),
            ),
            const SizedBox(width: 6),
          ],
          Expanded(
            child: Text(
              s.colFileName,
              style: TextStyle(
                fontSize: 11,
                fontWeight: FontWeight.w500,
                color: c.textMuted,
              ),
            ),
          ),
          SizedBox(
            width: 150,
            child: Padding(
              padding: const EdgeInsets.only(right: 12),
              child: Center(
                child: Text(
                  s.colProgress,
                  style: TextStyle(
                    fontSize: 11,
                    fontWeight: FontWeight.w500,
                    color: c.textMuted,
                  ),
                ),
              ),
            ),
          ),
          SizedBox(
            width: 90,
            child: Center(
              child: Text(
                s.colSpeed,
                style: TextStyle(
                  fontSize: 11,
                  fontWeight: FontWeight.w500,
                  color: c.textMuted,
                ),
              ),
            ),
          ),
          SizedBox(
            width: 80,
            child: Center(
              child: Text(
                s.colEta,
                style: TextStyle(
                  fontSize: 11,
                  fontWeight: FontWeight.w500,
                  color: c.textMuted,
                ),
              ),
            ),
          ),
          SizedBox(
            width: 60,
            child: Center(
              child: Text(
                s.colStatus,
                style: TextStyle(
                  fontSize: 11,
                  fontWeight: FontWeight.w500,
                  color: c.textMuted,
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

// =============================================================================
// 列头全选复选框
// =============================================================================

class _HeaderCheckbox extends StatefulWidget {
  final DownloadController controller;

  const _HeaderCheckbox({required this.controller});

  @override
  State<_HeaderCheckbox> createState() => _HeaderCheckboxState();
}

class _HeaderCheckboxState extends State<_HeaderCheckbox> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final allChecked = widget.controller.isAllFilteredChecked;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () {
          if (allChecked) {
            widget.controller.deselectAll();
          } else {
            widget.controller.selectAllFiltered();
          }
        },
        child: SizedBox(
          width: 20,
          height: 20,
          child: Icon(
            allChecked ? LucideIcons.squareCheck : LucideIcons.square,
            size: 16,
            color: allChecked
                ? c.accent
                : _isHovered
                ? c.textSecondary
                : c.textMuted,
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 管理按钮（进入管理模式的入口）
// =============================================================================

class _ManageToggleButton extends StatefulWidget {
  final VoidCallback onTap;

  const _ManageToggleButton({required this.onTap});

  @override
  State<_ManageToggleButton> createState() => _ManageToggleButtonState();
}

class _ManageToggleButtonState extends State<_ManageToggleButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final m = AppMetrics.of(context);

    return ShadTooltip(
      waitDuration: const Duration(milliseconds: 500),
      showDuration: Duration.zero,
      effects: const [],
      anchor: const ShadAnchor(
        childAlignment: Alignment.topCenter,
        overlayAlignment: Alignment.bottomCenter,
        offset: Offset(0, 4),
      ),
      builder: (_) => Text(s.manageTooltip),
      child: ShadGestureDetector(
        cursor: SystemMouseCursors.click,
        onTap: widget.onTap,
        onHoverChange: (v) => setState(() => _isHovered = v),
        child: Container(
          height: 24,
          padding: const EdgeInsets.symmetric(horizontal: 6),
          decoration: BoxDecoration(
            color: _isHovered ? c.surface3 : c.surface2,
            borderRadius: m.brSm,
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                LucideIcons.listChecks,
                size: 13,
                color: _isHovered ? c.textPrimary : c.textSecondary,
              ),
              const SizedBox(width: 3),
              Text(
                s.manage,
                style: TextStyle(
                  fontSize: 11,
                  color: _isHovered ? c.textPrimary : c.textSecondary,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 活跃任务组头部（不可折叠）
// =============================================================================

class _ActiveGroupHeader extends StatelessWidget {
  final int taskCount;
  final bool hasDownloading;
  final VoidCallback onPauseAll;

  const _ActiveGroupHeader({
    required this.taskCount,
    required this.hasDownloading,
    required this.onPauseAll,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    return Container(
      height: 32,
      padding: const EdgeInsets.symmetric(horizontal: 16),
      decoration: BoxDecoration(
        color: c.surface1,
        border: Border(bottom: BorderSide(color: c.border, width: 1)),
      ),
      child: Row(
        children: [
          Icon(LucideIcons.zap, size: 12, color: c.accent),
          const SizedBox(width: 6),
          Text(
            s.activeGroupLabel,
            style: TextStyle(
              fontSize: 12,
              fontWeight: FontWeight.w500,
              color: c.textSecondary,
            ),
          ),
          const SizedBox(width: 6),
          Text(
            '$taskCount',
            style: TextStyle(fontSize: 11, color: c.textMuted),
          ),
          const Spacer(),
          if (hasDownloading) _PauseAllButton(onTap: onPauseAll),
        ],
      ),
    );
  }
}

class _PauseAllButton extends StatefulWidget {
  final VoidCallback onTap;

  const _PauseAllButton({required this.onTap});

  @override
  State<_PauseAllButton> createState() => _PauseAllButtonState();
}

class _PauseAllButtonState extends State<_PauseAllButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final m = AppMetrics.of(context);

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          height: 22,
          padding: const EdgeInsets.symmetric(horizontal: 6),
          decoration: BoxDecoration(
            color: _isHovered ? c.hoverBg : Colors.transparent,
            borderRadius: m.brSm,
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                LucideIcons.pause,
                size: 12,
                color: _isHovered ? c.textPrimary : c.textMuted,
              ),
              const SizedBox(width: 3),
              Text(
                s.pauseAll,
                style: TextStyle(
                  fontSize: 11,
                  color: _isHovered ? c.textPrimary : c.textMuted,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 时间分组头部
// =============================================================================

class _GroupHeader extends StatefulWidget {
  final TimeGroup group;
  final int taskCount;
  final bool isCollapsed;
  final VoidCallback onToggle;

  const _GroupHeader({
    required this.group,
    required this.taskCount,
    required this.isCollapsed,
    required this.onToggle,
  });

  @override
  State<_GroupHeader> createState() => _GroupHeaderState();
}

class _GroupHeaderState extends State<_GroupHeader> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onToggle,
        child: Container(
          height: 32,
          padding: const EdgeInsets.symmetric(horizontal: 16),
          decoration: BoxDecoration(
            color: _isHovered ? c.hoverBg : c.surface1,
            border: Border(bottom: BorderSide(color: c.border, width: 1)),
          ),
          child: Row(
            children: [
              AnimatedRotation(
                turns: widget.isCollapsed ? -0.25 : 0,
                duration: const Duration(milliseconds: 150),
                child: Icon(
                  LucideIcons.chevronDown,
                  size: 14,
                  color: c.textMuted,
                ),
              ),
              const SizedBox(width: 6),
              Text(
                widget.group.label,
                style: TextStyle(
                  fontSize: 12,
                  fontWeight: FontWeight.w500,
                  color: c.textSecondary,
                ),
              ),
              const SizedBox(width: 6),
              Text(
                '${widget.taskCount}',
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 回到顶部按钮
// =============================================================================

class _ScrollToTopButton extends StatefulWidget {
  final VoidCallback onTap;

  const _ScrollToTopButton({required this.onTap});

  @override
  State<_ScrollToTopButton> createState() => _ScrollToTopButtonState();
}

class _ScrollToTopButtonState extends State<_ScrollToTopButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          width: 36,
          height: 36,
          decoration: BoxDecoration(
            color: _isHovered ? c.surface3 : c.surface2,
            borderRadius: m.brBadge,
            border: Border.all(color: c.border, width: 1),
          ),
          child: Center(
            child: Icon(
              LucideIcons.arrowUp,
              size: 16,
              color: _isHovered ? c.textPrimary : c.textSecondary,
            ),
          ),
        ),
      ),
    );
  }
}
