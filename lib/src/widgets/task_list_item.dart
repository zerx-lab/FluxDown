import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:super_drag_and_drop/super_drag_and_drop.dart';
import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import '../services/open_folder.dart';

/// 插件系统失败任务的错误消息前缀（引擎/hub/server 固定格式，逃生舱按钮据此判断）。
const _pluginErrorPrefix = '[插件]';

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

  /// 插件钩子处理中（旁路 UI 指示，仅在 completed 状态下有意义）
  final bool isPluginProcessing;

  /// 管理模式相关
  final bool isManageMode;
  final bool isChecked;
  final VoidCallback? onToggleChecked;

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
    this.isPluginProcessing = false,
    this.isManageMode = false,
    this.isChecked = false,
    this.onToggleChecked,
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

  /// 单击立即触发；若与上一次点击间隔在双击窗口内，则额外触发双击。
  void _handleTapDown() {
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
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final isManage = widget.isManageMode;
    final isChecked = widget.isChecked;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        // 管理模式：单击切换勾选（非幂等），无双击需求，直接用 onTap。
        // 非管理模式：用 onTapDown + 手动双击检测，单击立即响应、零延迟，
        //   双击仍可触发；避免 GestureDetector.onDoubleTap 因等待第二击
        //   而把单击延迟 ~300ms。
        onTap: isManage ? widget.onToggleChecked : null,
        onTapDown: isManage ? null : (_) => _handleTapDown(),
        onSecondaryTapDown: isManage ? null : _showContextMenu,
        child: Container(
          height: 64,
          padding: EdgeInsets.only(
            left: (widget.isSelected || (isManage && isChecked)) ? 0 : 16,
            right: 16,
            top: 8,
            bottom: 8,
          ),
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
          child: Row(
            children: [
              // 选中/勾选时左侧 accent 指示条
              if (widget.isSelected || (isManage && isChecked)) ...[
                Container(
                  width: 3,
                  height: 28,
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
              Expanded(child: _buildFileInfo(c, m)),
              SizedBox(width: 150, child: _buildProgress(c, m)),
              SizedBox(width: 90, child: _buildSpeed(c)),
              SizedBox(width: 80, child: _buildEta(c)),
              SizedBox(width: 60, child: _buildStatus(c)),
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildFileInfo(AppColors c, AppMetrics m) {
    final task = widget.task;
    // 已完成且文件仍在磁盘上的任务，文件图标支持拖出到资源管理器/其他应用。
    final canDragOut =
        task.status == TaskStatus.completed && !task.fileMissing;
    Widget icon = Container(
      width: 34,
      height: 34,
      decoration: BoxDecoration(
        color: c.surface2,
        borderRadius: m.brMd,
      ),
      child: Center(
        child: Text(
          task.fileExtension,
          style: TextStyle(
            fontSize: 10,
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
          borderRadius: m.brMd,
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
              Text(
                task.fileName,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 13, color: c.textPrimary),
              ),
              const SizedBox(height: 2),
              Text(
                task.subtitle,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _buildProgress(AppColors c, AppMetrics m) {
    final task = widget.task;
    final percentage = (task.progress * 100).toStringAsFixed(1);
    final progressColor = _progressColor(task, c);

    return Padding(
      padding: const EdgeInsets.only(right: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          Row(
            children: [
              Expanded(
                child: Container(
                  height: 3,
                  decoration: BoxDecoration(
                    color: c.surface3,
                    borderRadius: m.brProgress,
                  ),
                  clipBehavior: Clip.hardEdge,
                  child: task.isIndeterminate
                      ? _IndeterminateBar(color: progressColor)
                      : FractionallySizedBox(
                          alignment: Alignment.centerLeft,
                          widthFactor: task.progress,
                          child: Container(
                            decoration: BoxDecoration(
                              color: progressColor,
                            borderRadius: m.brProgress,
                            ),
                          ),
                        ),
                ),
              ),
              const SizedBox(width: 8),
              Text(
                task.isIndeterminate ? '—' : '$percentage%',
                style: TextStyle(
                  fontSize: 12,
                  color: c.textSecondary,
                  fontFeatures: const [FontFeature.tabularFigures()],
                ),
              ),
            ],
          ),
        ],
      ),
    );
  }

  Color _progressColor(DownloadTask task, AppColors c) {
    switch (task.status) {
      case TaskStatus.downloading:
      case TaskStatus.pending:
      case TaskStatus.preparing:
      case TaskStatus.resuming:
        return c.accent;
      case TaskStatus.completed:
        return task.fileMissing ? AppColors.amber : AppColors.green;
      case TaskStatus.paused:
        return AppColors.amber;
      case TaskStatus.error:
        return AppColors.red;
    }
  }

  Widget _buildSpeed(AppColors c) {
    final task = widget.task;
    final isActive = task.status == TaskStatus.downloading;
    return Center(
      child: Text(
        task.speedText,
        style: TextStyle(
          fontSize: 12,
          color: isActive ? AppColors.green : c.textMuted,
          fontFeatures: const [FontFeature.tabularFigures()],
        ),
      ),
    );
  }

  Widget _buildEta(AppColors c) {
    final task = widget.task;
    final isActive = task.status == TaskStatus.downloading;
    return Center(
      child: Text(
        task.etaText,
        style: TextStyle(
          fontSize: 12,
          color: isActive ? c.textSecondary : c.textMuted,
          fontFeatures: const [FontFeature.tabularFigures()],
        ),
      ),
    );
  }

  Widget _buildStatus(AppColors c) {
    final task = widget.task;
    Color statusColor;
    switch (task.status) {
      case TaskStatus.downloading:
      case TaskStatus.resuming:
      case TaskStatus.preparing:
        statusColor = c.accent;
      case TaskStatus.completed:
        statusColor = task.fileMissing ? AppColors.amber : AppColors.green;
      case TaskStatus.paused:
        statusColor = AppColors.amber;
      case TaskStatus.error:
        statusColor = AppColors.red;
      case TaskStatus.pending:
        statusColor = c.textMuted;
    }
    final statusText = Text(
      task.statusText,
      style: TextStyle(fontSize: 12, color: statusColor),
    );
    return Center(child: statusText);
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
        ShadSonner.of(context).show(
          ShadToast(
            title: Text(s.urlCopied),
            duration: const Duration(seconds: 2),
          ),
        );
      },
    ),
  );
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
