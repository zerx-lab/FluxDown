import 'package:flutter/widgets.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'bt_file_selection_shared.dart'
    show BtCheckbox, BtSelectAllRow, btFileIcon, formatBtFileSize;

// ---------------------------------------------------------------------------
// BtFileListWidget — the full interactive file selection list.
//
// Used both inside the new-download dialog (preview before task creation)
// and inside the post-metadata dialog for magnet links.
// ---------------------------------------------------------------------------

class BtFileListWidget extends StatelessWidget {
  /// All files in the torrent.
  final List<BtFileEntry> files;

  /// Currently selected indices.
  final Set<int> selectedIndices;

  /// Called when the user taps the select-all / deselect-all row.
  final VoidCallback onToggleAll;

  /// Called when the user taps a single file row.
  final ValueChanged<int> onToggleFile;

  /// Max height for the scrollable file list area. Defaults to 300.
  final double maxHeight;

  const BtFileListWidget({
    super.key,
    required this.files,
    required this.selectedIndices,
    required this.onToggleAll,
    required this.onToggleFile,
    this.maxHeight = 300,
  });

  bool get _allSelected => selectedIndices.length == files.length;
  bool get _noneSelected => selectedIndices.isEmpty;

  int get _selectedTotalBytes {
    int total = 0;
    for (final f in files) {
      if (selectedIndices.contains(f.index.toInt())) {
        total += f.size.toInt();
      }
    }
    return total;
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final isMultiFile = files.length > 1;

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // Select-all row (only for multi-file torrents)
        if (isMultiFile) ...[
          BtSelectAllRow(
            allSelected: _allSelected,
            noneSelected: _noneSelected,
            totalFiles: files.length,
            selectedCount: selectedIndices.length,
            selectedBytes: _selectedTotalBytes,
            onToggle: onToggleAll,
            c: c,
            s: s,
          ),
          const SizedBox(height: 8),
        ],
        // File list
        ConstrainedBox(
          constraints: BoxConstraints(maxHeight: maxHeight),
          child: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                for (final file in files)
                  BtFileTile(
                    file: file,
                    isSelected: selectedIndices.contains(file.index.toInt()),
                    onTap: () => onToggleFile(file.index.toInt()),
                    c: c,
                  ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

// ---------------------------------------------------------------------------
// BtFileTile
// ---------------------------------------------------------------------------

class BtFileTile extends StatelessWidget {
  final BtFileEntry file;
  final bool isSelected;
  final VoidCallback onTap;
  final AppColors c;

  const BtFileTile({
    super.key,
    required this.file,
    required this.isSelected,
    required this.onTap,
    required this.c,
  });

  String get _fileName {
    final p = file.path;
    final sep = p.contains('/') ? '/' : r'\';
    final idx = p.lastIndexOf(sep);
    return idx >= 0 ? p.substring(idx + 1) : p;
  }

  String get _dirPath {
    final p = file.path;
    final sep = p.contains('/') ? '/' : r'\';
    final lastSep = p.lastIndexOf(sep);
    if (lastSep <= 0) return '';
    final dir = p.substring(0, lastSep);
    // Strip the first path component (torrent root dir name) to avoid
    // showing the torrent name redundantly as a directory prefix.
    //   "TorrentName/file.ext"        → dirPath = ''
    //   "TorrentName/subdir/file.ext" → dirPath = 'subdir'
    //   "TorrentName/a/b/file.ext"    → dirPath = 'a/b'
    final firstSep = dir.indexOf(sep);
    if (firstSep < 0) return ''; // Only one level = torrent name, don't show
    return dir.substring(firstSep + 1);
  }

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final dirPath = _dirPath;
    final fileName = _fileName;

    return GestureDetector(
      onTap: onTap,
      child: Container(
        margin: const EdgeInsets.only(bottom: 4),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 9),
        decoration: BoxDecoration(
          color: isSelected
              ? m.faint(c.accent)
              : c.surface1,
          borderRadius: m.brCard,
          border: Border.all(
            color: isSelected ? m.borderFaint(c.accent) : c.border,
            width: 1,
          ),
        ),
        child: Row(
          children: [
            BtCheckbox(checked: isSelected, accentColor: c.accent),
            const SizedBox(width: 10),
            Container(
              width: 28,
              height: 28,
              decoration: BoxDecoration(
                color: isSelected
                    ? m.soft(c.accent)
                    : c.surface2,
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
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    fileName,
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w500,
                      color: c.textPrimary,
                    ),
                    overflow: TextOverflow.ellipsis,
                    maxLines: 1,
                  ),
                  if (dirPath.isNotEmpty) ...[
                    const SizedBox(height: 1),
                    Text(
                      dirPath,
                      style: TextStyle(fontSize: 11, color: c.textMuted),
                      overflow: TextOverflow.ellipsis,
                      maxLines: 1,
                    ),
                  ],
                ],
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
