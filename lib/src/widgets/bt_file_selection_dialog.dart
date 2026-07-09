import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../mobile/sheets/mobile_bt_file_sheet.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'bt_file_list_widget.dart';

void showBtFileSelectionDialog(
  BuildContext context, {
  required String taskId,
  required int totalBytes,
  required List<BtFileEntry> files,
  VoidCallback? onClosed,
}) {
  if (Platform.isAndroid || Platform.isIOS) {
    showMobileBtFileSheet(
      context,
      taskId: taskId,
      files: files,
      onClosed: onClosed,
    );
    return;
  }
  showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    barrierDismissible: false,
    animateIn: const [],
    animateOut: const [],
    builder: (context) => _BtFileSelectionDialogContent(
      taskId: taskId,
      totalBytes: totalBytes,
      files: files,
      onClosed: onClosed,
    ),
  );
}

class _BtFileSelectionDialogContent extends StatefulWidget {
  final String taskId;
  final int totalBytes;
  final List<BtFileEntry> files;
  final VoidCallback? onClosed;

  const _BtFileSelectionDialogContent({
    required this.taskId,
    required this.totalBytes,
    required this.files,
    this.onClosed,
  });

  @override
  State<_BtFileSelectionDialogContent> createState() =>
      _BtFileSelectionDialogContentState();
}

class _BtFileSelectionDialogContentState
    extends State<_BtFileSelectionDialogContent> {
  late Set<int> _selectedIndices;
  bool _selectionSent = false;

  @override
  void initState() {
    super.initState();
    _selectedIndices = widget.files.map((f) => f.index.toInt()).toSet();
  }

  @override
  void dispose() {
    if (!_selectionSent) {
      // Dialog dismissed without explicit action — treat as cancel.
      _selectionSent = true;
      SelectBtFiles(
        taskId: widget.taskId,
        selectedIndices: const [-1],
      ).sendSignalToRust();
    }
    widget.onClosed?.call();
    super.dispose();
  }

  bool get _noneSelected => _selectedIndices.isEmpty;

  int get _selectedTotalBytes {
    int total = 0;
    for (final f in widget.files) {
      if (_selectedIndices.contains(f.index.toInt())) {
        total += f.size.toInt();
      }
    }
    return total;
  }

  void _sendSelection() {
    if (_selectionSent) return;
    _selectionSent = true;
    final indices = _selectedIndices.toList()..sort();
    SelectBtFiles(
      taskId: widget.taskId,
      selectedIndices: indices,
    ).sendSignalToRust();
  }

  void _onConfirm() {
    if (_noneSelected) return;
    _sendSelection();
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final isMultiFile = widget.files.length > 1;

    return ShadDialog(
      constraints: const BoxConstraints(maxWidth: 560),
      title: Row(
        children: [
          Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: m.soft(c.accent),
              borderRadius: m.brMd,
            ),
            child: Icon(LucideIcons.folderOpen, size: 14, color: c.accent),
          ),
          const SizedBox(width: 10),
          Text(s.btFileSelectTitle),
        ],
      ),
      description: Text(
        isMultiFile
            ? s.btFileSelectDesc(widget.files.length)
            : s.btFileSelectDescSingle,
      ),
      actions: [
        ShadButton.outline(
          onPressed: () {
            if (!_selectionSent) {
              _selectionSent = true;
              SelectBtFiles(
                taskId: widget.taskId,
                selectedIndices: const [-1],
              ).sendSignalToRust();
            }
            Navigator.of(context).pop();
          },
          child: Text(s.cancel),
        ),
        ShadButton(
          onPressed: _noneSelected ? null : _onConfirm,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(
                LucideIcons.download,
                size: 13,
                color: Color(0xFFFFFFFF),
              ),
              const SizedBox(width: 6),
              Text(
                s.btFileSelectConfirm(
                  _selectedIndices.length,
                  formatBtFileSize(_selectedTotalBytes),
                ),
                style: const TextStyle(color: Color(0xFFFFFFFF)),
              ),
            ],
          ),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 12),
        child: BtFileListWidget(
          files: widget.files,
          selectedIndices: _selectedIndices,
          onToggleAll: () {
            setState(() {
              if (_selectedIndices.length == widget.files.length) {
                _selectedIndices = {};
              } else {
                _selectedIndices =
                    widget.files.map((f) => f.index.toInt()).toSet();
              }
            });
          },
          onToggleFile: (idx) {
            setState(() {
              if (_selectedIndices.contains(idx)) {
                _selectedIndices = Set.from(_selectedIndices)..remove(idx);
              } else {
                _selectedIndices = Set.from(_selectedIndices)..add(idx);
              }
            });
          },
          maxHeight: 340,
        ),
      ),
    );
  }
}
