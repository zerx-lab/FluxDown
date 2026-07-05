import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:shared_preferences/shared_preferences.dart';
import '../services/log_service.dart';
export 'translations.dart';
import 'translations.dart';

/// SharedPreferences key
const _kAppLocale = 'app_locale';

/// 语言偏好值: 'system' | 'zh' | 'en'
const kLocaleSystem = 'system';
const kLocaleZh = 'zh';
const kLocaleEn = 'en';
const _kPrefsInitTimeout = Duration(seconds: 3);

/// 获取系统语言并决定使用的 locale。
/// 支持 zh（中文）和 en（英文），其他语言默认使用英文。
String _resolveSystemLocale() {
  final locale = Platform.localeName; // e.g. "zh_CN", "en_US", "ja_JP"
  if (locale.startsWith('zh')) return 'zh';
  return 'en';
}

/// 全局 locale 实例 — 供无 context 场景使用（models, services, tray 等）。
/// 随 [LocaleNotifier] 变更自动更新。
S currentS = S.of(_resolveSystemLocale());

/// 当前实际 locale code ('zh' or 'en')
String currentLocale = _resolveSystemLocale();

/// 全局 LocaleNotifier 单例 — 在 main() 中创建并初始化
late final LocaleNotifier localeNotifier;

/// 运行时语言管理器
///
/// 支持三种模式: 跟随系统 / 中文 / 英文。
/// 持久化到 SharedPreferences，变更时 notifyListeners 触发 UI 重建。
class LocaleNotifier extends ChangeNotifier {
  /// 用户选择的语言偏好: 'system', 'zh', 'en'
  String _preference = kLocaleSystem;

  String get preference => _preference;
  S get s => currentS;

  /// 启动时调用，从 SharedPreferences 恢复语言偏好。
  /// 读取加 3 秒超时；超时或异常时记录日志并回退系统语言，避免卡住启动。
  Future<void> init() async {
    try {
      final prefs = await SharedPreferences.getInstance().timeout(
        _kPrefsInitTimeout,
      );
      final saved = prefs.getString(_kAppLocale);
      if (saved != null &&
          (saved == kLocaleSystem ||
              saved == kLocaleZh ||
              saved == kLocaleEn)) {
        _preference = saved;
      }
    } catch (e, stack) {
      logError('LocaleNotifier', 'init failed, using system locale', e, stack);
    }
    _applyLocale();
    // 静默加载，不触发 rebuild（main.dart 会在 init 完成后才 runApp）
  }

  /// 设置语言偏好并立即生效
  void setLocale(String pref) {
    if (_preference == pref) return;
    _preference = pref;
    _applyLocale();
    notifyListeners();
    _persist();
  }

  /// 根据偏好计算实际 locale 并更新全局变量
  void _applyLocale() {
    if (_preference == kLocaleSystem) {
      currentLocale = _resolveSystemLocale();
    } else {
      currentLocale = _preference;
    }
    currentS = S.of(currentLocale);
  }

  /// 异步写入 SharedPreferences（fire-and-forget）
  Future<void> _persist() async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_kAppLocale, _preference);
  }
}

/// InheritedWidget 用于在 widget tree 中传递 S 实例
class LocaleScope extends InheritedWidget {
  final S s;

  const LocaleScope({super.key, required this.s, required super.child});

  static S of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<LocaleScope>();
    return scope?.s ?? currentS;
  }

  @override
  bool updateShouldNotify(LocaleScope oldWidget) =>
      s.locale != oldWidget.s.locale;
}
