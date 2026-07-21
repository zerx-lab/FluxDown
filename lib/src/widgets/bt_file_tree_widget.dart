import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'bt_file_selection_shared.dart'
    show
        BtCheckbox,
        BtSelectAllRow,
        btFileIcon,
        formatBtFileSize,
        toggleBtFileSelection;

class BtFileTreeNode {
  final String name;
  final String path;
  final BtFileEntry? file;
  final List<BtFileTreeNode> children;
  final List<BtFileEntry> descendantFiles;

  const BtFileTreeNode._({
    required this.name,
    required this.path,
    required this.file,
    required this.children,
    required this.descendantFiles,
  });

  bool get isDirectory => file == null;
}

class _MutableBtDirectory {
  final String name;
  final String path;
  final Map<String, _MutableBtDirectory> directories = {};
  final List<BtFileTreeNode> files = [];

  _MutableBtDirectory({required this.name, required this.path});
}

List<BtFileTreeNode> buildBtFileTree(List<BtFileEntry> files) {
  final root = _MutableBtDirectory(name: '', path: '');

  for (final file in files) {
    final parts = file.path
        .split(RegExp(r'[\\/]+'))
        .where((part) => part.isNotEmpty)
        .toList();
    if (parts.isEmpty) parts.add('file_${file.index}');

    var parent = root;
    for (var i = 0; i < parts.length - 1; i++) {
      final name = parts[i];
      final path = parent.path.isEmpty ? name : '${parent.path}/$name';
      parent = parent.directories.putIfAbsent(
        name,
        () => _MutableBtDirectory(name: name, path: path),
      );
    }

    final name = parts.last;
    final path = parent.path.isEmpty ? name : '${parent.path}/$name';
    parent.files.add(
      BtFileTreeNode._(
        name: name,
        path: path,
        file: file,
        children: const [],
        descendantFiles: [file],
      ),
    );
  }

  BtFileTreeNode freezeDirectory(_MutableBtDirectory directory) {
    final children =
        <BtFileTreeNode>[
          for (final child in directory.directories.values)
            freezeDirectory(child),
          ...directory.files,
        ]..sort((a, b) {
          if (a.isDirectory != b.isDirectory) return a.isDirectory ? -1 : 1;
          final byName = a.name.toLowerCase().compareTo(b.name.toLowerCase());
          if (byName != 0) return byName;
          return a.path.compareTo(b.path);
        });
    return BtFileTreeNode._(
      name: directory.name,
      path: directory.path,
      file: null,
      children: children,
      descendantFiles: [for (final child in children) ...child.descendantFiles],
    );
  }

  return freezeDirectory(root).children;
}

class BtFileTreeWidget extends StatefulWidget {
  final List<BtFileEntry> files;
  final Set<int> selectedIndices;
  final ValueChanged<Set<int>> onSelectionChanged;
  final double maxHeight;

  const BtFileTreeWidget({
    super.key,
    required this.files,
    required this.selectedIndices,
    required this.onSelectionChanged,
    this.maxHeight = 300,
  });

  @override
  State<BtFileTreeWidget> createState() => _BtFileTreeWidgetState();
}

class _BtFileTreeWidgetState extends State<BtFileTreeWidget> {
  late List<BtFileTreeNode> _roots;
  final Set<String> _expandedDirectories = {};
  Set<String> _knownDirectories = {};

  @override
  void initState() {
    super.initState();
    _rebuildTree();
  }

  @override
  void didUpdateWidget(covariant BtFileTreeWidget oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.files, widget.files)) {
      _rebuildTree();
    }
  }

  void _rebuildTree() {
    _roots = buildBtFileTree(widget.files);
    final directories = <String>{};

    void collectDirectories(List<BtFileTreeNode> nodes) {
      for (final node in nodes) {
        if (!node.isDirectory) continue;
        directories.add(node.path);
        collectDirectories(node.children);
      }
    }

    collectDirectories(_roots);
    _expandedDirectories.retainAll(directories);
    _expandedDirectories.addAll(directories.difference(_knownDirectories));
    _knownDirectories = directories;
  }

  bool get _allSelected => widget.files.every(
    (file) => widget.selectedIndices.contains(file.index.toInt()),
  );

  int get _selectedTotalBytes => widget.files
      .where((file) => widget.selectedIndices.contains(file.index.toInt()))
      .fold(0, (total, file) => total + file.size.toInt());

  void _toggleFiles(Iterable<BtFileEntry> targetFiles) {
    widget.onSelectionChanged(
      toggleBtFileSelection(
        widget.selectedIndices,
        targetFiles.map((file) => file.index.toInt()),
      ),
    );
  }

  void _toggleExpanded(String path) {
    setState(() {
      if (!_expandedDirectories.remove(path)) {
        _expandedDirectories.add(path);
      }
    });
  }

  List<Widget> _buildTreeRows(
    List<BtFileTreeNode> nodes,
    int depth,
    AppColors c,
  ) {
    final rows = <Widget>[];
    for (final node in nodes) {
      if (node.isDirectory) {
        final expanded = _expandedDirectories.contains(node.path);
        rows.add(
          BtDirectoryTile(
            key: ValueKey('bt-tree-dir:${node.path}'),
            node: node,
            depth: depth,
            isExpanded: expanded,
            selectedIndices: widget.selectedIndices,
            onToggleExpanded: () => _toggleExpanded(node.path),
            onToggleSelection: () => _toggleFiles(node.descendantFiles),
            c: c,
          ),
        );
        if (expanded) {
          rows.addAll(_buildTreeRows(node.children, depth + 1, c));
        }
      } else {
        final file = node.file!;
        rows.add(
          _BtTreeFileTile(
            key: ValueKey('bt-tree-file:${file.index}'),
            file: file,
            depth: depth,
            isSelected: widget.selectedIndices.contains(file.index.toInt()),
            onTap: () => _toggleFiles([file]),
            c: c,
          ),
        );
      }
    }
    return rows;
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
          BtSelectAllRow(
            allSelected: _allSelected,
            noneSelected: widget.selectedIndices.isEmpty,
            totalFiles: widget.files.length,
            selectedCount: widget.selectedIndices.length,
            selectedBytes: _selectedTotalBytes,
            onToggle: () => _toggleFiles(widget.files),
            c: c,
            s: s,
          ),
          const SizedBox(height: 8),
        ],
        ConstrainedBox(
          constraints: BoxConstraints(maxHeight: widget.maxHeight),
          child: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [..._buildTreeRows(_roots, 0, c)],
            ),
          ),
        ),
      ],
    );
  }
}

class BtDirectoryTile extends StatelessWidget {
  final BtFileTreeNode node;
  final int depth;
  final bool isExpanded;
  final Set<int> selectedIndices;
  final VoidCallback onToggleExpanded;
  final VoidCallback onToggleSelection;
  final AppColors c;

  const BtDirectoryTile({
    super.key,
    required this.node,
    required this.depth,
    required this.isExpanded,
    required this.selectedIndices,
    required this.onToggleExpanded,
    required this.onToggleSelection,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    var selectedCount = 0;
    var selectedBytes = 0;
    for (final file in node.descendantFiles) {
      if (selectedIndices.contains(file.index.toInt())) {
        selectedCount++;
        selectedBytes += file.size.toInt();
      }
    }
    final allSelected = selectedCount == node.descendantFiles.length;
    final noneSelected = selectedCount == 0;
    final active = !noneSelected;

    return Container(
      margin: const EdgeInsets.only(bottom: 4),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
      decoration: BoxDecoration(
        color: active ? m.faint(c.accent) : c.surface1,
        borderRadius: m.brCard,
        border: Border.all(
          color: active ? m.borderFaint(c.accent) : c.border,
          width: 1,
        ),
      ),
      child: Row(
        children: [
          SizedBox(width: _btTreeIndent(depth)),
          GestureDetector(
            key: ValueKey('bt-tree-expand:${node.path}'),
            behavior: HitTestBehavior.opaque,
            onTap: onToggleExpanded,
            child: SizedBox(
              width: 24,
              height: 30,
              child: Icon(
                isExpanded ? LucideIcons.chevronDown : LucideIcons.chevronRight,
                size: 14,
                color: c.textMuted,
              ),
            ),
          ),
          GestureDetector(
            key: ValueKey('bt-tree-select:${node.path}'),
            behavior: HitTestBehavior.opaque,
            onTap: onToggleSelection,
            child: SizedBox(
              width: 18,
              height: 30,
              child: Align(
                alignment: Alignment.centerLeft,
                child: BtCheckbox(
                  checked: allSelected,
                  indeterminate: !allSelected && !noneSelected,
                  accentColor: c.accent,
                ),
              ),
            ),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: GestureDetector(
              key: ValueKey('bt-tree-open:${node.path}'),
              behavior: HitTestBehavior.opaque,
              onTap: onToggleExpanded,
              child: Row(
                children: [
                  Container(
                    width: 28,
                    height: 28,
                    decoration: BoxDecoration(
                      color: active ? m.soft(c.accent) : c.surface2,
                      borderRadius: m.brMd,
                    ),
                    child: Icon(
                      isExpanded ? LucideIcons.folderOpen : LucideIcons.folder,
                      size: 14,
                      color: active ? c.accent : c.textMuted,
                    ),
                  ),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      node.name,
                      style: TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w600,
                        color: c.textPrimary,
                      ),
                      overflow: TextOverflow.ellipsis,
                      maxLines: 1,
                    ),
                  ),
                  const SizedBox(width: 8),
                  Text(
                    '$selectedCount/${node.descendantFiles.length}  ·  '
                    '${formatBtFileSize(selectedBytes)}',
                    style: TextStyle(
                      fontSize: 11.5,
                      color: c.textMuted,
                      fontFeatures: const [FontFeature.tabularFigures()],
                    ),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _BtTreeFileTile extends StatelessWidget {
  final BtFileEntry file;
  final int depth;
  final bool isSelected;
  final VoidCallback onTap;
  final AppColors c;

  const _BtTreeFileTile({
    super.key,
    required this.file,
    required this.depth,
    required this.isSelected,
    required this.onTap,
    required this.c,
  });

  String get _fileName {
    final parts = file.path
        .split(RegExp(r'[\\/]+'))
        .where((part) => part.isNotEmpty)
        .toList();
    return parts.isEmpty ? file.path : parts.last;
  }

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return GestureDetector(
      onTap: onTap,
      child: Container(
        margin: const EdgeInsets.only(bottom: 4),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 9),
        decoration: BoxDecoration(
          color: isSelected ? m.faint(c.accent) : c.surface1,
          borderRadius: m.brCard,
          border: Border.all(
            color: isSelected ? m.borderFaint(c.accent) : c.border,
            width: 1,
          ),
        ),
        child: Row(
          children: [
            SizedBox(width: _btTreeIndent(depth)),
            const SizedBox(width: 24),
            BtCheckbox(checked: isSelected, accentColor: c.accent),
            const SizedBox(width: 10),
            Container(
              width: 28,
              height: 28,
              decoration: BoxDecoration(
                color: isSelected ? m.soft(c.accent) : c.surface2,
                borderRadius: m.brMd,
              ),
              child: Icon(
                btFileIcon(file.path),
                size: 14,
                color: isSelected ? c.accent : c.textMuted,
              ),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                _fileName,
                style: TextStyle(
                  fontSize: 13,
                  fontWeight: FontWeight.w500,
                  color: c.textPrimary,
                ),
                overflow: TextOverflow.ellipsis,
                maxLines: 1,
              ),
            ),
            const SizedBox(width: 8),
            Text(
              formatBtFileSize(file.size.toInt()),
              style: TextStyle(
                fontSize: 11.5,
                color: c.textMuted,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

double _btTreeIndent(int depth) => (depth > 8 ? 8 : depth) * 18.0;
