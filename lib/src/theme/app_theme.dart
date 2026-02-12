import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'theme_provider.dart';

/// MiSans 字体族名（与 pubspec.yaml 中声明的 family 一致）
const _fontFamily = 'MiSans';

/// 构建紧凑的按钮尺寸主题（降低所有变体高度）
const _buttonSizes = ShadButtonSizesTheme(
  regular: ShadButtonSizeTheme(
    height: 32,
    padding: EdgeInsets.symmetric(horizontal: 12, vertical: 4),
  ),
  sm: ShadButtonSizeTheme(
    height: 28,
    padding: EdgeInsets.symmetric(horizontal: 10, vertical: 2),
  ),
  lg: ShadButtonSizeTheme(
    height: 36,
    padding: EdgeInsets.symmetric(horizontal: 20, vertical: 6),
  ),
  icon: ShadButtonSizeTheme(height: 32, width: 32, padding: EdgeInsets.zero),
);

/// 根据 AppColorScheme 获取 shadcn 颜色方案（亮色）
ShadColorScheme _lightColorScheme(AppColorScheme scheme) {
  return switch (scheme) {
    AppColorScheme.blue => const ShadBlueColorScheme.light(),
    AppColorScheme.green => const ShadGreenColorScheme.light(),
    AppColorScheme.violet => const ShadVioletColorScheme.light(),
    AppColorScheme.rose => const ShadRoseColorScheme.light(),
    AppColorScheme.orange => const ShadOrangeColorScheme.light(),
    AppColorScheme.red => const ShadRedColorScheme.light(),
    AppColorScheme.yellow => const ShadYellowColorScheme.light(),
    AppColorScheme.slate => const ShadSlateColorScheme.light(),
    AppColorScheme.zinc => const ShadZincColorScheme.light(),
    AppColorScheme.gray => const ShadGrayColorScheme.light(),
    AppColorScheme.neutral => const ShadNeutralColorScheme.light(),
    AppColorScheme.stone => const ShadStoneColorScheme.light(),
  };
}

/// 根据 AppColorScheme 获取 shadcn 颜色方案（暗色）
ShadColorScheme _darkColorScheme(AppColorScheme scheme) {
  return switch (scheme) {
    AppColorScheme.blue => const ShadBlueColorScheme.dark(),
    AppColorScheme.green => const ShadGreenColorScheme.dark(),
    AppColorScheme.violet => const ShadVioletColorScheme.dark(),
    AppColorScheme.rose => const ShadRoseColorScheme.dark(),
    AppColorScheme.orange => const ShadOrangeColorScheme.dark(),
    AppColorScheme.red => const ShadRedColorScheme.dark(),
    AppColorScheme.yellow => const ShadYellowColorScheme.dark(),
    AppColorScheme.slate => const ShadSlateColorScheme.dark(),
    AppColorScheme.zinc => const ShadZincColorScheme.dark(),
    AppColorScheme.gray => const ShadGrayColorScheme.dark(),
    AppColorScheme.neutral => const ShadNeutralColorScheme.dark(),
    AppColorScheme.stone => const ShadStoneColorScheme.dark(),
  };
}

/// 缓存当前颜色方案对应的主题数据
AppColorScheme? _cachedScheme;
ShadThemeData? _cachedLight;
ShadThemeData? _cachedDark;

/// 清除主题缓存（颜色方案变更时调用）
void invalidateThemeCache() {
  _cachedScheme = null;
  _cachedLight = null;
  _cachedDark = null;
}

/// Apple 风格暗色模式的色值常量
const _darkSurface1 = Color(0xFF2C2C2E);
const _darkHoverBg = Color(0xFF363638);
const _darkBorder = Color(0xFF48484A);
const _darkSwitchTrack = Color(0xFF636366);

void _ensureCache(AppColorScheme scheme) {
  if (_cachedScheme == scheme) return;
  _cachedScheme = scheme;

  // Override shadcn's blue-tinted dark palette with neutral Apple-style grays.
  // Default shadcn dark schemes (e.g. ShadBlueColorScheme.dark) use dark blues
  // like 0xff020817 / 0xff1e293b which look "dirty" against our neutral gray
  // surfaces.  We keep `primary` and `primaryForeground` from the original
  // scheme so accent colors still reflect the user's chosen theme color.
  const bg = Color(0xFF1C1C1E);
  const fg = Color(0xFFF5F5F7);
  final darkColorScheme = _darkColorScheme(scheme).copyWith(
    background: bg,
    foreground: fg,
    card: _darkSurface1,
    cardForeground: fg,
    popover: _darkSurface1,
    popoverForeground: fg,
    secondary: const Color(0xFF3A3A3C),
    secondaryForeground: fg,
    muted: const Color(0xFF3A3A3C),
    mutedForeground: const Color(0xFFA1A1A6),
    accent: const Color(0xFF3A3A3C),
    accentForeground: fg,
    border: _darkBorder,
    input: _darkBorder,
    ring: _darkColorScheme(scheme).primary,
    selection: const Color(0xFF48484A),
  );

  _cachedLight = ShadThemeData(
    brightness: Brightness.light,
    colorScheme: _lightColorScheme(scheme),
    textTheme: ShadTextTheme(family: _fontFamily),
    buttonSizesTheme: _buttonSizes,
  );
  _cachedDark = ShadThemeData(
    brightness: Brightness.dark,
    colorScheme: darkColorScheme,
    textTheme: ShadTextTheme(family: _fontFamily),
    buttonSizesTheme: _buttonSizes,
    // ── Ghost/Outline 按钮 hover 适配 Apple 深灰层级 ──
    ghostButtonTheme: const ShadButtonTheme(hoverBackgroundColor: _darkHoverBg),
    outlineButtonTheme: const ShadButtonTheme(
      hoverBackgroundColor: _darkHoverBg,
    ),
    // ── Switch (Apple 风格) ──
    switchTheme: const ShadSwitchTheme(
      thumbColor: Colors.white,
      uncheckedTrackColor: _darkSwitchTrack,
    ),
    // ── Input (Apple 风格深灰背景 + 可见边框) ──
    inputTheme: ShadInputTheme(cursorColor: darkColorScheme.primary),
    // ── Dialog (使用 surface1 背景 + 清晰边框) ──
    primaryDialogTheme: ShadDialogTheme(
      backgroundColor: _darkSurface1,
      border: Border.all(color: _darkBorder, width: 1),
      shadows: const [
        BoxShadow(
          color: Color(0x40000000),
          blurRadius: 24,
          offset: Offset(0, 8),
        ),
      ],
    ),
    alertDialogTheme: ShadDialogTheme(
      backgroundColor: _darkSurface1,
      border: Border.all(color: _darkBorder, width: 1),
      shadows: const [
        BoxShadow(
          color: Color(0x40000000),
          blurRadius: 24,
          offset: Offset(0, 8),
        ),
      ],
    ),
  );
}

ShadThemeData buildLightTheme([AppColorScheme scheme = AppColorScheme.blue]) {
  _ensureCache(scheme);
  return _cachedLight!;
}

ShadThemeData buildDarkTheme([AppColorScheme scheme = AppColorScheme.blue]) {
  _ensureCache(scheme);
  return _cachedDark!;
}
