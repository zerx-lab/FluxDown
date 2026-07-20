import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'flux_sonner.dart';
import 'package:super_drag_and_drop/super_drag_and_drop.dart';
import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/view_prefs.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import '../models/download_controller.dart';
import '../services/open_folder.dart';
import 'queue_manager_dialog.dart';
import 'task_columns.dart';

/// 插件系统失败任务的错误消息前缀（引擎/hub/server 固定格式，逃生舱按钮据此判断）。
const _pluginErrorPrefix = '[插件]';

/// 舒适档默认列（现状硬编码 4 列，与 [ViewPrefs.defaultColumns] canonical
/// 顺序一致），保证不接入视图系统的调用点行为不变。
const _kDefaultColumns = [
  TaskColumnId.progress,
  TaskColumnId.speed,
  TaskColumnId.eta,
  TaskColumnId.status,
];

class TaskListItem extends StatefulWidget {
  final DownloadTask task;
  final bool isSelected;
  final VoidCallback onTap;
  final VoidCallback onPause;
  final VoidCallback onResume;
  final void Function({required bool deleteFiles}) onDelete;
  final VoidCallback? onDoubleTap;

  /// Boost 优先下载相关
  final bool isPriority;
  final VoidCallback? onBoost;

  /// 修改线程数（打开对话框）。null = 不显示该菜单项。
  final VoidCallback? onEditThreads;

  /// 插件钩子处理中（旁路 UI 指示，仅在 completed 状态下有意义）
  final bool isPluginProcessing;

  /// 管理模式相关
  final bool isManageMode;
  final bool isChecked;
  final VoidCallback? onToggleChecked;

  /// 视图系统：密度（舒适 64px / 紧凑 44px）。
  final ViewDensity density;

  /// 视图系统：本行渲染的列（已按 [effectiveColumns] 解析——紧凑档下
  /// progress 已替换为 size，本组件不再重复该映射）。
  final List<TaskColumnId> columns;

  /// 视图系统：协议徽标开关（关闭时副标题回退协议前缀）。
  final bool protocolBadges;

  const TaskListItem({
    super.key,
    required this.task,
    required this.isSelected,
    required this.onTap,
    required this.onPause,
    required this.onResume,
    required this.onDelete,
    this.onDoubleTap,
    this.isPriority = false,
    this.onBoost,
    this.onEditThreads,
    this.isPluginProcessing = false,
    this.isManageMode = false,
    this.isChecked = false,
    this.onToggleChecked,
    this.density = ViewDensity.comfortable,
    this.columns = _kDefaultColumns,
    this.protocolBadges = true,
  });

  @override
  State<TaskListItem> createState() => _TaskListItemState();
}

class _TaskListItemState extends State<TaskListItem> {
  bool _isHovered = false;

  // 手动双击检测：避免使用 GestureDetector.onDoubleTap，
  // 否则手势竞技场会为等待第二次点击而延迟单击响应。
  DateTime? _lastTapTime;
  static const _doubleTapWindow = Duration(milliseconds: 280);

  bool get _compact => widget.density == ViewDensity.compact;
  double get _rowHeight => _compact ? 44 : 64;
  double get _iconSize => _compact ? 24 : 34;

  /// 单击立即触发；若与上一次点击间隔在双击窗口内，则额外触发双击。
  void _handleTap() {
    final now = DateTime.now();
    final last = _lastTapTime;
    if (last != null && now.difference(last) < _doubleTapWindow) {
      _lastTapTime = null; // 消费掉，避免三连击误判
      widget.onDoubleTap?.call();
    } else {
      _lastTapTime = now;
      widget.onTap();
    }
  }

  void _showContextMenu(TapDownDetails details) {
    showTaskContextMenu(
      context,
      details.globalPosition,
      task: widget.task,
      onPause: widget.onPause,
      onResume: widget.onResume,
      onDelete: widget.onDelete,
      isPriority: widget.isPriority,
      onBoost: widget.onBoost,
      onEditThreads: widget.onEditThreads,
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final isManage = widget.isManageMode;
    final isChecked = widget.isChecked;
    final compact = _compact;

    final row = Row(
      children: [
        // 选中/勾选时左侧 accent 指示条
        if (widget.isSelected || (isManage && isChecked)) ...[
          Container(
            width: 3,
            height: compact ? 20 : 28,
            decoration: BoxDecoration(
              color: c.accent,
              borderRadius: m.brProgress,
            ),
          ),
          const SizedBox(width: 13),
        ],
        // 管理模式下显示复选框
        if (isManage) ...[
          SizedBox(
            width: 20,
            height: 20,
            child: Icon(
              isChecked ? LucideIcons.squareCheck : LucideIcons.square,
              size: 16,
              color: isChecked ? c.accent : c.textMuted,
            ),
          ),
          const SizedBox(width: 10),
        ],
        Expanded(child: _buildFileInfo(c, m, compact)),
        for (final col in widget.columns)
          SizedBox(
            width: kTaskColumns[col]!.width,
            child: kTaskColumns[col]!.cellBuilder(context, widget.task),
          ),
      ],
    );

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        // 管理模式：单击切换勾选（非幂等），无双击需求，直接用 onTap。
        // 非管理模式：onTap + 时间戳双击检测——本 detector 未注册
        //   onDoubleTap，onTap 在指针抬起、竞技场即时裁决后立刻触发，
        //   无 ~300ms 等待；且行内 hover 操作按钮（子 GestureDetector）
        //   赢得竞技场后本行 onTap 被正确拒绝，点操作按钮不再连带选中行
        //   （onTapDown 会在裁决前到点无条件触发，曾致点按钮穿透开详情）。
        onTap: isManage ? widget.onToggleChecked : _handleTap,
        onSecondaryTapDown: isManage ? null : _showContextMenu,
        child: Container(
          height: _rowHeight,
          decoration: BoxDecoration(
            color: isManage && isChecked
                ? c.selectedBg
                : widget.isSelected
                ? c.selectedBg
                : _isHovered
                ? c.hoverBg
                : Colors.transparent,
            border: Border(bottom: BorderSide(color: c.border, width: 1)),
          ),
          child: Stack(
            children: [
              Padding(
                padding: EdgeInsets.only(
                  left: (widget.isSelected || (isManage && isChecked)) ? 0 : 16,
                  right: 16,
                  top: compact ? 4 : 8,
                  bottom: compact ? 6 : 8,
                ),
                child: Stack(
                  clipBehavior: Clip.none,
                  children: [
                    row,
                    if (_isHovered && !isManage)
                      Positioned(
                        right: 0,
                        top: 0,
                        bottom: 0,
                        child: Center(
                          child: TaskHoverActionCluster(
                            task: widget.task,
                            onPause: widget.onPause,
                            onResume: widget.onResume,
                            onMoreTapDown: _showContextMenu,
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              // 紧凑档：进度移到行底边缘 2px 全宽条，信息不减、行高 -31%（P4）。
              if (compact)
                Positioned(
                  left: 0,
                  right: 0,
                  bottom: 0,
                  height: 2,
                  child: _CompactProgressEdge(task: widget.task),
                ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildFileInfo(AppColors c, AppMetrics m, bool compact) {
    final task = widget.task;
    // 已完成且文件仍在磁盘上的任务，文件图标支持拖出到资源管理器/其他应用。
    final canDragOut =
        task.status == TaskStatus.completed && !task.fileMissing;
    final iconSize = _iconSize;
    Widget icon = Container(
      width: iconSize,
      height: iconSize,
      decoration: BoxDecoration(
        color: c.surface2,
        borderRadius: compact ? m.brSm : m.brMd,
      ),
      child: Center(
        child: Text(
          task.fileExtension,
          style: TextStyle(
            fontSize: compact ? 8.5 : 10,
            fontWeight: FontWeight.w600,
            color: c.textSecondary,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
      ),
    );
    // 插件（onDone 钩子）仍在处理该已完成任务：文件图标外圈旋转扫光边框，
    // 纯旁路指示，不改变状态列布局。
    if (task.status == TaskStatus.completed && widget.isPluginProcessing) {
      final s = LocaleScope.of(context);
      icon = ShadTooltip(
        waitDuration: const Duration(milliseconds: 300),
        builder: (_) => Text(s.pluginProcessing),
        child: _PluginProcessingRing(
          borderRadius: compact ? m.brSm : m.brMd,
          color: c.accent,
          child: icon,
        ),
      );
    }
    if (canDragOut) {
      final filePath = task.filePath;
      final fileName = task.fileName;
      icon = DragItemWidget(
        allowedOperations: () => [DropOperation.copy, DropOperation.move],
        dragItemProvider: (request) async {
          final item = DragItem(suggestedName: fileName);
          item.add(Formats.fileUri(Uri.file(filePath)));
          return item;
        },
        child: DraggableWidget(child: icon),
      );
    }
    return Row(
      children: [
        // 优先下载时显示闪电图标徽章
        if (widget.isPriority) ...[
          Container(
            width: 18,
            height: 18,
            decoration: BoxDecoration(
              color: const Color(0xFFF59E0B), // amber-500
              borderRadius: m.brSm,
            ),
            child: const Center(
              child: Icon(LucideIcons.zap, size: 11, color: Colors.white),
            ),
          ),
          const SizedBox(width: 6),
        ],
        icon,
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Row(
                children: [
                  Flexible(
                    child: Text(
                      task.fileName,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(fontSize: 13, color: c.textPrimary),
                    ),
                  ),
                  if (widget.protocolBadges) ...[
                    const SizedBox(width: 6),
                    _ProtocolBadge(task: task),
                  ],
                ],
              ),
              if (!compact) ...[
                const SizedBox(height: 2),
                Text(
                  _subtitleText(),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
              ],
            ],
          ),
        ),
      ],
    );
  }

  /// 停止队列内的暂停任务显示「等待队列启动」，与用户手动暂停区分开
  /// （启动队列会按序恢复它们）。协议徽标开启时去重协议前缀
  /// （design-proto-spec §5 `subline`：badge 开时副标题不再重复协议）。
  String _subtitleText() {
    final task = widget.task;
    final full = task.subtitleWith(
      queueStopped: !(DownloadController.globalInstance?.isQueueRunning(
            task.queueId,
          ) ??
          true),
    );
    if (!widget.protocolBadges) return full;
    final prefix = '${task.protocolLabel} · ';
    return full.startsWith(prefix) ? full.substring(prefix.length) : full;
  }
}

// =============================================================================
// 协议徽标（9.5px 大写，design-proto-spec §5 `.badge`）
// =============================================================================

class _ProtocolBadge extends StatelessWidget {
  final DownloadTask task;
  const _ProtocolBadge({required this.task});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final label = task.siteKey == 'bt' ? 'BT' : task.protocolLabel;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 5, vertical: 1),
      decoration: BoxDecoration(
        color: c.surface2,
        borderRadius: const BorderRadius.all(Radius.circular(4)),
      ),
      child: Text(
        label.toUpperCase(),
        style: TextStyle(
          fontSize: 9.5,
          fontWeight: FontWeight.w600,
          letterSpacing: 0.2,
          color: c.textMuted,
        ),
      ),
    );
  }
}

// =============================================================================
// 紧凑档行底进度边缘条（2px 全宽，design-proto-spec §6 `.trow-edge`）
// =============================================================================

class _CompactProgressEdge extends StatelessWidget {
  final DownloadTask task;
  const _CompactProgressEdge({required this.task});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final color = taskStatusColor(task.status, c, fileMissing: task.fileMissing);
    return ColoredBox(
      color: c.surface3,
      child: task.isIndeterminate
          ? _IndeterminateBar(color: color)
          : Align(
              alignment: Alignment.centerLeft,
              child: FractionallySizedBox(
                widthFactor: task.progress.clamp(0.0, 1.0),
                heightFactor: 1,
                child: ColoredBox(color: color),
              ),
            ),
    );
  }
}

// =============================================================================
// hover 操作簇（28×28，右缘与状态列对齐，design-proto-spec §5 `.acts`）
// =============================================================================

class TaskHoverActionCluster extends StatelessWidget {
  final DownloadTask task;
  final VoidCallback onPause;
  final VoidCallback onResume;
  final void Function(TapDownDetails) onMoreTapDown;

  const TaskHoverActionCluster({
    super.key,
    required this.task,
    required this.onPause,
    required this.onResume,
    required this.onMoreTapDown,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final buttons = <Widget>[];
    switch (task.status) {
      case TaskStatus.downloading:
      case TaskStatus.pending:
      case TaskStatus.preparing:
      case TaskStatus.resuming:
        buttons.add(
          TaskActionButton(icon: LucideIcons.pause, primary: true, onTap: onPause),
        );
      case TaskStatus.paused:
      case TaskStatus.error:
        buttons.add(
          TaskActionButton(icon: LucideIcons.play, primary: true, onTap: onResume),
        );
      case TaskStatus.completed:
        break;
    }
    buttons.add(
      TaskActionButton(
        icon: LucideIcons.folderOpen,
        onTap: () => openFolder(task.revealFolderPath),
      ),
    );
    buttons.add(
      TaskActionButton(icon: LucideIcons.moreHorizontal, onTapDown: onMoreTapDown),
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
          for (var i = 0; i < buttons.length; i++) ...[
            if (i > 0) const SizedBox(width: 2),
            buttons[i],
          ],
        ],
      ),
    );
  }
}

/// 28×28 单个行/卡片操作按钮（design-proto-spec §5 `.act`）。
class TaskActionButton extends StatefulWidget {
  final IconData icon;
  final VoidCallback? onTap;
  final void Function(TapDownDetails)? onTapDown;
  final bool primary;

  const TaskActionButton({
    super.key,
    required this.icon,
    this.onTap,
    this.onTapDown,
    this.primary = false,
  });

  @override
  State<TaskActionButton> createState() => _TaskActionButtonState();
}

class _TaskActionButtonState extends State<TaskActionButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        onTapDown: widget.onTapDown,
        child: Container(
          width: 28,
          height: 28,
          decoration: BoxDecoration(
            color: widget.primary
                ? c.accentBg
                : _hovered
                ? c.hoverBg
                : Colors.transparent,
            borderRadius: m.brSm,
          ),
          child: Icon(
            widget.icon,
            size: 14,
            color: widget.primary ? c.accent : c.textSecondary,
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 不确定进度条（未知大小文件下载中）
// =============================================================================

class _IndeterminateBar extends StatefulWidget {
  final Color color;
  const _IndeterminateBar({required this.color});

  @override
  State<_IndeterminateBar> createState() => _IndeterminateBarState();
}

class _IndeterminateBarState extends State<_IndeterminateBar>
    with SingleTickerProviderStateMixin {
  late final AnimationController _ctrl;
  late final CurvedAnimation _curve;

  @override
  void initState() {
    super.initState();
    _ctrl = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 1500),
    )..repeat(reverse: true);
    _curve = CurvedAnimation(parent: _ctrl, curve: Curves.easeInOut);
  }

  @override
  void dispose() {
    _curve.dispose();
    _ctrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return AnimatedBuilder(
      animation: _curve,
      child: Container(
        decoration: BoxDecoration(
          color: widget.color,
          borderRadius: m.brProgress,
        ),
      ),
      builder: (context, child) {
        return FractionallySizedBox(
          alignment: Alignment(-1.0 + 2.0 * _curve.value, 0),
          widthFactor: 0.3,
          child: child,
        );
      },
    );
  }
}

// =============================================================================
// 任务行右键菜单
// =============================================================================

/// 显示任务右键菜单
void showTaskContextMenu(
  BuildContext context,
  Offset globalPosition, {
  required DownloadTask task,
  required VoidCallback onPause,
  required VoidCallback onResume,
  required void Function({required bool deleteFiles}) onDelete,
  bool isPriority = false,
  VoidCallback? onBoost,
  VoidCallback? onEditThreads,
}) {
  final c = AppColors.of(context);
  final s = LocaleScope.of(context);
  final items = <ContextMenuItem>[];
  final dividers = <int>{};

  // --- 暂停 / 继续 ---
  switch (task.status) {
    case TaskStatus.downloading:
    case TaskStatus.pending:
    case TaskStatus.preparing:
    case TaskStatus.resuming:
      items.add(
        ContextMenuItem(
          icon: LucideIcons.pause,
          label: s.pause,
          color: c.textPrimary,
          action: onPause,
        ),
      );
    case TaskStatus.paused:
    case TaskStatus.error:
      items.add(
        ContextMenuItem(
          icon: LucideIcons.play,
          label: s.resume,
          color: c.textPrimary,
          action: onResume,
        ),
      );
    case TaskStatus.completed:
      break;
  }

  // --- 忽略插件重试（逃生舱：插件解析失败任务专属）---
  if (task.status == TaskStatus.error &&
      task.errorMessage.startsWith(_pluginErrorPrefix)) {
    items.add(
      ContextMenuItem(
        icon: LucideIcons.shieldOff,
        label: s.taskIgnorePluginRetry,
        color: c.textPrimary,
        action: () => showIgnorePluginRetryDialog(context, taskId: task.id),
      ),
    );
  }

  // --- 优先下载 / 取消优先（仅对非完成任务显示）---
  if (task.status != TaskStatus.completed && onBoost != null) {
    items.add(
      ContextMenuItem(
        icon: isPriority ? LucideIcons.zapOff : LucideIcons.zap,
        label: isPriority ? s.cancelBoost : s.boostDownload,
        color: isPriority ? c.textPrimary : const Color(0xFFF59E0B),
        action: onBoost,
      ),
    );
  }

  // --- 修改线程数（非完成的 HTTP/FTP 任务）---
  final proto = task.protocolLabel;
  if (task.status != TaskStatus.completed &&
      onEditThreads != null &&
      (proto == 'HTTP' || proto == 'FTP')) {
    items.add(
      ContextMenuItem(
        icon: LucideIcons.settings2,
        label: s.editThreads,
        color: c.textPrimary,
        action: onEditThreads,
      ),
    );
  }

  // 暂停/继续/优先组后面加分隔线（如果有的话）
  if (items.isNotEmpty) {
    dividers.add(items.length - 1);
  }

  // --- 打开文件 / 打开所在文件夹 ---
  final filePath = task.filePath;
  final folderPath = task.revealFolderPath;

  if (task.status == TaskStatus.completed && !task.fileMissing) {
    items.add(
      ContextMenuItem(
        icon: LucideIcons.externalLink,
        label: s.openFile,
        color: c.textPrimary,
        action: () => _openFile(filePath),
      ),
    );
  }
  items.add(
    ContextMenuItem(
      icon: LucideIcons.folderOpen,
      label: s.openFolder,
      color: c.textPrimary,
      action: () => _openFolder(folderPath),
    ),
  );
  dividers.add(items.length - 1); // 文件操作组后加分隔线

  // --- 复制下载地址 ---
  items.add(
    ContextMenuItem(
      icon: LucideIcons.copy,
      label: s.copyUrl,
      color: c.textPrimary,
      action: () {
        Clipboard.setData(ClipboardData(text: task.url));
        FluxSonner.of(context).show(
          ShadToast(
            title: Text(s.urlCopied),
            duration: const Duration(seconds: 2),
          ),
        );
      },
    ),
  );

  // --- 移动到队列（与复制同组）---
  final queueCtrl = DownloadController.globalInstance;
  if (queueCtrl != null && queueCtrl.queues.isNotEmpty) {
    items.add(
      ContextMenuItem(
        icon: LucideIcons.layers,
        label: s.moveToQueueAction,
        color: c.textPrimary,
        action: () => showMoveToQueueDialog(context, queueCtrl, task),
      ),
    );
  }
  dividers.add(items.length - 1); // 复制组后加分隔线

  // --- 删除选项 ---
  items.add(
    ContextMenuItem(
      icon: LucideIcons.trash2,
      label: s.deleteTask,
      color: c.textPrimary,
      action: () => showDeleteConfirmDialog(
        context,
        task: task,
        deleteFiles: false,
        onConfirm: () => onDelete(deleteFiles: false),
      ),
    ),
  );
  items.add(
    ContextMenuItem(
      icon: LucideIcons.fileX,
      label: s.deleteTaskAndFile,
      color: AppColors.red,
      action: () => showDeleteConfirmDialog(
        context,
        task: task,
        deleteFiles: true,
        onConfirm: () => onDelete(deleteFiles: true),
      ),
    ),
  );

  showContextMenu(
    context,
    globalPosition,
    items: items,
    dividerAfterIndices: dividers,
  );
}

// =============================================================================
// 忽略插件重试确认对话框（逃生舱）
// =============================================================================

/// 逃生舱：确认后忽略插件重新解析，直接用原始链接恢复下载。
void showIgnorePluginRetryDialog(BuildContext context, {required String taskId}) {
  if (!context.mounted) return;
  final c = AppColors.of(context);
  final s = LocaleScope.of(context);
  showShadDialog(
    context: context,
    barrierColor: c.dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => ShadDialog(
      title: Text(s.taskIgnorePluginRetryTitle),
      description: Text(s.taskIgnorePluginRetryMsg),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(ctx).pop(),
          child: Text(s.cancel),
        ),
        ShadButton(
          onPressed: () {
            Navigator.of(ctx).pop();
            IgnorePluginRetry(taskId: taskId).sendSignalToRust();
          },
          child: Text(s.taskIgnorePluginRetry),
        ),
      ],
    ),
  );
}

// =============================================================================
// 文件/文件夹操作
// =============================================================================

Future<void> _openFile(String filePath) => openFile(filePath);

Future<void> _openFolder(String filePath) => openFolder(filePath);

// =============================================================================
// 单任务删除确认对话框（原有，保留兼容性）
// =============================================================================

void showDeleteConfirmDialog(
  BuildContext context, {
  required DownloadTask task,
  required bool deleteFiles,
  required VoidCallback onConfirm,
}) {
  if (!context.mounted) return;
  final c = AppColors.of(context);
  final s = LocaleScope.of(context);
  showShadDialog(
    context: context,
    barrierColor: c.dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => _DeleteConfirmDialogContent(
      title: s.deleteConfirmTitle(deleteFiles),
      description: s.deleteConfirmDesc(task.fileName, deleteFiles),
      cancelLabel: s.cancel,
      confirmLabel: s.deleteConfirmTitle(deleteFiles),
      isDeleteFiles: deleteFiles,
      onCancel: () => Navigator.of(ctx).pop(),
      onConfirm: () {
        Navigator.of(ctx).pop();
        onConfirm();
      },
    ),
  );
}

// =============================================================================
// 批量删除确认对话框（旧，保留兼容性，供管理栏按钮调用）
// =============================================================================

void showBatchDeleteConfirmDialog(
  BuildContext context, {
  required int count,
  required bool deleteFiles,
  required VoidCallback onConfirm,
}) {
  if (!context.mounted) return;
  final c = AppColors.of(context);
  final s = LocaleScope.of(context);
  showShadDialog(
    context: context,
    barrierColor: c.dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => _DeleteConfirmDialogContent(
      title: s.batchDeleteConfirmTitle(deleteFiles),
      description: s.batchDeleteConfirmDesc(count, deleteFiles),
      cancelLabel: s.cancel,
      confirmLabel: s.batchDeleteConfirmTitle(deleteFiles),
      isDeleteFiles: deleteFiles,
      onCancel: () => Navigator.of(ctx).pop(),
      onConfirm: () {
        Navigator.of(ctx).pop();
        onConfirm();
      },
    ),
  );
}

// =============================================================================
// 批量删除双选项对话框（Del 快捷键触发）
// =============================================================================

/// Del 快捷键触发的批量删除对话框。
///
/// 同时展示两个操作按钮：
/// - 删除任务（保留文件）  → Enter
/// - 删除任务和文件        → Ctrl+Enter
void showBatchDeleteDialog(
  BuildContext context, {
  required int count,
  required VoidCallback onDeleteTask,
  required VoidCallback onDeleteTaskAndFile,
}) {
  if (!context.mounted) return;
  final c = AppColors.of(context);
  final s = LocaleScope.of(context);
  showShadDialog(
    context: context,
    barrierColor: c.dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => _BatchDeleteDialogContent(
      count: count,
      cancelLabel: s.cancel,
      deleteTaskLabel: s.batchDeleteTask,
      deleteTaskAndFileLabel: s.batchDeleteTaskAndFile,
      description: s.batchDeleteConfirmDesc(count, false),
      onCancel: () => Navigator.of(ctx).pop(),
      onDeleteTask: () {
        Navigator.of(ctx).pop();
        onDeleteTask();
      },
      onDeleteTaskAndFile: () {
        Navigator.of(ctx).pop();
        onDeleteTaskAndFile();
      },
    ),
  );
}

// =============================================================================
// 删除确认对话框内容组件（单按钮确认，原有逻辑保留）
// =============================================================================

/// 单任务与管理栏批量删除的共用对话框内容组件（单确认按钮）。
class _DeleteConfirmDialogContent extends StatefulWidget {
  final String title;
  final String description;
  final String cancelLabel;
  final String confirmLabel;
  final bool isDeleteFiles;
  final VoidCallback onCancel;
  final VoidCallback onConfirm;

  const _DeleteConfirmDialogContent({
    required this.title,
    required this.description,
    required this.cancelLabel,
    required this.confirmLabel,
    required this.isDeleteFiles,
    required this.onCancel,
    required this.onConfirm,
  });

  @override
  State<_DeleteConfirmDialogContent> createState() =>
      _DeleteConfirmDialogContentState();
}

class _DeleteConfirmDialogContentState
    extends State<_DeleteConfirmDialogContent> {
  late final FocusNode _focusNode;

  @override
  void initState() {
    super.initState();
    _focusNode = FocusNode();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focusNode.requestFocus();
    });
  }

  @override
  void dispose() {
    _focusNode.dispose();
    super.dispose();
  }

  void _handleKey(KeyEvent event) {
    if (event is! KeyDownEvent) return;
    if (event.logicalKey == LogicalKeyboardKey.enter) {
      widget.onConfirm();
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return KeyboardListener(
      focusNode: _focusNode,
      onKeyEvent: _handleKey,
      child: ShadDialog(
        title: Text(
          widget.title,
          style: TextStyle(
            fontSize: 16,
            fontWeight: FontWeight.w600,
            color: c.textPrimary,
          ),
        ),
        description: Text(
          widget.description,
          style: TextStyle(fontSize: 13, color: c.textSecondary),
        ),
        actions: [
          ShadButton.outline(
            onPressed: widget.onCancel,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(LucideIcons.x, size: 13, color: c.textPrimary),
                const SizedBox(width: 5),
                Text(
                  widget.cancelLabel,
                  style: TextStyle(fontSize: 13, color: c.textPrimary),
                ),
              ],
            ),
          ),
          ShadButton.destructive(
            onPressed: widget.onConfirm,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  widget.isDeleteFiles ? LucideIcons.fileX : LucideIcons.trash2,
                  size: 13,
                  color: Colors.white,
                ),
                const SizedBox(width: 5),
                Text(
                  widget.confirmLabel,
                  style: const TextStyle(fontSize: 13, color: Colors.white),
                ),
                const SizedBox(width: 6),
                _KeyBadge(label: '↵'),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

// =============================================================================
// 批量删除双选项对话框内容组件（Del 快捷键触发）
// =============================================================================

/// Del 快捷键弹出的对话框：同时展示两个删除操作。
///
/// 键盘行为：
/// - Enter       → 删除任务（保留文件）
/// - Ctrl+Enter  → 删除任务和文件
/// - Escape      → 取消
class _BatchDeleteDialogContent extends StatefulWidget {
  final int count;
  final String cancelLabel;
  final String deleteTaskLabel;
  final String deleteTaskAndFileLabel;
  final String description;
  final VoidCallback onCancel;
  final VoidCallback onDeleteTask;
  final VoidCallback onDeleteTaskAndFile;

  const _BatchDeleteDialogContent({
    required this.count,
    required this.cancelLabel,
    required this.deleteTaskLabel,
    required this.deleteTaskAndFileLabel,
    required this.description,
    required this.onCancel,
    required this.onDeleteTask,
    required this.onDeleteTaskAndFile,
  });

  @override
  State<_BatchDeleteDialogContent> createState() =>
      _BatchDeleteDialogContentState();
}

class _BatchDeleteDialogContentState extends State<_BatchDeleteDialogContent> {
  late final FocusNode _focusNode;

  @override
  void initState() {
    super.initState();
    _focusNode = FocusNode();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focusNode.requestFocus();
    });
  }

  @override
  void dispose() {
    _focusNode.dispose();
    super.dispose();
  }

  void _handleKey(KeyEvent event) {
    if (event is! KeyDownEvent) return;
    if (event.logicalKey != LogicalKeyboardKey.enter) return;

    if (HardwareKeyboard.instance.isControlPressed) {
      widget.onDeleteTaskAndFile();
    } else {
      widget.onDeleteTask();
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return KeyboardListener(
      focusNode: _focusNode,
      onKeyEvent: _handleKey,
      child: ShadDialog(
        title: Text(
          widget.deleteTaskLabel,
          style: TextStyle(
            fontSize: 16,
            fontWeight: FontWeight.w600,
            color: c.textPrimary,
          ),
        ),
        description: Text(
          widget.description,
          style: TextStyle(fontSize: 13, color: c.textSecondary),
        ),
        actions: [
          // 取消
          ShadButton.outline(
            onPressed: widget.onCancel,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(LucideIcons.x, size: 13, color: c.textPrimary),
                const SizedBox(width: 5),
                Text(
                  widget.cancelLabel,
                  style: TextStyle(fontSize: 13, color: c.textPrimary),
                ),
              ],
            ),
          ),
          // 删除任务（保留文件）
          ShadButton.destructive(
            onPressed: widget.onDeleteTask,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(LucideIcons.trash2, size: 13, color: Colors.white),
                const SizedBox(width: 5),
                Text(
                  widget.deleteTaskLabel,
                  style: const TextStyle(fontSize: 13, color: Colors.white),
                ),
                const SizedBox(width: 6),
                _KeyBadge(label: '↵'),
              ],
            ),
          ),
          // 删除任务和文件
          ShadButton.destructive(
            onPressed: widget.onDeleteTaskAndFile,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(LucideIcons.fileX, size: 13, color: Colors.white),
                const SizedBox(width: 5),
                Text(
                  widget.deleteTaskAndFileLabel,
                  style: const TextStyle(fontSize: 13, color: Colors.white),
                ),
                const SizedBox(width: 6),
                _KeyBadge(label: 'Ctrl+↵'),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

// =============================================================================
// 快捷键 Badge 组件
// =============================================================================

/// 显示快捷键提示的小徽章（如 "↵"、"Ctrl+↵"）。
class _KeyBadge extends StatelessWidget {
  final String label;

  const _KeyBadge({required this.label});

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 5, vertical: 1),
      decoration: BoxDecoration(
        // 刻意保留：Ctrl+↵ 快捷键徽章叠加在危险态深色按钮上的白色薄底，
        // 强制白底（非主题色）保证对比度，一次性装饰值。
        color: Colors.white.withValues(alpha: 0.2),
        borderRadius: m.brSm,
      ),
      child: Text(
        label,
        style: const TextStyle(
          fontSize: 11,
          color: Colors.white,
          fontWeight: FontWeight.w500,
          height: 1.3,
        ),
      ),
    );
  }
}

// =============================================================================
// 插件处理中 — 文件图标外圈旋转扫光边框（旁路指示，不影响任务状态机）
// =============================================================================

class _PluginProcessingRing extends StatefulWidget {
  final BorderRadius borderRadius;
  final Color color;
  final Widget child;

  const _PluginProcessingRing({
    required this.borderRadius,
    required this.color,
    required this.child,
  });

  @override
  State<_PluginProcessingRing> createState() => _PluginProcessingRingState();
}

class _PluginProcessingRingState extends State<_PluginProcessingRing>
    with SingleTickerProviderStateMixin {
  late final AnimationController _ctrl = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1400),
  )..repeat();

  @override
  void dispose() {
    _ctrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _ctrl,
      builder: (context, child) => CustomPaint(
        foregroundPainter: _SweepBorderPainter(
          progress: _ctrl.value,
          color: widget.color,
          borderRadius: widget.borderRadius,
        ),
        child: child,
      ),
      child: widget.child,
    );
  }
}

/// 旋转扫光描边：SweepGradient 沿圆角矩形边框旋转，形成「追光」环。
class _SweepBorderPainter extends CustomPainter {
  final double progress;
  final Color color;
  final BorderRadius borderRadius;

  _SweepBorderPainter({
    required this.progress,
    required this.color,
    required this.borderRadius,
  });

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    final rrect = borderRadius.toRRect(rect.deflate(0.75));
    final paint = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.5
      ..strokeCap = StrokeCap.round
      ..shader = SweepGradient(
        colors: [
          color.withValues(alpha: 0),
          color.withValues(alpha: 0),
          color,
        ],
        stops: const [0.0, 0.55, 1.0],
        transform: GradientRotation(progress * 2 * math.pi),
      ).createShader(rect);
    canvas.drawRRect(rrect, paint);
  }

  @override
  bool shouldRepaint(_SweepBorderPainter old) =>
      old.progress != progress ||
      old.color != color ||
      old.borderRadius != borderRadius;
}
