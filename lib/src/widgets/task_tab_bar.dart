import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../theme/app_colors.dart';
import 'task_list_item.dart';

class TaskTabBar extends StatelessWidget {
  final DownloadController controller;

  const TaskTabBar({super.key, required this.controller});

  static List<(StatusTab, String)> _tabs(S s) => [
    (StatusTab.all, s.tabAll),
    (StatusTab.downloading, s.tabDownloading),
    (StatusTab.completed, s.tabCompleted),
    (StatusTab.paused, s.tabPaused),
    (StatusTab.error, s.tabError),
  ];

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        final ctrl = controller;

        // 管理模式 → 显示操作栏
        if (ctrl.isManageMode) {
          return _buildManageBar(context, c, ctrl, s);
        }

        // 普通模式 → 显示 Tab 栏
        final selected = ctrl.statusTab;
        final tabs = _tabs(s);
        return Container(
          height: 40,
          padding: const EdgeInsets.symmetric(horizontal: 16),
          decoration: BoxDecoration(
            color: c.surface1,
            border: Border(bottom: BorderSide(color: c.border, width: 1)),
          ),
          child: Row(
            children: [
              for (final (tab, label) in tabs) ...[
                _Tab(
                  label: '$label (${ctrl.filteredCountForStatus(tab)})',
                  isSelected: selected == tab,
                  onTap: () => ctrl.setStatusTab(tab),
                ),
                const SizedBox(width: 6),
              ],
            ],
          ),
        );
      },
    );
  }

  Widget _buildManageBar(
    BuildContext context,
    AppColors c,
    DownloadController ctrl,
    S s,
  ) {
    final checkedCount = ctrl.checkedCount;
    final allChecked = ctrl.isAllFilteredChecked;

    return Container(
      height: 40,
      padding: const EdgeInsets.symmetric(horizontal: 16),
      decoration: BoxDecoration(
        color: c.surface1,
        border: Border(bottom: BorderSide(color: c.border, width: 1)),
      ),
      child: Row(
        children: [
          // 全选/取消全选按钮
          _ManageButton(
            icon: allChecked ? LucideIcons.checkCheck : LucideIcons.squareCheck,
            label: allChecked ? s.deselectAll : s.selectAll,
            color: c.textPrimary,
            onTap: () {
              if (allChecked) {
                ctrl.deselectAll();
              } else {
                ctrl.selectAllFiltered();
              }
            },
          ),
          const SizedBox(width: 4),

          // 已选计数
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
            decoration: BoxDecoration(
              color: checkedCount > 0
                  ? c.accent.withValues(alpha: 0.1)
                  : Colors.transparent,
              borderRadius: BorderRadius.circular(4),
            ),
            child: Text(
              s.selectedCount(checkedCount),
              style: TextStyle(
                fontSize: 12,
                color: checkedCount > 0 ? c.accent : c.textMuted,
                fontWeight: FontWeight.w500,
              ),
            ),
          ),

          const Spacer(),

          // 删除任务按钮
          _ManageButton(
            icon: LucideIcons.trash2,
            label: s.deleteTask,
            color: checkedCount > 0 ? c.textPrimary : c.textMuted,
            onTap: checkedCount > 0
                ? () => showBatchDeleteConfirmDialog(
                    context,
                    count: checkedCount,
                    deleteFiles: false,
                    onConfirm: () =>
                        ctrl.deleteCheckedTasks(deleteFiles: false),
                  )
                : null,
          ),
          const SizedBox(width: 4),

          // 删除任务和文件按钮
          _ManageButton(
            icon: LucideIcons.fileX,
            label: s.deleteTaskAndFile,
            color: checkedCount > 0 ? AppColors.red : c.textMuted,
            onTap: checkedCount > 0
                ? () => showBatchDeleteConfirmDialog(
                    context,
                    count: checkedCount,
                    deleteFiles: true,
                    onConfirm: () => ctrl.deleteCheckedTasks(deleteFiles: true),
                  )
                : null,
          ),
          const SizedBox(width: 8),

          // 退出管理模式
          _ManageButton(
            icon: LucideIcons.x,
            label: s.cancel,
            color: c.textSecondary,
            onTap: () => ctrl.exitManageMode(),
          ),
        ],
      ),
    );
  }
}

// =============================================================================
// 管理栏按钮
// =============================================================================

class _ManageButton extends StatefulWidget {
  final IconData icon;
  final String label;
  final Color color;
  final VoidCallback? onTap;

  const _ManageButton({
    required this.icon,
    required this.label,
    required this.color,
    this.onTap,
  });

  @override
  State<_ManageButton> createState() => _ManageButtonState();
}

class _ManageButtonState extends State<_ManageButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final enabled = widget.onTap != null;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          height: 28,
          padding: const EdgeInsets.symmetric(horizontal: 8),
          decoration: BoxDecoration(
            color: _isHovered && enabled ? c.hoverBg : Colors.transparent,
            borderRadius: BorderRadius.circular(4),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                widget.icon,
                size: 14,
                color: enabled
                    ? widget.color
                    : widget.color.withValues(alpha: 0.4),
              ),
              const SizedBox(width: 4),
              Text(
                widget.label,
                style: TextStyle(
                  fontSize: 12,
                  color: enabled
                      ? widget.color
                      : widget.color.withValues(alpha: 0.4),
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
// 普通 Tab
// =============================================================================

class _Tab extends StatefulWidget {
  final String label;
  final bool isSelected;
  final VoidCallback onTap;

  const _Tab({
    required this.label,
    required this.isSelected,
    required this.onTap,
  });

  @override
  State<_Tab> createState() => _TabState();
}

class _TabState extends State<_Tab> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final selected = widget.isSelected;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10),
          decoration: BoxDecoration(
            border: Border(
              bottom: BorderSide(
                color: selected ? c.accent : Colors.transparent,
                width: 2,
              ),
            ),
          ),
          child: Center(
            child: Text(
              widget.label,
              style: TextStyle(
                fontSize: 13,
                color: selected
                    ? c.textPrimary
                    : _isHovered
                    ? c.textSecondary
                    : c.textMuted,
                fontWeight: selected ? FontWeight.w500 : FontWeight.normal,
              ),
            ),
          ),
        ),
      ),
    );
  }
}
