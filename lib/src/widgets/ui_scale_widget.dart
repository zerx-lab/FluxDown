import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

// ─────────────────────────────────────────────
// 界面缩放 RenderObject
// ─────────────────────────────────────────────

/// 自定义缩放容器，统一处理布局约束、绘制变换和 hit testing。
///
/// 与 [FractionallySizedBox] + Transform.scale 组合不同，
/// 此 RenderObject 的布局大小始终等于父级约束（全屏），
/// 保证 hit test 覆盖整个屏幕；同时将子树约束缩小为
/// `constraints / scale`，让子树在逻辑尺寸下布局，
/// 绘制时再统一放大，视觉上正好填满屏幕。
class UiScaleWidget extends SingleChildRenderObjectWidget {
  final double scale;

  const UiScaleWidget({super.key, required this.scale, required super.child});

  @override
  RenderObject createRenderObject(BuildContext context) {
    return RenderUiScale(scale: scale);
  }

  @override
  void updateRenderObject(BuildContext context, RenderUiScale renderObject) {
    renderObject.scale = scale;
  }
}

class RenderUiScale extends RenderProxyBox {
  double _scale;

  RenderUiScale({required double scale}) : _scale = scale;

  double get scale => _scale;

  set scale(double value) {
    if (_scale == value) return;
    _scale = value;
    markNeedsLayout();
    // 自身 size 恒为 constraints.biggest，布局后不会因尺寸变化自动重绘；
    // 子树内的 repaint boundary 会挡住子级重绘向上冒泡，若不显式标脏，
    // paint() 中的 TransformLayer 会沿用旧缩放矩阵，导致画面与布局/命中测试错位。
    markNeedsPaint();
  }

  Matrix4 get _paintTransform => Matrix4.diagonal3Values(_scale, _scale, 1.0);

  @override
  void performLayout() {
    if (child == null) {
      size = constraints.smallest;
      return;
    }
    // 子树在 逻辑尺寸(= 屏幕尺寸 / scale) 下布局
    child!.layout(
      BoxConstraints.tight(constraints.biggest / _scale),
      parentUsesSize: true,
    );
    // 自身占满全屏，hit test 区域覆盖整个屏幕
    size = constraints.biggest;
  }

  @override
  void paint(PaintingContext context, Offset offset) {
    if (child == null) return;
    // 绘制时应用 scale 变换，视觉上放大到全屏
    context.pushTransform(
      needsCompositing,
      offset,
      _paintTransform,
      super.paint,
    );
  }

  @override
  bool hitTestChildren(BoxHitTestResult result, {required Offset position}) {
    // 对点击坐标做逆变换（屏幕坐标 → 逻辑坐标），再转发给子树
    return result.addWithPaintTransform(
      transform: _paintTransform,
      position: position,
      hitTest: (BoxHitTestResult result, Offset position) {
        return child?.hitTest(result, position: position) ?? false;
      },
    );
  }

  @override
  void applyPaintTransform(RenderBox child, Matrix4 transform) {
    transform.multiply(_paintTransform);
  }
}
