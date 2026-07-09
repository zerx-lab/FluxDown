import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../../bindings/bindings.dart';
import '../../i18n/locale_provider.dart';
import '../../theme/app_colors.dart';
import '../../theme/app_metrics.dart';
import '../mobile_ui.dart';

/// 移动端 HLS 画质选择底部弹层（Liquid Glass 风格）。
///
/// 与桌面 `showHlsQualityDialog` 语义一致：关闭前若未显式确认，
/// 按当前选中项（默认最高码率）发送 [SelectHlsQuality]。
Future<void> showMobileHlsQualitySheet(
  BuildContext context, {
  required String taskId,
  required List<HlsQualityOption> options,
}) {
  return showMobileSheet<void>(
    context,
    builder: (ctx) => _HlsQualitySheet(taskId: taskId, options: options),
  );
}

String _formatBandwidth(int bps) {
  if (bps >= 1000000) return '${(bps / 1000000).toStringAsFixed(1)} Mbps';
  if (bps >= 1000) return '${(bps / 1000).toStringAsFixed(0)} Kbps';
  return '$bps bps';
}

String _formatResolution(int w, int h) {
  if (w == 0 || h == 0) return '';
  if (h >= 2160) return '4K (${w}x$h)';
  if (h >= 1440) return '2K (${w}x$h)';
  if (h >= 1080) return '1080p (${w}x$h)';
  if (h >= 720) return '720p (${w}x$h)';
  if (h >= 480) return '480p (${w}x$h)';
  if (h >= 360) return '360p (${w}x$h)';
  return '${w}x$h';
}

class _HlsQualitySheet extends StatefulWidget {
  final String taskId;
  final List<HlsQualityOption> options;

  const _HlsQualitySheet({required this.taskId, required this.options});

  @override
  State<_HlsQualitySheet> createState() => _HlsQualitySheetState();
}

class _HlsQualitySheetState extends State<_HlsQualitySheet> {
  late int _selectedIndex;
  bool _sent = false;

  @override
  void initState() {
    super.initState();
    var bestIdx = 0;
    var bestBw = 0;
    for (var i = 0; i < widget.options.length; i++) {
      if (widget.options[i].bandwidth > bestBw) {
        bestBw = widget.options[i].bandwidth.toInt();
        bestIdx = i;
      }
    }
    _selectedIndex = bestIdx;
  }

  @override
  void dispose() {
    if (!_sent) _send(_selectedIndex);
    super.dispose();
  }

  void _send(int listIndex) {
    if (_sent) return;
    _sent = true;
    SelectHlsQuality(
      taskId: widget.taskId,
      selectedIndex: widget.options[listIndex].index,
    ).sendSignalToRust();
  }

  void _confirm() {
    _send(_selectedIndex);
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    final sorted = List<int>.generate(widget.options.length, (i) => i)
      ..sort(
        (a, b) =>
            widget.options[b].bandwidth.compareTo(widget.options[a].bandwidth),
      );

    return MobileSheetContainer(
      title: s.hlsQualityTitle,
      footer: MobilePrimaryButton(
        icon: LucideIcons.download,
        label: s.confirm,
        onTap: _confirm,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(bottom: 12),
            child: Text(
              s.hlsQualityDesc,
              style: TextStyle(fontSize: 12.5, height: 1.4, color: c.textMuted),
            ),
          ),
          for (final idx in sorted)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: _QualityRow(
                option: widget.options[idx],
                selected: idx == _selectedIndex,
                isBest: idx == sorted.first,
                onTap: () => setState(() => _selectedIndex = idx),
                c: c,
              ),
            ),
        ],
      ),
    );
  }
}

class _QualityRow extends StatelessWidget {
  final HlsQualityOption option;
  final bool selected;
  final bool isBest;
  final VoidCallback onTap;
  final AppColors c;

  const _QualityRow({
    required this.option,
    required this.selected,
    required this.isBest,
    required this.onTap,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final hasRes = option.width > 0 && option.height > 0;
    final resLabel = hasRes
        ? _formatResolution(option.width.toInt(), option.height.toInt())
        : '';
    final bwLabel = _formatBandwidth(option.bandwidth.toInt());

    return GestureDetector(
      onTap: onTap,
      behavior: HitTestBehavior.opaque,
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 150),
        curve: Curves.easeOut,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 12),
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
            _RadioDot(selected: selected, c: c),
            const SizedBox(width: 12),
            Expanded(
              child: Row(
                children: [
                  if (hasRes) ...[
                    Text(
                      resLabel,
                      style: TextStyle(
                        fontSize: 13.5,
                        fontWeight: FontWeight.w600,
                        color: c.textPrimary,
                      ),
                    ),
                    const SizedBox(width: 8),
                  ],
                  Text(
                    bwLabel,
                    style: TextStyle(
                      fontSize: hasRes ? 11.5 : 13.5,
                      fontWeight: hasRes ? FontWeight.w400 : FontWeight.w600,
                      color: hasRes ? c.textMuted : c.textPrimary,
                    ),
                  ),
                ],
              ),
            ),
            if (isBest)
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
                decoration: BoxDecoration(
                  color: m.mutedStrong(c.accent),
                  borderRadius: m.brMd,
                ),
                child: Text(
                  'Best',
                  style: TextStyle(
                    fontSize: 10,
                    fontWeight: FontWeight.w700,
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

class _RadioDot extends StatelessWidget {
  final bool selected;
  final AppColors c;

  const _RadioDot({required this.selected, required this.c});

  @override
  Widget build(BuildContext context) {
    return AnimatedContainer(
      duration: const Duration(milliseconds: 150),
      width: 20,
      height: 20,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: selected ? c.accent : const Color(0x00000000),
        border: Border.all(
          color: selected ? c.accent : c.border,
          width: 1.5,
        ),
      ),
      child: selected
          ? Container(
              width: 7,
              height: 7,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: c.accentForeground,
              ),
            )
          : null,
    );
  }
}
