import 'dart:io';

import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:window_manager/window_manager.dart';
import '../models/custom_category.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';

import '../services/app_icon_service.dart';
import '../services/update_service.dart';
import '../i18n/locale_provider.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'category_edit_dialog.dart';
import 'context_menu.dart';

class Sidebar extends StatefulWidget {
  final DownloadController controller;
  final SettingsProvider settingsProvider;

  const Sidebar({
    super.key,
    required this.controller,
    required this.settingsProvider,
  });

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  // ─────────────────────────────────────────────
  // 图标映射
  // ─────────────────────────────────────────────

  static IconData _statusIcon(StatusTab tab) => switch (tab) {
    StatusTab.all => LucideIcons.layoutGrid,
    StatusTab.downloading => LucideIcons.download,
    StatusTab.completed => LucideIcons.circleCheck,
    StatusTab.paused => LucideIcons.circlePause,
    StatusTab.error => LucideIcons.circleAlert,
  };

  static String _statusLabel(S s, StatusTab tab) => switch (tab) {
    StatusTab.all => s.tabAll,
    StatusTab.downloading => s.tabDownloading,
    StatusTab.completed => s.tabCompleted,
    StatusTab.paused => s.tabPaused,
    StatusTab.error => s.tabError,
  };

  // ─────────────────────────────────────────────
  // Build
  // ─────────────────────────────────────────────

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Container(
      color: c.surface1,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _buildLogo(c),
          const SizedBox(height: 10),
          // Only the data-driven sections rebuild on controller changes.
          Expanded(
            child: ListenableBuilder(
              listenable: Listenable.merge([
                widget.controller,
                widget.settingsProvider,
              ]),
              builder: (context, _) {
                final ctrl = widget.controller;
                final sp = widget.settingsProvider;
                final s = LocaleScope.of(context);
                return SingleChildScrollView(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      if (sp.showSidebarStatus) ...[
                        _buildStatusSection(ctrl, s, c),
                        const SizedBox(height: 6),
                      ],
                      if (sp.showSidebarQueues) ...[
                        _buildQueuesSection(ctrl, s, c),
                        const SizedBox(height: 6),
                      ],
                      if (sp.showSidebarCategory)
                        _buildCategorySection(ctrl, s, c),
                    ],
                  ),
                );
              },
            ),
          ),
          const _UpdateFooter(),
        ],
      ),
    );
  }

  // ─────────────────────────────────────────────
  // Logo
  // ─────────────────────────────────────────────

  Widget _buildLogo(AppColors c) {
    // macOS: traffic light 按钮已在左上角，logo/名称隐藏，只保留拖拽区占位
    if (Platform.isMacOS) {
      return DragToMoveArea(child: Container(height: 40, color: c.surface1));
    }
    return DragToMoveArea(
      child: Container(
        height: 40,
        padding: const EdgeInsets.symmetric(horizontal: 16),
        child: Row(
          children: [
            // 跟随「设置-外观-应用图标」切换：内置闪电/自定义图标启用时显示其预览
            ListenableBuilder(
              listenable: AppIconService.instance,
              builder: (context, _) {
                final svc = AppIconService.instance;
                final m = AppMetrics.of(context);
                if (svc.isBolt) {
                  return ClipRRect(
                    borderRadius: m.brMd,
                    child: Image.asset(
                      AppIconService.builtinBoltAsset,
                      width: 22,
                      height: 22,
                      filterQuality: FilterQuality.medium,
                    ),
                  );
                }
                final customPreview = svc.isCustom ? svc.previewPngPath : null;
                if (customPreview != null) {
                  return ClipRRect(
                    borderRadius: m.brMd,
                    child: Image(
                      key: ValueKey(svc.previewRevision),
                      image: FileImage(File(customPreview)),
                      width: 22,
                      height: 22,
                      filterQuality: FilterQuality.medium,
                      gaplessPlayback: true,
                    ),
                  );
                }
                // 暗色主题：蓝色箭头 + 透明背景（无白色圆角矩形，避免在深色侧边栏上显得突兀）
                // 亮色主题：完整圆角图标（白底 + 蓝色箭头）
                if (c.tokens.appearance == Brightness.dark) {
                  return Image.asset(
                    'assets/logo/logo_on_dark.png',
                    width: 22,
                    height: 22,
                    filterQuality: FilterQuality.medium,
                  );
                }
                return ClipRRect(
                  borderRadius: m.brMd,
                  child: Image.asset(
                    'assets/logo/fluxdown_logo.png',
                    width: 22,
                    height: 22,
                    filterQuality: FilterQuality.medium,
                  ),
                );
              },
            ),
            const SizedBox(width: 9),
            Text.rich(
              TextSpan(
                children: [
                  TextSpan(
                    text: 'Flux',
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                      color: c.accent,
                      letterSpacing: 0.3,
                    ),
                  ),
                  TextSpan(
                    text: 'Down',
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w500,
                      color: c.textPrimary,
                      letterSpacing: 0.3,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  // ─────────────────────────────────────────────
  // 状态区块（主导航）
  // ─────────────────────────────────────────────

  Widget _buildStatusSection(DownloadController ctrl, S s, AppColors c) {
    final selectedStatus = ctrl.statusTab;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        GestureDetector(
          onSecondaryTapUp: (d) => _showSectionContextMenu(
            context,
            d.globalPosition,
            s,
            onHide: () => widget.settingsProvider.setShowSidebarStatus(false),
          ),
          child: _SectionHeader(title: s.sidebarStatus, c: c),
        ),
        const SizedBox(height: 4),
        for (final tab in StatusTab.values)
          _NavItem(
            icon: _statusIcon(tab),
            label: _statusLabel(s, tab),
            count: ctrl.countForStatus(tab),
            isSelected: selectedStatus == tab,
            showActivityDot:
                tab == StatusTab.downloading && ctrl.downloadingCount > 0,
            onTap: () => ctrl.setStatusTab(tab),
          ),
      ],
    );
  }

  // ─────────────────────────────────────────────
  // 队列区块（可折叠，含新建按钮）
  // ─────────────────────────────────────────────

  Widget _buildQueuesSection(DownloadController ctrl, S s, AppColors c) {
    final queues = ctrl.queues;
    final queueFilter = ctrl.queueFilter;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        GestureDetector(
          onSecondaryTapUp: (d) => _showSectionContextMenu(
            context,
            d.globalPosition,
            s,
            onHide: () => widget.settingsProvider.setShowSidebarQueues(false),
          ),
          child: _CollapsibleSectionHeader(
            title: s.sidebarQueues,
            expanded: widget.settingsProvider.sidebarQueuesExpanded,
            c: c,
            onToggle: () => widget.settingsProvider.setSidebarQueuesExpanded(
              !widget.settingsProvider.sidebarQueuesExpanded,
            ),
            trailing: _QueueAddButton(
              c: c,
              onTap: () => _showCreateQueueDialog(context, ctrl, s, c),
            ),
          ),
        ),
        if (widget.settingsProvider.sidebarQueuesExpanded) ...[
          const SizedBox(height: 4),
          // 默认队列
          _NavItem(
            icon: LucideIcons.inbox,
            label: s.defaultQueue,
            count: ctrl.countForQueue(''),
            isSelected: queueFilter == '',
            onTap: () => ctrl.setQueueFilter(''),
          ),
          // 命名队列
          for (final queue in queues)
            _QueueNavItem(
              queue: queue,
              count: ctrl.countForQueue(queue.queueId),
              isSelected: queueFilter == queue.queueId,
              c: c,
              onTap: () => ctrl.setQueueFilter(queue.queueId),
              onEdit: () => _showEditQueueDialog(context, ctrl, s, c, queue),
              onDelete: () =>
                  _showDeleteQueueDialog(context, ctrl, s, c, queue),
            ),
        ],
      ],
    );
  }

  // 新建队列对话框
  void _showCreateQueueDialog(
    BuildContext context,
    DownloadController ctrl,
    S s,
    AppColors c,
  ) {
    final nameCtrl = TextEditingController();
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => _QueueDialog(
        title: s.createQueueAction,
        nameCtrl: nameCtrl,
        s: s,
        c: c,
        onConfirm:
            (
              name,
              speedLimit,
              maxConcurrent,
              saveDir,
              defaultSegments,
              defaultUserAgent,
            ) {
              ctrl.createQueue(
                name: name,
                speedLimitKbps: speedLimit,
                maxConcurrent: maxConcurrent,
                defaultSaveDir: saveDir,
                defaultSegments: defaultSegments,
                defaultUserAgent: defaultUserAgent,
              );
            },
      ),
    ).then((_) => nameCtrl.dispose());
  }

  // 编辑队列对话框
  void _showEditQueueDialog(
    BuildContext context,
    DownloadController ctrl,
    S s,
    AppColors c,
    DownloadQueue queue,
  ) {
    final nameCtrl = TextEditingController(text: queue.name);
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => _QueueDialog(
        title: s.editQueue,
        nameCtrl: nameCtrl,
        s: s,
        c: c,
        initialSpeedLimit: queue.speedLimitKbps,
        initialMaxConcurrent: queue.maxConcurrent,
        initialSaveDir: queue.defaultSaveDir,
        initialDefaultSegments: queue.defaultSegments,
        initialUserAgent: queue.defaultUserAgent,
        onConfirm:
            (
              name,
              speedLimit,
              maxConcurrent,
              saveDir,
              defaultSegments,
              defaultUserAgent,
            ) {
              ctrl.updateQueue(
                queueId: queue.queueId,
                name: name,
                speedLimitKbps: speedLimit,
                maxConcurrent: maxConcurrent,
                defaultSaveDir: saveDir,
                defaultSegments: defaultSegments,
                defaultUserAgent: defaultUserAgent,
              );
            },
      ),
    ).then((_) => nameCtrl.dispose());
  }

  // 删除队列确认对话框
  void _showDeleteQueueDialog(
    BuildContext context,
    DownloadController ctrl,
    S s,
    AppColors c,
    DownloadQueue queue,
  ) {
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.deleteQueueAction),
        description: Text(s.queueDeleteConfirmDesc(queue.name)),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
          ShadButton.destructive(
            onPressed: () {
              Navigator.of(ctx).pop();
              ctrl.deleteQueue(queue.queueId);
            },
            child: Text(s.deleteQueueAction),
          ),
        ],
      ),
    );
  }

  // ─────────────────────────────────────────────
  // 分类区块（可折叠）
  // ─────────────────────────────────────────────

  /// 内置分类的 i18n 名称映射
  static String _builtinCategoryLabel(S s, String? builtinType) =>
      switch (builtinType) {
        'all' => s.categoryAll,
        'video' => s.categoryVideo,
        'audio' => s.categoryAudio,
        'document' => s.categoryDocument,
        'image' => s.categoryImage,
        'archive' => s.categoryArchive,
        'other' => s.categoryOther,
        _ => '',
      };

  Widget _buildCategorySection(DownloadController ctrl, S s, AppColors c) {
    final customFilter = ctrl.customCategoryFilter;
    final visibleCategories = widget.settingsProvider.visibleCategories;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        GestureDetector(
          onSecondaryTapUp: (d) => _showSectionContextMenu(
            context,
            d.globalPosition,
            s,
            onHide: () => widget.settingsProvider.setShowSidebarCategory(false),
          ),
          child: _CollapsibleSectionHeader(
            title: s.sidebarCategory,
            expanded: widget.settingsProvider.sidebarCategoryExpanded,
            c: c,
            onToggle: () => widget.settingsProvider.setSidebarCategoryExpanded(
              !widget.settingsProvider.sidebarCategoryExpanded,
            ),
          ),
        ),
        if (widget.settingsProvider.sidebarCategoryExpanded) ...[
          const SizedBox(height: 4),
          for (final cat in visibleCategories)
            GestureDetector(
              onSecondaryTapUp: (d) => _showCategoryItemContextMenu(
                context,
                d.globalPosition,
                s,
                c,
                cat,
              ),
              child: _NavItem(
                icon: categoryIconData(cat.icon),
                label: cat.isBuiltin
                    ? _builtinCategoryLabel(s, cat.builtinType)
                    : cat.name,
                count: ctrl.countForUnifiedCategory(cat, visibleCategories),
                isSelected: customFilter?.id == cat.id,
                onTap: () => ctrl.setCustomCategoryFilter(
                  cat,
                  allVisible: visibleCategories,
                ),
              ),
            ),
        ],
      ],
    );
  }

  void _showSectionContextMenu(
    BuildContext context,
    Offset position,
    S s, {
    required VoidCallback onHide,
  }) {
    final c = AppColors.of(context);
    showContextMenu(
      context,
      position,
      items: [
        ContextMenuItem(
          icon: LucideIcons.eyeOff,
          label: s.hideSection,
          color: c.textSecondary,
          action: onHide,
        ),
      ],
    );
  }

  void _showCategoryItemContextMenu(
    BuildContext context,
    Offset position,
    S s,
    AppColors c,
    CustomCategory cat,
  ) {
    // 只有 "全部文件" 才完全锁定（无法编辑/重置）；"其他" 与普通内置分类一样可编辑
    final isSpecial = cat.builtinType == 'all';

    showContextMenu(
      context,
      position,
      items: [
        // 编辑（非 all/other 可编辑）
        if (!isSpecial)
          ContextMenuItem(
            icon: LucideIcons.pencil,
            label: s.editCategory,
            color: c.textSecondary,
            action: () => showCategoryEditDialog(
              context,
              existing: cat,
              onSave: (updated) =>
                  widget.settingsProvider.updateCustomCategory(updated),
              onDelete: cat.builtinType == 'all'
                  ? null
                  : () => widget.settingsProvider.removeCustomCategory(cat.id),
            ),
          ),
        // 隐藏
        ContextMenuItem(
          icon: LucideIcons.eyeOff,
          label: s.hideSection,
          color: c.textSecondary,
          action: () => widget.settingsProvider.updateCustomCategory(
            cat.copyWith(visible: false),
          ),
        ),
        // 内置分类(非all): 重置选项
        if (cat.isBuiltin && !isSpecial)
          ContextMenuItem(
            icon: LucideIcons.rotateCcw,
            label: s.resetBuiltinCategories,
            color: c.textMuted,
            action: () =>
                widget.settingsProvider.resetBuiltinCategory(cat.builtinType!),
          ),
        // 非"全部文件"的所有分类（含内置视频/音频等）均可删除
        if (cat.builtinType != 'all')
          ContextMenuItem(
            icon: LucideIcons.trash2,
            label: s.deleteCategory,
            color: AppColors.red,
            action: () => widget.settingsProvider.removeCustomCategory(cat.id),
          ),
      ],
      dividerAfterIndices: {isSpecial ? 0 : 1},
    );
  }
}

// =============================================================================
// Section Headers
// =============================================================================

class _SectionHeader extends StatelessWidget {
  final String title;
  final AppColors c;

  const _SectionHeader({required this.title, required this.c});

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
      child: Text(
        title,
        style: TextStyle(
          fontSize: 10.5,
          fontWeight: FontWeight.w500,
          color: c.textMuted,
          letterSpacing: 0.5,
        ),
      ),
    );
  }
}

class _CollapsibleSectionHeader extends StatefulWidget {
  final String title;
  final bool expanded;
  final AppColors c;
  final VoidCallback onToggle;
  final Widget? trailing;

  const _CollapsibleSectionHeader({
    required this.title,
    required this.expanded,
    required this.c,
    required this.onToggle,
    this.trailing,
  });

  @override
  State<_CollapsibleSectionHeader> createState() =>
      _CollapsibleSectionHeaderState();
}

class _CollapsibleSectionHeaderState extends State<_CollapsibleSectionHeader> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: GestureDetector(
        onTap: widget.onToggle,
        child: Container(
          color: Colors.transparent,
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 5),
          child: Row(
            children: [
              Text(
                widget.title,
                style: TextStyle(
                  fontSize: 10.5,
                  fontWeight: FontWeight.w500,
                  color: _isHovered ? c.textSecondary : c.textMuted,
                  letterSpacing: 0.5,
                ),
              ),
              const Spacer(),
              Icon(
                widget.expanded
                    ? LucideIcons.chevronDown
                    : LucideIcons.chevronRight,
                size: 11,
                color: _isHovered ? c.textSecondary : c.textMuted,
              ),
              if (widget.trailing != null) ...[
                const SizedBox(width: 4),
                widget.trailing!,
              ],
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// Nav Item
// =============================================================================

class _NavItem extends StatefulWidget {
  final IconData icon;
  final String label;
  final int? count;
  final bool isSelected;
  final bool showActivityDot;
  final VoidCallback onTap;

  const _NavItem({
    required this.icon,
    required this.label,
    this.count,
    required this.isSelected,
    this.showActivityDot = false,
    required this.onTap,
  });

  @override
  State<_NavItem> createState() => _NavItemState();
}

class _NavItemState extends State<_NavItem> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final selected = widget.isSelected;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          height: 32,
          margin: const EdgeInsets.symmetric(horizontal: 8, vertical: 1),
          padding: const EdgeInsets.symmetric(horizontal: 8),
          decoration: BoxDecoration(
            color: selected
                ? c.accentBg
                : _isHovered
                ? c.hoverBg
                : Colors.transparent,
            borderRadius: m.brMd,
          ),
          child: Row(
            children: [
              // 活跃下载点 or 图标
              Stack(
                clipBehavior: Clip.none,
                children: [
                  Icon(
                    widget.icon,
                    size: 14,
                    color: selected ? c.accent : c.textSecondary,
                  ),
                  if (widget.showActivityDot)
                    Positioned(
                      top: -2,
                      right: -3,
                      child: Container(
                        width: 6,
                        height: 6,
                        decoration: BoxDecoration(
                          color: AppColors.green,
                          shape: BoxShape.circle,
                          border: Border.all(color: c.surface1, width: 1),
                        ),
                      ),
                    ),
                ],
              ),
              const SizedBox(width: 8),
              Text(
                widget.label,
                style: TextStyle(
                  fontSize: 12.5,
                  color: selected ? c.accent : c.textSecondary,
                  fontWeight: selected ? FontWeight.w500 : FontWeight.normal,
                ),
              ),
              if (widget.count != null) ...[
                const Spacer(),
                Text(
                  widget.count.toString(),
                  style: TextStyle(
                    fontSize: 11,
                    color: selected ? c.accent : c.textMuted,
                    fontFeatures: const [FontFeature.tabularFigures()],
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// Queue section helpers
// =============================================================================

/// "+" 按钮：新建队列
class _QueueAddButton extends StatefulWidget {
  final AppColors c;
  final VoidCallback onTap;

  const _QueueAddButton({required this.c, required this.onTap});

  @override
  State<_QueueAddButton> createState() => _QueueAddButtonState();
}

class _QueueAddButtonState extends State<_QueueAddButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    final m = AppMetrics.of(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          width: 16,
          height: 16,
          decoration: BoxDecoration(
            color: _isHovered ? c.hoverBg : Colors.transparent,
            borderRadius: m.brSm,
          ),
          child: Icon(LucideIcons.plus, size: 11, color: c.textMuted),
        ),
      ),
    );
  }
}

/// 队列导航项（带右键或悬浮菜单的编辑/删除）
class _QueueNavItem extends StatefulWidget {
  final DownloadQueue queue;
  final int count;
  final bool isSelected;
  final AppColors c;
  final VoidCallback onTap;
  final VoidCallback onEdit;
  final VoidCallback onDelete;

  const _QueueNavItem({
    required this.queue,
    required this.count,
    required this.isSelected,
    required this.c,
    required this.onTap,
    required this.onEdit,
    required this.onDelete,
  });

  @override
  State<_QueueNavItem> createState() => _QueueNavItemState();
}

class _QueueNavItemState extends State<_QueueNavItem> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    final m = AppMetrics.of(context);
    final selected = widget.isSelected;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        onSecondaryTapUp: (d) => _showContextMenu(context, d.globalPosition),
        child: Container(
          height: 32,
          margin: const EdgeInsets.symmetric(horizontal: 8, vertical: 1),
          padding: const EdgeInsets.symmetric(horizontal: 8),
          decoration: BoxDecoration(
            color: selected
                ? c.accentBg
                : _isHovered
                ? c.hoverBg
                : Colors.transparent,
            borderRadius: m.brMd,
          ),
          child: Row(
            children: [
              Icon(
                LucideIcons.layers,
                size: 14,
                color: selected ? c.accent : c.textSecondary,
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  widget.queue.name,
                  style: TextStyle(
                    fontSize: 12.5,
                    color: selected ? c.accent : c.textSecondary,
                    fontWeight: selected ? FontWeight.w500 : FontWeight.normal,
                  ),
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              if (_isHovered && !selected) ...[
                _QueueActionIcon(
                  icon: LucideIcons.pencil,
                  c: c,
                  onTap: widget.onEdit,
                ),
                const SizedBox(width: 2),
                _QueueActionIcon(
                  icon: LucideIcons.trash2,
                  c: c,
                  onTap: widget.onDelete,
                  isDestructive: true,
                ),
              ] else ...[
                Text(
                  widget.count.toString(),
                  style: TextStyle(
                    fontSize: 11,
                    color: selected ? c.accent : c.textMuted,
                    fontFeatures: const [FontFeature.tabularFigures()],
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }

  void _showContextMenu(BuildContext context, Offset position) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    showContextMenu(
      context,
      position,
      items: [
        ContextMenuItem(
          icon: LucideIcons.pencil,
          label: s.editQueue,
          color: c.textSecondary,
          action: widget.onEdit,
        ),
        ContextMenuItem(
          icon: LucideIcons.trash2,
          label: s.deleteQueueAction,
          color: AppColors.red,
          action: widget.onDelete,
        ),
      ],
    );
  }
}

class _QueueActionIcon extends StatefulWidget {
  final IconData icon;
  final AppColors c;
  final VoidCallback onTap;
  final bool isDestructive;

  const _QueueActionIcon({
    required this.icon,
    required this.c,
    required this.onTap,
    this.isDestructive = false,
  });

  @override
  State<_QueueActionIcon> createState() => _QueueActionIconState();
}

class _QueueActionIconState extends State<_QueueActionIcon> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final color = widget.isDestructive ? AppColors.red : widget.c.textSecondary;
    final m = AppMetrics.of(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          width: 18,
          height: 18,
          decoration: BoxDecoration(
            color: _isHovered
                ? m.soft(color)
                : Colors.transparent,
            borderRadius: m.brSm,
          ),
          child: Icon(widget.icon, size: 11, color: color),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 队列对话框 UA 预设（与设置页保持同步）
// ─────────────────────────────────────────────

/// key '' = 继承全局；其余 key 对应具体 UA 字符串
const _kQueueUaPresets = {
  '': '', // 继承全局设置
  'chrome':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
      '(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36',
  'firefox':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) '
      'Gecko/20100101 Firefox/147.0',
  'edge':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
      '(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.3800.70',
  'safari':
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) '
      'AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15',
  'netdisk': 'netdisk',
};

/// 根据 UA 字符串反推预设 key
String _detectQueueUaPreset(String ua) {
  for (final entry in _kQueueUaPresets.entries) {
    if (entry.value == ua) return entry.key;
  }
  return ua.isEmpty ? '' : 'custom';
}

/// 新建/编辑队列对话框
class _QueueDialog extends StatefulWidget {
  final String title;
  final TextEditingController nameCtrl;
  final S s;
  final AppColors c;
  final int initialSpeedLimit;
  final int initialMaxConcurrent;
  final String initialSaveDir;
  final int initialDefaultSegments;
  final String initialUserAgent;
  final void Function(
    String name,
    int speedLimit,
    int maxConcurrent,
    String saveDir,
    int defaultSegments,
    String defaultUserAgent,
  )
  onConfirm;

  const _QueueDialog({
    required this.title,
    required this.nameCtrl,
    required this.s,
    required this.c,
    this.initialSpeedLimit = 0,
    this.initialMaxConcurrent = 0,
    this.initialSaveDir = '',
    this.initialDefaultSegments = 0,
    this.initialUserAgent = '',
    required this.onConfirm,
  });

  @override
  State<_QueueDialog> createState() => _QueueDialogState();
}

class _QueueDialogState extends State<_QueueDialog> {
  late final TextEditingController _speedCtrl;
  late final TextEditingController _concurrentCtrl;
  late final TextEditingController _saveDirCtrl;
  late final TextEditingController _uaCtrl;
  late String _selectedSegments;
  late String _selectedUaPreset;

  static const _segmentOptions = ['0', '4', '8', '16', '32', '64'];

  @override
  void initState() {
    super.initState();
    _speedCtrl = TextEditingController(
      text: widget.initialSpeedLimit > 0
          ? widget.initialSpeedLimit.toString()
          : '',
    );
    _concurrentCtrl = TextEditingController(
      text: widget.initialMaxConcurrent > 0
          ? widget.initialMaxConcurrent.toString()
          : '',
    );
    _saveDirCtrl = TextEditingController(text: widget.initialSaveDir);
    _uaCtrl = TextEditingController(text: widget.initialUserAgent);
    _selectedSegments = widget.initialDefaultSegments > 0
        ? widget.initialDefaultSegments.toString()
        : '0';
    _selectedUaPreset = _detectQueueUaPreset(widget.initialUserAgent);
  }

  @override
  void dispose() {
    _speedCtrl.dispose();
    _concurrentCtrl.dispose();
    _saveDirCtrl.dispose();
    _uaCtrl.dispose();
    super.dispose();
  }

  void _onUaPresetChanged(String? preset) {
    if (preset == null) return;
    setState(() => _selectedUaPreset = preset);
    if (preset != 'custom') {
      _uaCtrl.text = _kQueueUaPresets[preset] ?? '';
    }
  }

  void _onUaTextChanged(String value) {
    final detected = _detectQueueUaPreset(value);
    if (detected != _selectedUaPreset) {
      setState(() => _selectedUaPreset = detected);
    }
  }

  void _confirm() {
    final name = widget.nameCtrl.text.trim();
    if (name.isEmpty) return;
    // 钳制到合法范围：速度 0-1073741824 KB/s，并发 0-100（0 = 使用全局设置）
    final speedLimit = (int.tryParse(_speedCtrl.text.trim()) ?? 0).clamp(
      0,
      1 << 30,
    );
    final maxConcurrent = (int.tryParse(_concurrentCtrl.text.trim()) ?? 0)
        .clamp(0, 100);
    final saveDir = _saveDirCtrl.text.trim();
    final defaultSegments = int.tryParse(_selectedSegments) ?? 0;
    final defaultUserAgent = _uaCtrl.text.trim();
    Navigator.of(context).pop();
    widget.onConfirm(
      name,
      speedLimit,
      maxConcurrent,
      saveDir,
      defaultSegments,
      defaultUserAgent,
    );
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.s;
    final c = widget.c;
    return ShadDialog(
      title: Text(widget.title),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(s.cancel),
        ),
        ShadButton(onPressed: _confirm, child: Text(s.confirm)),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 12),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              s.queueNameLabel,
              style: TextStyle(
                fontSize: 11.5,
                fontWeight: FontWeight.w500,
                color: c.textSecondary,
              ),
            ),
            const SizedBox(height: 6),
            ShadInput(
              controller: widget.nameCtrl,
              placeholder: Text(s.queueNameHint),
              autofocus: true,
              onSubmitted: (_) => _confirm(),
            ),
            const SizedBox(height: 12),
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        s.queueSpeedLimit,
                        style: TextStyle(
                          fontSize: 11.5,
                          fontWeight: FontWeight.w500,
                          color: c.textSecondary,
                        ),
                      ),
                      const SizedBox(height: 6),
                      ShadInput(
                        controller: _speedCtrl,
                        placeholder: Text(s.queueSpeedLimitHint),
                        keyboardType: TextInputType.number,
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        s.queueMaxConcurrent,
                        style: TextStyle(
                          fontSize: 11.5,
                          fontWeight: FontWeight.w500,
                          color: c.textSecondary,
                        ),
                      ),
                      const SizedBox(height: 6),
                      ShadInput(
                        controller: _concurrentCtrl,
                        placeholder: Text(s.queueMaxConcurrentHint),
                        keyboardType: TextInputType.number,
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        s.queueDefaultSegments,
                        style: TextStyle(
                          fontSize: 11.5,
                          fontWeight: FontWeight.w500,
                          color: c.textSecondary,
                        ),
                      ),
                      const SizedBox(height: 6),
                      SizedBox(
                        width: double.infinity,
                        child: ShadSelect<String>(
                          initialValue: _selectedSegments,
                          onChanged: (v) {
                            if (v != null) {
                              setState(() => _selectedSegments = v);
                            }
                          },
                          options: _segmentOptions
                              .map(
                                (opt) => ShadOption(
                                  value: opt,
                                  child: Text(
                                    opt == '0'
                                        ? s.queueDefaultSegmentsHint
                                        : opt,
                                  ),
                                ),
                              )
                              .toList(),
                          selectedOptionBuilder: (ctx, v) =>
                              Text(v == '0' ? s.queueDefaultSegmentsHint : v),
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            Text(
              s.queueDefaultUserAgent,
              style: TextStyle(
                fontSize: 11.5,
                fontWeight: FontWeight.w500,
                color: c.textSecondary,
              ),
            ),
            const SizedBox(height: 6),
            Row(
              children: [
                SizedBox(
                  width: 130,
                  child: ShadSelect<String>(
                    initialValue: _selectedUaPreset,
                    options: [
                      ShadOption(
                        value: '',
                        child: Text(s.queueUaInheritGlobal),
                      ),
                      ShadOption(
                        value: 'chrome',
                        child: Text(s.userAgentPresetChrome),
                      ),
                      ShadOption(
                        value: 'firefox',
                        child: Text(s.userAgentPresetFirefox),
                      ),
                      ShadOption(
                        value: 'edge',
                        child: Text(s.userAgentPresetEdge),
                      ),
                      ShadOption(
                        value: 'safari',
                        child: Text(s.userAgentPresetSafari),
                      ),
                      ShadOption(
                        value: 'netdisk',
                        child: Text(s.userAgentPresetNetdisk),
                      ),
                      ShadOption(
                        value: 'custom',
                        child: Text(s.userAgentPresetCustom),
                      ),
                    ],
                    selectedOptionBuilder: (ctx, v) {
                      final label = switch (v) {
                        'chrome' => 'Chrome',
                        'firefox' => 'Firefox',
                        'edge' => 'Edge',
                        'safari' => 'Safari',
                        'netdisk' => 'netdisk',
                        'custom' => s.userAgentPresetCustom,
                        _ => s.queueUaInheritGlobal,
                      };
                      return Text(
                        label,
                        overflow: TextOverflow.ellipsis,
                        maxLines: 1,
                      );
                    },
                    onChanged: _onUaPresetChanged,
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ShadInput(
                    controller: _uaCtrl,
                    placeholder: Text(s.queueUaHint),
                    onChanged: _onUaTextChanged,
                  ),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

// =============================================================================
// Sidebar footer: version display + update UI
// =============================================================================

class _UpdateFooter extends StatelessWidget {
  const _UpdateFooter();

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: UpdateService.instance,
      builder: (context, _) {
        final svc = UpdateService.instance;
        final c = AppColors.of(context);
        final status = svc.status;

        return Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (status == UpdateStatus.downloading) _buildProgressBar(svc, c),
            Container(
              height: 28,
              padding: const EdgeInsets.symmetric(horizontal: 12),
              decoration: BoxDecoration(
                border: Border(top: BorderSide(color: c.border, width: 1)),
              ),
              child: Row(
                children: [
                  Text(
                    _versionText(svc),
                    style: TextStyle(fontSize: 10.5, color: c.textMuted),
                  ),
                  const Spacer(),
                  _buildAction(context, svc, c, status),
                ],
              ),
            ),
          ],
        );
      },
    );
  }

  String _versionText(UpdateService svc) {
    final v = svc.currentVersion;
    final label = v == 'dev' ? 'dev' : 'v$v';
    if (svc.status == UpdateStatus.available ||
        svc.status == UpdateStatus.downloading ||
        svc.status == UpdateStatus.readyToInstall) {
      return '$label -> v${svc.checkResult?.latestVersion ?? ''}';
    }
    return label;
  }

  Widget _buildAction(
    BuildContext context,
    UpdateService svc,
    AppColors c,
    UpdateStatus status,
  ) {
    switch (status) {
      case UpdateStatus.available:
        return _UpdateActionButton(
          icon: LucideIcons.download,
          tooltip: LocaleScope.of(
            context,
          ).downloadUpdateVersion(svc.checkResult?.latestVersion ?? ''),
          color: AppColors.red,
          onTap: svc.downloadUpdate,
        );
      case UpdateStatus.downloading:
        final p = svc.progress;
        final pct = (p != null && p.totalBytes > 0)
            ? '${(p.downloadedBytes / p.totalBytes * 100).toStringAsFixed(0)}%'
            : '...';
        return Text(
          pct,
          style: TextStyle(
            fontSize: 10,
            color: c.accent,
            fontWeight: FontWeight.w600,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        );
      case UpdateStatus.readyToInstall:
        return _UpdateActionButton(
          icon: LucideIcons.rotateCcw,
          tooltip: LocaleScope.of(context).installAndRestart,
          color: AppColors.green,
          onTap: svc.installUpdate,
        );
      case UpdateStatus.checking:
        return SizedBox(
          width: 12,
          height: 12,
          child: CircularProgressIndicator(
            strokeWidth: 1.5,
            color: c.textMuted,
          ),
        );
      default:
        return const SizedBox.shrink();
    }
  }

  Widget _buildProgressBar(UpdateService svc, AppColors c) {
    final p = svc.progress;
    final fraction = (p != null && p.totalBytes > 0)
        ? (p.downloadedBytes / p.totalBytes).clamp(0.0, 1.0)
        : 0.0;

    return SizedBox(
      height: 3,
      child: LinearProgressIndicator(
        value: fraction,
        backgroundColor: c.surface2,
        valueColor: AlwaysStoppedAnimation<Color>(c.accent),
        minHeight: 3,
      ),
    );
  }
}

class _UpdateActionButton extends StatefulWidget {
  final IconData icon;
  final String tooltip;
  final Color color;
  final VoidCallback onTap;

  const _UpdateActionButton({
    required this.icon,
    required this.tooltip,
    required this.color,
    required this.onTap,
  });

  @override
  State<_UpdateActionButton> createState() => _UpdateActionButtonState();
}

class _UpdateActionButtonState extends State<_UpdateActionButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return ShadTooltip(
      builder: (_) => Text(widget.tooltip),
      child: MouseRegion(
        onEnter: (_) => setState(() => _isHovered = true),
        onExit: (_) => setState(() => _isHovered = false),
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: widget.onTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 150),
            width: 22,
            height: 22,
            decoration: BoxDecoration(
              color: _isHovered
                  ? m.active(widget.color)
                  : Colors.transparent,
              borderRadius: m.brSm,
            ),
            child: Icon(widget.icon, size: 13, color: widget.color),
          ),
        ),
      ),
    );
  }
}
