import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../../i18n/locale_provider.dart';
import '../../models/download_controller.dart';
import '../../models/download_task.dart';
import '../../theme/app_colors.dart';
import '../../theme/app_metrics.dart';
import '../mobile_ui.dart';

/// 任务动作面板（长按卡片 / 详情页「⋯」唤起）
Future<void> showMobileTaskActionSheet(
  BuildContext context,
  DownloadController controller,
  DownloadTask task,
) {
  return showMobileSheet<void>(
    context,
    builder: (ctx) {
      final s = LocaleScope.of(ctx);
      final c = AppColors.of(ctx);
      final m = AppMetrics.of(ctx);
      final boosted = controller.priorityTaskId == task.id;

      // 宫格动作 tile：图标在上、文字在下，宽度随面板自适应分列
      Widget tile({
        required IconData icon,
        required String label,
        required VoidCallback onTap,
        bool danger = false,
      }) {
        final fg = danger ? c.statusError : c.textPrimary;
        return GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onTap,
          child: Container(
            padding: const EdgeInsets.symmetric(vertical: 14, horizontal: 6),
            decoration: BoxDecoration(
              color: m.glass(c.surface1),
              borderRadius: m.brMobileCard,
              border: Border.all(
                color: danger ? m.borderFade(c.statusError) : c.border,
              ),
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  icon,
                  size: 20,
                  color: danger ? c.statusError : c.textSecondary,
                ),
                const SizedBox(height: 8),
                Text(
                  label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  textAlign: TextAlign.center,
                  style: TextStyle(
                    fontSize: 12,
                    fontWeight: FontWeight.w500,
                    color: fg,
                  ),
                ),
              ],
            ),
          ),
        );
      }

      // 暂停 ⇄ 继续 / 重试：仅非终态任务展示
      final (IconData toggleIcon, String toggleLabel) = switch (task.status) {
        TaskStatus.downloading ||
        TaskStatus.preparing ||
        TaskStatus.resuming => (LucideIcons.pause, s.pause),
        TaskStatus.error => (LucideIcons.rotateCcw, s.mobileRetry),
        _ => (LucideIcons.play, s.resume),
      };

      final toggleItem = task.status != TaskStatus.completed
          ? tile(
              icon: toggleIcon,
              label: toggleLabel,
              onTap: () {
                Navigator.of(ctx).pop();
                _toggleTask(controller, task);
              },
            )
          : null;

      // Boost 与移动到队列对已完成任务无意义
      final boostItem = task.status != TaskStatus.completed
          ? tile(
              icon: LucideIcons.zap,
              label: boosted ? s.cancelBoost : s.mobileBoostAction,
              onTap: () {
                Navigator.of(ctx).pop();
                controller.setPriorityTask(boosted ? '' : task.id);
                showMobileToast(
                  context,
                  boosted ? s.mobileBoostOff : s.mobileBoostOn,
                );
              },
            )
          : null;

      final queueItem = task.status != TaskStatus.completed
          ? tile(
              icon: LucideIcons.layers,
              label: s.mobileMoveToQueue,
              onTap: () {
                Navigator.of(ctx).pop();
                _showMoveToQueueSheet(context, controller, task);
              },
            )
          : null;

      final copyItem = tile(
        icon: LucideIcons.copy,
        label: s.copyUrl,
        onTap: () {
          Navigator.of(ctx).pop();
          Clipboard.setData(ClipboardData(text: task.url));
          showMobileToast(context, s.urlCopied);
        },
      );

      // 平铺为一个宫格：常规动作在前，危险动作在后
      final tiles = <Widget>[
        ?toggleItem,
        ?boostItem,
        copyItem,
        ?queueItem,
        tile(
          icon: LucideIcons.trash2,
          label: s.deleteTask,
          danger: true,
          onTap: () {
            Navigator.of(ctx).pop();
            confirmMobileDeleteTask(
              context,
              controller,
              task,
              deleteFiles: false,
            );
          },
        ),
        tile(
          icon: LucideIcons.trash2,
          label: s.deleteTaskAndFile,
          danger: true,
          onTap: () {
            Navigator.of(ctx).pop();
            confirmMobileDeleteTask(
              context,
              controller,
              task,
              deleteFiles: true,
            );
          },
        ),
      ];

      return MobileSheetContainer(
        title: task.fileName,
        child: LayoutBuilder(
          builder: (ctx3, constraints) {
            // 按可用宽度动态分列：tile 最小 104px，2~4 列
            const gap = 10.0;
            final width = constraints.maxWidth;
            final cols = (width / 114).floor().clamp(2, 4);
            final tileWidth = (width - gap * (cols - 1)) / cols;
            return Padding(
              padding: const EdgeInsets.symmetric(vertical: 4),
              child: Wrap(
                spacing: gap,
                runSpacing: gap,
                children: [
                  for (final t in tiles) SizedBox(width: tileWidth, child: t),
                ],
              ),
            );
          },
        ),
      );
    },
  );
}

/// 暂停 ⇄ 继续 / 重试。列表卡片按钮和动作面板共用。
void _toggleTask(DownloadController controller, DownloadTask task) {
  switch (task.status) {
    case TaskStatus.downloading:
    case TaskStatus.preparing:
    case TaskStatus.resuming:
      controller.pauseTask(task.id);
    case TaskStatus.paused:
    case TaskStatus.pending:
    case TaskStatus.error:
      controller.resumeTask(task.id);
    case TaskStatus.completed:
      break;
  }
}

/// 供列表卡片直接调用的暂停/继续切换
void toggleMobileTask(DownloadController controller, DownloadTask task) =>
    _toggleTask(controller, task);

Future<void> _showMoveToQueueSheet(
  BuildContext context,
  DownloadController controller,
  DownloadTask task,
) {
  return showMobileSheet<void>(
    context,
    builder: (ctx) {
      final s = LocaleScope.of(ctx);
      final c = AppColors.of(ctx);
      final m = AppMetrics.of(ctx);

      Widget queueItem(String id, String name) {
        final selected = task.queueId == id;
        return GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () {
            Navigator.of(ctx).pop();
            if (!selected) {
              controller.moveTaskToQueue(task.id, id);
              showMobileToast(context, s.mobileMovedToQueue);
            }
          },
          child: Container(
            height: 48,
            padding: const EdgeInsets.symmetric(horizontal: 14),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 14.5,
                      fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                      color: c.textPrimary,
                    ),
                  ),
                ),
                if (selected)
                  Icon(LucideIcons.check, size: 17, color: c.accent),
              ],
            ),
          ),
        );
      }

      final rows = <Widget>[
        queueItem('', s.defaultQueue),
        for (final q in controller.queues) queueItem(q.queueId, q.name),
      ];

      return MobileSheetContainer(
        title: s.mobileSelectQueue,
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Container(
            decoration: BoxDecoration(
              color: m.glass(c.surface1),
              borderRadius: m.brMobileCard,
              border: Border.all(color: c.border),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                for (var i = 0; i < rows.length; i++) ...[
                  if (i > 0)
                    Padding(
                      padding: const EdgeInsets.only(left: 14),
                      child: Container(height: 1, color: c.border),
                    ),
                  rows[i],
                ],
              ],
            ),
          ),
        ),
      );
    },
  );
}

/// 删除确认对话框（任务 / 任务+文件）
Future<void> confirmMobileDeleteTask(
  BuildContext context,
  DownloadController controller,
  DownloadTask task, {
  required bool deleteFiles,
}) async {
  final s = LocaleScope.of(context);
  final confirmed = await showMobileConfirm(
    context,
    title: s.deleteConfirmTitle(deleteFiles),
    message: s.deleteConfirmDesc(task.fileName, deleteFiles),
    confirmLabel: s.confirm,
    cancelLabel: s.cancel,
    confirmIcon: LucideIcons.trash2,
    destructive: true,
  );
  if (confirmed != true) return;
  controller.deleteTask(task.id, deleteFiles: deleteFiles);
  if (context.mounted) {
    showMobileToast(
      context,
      deleteFiles ? s.mobileTaskFileDeleted : s.mobileTaskDeleted,
    );
  }
}
