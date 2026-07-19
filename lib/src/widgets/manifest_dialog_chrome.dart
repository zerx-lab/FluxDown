// manifest_select_dialog.dart 的静态外壳部分：摘要区（1）/ 工具栏（2）/
// 面包屑条（3）/ 底栏（6）四个纯展示 + 回调转发的 widget（下钻导航主体
// 见 manifest_browse_list.dart，高级选项面板见 manifest_advanced_panel.dart）。
// 拆出本文件只是为控制 manifest_select_dialog.dart 的文件体量——组件不持有
// 任何自身状态，所有交互状态仍由对话框 State 持有并通过回调下发。

import 'package:flutter/widgets.dart';
import 'package:path/path.dart' as p;
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/manifest_breadcrumb.dart';
import '../models/manifest_selection.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'dir_picker_field.dart';
import 'split_action_button.dart';

// =============================================================================
// 1. 摘要区
// =============================================================================

class ManifestSummaryHeader extends StatelessWidget {
  final TextEditingController groupNameController;
  final int itemCount;
  final int totalSize;
  final String sourceUrl;
  final VoidCallback onClose;

  const ManifestSummaryHeader({
    super.key,
    required this.groupNameController,
    required this.itemCount,
    required this.totalSize,
    required this.sourceUrl,
    required this.onClose,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final site = manifestSourceHost(sourceUrl);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Container(
          width: 30,
          height: 30,
          alignment: Alignment.center,
          decoration: BoxDecoration(color: m.soft(c.accent), borderRadius: m.brMd),
          child: Icon(LucideIcons.folder, size: 15, color: c.accent),
        ),
        const SizedBox(width: 10),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              ShadTooltip(
                builder: (_) => Text(s.manifestGroupNameTooltip),
                child: ShadInput(
                  controller: groupNameController,
                  placeholder: Text(s.manifestGroupNamePlaceholder),
                  style: const TextStyle(fontSize: 14, fontWeight: FontWeight.w600),
                ),
              ),
              const SizedBox(height: 4),
              Row(
                children: [
                  Flexible(
                    child: Text(
                      s.manifestSummary(itemCount, DownloadTask.formatBytes(totalSize)),
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(fontSize: 11.5, color: c.textMuted),
                    ),
                  ),
                  if (site.isNotEmpty) ...[
                    Text(' · ', style: TextStyle(fontSize: 11.5, color: c.textMuted)),
                    Icon(LucideIcons.link, size: 10, color: c.textMuted),
                    const SizedBox(width: 3),
                    Flexible(
                      child: Text(
                        site,
                        overflow: TextOverflow.ellipsis,
                        maxLines: 1,
                        style: TextStyle(fontSize: 11.5, color: c.textMuted),
                      ),
                    ),
                  ],
                  const SizedBox(width: 6),
                  Container(
                    padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
                    decoration: BoxDecoration(
                      color: c.surface2,
                      borderRadius: m.brPill,
                    ),
                    child: Text(
                      s.manifestPluginBadge,
                      style: TextStyle(fontSize: 9.5, color: c.textSecondary),
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
        const SizedBox(width: 8),
        GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onClose,
          child: Padding(
            padding: const EdgeInsets.all(4),
            child: Icon(LucideIcons.x, size: 16, color: c.textMuted),
          ),
        ),
      ],
    );
  }
}

// =============================================================================
// 2. 工具栏
// =============================================================================

class ManifestToolbar extends StatelessWidget {
  final TextEditingController searchController;
  final ValueChanged<String> onSearchChanged;
  final List<ManifestExtChip> topExtensions;
  final Set<String> extFilter;
  final ValueChanged<String> onToggleExt;
  final VoidCallback onSelectAll;
  final VoidCallback onInvert;
  final VoidCallback onClear;
  final ManifestSortKey sortKey;
  final VoidCallback onToggleSort;

  const ManifestToolbar({
    super.key,
    required this.searchController,
    required this.onSearchChanged,
    required this.topExtensions,
    required this.extFilter,
    required this.onToggleExt,
    required this.onSelectAll,
    required this.onInvert,
    required this.onClear,
    required this.sortKey,
    required this.onToggleSort,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return Row(
      children: [
        SizedBox(
          width: 190,
          child: ShadInput(
            controller: searchController,
            placeholder: Text(s.manifestSearchPlaceholder),
            leading: Padding(
              padding: const EdgeInsets.only(left: 2),
              child: Icon(LucideIcons.search, size: 13, color: c.textMuted),
            ),
            onChanged: onSearchChanged,
          ),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: Row(
              children: [
                for (final chip in topExtensions)
                  Padding(
                    padding: const EdgeInsets.only(right: 4),
                    child: _ExtChip(
                      chip: chip,
                      selected: extFilter.contains(chip.ext),
                      onTap: () => onToggleExt(chip.ext),
                    ),
                  ),
              ],
            ),
          ),
        ),
        const SizedBox(width: 8),
        _MiniButton(label: s.manifestSelectAll, onTap: onSelectAll),
        const SizedBox(width: 4),
        _MiniButton(label: s.manifestInvertSelection, onTap: onInvert),
        const SizedBox(width: 4),
        _MiniButton(label: s.manifestClearSelection, onTap: onClear),
        const SizedBox(width: 4),
        _MiniButton(
          label: sortKey == ManifestSortKey.size
              ? s.manifestSortBySizeDesc
              : s.manifestSortByName,
          onTap: onToggleSort,
          active: sortKey == ManifestSortKey.size,
        ),
      ],
    );
  }
}

class _ExtChip extends StatelessWidget {
  final ManifestExtChip chip;
  final bool selected;
  final VoidCallback onTap;

  const _ExtChip({required this.chip, required this.selected, required this.onTap});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return GestureDetector(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        decoration: BoxDecoration(
          color: selected ? m.soft(c.accent) : c.surface2,
          borderRadius: m.brPill,
          border: selected ? Border.all(color: c.accent) : null,
        ),
        child: Text.rich(
          TextSpan(
            children: [
              TextSpan(
                text: chip.ext,
                style: TextStyle(
                  fontSize: 11,
                  fontWeight: FontWeight.w500,
                  color: selected ? c.accent : c.textSecondary,
                ),
              ),
              TextSpan(
                text: ' ${chip.count}',
                style: TextStyle(fontSize: 10, color: c.textMuted),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _MiniButton extends StatelessWidget {
  final String label;
  final VoidCallback onTap;
  final bool active;

  const _MiniButton({required this.label, required this.onTap, this.active = false});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return GestureDetector(
      onTap: onTap,
      child: Container(
        height: 26,
        padding: const EdgeInsets.symmetric(horizontal: 8),
        alignment: Alignment.center,
        decoration: BoxDecoration(
          color: active ? m.soft(c.accent) : c.surface1,
          border: Border.all(color: active ? c.accent : c.border),
          borderRadius: m.brSm,
        ),
        child: Text(
          label,
          style: TextStyle(fontSize: 11.5, color: active ? c.accent : c.textSecondary),
        ),
      ),
    );
  }
}

// =============================================================================
// 3. 面包屑条
// =============================================================================

class ManifestBreadcrumbBar extends StatelessWidget {
  final ManifestBreadcrumbModel breadcrumb;
  final ValueChanged<String> onNavigate;
  final VoidCallback onUp;
  final void Function(BuildContext anchor, List<ManifestCrumbSegment> overflow)
  onShowOverflowMenu;

  const ManifestBreadcrumbBar({
    super.key,
    required this.breadcrumb,
    required this.onNavigate,
    required this.onUp,
    required this.onShowOverflowMenu,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    if (breadcrumb.searching) {
      return Text(
        s.manifestSearchResultCount(breadcrumb.searchResultCount),
        style: TextStyle(fontSize: 11.5, color: c.textSecondary),
      );
    }
    final children = <Widget>[];
    if (breadcrumb.showUp) {
      children.add(
        ShadTooltip(
          builder: (_) => Text(s.manifestBreadcrumbUpTooltip),
          child: GestureDetector(
            onTap: onUp,
            child: Padding(
              padding: const EdgeInsets.only(right: 6),
              child: Icon(LucideIcons.arrowLeft, size: 13, color: c.textSecondary),
            ),
          ),
        ),
      );
    }
    for (var i = 0; i < breadcrumb.segments.length; i++) {
      final seg = breadcrumb.segments[i];
      if (i > 0) {
        children.add(Text(' / ', style: TextStyle(fontSize: 11, color: c.textMuted)));
      }
      children.add(
        _CrumbSegmentWidget(
          seg: seg,
          overflow: breadcrumb.overflowSegments,
          onNavigate: onNavigate,
          onShowOverflowMenu: onShowOverflowMenu,
        ),
      );
    }
    return Row(children: children);
  }
}

class _CrumbSegmentWidget extends StatelessWidget {
  final ManifestCrumbSegment seg;
  final List<ManifestCrumbSegment> overflow;
  final ValueChanged<String> onNavigate;
  final void Function(BuildContext anchor, List<ManifestCrumbSegment> overflow)
  onShowOverflowMenu;

  const _CrumbSegmentWidget({
    required this.seg,
    required this.overflow,
    required this.onNavigate,
    required this.onShowOverflowMenu,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    switch (seg.kind) {
      case ManifestCrumbKind.home:
        return GestureDetector(
          onTap: seg.isLast ? null : () => onNavigate(''),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(LucideIcons.folder, size: 12, color: c.textMuted),
              const SizedBox(width: 4),
              Text(
                s.categoryAll,
                style: TextStyle(
                  fontSize: 11.5,
                  fontWeight: seg.isLast ? FontWeight.w600 : FontWeight.w400,
                  color: seg.isLast ? c.textPrimary : c.textSecondary,
                ),
              ),
            ],
          ),
        );
      case ManifestCrumbKind.ellipsis:
        return Builder(
          builder: (anchor) => GestureDetector(
            onTap: () => onShowOverflowMenu(anchor, overflow),
            child: ShadTooltip(
              builder: (_) => Text(s.manifestBreadcrumbMoreTooltip),
              child: Text(
                seg.label,
                style: TextStyle(
                  fontSize: 12,
                  fontWeight: FontWeight.w700,
                  color: c.textSecondary,
                ),
              ),
            ),
          ),
        );
      case ManifestCrumbKind.segment:
        return GestureDetector(
          onTap: seg.isLast ? null : () => onNavigate(seg.path),
          child: Text(
            seg.label,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(
              fontSize: 11.5,
              fontWeight: seg.isLast ? FontWeight.w600 : FontWeight.w400,
              color: seg.isLast ? c.textPrimary : c.textSecondary,
            ),
          ),
        );
    }
  }
}

// =============================================================================
// 6. 底栏
// =============================================================================

class ManifestFooterBar extends StatelessWidget {
  final String saveDir;
  final String manifestName;
  final TextEditingController groupNameController;
  final bool isPickingDir;
  final VoidCallback onPickSaveDir;
  final ManifestSelectionStat selStat;
  final VoidCallback onCancel;

  /// 「开始下载 ▾」主按钮 tooltip 目标队列/组名文案（主队列名或组名，由
  /// 调用方按 `initialQueueId == kMainQueueId` 判定预先算好传入）。
  final String startTooltipTarget;

  final VoidCallback onSubmitLater;
  final void Function(BuildContext anchor) onPickLaterQueue;
  final VoidCallback onSubmitStart;
  final void Function(BuildContext anchor) onPickStartQueue;

  const ManifestFooterBar({
    super.key,
    required this.saveDir,
    required this.manifestName,
    required this.groupNameController,
    required this.isPickingDir,
    required this.onPickSaveDir,
    required this.selStat,
    required this.onCancel,
    required this.startTooltipTarget,
    required this.onSubmitLater,
    required this.onPickLaterQueue,
    required this.onSubmitStart,
    required this.onPickStartQueue,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final summaryText = selStat.count == 0
        ? s.manifestNoSelection
        : selStat.unknownCount > 0
        ? '${s.manifestSelectedSummaryApprox(selStat.count, DownloadTask.formatBytes(selStat.size))} '
              '${s.manifestUnknownSizeNote(selStat.unknownCount)}'
        : s.manifestSelectedSummary(selStat.count, DownloadTask.formatBytes(selStat.size));
    final enabled = selStat.count > 0;

    return Row(
      crossAxisAlignment: CrossAxisAlignment.center,
      children: [
        SizedBox(
          width: 220,
          child: ListenableBuilder(
            listenable: groupNameController,
            builder: (context, _) => DirPickerField(
              path: p.join(
                saveDir,
                groupNameController.text.trim().isEmpty
                    ? manifestName
                    : groupNameController.text.trim(),
              ),
              enabled: !isPickingDir,
              onTap: onPickSaveDir,
            ),
          ),
        ),
        const SizedBox(width: 12),
        Expanded(
          child: Text(
            summaryText,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(fontSize: 12, fontWeight: FontWeight.w500, color: c.textPrimary),
          ),
        ),
        const SizedBox(width: 10),
        ShadButton.outline(onPressed: onCancel, child: Text(s.cancel)),
        const SizedBox(width: 8),
        SplitActionButton(
          enabled: enabled,
          icon: LucideIcons.clock,
          label: s.downloadLater,
          tooltip: s.laterIntoQueueTooltip(s.laterQueue),
          onPressed: onSubmitLater,
          onPickQueue: onPickLaterQueue,
        ),
        const SizedBox(width: 8),
        SplitActionButton(
          primary: true,
          enabled: enabled,
          icon: LucideIcons.download,
          label: selStat.count > 0
              ? s.manifestStartDownloadWithCount(selStat.count)
              : s.startDownload,
          tooltip: s.startIntoQueueTooltip(startTooltipTarget),
          onPressed: onSubmitStart,
          onPickQueue: onPickStartQueue,
        ),
      ],
    );
  }
}
