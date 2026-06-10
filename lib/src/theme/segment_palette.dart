import 'dart:math' as math;

import 'package:flutter/painting.dart';

import 'app_colors.dart';
import 'flux_theme_tokens.dart';

/// 分片线程配色生成器 — 最多 256 条线程，每条颜色唯一
///
/// 算法：
/// - 以主题 accent 的色相为锚点，按黄金角（≈137.5°）旋转生成色相，
///   保证相邻索引色相差最大化且 256 个色相互不重复；
/// - 亮度（3 档）× 饱和度（2 档）循环，让色相接近的索引也能靠
///   明暗/浓淡区分；
/// - 档位按主题明暗选择（暗色主题提亮、浅色主题压暗），并对与
///   背景对比不足的颜色自动调整亮度，保证可读不冲突。
///
/// 若用户自定义主题提供了 segmentPalette，则前 N 条优先使用自定义
/// 颜色，超出部分继续用生成色补齐。
class SegmentPalette {
  SegmentPalette._();

  /// 支持的最大线程数
  static const int maxThreads = 256;

  /// 黄金角 — 色相旋转步长
  static const double _goldenAngle = 137.50776405003785;

  /// 颜色与背景的最低对比度（WCAG 对比度公式）
  static const double _minContrast = 1.6;

  // ── 缓存（按 accent + 背景 + 自定义调色板）──
  static int? _cacheKey;
  static List<Color> _cache = const [];

  /// 获取当前主题下的分片调色板（256 色，带缓存）
  static List<Color> of(AppColors c) {
    final accent = c.accent;
    final background = c.surface1;
    final custom = _customPaletteOf(c);
    final key = Object.hash(
      accent.toARGB32(),
      background.toARGB32(),
      custom == null ? 0 : Object.hashAll(custom.map((e) => e.toARGB32())),
    );
    if (key == _cacheKey && _cache.isNotEmpty) return _cache;
    _cacheKey = key;
    _cache = _generate(accent: accent, background: background, custom: custom);
    return _cache;
  }

  /// 第 [index] 条线程的颜色（超出 256 时取模循环）
  static Color colorFor(List<Color> palette, int index) {
    if (palette.isEmpty) return const Color(0xFF3B82F6);
    return palette[index.abs() % palette.length];
  }

  /// 自定义主题调色板（未自定义时返回 null，走纯生成路径）
  static List<Color>? _customPaletteOf(AppColors c) {
    final palette = c.segmentPalette;
    if (palette.isEmpty ||
        identical(palette, FluxThemeTokens.defaultSegmentPalette)) {
      return null;
    }
    return palette;
  }

  static List<Color> _generate({
    required Color accent,
    required Color background,
    List<Color>? custom,
  }) {
    final isDark = background.computeLuminance() < 0.5;
    final baseHue = HSLColor.fromColor(accent).hue;

    // 亮度 3 档 × 饱和度 2 档（组合周期 6），按主题明暗选择区间
    final lightnessTiers = isDark
        ? const [0.66, 0.74, 0.56]
        : const [0.42, 0.34, 0.50];
    final saturationTiers = isDark ? const [0.60, 0.78] : const [0.62, 0.82];

    final colors = List<Color>.filled(maxThreads, accent);
    for (var i = 0; i < maxThreads; i++) {
      if (custom != null && i < custom.length) {
        colors[i] = _ensureContrast(custom[i], background, isDark);
        continue;
      }
      if (i == 0 && custom == null) {
        // 0 号线程使用主题色本身（对比不足时微调亮度）
        colors[i] = _ensureContrast(accent, background, isDark);
        continue;
      }
      final hue = (baseHue + i * _goldenAngle) % 360.0;
      final lightness = lightnessTiers[i % lightnessTiers.length];
      final saturation = saturationTiers[i % saturationTiers.length];
      final color = HSLColor.fromAHSL(1.0, hue, saturation, lightness)
          .toColor();
      colors[i] = _ensureContrast(color, background, isDark);
    }
    return colors;
  }

  /// 对比度不足时向远离背景的方向调整亮度
  static Color _ensureContrast(Color color, Color background, bool isDark) {
    var hsl = HSLColor.fromColor(color);
    var adjusted = color;
    for (var i = 0; i < 6; i++) {
      if (_contrastRatio(adjusted, background) >= _minContrast) break;
      final delta = isDark ? 0.06 : -0.06;
      hsl = hsl.withLightness((hsl.lightness + delta).clamp(0.05, 0.95));
      adjusted = hsl.toColor();
    }
    return adjusted;
  }

  static double _contrastRatio(Color a, Color b) {
    final la = a.computeLuminance();
    final lb = b.computeLuminance();
    final hi = math.max(la, lb);
    final lo = math.min(la, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
}
