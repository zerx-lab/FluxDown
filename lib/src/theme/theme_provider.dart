import 'package:flutter/material.dart';
import 'package:shared_preferences/shared_preferences.dart';
import '../i18n/locale_provider.dart';
import 'app_theme.dart';

/// 支持的主题色方案
enum AppColorScheme {
  blue(Color(0xFF3B82F6)),
  green(Color(0xFF22C55E)),
  violet(Color(0xFF8B5CF6)),
  rose(Color(0xFFF43F5E)),
  orange(Color(0xFFF97316)),
  red(Color(0xFFEF4444)),
  yellow(Color(0xFFEAB308)),
  slate(Color(0xFF64748B)),
  zinc(Color(0xFF71717A)),
  gray(Color(0xFF6B7280)),
  neutral(Color(0xFF737373)),
  stone(Color(0xFF78716C));

  final Color previewColor;
  const AppColorScheme(this.previewColor);
}

/// 国际化颜色名称
extension AppColorSchemeI18n on AppColorScheme {
  String get label {
    final s = currentS;
    return switch (this) {
      AppColorScheme.blue => s.colorBlue,
      AppColorScheme.green => s.colorGreen,
      AppColorScheme.violet => s.colorViolet,
      AppColorScheme.rose => s.colorRose,
      AppColorScheme.orange => s.colorOrange,
      AppColorScheme.red => s.colorRed,
      AppColorScheme.yellow => s.colorYellow,
      AppColorScheme.slate => s.colorSlate,
      AppColorScheme.zinc => s.colorZinc,
      AppColorScheme.gray => s.colorGray,
      AppColorScheme.neutral => s.colorNeutral,
      AppColorScheme.stone => s.colorStone,
    };
  }
}

/// SharedPreferences 存储 key
const _kThemeMode = 'theme_mode';
const _kColorScheme = 'color_scheme';

/// 全局主题模式 + 颜色方案管理（带 SharedPreferences 持久化）
class ThemeProvider extends ChangeNotifier {
  ThemeMode _themeMode = ThemeMode.system;
  AppColorScheme _colorScheme = AppColorScheme.blue;

  ThemeMode get themeMode => _themeMode;
  AppColorScheme get colorScheme => _colorScheme;

  /// 启动时调用，从 SharedPreferences 恢复上次保存的主题设置。
  /// 若无保存值则使用默认值（system + blue），不会 notifyListeners。
  Future<void> init() async {
    final prefs = await SharedPreferences.getInstance();

    final modeStr = prefs.getString(_kThemeMode);
    if (modeStr != null) {
      _themeMode = ThemeMode.values.firstWhere(
        (m) => m.name == modeStr,
        orElse: () => ThemeMode.system,
      );
    }

    final schemeStr = prefs.getString(_kColorScheme);
    if (schemeStr != null) {
      _colorScheme = AppColorScheme.values.firstWhere(
        (s) => s.name == schemeStr,
        orElse: () => AppColorScheme.blue,
      );
    }

    // 静默加载，不触发 rebuild（main.dart 会在 init 完成后才 runApp）
  }

  void setThemeMode(ThemeMode mode) {
    if (_themeMode == mode) return;
    _themeMode = mode;
    invalidateThemeCache();
    notifyListeners();
    _persist(_kThemeMode, mode.name);
  }

  void setColorScheme(AppColorScheme scheme) {
    if (_colorScheme == scheme) return;
    _colorScheme = scheme;
    invalidateThemeCache();
    notifyListeners();
    _persist(_kColorScheme, scheme.name);
  }

  void toggleTheme(BuildContext context) {
    final brightness = MediaQuery.platformBrightnessOf(context);
    final isDark =
        _themeMode == ThemeMode.dark ||
        (_themeMode == ThemeMode.system && brightness == Brightness.dark);
    setThemeMode(isDark ? ThemeMode.light : ThemeMode.dark);
  }

  /// 获取当前实际是否为暗色模式
  bool isDark(BuildContext context) {
    if (_themeMode == ThemeMode.system) {
      return MediaQuery.platformBrightnessOf(context) == Brightness.dark;
    }
    return _themeMode == ThemeMode.dark;
  }

  /// 异步写入 SharedPreferences（fire-and-forget，不阻塞 UI）
  Future<void> _persist(String key, String value) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(key, value);
  }
}
