// FluxThemeTokens（Layer0 颜色）+ buildThemeFromTokens 缓存 回归测试。
//
// 覆盖两类历史 bug：
//   Bug A — buildThemeFromTokens 的模块级缓存曾只按部分字段判断"tokens 是否
//     相同"，导致两个仅 metric 不同的 FluxThemeTokens 被误判为同一份主题，
//     命中缓存返回错误（陈旧）的 ShadThemeData。现修复为 FluxThemeTokens.==
//     全字段深比较（含 metric），buildThemeFromTokens 内部据此正确判断缓存
//     是否失效。
//   Bug B — FluxThemeTokens.fromJson 曾在旧版本 JSON（缺 metrics 段 / 部分
//     颜色子键 / 整个 colors 块）时抛异常或产出错误的默认值，破坏了"用户旧主题
//     文件可安全加载"的向后兼容契约。现改为逐叶回退默认值，不抛异常。
//
// 以下测试直接构造 FluxThemeTokens / FluxMetricTokens（纯 Dart 数据类，无需
// widget 树），不使用 testWidgets / MaterialApp（保持轻量，避免不必要的
// Material 依赖）。

import 'package:flutter/widgets.dart' show BorderRadius, Brightness, Color;
import 'package:flutter_test/flutter_test.dart';

import 'package:flux_down/src/theme/app_theme.dart';
import 'package:flux_down/src/theme/flux_metric_tokens.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';

/// 一份"非默认"的自定义主题：全部 29 个颜色字段均使用与任何内置预设都不同的
/// 精确 8 位 hex 字面量（而非 `withValues(alpha: 分数)` 派生色），确保
/// toJson()→fromJson() 的十六进制往返在浮点层面是无损的（8 位 hex 的每个
/// 分量本就是 0-255 的整数，`Color(int)` 构造与 `_colorToHex` 互为精确逆
/// 运算；而 `withValues(alpha: 0.18)` 这类非 1/255 整数倍的分数 alpha 在编码
/// 为 hex 再解码后会因量化产生浮点误差，不适合做"内容不同必须往返相等"的用例）。
FluxThemeTokens _customTokens() => const FluxThemeTokens(
  name: 'Custom Roundtrip',
  author: 'tester',
  appearance: Brightness.dark,
  background: Color(0xFF010203),
  surface1: Color(0xFF112233),
  surface2: Color(0xFF223344),
  surface3: Color(0xFF334455),
  elementHover: Color(0xFF445566),
  elementSelected: Color(0xFF556677),
  elementActive: Color(0xFF667788),
  textPrimary: Color(0xFFEEEEEE),
  textSecondary: Color(0xFFCCCCCC),
  textMuted: Color(0xFFAAAAAA),
  textDisabled: Color(0xFF888888),
  border: Color(0xFF999999),
  borderFocused: Color(0xFFABCDEF),
  accent: Color(0xFF00FF88),
  accentHover: Color(0xFF00CC66),
  accentBackground: Color(0x3300FF88),
  accentForeground: Color(0xFF000000),
  inputBackground: Color(0xFF0A0A0A),
  inputBorder: Color(0xFF1A1A1A),
  inputFocusBorder: Color(0xFF2A2A2A),
  inputFocusBackground: Color(0xFF3A3A3A),
  dialogBackground: Color(0xFF4A4A4A),
  dialogBarrier: Color(0x664A4A4A),
  switchTrack: Color(0xFF5A5A5A),
  switchThumb: Color(0xFF6A6A6A),
  shadow: Color(0xFF000000),
  statusSuccess: Color(0xFF00FF00),
  statusWarning: Color(0xFFFFFF00),
  statusError: Color(0xFFFF0000),
  segmentPalette: [Color(0xFF112233), Color(0xFF223344), Color(0xFF334455)],
  metric: FluxMetricTokens(
    radiusCard: 24,
    spacingMd: 20,
    mobileFabSize: 50,
    alphaSoft: 0.37,
    strokeThin: 3,
  ),
);

/// 每次返回全新的 Map（toJson() 内部即时构造新字面量），测试间互不干扰。
Map<String, dynamic> _customJson() => _customTokens().toJson();

void main() {
  group('buildThemeFromTokens 缓存不因 metric-only 差异而串（Bug A 回归）', () {
    test('仅 metric.radiusMd 不同的两个 tokens 不共用缓存实例，.radius 分别正确', () {
      final base = FluxThemeTokens.defaultDark();
      final t1 = base.copyWith(metric: base.metric.copyWith(radiusMd: 24));
      final t2 = base.copyWith(metric: base.metric.copyWith(radiusMd: 8));
      expect(t1 == t2, isFalse);

      final theme1 = buildThemeFromTokens(t1);
      final theme2 = buildThemeFromTokens(t2);
      expect(
        identical(theme1, theme2),
        isFalse,
        reason: '缓存把仅 metric 不同的两个 tokens 误判为同一份主题',
      );
      expect(theme1.radius, equals(BorderRadius.circular(24)));
      expect(theme2.radius, equals(BorderRadius.circular(8)));
    });

    test('同一 tokens 实例重复调用命中缓存，返回同一 ShadThemeData 对象', () {
      final t = FluxThemeTokens.defaultDark().copyWith(
        metric: const FluxMetricTokens().copyWith(radiusMd: 16),
      );
      final themeA = buildThemeFromTokens(t);
      final themeB = buildThemeFromTokens(t);
      expect(identical(themeA, themeB), isTrue);
    });

    test('t == t.copyWith() 且 hashCode 相同', () {
      final t = FluxThemeTokens.defaultDark();
      final copy = t.copyWith();
      expect(identical(t, copy), isFalse);
      expect(copy, equals(t));
      expect(copy.hashCode, equals(t.hashCode));
    });

    test('两个内容相同的独立构造实例 == 且同 hashCode（非 identity 比较）', () {
      final a = FluxThemeTokens.defaultDark();
      final b = FluxThemeTokens.defaultDark();
      expect(identical(a, b), isFalse);
      expect(a, equals(b));
      expect(a.hashCode, equals(b.hashCode));
    });
  });

  group('FluxThemeTokens.fromJson 向后兼容旧版 JSON（Bug B 回归）', () {
    test('(a) 缺失 metrics 键 → 不抛异常，metric 回退默认，其余字段不受影响', () {
      final json = _customJson()..remove('metrics');
      expect(() => FluxThemeTokens.fromJson(json), returnsNormally);

      final result = FluxThemeTokens.fromJson(json);
      final expected = _customTokens().copyWith(metric: const FluxMetricTokens());
      expect(result, equals(expected));
    });

    test('(b) colors 缺 surface/element 子键 → 对应字段回退该 appearance 默认色，其它字段保留', () {
      final json = _customJson();
      final colors = Map<String, dynamic>.from(json['colors'] as Map)
        ..remove('surface')
        ..remove('element');
      json['colors'] = colors;

      final result = FluxThemeTokens.fromJson(json);
      final darkDefaults = FluxThemeTokens.defaultDark();

      expect(result.background, equals(darkDefaults.background));
      expect(result.surface1, equals(darkDefaults.surface1));
      expect(result.surface2, equals(darkDefaults.surface2));
      expect(result.surface3, equals(darkDefaults.surface3));
      expect(result.elementHover, equals(darkDefaults.elementHover));
      expect(result.elementSelected, equals(darkDefaults.elementSelected));
      expect(result.elementActive, equals(darkDefaults.elementActive));
      // 未缺失的颜色仍按提供值生效，证明回退是逐叶而非整体炸掉
      expect(result.accent, equals(const Color(0xFF00FF88)));
      expect(result.statusError, equals(const Color(0xFFFF0000)));
      expect(result.metric, equals(_customTokens().metric));
    });

    test('(c) 缺失整个 colors 块 → 全部颜色字段回退该 appearance 默认主题，metric/name/author 不受影响', () {
      final json = _customJson()..remove('colors');
      expect(() => FluxThemeTokens.fromJson(json), returnsNormally);

      final result = FluxThemeTokens.fromJson(json);
      final expected = FluxThemeTokens.defaultDark().copyWith(
        name: _customTokens().name,
        author: _customTokens().author,
        metric: _customTokens().metric,
      );
      expect(result, equals(expected));
    });
  });

  group('FluxThemeTokens.fromJson 畸形颜色 / segmentPalette 回退', () {
    test('非法 hex 颜色字符串 → 该字段回退默认色，不影响其它字段', () {
      final json = _customJson();
      final colors = Map<String, dynamic>.from(json['colors'] as Map);
      final accentMap = Map<String, dynamic>.from(colors['accent'] as Map)
        ..['color'] = 'not-a-hex-color';
      colors['accent'] = accentMap;
      json['colors'] = colors;

      final result = FluxThemeTokens.fromJson(json);
      expect(result.accent, equals(FluxThemeTokens.defaultDark().accent));
      expect(result.background, equals(const Color(0xFF010203)));
      expect(result.metric, equals(_customTokens().metric));
    });

    test('segmentPalette 长度 100 → 截断到前 32 个（保持原顺序）', () {
      final generated = List<Color>.generate(100, (i) => Color(0xFF000000 + i));
      final hexList = generated
          .map((c) => c.toARGB32().toRadixString(16).padLeft(8, '0'))
          .toList();
      final json = _customJson();
      final colors = Map<String, dynamic>.from(json['colors'] as Map)
        ..['segmentPalette'] = hexList;
      json['colors'] = colors;

      final result = FluxThemeTokens.fromJson(json);
      expect(result.segmentPalette.length, 32);
      expect(result.segmentPalette, equals(generated.sublist(0, 32)));
    });

    test('segmentPalette 全部元素非法 → 回退 defaultSegmentPalette', () {
      final json = _customJson();
      final colors = Map<String, dynamic>.from(json['colors'] as Map)
        ..['segmentPalette'] = ['zz', 'not-hex', 123, null, true];
      json['colors'] = colors;

      final result = FluxThemeTokens.fromJson(json);
      expect(result.segmentPalette, equals(FluxThemeTokens.defaultSegmentPalette));
    });
  });

  group('round-trip', () {
    test('非默认 tokens 经 toJson→fromJson 往返恒等（含 metric 与 segmentPalette）', () {
      final t = _customTokens();
      final roundTripped = FluxThemeTokens.fromJson(t.toJson());
      expect(roundTripped, equals(t));
      expect(roundTripped.metric, equals(t.metric));
      expect(roundTripped.segmentPalette, equals(t.segmentPalette));
    });
  });
}
