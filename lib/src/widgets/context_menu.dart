import 'package:flutter/material.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

/// 菜单项数据
class ContextMenuItem {
  final IconData icon;
  final String label;
  final Color color;
  final VoidCallback action;

  /// 是否可用。false 时置灰显示且不响应点击。
  final bool enabled;

  const ContextMenuItem({
    required this.icon,
    required this.label,
    required this.color,
    required this.action,
    this.enabled = true,
  });
}

/// 在指定位置弹出自定义 Overlay 右键菜单（不依赖 MaterialLocalizations）。
///
/// [items] 菜单项列表。
/// [dividerAfterIndices] 在哪些 index 后面插入分隔线。
/// [menuWidth] 是宽度下限：实际宽度按最长标签实测自适应（clamp 到 320，
/// 超出部分由菜单项内的省略号兜底），长文案（如「开始下载到 · 队列名」）
/// 不再横向溢出。
void showContextMenu(
  BuildContext context,
  Offset globalPosition, {
  required List<ContextMenuItem> items,
  Set<int> dividerAfterIndices = const {},
  double menuWidth = 180.0,
}) {
  // 标签实测宽度（样式对齐 _ContextMenuItemWidget：fontSize 13）。
  // 水平开销 = 外边距 4×2 + 内边距 12×2 + 图标 15 + 间距 10 + 边框 1×2
  //          = 59，另加 8px 字体度量安全余量（TextPainter 用默认字族测量，
  //          与实际渲染字族可能有轻微差异，省略号兜底残余误差）。
  var maxLabel = 0.0;
  final painter = TextPainter(textDirection: TextDirection.ltr);
  for (final item in items) {
    painter.text = TextSpan(
      text: item.label,
      style: const TextStyle(fontSize: 13),
    );
    painter.layout();
    if (painter.width > maxLabel) maxLabel = painter.width;
  }
  // 只增不减：调用方的 menuWidth 是下限；自适应增宽封顶 320（更长文案由
  // 省略号兜底）。不用 clamp(menuWidth, 320)——调用方传 >320 会因下限>上限抛错。
  final fitted = maxLabel + 59 + 8;
  if (fitted > menuWidth) menuWidth = fitted > 320.0 ? 320.0 : fitted;
  final overlay = Overlay.of(context);
  final c = AppColors.of(context);

  const itemHeight = 36.0;
  const dividerHeight = 9.0;

  final menuHeight =
      items.length * itemHeight +
      dividerAfterIndices.length * dividerHeight +
      8; // vertical padding

  final screenSize = MediaQuery.of(context).size;
  double left = globalPosition.dx;
  double top = globalPosition.dy;

  if (left + menuWidth > screenSize.width) {
    left = screenSize.width - menuWidth - 4;
  }
  if (top + menuHeight > screenSize.height) {
    top = screenSize.height - menuHeight - 4;
  }

  late OverlayEntry entry;
  entry = OverlayEntry(
    builder: (_) => _ContextMenuOverlay(
      left: left,
      top: top,
      menuWidth: menuWidth,
      itemHeight: itemHeight,
      colors: c,
      items: items,
      dividerAfterIndices: dividerAfterIndices,
      onDismiss: () => entry.remove(),
    ),
  );
  overlay.insert(entry);
}

// =============================================================================
// 内部实现
// =============================================================================

class _ContextMenuOverlay extends StatelessWidget {
  final double left;
  final double top;
  final double menuWidth;
  final double itemHeight;
  final AppColors colors;
  final List<ContextMenuItem> items;
  final Set<int> dividerAfterIndices;
  final VoidCallback onDismiss;

  const _ContextMenuOverlay({
    required this.left,
    required this.top,
    required this.menuWidth,
    required this.itemHeight,
    required this.colors,
    required this.items,
    required this.dividerAfterIndices,
    required this.onDismiss,
  });

  @override
  Widget build(BuildContext context) {
    return Stack(
      children: [
        // 全屏透明遮罩 — 点击/右键任意区域关闭菜单
        Positioned.fill(
          child: GestureDetector(
            onTap: onDismiss,
            onSecondaryTap: onDismiss,
            behavior: HitTestBehavior.opaque,
            child: const ColoredBox(color: Colors.transparent),
          ),
        ),
        // 菜单面板
        Positioned(left: left, top: top, child: _buildMenu(context)),
      ],
    );
  }

  Widget _buildMenu(BuildContext context) {
    final children = <Widget>[];
    final m = AppMetrics.of(context);
    for (var i = 0; i < items.length; i++) {
      children.add(
        _ContextMenuItemWidget(
          item: items[i],
          itemHeight: itemHeight,
          colors: colors,
          onTap: items[i].enabled
              ? () {
                  onDismiss();
                  items[i].action();
                }
              : null,
        ),
      );
      if (dividerAfterIndices.contains(i)) {
        children.add(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
            child: Divider(height: 1, thickness: 1, color: colors.border),
          ),
        );
      }
    }

    return Container(
      width: menuWidth,
      padding: const EdgeInsets.symmetric(vertical: 4),
      decoration: BoxDecoration(
        color: colors.surface1,
        borderRadius: m.brCard,
        border: Border.all(color: colors.border, width: 1),
        boxShadow: [
          BoxShadow(
            color: m.muted(Colors.black),
            blurRadius: 12,
            offset: const Offset(0, 4),
          ),
        ],
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: children,
      ),
    );
  }
}

class _ContextMenuItemWidget extends StatefulWidget {
  final ContextMenuItem item;
  final double itemHeight;
  final AppColors colors;
  final VoidCallback? onTap;

  const _ContextMenuItemWidget({
    required this.item,
    required this.itemHeight,
    required this.colors,
    required this.onTap,
  });

  @override
  State<_ContextMenuItemWidget> createState() => _ContextMenuItemWidgetState();
}

class _ContextMenuItemWidgetState extends State<_ContextMenuItemWidget> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final enabled = widget.item.enabled;
    final color = enabled ? widget.item.color : widget.colors.textMuted;
    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      child: GestureDetector(
        // opaque：整行（含文字右侧空白）都可点击，不留命中死区。
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        child: Container(
          height: widget.itemHeight,
          padding: const EdgeInsets.symmetric(horizontal: 12),
          margin: const EdgeInsets.symmetric(horizontal: 4),
          decoration: BoxDecoration(
            color: enabled && _isHovered
                ? widget.colors.hoverBg
                : Colors.transparent,
            borderRadius: m.brSm,
          ),
          child: Row(
            children: [
              Icon(widget.item.icon, size: 15, color: color),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  widget.item.label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 13, color: color),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
