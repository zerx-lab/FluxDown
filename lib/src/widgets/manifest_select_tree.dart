// manifest_select_dialog.dart 的文件树部分：虚拟化 ListView.builder + 单行
// 渲染。行布局两段式——名称列（吃全部缩进）+ 右列（规格下拉/大小，固定宽
// 右对齐，跨行始终对齐，design-proto-spec §8 E4 缩进只吃名称列）。

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/manifest_selection.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'bt_file_list_widget.dart' show BtCheckbox, btFileIcon;

const double kManifestRowHeight = 36;
const double _kIndentUnit = 16;
const double _kVariantColWidth = 118;
const double _kSizeColWidth = 68;

/// 虚拟化文件树：外部传入已经算好的可见行（[flattenManifestTree] 输出），
/// 本组件只负责渲染 + 交互回调，不持有选择/折叠状态。
class ManifestTreeList extends StatelessWidget {
  final List<ManifestVisibleRow> rows;
  final Set<String> selectedItemIds;
  final Set<String> collapsedDirPaths;
  final Map<String, String?> effectiveVariants;
  final double height;
  final ValueChanged<ManifestDirNode> onToggleDirCollapse;
  final void Function(ManifestDirNode dir, bool select) onToggleDirSelection;
  final void Function(ManifestFileNode file) onToggleFileSelection;
  final void Function(ManifestFileNode file, String variantId) onSelectVariant;

  const ManifestTreeList({
    super.key,
    required this.rows,
    required this.selectedItemIds,
    required this.collapsedDirPaths,
    required this.effectiveVariants,
    required this.onToggleDirCollapse,
    required this.onToggleDirSelection,
    required this.onToggleFileSelection,
    required this.onSelectVariant,
    this.height = 300,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Container(
      height: height,
      decoration: BoxDecoration(
        color: c.surface2,
        borderRadius: AppMetrics.of(context).brMd,
        border: Border.all(color: c.border),
      ),
      child: rows.isEmpty
          ? Center(
              child: Text(
                LocaleScope.of(context).manifestTreeEmpty,
                style: TextStyle(fontSize: 12, color: c.textMuted),
              ),
            )
          : ListView.builder(
              padding: const EdgeInsets.symmetric(vertical: 4),
              itemExtent: kManifestRowHeight,
              itemCount: rows.length,
              itemBuilder: (context, index) {
                final row = rows[index];
                return ManifestTreeRow(
                  row: row,
                  selectedItemIds: selectedItemIds,
                  collapsed:
                      row.node is ManifestDirNode &&
                      collapsedDirPaths.contains(
                        (row.node as ManifestDirNode).path,
                      ),
                  effectiveVariants: effectiveVariants,
                  onToggleDirCollapse: onToggleDirCollapse,
                  onToggleDirSelection: onToggleDirSelection,
                  onToggleFileSelection: onToggleFileSelection,
                  onSelectVariant: onSelectVariant,
                );
              },
            ),
    );
  }
}

class ManifestTreeRow extends StatelessWidget {
  final ManifestVisibleRow row;
  final Set<String> selectedItemIds;
  final bool collapsed;
  final Map<String, String?> effectiveVariants;
  final ValueChanged<ManifestDirNode> onToggleDirCollapse;
  final void Function(ManifestDirNode dir, bool select) onToggleDirSelection;
  final void Function(ManifestFileNode file) onToggleFileSelection;
  final void Function(ManifestFileNode file, String variantId) onSelectVariant;

  const ManifestTreeRow({
    super.key,
    required this.row,
    required this.selectedItemIds,
    required this.collapsed,
    required this.effectiveVariants,
    required this.onToggleDirCollapse,
    required this.onToggleDirSelection,
    required this.onToggleFileSelection,
    required this.onSelectVariant,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final node = row.node;
    return switch (node) {
      ManifestDirNode() => _buildDirRow(context, c, node),
      ManifestFileNode() => _buildFileRow(context, c, node),
    };
  }

  Widget _buildDirRow(BuildContext context, AppColors c, ManifestDirNode dir) {
    final state = manifestDirCheckState(dir, selectedItemIds);
    return _rowShell(
      c: c,
      onTap: () =>
          onToggleDirSelection(dir, state != ManifestCheckState.checked),
      children: [
        SizedBox(width: _kIndentUnit * row.indent),
        GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () => onToggleDirCollapse(dir),
          child: SizedBox(
            width: 18,
            height: kManifestRowHeight,
            child: Icon(
              collapsed ? LucideIcons.chevronRight : LucideIcons.chevronDown,
              size: 14,
              color: c.textMuted,
            ),
          ),
        ),
        BtCheckbox(
          checked: state == ManifestCheckState.checked,
          indeterminate: state == ManifestCheckState.indeterminate,
          accentColor: c.accent,
        ),
        const SizedBox(width: 8),
        Icon(LucideIcons.folder, size: 14, color: c.textMuted),
        const SizedBox(width: 6),
        Expanded(
          child: Text.rich(
            TextSpan(
              children: [
                if (row.greyPrefix.isNotEmpty)
                  TextSpan(
                    text: row.greyPrefix,
                    style: TextStyle(color: c.textMuted, fontSize: 12.5),
                  ),
                TextSpan(
                  text: dir.name,
                  style: TextStyle(
                    color: c.textPrimary,
                    fontSize: 12.5,
                    fontWeight: FontWeight.w500,
                  ),
                ),
              ],
            ),
            overflow: TextOverflow.ellipsis,
            maxLines: 1,
          ),
        ),
        const SizedBox(width: _kVariantColWidth),
        SizedBox(
          width: _kSizeColWidth,
          child: Text(
            DownloadTask.formatBytes(manifestNodeTotalSize(dir)),
            textAlign: TextAlign.right,
            style: TextStyle(fontSize: 11.5, color: c.textMuted),
          ),
        ),
      ],
    );
  }

  Widget _buildFileRow(
    BuildContext context,
    AppColors c,
    ManifestFileNode file,
  ) {
    final selected = selectedItemIds.contains(file.item.id);
    final variants = file.item.variants;
    final currentVariantId = effectiveVariants[file.item.id];
    return _rowShell(
      c: c,
      onTap: () => onToggleFileSelection(file),
      children: [
        SizedBox(width: _kIndentUnit * row.indent),
        const SizedBox(width: 18),
        BtCheckbox(checked: selected, accentColor: c.accent),
        const SizedBox(width: 8),
        Icon(btFileIcon(file.item.name), size: 14, color: c.textMuted),
        const SizedBox(width: 6),
        Expanded(
          child: Text.rich(
            TextSpan(
              children: [
                if (row.greyPrefix.isNotEmpty)
                  TextSpan(
                    text: row.greyPrefix,
                    style: TextStyle(color: c.textMuted, fontSize: 12.5),
                  ),
                TextSpan(
                  text: file.name,
                  style: TextStyle(color: c.textPrimary, fontSize: 12.5),
                ),
              ],
            ),
            overflow: TextOverflow.ellipsis,
            maxLines: 1,
          ),
        ),
        SizedBox(
          width: _kVariantColWidth,
          child: variants.isEmpty
              ? null
              : Padding(
                  padding: const EdgeInsets.only(right: 6),
                  child: ShadSelect<String>(
                    key: ValueKey('variant_${file.item.id}'),
                    initialValue: currentVariantId,
                    minWidth: _kVariantColWidth - 6,
                    options: [
                      for (final v in variants)
                        ShadOption(value: v.id, child: Text(v.label)),
                    ],
                    selectedOptionBuilder: (context, value) {
                      final v = variants
                          .where((v) => v.id == value)
                          .firstOrNull;
                      return Text(
                        v?.label ?? '',
                        overflow: TextOverflow.ellipsis,
                        maxLines: 1,
                        style: const TextStyle(fontSize: 11.5),
                      );
                    },
                    onChanged: (value) {
                      if (value != null) onSelectVariant(file, value);
                    },
                  ),
                ),
        ),
        SizedBox(
          width: _kSizeColWidth,
          child: Text(
            DownloadTask.formatBytes(
              currentVariantId == null
                  ? file.item.size
                  : (variants
                            .where((v) => v.id == currentVariantId)
                            .firstOrNull
                            ?.size ??
                        file.item.size),
            ),
            textAlign: TextAlign.right,
            style: TextStyle(fontSize: 11.5, color: c.textMuted),
          ),
        ),
      ],
    );
  }

  Widget _rowShell({
    required AppColors c,
    required VoidCallback onTap,
    required List<Widget> children,
  }) {
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: SizedBox(
        height: kManifestRowHeight,
        child: Row(children: children),
      ),
    );
  }
}
