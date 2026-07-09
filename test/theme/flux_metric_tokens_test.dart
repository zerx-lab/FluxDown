// FluxMetricTokens（Layer1 圆角/间距/描边/按钮/透明度/移动端几何）+
// AppMetrics 门面单测。
//
// 覆盖：
//   - 值域夹取（getter 侧 clamp，构造函数纯赋值不 clamp）：越界值回落到区间边界，
//     不影响其它字段；非法/缺失 JSON 输入逐字段回退默认（fromJson 的 _numOr）。
//   - 逐字段最小对差（防 ==/toJson/fromJson 漏字段回归，D2b-F5）：55 个字段各自
//     独立扰动一次，断言 == / toJson / fromJson round-trip 三个环节都感知到差异。
//   - AppMetrics 门面直读：圆角 getter 与 metric 对应字段相等；alpha 派生
//     （soft/muted/...）与 `base.withValues(alpha: metric.alphaXxx)` 等价。
import 'package:flutter/widgets.dart' show BorderRadius, Color;
import 'package:flutter_test/flutter_test.dart';

import 'package:flux_down/src/theme/app_metrics.dart';
import 'package:flux_down/src/theme/flux_metric_tokens.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';

/// 55 个字段各自的最小扰动闭包：对 [FluxMetricTokens] 的 copyWith 施加一个
/// 可辨识的变化（geom/stroke +1，alpha ±0.05 且保持在 (0,1) 内）。
/// 字段集合照抄 flux_metric_tokens.dart 全部公开 getter。
final Map<String, FluxMetricTokens Function(FluxMetricTokens)> _fieldMutators =
    {
      // 圆角
      'radiusProgress': (t) => t.copyWith(radiusProgress: t.radiusProgress + 1),
      'radiusXs': (t) => t.copyWith(radiusXs: t.radiusXs + 1),
      'radiusSegmentCell': (t) =>
          t.copyWith(radiusSegmentCell: t.radiusSegmentCell + 1),
      'radiusSm': (t) => t.copyWith(radiusSm: t.radiusSm + 1),
      'radiusMd': (t) => t.copyWith(radiusMd: t.radiusMd + 1),
      'radiusInput': (t) => t.copyWith(radiusInput: t.radiusInput + 1),
      'radiusCard': (t) => t.copyWith(radiusCard: t.radiusCard + 1),
      'radiusIconTile': (t) => t.copyWith(radiusIconTile: t.radiusIconTile + 1),
      'radiusDialog': (t) => t.copyWith(radiusDialog: t.radiusDialog + 1),
      'radiusFieldMobile': (t) =>
          t.copyWith(radiusFieldMobile: t.radiusFieldMobile + 1),
      'radiusChipLg': (t) => t.copyWith(radiusChipLg: t.radiusChipLg + 1),
      'radiusChipXl': (t) => t.copyWith(radiusChipXl: t.radiusChipXl + 1),
      'radiusBadge': (t) => t.copyWith(radiusBadge: t.radiusBadge + 1),
      // pill 默认 999 已在 clamp 上限 2000 内有余量，+1 仍合法可辨识
      'radiusPill': (t) => t.copyWith(radiusPill: t.radiusPill + 1),
      'radiusSheet': (t) => t.copyWith(radiusSheet: t.radiusSheet + 1),
      // 描边
      'strokeThin': (t) => t.copyWith(strokeThin: t.strokeThin + 1),
      'strokeThick': (t) => t.copyWith(strokeThick: t.strokeThick + 1),
      // 间距
      'spacingXs': (t) => t.copyWith(spacingXs: t.spacingXs + 1),
      'spacingSm': (t) => t.copyWith(spacingSm: t.spacingSm + 1),
      'spacingMd': (t) => t.copyWith(spacingMd: t.spacingMd + 1),
      'spacingLg': (t) => t.copyWith(spacingLg: t.spacingLg + 1),
      'spacingXl': (t) => t.copyWith(spacingXl: t.spacingXl + 1),
      // 按钮
      'buttonHeightSm': (t) =>
          t.copyWith(buttonHeightSm: t.buttonHeightSm + 1),
      'buttonHeightMd': (t) =>
          t.copyWith(buttonHeightMd: t.buttonHeightMd + 1),
      'buttonHeightLg': (t) =>
          t.copyWith(buttonHeightLg: t.buttonHeightLg + 1),
      // 透明度（默认值均 <= 0.8，+0.05 仍在 (0,1) 内）
      'alphaSubtle': (t) => t.copyWith(alphaSubtle: t.alphaSubtle + 0.05),
      'alphaSoft': (t) => t.copyWith(alphaSoft: t.alphaSoft + 0.05),
      'alphaMuted': (t) => t.copyWith(alphaMuted: t.alphaMuted + 0.05),
      'alphaMutedStrong': (t) =>
          t.copyWith(alphaMutedStrong: t.alphaMutedStrong + 0.05),
      'alphaActive': (t) => t.copyWith(alphaActive: t.alphaActive + 0.05),
      'alphaSelectedBorder': (t) =>
          t.copyWith(alphaSelectedBorder: t.alphaSelectedBorder + 0.05),
      'alphaScrim': (t) => t.copyWith(alphaScrim: t.alphaScrim + 0.05),
      'alphaBorder': (t) => t.copyWith(alphaBorder: t.alphaBorder + 0.05),
      'alphaBorderStrong': (t) =>
          // 0.8 + 0.05 = 0.85，仍 < 1
          t.copyWith(alphaBorderStrong: t.alphaBorderStrong + 0.05),
      'alphaDisabled': (t) => t.copyWith(alphaDisabled: t.alphaDisabled + 0.05),
      'alphaGlass': (t) => t.copyWith(alphaGlass: t.alphaGlass + 0.05),
      'alphaFocusRing': (t) =>
          t.copyWith(alphaFocusRing: t.alphaFocusRing + 0.05),
      'alphaShadowStrong': (t) =>
          t.copyWith(alphaShadowStrong: t.alphaShadowStrong + 0.05),
      'alphaShadowSoft': (t) =>
          t.copyWith(alphaShadowSoft: t.alphaShadowSoft + 0.05),
      'alphaShadowFaint': (t) =>
          t.copyWith(alphaShadowFaint: t.alphaShadowFaint + 0.05),
      'alphaFaint': (t) => t.copyWith(alphaFaint: t.alphaFaint + 0.05),
      'alphaTextSelection': (t) =>
          t.copyWith(alphaTextSelection: t.alphaTextSelection + 0.05),
      'alphaBorderSubtle': (t) =>
          t.copyWith(alphaBorderSubtle: t.alphaBorderSubtle + 0.05),
      'alphaBorderFaint': (t) =>
          t.copyWith(alphaBorderFaint: t.alphaBorderFaint + 0.05),
      'alphaBorderMedium': (t) =>
          t.copyWith(alphaBorderMedium: t.alphaBorderMedium + 0.05),
      'alphaEmphasis': (t) => t.copyWith(alphaEmphasis: t.alphaEmphasis + 0.05),
      'alphaGlassSubtle': (t) =>
          t.copyWith(alphaGlassSubtle: t.alphaGlassSubtle + 0.05),
      // 移动端几何
      'mobilePageMargin': (t) =>
          t.copyWith(mobilePageMargin: t.mobilePageMargin + 1),
      'mobileCardRadius': (t) =>
          t.copyWith(mobileCardRadius: t.mobileCardRadius + 1),
      'mobileCardGap': (t) => t.copyWith(mobileCardGap: t.mobileCardGap + 1),
      'mobileAppBarHeight': (t) =>
          t.copyWith(mobileAppBarHeight: t.mobileAppBarHeight + 1),
      'mobileTabsHeight': (t) =>
          t.copyWith(mobileTabsHeight: t.mobileTabsHeight + 1),
      'mobileDockBottomGap': (t) =>
          t.copyWith(mobileDockBottomGap: t.mobileDockBottomGap + 1),
      'mobileFabSize': (t) => t.copyWith(mobileFabSize: t.mobileFabSize + 1),
      'mobileScrollBottomPadding': (t) => t.copyWith(
        mobileScrollBottomPadding: t.mobileScrollBottomPadding + 1,
      ),
    };

/// 从字段名读取该字段当前（夹取后）的 double 值，供 fromJson round-trip 断言用。
double _fieldValue(FluxMetricTokens t, String field) {
  switch (field) {
    case 'radiusProgress':
      return t.radiusProgress;
    case 'radiusXs':
      return t.radiusXs;
    case 'radiusSegmentCell':
      return t.radiusSegmentCell;
    case 'radiusSm':
      return t.radiusSm;
    case 'radiusMd':
      return t.radiusMd;
    case 'radiusInput':
      return t.radiusInput;
    case 'radiusCard':
      return t.radiusCard;
    case 'radiusIconTile':
      return t.radiusIconTile;
    case 'radiusDialog':
      return t.radiusDialog;
    case 'radiusFieldMobile':
      return t.radiusFieldMobile;
    case 'radiusChipLg':
      return t.radiusChipLg;
    case 'radiusChipXl':
      return t.radiusChipXl;
    case 'radiusBadge':
      return t.radiusBadge;
    case 'radiusPill':
      return t.radiusPill;
    case 'radiusSheet':
      return t.radiusSheet;
    case 'strokeThin':
      return t.strokeThin;
    case 'strokeThick':
      return t.strokeThick;
    case 'spacingXs':
      return t.spacingXs;
    case 'spacingSm':
      return t.spacingSm;
    case 'spacingMd':
      return t.spacingMd;
    case 'spacingLg':
      return t.spacingLg;
    case 'spacingXl':
      return t.spacingXl;
    case 'buttonHeightSm':
      return t.buttonHeightSm;
    case 'buttonHeightMd':
      return t.buttonHeightMd;
    case 'buttonHeightLg':
      return t.buttonHeightLg;
    case 'alphaSubtle':
      return t.alphaSubtle;
    case 'alphaSoft':
      return t.alphaSoft;
    case 'alphaMuted':
      return t.alphaMuted;
    case 'alphaMutedStrong':
      return t.alphaMutedStrong;
    case 'alphaActive':
      return t.alphaActive;
    case 'alphaSelectedBorder':
      return t.alphaSelectedBorder;
    case 'alphaScrim':
      return t.alphaScrim;
    case 'alphaBorder':
      return t.alphaBorder;
    case 'alphaBorderStrong':
      return t.alphaBorderStrong;
    case 'alphaDisabled':
      return t.alphaDisabled;
    case 'alphaGlass':
      return t.alphaGlass;
    case 'alphaFocusRing':
      return t.alphaFocusRing;
    case 'alphaShadowStrong':
      return t.alphaShadowStrong;
    case 'alphaShadowSoft':
      return t.alphaShadowSoft;
    case 'alphaShadowFaint':
      return t.alphaShadowFaint;
    case 'alphaFaint':
      return t.alphaFaint;
    case 'alphaTextSelection':
      return t.alphaTextSelection;
    case 'alphaBorderSubtle':
      return t.alphaBorderSubtle;
    case 'alphaBorderFaint':
      return t.alphaBorderFaint;
    case 'alphaBorderMedium':
      return t.alphaBorderMedium;
    case 'alphaEmphasis':
      return t.alphaEmphasis;
    case 'alphaGlassSubtle':
      return t.alphaGlassSubtle;
    case 'mobilePageMargin':
      return t.mobilePageMargin;
    case 'mobileCardRadius':
      return t.mobileCardRadius;
    case 'mobileCardGap':
      return t.mobileCardGap;
    case 'mobileAppBarHeight':
      return t.mobileAppBarHeight;
    case 'mobileTabsHeight':
      return t.mobileTabsHeight;
    case 'mobileDockBottomGap':
      return t.mobileDockBottomGap;
    case 'mobileFabSize':
      return t.mobileFabSize;
    case 'mobileScrollBottomPadding':
      return t.mobileScrollBottomPadding;
    default:
      throw ArgumentError('unknown field: $field');
  }
}

void main() {
  group('值域夹取（getter 侧 clamp）', () {
    test('圆角/间距/移动几何越界 clamp 到 [0, 2000]，不影响其它字段', () {
      final t = FluxMetricTokens.fromJson({
        'radius': {'card': -50, 'dialog': 99999},
      });
      expect(t.radiusCard, 0);
      expect(t.radiusDialog, 2000);
      // 未触碰字段保持默认
      expect(t.radiusMd, const FluxMetricTokens().radiusMd);
      expect(t.spacingMd, const FluxMetricTokens().spacingMd);
    });

    test('描边越界 clamp 到 [0, 8]', () {
      final t = FluxMetricTokens.fromJson({
        'stroke': {'thin': 100, 'thick': -3},
      });
      expect(t.strokeThin, 8);
      expect(t.strokeThick, 0);
    });

    test('透明度越界 clamp 到 [0, 1]', () {
      final t = FluxMetricTokens.fromJson({
        'alpha': {'subtle': 5, 'soft': -1},
      });
      expect(t.alphaSubtle, 1);
      expect(t.alphaSoft, 0);
    });

    test('copyWith 传入越界原始值同样在 getter 侧被夹取', () {
      final t = const FluxMetricTokens().copyWith(
        radiusBadge: -10,
        alphaScrim: 2.5,
      );
      expect(t.radiusBadge, 0);
      expect(t.alphaScrim, 1);
    });
  });

  group('FluxMetricTokens.fromJson 畸形/缺失输入回退默认', () {
    const defaults = FluxMetricTokens();

    test('完全空 Map → 全部字段等于默认值', () {
      final t = FluxMetricTokens.fromJson(const {});
      expect(t, equals(defaults));
    });

    test('子组类型非法（非 Map）→ 该组全部字段回退默认，不抛异常', () {
      expect(
        () => FluxMetricTokens.fromJson({'radius': 'not-a-map'}),
        returnsNormally,
      );
      final t = FluxMetricTokens.fromJson({'radius': 'not-a-map'});
      expect(t.radiusCard, defaults.radiusCard);
      expect(t.radiusDialog, defaults.radiusDialog);
    });

    test('单字段类型非法（字符串代替 num）→ 仅该字段回退默认，同组其它字段不受影响', () {
      final t = FluxMetricTokens.fromJson({
        'alpha': {'soft': 'x', 'muted': 0.3},
      });
      expect(t.alphaSoft, defaults.alphaSoft);
      expect(t.alphaMuted, 0.3);
    });

    test('int 类型数值（非 double 字面量）能正确解析', () {
      final t = FluxMetricTokens.fromJson({
        'radius': {'card': 20},
      });
      expect(t.radiusCard, 20.0);
    });
  });

  group('逐字段最小对差（防 ==/toJson/fromJson 漏字段回归，D2b-F5）', () {
    const base = FluxMetricTokens();

    for (final entry in _fieldMutators.entries) {
      final field = entry.key;
      final mutate = entry.value;

      test('字段 `$field`：mutate 后 == / toJson / fromJson 三环节均可感知差异', () {
        final changed = mutate(base);

        expect(
          changed == base,
          isFalse,
          reason: '字段 `$field` 的变化未被 == 检测到（likely `==` 遗漏该字段）',
        );

        expect(
          changed.toJson().toString() == base.toJson().toString(),
          isFalse,
          reason: '字段 `$field` 的变化未反映在 toJson()（likely `toJson` 遗漏该字段）',
        );

        final roundTripped = FluxMetricTokens.fromJson(changed.toJson());
        expect(
          _fieldValue(roundTripped, field),
          closeTo(_fieldValue(changed, field), 1e-9),
          reason: '字段 `$field` 经 toJson→fromJson 往返值不一致（likely `fromJson` 遗漏该字段）',
        );
      });
    }

    test('对差表覆盖全部 55 个公开字段', () {
      expect(_fieldMutators.length, 55);
    });
  });

  group('FluxMetricTokens 全字段 == / hashCode', () {
    test('相等 tokens 同 hashCode；copyWith() 不改变字段', () {
      const a = FluxMetricTokens();
      final b = const FluxMetricTokens().copyWith();
      expect(a, equals(b));
      expect(a.hashCode, equals(b.hashCode));
    });

    test('standard 静态实例等于默认构造', () {
      expect(FluxMetricTokens.standard, equals(const FluxMetricTokens()));
    });
  });

  group('AppMetrics 门面直读', () {
    FluxThemeTokens themeWith(FluxMetricTokens metric) =>
        FluxThemeTokens.defaultDark().copyWith(metric: metric);

    test('圆角 getter 与 metric 对应字段相等（覆盖全部圆角角色）', () {
      const metric = FluxMetricTokens(
        radiusCard: 11,
        radiusInput: 13,
        radiusDialog: 17,
        radiusBadge: 19,
        radiusPill: 500,
        radiusProgress: 3,
        radiusSegmentCell: 4,
        radiusXs: 5,
        radiusSm: 6,
        radiusMd: 7,
        radiusIconTile: 8,
        radiusFieldMobile: 9,
        radiusChipLg: 10,
        radiusChipXl: 12,
      );
      final m = AppMetrics.fromTokens(themeWith(metric));

      expect(m.card, metric.radiusCard);
      expect(m.input, metric.radiusInput);
      expect(m.dialog, metric.radiusDialog);
      expect(m.badge, metric.radiusBadge);
      expect(m.pill, metric.radiusPill);
      expect(m.progress, metric.radiusProgress);
      expect(m.segmentCell, metric.radiusSegmentCell);
      expect(m.xs, metric.radiusXs);
      expect(m.sm, metric.radiusSm);
      expect(m.md, metric.radiusMd);
      expect(m.iconTile, metric.radiusIconTile);
      expect(m.fieldMobile, metric.radiusFieldMobile);
      expect(m.chipLg, metric.radiusChipLg);
      expect(m.chipXl, metric.radiusChipXl);

      // BorderRadius 便捷 getter 与裸 double 值一致
      expect(m.brCard, BorderRadius.circular(metric.radiusCard));
      expect(m.brPill, BorderRadius.circular(metric.radiusPill));
    });

    test('间距/描边/按钮/移动几何 getter 直通 metric', () {
      const metric = FluxMetricTokens(
        spacingMd: 13,
        strokeThick: 2,
        buttonHeightLg: 40,
        mobileFabSize: 52,
      );
      final m = AppMetrics.fromTokens(themeWith(metric));
      expect(m.spacingMd, 13);
      expect(m.strokeThick, 2);
      expect(m.buttonHeightLg, 40);
      expect(m.mobileFabSize, 52);
    });

    test('透明度派生 getter 与 base.withValues(alpha: metric.alphaXxx) 等价', () {
      const metric = FluxMetricTokens(
        alphaSoft: 0.37,
        alphaMuted: 0.2,
        alphaGlass: 0.6,
      );
      final m = AppMetrics.fromTokens(themeWith(metric));
      const base = Color(0xFF112233);

      Color expectedAlpha(double a) => base.withValues(alpha: a);

      expect(m.soft(base), expectedAlpha(metric.alphaSoft));
      expect(m.muted(base), expectedAlpha(metric.alphaMuted));
      expect(m.glass(base), expectedAlpha(metric.alphaGlass));
    });

    test('AppMetrics.of(context) 通过 FluxThemeScope 读取同一份 metric', () {
      // 门面 API 是纯函数式读取（无 context 时用 fromTokens），
      // 这里只验证 fromTokens 与直接访问 tokens.metric 完全一致，
      // 覆盖 AppMetrics 与 FluxThemeTokens 的耦合契约。
      final tokens = themeWith(const FluxMetricTokens(radiusCard: 33));
      final m = AppMetrics.fromTokens(tokens);
      expect(m.metric, equals(tokens.metric));
      expect(m.card, equals(tokens.metric.radiusCard));
    });
  });
}
