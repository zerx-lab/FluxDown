import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:shared_preferences/shared_preferences.dart';

import '../i18n/locale_provider.dart';
import '../services/log_service.dart';
import 'flux_theme_tokens.dart';

// ═══════════════════════════════════════════════════════════
//  内置主题定义
// ═══════════════════════════════════════════════════════════

/// 内置主题 ID — 每个 ID 对应一套完整的 FluxThemeTokens
enum BuiltinThemeId { defaultDark, defaultLight, midnightBlue, nord, warmLight }

/// 内置主题注册表条目
class BuiltinThemeEntry {
  final BuiltinThemeId id;
  final Brightness appearance;

  /// 不带强调色的固定预览色（用于主题卡片中显示代表色）
  final Color previewBg;
  final Color previewAccent;

  /// 生成完整 token 的工厂（支持传入强调色覆盖）
  final FluxThemeTokens Function({Color accent}) _factory;

  const BuiltinThemeEntry._({
    required this.id,
    required this.appearance,
    required this.previewBg,
    required this.previewAccent,
    required FluxThemeTokens Function({Color accent}) factory,
  }) : _factory = factory;

  FluxThemeTokens build({Color? accent}) =>
      accent != null ? _factory(accent: accent) : _factory();
}

/// 所有内置主题（顺序即 UI 显示顺序）
final builtinThemes = <BuiltinThemeEntry>[
  BuiltinThemeEntry._(
    id: BuiltinThemeId.defaultDark,
    appearance: Brightness.dark,
    previewBg: const Color(0xFF1C1C1E),
    previewAccent: const Color(0xFF3B82F6),
    factory: FluxThemeTokens.defaultDark,
  ),
  BuiltinThemeEntry._(
    id: BuiltinThemeId.defaultLight,
    appearance: Brightness.light,
    previewBg: const Color(0xFFF8F9FA),
    previewAccent: const Color(0xFF3B82F6),
    factory: FluxThemeTokens.defaultLight,
  ),
  BuiltinThemeEntry._(
    id: BuiltinThemeId.midnightBlue,
    appearance: Brightness.dark,
    previewBg: const Color(0xFF0F172A),
    previewAccent: const Color(0xFF60A5FA),
    factory: FluxThemeTokens.midnightBlue,
  ),
  BuiltinThemeEntry._(
    id: BuiltinThemeId.nord,
    appearance: Brightness.dark,
    previewBg: const Color(0xFF2E3440),
    previewAccent: const Color(0xFF88C0D0),
    factory: FluxThemeTokens.nord,
  ),
  BuiltinThemeEntry._(
    id: BuiltinThemeId.warmLight,
    appearance: Brightness.light,
    previewBg: const Color(0xFFFFFBEB),
    previewAccent: const Color(0xFFE11D48),
    factory: FluxThemeTokens.warmLight,
  ),
];

// ═══════════════════════════════════════════════════════════
//  强调色方案（快速切换强调色的简化入口）
// ═══════════════════════════════════════════════════════════

enum AppColorScheme {
  blue(Color(0xFF3B82F6)),
  green(Color(0xFF22C55E)),
  violet(Color(0xFF8B5CF6)),
  rose(Color(0xFFF43F5E)),
  custom(Color(0xFF6366F1));

  final Color previewColor;
  const AppColorScheme(this.previewColor);
}

extension AppColorSchemeI18n on AppColorScheme {
  String get label {
    final s = currentS;
    return switch (this) {
      AppColorScheme.blue => s.colorBlue,
      AppColorScheme.green => s.colorGreen,
      AppColorScheme.violet => s.colorViolet,
      AppColorScheme.rose => s.colorRose,
      AppColorScheme.custom => s.colorCustom,
    };
  }
}

// ═══════════════════════════════════════════════════════════
//  i18n 工具
// ═══════════════════════════════════════════════════════════

extension BuiltinThemeI18n on BuiltinThemeId {
  String get label {
    final s = currentS;
    return switch (this) {
      BuiltinThemeId.defaultDark => s.themeDefaultDark,
      BuiltinThemeId.defaultLight => s.themeDefaultLight,
      BuiltinThemeId.midnightBlue => s.themeMidnightBlue,
      BuiltinThemeId.nord => s.themeNord,
      BuiltinThemeId.warmLight => s.themeWarmLight,
    };
  }
}

// ═══════════════════════════════════════════════════════════
//  导入的自定义主题条目
// ═══════════════════════════════════════════════════════════

class ImportedThemeEntry {
  final String id;
  final FluxThemeTokens tokens;

  const ImportedThemeEntry({required this.id, required this.tokens});

  Brightness get appearance => tokens.appearance;

  Map<String, dynamic> toJson() => {'id': id, 'tokens': tokens.toJson()};

  factory ImportedThemeEntry.fromJson(Map<String, dynamic> json) {
    return ImportedThemeEntry(
      id: json['id'] as String,
      tokens: FluxThemeTokens.fromJson(json['tokens'] as Map<String, dynamic>),
    );
  }
}

// ═══════════════════════════════════════════════════════════
//  SharedPreferences 存储 key
// ═══════════════════════════════════════════════════════════

const _kThemeMode = 'theme_mode';
const _kSelectedTheme = 'selected_theme';
const _kColorScheme = 'color_scheme';
const _kCustomColor = 'custom_color';
const _kImportedThemes = 'imported_themes_v2';
const _kSelectedCustomDark = 'selected_custom_dark_id';
const _kSelectedCustomLight = 'selected_custom_light_id';
const _kUiScale = 'ui_scale';

// 旧版 key（迁移用）
const _kLegacyCustomThemeDark = 'custom_theme_dark_json';
const _kLegacyCustomThemeLight = 'custom_theme_light_json';
const _kPrefsInitTimeout = Duration(seconds: 3);

// ═══════════════════════════════════════════════════════════
//  ThemeProvider
// ═══════════════════════════════════════════════════════════

/// 全局主题管理器
///
/// 主题选择逻辑：
/// - [themeMode] = system 时，根据系统亮/暗自动选对应主题
///   - 暗色 → [selectedDarkTheme] 或 [_selectedCustomDarkId]
///   - 亮色 → [selectedLightTheme] 或 [_selectedCustomLightId]
/// - [themeMode] = light/dark 时，强制使用对应主题
///
/// 主题来源优先级：
/// 1. 选中的导入主题（[_selectedCustomDarkId] / [_selectedCustomLightId]）
/// 2. 内置主题 + 强调色覆盖
class ThemeProvider extends ChangeNotifier {
  ThemeMode _themeMode = ThemeMode.system;

  /// 用户选择的暗色主题和亮色主题（内置）
  BuiltinThemeId _selectedDarkTheme = BuiltinThemeId.defaultDark;
  BuiltinThemeId _selectedLightTheme = BuiltinThemeId.defaultLight;

  /// 强调色
  AppColorScheme _colorScheme = AppColorScheme.blue;
  Color _customColor = const Color(0xFF6366F1);

  /// 界面缩放比例（0.8 ~ 1.5，默认 1.0）
  double _uiScale = 1.0;

  /// 导入的自定义主题列表
  final List<ImportedThemeEntry> _importedThemes = [];

  /// 当前选中的自定义主题 ID（null = 使用内置主题）
  String? _selectedCustomDarkId;
  String? _selectedCustomLightId;

  /// 缓存
  FluxThemeTokens? _cachedTokens;
  bool _cachedIsDark = false;

  // ── Getters ──

  ThemeMode get themeMode => _themeMode;
  BuiltinThemeId get selectedDarkTheme => _selectedDarkTheme;
  BuiltinThemeId get selectedLightTheme => _selectedLightTheme;
  AppColorScheme get colorScheme => _colorScheme;
  Color get customColor => _customColor;
  double get uiScale => _uiScale;

  List<ImportedThemeEntry> get importedThemes =>
      List.unmodifiable(_importedThemes);

  List<ImportedThemeEntry> importedThemesFor(Brightness appearance) =>
      _importedThemes.where((e) => e.appearance == appearance).toList();

  String? get selectedCustomDarkId => _selectedCustomDarkId;
  String? get selectedCustomLightId => _selectedCustomLightId;

  bool get isCustomDarkActive =>
      _selectedCustomDarkId != null &&
      _importedThemes.any((e) => e.id == _selectedCustomDarkId);
  bool get isCustomLightActive =>
      _selectedCustomLightId != null &&
      _importedThemes.any((e) => e.id == _selectedCustomLightId);

  ImportedThemeEntry? get activeCustomDark => _selectedCustomDarkId == null
      ? null
      : _importedThemes.where((e) => e.id == _selectedCustomDarkId).firstOrNull;
  ImportedThemeEntry? get activeCustomLight => _selectedCustomLightId == null
      ? null
      : _importedThemes
            .where((e) => e.id == _selectedCustomLightId)
            .firstOrNull;

  // 向后兼容旧 API
  FluxThemeTokens? get customDarkTokens => activeCustomDark?.tokens;
  FluxThemeTokens? get customLightTokens => activeCustomLight?.tokens;
  bool get hasCustomTheme => _importedThemes.isNotEmpty;
  String? get customThemeName =>
      activeCustomDark?.tokens.name ?? activeCustomLight?.tokens.name;

  Color get activePreviewColor => _colorScheme == AppColorScheme.custom
      ? _customColor
      : _colorScheme.previewColor;

  /// 当前亮/暗模式下生效的内置主题 ID
  BuiltinThemeId activeBuiltinTheme(bool dark) =>
      dark ? _selectedDarkTheme : _selectedLightTheme;

  // ── 核心：计算当前 token ──

  FluxThemeTokens activeTokens(BuildContext context) {
    final dark = isDark(context);
    if (_cachedTokens != null && _cachedIsDark == dark) return _cachedTokens!;
    _cachedIsDark = dark;
    _cachedTokens = _computeTokens(dark);
    return _cachedTokens!;
  }

  /// 免 BuildContext 解析指定亮/暗模式下的生效 token
  /// （供离屏渲染等无 context 场景使用，如 Win32 Toast 卡片）。
  FluxThemeTokens tokensFor({required bool dark}) => _computeTokens(dark);

  FluxThemeTokens _computeTokens(bool dark) {
    // 优先级 1：选中的导入主题
    final customId = dark ? _selectedCustomDarkId : _selectedCustomLightId;
    if (customId != null) {
      final entry = _importedThemes.where((e) => e.id == customId).firstOrNull;
      if (entry != null) return entry.tokens;
    }

    // 优先级 2：内置主题 + 强调色覆盖
    final themeId = dark ? _selectedDarkTheme : _selectedLightTheme;
    final entry = builtinThemes.firstWhere((e) => e.id == themeId);
    final accent = _resolveAccentColor();
    return entry.build(accent: accent);
  }

  Color _resolveAccentColor() {
    return _colorScheme == AppColorScheme.custom
        ? _customColor
        : _colorScheme.previewColor;
  }

  // ═══════════════════════════════════════════════════════════
  //  初始化 & 持久化
  // ═══════════════════════════════════════════════════════════

// 失败时直接使用默认主题配置，不再阻塞进入 UI
  Future<void> init() async {
    try {
      final prefs = await SharedPreferences.getInstance().timeout(
        _kPrefsInitTimeout,
      );

      // 主题模式
      final modeStr = prefs.getString(_kThemeMode);
      if (modeStr != null) {
        _themeMode = ThemeMode.values.firstWhere(
          (m) => m.name == modeStr,
          orElse: () => ThemeMode.system,
        );
      }

      // 选中的主题
      final themeStr = prefs.getString(_kSelectedTheme);
      if (themeStr != null) {
        _loadSelectedThemes(themeStr);
      }

      // 强调色方案
      final schemeStr = prefs.getString(_kColorScheme);
      if (schemeStr != null) {
        _colorScheme = AppColorScheme.values.firstWhere(
          (s) => s.name == schemeStr,
          orElse: () => AppColorScheme.blue,
        );
      }

      // 自定义颜色
      final customHex = prefs.getString(_kCustomColor);
      if (customHex != null) {
        final parsed = int.tryParse(customHex, radix: 16);
        if (parsed != null) _customColor = Color(parsed);
      }

      // 导入的主题列表
      _loadImportedThemes(prefs);

      // 迁移旧版单主题数据
      _migrateV1(prefs);

      // 选中的自定义主题 ID
      _selectedCustomDarkId = prefs.getString(_kSelectedCustomDark);
      _selectedCustomLightId = prefs.getString(_kSelectedCustomLight);

      // 界面缩放
      final scaleStr = prefs.getString(_kUiScale);
      if (scaleStr != null) {
        final parsed = double.tryParse(scaleStr);
        if (parsed != null && parsed >= 0.8 && parsed <= 1.5) {
          _uiScale = parsed;
        }
      }
    } catch (e, stack) {
      logError('ThemeProvider', 'init failed, using defaults', e, stack);
    }
  }

  /// 迁移旧版存储（单个 custom_theme_dark_json / custom_theme_light_json）
  void _migrateV1(SharedPreferences prefs) {
    var migrated = false;
    final darkJson = prefs.getString(_kLegacyCustomThemeDark);
    if (darkJson != null) {
      try {
        final tokens = FluxThemeTokens.fromJson(
          jsonDecode(darkJson) as Map<String, dynamic>,
        );
        final id = _generateId();
        _importedThemes.add(ImportedThemeEntry(id: id, tokens: tokens));
        _selectedCustomDarkId = id;
        migrated = true;
      } catch (_) {}
      prefs.remove(_kLegacyCustomThemeDark);
    }
    final lightJson = prefs.getString(_kLegacyCustomThemeLight);
    if (lightJson != null) {
      try {
        final tokens = FluxThemeTokens.fromJson(
          jsonDecode(lightJson) as Map<String, dynamic>,
        );
        final id = _generateId();
        _importedThemes.add(ImportedThemeEntry(id: id, tokens: tokens));
        _selectedCustomLightId = id;
        migrated = true;
      } catch (_) {}
      prefs.remove(_kLegacyCustomThemeLight);
    }
    // 清除旧的 use_custom 标志
    prefs.remove('use_custom_dark');
    prefs.remove('use_custom_light');

    if (migrated) {
      _persistImportedThemes();
      if (_selectedCustomDarkId != null) {
        _persist(_kSelectedCustomDark, _selectedCustomDarkId!);
      }
      if (_selectedCustomLightId != null) {
        _persist(_kSelectedCustomLight, _selectedCustomLightId!);
      }
    }
  }

  // ── 主题模式 ──

  void setThemeMode(ThemeMode mode) {
    if (_themeMode == mode) return;
    _themeMode = mode;
    _invalidateCache();
    notifyListeners();
    _persist(_kThemeMode, mode.name);
  }

  // ── 主题选择 ──

  /// 选择暗色主题（从内置主题中选）
  void setDarkTheme(BuiltinThemeId id) {
    final changed = _selectedDarkTheme != id || _selectedCustomDarkId != null;
    if (!changed) return;
    _selectedDarkTheme = id;
    _selectedCustomDarkId = null; // 取消自定义主题选择
    _persistRemove(_kSelectedCustomDark);
    _invalidateCache();
    notifyListeners();
    _persistSelectedThemes();
  }

  /// 选择亮色主题（从内置主题中选）
  void setLightTheme(BuiltinThemeId id) {
    final changed = _selectedLightTheme != id || _selectedCustomLightId != null;
    if (!changed) return;
    _selectedLightTheme = id;
    _selectedCustomLightId = null;
    _persistRemove(_kSelectedCustomLight);
    _invalidateCache();
    notifyListeners();
    _persistSelectedThemes();
  }

  // ── 强调色 ──

  void setColorScheme(AppColorScheme scheme) {
    if (_colorScheme == scheme) return;
    _colorScheme = scheme;
    _invalidateCache();
    notifyListeners();
    _persist(_kColorScheme, scheme.name);
  }

  // ── 界面缩放 ──

  void setUiScale(double scale) {
    // 限制范围 0.8 ~ 1.5
    final clamped = scale.clamp(0.8, 1.5);
    // 四舍五入到 0.1
    final rounded = (clamped * 10).roundToDouble() / 10;
    if (_uiScale == rounded) return;
    _uiScale = rounded;
    notifyListeners();
    _persist(_kUiScale, rounded.toString());
  }

  void setCustomColor(Color color) {
    _customColor = color;
    if (_colorScheme != AppColorScheme.custom) {
      _colorScheme = AppColorScheme.custom;
      _persist(_kColorScheme, AppColorScheme.custom.name);
    }
    _invalidateCache();
    notifyListeners();
    _persist(_kCustomColor, color.toARGB32().toRadixString(16).padLeft(8, '0'));
  }

  // ── 便捷操作 ──

  void toggleTheme(BuildContext context) {
    final brightness = MediaQuery.platformBrightnessOf(context);
    final currentDark =
        _themeMode == ThemeMode.dark ||
        (_themeMode == ThemeMode.system && brightness == Brightness.dark);
    setThemeMode(currentDark ? ThemeMode.light : ThemeMode.dark);
  }

  bool isDark(BuildContext context) {
    if (_themeMode == ThemeMode.system) {
      return MediaQuery.platformBrightnessOf(context) == Brightness.dark;
    }
    return _themeMode == ThemeMode.dark;
  }

  // ═══════════════════════════════════════════════════════════
  //  导入主题管理（多主题）
  // ═══════════════════════════════════════════════════════════

  /// 添加导入的主题并自动选中
  String addImportedTheme(FluxThemeTokens tokens) {
    final id = _generateId();
    _importedThemes.add(ImportedThemeEntry(id: id, tokens: tokens));

    // 自动选中
    if (tokens.appearance == Brightness.dark) {
      _selectedCustomDarkId = id;
      _persist(_kSelectedCustomDark, id);
    } else {
      _selectedCustomLightId = id;
      _persist(_kSelectedCustomLight, id);
    }

    _persistImportedThemes();
    _invalidateCache();
    notifyListeners();
    return id;
  }

  /// 删除导入的主题
  void removeImportedTheme(String id) {
    _importedThemes.removeWhere((e) => e.id == id);

    // 如果删除的是当前选中的，回退到内置
    if (_selectedCustomDarkId == id) {
      _selectedCustomDarkId = null;
      _persistRemove(_kSelectedCustomDark);
    }
    if (_selectedCustomLightId == id) {
      _selectedCustomLightId = null;
      _persistRemove(_kSelectedCustomLight);
    }

    _persistImportedThemes();
    _invalidateCache();
    notifyListeners();
  }

  /// 选中某个导入的主题
  void selectImportedTheme(String id) {
    final entry = _importedThemes.where((e) => e.id == id).firstOrNull;
    if (entry == null) return;

    if (entry.appearance == Brightness.dark) {
      if (_selectedCustomDarkId == id) return;
      _selectedCustomDarkId = id;
      _persist(_kSelectedCustomDark, id);
    } else {
      if (_selectedCustomLightId == id) return;
      _selectedCustomLightId = id;
      _persist(_kSelectedCustomLight, id);
    }

    _invalidateCache();
    notifyListeners();
  }

  // 向后兼容旧 API — setCustomTheme / clearCustomTheme / activateCustomTheme

  /// 设置自定义主题（向后兼容，内部转为 addImportedTheme）
  void setCustomTheme({FluxThemeTokens? dark, FluxThemeTokens? light}) {
    if (dark != null) addImportedTheme(dark);
    if (light != null) addImportedTheme(light);
  }

  /// 清除某侧的自定义主题（删除当前选中的导入主题）
  void clearCustomTheme({required bool dark}) {
    final id = dark ? _selectedCustomDarkId : _selectedCustomLightId;
    if (id != null) removeImportedTheme(id);
  }

  /// 激活自定义主题（向后兼容）
  void activateCustomTheme({required bool dark}) {
    // 已经选中了就不用处理
  }

  void updateToken({
    required bool dark,
    required FluxThemeTokens Function(FluxThemeTokens) updater,
  }) {
    final accent = _resolveAccentColor();
    if (dark) {
      // 如果当前选中了导入主题，更新它
      final customId = _selectedCustomDarkId;
      if (customId != null) {
        final idx = _importedThemes.indexWhere((e) => e.id == customId);
        if (idx >= 0) {
          final updated = updater(_importedThemes[idx].tokens);
          _importedThemes[idx] = ImportedThemeEntry(
            id: customId,
            tokens: updated,
          );
          _persistImportedThemes();
          _invalidateCache();
          notifyListeners();
          return;
        }
      }
      // 否则基于内置主题创建新的导入主题
      final themeEntry = builtinThemes.firstWhere(
        (e) => e.id == _selectedDarkTheme,
      );
      final base = themeEntry.build(accent: accent);
      addImportedTheme(updater(base));
    } else {
      final customId = _selectedCustomLightId;
      if (customId != null) {
        final idx = _importedThemes.indexWhere((e) => e.id == customId);
        if (idx >= 0) {
          final updated = updater(_importedThemes[idx].tokens);
          _importedThemes[idx] = ImportedThemeEntry(
            id: customId,
            tokens: updated,
          );
          _persistImportedThemes();
          _invalidateCache();
          notifyListeners();
          return;
        }
      }
      final themeEntry = builtinThemes.firstWhere(
        (e) => e.id == _selectedLightTheme,
      );
      final base = themeEntry.build(accent: accent);
      addImportedTheme(updater(base));
    }
  }

  void resetToDefault() {
    _importedThemes.clear();
    _selectedCustomDarkId = null;
    _selectedCustomLightId = null;
    _selectedDarkTheme = BuiltinThemeId.defaultDark;
    _selectedLightTheme = BuiltinThemeId.defaultLight;
    _colorScheme = AppColorScheme.blue;
    _uiScale = 1.0;
    _invalidateCache();
    notifyListeners();
    _persist(_kColorScheme, AppColorScheme.blue.name);
    _persistRemove(_kImportedThemes);
    _persistRemove(_kSelectedCustomDark);
    _persistRemove(_kSelectedCustomLight);
    _persistRemove(_kUiScale);
    _persistSelectedThemes();
  }

  // ═══════════════════════════════════════════════════════════
  //  主题导入 / 导出
  // ═══════════════════════════════════════════════════════════

  String exportThemeJson(FluxThemeTokens tokens) {
    return const JsonEncoder.withIndent('  ').convert(tokens.toJson());
  }

  FluxThemeTokens importThemeJson(String jsonStr) {
    final json = jsonDecode(jsonStr) as Map<String, dynamic>;
    return FluxThemeTokens.fromJson(json);
  }

  FluxThemeTokens getExportableTokens(bool dark) {
    return _computeTokens(dark);
  }

  // ═══════════════════════════════════════════════════════════
  //  内部辅助
  // ═══════════════════════════════════════════════════════════

  void _invalidateCache() {
    _cachedTokens = null;
  }

  /// 持久化选中主题：格式 "darkId:lightId"
  void _persistSelectedThemes() {
    _persist(
      _kSelectedTheme,
      '${_selectedDarkTheme.name}:${_selectedLightTheme.name}',
    );
  }

  void _loadSelectedThemes(String str) {
    final parts = str.split(':');
    if (parts.length == 2) {
      _selectedDarkTheme = BuiltinThemeId.values.firstWhere(
        (e) => e.name == parts[0],
        orElse: () => BuiltinThemeId.defaultDark,
      );
      _selectedLightTheme = BuiltinThemeId.values.firstWhere(
        (e) => e.name == parts[1],
        orElse: () => BuiltinThemeId.defaultLight,
      );
    }
  }

  void _loadImportedThemes(SharedPreferences prefs) {
    final jsonStr = prefs.getString(_kImportedThemes);
    if (jsonStr == null) return;
    try {
      final list = jsonDecode(jsonStr) as List<dynamic>;
      for (final item in list) {
        if (item is Map<String, dynamic>) {
          _importedThemes.add(ImportedThemeEntry.fromJson(item));
        }
      }
    } catch (_) {}
  }

  void _persistImportedThemes() {
    final json = jsonEncode(_importedThemes.map((e) => e.toJson()).toList());
    _persist(_kImportedThemes, json);
  }

  String _generateId() {
    return '${DateTime.now().millisecondsSinceEpoch}_${_importedThemes.length}';
  }

  Future<void> _persist(String key, String value) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(key, value);
  }

  Future<void> _persistRemove(String key) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.remove(key);
  }
}
