import 'package:flutter/material.dart' show Colors;
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../theme/app_colors.dart';

/// 拆分动作按钮：主体动作 + 右侧箭头，融合在同一个按钮外壳内、以细分隔线
/// 区分两个点击区。
///
/// - 主体（分隔线左侧）：执行 [onPressed]；
/// - 箭头（**分隔线起至按钮右缘的全部区域**）：执行 [onPickQueue]，携带
///   anchor context 供调用方在按钮下方定位弹出菜单。
///
/// 命中实现：箭头区不是按钮内嵌手势，而是 Stack 顶层的绝对定位透明手势层
/// ——覆盖分隔线右侧全宽（含按钮自身右内边距），后插入者优先命中，
/// 不存在「点到按钮右内边距落入主动作」的缝隙。几何由本组件自控的
/// padding 常量精确锁定。
///
/// [primary] = true 用实心主按钮样式（accent 底白字），false 用 outline。
class SplitActionButton extends StatelessWidget {
  final bool primary;
  final bool enabled;
  final IconData icon;
  final String label;
  final String tooltip;
  final VoidCallback onPressed;
  final void Function(BuildContext anchor) onPickQueue;

  const SplitActionButton({
    super.key,
    this.primary = false,
    this.enabled = true,
    required this.icon,
    required this.label,
    required this.tooltip,
    required this.onPressed,
    required this.onPickQueue,
  });

  // 自控几何（与下方 Row 布局一一对应），保证覆盖层宽度精确。
  static const double _leftPad = 16;
  static const double _dividerWidth = 1;
  static const double _chevronGap = 6;
  static const double _chevronSize = 13;
  static const double _rightPad = 10;

  /// 覆盖命中区宽度：分隔线 + 间距 + 箭头 + 右内边距。
  static const double _pickZoneWidth =
      _dividerWidth + _chevronGap + _chevronSize + _rightPad;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final fg = primary ? Colors.white : c.textSecondary;
    final divider = primary ? Colors.white.withValues(alpha: 0.35) : c.border;
    final labelStyle = primary ? const TextStyle(color: Colors.white) : null;

    final child = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 13, color: fg),
        const SizedBox(width: 6),
        Text(label, style: labelStyle),
        const SizedBox(width: 10),
        // 分隔线与箭头是纯视觉；命中由 Stack 覆盖层负责。
        Container(width: _dividerWidth, height: 14, color: divider),
        const SizedBox(width: _chevronGap),
        Icon(LucideIcons.chevronDown, size: _chevronSize, color: fg),
      ],
    );

    final button = primary
        ? ShadButton(
            onPressed: enabled ? onPressed : null,
            padding: const EdgeInsets.only(left: _leftPad, right: _rightPad),
            child: child,
          )
        : ShadButton.outline(
            onPressed: enabled ? onPressed : null,
            padding: const EdgeInsets.only(left: _leftPad, right: _rightPad),
            child: child,
          );

    // 禁用态视觉降级：ShadButton 置 null onPressed 只拦截交互不改外观
    // （子内容颜色是本组件显式指定的），不降透明度会呈现「看似可点但
    // 点不动」的假可用态（2026-07-19 manifest 弹窗 0 选中反馈）。
    final body = Stack(
        children: [
          button,
          // 覆盖命中区：分隔线右侧（含右内边距）全部触发下拉。
          Positioned(
            top: 0,
            bottom: 0,
            right: 0,
            width: _pickZoneWidth,
            child: Builder(
              builder: (anchor) => GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: enabled ? () => onPickQueue(anchor) : null,
                child: MouseRegion(
                  cursor: enabled
                      ? SystemMouseCursors.click
                      : SystemMouseCursors.basic,
                  child: const SizedBox.expand(),
                ),
              ),
            ),
          ),
        ],
    );

    return ShadTooltip(
      builder: (_) => Text(tooltip),
      child: enabled ? body : Opacity(opacity: 0.5, child: body),
    );
  }
}
