import 'dart:math' as math;
import 'dart:ui';

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../models/download_task.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

/// 顶栏 / Dock 通用毛玻璃滤镜
final ImageFilter mobileBlurFilter = ImageFilter.blur(sigmaX: 22, sigmaY: 22);

/// 将任务的分段下载区间映射到 [cells] 个可视化格子的填充率 [0,1]。
///
/// 规则：
/// - 已完成任务 → 全部填满；
/// - 总大小未知 → 全部为 0；
/// - 无分段信息 → 按整体进度做前缀填充；
/// - 有分段信息 → 每个分段的已下载区间 `[startByte, startByte+downloadedBytes)`
///   按字节比例累加到重叠的格子上（结果 clamp 到 [0,1]）。
List<double> mobileSegmentCellFills(DownloadTask task, int cells) {
  if (task.status == TaskStatus.completed) {
    return List.filled(cells, 1.0);
  }
  final total = task.totalBytes;
  if (total <= 0) return List.filled(cells, 0.0);

  final segments = task.segments;
  if (segments == null || segments.isEmpty) {
    // 无分段信息 → 按整体进度前缀填充
    final filled = task.progress * cells;
    return List.generate(cells, (i) => (filled - i).clamp(0.0, 1.0));
  }

  final fills = List.filled(cells, 0.0);
  final cellSize = total / cells;
  for (final seg in segments) {
    if (seg.downloadedBytes <= 0) continue;
    final start = seg.startByte.toDouble();
    final end = start + seg.downloadedBytes.toDouble();
    final firstCell = (start / cellSize).floor().clamp(0, cells - 1);
    final lastCell = ((end - 1) / cellSize).floor().clamp(0, cells - 1);
    for (var i = firstCell; i <= lastCell; i++) {
      final cellStart = i * cellSize;
      final cellEnd = cellStart + cellSize;
      final overlap =
          (math.min(end, cellEnd) - math.max(start, cellStart)) / cellSize;
      fills[i] = (fills[i] + overlap).clamp(0.0, 1.0);
    }
  }
  return fills;
}

/// 文件分类 → Lucide 图标
IconData mobileCategoryIcon(FileCategory category) {
  return switch (category) {
    FileCategory.video => LucideIcons.film,
    FileCategory.audio => LucideIcons.music,
    FileCategory.document => LucideIcons.fileText,
    FileCategory.image => LucideIcons.image,
    FileCategory.archive => LucideIcons.archive,
    FileCategory.all => LucideIcons.layoutGrid,
    FileCategory.other => LucideIcons.file,
  };
}

/// 轻量 Toast（复用 ShadSonner）
void showMobileToast(BuildContext context, String message) {
  ShadSonner.of(context).show(
    ShadToast(
      alignment: Alignment.topCenter,
      title: Text(message, maxLines: 2, overflow: TextOverflow.ellipsis),
      duration: const Duration(milliseconds: 2000),
    ),
  );
}

/// 玻璃卡片装饰（浅色: 白面板；深色: 深面板）
BoxDecoration mobileCardDecoration(AppColors c, AppMetrics m) {
  return BoxDecoration(
    color: c.surface1,
    borderRadius: m.brMobileCard,
    border: Border.all(color: c.border),
    boxShadow: [
      BoxShadow(
        color: c.shadow.withValues(alpha: 0.05),
        blurRadius: 3,
        offset: const Offset(0, 1),
      ),
    ],
  );
}

/// 胶囊 Chip（筛选 / 线程数 / 队列选择）
class MobileChip extends StatelessWidget {
  final String label;
  final bool selected;
  final VoidCallback onTap;

  const MobileChip({
    super.key,
    required this.label,
    required this.selected,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return GestureDetector(
      onTap: onTap,
      child: Container(
        alignment: Alignment.center,
        padding: const EdgeInsets.symmetric(horizontal: 13, vertical: 6),
        decoration: BoxDecoration(
          color: selected ? m.soft(c.accent) : c.surface1,
          borderRadius: m.brPill,
          border: Border.all(
            color: selected ? m.selectedBorder(c.accent) : c.border,
          ),
        ),
        child: Text(
          label,
          textAlign: TextAlign.center,
          style: TextStyle(
            fontSize: 12.5,
            height: 1.0,
            fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
            color: selected ? c.accent : c.textSecondary,
          ),
        ),
      ),
    );
  }
}

/// 单行分段选择器（iOS Segmented Control 风格）：所有选项等分一行，
/// 紧凑不换行。用于线程数等固定枚举选择。
class MobileSegmentedRow extends StatelessWidget {
  final List<String> options;
  final List<String> labels;
  final String selected;
  final ValueChanged<String> onSelect;

  const MobileSegmentedRow({
    super.key,
    required this.options,
    required this.labels,
    required this.selected,
    required this.onSelect,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Container(
      padding: const EdgeInsets.all(3),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brChipLg,
        border: Border.all(color: c.border),
      ),
      child: Row(
        children: [
          for (var i = 0; i < options.length; i++)
            Expanded(
              child: GestureDetector(
                onTap: () => onSelect(options[i]),
                behavior: HitTestBehavior.opaque,
                child: AnimatedContainer(
                  duration: const Duration(milliseconds: 150),
                  curve: Curves.easeOut,
                  height: 32,
                  alignment: Alignment.center,
                  decoration: BoxDecoration(
                    color: selected == options[i]
                        ? c.accent
                        : const Color(0x00000000),
                    borderRadius: m.brIconTile,
                  ),
                  child: Text(
                    labels[i],
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: selected == options[i]
                          ? FontWeight.w700
                          : FontWeight.w500,
                      color: selected == options[i]
                          ? c.accentForeground
                          : c.textSecondary,
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// 圆形图标按钮（顶栏）
class MobileIconButton extends StatelessWidget {
  final IconData icon;
  final VoidCallback onTap;
  final bool showDot;
  final Color? color;

  const MobileIconButton({
    super.key,
    required this.icon,
    required this.onTap,
    this.showDot = false,
    this.color,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: SizedBox(
        width: 40,
        height: 40,
        child: Stack(
          alignment: Alignment.center,
          children: [
            Icon(icon, size: 19, color: color ?? c.textPrimary),
            if (showDot)
              Positioned(
                top: 7,
                right: 7,
                child: Container(
                  width: 7,
                  height: 7,
                  decoration: BoxDecoration(
                    color: c.accent,
                    shape: BoxShape.circle,
                    border: Border.all(color: c.surface1, width: 1.5),
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// 进度条（细，圆角）
class MobileProgressBar extends StatelessWidget {
  final double progress;
  final Color color;
  final double height;

  const MobileProgressBar({
    super.key,
    required this.progress,
    required this.color,
    this.height = 5,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return ClipRRect(
      borderRadius: m.brPill,
      child: SizedBox(
        height: height,
        child: Stack(
          children: [
            Container(color: c.switchTrack),
            FractionallySizedBox(
              widthFactor: progress.clamp(0.0, 1.0),
              child: Container(color: color),
            ),
          ],
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 底部弹层（Liquid Glass 风格）
// ─────────────────────────────────────────────

/// 从底部滑入的弹层。[builder] 构建内容（置于玻璃容器内）。
Future<T?> showMobileSheet<T>(
  BuildContext context, {
  required WidgetBuilder builder,
}) {
  return showGeneralDialog<T>(
    context: context,
    barrierDismissible: true,
    barrierLabel: 'mobile-sheet',
    barrierColor: const Color(0x59000000),
    transitionDuration: const Duration(milliseconds: 320),
    pageBuilder: (ctx, _, _) {
      return Align(
        alignment: Alignment.bottomCenter,
        child: AnimatedPadding(
          duration: const Duration(milliseconds: 150),
          curve: Curves.easeOut,
          padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(ctx).bottom),
          child: builder(ctx),
        ),
      );
    },
    transitionBuilder: (ctx, anim, _, child) {
      final curved = CurvedAnimation(
        parent: anim,
        curve: const Cubic(0.32, 0.72, 0.32, 1),
        reverseCurve: Curves.easeIn,
      );
      return SlideTransition(
        position: Tween<Offset>(
          begin: const Offset(0, 1),
          end: Offset.zero,
        ).animate(curved),
        child: child,
      );
    },
  );
}

/// 弹层玻璃容器：圆角顶部 + 毛玻璃 + 抓手 + 标题
class MobileSheetContainer extends StatelessWidget {
  final String? title;
  final Widget child;

  /// 标题行右侧动作（如「重置」文字按钮），与标题同一行、垂直居中。
  final Widget? trailing;

  /// 固定在底部的页脚（如「开始下载」按钮），不随内容滚动。
  final Widget? footer;

  const MobileSheetContainer({
    super.key,
    this.title,
    required this.child,
    this.trailing,
    this.footer,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final media = MediaQuery.of(context);
    final maxHeight = media.size.height * 0.86;
    return ClipRRect(
      borderRadius: m.brSheetTop,
      child: BackdropFilter(
        filter: ImageFilter.blur(sigmaX: 28, sigmaY: 28),
        child: Container(
          width: double.infinity,
          constraints: BoxConstraints(maxHeight: maxHeight),
          decoration: BoxDecoration(
            color: m.glass(c.bg),
            border: Border(top: BorderSide(color: m.borderStrong(c.border))),
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              // 抓手
              Center(
                child: Container(
                  width: 40,
                  height: 4.5,
                  margin: const EdgeInsets.only(top: 10, bottom: 2),
                  decoration: BoxDecoration(
                    color: c.switchTrack,
                    borderRadius: m.brSm,
                  ),
                ),
              ),
              if (title != null)
                Padding(
                  padding: const EdgeInsets.fromLTRB(20, 10, 20, 4),
                  child: Row(
                    children: [
                      Expanded(
                        child: Text(
                          title!,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: 17,
                            fontWeight: FontWeight.w700,
                            color: c.textPrimary,
                          ),
                        ),
                      ),
                      ?trailing,
                    ],
                  ),
                ),
              Flexible(
                child: SingleChildScrollView(
                  padding: EdgeInsets.fromLTRB(
                    20,
                    6,
                    20,
                    footer != null ? 10 : 30 + media.padding.bottom,
                  ),
                  child: child,
                ),
              ),
              if (footer != null)
                Container(
                  width: double.infinity,
                  padding: EdgeInsets.fromLTRB(
                    20,
                    12,
                    20,
                    16 + media.padding.bottom,
                  ),
                  decoration: BoxDecoration(
                    border: Border(top: BorderSide(color: c.border)),
                  ),
                  child: footer!,
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 弹层字段小标题
class MobileFieldLabel extends StatelessWidget {
  final String text;

  const MobileFieldLabel(this.text, {super.key});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Padding(
      padding: const EdgeInsets.only(top: 14, bottom: 8, left: 2, right: 2),
      child: Text(
        text,
        style: TextStyle(
          fontSize: 12,
          fontWeight: FontWeight.w600,
          color: c.textMuted,
        ),
      ),
    );
  }
}

/// 统一样式文本输入框：玻璃填充 + 描边 + 圆角，避免与背景融合。
/// [suffix] 叠加在右下角（如粘贴按钮）。
class MobileTextField extends StatelessWidget {
  final TextEditingController controller;
  final String? placeholder;
  final int maxLines;
  final bool dense;
  final FocusNode? focusNode;
  final ValueChanged<String>? onChanged;
  final Widget? suffix;

  const MobileTextField({
    super.key,
    required this.controller,
    this.placeholder,
    this.maxLines = 1,
    this.dense = false,
    this.focusNode,
    this.onChanged,
    this.suffix,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final single = maxLines == 1;
    final input = ShadInput(
      controller: controller,
      focusNode: focusNode,
      maxLines: maxLines,
      onChanged: onChanged,
      alignment: single ? Alignment.centerLeft : Alignment.topLeft,
      placeholderAlignment: single ? Alignment.centerLeft : Alignment.topLeft,
      padding: single
          ? const EdgeInsets.symmetric(horizontal: 12)
          : const EdgeInsets.fromLTRB(12, 10, 12, 10),
      constraints: single ? const BoxConstraints() : null,
      decoration: ShadDecoration.none,
      style: TextStyle(fontSize: 13, height: 1.2, color: c.textPrimary),
      placeholder: placeholder != null
          ? Text(
              placeholder!,
              style: TextStyle(color: c.textMuted, fontSize: 13, height: 1.2),
            )
          : null,
    );
    final Widget field = single
        ? SizedBox(
            height: dense ? 38 : 44,
            child: Align(alignment: Alignment.centerLeft, child: input),
          )
        : input;
    return Container(
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brChipLg,
        border: Border.all(color: c.border),
      ),
      child: suffix == null
          ? field
          : Stack(
              children: [
                field,
                Positioned(right: 8, bottom: 8, child: suffix!),
              ],
            ),
    );
  }
}

/// 主按钮（胶囊，accent 填充）
class MobilePrimaryButton extends StatelessWidget {
  final String label;
  final IconData? icon;
  final VoidCallback onTap;
  final bool destructive;
  final bool filled;

  const MobilePrimaryButton({
    super.key,
    required this.label,
    this.icon,
    required this.onTap,
    this.destructive = false,
    this.filled = true,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final Color fg;
    final Color bgColor;
    final Color borderColor;
    if (destructive && filled) {
      fg = c.accentForeground;
      bgColor = c.statusError;
      borderColor = c.statusError;
    } else if (destructive) {
      fg = c.statusError;
      bgColor = const Color(0x00000000);
      borderColor = m.borderFade(c.statusError);
    } else if (filled) {
      fg = c.accentForeground;
      bgColor = c.accent;
      borderColor = c.accent;
    } else {
      fg = c.textPrimary;
      bgColor = const Color(0x00000000);
      borderColor = c.border;
    }
    return GestureDetector(
      onTap: onTap,
      child: Container(
        height: 46,
        alignment: Alignment.center,
        decoration: BoxDecoration(
          color: bgColor,
          borderRadius: m.brChipXl,
          border: Border.all(color: borderColor),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (icon != null) ...[
              Icon(icon, size: 16, color: fg),
              const SizedBox(width: 7),
            ],
            Text(
              label,
              style: TextStyle(
                fontSize: 14.5,
                fontWeight: FontWeight.w700,
                color: fg,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 通用二次确认底部弹层（Liquid Glass 风格）。
///
/// 返回 `true` 表示用户点击了确认按钮；取消 / 划走 / 点遮罩返回 `null`。
/// [destructive] 为 true 时确认按钮渲染为红色危险样式。
Future<bool?> showMobileConfirm(
  BuildContext context, {
  required String title,
  required String message,
  required String confirmLabel,
  required String cancelLabel,
  IconData? confirmIcon,
  bool destructive = false,
}) {
  return showMobileSheet<bool>(
    context,
    builder: (ctx) {
      final c = AppColors.of(ctx);
      return MobileSheetContainer(
        title: title,
        footer: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            MobilePrimaryButton(
              icon: confirmIcon,
              label: confirmLabel,
              destructive: destructive,
              filled: true,
              onTap: () => Navigator.of(ctx).pop(true),
            ),
            const SizedBox(height: 10),
            MobilePrimaryButton(
              label: cancelLabel,
              filled: false,
              onTap: () => Navigator.of(ctx).pop(),
            ),
          ],
        ),
        child: Padding(
          padding: const EdgeInsets.only(top: 2, bottom: 4),
          child: Text(
            message,
            style: TextStyle(fontSize: 13, height: 1.5, color: c.textMuted),
          ),
        ),
      );
    },
  );
}
