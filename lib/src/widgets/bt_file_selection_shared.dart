import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

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

Set<int> toggleBtFileSelection(
  Set<int> selectedIndices,
  Iterable<int> targetIndices,
) {
  final indices = targetIndices.toSet();
  if (indices.isEmpty) return Set<int>.from(selectedIndices);

  final result = Set<int>.from(selectedIndices);
  if (indices.every(selectedIndices.contains)) {
    result.removeAll(indices);
  } else {
    result.addAll(indices);
  }
  return result;
}

class BtSelectAllRow extends StatelessWidget {
  final bool allSelected;
  final bool noneSelected;
  final int totalFiles;
  final int selectedCount;
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
          color: allSelected ? m.faint(c.accent) : c.surface1,
          borderRadius: m.brCard,
          border: Border.all(
            color: allSelected ? m.borderFaint(c.accent) : c.border,
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
