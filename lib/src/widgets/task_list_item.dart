import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../theme/app_colors.dart';
import 'context_menu.dart';

class TaskListItem extends StatefulWidget {
  final DownloadTask task;
  final bool isSelected;
  final VoidCallback onTap;
  final VoidCallback onPause;
  final VoidCallback onResume;
  final void Function({required bool deleteFiles}) onDelete;

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
    this.isManageMode = false,
    this.isChecked = false,
    this.onToggleChecked,
  });

  @override
  State<TaskListItem> createState() => _TaskListItemState();
}

class _TaskListItemState extends State<TaskListItem> {
  bool _isHovered = false;

  void _showContextMenu(TapDownDetails details) {
    showTaskContextMenu(
      context,
      details.globalPosition,
      task: widget.task,
      onPause: widget.onPause,
      onResume: widget.onResume,
      onDelete: widget.onDelete,
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final isManage = widget.isManageMode;
    final isChecked = widget.isChecked;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: isManage ? widget.onToggleChecked : widget.onTap,
        onSecondaryTapDown: isManage ? null : _showContextMenu,
        child: Container(
          height: 64,
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
          decoration: BoxDecoration(
            color: isManage && isChecked
                ? c.accentBg
                : widget.isSelected
                ? c.accentBg
                : _isHovered
                ? c.hoverBg
                : Colors.transparent,
            border: Border(bottom: BorderSide(color: c.border, width: 1)),
          ),
          child: Row(
            children: [
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
              Expanded(child: _buildFileInfo(c)),
              SizedBox(width: 150, child: _buildProgress(c)),
              SizedBox(width: 100, child: _buildSpeed(c)),
              SizedBox(width: 60, child: _buildStatus(c)),
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildFileInfo(AppColors c) {
    final task = widget.task;
    return Row(
      children: [
        Container(
          width: 34,
          height: 34,
          decoration: BoxDecoration(
            color: c.surface2,
            borderRadius: BorderRadius.circular(6),
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
        ),
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

  Widget _buildProgress(AppColors c) {
    final task = widget.task;
    final percentage = (task.progress * 100).toStringAsFixed(1);
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
                    borderRadius: BorderRadius.circular(1.5),
                  ),
                  child: FractionallySizedBox(
                    alignment: Alignment.centerLeft,
                    widthFactor: task.progress,
                    child: Container(
                      decoration: BoxDecoration(
                        color: _progressColor(task, c),
                        borderRadius: BorderRadius.circular(1.5),
                      ),
                    ),
                  ),
                ),
              ),
              const SizedBox(width: 8),
              Text(
                '$percentage%',
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
        return AppColors.green;
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

  Widget _buildStatus(AppColors c) {
    final task = widget.task;
    Color statusColor;
    switch (task.status) {
      case TaskStatus.downloading:
      case TaskStatus.resuming:
      case TaskStatus.preparing:
        statusColor = c.accent;
      case TaskStatus.completed:
        statusColor = AppColors.green;
      case TaskStatus.paused:
        statusColor = AppColors.amber;
      case TaskStatus.error:
        statusColor = AppColors.red;
      case TaskStatus.pending:
        statusColor = c.textMuted;
    }
    return Text(
      task.statusText,
      style: TextStyle(fontSize: 12, color: statusColor),
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

  // 暂停/继续组后面加分隔线（如果有的话）
  if (items.isNotEmpty) {
    dividers.add(items.length - 1);
  }

  // --- 打开文件 / 打开所在文件夹 ---
  final filePath = '${task.saveDir}${Platform.pathSeparator}${task.fileName}';

  if (task.status == TaskStatus.completed) {
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
      action: () => _openFolder(filePath),
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
// 文件/文件夹操作
// =============================================================================

void _openFile(String filePath) {
  if (Platform.isWindows) {
    Process.run('cmd', ['/c', 'start', '', filePath]);
  } else if (Platform.isMacOS) {
    Process.run('open', [filePath]);
  } else if (Platform.isLinux) {
    Process.run('xdg-open', [filePath]);
  }
}

void _openFolder(String filePath) {
  final file = File(filePath);
  final dir = file.parent.path;

  if (file.existsSync()) {
    // 文件存在 — 打开目录并选中文件
    if (Platform.isWindows) {
      Process.run('explorer', ['/select,', filePath]);
    } else if (Platform.isMacOS) {
      Process.run('open', ['-R', filePath]);
    } else if (Platform.isLinux) {
      Process.run('xdg-open', [dir]);
    }
  } else {
    // 文件不存在（下载中/未完成）— 直接打开所在目录
    if (Platform.isWindows) {
      Process.run('explorer', [dir]);
    } else if (Platform.isMacOS) {
      Process.run('open', [dir]);
    } else if (Platform.isLinux) {
      Process.run('xdg-open', [dir]);
    }
  }
}

// =============================================================================
// 删除确认对话框
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
    barrierColor: const Color(0x1A000000),
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => ShadDialog(
      title: Text(
        s.deleteConfirmTitle(deleteFiles),
        style: TextStyle(
          fontSize: 16,
          fontWeight: FontWeight.w600,
          color: c.textPrimary,
        ),
      ),
      description: Text(
        s.deleteConfirmDesc(task.fileName, deleteFiles),
        style: TextStyle(fontSize: 13, color: c.textSecondary),
      ),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(ctx).pop(),
          child: Text(
            s.cancel,
            style: TextStyle(fontSize: 13, color: c.textPrimary),
          ),
        ),
        ShadButton.destructive(
          onPressed: () {
            Navigator.of(ctx).pop();
            onConfirm();
          },
          child: Text(
            s.deleteConfirmTitle(deleteFiles),
            style: const TextStyle(fontSize: 13, color: Colors.white),
          ),
        ),
      ],
    ),
  );
}

// =============================================================================
// 批量删除确认对话框
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
    barrierColor: const Color(0x1A000000),
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => ShadDialog(
      title: Text(
        s.batchDeleteConfirmTitle(deleteFiles),
        style: TextStyle(
          fontSize: 16,
          fontWeight: FontWeight.w600,
          color: c.textPrimary,
        ),
      ),
      description: Text(
        s.batchDeleteConfirmDesc(count, deleteFiles),
        style: TextStyle(fontSize: 13, color: c.textSecondary),
      ),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(ctx).pop(),
          child: Text(
            s.cancel,
            style: TextStyle(fontSize: 13, color: c.textPrimary),
          ),
        ),
        ShadButton.destructive(
          onPressed: () {
            Navigator.of(ctx).pop();
            onConfirm();
          },
          child: Text(
            s.batchDeleteConfirmTitle(deleteFiles),
            style: const TextStyle(fontSize: 13, color: Colors.white),
          ),
        ),
      ],
    ),
  );
}
