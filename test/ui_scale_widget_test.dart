import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/widgets/ui_scale_widget.dart';

/// 复刻 main.dart 中的接入结构：
/// WidgetsApp.builder → MediaQuery(size/scale) → UiScaleWidget → 页面。
/// 页面里放一排"缩放 chip"，点击即 setState 修改 scale，
/// 模拟设置页「界面缩放」的真实交互路径。
class _Harness extends StatefulWidget {
  const _Harness();

  @override
  State<_Harness> createState() => _HarnessState();
}

class _HarnessState extends State<_Harness> {
  double scale = 1.0;
  final List<double> tapped = [];

  static const options = [0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.5];

  @override
  Widget build(BuildContext context) {
    return WidgetsApp(
      color: const Color(0xFF000000),
      pageRouteBuilder: <T>(RouteSettings settings, WidgetBuilder builder) {
        return PageRouteBuilder<T>(
          settings: settings,
          pageBuilder: (context, _, _) => builder(context),
        );
      },
      builder: (context, child) {
        final s = scale;
        if (s == 1.0) return child!;
        final mq = MediaQuery.of(context);
        return MediaQuery(
          data: mq.copyWith(size: mq.size / s),
          child: UiScaleWidget(scale: s, child: child!),
        );
      },
      home: Directionality(
        textDirection: TextDirection.ltr,
        child: ColoredBox(
          color: const Color(0xFFFFFFFF),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const SizedBox(height: 100),
              // 模拟设置页里嵌套 RepaintBoundary 的情况
              RepaintBoundary(
                child: Row(
                  children: options.map((v) {
                    return GestureDetector(
                      key: ValueKey(v),
                      onTap: () {
                        tapped.add(v);
                        setState(() => scale = v);
                      },
                      child: Container(
                        width: 50,
                        height: 28,
                        margin: const EdgeInsets.only(right: 6),
                        color: (v - scale).abs() < 0.01
                            ? const Color(0xFF0000FF)
                            : const Color(0xFFEEEEEE),
                      ),
                    );
                  }).toList(),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

void main() {
  /// 按当前 scale 把 chip 的逻辑中心换算成屏幕（视觉）坐标后点击，
  /// 模拟用户"点看到的那个按钮"。
  Future<void> tapChipVisually(
    WidgetTester tester,
    _HarnessState state,
    double value,
  ) async {
    final index = _HarnessState.options.indexOf(value);
    // 逻辑坐标：chip 宽 50 + 右边距 6
    final logicalCenter = Offset(index * 56.0 + 25.0, 100.0 + 14.0);
    final visual = logicalCenter * state.scale;
    await tester.tapAt(visual);
    await tester.pumpAndSettle();
  }

  testWidgets('连续切换非 100% 缩放，每次点击都命中正确的 chip', (tester) async {
    await tester.pumpWidget(const _Harness());
    final state = tester.state<_HarnessState>(find.byType(_Harness));

    // 100% → 110%
    await tapChipVisually(tester, state, 1.1);
    expect(state.tapped, [1.1]);
    expect(state.scale, 1.1);

    // 110% → 120%（非 1.0 → 非 1.0，用户报告的失败路径）
    await tapChipVisually(tester, state, 1.2);
    expect(state.tapped, [1.1, 1.2]);
    expect(state.scale, 1.2);

    // 120% → 110%
    await tapChipVisually(tester, state, 1.1);
    expect(state.tapped, [1.1, 1.2, 1.1]);
    expect(state.scale, 1.1);

    // 110% → 130%
    await tapChipVisually(tester, state, 1.3);
    expect(state.tapped, [1.1, 1.2, 1.1, 1.3]);
    expect(state.scale, 1.3);

    // 130% → 100%
    await tapChipVisually(tester, state, 1.0);
    expect(state.scale, 1.0);
  });

  testWidgets('scale 切换后 RenderUiScale 的布局与绘制变换同步更新', (tester) async {
    await tester.pumpWidget(const _Harness());
    final state = tester.state<_HarnessState>(find.byType(_Harness));

    await tapChipVisually(tester, state, 1.2);
    var render = tester.renderObject<RenderUiScale>(
      find.byType(UiScaleWidget),
    );
    expect(render.scale, 1.2);
    // 子树按 逻辑尺寸 = 屏幕 / 1.2 布局
    final childSize = (render.child as RenderBox).size;
    expect(childSize.width, closeTo(800 / 1.2, 0.01));

    // 绘制矩阵与 hit test 矩阵一致（getTransformTo 走 applyPaintTransform）
    final chip = tester.renderObject<RenderBox>(
      find.byKey(const ValueKey(1.2)),
    );
    final origin = MatrixUtils.transformPoint(
      chip.getTransformTo(null),
      Offset.zero,
    );
    // chip 1.2 是第 5 个（index 4）：逻辑 x = 4*56，视觉 x = 4*56*1.2
    expect(origin.dx, closeTo(4 * 56 * 1.2, 0.01));
    expect(origin.dy, closeTo(100 * 1.2, 0.01));

    // 非 1.0 → 非 1.0 再切一次，矩阵必须跟着变
    await tapChipVisually(tester, state, 1.1);
    render = tester.renderObject<RenderUiScale>(find.byType(UiScaleWidget));
    expect(render.scale, 1.1);
    final origin2 = MatrixUtils.transformPoint(
      chip.getTransformTo(null),
      Offset.zero,
    );
    expect(origin2.dx, closeTo(4 * 56 * 1.1, 0.01));
  });
}
