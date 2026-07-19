import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../models/list_entity.dart';
import '../models/view_prefs.dart';
import '../services/open_folder.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import 'edit_threads_dialog.dart';
import 'flux_sonner.dart';
import 'view_options_panel.dart';
import 'task_columns.dart';
import 'task_group_card.dart';
import 'task_list_item.dart';

class TaskList extends StatefulWidget {
  final DownloadController controller;
  final ViewPrefsStore viewPrefsStore;
  final ValueChanged<String>? onTaskTap;
  final ValueChanged<String>? onGroupTap;
  final VoidCallback? onNewDownload;

  const TaskList({
    super.key,
    required this.controller,
    required this.viewPrefsStore,
    this.onTaskTap,
    this.onGroupTap,
    this.onNewDownload,
  });

  @override
  State<TaskList> createState() => _TaskListState();
}

class _TaskListState extends State<TaskList> {
  static const double _scrollToTopThreshold = 400.0;
  static const double _gridGap = 10.0;
  static const double _gridMinCardWidth = 210.0;
  static const double _gridCardHeight = 138.0;

  final ScrollController _scrollController = ScrollController();
  bool _showScrollToTop = false;

  /// 分组头折叠状态（key=`ListSection.key`）。视图运行态，非持久化
  /// （design-proto-spec §1 `state.folded` 同为运行态，不进 `tabPrefs`）。
  final Set<String> _foldedSections = {};

  /// 「N 失败」直达高亮：目标成员 ID + 递增 epoch（每次触发换新 epoch，
  /// 驱动 GroupMemberRow 内部一次性淡出动画重新播放）。
  String? _flashMemberId;
  int _flashEpoch = 0;

  /// 组成员行 GlobalKey（仅成员行需要——`jumpToFail` 滚动定位依赖
  /// `GlobalKey.currentContext`；任务/组/目录行沿用轻量 ValueKey）。
  final Map<String, GlobalKey> _memberRowKeys = {};

  GlobalKey _memberKeyFor(String taskId) =>
      _memberRowKeys.putIfAbsent(taskId, () => GlobalKey());

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

  String get _tab => widget.controller.statusTab.name;
  ViewPrefs get _prefs => widget.viewPrefsStore.resolve(_tab);

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

  /// 桶内批量暂停（活跃/排队任务；组按 GroupControl 整组暂停）。
  void _bulkPauseSection(ListSection section) {
    for (final e in section.entities) {
      if (e is TaskEntity && e.task.status.isActiveOrQueued) {
        widget.controller.pauseTask(e.task.id);
      } else if (e is GroupEntity) {
        widget.controller.pauseGroup(e.groupId);
      }
      // GroupMemberEntity/GroupDirEntity：展开态下已由所属 GroupEntity 覆盖。
    }
  }

  /// 桶内批量重试（失败任务；组按 GroupControl 仅重试失败成员）。
  void _bulkRetrySection(ListSection section) {
    for (final e in section.entities) {
      if (e is TaskEntity && e.task.status == TaskStatus.error) {
        widget.controller.resumeTask(e.task.id);
      } else if (e is GroupEntity) {
        widget.controller.retryGroupFailed(e.groupId);
      }
    }
  }

  /// 「N 失败」直达：展开组 + 展开目标目录（controller 状态），滚动居中 +
  /// 1.6s 闪烁高亮（design-proto-spec §8 `jumpToFail`）。
  void _jumpToFail(GroupEntity group) {
    DownloadTask? failed;
    for (final m in group.members) {
      if (m.statusBucket == TaskStatus.error && m is TaskEntity) {
        failed = m.task;
        break;
      }
    }
    if (failed == null) return;
    final failedTask = failed;
    widget.controller.revealGroupMember(group.groupId, failedTask.id);
    setState(() {
      _flashMemberId = failedTask.id;
      _flashEpoch++;
    });
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final ctx = _memberRowKeys[failedTask.id]?.currentContext;
      if (ctx != null) {
        Scrollable.ensureVisible(
          ctx,
          alignment: 0.5,
          duration: const Duration(milliseconds: 300),
        );
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([widget.controller, widget.viewPrefsStore]),
      builder: (context, _) {
        final prefs = _prefs;
        final sections = widget.controller.buildListSections(prefs);
        final isEmpty = sections.every((s) => s.entities.isEmpty);
        return ColoredBox(
          color: c.bg,
          child: LayoutBuilder(
            builder: (context, constraints) {
              final listWidth = constraints.maxWidth;
              widget.controller.listContentWidth = listWidth;
              return Column(
                children: [
                  // 表头条两种形态都渲染：列表 = 管理+列名+显示选项按钮；
                  // 网格 = 管理+显示选项按钮（无列语义，不渲染列名）——
                  // 「显示选项」入口已从 titlebar 移到此处（用户决策），
                  // 网格形态必须保留可达入口。
                  _buildHeader(context, prefs, listWidth),
                  Expanded(
                    child: Stack(
                      children: [
                        isEmpty
                            ? _buildEmpty(context)
                            : prefs.form == ViewForm.list
                            ? _buildListBody(context, prefs, sections, listWidth)
                            : _buildGridBody(
                                context,
                                prefs,
                                sections,
                                listWidth,
                              ),
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
              );
            },
          ),
        );
      },
    );
  }

  // ===========================================================================
  // 列表形态：分组头吸顶 + 行虚拟化
  // ===========================================================================

  Widget _buildListBody(
    BuildContext context,
    ViewPrefs prefs,
    List<ListSection> sections,
    double listWidth,
  ) {
    final isManage = widget.controller.isManageMode;
    // 渲染期裁列：窗口变窄/详情面板打开后，勾选时合法的列组合可能超出
    // 当前预算——按重要性自动隐藏低优列，行内固定宽 Row 永不溢出。
    final columns = fitColumnsToWidth(effectiveColumns(prefs), listWidth);

    return CustomScrollView(
      controller: _scrollController,
      slivers: [
        for (final section in sections)
          SliverMainAxisGroup(
            slivers: [
              if (section.title != null)
                SliverPersistentHeader(
                  pinned: true,
                  delegate: _SectionHeaderDelegate(
                    section: section,
                    folded: _foldedSections.contains(section.key),
                    onToggleFold: () => setState(() {
                      if (!_foldedSections.add(section.key)) {
                        _foldedSections.remove(section.key);
                      }
                    }),
                    onBulkPause: section.meta.hasActive
                        ? () => _bulkPauseSection(section)
                        : null,
                    onBulkRetry:
                        !section.meta.hasActive && section.meta.hasError
                        ? () => _bulkRetrySection(section)
                        : null,
                  ),
                ),
              // 分组头折叠：只隐藏 body sliver，head 仍渲染（不分组时无 head，恒渲染 body）。
              if (section.title == null ||
                  !_foldedSections.contains(section.key))
                SliverList(
                  delegate: SliverChildBuilderDelegate((context, index) {
                    final entity = section.entities[index];
                    if (entity is GroupEntity) {
                      final downloadGroup = widget.controller.groupById(
                        entity.groupId,
                      );
                      if (downloadGroup == null) return const SizedBox.shrink();
                      final hasFailed = entity.members.any(
                        (m) => m.statusBucket == TaskStatus.error,
                      );
                      return RepaintBoundary(
                        key: ValueKey(entity.groupId),
                        child: TaskGroupRow(
                          group: entity,
                          downloadGroup: downloadGroup,
                          expanded: widget.controller.isGroupExpanded(
                            entity.groupId,
                          ),
                          isSelected:
                              entity.groupId ==
                              widget.controller.selectedGroupId,
                          density: prefs.density,
                          // 行点击 = 查看组详情；展开/收起仅由左侧 chevron 触发。
                          onTap: () =>
                              widget.onGroupTap?.call(entity.groupId),
                          onToggleExpand: () => widget.controller
                              .toggleGroupExpanded(entity.groupId),
                          onPauseAll: () =>
                              widget.controller.pauseGroup(entity.groupId),
                          onResumeAll: () =>
                              widget.controller.resumeGroup(entity.groupId),
                          onRetryFailed: hasFailed
                              ? () => widget.controller.retryGroupFailed(
                                  entity.groupId,
                                )
                              : null,
                          onOpenFolder: () => openFolder(downloadGroup.saveDir),
                          onCopySource: () => copyGroupSourceLink(
                            context,
                            downloadGroup.sourceUrl,
                          ),
                          onDelete: ({required bool deleteFiles}) => widget
                              .controller
                              .deleteGroup(
                                entity.groupId,
                                deleteFiles: deleteFiles,
                              ),
                          onJumpToFail: hasFailed
                              ? () => _jumpToFail(entity)
                              : null,
                        ),
                      );
                    }
                    if (entity is GroupMemberEntity) {
                      final task = entity.task;
                      return RepaintBoundary(
                        key: _memberKeyFor(task.id),
                        child: GroupMemberRow(
                          task: task,
                          density: prefs.density,
                          isSelected:
                              task.id == widget.controller.selectedTaskId,
                          flashEpoch:
                              task.id == _flashMemberId ? _flashEpoch : 0,
                          onTap: () => widget.onTaskTap?.call(task.id),
                          onPause: () => widget.controller.pauseTask(task.id),
                          onResume: () =>
                              widget.controller.resumeTask(task.id),
                          onOpenFile: () => openFile(task.filePath),
                          onMoreTapDown: (details) => showTaskContextMenu(
                            context,
                            details.globalPosition,
                            task: task,
                            onPause: () =>
                                widget.controller.pauseTask(task.id),
                            onResume: () =>
                                widget.controller.resumeTask(task.id),
                            onDelete: ({required bool deleteFiles}) => widget
                                .controller
                                .deleteTask(task.id, deleteFiles: deleteFiles),
                            isPriority:
                                widget.controller.priorityTaskId == task.id,
                            onBoost: () =>
                                widget.controller.setPriorityTask(task.id),
                            onEditThreads: () => showEditThreadsDialog(
                              context,
                              widget.controller,
                              task,
                            ),
                          ),
                        ),
                      );
                    }
                    if (entity is GroupDirEntity) {
                      return GroupDirRow(
                        key: ValueKey(entity.id),
                        entity: entity,
                        collapsed: widget.controller.isDirCollapsed(
                          entity.groupId,
                          entity.path,
                        ),
                        density: prefs.density,
                        onTap: () => widget.controller.toggleDirCollapsed(
                          entity.groupId,
                          entity.path,
                        ),
                      );
                    }
                    final task = (entity as TaskEntity).task;
                    return RepaintBoundary(
                      key: ValueKey(task.id),
                      child: TaskListItem(
                        task: task,
                        isSelected: task.id == widget.controller.selectedTaskId,
                        onTap: () => widget.onTaskTap?.call(task.id),
                        onDoubleTap: () => _onDoubleTap(task),
                        onPause: () => widget.controller.pauseTask(task.id),
                        onResume: () => widget.controller.resumeTask(task.id),
                        onDelete: ({required bool deleteFiles}) => widget
                            .controller
                            .deleteTask(task.id, deleteFiles: deleteFiles),
                        isPriority:
                            widget.controller.priorityTaskId == task.id,
                        onBoost: () =>
                            widget.controller.setPriorityTask(task.id),
                        onEditThreads: () => showEditThreadsDialog(
                          context,
                          widget.controller,
                          task,
                        ),
                        isPluginProcessing: widget.controller
                            .isPluginProcessing(task.id),
                        isManageMode: isManage,
                        isChecked: widget.controller.checkedTaskIds.contains(
                          task.id,
                        ),
                        onToggleChecked: () =>
                            widget.controller.toggleTaskChecked(task.id),
                        density: prefs.density,
                        columns: columns,
                        protocolBadges: prefs.protocolBadges,
                      ),
                    );
                  }, childCount: section.entities.length),
                ),
            ],
          ),
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

  // ===========================================================================
  // 网格 bento 形态：LayoutBuilder 算列数 + 行装箱虚拟化
  //
  // E10 偏离记录：SliverGrid 不支持跨列（组卡 2× 跨列），故采用行装箱
  // （贪心：组占 2 槽/任务占 1 槽，凑满列数换行）打成行单元 → SliverList
  // 行虚拟化，代替 SliverGrid。仍是扁平 Sliver 虚拟化，无嵌套滚动。
  // ===========================================================================

  Widget _buildGridBody(
    BuildContext context,
    ViewPrefs prefs,
    List<ListSection> sections,
    double listWidth,
  ) {
    final isManage = widget.controller.isManageMode;
    const gap = _gridGap;
    final usableWidth = (listWidth - 32).clamp(_gridMinCardWidth, double.infinity);
    final cols = (((usableWidth + gap) / (_gridMinCardWidth + gap)).floor())
        .clamp(1, 999);
    final cardWidth = (usableWidth - (cols - 1) * gap) / cols;

    return CustomScrollView(
      controller: _scrollController,
      slivers: [
        for (final section in sections)
          SliverMainAxisGroup(
            slivers: [
              if (section.title != null)
                SliverPersistentHeader(
                  pinned: true,
                  delegate: _SectionHeaderDelegate(
                    section: section,
                    folded: _foldedSections.contains(section.key),
                    onToggleFold: () => setState(() {
                      if (!_foldedSections.add(section.key)) {
                        _foldedSections.remove(section.key);
                      }
                    }),
                    onBulkPause: section.meta.hasActive
                        ? () => _bulkPauseSection(section)
                        : null,
                    onBulkRetry:
                        !section.meta.hasActive && section.meta.hasError
                        ? () => _bulkRetrySection(section)
                        : null,
                  ),
                ),
              if (section.title == null ||
                  !_foldedSections.contains(section.key))
                Builder(
                  builder: (context) {
                    final rows = _packGridRows(section.entities, cols);
                    return SliverPadding(
                      padding: const EdgeInsets.fromLTRB(16, 10, 16, 4),
                      sliver: SliverList(
                        delegate: SliverChildBuilderDelegate((context, index) {
                          return Padding(
                            padding: const EdgeInsets.only(bottom: gap),
                            child: _buildGridRow(
                              context,
                              rows[index],
                              cardWidth,
                              gap,
                              isManage,
                            ),
                          );
                        }, childCount: rows.length),
                      ),
                    );
                  },
                ),
            ],
          ),
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

  /// 贪心行装箱：任务占 1 槽，组占 2 槽（本波 [GroupEntity] 恒空集不产出，
  /// 此逻辑为下一波组卡片预留）。
  List<List<ListEntity>> _packGridRows(
    List<ListEntity> entities,
    int cols,
  ) {
    final rows = <List<ListEntity>>[];
    var current = <ListEntity>[];
    var used = 0;
    for (final e in entities) {
      final span = e is GroupEntity ? 2 : 1;
      if (used > 0 && used + span > cols) {
        rows.add(current);
        current = [];
        used = 0;
      }
      current.add(e);
      used += span;
    }
    if (current.isNotEmpty) rows.add(current);
    return rows;
  }

  Widget _buildGridRow(
    BuildContext context,
    List<ListEntity> row,
    double cardWidth,
    double gap,
    bool isManage,
  ) {
    final children = <Widget>[];
    for (var i = 0; i < row.length; i++) {
      if (i > 0) children.add(SizedBox(width: gap));
      final entity = row[i];
      final span = entity is GroupEntity ? 2 : 1;
      final width = cardWidth * span + gap * (span - 1);
      children.add(
        SizedBox(
          width: width,
          height: _gridCardHeight,
          child: _buildGridCard(context, entity, isManage),
        ),
      );
    }
    return Row(crossAxisAlignment: CrossAxisAlignment.start, children: children);
  }

  Widget _buildGridCard(BuildContext context, ListEntity entity, bool isManage) {
    if (entity is GroupEntity) {
      final downloadGroup = widget.controller.groupById(entity.groupId);
      if (downloadGroup == null) return const SizedBox.shrink();
      final hasFailed = entity.members.any(
        (m) => m.statusBucket == TaskStatus.error,
      );
      return TaskGroupCard(
        group: entity,
        downloadGroup: downloadGroup,
        isSelected: entity.groupId == widget.controller.selectedGroupId,
        onTap: () => widget.onGroupTap?.call(entity.groupId),
        onMoreTapDown: (details) => showGroupContextMenu(
          context,
          details.globalPosition,
          group: entity,
          onPauseAll: () => widget.controller.pauseGroup(entity.groupId),
          onResumeAll: () => widget.controller.resumeGroup(entity.groupId),
          onRetryFailed: hasFailed
              ? () => widget.controller.retryGroupFailed(entity.groupId)
              : null,
          onOpenFolder: () => openFolder(downloadGroup.saveDir),
          onCopySource: () =>
              copyGroupSourceLink(context, downloadGroup.sourceUrl),
          onDelete: ({required bool deleteFiles}) => widget.controller
              .deleteGroup(entity.groupId, deleteFiles: deleteFiles),
        ),
      );
    }
    final task = (entity as TaskEntity).task;
    return _TaskGridCard(
      task: task,
      isSelected: task.id == widget.controller.selectedTaskId,
      isManageMode: isManage,
      isChecked: widget.controller.checkedTaskIds.contains(task.id),
      onTap: () {
        if (isManage) {
          widget.controller.toggleTaskChecked(task.id);
        } else {
          widget.onTaskTap?.call(task.id);
        }
      },
      onDoubleTap: () => _onDoubleTap(task),
      onPause: () => widget.controller.pauseTask(task.id),
      onResume: () => widget.controller.resumeTask(task.id),
      onMoreTapDown: (details) => showTaskContextMenu(
        context,
        details.globalPosition,
        task: task,
        onPause: () => widget.controller.pauseTask(task.id),
        onResume: () => widget.controller.resumeTask(task.id),
        onDelete: ({required bool deleteFiles}) =>
            widget.controller.deleteTask(task.id, deleteFiles: deleteFiles),
        isPriority: widget.controller.priorityTaskId == task.id,
        onBoost: () => widget.controller.setPriorityTask(task.id),
        onEditThreads: () =>
            showEditThreadsDialog(context, widget.controller, task),
      ),
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

  // ===========================================================================
  // 列表头（36px，列表形态常驻；网格形态隐藏——由 build() 条件渲染）
  // ===========================================================================

  Widget _buildHeader(BuildContext context, ViewPrefs prefs, double listWidth) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final isManage = widget.controller.isManageMode;
    final hasTasks = widget.controller.filteredTasks.isNotEmpty;
    final isList = prefs.form == ViewForm.list;
    final columns = isList
        ? fitColumnsToWidth(effectiveColumns(prefs), listWidth)
        : const <TaskColumnId>[];

    return GestureDetector(
      // 右键表头 = 列勾选菜单（仅列表形态有列语义）。
      onSecondaryTapDown: isList
          ? (details) => _showColumnMenu(
              context,
              details.globalPosition,
              prefs,
              listWidth,
            )
          : null,
      child: Container(
        height: 36,
        decoration: BoxDecoration(
          color: c.surface1,
          border: Border(bottom: BorderSide(color: c.border, width: 1)),
        ),
        // Stack：基础 Row 与任务行完全同几何（padding 16/16 + Expanded 名称
        // + 定宽列），列名与行数据像素对齐；⊞ 列按钮改为零占位覆盖层
        // （design-proto DESIGN §4.1.6「嵌在滚动条补偿死区，零占位」），
        // 管理按钮收进名称 Expanded 区（标签之后），不再挤动任何列。
        child: Stack(
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Row(
                children: [
                  // 管理模式下列头显示全选复选框（行内复选框同为 20+10，对齐）
                  if (isManage) ...[
                    _HeaderCheckbox(controller: widget.controller),
                    const SizedBox(width: 10),
                  ],
                  Expanded(
                    child: Row(
                      children: [
                        if (isList)
                          Text(
                            s.colFileName,
                            style: TextStyle(
                              fontSize: 11,
                              fontWeight: FontWeight.w500,
                              color: c.textMuted,
                            ),
                          ),
                        // 管理入口（名称标签之后，被 Expanded 吸收，零错位）
                        if (hasTasks && !isManage) ...[
                          if (isList) const SizedBox(width: 8),
                          _ManageToggleButton(
                            onTap: () => widget.controller.toggleManageMode(),
                          ),
                        ],
                      ],
                    ),
                  ),
                  for (final col in columns)
                    SizedBox(
                      width: kTaskColumns[col]!.width,
                      child: Padding(
                        padding: col == TaskColumnId.progress
                            ? const EdgeInsets.only(right: 12)
                            : EdgeInsets.zero,
                        child: Center(
                          child: Text(
                            kTaskColumns[col]!.label(s),
                            style: TextStyle(
                              fontSize: 11,
                              fontWeight: FontWeight.w500,
                              color: c.textMuted,
                            ),
                          ),
                        ),
                      ),
                    ),
                ],
              ),
            ),
            // 「显示选项」按钮：表头右缘零占位覆盖层（原 ⊞ 位置；入口自
            // titlebar 移入，用户决策）。
            Positioned(
              right: 2,
              top: 0,
              bottom: 0,
              child: Center(
                child: _ViewOptionsHeaderButton(
                  controller: widget.controller,
                  viewPrefsStore: widget.viewPrefsStore,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  /// 两入口共用的列菜单（右键表头 / 显示选项面板「列」chips 走同一
  /// [tryToggleColumn] 状态机；表头 ⊞ 按钮已按用户决策移除——列配置入口
  /// 收敛到标题栏「显示选项」）。勾选后原位重开菜单，连续增删不打断。
  void _showColumnMenu(
    BuildContext context,
    Offset globalPosition,
    ViewPrefs prefs,
    double listWidth,
  ) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final compact = prefs.density == ViewDensity.compact;

    void toggle(TaskColumnId id) {
      final rejection = tryToggleColumn(
        current: prefs.columns,
        toggling: id,
        listWidth: listWidth,
        s: s,
      );
      if (rejection != null) {
        FluxSonner.of(context).show(
          ShadToast.destructive(
            title: Text(rejection),
            duration: const Duration(seconds: 2),
          ),
        );
        _showColumnMenu(context, globalPosition, prefs, listWidth);
        return;
      }
      final next = {...prefs.columns};
      if (next.contains(id)) {
        next.remove(id);
      } else {
        next.add(id);
      }
      widget.viewPrefsStore.update(_tab, (p) => p.copyWith(columns: next));
      _showColumnMenu(
        context,
        globalPosition,
        widget.viewPrefsStore.resolve(_tab),
        listWidth,
      );
    }

    final items = <ContextMenuItem>[
      for (final id in kColumnCanonicalOrder)
        ContextMenuItem(
          icon: prefs.columns.contains(id)
              ? LucideIcons.squareCheck
              : LucideIcons.square,
          label: compact && id == TaskColumnId.progress
              ? '${kTaskColumns[id]!.label(s)} (${s.viewColumnHintCompactRow})'
              : kTaskColumns[id]!.label(s),
          color: prefs.columns.contains(id) ? c.accent : c.textPrimary,
          action: () => toggle(id),
        ),
      ContextMenuItem(
        icon: LucideIcons.rotateCcw,
        label: s.viewColumnsResetAction,
        color: c.textPrimary,
        action: () => widget.viewPrefsStore.update(
          _tab,
          (p) => p.copyWith(columns: ViewPrefs.defaultColumns),
        ),
      ),
    ];

    showContextMenu(
      context,
      globalPosition,
      items: items,
      dividerAfterIndices: {items.length - 2},
      menuWidth: 230,
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
// 分组头（吸顶 + 玻璃 + 聚合信息 + hover 批量操作）
// =============================================================================

class _SectionHeaderDelegate extends SliverPersistentHeaderDelegate {
  final ListSection section;
  final bool folded;
  final VoidCallback onToggleFold;
  final VoidCallback? onBulkPause;
  final VoidCallback? onBulkRetry;

  const _SectionHeaderDelegate({
    required this.section,
    required this.folded,
    required this.onToggleFold,
    this.onBulkPause,
    this.onBulkRetry,
  });

  @override
  double get minExtent => 32;
  @override
  double get maxExtent => 32;

  @override
  Widget build(BuildContext context, double shrinkOffset, bool overlapsContent) {
    return _SectionHeaderRow(
      section: section,
      folded: folded,
      onToggleFold: onToggleFold,
      onBulkPause: onBulkPause,
      onBulkRetry: onBulkRetry,
    );
  }

  @override
  bool shouldRebuild(covariant _SectionHeaderDelegate oldDelegate) => true;
}

class _SectionHeaderRow extends StatefulWidget {
  final ListSection section;
  final bool folded;
  final VoidCallback onToggleFold;
  final VoidCallback? onBulkPause;
  final VoidCallback? onBulkRetry;

  const _SectionHeaderRow({
    required this.section,
    required this.folded,
    required this.onToggleFold,
    this.onBulkPause,
    this.onBulkRetry,
  });

  @override
  State<_SectionHeaderRow> createState() => _SectionHeaderRowState();
}

class _SectionHeaderRowState extends State<_SectionHeaderRow> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final meta = widget.section.meta;
    final metaText = meta.hasActive
        ? '↓ ${DownloadTask.formatBytes(meta.activeSpeedBytesPerSec)}/s'
        : DownloadTask.formatBytes(meta.totalBytes);

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onToggleFold,
        child: ClipRect(
          child: BackdropFilter(
            filter: ui.ImageFilter.blur(sigmaX: 10, sigmaY: 10),
            child: Container(
              height: 32,
              padding: const EdgeInsets.symmetric(horizontal: 16),
              decoration: BoxDecoration(
                color: m.glass(c.surface1),
                border: Border(bottom: BorderSide(color: c.border, width: 1)),
              ),
              child: Row(
                children: [
                  AnimatedRotation(
                    turns: widget.folded ? -0.25 : 0,
                    duration: const Duration(milliseconds: 150),
                    child: Icon(
                      LucideIcons.chevronDown,
                      size: 13,
                      color: c.textMuted,
                    ),
                  ),
                  const SizedBox(width: 6),
                  Flexible(
                    child: Text(
                      widget.section.title ?? '',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 12,
                        fontWeight: FontWeight.w500,
                        color: c.textSecondary,
                      ),
                    ),
                  ),
                  const SizedBox(width: 6),
                  Text(
                    '${widget.section.topLevelCount}',
                    style: TextStyle(fontSize: 11, color: c.textMuted),
                  ),
                  const Spacer(),
                  Text(
                    metaText,
                    style: TextStyle(
                      fontSize: 11,
                      color: c.textMuted,
                      fontFeatures: const [FontFeature.tabularFigures()],
                    ),
                  ),
                  if (_hovered &&
                      (widget.onBulkPause != null ||
                          widget.onBulkRetry != null)) ...[
                    const SizedBox(width: 10),
                    if (widget.onBulkPause != null)
                      _BucketBulkButton(
                        icon: LucideIcons.pause,
                        label: s.pauseAll,
                        onTap: widget.onBulkPause!,
                      ),
                    if (widget.onBulkRetry != null)
                      _BucketBulkButton(
                        icon: LucideIcons.rotateCcw,
                        label: s.viewBucketRetryAll,
                        onTap: widget.onBulkRetry!,
                      ),
                  ],
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _BucketBulkButton extends StatefulWidget {
  final IconData icon;
  final String label;
  final VoidCallback onTap;

  const _BucketBulkButton({
    required this.icon,
    required this.label,
    required this.onTap,
  });

  @override
  State<_BucketBulkButton> createState() => _BucketBulkButtonState();
}

class _BucketBulkButtonState extends State<_BucketBulkButton> {
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
        behavior: HitTestBehavior.opaque,
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
                widget.icon,
                size: 12,
                color: _isHovered ? c.textPrimary : c.textMuted,
              ),
              const SizedBox(width: 3),
              Text(
                widget.label,
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
// 网格 · 单任务卡（design-proto-spec §7 `.gcard`）
// =============================================================================

class _TaskGridCard extends StatefulWidget {
  final DownloadTask task;
  final bool isSelected;
  final bool isManageMode;
  final bool isChecked;
  final VoidCallback onTap;
  final VoidCallback onDoubleTap;
  final VoidCallback onPause;
  final VoidCallback onResume;
  final void Function(TapDownDetails) onMoreTapDown;

  const _TaskGridCard({
    required this.task,
    required this.isSelected,
    required this.isManageMode,
    required this.isChecked,
    required this.onTap,
    required this.onDoubleTap,
    required this.onPause,
    required this.onResume,
    required this.onMoreTapDown,
  });

  @override
  State<_TaskGridCard> createState() => _TaskGridCardState();
}

class _TaskGridCardState extends State<_TaskGridCard> {
  bool _hovered = false;
  DateTime? _lastTapTime;
  static const _doubleTapWindow = Duration(milliseconds: 280);

  void _handleTap() {
    final now = DateTime.now();
    final last = _lastTapTime;
    if (last != null && now.difference(last) < _doubleTapWindow) {
      _lastTapTime = null;
      widget.onDoubleTap();
    } else {
      _lastTapTime = now;
      widget.onTap();
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final task = widget.task;
    final isDone = task.status == TaskStatus.completed;
    final isErr = task.status == TaskStatus.error;
    final isDl = task.status == TaskStatus.downloading;
    final color = taskStatusColor(task.status, c, fileMissing: task.fileMissing);
    final selected = widget.isSelected || (widget.isManageMode && widget.isChecked);

    final metaText = isDl
        ? '${task.speedText} · ${(task.progress * 100).toStringAsFixed(0)}%'
        : isErr
        ? (task.errorMessage.isEmpty ? currentS.subtitleError : task.errorMessage)
        : '${task.statusText} · ${task.sizeText}';

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        // onTap（非 onTapDown）：竞技场裁决后触发，卡内操作按钮点击不再
        // 连带选中卡片（onTapDown 会在裁决前无条件触发）。
        onTap: _handleTap,
        onSecondaryTapDown: widget.onMoreTapDown,
        child: Stack(
          children: [
            // 普通 Container 即时切色（规则 no-lerp-from-transparent：悬浮/
            // 选中属即时状态切换，与侧栏 _NavItem/列表行一致，不加颜色动画）。
            Container(
              padding: const EdgeInsets.fromLTRB(12, 12, 12, 11),
              decoration: BoxDecoration(
                // 选中填充合成为不透明色：selectedBg 浅色主题是 accent@10%
                // 半透明，直接作填充时 hover 的 boxShadow 画在填充背后、
                // 透过卡体渗出 → 浑浊灰蓝。alphaBlend 到 surface1 上杜绝
                // 透射（深色主题 selectedBg 本就不透明，恒等无副作用）。
                color: selected
                    ? Color.alphaBlend(c.selectedBg, c.surface1)
                    : c.surface1,
                borderRadius: m.brCard,
                border: Border.all(
                  color: selected
                      ? c.accent
                      : isErr
                      ? AppColors.red
                      : c.border,
                  width: selected || isErr ? 1.5 : 1,
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
              transform: Matrix4.translationValues(
                0.0,
                _hovered ? -1.0 : 0.0,
                0.0,
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Container(
                        width: 36,
                        height: 36,
                        decoration: BoxDecoration(
                          color: c.surface2,
                          borderRadius: m.brIconTile,
                        ),
                        child: Center(
                          child: Text(
                            task.fileExtension,
                            style: TextStyle(
                              fontSize: 10,
                              fontWeight: FontWeight.w600,
                              color: c.textSecondary,
                              fontFeatures: const [
                                FontFeature.tabularFigures(),
                              ],
                            ),
                          ),
                        ),
                      ),
                      const Spacer(),
                      Icon(taskStatusIcon(task.status), size: 13, color: color),
                    ],
                  ),
                  const SizedBox(height: 8),
                  SizedBox(
                    height: 34,
                    child: ShadTooltip(
                      waitDuration: const Duration(milliseconds: 500),
                      builder: (_) => Text(task.fileName),
                      child: Text(
                        task.fileName,
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(
                          fontSize: 12.5,
                          // 显式行高 1.36：两行恰为 12.5×1.36×2 = 34px，
                          // 与定高盒子严丝合缝——MiSans 默认行高下两行 ≈35px
                          // 会被拦腰裁切（用户截图的半字截断）；行高收纳后
                          // maxLines+ellipsis 正常生效，超长名悬浮看全称。
                          height: 1.36,
                          fontWeight: FontWeight.w500,
                          color: isDone ? c.textSecondary : c.textPrimary,
                        ),
                      ),
                    ),
                  ),
                  const SizedBox(height: 8),
                  Container(
                    height: 4,
                    decoration: BoxDecoration(
                      color: c.surface3,
                      borderRadius: m.brProgress,
                    ),
                    clipBehavior: Clip.hardEdge,
                    child: task.isIndeterminate
                        ? null
                        : FractionallySizedBox(
                            alignment: Alignment.centerLeft,
                            widthFactor: task.progress.clamp(0.0, 1.0),
                            child: ColoredBox(color: color),
                          ),
                  ),
                  // 尾行弹性底对齐：卡片受行装箱固定高度约束（_gridCardHeight），
                  // 固定间距 + 字体行高浮动曾致底部溢出 4px——让剩余空间被
                  // Expanded 吸收，meta 行贴底，任何 ≥ 最小内容高的行高都不溢出。
                  Expanded(
                    child: Align(
                      alignment: Alignment.bottomLeft,
                      child: Padding(
                        padding: const EdgeInsets.only(top: 8),
                        child: Text(
                          metaText,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: 11,
                            color: isDl
                                ? AppColors.green
                                : isErr
                                ? AppColors.red
                                : c.textMuted,
                            fontFeatures: const [FontFeature.tabularFigures()],
                          ),
                        ),
                      ),
                    ),
                  ),
                ],
              ),
            ),
            if (widget.isManageMode)
              Positioned(
                left: 8,
                top: 8,
                child: Icon(
                  widget.isChecked
                      ? LucideIcons.squareCheck
                      : LucideIcons.square,
                  size: 16,
                  color: widget.isChecked ? c.accent : c.textMuted,
                ),
              ),
            if (_hovered && !widget.isManageMode)
              Positioned(
                right: 8,
                top: 8,
                child: TaskHoverActionCluster(
                  task: task,
                  onPause: widget.onPause,
                  onResume: widget.onResume,
                  onMoreTapDown: widget.onMoreTapDown,
                ),
              ),
          ],
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

// =============================================================================
// 表头「显示选项」按钮（原 titlebar 入口移入表头右缘，22×22 表头方块样式）
// =============================================================================

class _ViewOptionsHeaderButton extends StatefulWidget {
  final DownloadController controller;
  final ViewPrefsStore viewPrefsStore;

  const _ViewOptionsHeaderButton({
    required this.controller,
    required this.viewPrefsStore,
  });

  @override
  State<_ViewOptionsHeaderButton> createState() =>
      _ViewOptionsHeaderButtonState();
}

class _ViewOptionsHeaderButtonState extends State<_ViewOptionsHeaderButton> {
  final _popoverController = ShadPopoverController();
  bool _isHovered = false;

  @override
  void dispose() {
    _popoverController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([widget.controller, widget.viewPrefsStore]),
      builder: (context, _) {
        final c = AppColors.of(context);
        final tab = widget.controller.statusTab.name;
        final prefs = widget.viewPrefsStore.resolve(tab);
        return ShadPopover(
          controller: _popoverController,
          // 固定向下展开（overlay topRight 贴按钮 bottomRight——portal.dart
          // 中 childAlignment 实际作用于 overlay、overlayAlignment 作用于
          // 按钮）。不用 ShadAnchorAuto：按钮贴近窗口顶部，空间不足时的
          // 向上翻转会把面板裁出屏外；面板自身已限高滚动，向下恒有空间。
          anchor: const ShadAnchor(
            childAlignment: Alignment.topRight,
            overlayAlignment: Alignment.bottomRight,
            offset: Offset(0, 6),
          ),
          padding: EdgeInsets.zero,
          popover: (ctx) => ViewOptionsPanel(
            controller: widget.controller,
            viewPrefsStore: widget.viewPrefsStore,
          ),
          child: ShadTooltip(
            waitDuration: const Duration(milliseconds: 500),
            builder: (_) => Text(s.viewEntryTooltip(describeViewState(prefs))),
            child: MouseRegion(
              onEnter: (_) => setState(() => _isHovered = true),
              onExit: (_) => setState(() => _isHovered = false),
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: _popoverController.toggle,
                child: Container(
                  width: 22,
                  height: 22,
                  alignment: Alignment.center,
                  decoration: BoxDecoration(
                    color: _isHovered ? c.surface3 : Colors.transparent,
                    borderRadius: BorderRadius.circular(4),
                  ),
                  // 非默认态圆点已按用户决策移除：视图状态经 tooltip 与
                  // 状态栏右端回显表达，不再加角标提示。
                  child: Icon(
                    LucideIcons.slidersHorizontal,
                    size: 13,
                    color: _isHovered ? c.textPrimary : c.textMuted,
                  ),
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}
