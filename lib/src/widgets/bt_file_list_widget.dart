import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

String formatBtFileSize(int bytes) {
  if (bytes <= 0) return '0 B';
  if (bytes >= 1024 * 1024 * 1024) {
    return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)} GB';
  }
  if (bytes >= 1024 * 1024) {
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
  }
  if (bytes >= 1024) {
    return '${(bytes / 1024).toStringAsFixed(1)} KB';
  }
  return '$bytes B';
}

IconData btFileIcon(String path) {
  final lower = path.toLowerCase();
  if (lower.endsWith('.mp4') ||
      lower.endsWith('.mkv') ||
      lower.endsWith('.avi') ||
      lower.endsWith('.mov') ||
      lower.endsWith('.wmv') ||
      lower.endsWith('.flv') ||
      lower.endsWith('.webm') ||
      lower.endsWith('.m4v') ||
      lower.endsWith('.ts') ||
      lower.endsWith('.m2ts')) {
    return LucideIcons.film;
  }
  if (lower.endsWith('.mp3') ||
      lower.endsWith('.flac') ||
      lower.endsWith('.aac') ||
      lower.endsWith('.ogg') ||
      lower.endsWith('.wav') ||
      lower.endsWith('.m4a') ||
      lower.endsWith('.opus')) {
    return LucideIcons.music;
  }
  if (lower.endsWith('.jpg') ||
      lower.endsWith('.jpeg') ||
      lower.endsWith('.png') ||
      lower.endsWith('.gif') ||
      lower.endsWith('.bmp') ||
      lower.endsWith('.webp') ||
      lower.endsWith('.svg') ||
      lower.endsWith('.tiff')) {
    return LucideIcons.image;
  }
  if (lower.endsWith('.zip') ||
      lower.endsWith('.rar') ||
      lower.endsWith('.7z') ||
      lower.endsWith('.tar') ||
      lower.endsWith('.gz') ||
      lower.endsWith('.bz2') ||
      lower.endsWith('.xz')) {
    return LucideIcons.package2;
  }
  if (lower.endsWith('.pdf') ||
      lower.endsWith('.doc') ||
      lower.endsWith('.docx') ||
      lower.endsWith('.xls') ||
      lower.endsWith('.xlsx') ||
      lower.endsWith('.ppt') ||
      lower.endsWith('.pptx') ||
      lower.endsWith('.txt') ||
      lower.endsWith('.md')) {
    return LucideIcons.fileText;
  }
  return LucideIcons.file;
}

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

  int get _totalBytes {
    int total = 0;
    for (final f in files) {
      total += f.size.toInt();
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
            totalBytes: _totalBytes,
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
// BtSelectAllRow
// ---------------------------------------------------------------------------

class BtSelectAllRow extends StatelessWidget {
  final bool allSelected;
  final bool noneSelected;
  final int totalFiles;
  final int selectedCount;
  final int totalBytes;
  final int selectedBytes;
  final VoidCallback onToggle;
  final AppColors c;
  final S s;

  const BtSelectAllRow({
    super.key,
    required this.allSelected,
    required this.noneSelected,
    required this.totalFiles,
    required this.selectedCount,
    required this.totalBytes,
    required this.selectedBytes,
    required this.onToggle,
    required this.c,
    required this.s,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return GestureDetector(
      onTap: onToggle,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 9),
        decoration: BoxDecoration(
          color: allSelected
              ? m.faint(c.accent)
              : c.surface1,
          borderRadius: m.brCard,
          border: Border.all(
            color: allSelected
                ? m.borderFaint(c.accent)
                : c.border,
            width: 1,
          ),
        ),
        child: Row(
          children: [
            BtCheckbox(
              checked: allSelected,
              indeterminate: !allSelected && !noneSelected,
              accentColor: c.accent,
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                s.btFileSelectAll,
                style: TextStyle(
                  fontSize: 13,
                  fontWeight: FontWeight.w600,
                  color: c.textPrimary,
                ),
              ),
            ),
            Text(
              '$selectedCount / $totalFiles  ·  ${formatBtFileSize(selectedBytes)}',
              style: TextStyle(fontSize: 11.5, color: c.textMuted),
            ),
          ],
        ),
      ),
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

// ---------------------------------------------------------------------------
// BtCheckbox — minimal checkbox widget (no Material dependency)
// ---------------------------------------------------------------------------

class BtCheckbox extends StatelessWidget {
  final bool checked;
  final bool indeterminate;
  final Color accentColor;

  const BtCheckbox({
    super.key,
    required this.checked,
    this.indeterminate = false,
    required this.accentColor,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final active = checked || indeterminate;
    return Container(
      width: 17,
      height: 17,
      decoration: BoxDecoration(
        borderRadius: m.brSm,
        color: active ? accentColor : const Color(0x00000000),
        border: Border.all(
          color: active ? accentColor : const Color(0x66888888),
          width: 1.5,
        ),
      ),
      child: active
          ? Center(
              child: indeterminate
                  ? Container(
                      width: 9,
                      height: 2,
                      decoration: BoxDecoration(
                        color: const Color(0xFFFFFFFF),
                        borderRadius: m.brProgress,
                      ),
                    )
                  : const Icon(
                      LucideIcons.check,
                      size: 11,
                      color: Color(0xFFFFFFFF),
                    ),
            )
          : null,
    );
  }
}
