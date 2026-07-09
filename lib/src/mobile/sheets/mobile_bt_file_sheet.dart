import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../../bindings/bindings.dart';
import '../../i18n/locale_provider.dart';
import '../../theme/app_colors.dart';
import '../../widgets/bt_file_list_widget.dart' show formatBtFileSize;
import '../../theme/app_metrics.dart';
import '../mobile_ui.dart';

/// 移动端 BT 文件选择底部弹层（Liquid Glass 风格）。
///
/// 与桌面 `showBtFileSelectionDialog` 语义一致：
///   - 关闭 / 取消（未显式确认）→ 发送 `SelectBtFiles([-1])` 让 Rust 暂停任务；
///   - 确认 → 发送已选索引。
Future<void> showMobileBtFileSheet(
  BuildContext context, {
  required String taskId,
  required List<BtFileEntry> files,
  VoidCallback? onClosed,
}) async {
  await showMobileSheet<void>(
    context,
    builder: (ctx) => _BtFileSheet(taskId: taskId, files: files),
  );
  onClosed?.call();
}

class _BtFileSheet extends StatefulWidget {
  final String taskId;
  final List<BtFileEntry> files;

  const _BtFileSheet({required this.taskId, required this.files});

  @override
  State<_BtFileSheet> createState() => _BtFileSheetState();
}

class _BtFileSheetState extends State<_BtFileSheet> {
  late Set<int> _selected;
  bool _sent = false;

  @override
  void initState() {
    super.initState();
    _selected = widget.files.map((f) => f.index.toInt()).toSet();
  }

  @override
  void dispose() {
    // 弹层被划走 / 点遮罩关闭而未显式确认 → 视为取消。
    if (!_sent) {
      _sent = true;
      SelectBtFiles(
        taskId: widget.taskId,
        selectedIndices: const [-1],
      ).sendSignalToRust();
    }
    super.dispose();
  }

  bool get _noneSelected => _selected.isEmpty;
  bool get _allSelected => _selected.length == widget.files.length;

  int get _selectedBytes {
    var total = 0;
    for (final f in widget.files) {
      if (_selected.contains(f.index.toInt())) total += f.size.toInt();
    }
    return total;
  }

  void _toggleFile(int idx) {
    setState(() {
      if (_selected.contains(idx)) {
        _selected = Set.from(_selected)..remove(idx);
      } else {
        _selected = Set.from(_selected)..add(idx);
      }
    });
  }

  void _toggleAll() {
    setState(() {
      _selected = _allSelected
          ? <int>{}
          : widget.files.map((f) => f.index.toInt()).toSet();
    });
  }

  void _confirm() {
    if (_noneSelected || _sent) return;
    _sent = true;
    final indices = _selected.toList()..sort();
    SelectBtFiles(
      taskId: widget.taskId,
      selectedIndices: indices,
    ).sendSignalToRust();
    Navigator.of(context).pop();
  }

  void _cancel() {
    if (!_sent) {
      _sent = true;
      SelectBtFiles(
        taskId: widget.taskId,
        selectedIndices: const [-1],
      ).sendSignalToRust();
    }
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final multi = widget.files.length > 1;

    return MobileSheetContainer(
      title: s.btFileSelectTitle,
      footer: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Opacity(
            opacity: _noneSelected ? 0.45 : 1,
            child: MobilePrimaryButton(
              icon: LucideIcons.download,
              label: s.btFileSelectConfirm(
                _selected.length,
                formatBtFileSize(_selectedBytes),
              ),
              onTap: _confirm,
            ),
          ),
          const SizedBox(height: 10),
          MobilePrimaryButton(
            label: s.cancel,
            filled: false,
            onTap: _cancel,
          ),
        ],
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 描述 + （多文件时）全选切换
          Row(
            children: [
              Expanded(
                child: Text(
                  multi
                      ? s.btFileSelectDesc(widget.files.length)
                      : s.btFileSelectDescSingle,
                  style: TextStyle(
                    fontSize: 12.5,
                    height: 1.4,
                    color: c.textMuted,
                  ),
                ),
              ),
              if (multi) ...[
                const SizedBox(width: 8),
                MobileChip(
                  label: s.btFileSelectAll,
                  selected: _allSelected,
                  onTap: _toggleAll,
                ),
              ],
            ],
          ),
          const SizedBox(height: 12),
          for (final file in widget.files)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: _BtFileRow(
                file: file,
                selected: _selected.contains(file.index.toInt()),
                onTap: () => _toggleFile(file.index.toInt()),
                c: c,
              ),
            ),
        ],
      ),
    );
  }
}

/// 单个文件行：玻璃卡片 + 圆形勾选框 + 文件名 + 大小。
class _BtFileRow extends StatelessWidget {
  final BtFileEntry file;
  final bool selected;
  final VoidCallback onTap;
  final AppColors c;

  const _BtFileRow({
    required this.file,
    required this.selected,
    required this.onTap,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final name = file.path.split('/').last;
    return GestureDetector(
      onTap: onTap,
      behavior: HitTestBehavior.opaque,
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 150),
        curve: Curves.easeOut,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 11),
        decoration: BoxDecoration(
          color: selected
              ? m.soft(c.accent)
              : m.glassSubtle(c.surface1),
          borderRadius: m.brChipXl,
          border: Border.all(
            color: selected
                ? m.glassSubtle(c.accent)
                : m.emphasis(c.border),
            width: selected ? 1.4 : 1,
          ),
        ),
        child: Row(
          children: [
            _CheckDot(selected: selected, c: c),
            const SizedBox(width: 11),
            Icon(
              LucideIcons.file,
              size: 16,
              color: selected ? c.accent : c.textMuted,
            ),
            const SizedBox(width: 9),
            Expanded(
              child: Text(
                name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  fontSize: 13.5,
                  fontWeight: FontWeight.w500,
                  color: c.textPrimary,
                ),
              ),
            ),
            const SizedBox(width: 10),
            Text(
              formatBtFileSize(file.size.toInt()),
              style: TextStyle(fontSize: 11.5, color: c.textMuted),
            ),
          ],
        ),
      ),
    );
  }
}

/// 圆形勾选指示。
class _CheckDot extends StatelessWidget {
  final bool selected;
  final AppColors c;

  const _CheckDot({required this.selected, required this.c});

  @override
  Widget build(BuildContext context) {
    return AnimatedContainer(
      duration: const Duration(milliseconds: 150),
      width: 20,
      height: 20,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: selected ? c.accent : const Color(0x00000000),
        shape: BoxShape.circle,
        border: Border.all(
          color: selected ? c.accent : c.border,
          width: 1.5,
        ),
      ),
      child: selected
          ? Icon(LucideIcons.check, size: 13, color: c.accentForeground)
          : null,
    );
  }
}
