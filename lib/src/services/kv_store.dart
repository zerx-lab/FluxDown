import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'log_service.dart';
import 'platform_utils.dart';

const _tag = 'KvStore';

/// 便携模式下 JSON 存储文件名（放在 [resolveDataDir] 目录下，
/// 即 `<exe_dir>/portable_data/`）。
const _kPortableFileName = 'settings.json';

/// 便携模式写盘防抖间隔。
const _kFlushDebounce = Duration(milliseconds: 400);

/// 轻量级键值存储门面，用于替代散落各处的 [SharedPreferences] 直接调用。
///
/// 动机：`shared_preferences` 插件链在 Windows 上硬编码写入
/// `%APPDATA%\Roaming\<company>\<product>\shared_preferences.json`，
/// 便携版首次打开即污染 C 盘、拔盘不带走。本门面在**便携模式**下改用
/// [resolveDataDir]（即 exe 目录）下的 `settings.json`，实现真正的
/// 零系统痕迹；**安装模式**下继续透传 [SharedPreferences]，行为不变。
///
/// 对外仅暴露同步读写子集（`getString/setString`、`getBool/setBool`、
/// `getDouble/setDouble`、`remove`），供 `runApp` 之前的窗口状态恢复等
/// 早期同步读取场景使用。所有值在 [init] 时一次性载入内存缓存，
/// 因此读取始终同步；写入即时更新缓存，落盘按后端异步进行。
///
/// 使用前必须先 `await KvStore.instance.init()`（在 `main` 最早期，
/// 早于任何 provider/service 的读取）。
class KvStore {
  KvStore._();

  /// 全局单例。
  static final KvStore instance = KvStore._();

  /// 内存缓存：所有键值一次性载入，读取全部同步命中。
  final Map<String, Object> _cache = {};

  /// 安装模式后端；便携模式为 null。
  SharedPreferences? _prefs;

  /// 便携模式 JSON 文件；安装模式为 null。
  File? _file;

  /// 便携模式写盘防抖定时器。
  Timer? _flushTimer;

  /// 是否已初始化，防止重复 init。
  bool _initialized = false;

  /// 是否便携模式（决定后端）。
  bool get _portable => _file != null;

  /// 载入持久化数据到内存缓存。
  ///
  /// 便携模式：同步读取 exe 目录下 `settings.json`。
  /// 安装模式：`await SharedPreferences.getInstance()`，把全部键值拷入缓存，
  /// 后续读取即无需再 await。
  ///
  /// 幂等：重复调用直接返回。任何异常都降级为空缓存并记日志，绝不阻塞启动。
  Future<void> init() async {
    if (_initialized) return;
    _initialized = true;
    try {
      if (isPortableMode()) {
        _file = File('${resolveDataDir()}${Platform.pathSeparator}'
            '$_kPortableFileName');
        _loadFromFile();
        logInfo(_tag, 'portable backend: ${_file!.path}, '
            '${_cache.length} entries');
      } else {
        final prefs = await SharedPreferences.getInstance();
        _prefs = prefs;
        for (final key in prefs.getKeys()) {
          final value = prefs.get(key);
          // 仅缓存本门面支持的标量类型；其他类型（如 List）保持由 SP 直存，
          // 但本项目所有调用点均只用 String/bool/double，不会触发。
          if (value is String || value is bool || value is double) {
            _cache[key] = value as Object;
          }
        }
        logInfo(_tag, 'installed backend (SharedPreferences), '
            '${_cache.length} entries');
      }
    } catch (e, stack) {
      logError(_tag, 'init failed, using empty cache', e, stack);
    }
  }

  /// 测试专用：强制以便携模式后端初始化到指定 [file]，绕过 exe 路径探测。
  ///
  /// 生产代码永不调用；仅供单元测试验证便携模式 JSON 读写往返。
  @visibleForTesting
  void debugInitPortable(File file) {
    _initialized = true;
    _file = file;
    _loadFromFile();
  }

  /// 测试专用：清空状态，供多个测试用例间隔离。
  @visibleForTesting
  void debugReset() {
    _flushTimer?.cancel();
    _flushTimer = null;
    _cache.clear();
    _prefs = null;
    _file = null;
    _initialized = false;
  }

  void _loadFromFile() {
    final file = _file;
    if (file == null || !file.existsSync()) return;
    try {
      final raw = file.readAsStringSync();
      if (raw.trim().isEmpty) return;
      final decoded = jsonDecode(raw);
      if (decoded is Map<String, dynamic>) {
        decoded.forEach((key, value) {
          if (value is String || value is bool) {
            _cache[key] = value;
          } else if (value is num) {
            // JSON 数字统一按 double 存回（窗口坐标/尺寸均为 double）。
            _cache[key] = value.toDouble();
          }
        });
      }
    } catch (e, stack) {
      logError(_tag, 'failed to read portable settings, ignoring', e, stack);
    }
  }

  // ---------------------------------------------------------------------------
  // 同步读取
  // ---------------------------------------------------------------------------

  /// 读取字符串；不存在或类型不符时返回 null。
  String? getString(String key) {
    final value = _cache[key];
    return value is String ? value : null;
  }

  /// 读取布尔；不存在或类型不符时返回 null。
  bool? getBool(String key) {
    final value = _cache[key];
    return value is bool ? value : null;
  }

  /// 读取浮点；不存在或类型不符时返回 null。
  double? getDouble(String key) {
    final value = _cache[key];
    return value is double ? value : null;
  }

  // ---------------------------------------------------------------------------
  // 写入（即时更新缓存，落盘异步）
  // ---------------------------------------------------------------------------

  /// 写入字符串。
  Future<void> setString(String key, String value) => _put(key, value);

  /// 写入布尔。
  Future<void> setBool(String key, bool value) => _put(key, value);

  /// 写入浮点。
  Future<void> setDouble(String key, double value) => _put(key, value);

  /// 删除键。
  Future<void> remove(String key) async {
    _cache.remove(key);
    if (_portable) {
      _scheduleFlush();
    } else {
      await _prefs?.remove(key);
    }
  }

  Future<void> _put(String key, Object value) async {
    _cache[key] = value;
    if (_portable) {
      _scheduleFlush();
    } else {
      final prefs = _prefs;
      if (prefs == null) return;
      if (value is String) {
        await prefs.setString(key, value);
      } else if (value is bool) {
        await prefs.setBool(key, value);
      } else if (value is double) {
        await prefs.setDouble(key, value);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // 便携模式落盘
  // ---------------------------------------------------------------------------

  void _scheduleFlush() {
    _flushTimer?.cancel();
    _flushTimer = Timer(_kFlushDebounce, _flushToFile);
  }

  /// 立即把缓存落盘（便携模式）。安装模式为 no-op（SP 已即时写入）。
  ///
  /// 供退出/隐藏等关键时刻调用，确保最新状态不因防抖丢失。
  Future<void> flush() async {
    _flushTimer?.cancel();
    _flushTimer = null;
    _flushToFile();
  }

  void _flushToFile() {
    final file = _file;
    if (file == null) return;
    try {
      final dir = file.parent;
      if (!dir.existsSync()) dir.createSync(recursive: true);
      file.writeAsStringSync(jsonEncode(_cache));
    } catch (e, stack) {
      logError(_tag, 'failed to write portable settings', e, stack);
    }
  }
}
