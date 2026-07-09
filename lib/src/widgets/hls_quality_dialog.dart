import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../mobile/sheets/mobile_hls_quality_sheet.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

void showHlsQualityDialog(
  BuildContext context, {
  required String taskId,
  required List<HlsQualityOption> options,
}) {
  if (Platform.isAndroid || Platform.isIOS) {
    showMobileHlsQualitySheet(context, taskId: taskId, options: options);
    return;
  }
  showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    barrierDismissible: false,
    animateIn: const [],
    animateOut: const [],
    builder: (context) =>
        _HlsQualityDialogContent(taskId: taskId, options: options),
  );
}

String _formatBandwidth(int bps) {
  if (bps >= 1000000) {
    final mbps = bps / 1000000.0;
    return '${mbps.toStringAsFixed(1)} Mbps';
  }
  if (bps >= 1000) {
    final kbps = bps / 1000.0;
    return '${kbps.toStringAsFixed(0)} Kbps';
  }
  return '$bps bps';
}

String _formatResolution(int w, int h) {
  if (w == 0 || h == 0) return '';
  // Common label shortcuts
  if (h >= 2160) return '4K (${w}x$h)';
  if (h >= 1440) return '2K (${w}x$h)';
  if (h >= 1080) return '1080p (${w}x$h)';
  if (h >= 720) return '720p (${w}x$h)';
  if (h >= 480) return '480p (${w}x$h)';
  if (h >= 360) return '360p (${w}x$h)';
  return '${w}x$h';
}

class _HlsQualityDialogContent extends StatefulWidget {
  final String taskId;
  final List<HlsQualityOption> options;

  const _HlsQualityDialogContent({required this.taskId, required this.options});

  @override
  State<_HlsQualityDialogContent> createState() =>
      _HlsQualityDialogContentState();
}

class _HlsQualityDialogContentState extends State<_HlsQualityDialogContent> {
  late int _selectedIndex;
  bool _selectionSent = false;

  @override
  void initState() {
    super.initState();
    int bestIdx = 0;
    int bestBw = 0;
    for (int i = 0; i < widget.options.length; i++) {
      if (widget.options[i].bandwidth > bestBw) {
        bestBw = widget.options[i].bandwidth.toInt();
        bestIdx = i;
      }
    }
    _selectedIndex = bestIdx;
  }

  @override
  void dispose() {
    if (!_selectionSent) {
      _sendSelection(_selectedIndex);
    }
    super.dispose();
  }

  void _sendSelection(int optionListIndex) {
    if (_selectionSent) return;
    _selectionSent = true;
    final option = widget.options[optionListIndex];
    SelectHlsQuality(
      taskId: widget.taskId,
      selectedIndex: option.index,
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

    // Sort options by bandwidth descending for display
    final sorted = List<int>.generate(widget.options.length, (i) => i);
    sorted.sort(
      (a, b) =>
          widget.options[b].bandwidth.compareTo(widget.options[a].bandwidth),
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
            child: Icon(LucideIcons.settings2, size: 14, color: c.accent),
          ),
          const SizedBox(width: 10),
          Text(s.hlsQualityTitle),
        ],
      ),
      description: Text(s.hlsQualityDesc),
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
            for (final idx in sorted)
              _QualityOptionTile(
                option: widget.options[idx],
                isSelected: idx == _selectedIndex,
                isBest: idx == sorted.first,
                onTap: () => setState(() => _selectedIndex = idx),
                c: c,
              ),
          ],
        ),
      ),
    );
  }
}

class _QualityOptionTile extends StatelessWidget {
  final HlsQualityOption option;
  final bool isSelected;
  final bool isBest;
  final VoidCallback onTap;
  final AppColors c;

  const _QualityOptionTile({
    required this.option,
    required this.isSelected,
    required this.isBest,
    required this.onTap,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final hasResolution = option.width > 0 && option.height > 0;
    final resLabel = hasResolution
        ? _formatResolution(option.width.toInt(), option.height.toInt())
        : '';
    final bwLabel = _formatBandwidth(option.bandwidth.toInt());

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
                  Row(
                    children: [
                      if (hasResolution)
                        Text(
                          resLabel,
                          style: TextStyle(
                            fontSize: 13,
                            fontWeight: FontWeight.w600,
                            color: c.textPrimary,
                          ),
                        ),
                      if (hasResolution) const SizedBox(width: 8),
                      Text(
                        bwLabel,
                        style: TextStyle(
                          fontSize: hasResolution ? 11.5 : 13,
                          fontWeight: hasResolution
                              ? FontWeight.w400
                              : FontWeight.w600,
                          color: hasResolution ? c.textMuted : c.textPrimary,
                        ),
                      ),
                    ],
                  ),
                ],
              ),
            ),
            if (isBest)
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
