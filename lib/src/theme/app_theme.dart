import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import 'flux_theme_tokens.dart';

/// MiSans 字体族名（与 pubspec.yaml 中声明的 family 一致）
const _fontFamily = 'MiSans';

/// 构建紧凑的按钮尺寸主题
ShadButtonSizesTheme _buttonSizes(FluxThemeTokens tokens) {
  final m = tokens.metric;
  return ShadButtonSizesTheme(
    regular: ShadButtonSizeTheme(
      height: m.buttonHeightMd,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
    ),
    sm: ShadButtonSizeTheme(
      height: m.buttonHeightSm,
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 2),
    ),
    lg: ShadButtonSizeTheme(
      height: m.buttonHeightLg,
      padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 6),
    ),
    icon: ShadButtonSizeTheme(
      height: m.buttonHeightMd,
      width: m.buttonHeightMd,
      padding: EdgeInsets.zero,
    ),
  );
}

// ═══════════════════════════════════════════════════════════
//  从 FluxThemeTokens 构建 ShadThemeData
// ═══════════════════════════════════════════════════════════

/// 缓存
FluxThemeTokens? _cachedTokens;
ShadThemeData? _cachedThemeData;

/// 从 [FluxThemeTokens] 构建 [ShadThemeData]。
///
/// 结果会被缓存，相同 tokens 不会重复构建。
ShadThemeData buildThemeFromTokens(FluxThemeTokens tokens) {
  if (identical(_cachedTokens, tokens) || _cachedTokens == tokens) {
    return _cachedThemeData!;
  }
  _cachedTokens = tokens;

  final isDark = tokens.appearance == Brightness.dark;
  final colorScheme = _buildColorScheme(tokens, isDark);

  if (isDark) {
    _cachedThemeData = ShadThemeData(
      brightness: Brightness.dark,
      colorScheme: colorScheme,
      radius: BorderRadius.circular(tokens.metric.radiusMd),
      textTheme: ShadTextTheme(family: _fontFamily),
      buttonSizesTheme: _buttonSizes(tokens),
      ghostButtonTheme: ShadButtonTheme(
        hoverBackgroundColor: tokens.elementHover,
      ),
      outlineButtonTheme: ShadButtonTheme(
        hoverBackgroundColor: tokens.elementHover,
      ),
      switchTheme: ShadSwitchTheme(
        thumbColor: tokens.switchThumb,
        uncheckedTrackColor: tokens.switchTrack,
      ),
      inputTheme: ShadInputTheme(cursorColor: tokens.accent),
      primaryDialogTheme: ShadDialogTheme(
        backgroundColor: tokens.dialogBackground,
        border: Border.all(color: tokens.border, width: 1),
        shadows: [
          BoxShadow(
            color: tokens.shadow.withValues(alpha: tokens.metric.alphaShadowStrong),
            blurRadius: 24,
            offset: const Offset(0, 8),
          ),
        ],
      ),
      alertDialogTheme: ShadDialogTheme(
        backgroundColor: tokens.dialogBackground,
        border: Border.all(color: tokens.border, width: 1),
        shadows: [
          BoxShadow(
            color: tokens.shadow.withValues(alpha: tokens.metric.alphaShadowStrong),
            blurRadius: 24,
            offset: const Offset(0, 8),
          ),
        ],
      ),
      primaryToastTheme: _primaryToastTheme(tokens),
      destructiveToastTheme: _destructiveToastTheme(tokens, colorScheme),
    );
  } else {
    _cachedThemeData = ShadThemeData(
      brightness: Brightness.light,
      colorScheme: colorScheme,
      radius: BorderRadius.circular(tokens.metric.radiusMd),
      textTheme: ShadTextTheme(family: _fontFamily),
      buttonSizesTheme: _buttonSizes(tokens),
      ghostButtonTheme: ShadButtonTheme(
        hoverBackgroundColor: tokens.elementHover,
      ),
      outlineButtonTheme: ShadButtonTheme(
        hoverBackgroundColor: tokens.elementHover,
      ),
      primaryToastTheme: _primaryToastTheme(tokens),
      destructiveToastTheme: _destructiveToastTheme(tokens, colorScheme),
    );
  }

  return _cachedThemeData!;
}

/// Toast 通用布局（紧凑卡片，统一宽度，右下角堆叠美观）
const _toastPadding = EdgeInsets.symmetric(horizontal: 16, vertical: 12);
const _toastConstraints = BoxConstraints(minWidth: 320, maxWidth: 380);

List<BoxShadow> _toastShadows(FluxThemeTokens tokens) => [
  BoxShadow(
    color: tokens.shadow.withValues(alpha: tokens.metric.alphaShadowSoft),
    blurRadius: 16,
    offset: const Offset(0, 6),
  ),
  BoxShadow(
    color: tokens.shadow.withValues(alpha: tokens.metric.alphaShadowFaint),
    blurRadius: 4,
    offset: const Offset(0, 2),
  ),
];

/// 普通 toast — 悬浮卡片底色 + 紧凑排版
ShadToastTheme _primaryToastTheme(FluxThemeTokens tokens) {
  return ShadToastTheme(
    backgroundColor: tokens.dialogBackground,
    border: ShadBorder.all(color: tokens.border, width: 1),
    radius: BorderRadius.circular(tokens.metric.radiusDialog),
    shadows: _toastShadows(tokens),
    padding: _toastPadding,
    constraints: _toastConstraints,
    titleStyle: TextStyle(
      fontFamily: _fontFamily,
      fontSize: 13,
      fontWeight: FontWeight.w500,
      color: tokens.textPrimary,
    ),
    descriptionStyle: TextStyle(
      fontFamily: _fontFamily,
      fontSize: 12,
      color: tokens.textSecondary,
    ),
  );
}

/// 错误 toast — 保留 destructive 底色，排版与普通 toast 一致
ShadToastTheme _destructiveToastTheme(
  FluxThemeTokens tokens,
  ShadColorScheme colorScheme,
) {
  return ShadToastTheme(
    backgroundColor: colorScheme.destructive,
    border: ShadBorder.all(
      color: colorScheme.destructive.withValues(alpha: 0.5),
      width: 1,
    ),
    radius: BorderRadius.circular(tokens.metric.radiusDialog),
    shadows: _toastShadows(tokens),
    padding: _toastPadding,
    constraints: _toastConstraints,
    titleStyle: TextStyle(
      fontFamily: _fontFamily,
      fontSize: 13,
      fontWeight: FontWeight.w500,
      color: colorScheme.destructiveForeground,
    ),
    descriptionStyle: TextStyle(
      fontFamily: _fontFamily,
      fontSize: 12,
      color: colorScheme.destructiveForeground.withValues(alpha: 0.9),
    ),
  );
}

/// 从 Token 构建 ShadColorScheme
ShadColorScheme _buildColorScheme(FluxThemeTokens tokens, bool isDark) {
  // 获取基底 scheme（用于保留 shadcn 的 destructive 等未覆盖色）
  final base = isDark
      ? _baseDarkScheme(tokens.accent)
      : _baseLightScheme(tokens.accent);

  return base.copyWith(
    background: tokens.background,
    foreground: tokens.textPrimary,
    primary: tokens.accent,
    primaryForeground: tokens.accentForeground,
    card: tokens.surface1,
    cardForeground: tokens.textPrimary,
    popover: tokens.surface1,
    popoverForeground: tokens.textPrimary,
    secondary: tokens.surface2,
    secondaryForeground: tokens.textPrimary,
    muted: tokens.surface2,
    mutedForeground: tokens.textSecondary,
    accent: tokens.surface2,
    accentForeground: tokens.textPrimary,
    border: tokens.border,
    input: tokens.border,
    ring: tokens.accent,
    selection: tokens.elementSelected,
  );
}

/// 获取基底 dark scheme（保留 destructive 等）
ShadColorScheme _baseDarkScheme(Color accent) {
  // 使用 Zinc 基底（中性灰，最适合覆盖）
  return const ShadZincColorScheme.dark();
}

/// 获取基底 light scheme
ShadColorScheme _baseLightScheme(Color accent) {
  return const ShadZincColorScheme.light();
}
