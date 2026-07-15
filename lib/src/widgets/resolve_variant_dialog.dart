import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

void showResolveVariantDialog(
  BuildContext context, {
  required String taskId,
  required int defaultIndex,
  required List<ResolveVariantOption> options,
}) {
  showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    barrierDismissible: false,
    animateIn: const [],
    animateOut: const [],
    builder: (context) => _ResolveVariantDialogContent(
      taskId: taskId,
      defaultIndex: defaultIndex,
      options: options,
    ),
  );
}

String _formatBandwidth(int bps) {
  if (bps <= 0) return '';
  if (bps >= 1000000) return '${(bps / 1000000).toStringAsFixed(1)} Mbps';
  if (bps >= 1000) return '${(bps / 1000).toStringAsFixed(0)} Kbps';
  return '$bps bps';
}

String _formatResolution(int w, int h) {
  if (w <= 0 || h <= 0) return '';
  return '${w}x$h';
}

class _ResolveVariantDialogContent extends StatefulWidget {
  final String taskId;
  final int defaultIndex;
  final List<ResolveVariantOption> options;

  const _ResolveVariantDialogContent({
    required this.taskId,
    required this.defaultIndex,
    required this.options,
  });

  @override
  State<_ResolveVariantDialogContent> createState() =>
      _ResolveVariantDialogContentState();
}

class _ResolveVariantDialogContentState
    extends State<_ResolveVariantDialogContent> {
  late int _selectedIndex;
  bool _selectionSent = false;

  @override
  void initState() {
    super.initState();
    final idx = widget.options.indexWhere(
      (o) => o.index == widget.defaultIndex,
    );
    _selectedIndex = idx >= 0 ? idx : 0;
  }

  @override
  void dispose() {
    // 未点「确定」就关闭（右上角 X / 程序性 pop）→ 发送取消哨兵 -1，
    // 宿主据此取消任务，而非回退默认变体继续下载。
    if (!_selectionSent) {
      _sendCancel();
    }
    super.dispose();
  }

  void _sendSelection(int optionListIndex) {
    if (_selectionSent) return;
    _selectionSent = true;
    final option = widget.options[optionListIndex];
    SelectResolveVariant(
      taskId: widget.taskId,
      selectedIndex: option.index,
    ).sendSignalToRust();
  }

  void _sendCancel() {
    if (_selectionSent) return;
    _selectionSent = true;
    SelectResolveVariant(
      taskId: widget.taskId,
      selectedIndex: -1,
    ).sendSignalToRust();
  }

  void _onConfirm() {
    _sendSelection(_selectedIndex);
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    final defaultListIndex = widget.options.indexWhere(
      (o) => o.index == widget.defaultIndex,
    );

    return ShadDialog(
      title: Row(
        children: [
          Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: c.accent.withValues(alpha: 0.1),
              borderRadius: BorderRadius.circular(6),
            ),
            child: Icon(LucideIcons.listVideo, size: 14, color: c.accent),
          ),
          const SizedBox(width: 10),
          Text(s.resolveVariantTitle),
        ],
      ),
      description: Text(s.resolveVariantDesc),
      actions: [
        ShadButton(
          onPressed: _onConfirm,
          child: Text(
            s.confirm,
            style: const TextStyle(color: Color(0xFFFFFFFF)),
          ),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 12),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            for (int idx = 0; idx < widget.options.length; idx++)
              _VariantOptionTile(
                option: widget.options[idx],
                isSelected: idx == _selectedIndex,
                isDefault: idx == defaultListIndex,
                onTap: () => setState(() => _selectedIndex = idx),
                c: c,
              ),
          ],
        ),
      ),
    );
  }
}

class _VariantOptionTile extends StatelessWidget {
  final ResolveVariantOption option;
  final bool isSelected;
  final bool isDefault;
  final VoidCallback onTap;
  final AppColors c;

  const _VariantOptionTile({
    required this.option,
    required this.isSelected,
    required this.isDefault,
    required this.onTap,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final parts = <String>[
      if (option.container.isNotEmpty) option.container,
      _formatResolution(option.width.toInt(), option.height.toInt()),
      _formatBandwidth(option.bandwidth.toInt()),
      if (option.totalBytes > 0) DownloadTask.formatBytes(option.totalBytes.toInt()),
    ].where((p) => p.isNotEmpty).toList();
    final subLabel = parts.join(' · ');

    return GestureDetector(
      onTap: onTap,
      child: Container(
        margin: const EdgeInsets.only(bottom: 6),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        decoration: BoxDecoration(
          color: isSelected ? m.subtle(c.accent) : c.surface1,
          borderRadius: m.brCard,
          border: Border.all(
            color: isSelected ? c.accent : c.border,
            width: isSelected ? 1.5 : 1,
          ),
        ),
        child: Row(
          children: [
            Container(
              width: 18,
              height: 18,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                border: Border.all(
                  color: isSelected ? c.accent : c.textMuted,
                  width: isSelected ? 5 : 1.5,
                ),
                color: isSelected ? c.accent : null,
              ),
              child: isSelected
                  ? Center(
                      child: Container(
                        width: 6,
                        height: 6,
                        decoration: const BoxDecoration(
                          shape: BoxShape.circle,
                          color: Color(0xFFFFFFFF),
                        ),
                      ),
                    )
                  : null,
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    option.label,
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                      color: c.textPrimary,
                    ),
                  ),
                  if (subLabel.isNotEmpty) ...[
                    const SizedBox(height: 2),
                    Text(
                      subLabel,
                      style: TextStyle(
                        fontSize: 11.5,
                        fontWeight: FontWeight.w400,
                        color: c.textMuted,
                      ),
                    ),
                  ],
                ],
              ),
            ),
            if (isDefault)
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(
                  color: m.muted(c.accent),
                  borderRadius: m.brSm,
                ),
                child: Text(
                  'Best',
                  style: TextStyle(
                    fontSize: 10,
                    fontWeight: FontWeight.w600,
                    color: c.accent,
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}
