import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'bt_file_list_widget.dart';
import 'bt_file_selection_shared.dart' show toggleBtFileSelection;
import 'bt_file_tree_widget.dart';

enum BtFileDisplayMode { tree, list }

class BtFileSelectionView extends StatefulWidget {
  final List<BtFileEntry> files;
  final Set<int> selectedIndices;
  final ValueChanged<Set<int>> onSelectionChanged;
  final double maxHeight;

  const BtFileSelectionView({
    super.key,
    required this.files,
    required this.selectedIndices,
    required this.onSelectionChanged,
    this.maxHeight = 300,
  });

  @override
  State<BtFileSelectionView> createState() => _BtFileSelectionViewState();
}

class _BtFileSelectionViewState extends State<BtFileSelectionView> {
  BtFileDisplayMode _mode = BtFileDisplayMode.tree;

  /// Directory expansion is owned here (not inside the tree widget) so it
  /// survives switching between tree and list views.
  final Set<String> _collapsedDirectories = {};

  void _toggleIndices(Iterable<int> indices) {
    widget.onSelectionChanged(
      toggleBtFileSelection(widget.selectedIndices, indices),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (widget.files.length > 1) ...[
          Align(
            alignment: Alignment.centerRight,
            child: _BtFileDisplayModeControl(
              mode: _mode,
              onChanged: (mode) => setState(() => _mode = mode),
              c: c,
              s: s,
            ),
          ),
          const SizedBox(height: 6),
        ],
        if (_mode == BtFileDisplayMode.tree)
          BtFileTreeWidget(
            files: widget.files,
            selectedIndices: widget.selectedIndices,
            onSelectionChanged: widget.onSelectionChanged,
            maxHeight: widget.maxHeight,
            collapsedDirectories: _collapsedDirectories,
            onToggleDirectory: (path) => setState(() {
              if (!_collapsedDirectories.remove(path)) {
                _collapsedDirectories.add(path);
              }
            }),
          )
        else
          BtFileListWidget(
            files: widget.files,
            selectedIndices: widget.selectedIndices,
            onToggleAll: () =>
                _toggleIndices(widget.files.map((file) => file.index.toInt())),
            onToggleFile: (index) => _toggleIndices([index]),
            maxHeight: widget.maxHeight,
          ),
      ],
    );
  }
}

class _BtFileDisplayModeControl extends StatelessWidget {
  final BtFileDisplayMode mode;
  final ValueChanged<BtFileDisplayMode> onChanged;
  final AppColors c;
  final S s;

  const _BtFileDisplayModeControl({
    required this.mode,
    required this.onChanged,
    required this.c,
    required this.s,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Container(
      padding: const EdgeInsets.all(3),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brCard,
        border: Border.all(color: c.border),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          _BtFileDisplayModeButton(
            key: const ValueKey('bt-view-tree'),
            icon: LucideIcons.folderTree,
            tooltip: s.btFileTreeView,
            selected: mode == BtFileDisplayMode.tree,
            onTap: () => onChanged(BtFileDisplayMode.tree),
            c: c,
          ),
          _BtFileDisplayModeButton(
            key: const ValueKey('bt-view-list'),
            icon: LucideIcons.list,
            tooltip: s.btFileListView,
            selected: mode == BtFileDisplayMode.list,
            onTap: () => onChanged(BtFileDisplayMode.list),
            c: c,
          ),
        ],
      ),
    );
  }
}

class _BtFileDisplayModeButton extends StatelessWidget {
  final IconData icon;
  final String tooltip;
  final bool selected;
  final VoidCallback onTap;
  final AppColors c;

  const _BtFileDisplayModeButton({
    super.key,
    required this.icon,
    required this.tooltip,
    required this.selected,
    required this.onTap,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return ShadTooltip(
      waitDuration: const Duration(milliseconds: 350),
      builder: (_) => Text(tooltip),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onTap,
          child: Container(
            width: 30,
            height: 30,
            decoration: BoxDecoration(
              color: selected ? m.soft(c.accent) : const Color(0x00000000),
              borderRadius: m.brMd,
            ),
            child: Icon(
              icon,
              size: 15,
              color: selected ? c.accent : c.textMuted,
            ),
          ),
        ),
      ),
    );
  }
}
