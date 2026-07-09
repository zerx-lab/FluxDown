import 'package:flutter/material.dart' show Colors;
import 'package:flutter/widgets.dart';
import 'package:flutter/foundation.dart';

import 'flux_metric_tokens.dart';

// ═══════════════════════════════════════════════════════════
//  FluxThemeScope — InheritedWidget 向下传递 Token
// ═══════════════════════════════════════════════════════════

/// 通过 widget tree 向下传递当前生效的 [FluxThemeTokens]。
///
/// 在 main.dart 中包裹整个应用，AppColors.of(context) 通过此节点获取 tokens。
class FluxThemeScope extends InheritedWidget {
  final FluxThemeTokens tokens;

  const FluxThemeScope({super.key, required this.tokens, required super.child});

  static FluxThemeTokens of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<FluxThemeScope>();
    assert(scope != null, 'FluxThemeScope not found in widget tree');
    return scope!.tokens;
  }

  @override
  bool updateShouldNotify(FluxThemeScope oldWidget) =>
      tokens != oldWidget.tokens;
}

// ═══════════════════════════════════════════════════════════
//  FluxThemeTokens — 主题 Token 数据类
// ═══════════════════════════════════════════════════════════

/// FluxDown 主题 Token 系统
///
/// 将所有 UI 颜色抽象为语义化 Token，支持 JSON 序列化/反序列化，
/// 允许用户完全自定义每个 UI 元素的颜色。
@immutable
class FluxThemeTokens {
  // ── 元数据 ──
  final String name;
  final String? author;
  final Brightness appearance;

  // ── Surface（表面/背景层级）──
  final Color background;
  final Color surface1;
  final Color surface2;
  final Color surface3;

  // ── Element（交互态）──
  final Color elementHover;
  final Color elementSelected;
  final Color elementActive;

  // ── Text（文字层级）──
  final Color textPrimary;
  final Color textSecondary;
  final Color textMuted;
  final Color textDisabled;

  // ── Border（边框）──
  final Color border;
  final Color borderFocused;

  // ── Accent（强调色系）──
  final Color accent;
  final Color accentHover;
  final Color accentBackground;
  final Color accentForeground;

  // ── Input（输入框）──
  final Color inputBackground;
  final Color inputBorder;
  final Color inputFocusBorder;
  final Color inputFocusBackground;

  // ── Dialog（对话框）──
  final Color dialogBackground;
  final Color dialogBarrier;

  // ── Switch（开关）──
  final Color switchTrack;
  final Color switchThumb;

  // ── Shadow（阴影基色）──
  final Color shadow;

  // ── Status（语义状态色）──
  final Color statusSuccess;
  final Color statusWarning;
  final Color statusError;

  // ── Segment Palette（分片调色板）──
  final List<Color> segmentPalette;

  // ── Metric（Layer1：圆角/间距/透明度等非颜色设计变量）──
  final FluxMetricTokens metric;

  const FluxThemeTokens({
    required this.name,
    this.author,
    required this.appearance,
    required this.background,
    required this.surface1,
    required this.surface2,
    required this.surface3,
    required this.elementHover,
    required this.elementSelected,
    required this.elementActive,
    required this.textPrimary,
    required this.textSecondary,
    required this.textMuted,
    required this.textDisabled,
    required this.border,
    required this.borderFocused,
    required this.accent,
    required this.accentHover,
    required this.accentBackground,
    required this.accentForeground,
    required this.inputBackground,
    required this.inputBorder,
    required this.inputFocusBorder,
    required this.inputFocusBackground,
    required this.dialogBackground,
    required this.dialogBarrier,
    required this.switchTrack,
    required this.switchThumb,
    required this.shadow,
    required this.statusSuccess,
    required this.statusWarning,
    required this.statusError,
    this.segmentPalette = defaultSegmentPalette,
    this.metric = FluxMetricTokens.standard,
  });

  // ── 默认分片调色板（自定义主题未提供 segmentPalette 时的占位值；
  //    运行时由 SegmentPalette 基于 accent 动态生成 256 色）──
  static const defaultSegmentPalette = <Color>[
    Color(0xFF22C55E),
    Color(0xFFF59E0B),
    Color(0xFFA855F7),
    Color(0xFF06B6D4),
    Color(0xFFEC4899),
    Color(0xFF14B8A6),
    Color(0xFFEF4444),
    Color(0xFF8B5CF6),
    Color(0xFFF97316),
    Color(0xFF10B981),
    Color(0xFFE11D48),
    Color(0xFF0EA5E9),
    Color(0xFFD946EF),
    Color(0xFF84CC16),
    Color(0xFF64748B),
    Color(0xFF3B82F6),
  ];

  // ═══════════════════════════════════════════════════════════
  //  内置预设
  // ═══════════════════════════════════════════════════════════

  /// 默认暗色主题（Apple 风格深灰）
  static FluxThemeTokens defaultDark({Color accent = const Color(0xFF3B82F6)}) {
    final hsl = HSLColor.fromColor(accent);
    final hover = hsl
        .withLightness((hsl.lightness + 0.08).clamp(0.0, 1.0))
        .toColor();
    final fg = _foregroundFor(accent);
    return FluxThemeTokens(
      name: 'Default Dark',
      appearance: Brightness.dark,
      // Surface
      background: const Color(0xFF1C1C1E),
      surface1: const Color(0xFF2C2C2E),
      surface2: const Color(0xFF3A3A3C),
      surface3: const Color(0xFF48484A),
      // Element
      elementHover: const Color(0xFF424245),
      elementSelected: const Color(0xFF3A3A3C),
      elementActive: accent.withValues(alpha: 0.18),
      // Text
      textPrimary: const Color(0xFFF5F5F7),
      textSecondary: const Color(0xFFA1A1A6),
      textMuted: const Color(0xFF8E8E93),
      textDisabled: const Color(0xFF8E8E93).withValues(alpha: 0.5),
      // Border
      border: const Color(0xFF48484A),
      borderFocused: accent,
      // Accent
      accent: accent,
      accentHover: hover,
      accentBackground: accent.withValues(alpha: 0.18),
      accentForeground: fg,
      // Input
      inputBackground: const Color(0xFF1C1C1E),
      inputBorder: const Color(0xFF48484A),
      inputFocusBorder: accent,
      inputFocusBackground: accent.withValues(alpha: 0.08),
      // Dialog
      dialogBackground: const Color(0xFF2C2C2E),
      dialogBarrier: const Color(0x40000000),
      // Switch
      switchTrack: const Color(0xFF636366),
      switchThumb: const Color(0xFFFFFFFF),
      // Shadow
      shadow: const Color(0xFF000000),
      // Status
      statusSuccess: const Color(0xFF22C55E),
      statusWarning: const Color(0xFFF59E0B),
      statusError: const Color(0xFFEF4444),
    );
  }

  /// 默认亮色主题
  static FluxThemeTokens defaultLight({
    Color accent = const Color(0xFF3B82F6),
  }) {
    final hsl = HSLColor.fromColor(accent);
    final hover = hsl
        .withLightness((hsl.lightness + 0.06).clamp(0.0, 1.0))
        .toColor();
    final fg = _foregroundFor(accent);
    return FluxThemeTokens(
      name: 'Default Light',
      appearance: Brightness.light,
      // Surface
      background: const Color(0xFFF8F9FA),
      surface1: const Color(0xFFFFFFFF),
      surface2: const Color(0xFFF1F3F5),
      surface3: const Color(0xFFE9ECEF),
      // Element
      elementHover: const Color(0xFFF1F3F5),
      elementSelected: accent.withValues(alpha: 0.10),
      elementActive: accent.withValues(alpha: 0.10),
      // Text
      textPrimary: const Color(0xFF09090B),
      textSecondary: const Color(0xFF71717A),
      textMuted: const Color(0xFFA1A1AA),
      textDisabled: const Color(0xFFA1A1AA).withValues(alpha: 0.5),
      // Border
      border: const Color(0xFFE4E4E7),
      borderFocused: accent,
      // Accent
      accent: accent,
      accentHover: hover,
      accentBackground: accent.withValues(alpha: 0.10),
      accentForeground: fg,
      // Input
      inputBackground: const Color(0xFFFFFFFF),
      inputBorder: const Color(0xFFE4E4E7),
      inputFocusBorder: accent,
      inputFocusBackground: const Color(0xFFFFFFFF),
      // Dialog
      dialogBackground: const Color(0xFFFFFFFF),
      dialogBarrier: const Color(0x1A000000),
      // Switch
      switchTrack: const Color(0xFFE5E5EA),
      switchThumb: const Color(0xFFFFFFFF),
      // Shadow
      shadow: const Color(0xFF000000),
      // Status
      statusSuccess: const Color(0xFF22C55E),
      statusWarning: const Color(0xFFF59E0B),
      statusError: const Color(0xFFEF4444),
    );
  }

  /// 根据颜色亮度自动选择前景色
  static Color _foregroundFor(Color c) =>
      c.computeLuminance() > 0.5 ? const Color(0xFF09090B) : Colors.white;

  // ═══════════════════════════════════════════════════════════
  //  额外内置主题
  // ═══════════════════════════════════════════════════════════

  /// Midnight Blue — 深海蓝暗色主题
  static FluxThemeTokens midnightBlue({
    Color accent = const Color(0xFF60A5FA),
  }) {
    final hsl = HSLColor.fromColor(accent);
    final hover = hsl
        .withLightness((hsl.lightness + 0.08).clamp(0.0, 1.0))
        .toColor();
    return FluxThemeTokens(
      name: 'Midnight Blue',
      appearance: Brightness.dark,
      background: const Color(0xFF0F172A),
      surface1: const Color(0xFF1E293B),
      surface2: const Color(0xFF334155),
      surface3: const Color(0xFF475569),
      elementHover: const Color(0xFF334155),
      elementSelected: const Color(0xFF334155),
      elementActive: accent.withValues(alpha: 0.18),
      textPrimary: const Color(0xFFF1F5F9),
      textSecondary: const Color(0xFF94A3B8),
      textMuted: const Color(0xFF64748B),
      textDisabled: const Color(0xFF64748B).withValues(alpha: 0.5),
      border: const Color(0xFF334155),
      borderFocused: accent,
      accent: accent,
      accentHover: hover,
      accentBackground: accent.withValues(alpha: 0.15),
      accentForeground: _foregroundFor(accent),
      inputBackground: const Color(0xFF0F172A),
      inputBorder: const Color(0xFF334155),
      inputFocusBorder: accent,
      inputFocusBackground: accent.withValues(alpha: 0.08),
      dialogBackground: const Color(0xFF1E293B),
      dialogBarrier: const Color(0x50000000),
      switchTrack: const Color(0xFF475569),
      switchThumb: const Color(0xFFFFFFFF),
      shadow: const Color(0xFF000000),
      statusSuccess: const Color(0xFF22C55E),
      statusWarning: const Color(0xFFF59E0B),
      statusError: const Color(0xFFEF4444),
    );
  }

  /// Nord — 北欧风冷色暗色主题
  static FluxThemeTokens nord({Color accent = const Color(0xFF88C0D0)}) {
    final hsl = HSLColor.fromColor(accent);
    final hover = hsl
        .withLightness((hsl.lightness + 0.08).clamp(0.0, 1.0))
        .toColor();
    return FluxThemeTokens(
      name: 'Nord',
      appearance: Brightness.dark,
      background: const Color(0xFF2E3440),
      surface1: const Color(0xFF3B4252),
      surface2: const Color(0xFF434C5E),
      surface3: const Color(0xFF4C566A),
      elementHover: const Color(0xFF4C566A),
      elementSelected: const Color(0xFF434C5E),
      elementActive: accent.withValues(alpha: 0.18),
      textPrimary: const Color(0xFFECEFF4),
      textSecondary: const Color(0xFFD8DEE9),
      textMuted: const Color(0xFF7B88A1),
      textDisabled: const Color(0xFF7B88A1).withValues(alpha: 0.5),
      border: const Color(0xFF4C566A),
      borderFocused: accent,
      accent: accent,
      accentHover: hover,
      accentBackground: accent.withValues(alpha: 0.15),
      accentForeground: const Color(0xFF2E3440),
      inputBackground: const Color(0xFF2E3440),
      inputBorder: const Color(0xFF4C566A),
      inputFocusBorder: accent,
      inputFocusBackground: accent.withValues(alpha: 0.08),
      dialogBackground: const Color(0xFF3B4252),
      dialogBarrier: const Color(0x50000000),
      switchTrack: const Color(0xFF4C566A),
      switchThumb: const Color(0xFFECEFF4),
      shadow: const Color(0xFF000000),
      statusSuccess: const Color(0xFFA3BE8C),
      statusWarning: const Color(0xFFEBCB8B),
      statusError: const Color(0xFFBF616A),
    );
  }

  /// Warm Light — 暖色调亮色主题
  static FluxThemeTokens warmLight({Color accent = const Color(0xFFE11D48)}) {
    final hsl = HSLColor.fromColor(accent);
    final hover = hsl
        .withLightness((hsl.lightness + 0.06).clamp(0.0, 1.0))
        .toColor();
    return FluxThemeTokens(
      name: 'Warm Light',
      appearance: Brightness.light,
      background: const Color(0xFFFFFBEB),
      surface1: const Color(0xFFFFFFFF),
      surface2: const Color(0xFFFEF3C7),
      surface3: const Color(0xFFFDE68A),
      elementHover: const Color(0xFFFEF3C7),
      elementSelected: accent.withValues(alpha: 0.10),
      elementActive: accent.withValues(alpha: 0.10),
      textPrimary: const Color(0xFF1C1917),
      textSecondary: const Color(0xFF78716C),
      textMuted: const Color(0xFFA8A29E),
      textDisabled: const Color(0xFFA8A29E).withValues(alpha: 0.5),
      border: const Color(0xFFE7E5E4),
      borderFocused: accent,
      accent: accent,
      accentHover: hover,
      accentBackground: accent.withValues(alpha: 0.10),
      accentForeground: _foregroundFor(accent),
      inputBackground: const Color(0xFFFFFFFF),
      inputBorder: const Color(0xFFE7E5E4),
      inputFocusBorder: accent,
      inputFocusBackground: const Color(0xFFFFFFFF),
      dialogBackground: const Color(0xFFFFFFFF),
      dialogBarrier: const Color(0x1A000000),
      switchTrack: const Color(0xFFE7E5E4),
      switchThumb: const Color(0xFFFFFFFF),
      shadow: const Color(0xFF000000),
      statusSuccess: const Color(0xFF16A34A),
      statusWarning: const Color(0xFFD97706),
      statusError: const Color(0xFFDC2626),
    );
  }

  // ═══════════════════════════════════════════════════════════
  //  JSON 序列化
  // ═══════════════════════════════════════════════════════════

  static String _colorToHex(Color c) =>
      c.toARGB32().toRadixString(16).padLeft(8, '0');

  Map<String, dynamic> toJson() => {
    'name': name,
    if (author != null) 'author': author,
    'appearance': appearance == Brightness.dark ? 'dark' : 'light',
    'colors': {
      'surface': {
        'background': _colorToHex(background),
        'surface1': _colorToHex(surface1),
        'surface2': _colorToHex(surface2),
        'surface3': _colorToHex(surface3),
      },
      'element': {
        'hover': _colorToHex(elementHover),
        'selected': _colorToHex(elementSelected),
        'active': _colorToHex(elementActive),
      },
      'text': {
        'primary': _colorToHex(textPrimary),
        'secondary': _colorToHex(textSecondary),
        'muted': _colorToHex(textMuted),
        'disabled': _colorToHex(textDisabled),
      },
      'border': {
        'default': _colorToHex(border),
        'focused': _colorToHex(borderFocused),
      },
      'accent': {
        'color': _colorToHex(accent),
        'hover': _colorToHex(accentHover),
        'background': _colorToHex(accentBackground),
        'foreground': _colorToHex(accentForeground),
      },
      'input': {
        'background': _colorToHex(inputBackground),
        'border': _colorToHex(inputBorder),
        'focusBorder': _colorToHex(inputFocusBorder),
        'focusBackground': _colorToHex(inputFocusBackground),
      },
      'dialog': {
        'background': _colorToHex(dialogBackground),
        'barrier': _colorToHex(dialogBarrier),
      },
      'switch': {
        'track': _colorToHex(switchTrack),
        'thumb': _colorToHex(switchThumb),
      },
      'shadow': _colorToHex(shadow),
      'status': {
        'success': _colorToHex(statusSuccess),
        'warning': _colorToHex(statusWarning),
        'error': _colorToHex(statusError),
      },
      'segmentPalette': segmentPalette.map(_colorToHex).toList(),
    },
    'metrics': metric.toJson(),
    'schemaVersion': 2,
  };

  factory FluxThemeTokens.fromJson(Map<String, dynamic> json) {
    final appearance = json['appearance'] == 'dark'
        ? Brightness.dark
        : Brightness.light;
    final base = _defaultsFor(appearance);
    final colors = _mapOr(json['colors']);
    final surface = _mapOr(colors['surface']);
    final element = _mapOr(colors['element']);
    final text = _mapOr(colors['text']);
    final borderMap = _mapOr(colors['border']);
    final accentMap = _mapOr(colors['accent']);
    final input = _mapOr(colors['input']);
    final dialog = _mapOr(colors['dialog']);
    final switchMap = _mapOr(colors['switch']);
    final status = _mapOr(colors['status']);

    final paletteRaw = colors['segmentPalette'];
    final List<Color> palette;
    if (paletteRaw is List) {
      final parsed = paletteRaw
          .map(tryParseHexColor)
          .whereType<Color>()
          .toList(growable: false);
      palette = parsed.isEmpty
          ? defaultSegmentPalette
          : (parsed.length > 32 ? parsed.sublist(0, 32) : parsed);
    } else {
      palette = defaultSegmentPalette;
    }

    return FluxThemeTokens(
      name: json['name'] is String ? json['name'] as String : base.name,
      author: json['author'] is String ? json['author'] as String : null,
      appearance: appearance,
      background: _hexOr(surface['background'], base.background),
      surface1: _hexOr(surface['surface1'], base.surface1),
      surface2: _hexOr(surface['surface2'], base.surface2),
      surface3: _hexOr(surface['surface3'], base.surface3),
      elementHover: _hexOr(element['hover'], base.elementHover),
      elementSelected: _hexOr(element['selected'], base.elementSelected),
      elementActive: _hexOr(element['active'], base.elementActive),
      textPrimary: _hexOr(text['primary'], base.textPrimary),
      textSecondary: _hexOr(text['secondary'], base.textSecondary),
      textMuted: _hexOr(text['muted'], base.textMuted),
      textDisabled: _hexOr(text['disabled'], base.textDisabled),
      border: _hexOr(borderMap['default'], base.border),
      borderFocused: _hexOr(borderMap['focused'], base.borderFocused),
      accent: _hexOr(accentMap['color'], base.accent),
      accentHover: _hexOr(accentMap['hover'], base.accentHover),
      accentBackground: _hexOr(accentMap['background'], base.accentBackground),
      accentForeground: _hexOr(accentMap['foreground'], base.accentForeground),
      inputBackground: _hexOr(input['background'], base.inputBackground),
      inputBorder: _hexOr(input['border'], base.inputBorder),
      inputFocusBorder: _hexOr(input['focusBorder'], base.inputFocusBorder),
      inputFocusBackground: _hexOr(
        input['focusBackground'],
        base.inputFocusBackground,
      ),
      dialogBackground: _hexOr(dialog['background'], base.dialogBackground),
      dialogBarrier: _hexOr(dialog['barrier'], base.dialogBarrier),
      switchTrack: _hexOr(switchMap['track'], base.switchTrack),
      switchThumb: _hexOr(switchMap['thumb'], base.switchThumb),
      shadow: _hexOr(colors['shadow'], base.shadow),
      statusSuccess: _hexOr(status['success'], base.statusSuccess),
      statusWarning: _hexOr(status['warning'], base.statusWarning),
      statusError: _hexOr(status['error'], base.statusError),
      segmentPalette: palette,
      metric: json['metrics'] is Map<String, dynamic>
          ? FluxMetricTokens.fromJson(json['metrics'] as Map<String, dynamic>)
          : const FluxMetricTokens(),
    );
  }

  static FluxThemeTokens _defaultsFor(Brightness b) =>
      b == Brightness.dark ? defaultDark() : defaultLight();

  static Map<String, dynamic> _mapOr(dynamic v) =>
      v is Map<String, dynamic> ? v : const {};

  static Color _hexOr(dynamic v, Color fallback) =>
      v is String ? (tryParseHexColor(v) ?? fallback) : fallback;

  /// 解析 6/8 位 hex 颜色（可带前导 `#`）；非法或非字符串返回 null。
  static Color? tryParseHexColor(dynamic v) {
    if (v is! String) return null;
    var s = v.trim();
    if (s.startsWith('#')) s = s.substring(1);
    if (s.length == 6) s = 'ff$s';
    if (s.length != 8) return null;
    final n = int.tryParse(s, radix: 16);
    return n == null ? null : Color(n);
  }

  // ═══════════════════════════════════════════════════════════
  //  copyWith
  // ═══════════════════════════════════════════════════════════

  FluxThemeTokens copyWith({
    String? name,
    String? author,
    Brightness? appearance,
    Color? background,
    Color? surface1,
    Color? surface2,
    Color? surface3,
    Color? elementHover,
    Color? elementSelected,
    Color? elementActive,
    Color? textPrimary,
    Color? textSecondary,
    Color? textMuted,
    Color? textDisabled,
    Color? border,
    Color? borderFocused,
    Color? accent,
    Color? accentHover,
    Color? accentBackground,
    Color? accentForeground,
    Color? inputBackground,
    Color? inputBorder,
    Color? inputFocusBorder,
    Color? inputFocusBackground,
    Color? dialogBackground,
    Color? dialogBarrier,
    Color? switchTrack,
    Color? switchThumb,
    Color? shadow,
    Color? statusSuccess,
    Color? statusWarning,
    Color? statusError,
    List<Color>? segmentPalette,
    FluxMetricTokens? metric,
  }) {
    return FluxThemeTokens(
      name: name ?? this.name,
      author: author ?? this.author,
      appearance: appearance ?? this.appearance,
      background: background ?? this.background,
      surface1: surface1 ?? this.surface1,
      surface2: surface2 ?? this.surface2,
      surface3: surface3 ?? this.surface3,
      elementHover: elementHover ?? this.elementHover,
      elementSelected: elementSelected ?? this.elementSelected,
      elementActive: elementActive ?? this.elementActive,
      textPrimary: textPrimary ?? this.textPrimary,
      textSecondary: textSecondary ?? this.textSecondary,
      textMuted: textMuted ?? this.textMuted,
      textDisabled: textDisabled ?? this.textDisabled,
      border: border ?? this.border,
      borderFocused: borderFocused ?? this.borderFocused,
      accent: accent ?? this.accent,
      accentHover: accentHover ?? this.accentHover,
      accentBackground: accentBackground ?? this.accentBackground,
      accentForeground: accentForeground ?? this.accentForeground,
      inputBackground: inputBackground ?? this.inputBackground,
      inputBorder: inputBorder ?? this.inputBorder,
      inputFocusBorder: inputFocusBorder ?? this.inputFocusBorder,
      inputFocusBackground: inputFocusBackground ?? this.inputFocusBackground,
      dialogBackground: dialogBackground ?? this.dialogBackground,
      dialogBarrier: dialogBarrier ?? this.dialogBarrier,
      switchTrack: switchTrack ?? this.switchTrack,
      switchThumb: switchThumb ?? this.switchThumb,
      shadow: shadow ?? this.shadow,
      statusSuccess: statusSuccess ?? this.statusSuccess,
      statusWarning: statusWarning ?? this.statusWarning,
      statusError: statusError ?? this.statusError,
      segmentPalette: segmentPalette ?? this.segmentPalette,
      metric: metric ?? this.metric,
    );
  }

  @override
  bool operator ==(Object other) {
    if (identical(this, other)) return true;
    return other is FluxThemeTokens &&
        name == other.name &&
        author == other.author &&
        appearance == other.appearance &&
        background == other.background &&
        surface1 == other.surface1 &&
        surface2 == other.surface2 &&
        surface3 == other.surface3 &&
        elementHover == other.elementHover &&
        elementSelected == other.elementSelected &&
        elementActive == other.elementActive &&
        textPrimary == other.textPrimary &&
        textSecondary == other.textSecondary &&
        textMuted == other.textMuted &&
        textDisabled == other.textDisabled &&
        border == other.border &&
        borderFocused == other.borderFocused &&
        accent == other.accent &&
        accentHover == other.accentHover &&
        accentBackground == other.accentBackground &&
        accentForeground == other.accentForeground &&
        inputBackground == other.inputBackground &&
        inputBorder == other.inputBorder &&
        inputFocusBorder == other.inputFocusBorder &&
        inputFocusBackground == other.inputFocusBackground &&
        dialogBackground == other.dialogBackground &&
        dialogBarrier == other.dialogBarrier &&
        switchTrack == other.switchTrack &&
        switchThumb == other.switchThumb &&
        shadow == other.shadow &&
        statusSuccess == other.statusSuccess &&
        statusWarning == other.statusWarning &&
        statusError == other.statusError &&
        metric == other.metric &&
        listEquals(segmentPalette, other.segmentPalette);
  }

  @override
  int get hashCode => Object.hashAll([
    name,
    author,
    appearance,
    background,
    surface1,
    surface2,
    surface3,
    elementHover,
    elementSelected,
    elementActive,
    textPrimary,
    textSecondary,
    textMuted,
    textDisabled,
    border,
    borderFocused,
    accent,
    accentHover,
    accentBackground,
    accentForeground,
    inputBackground,
    inputBorder,
    inputFocusBorder,
    inputFocusBackground,
    dialogBackground,
    dialogBarrier,
    switchTrack,
    switchThumb,
    shadow,
    statusSuccess,
    statusWarning,
    statusError,
    metric,
    Object.hashAll(segmentPalette),
  ]);
}
