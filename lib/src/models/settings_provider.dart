import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import '../services/analytics_service.dart';
import '../services/log_service.dart';

/// 下载引擎相关配置（持久化在 Rust SQLite 中）
class SettingsProvider extends ChangeNotifier {
  /// 全局单例引用，供 WindowListener 等无 context 场景读取设置
  static SettingsProvider? globalInstance;

  String _defaultSaveDir = _platformDefaultSaveDir();
  int _defaultSegments = 0; // 0 = 自动（由 Rust segment_advisor 动态计算）
  int _maxConcurrentTasks = 5;
  int _speedLimitBytes = 0; // 0 = 无限制
  bool _autoResumeOnStart = false;
  bool _closeToTray = true; // 默认关闭到托盘
  bool _autoStartup = false; // 默认不开机启动
  bool _autoCheckUpdate = true; // 默认启动时自动检查更新
  bool _analyticsEnabled = true; // 默认启用匿名数据分析

  // 文件关联
  bool _torrentAssocPrompted = false; // 是否已弹窗提示过文件关联
  bool _torrentAssociated = false; // .torrent 文件是否已关联到 FluxDown

  // 代理设置
  String _proxyMode = 'none'; // none / system / manual
  String _proxyType = 'http'; // http / https / socks4 / socks5
  String _proxyHost = '';
  String _proxyPort = '';
  String _proxyUsername = '';
  String _proxyPassword = '';
  String _proxyNoList = ''; // 逗号分隔的排除列表

  // BT 设置
  bool _btEnableDht = true; // DHT 分布式哈希表
  bool _btEnableUpnp = true; // UPnP 端口映射
  int _btPortStart = 6881; // 监听端口起始
  int _btPortEnd = 6891; // 监听端口结束
  String _btCustomTrackers = ''; // 用户自定义 Tracker 列表（换行分隔）

  /// 配置是否已从 Rust 端加载完成
  bool _loaded = false;

  /// 是否启用文件关联功能（查询/监听注册表状态）。
  /// `_settingsForExternal`（main.dart）不需要此功能，设为 false 避免重复查询。
  final bool _enableFileAssoc;

  StreamSubscription<RustSignalPack<ConfigLoaded>>? _configSub;
  StreamSubscription<RustSignalPack<FileAssociationStatus>>? _fileAssocSub;

  SettingsProvider({bool enableFileAssoc = true})
    : _enableFileAssoc = enableFileAssoc {
    logInfo(
      'Settings',
      'constructor, enableFileAssoc=$enableFileAssoc, setting globalInstance',
    );
    globalInstance = this;
    _startListening();
    _syncAutoStartupState();
  }

  @override
  void dispose() {
    logInfo('Settings', 'dispose');
    _configSub?.cancel();
    _fileAssocSub?.cancel();
    if (globalInstance == this) {
      globalInstance = null;
    }
    super.dispose();
  }

  // ---------------------------------------------------------------------------
  // Getters
  // ---------------------------------------------------------------------------

  bool get loaded => _loaded;
  String get defaultSaveDir => _defaultSaveDir;
  int get defaultSegments => _defaultSegments;
  int get maxConcurrentTasks => _maxConcurrentTasks;
  int get speedLimitBytes => _speedLimitBytes;
  bool get autoResumeOnStart => _autoResumeOnStart;
  bool get closeToTray => _closeToTray;
  bool get autoStartup => _autoStartup;
  bool get autoCheckUpdate => _autoCheckUpdate;
  bool get analyticsEnabled => _analyticsEnabled;

  // 文件关联 Getters
  bool get torrentAssocPrompted => _torrentAssocPrompted;
  bool get torrentAssociated => _torrentAssociated;

  // 代理设置 Getters
  String get proxyMode => _proxyMode;
  String get proxyType => _proxyType;
  String get proxyHost => _proxyHost;
  String get proxyPort => _proxyPort;
  String get proxyUsername => _proxyUsername;
  String get proxyPassword => _proxyPassword;
  String get proxyNoList => _proxyNoList;

  // BT 设置 Getters
  bool get btEnableDht => _btEnableDht;
  bool get btEnableUpnp => _btEnableUpnp;
  int get btPortStart => _btPortStart;
  int get btPortEnd => _btPortEnd;
  String get btCustomTrackers => _btCustomTrackers;

  // ---------------------------------------------------------------------------
  // Setters — 修改值 + 通知 Rust 持久化
  // ---------------------------------------------------------------------------

  void setDefaultSaveDir(String value) {
    if (_defaultSaveDir == value) return;
    _defaultSaveDir = value;
    notifyListeners();
    _saveToRust('default_save_dir', value);
  }

  void setDefaultSegments(int value) {
    if (_defaultSegments == value) return;
    _defaultSegments = value;
    notifyListeners();
    _saveToRust('default_segments', value.toString());
  }

  void setMaxConcurrentTasks(int value) {
    if (_maxConcurrentTasks == value) return;
    _maxConcurrentTasks = value;
    notifyListeners();
    _saveToRust('max_concurrent_tasks', value.toString());
  }

  void setSpeedLimitBytes(int value) {
    if (_speedLimitBytes == value) return;
    _speedLimitBytes = value;
    notifyListeners();
    _saveToRust('speed_limit_bytes', value.toString());
  }

  void setAutoResumeOnStart(bool value) {
    if (_autoResumeOnStart == value) return;
    _autoResumeOnStart = value;
    notifyListeners();
    _saveToRust('auto_resume_on_start', value.toString());
  }

  void setCloseToTray(bool value) {
    if (_closeToTray == value) return;
    _closeToTray = value;
    notifyListeners();
    _saveToRust('close_to_tray', value.toString());
  }

  void setAutoCheckUpdate(bool value) {
    if (_autoCheckUpdate == value) return;
    _autoCheckUpdate = value;
    notifyListeners();
    _saveToRust('auto_check_update', value.toString());
  }

  void setAnalyticsEnabled(bool value) {
    if (_analyticsEnabled == value) return;
    _analyticsEnabled = value;
    notifyListeners();
    _saveToRust('analytics_enabled', value.toString());
    AnalyticsService.instance.setEnabled(value);
  }

  // 代理设置 Setters

  void setProxyMode(String value) {
    if (_proxyMode == value) return;
    _proxyMode = value;
    notifyListeners();
    _saveToRust('proxy_mode', value);
  }

  void setProxyType(String value) {
    if (_proxyType == value) return;
    _proxyType = value;
    notifyListeners();
    _saveToRust('proxy_type', value);
  }

  void setProxyHost(String value) {
    if (_proxyHost == value) return;
    _proxyHost = value;
    notifyListeners();
    _saveToRust('proxy_host', value);
  }

  void setProxyPort(String value) {
    if (_proxyPort == value) return;
    _proxyPort = value;
    notifyListeners();
    _saveToRust('proxy_port', value);
  }

  void setProxyUsername(String value) {
    if (_proxyUsername == value) return;
    _proxyUsername = value;
    notifyListeners();
    _saveToRust('proxy_username', value);
  }

  void setProxyPassword(String value) {
    if (_proxyPassword == value) return;
    _proxyPassword = value;
    notifyListeners();
    _saveToRust('proxy_password', value);
  }

  void setProxyNoList(String value) {
    if (_proxyNoList == value) return;
    _proxyNoList = value;
    notifyListeners();
    _saveToRust('proxy_no_list', value);
  }

  // BT 设置 Setters

  void setBtEnableDht(bool value) {
    if (_btEnableDht == value) return;
    _btEnableDht = value;
    notifyListeners();
    _saveToRust('bt_enable_dht', value.toString());
  }

  void setBtEnableUpnp(bool value) {
    if (_btEnableUpnp == value) return;
    _btEnableUpnp = value;
    notifyListeners();
    _saveToRust('bt_enable_upnp', value.toString());
  }

  void setBtPortStart(int value) {
    if (_btPortStart == value) return;
    _btPortStart = value;
    notifyListeners();
    _saveToRust('bt_port_start', value.toString());
  }

  void setBtPortEnd(int value) {
    if (_btPortEnd == value) return;
    _btPortEnd = value;
    notifyListeners();
    _saveToRust('bt_port_end', value.toString());
  }

  void setBtCustomTrackers(String value) {
    if (_btCustomTrackers == value) return;
    _btCustomTrackers = value;
    notifyListeners();
    _saveToRust('bt_custom_trackers', value);
  }

  // 文件关联操作

  /// 标记已弹窗提示过文件关联（持久化到 Rust SQLite）
  void markTorrentAssocPrompted() {
    if (_torrentAssocPrompted) return;
    _torrentAssocPrompted = true;
    notifyListeners();
    _saveToRust('torrent_assoc_prompted', 'true');
  }

  /// 请求 Rust 检查当前 .torrent 文件关联状态
  void checkFileAssociation() {
    const CheckFileAssociation().sendSignalToRust();
  }

  /// 设置或取消 .torrent 文件关联。
  /// 乐观更新 UI，Rust 回传真实状态后会校正。
  void setFileAssociation(bool enable) {
    logInfo('Settings', 'setFileAssociation: enable=$enable');
    _torrentAssociated = enable;
    notifyListeners();
    SetFileAssociation(enable: enable).sendSignalToRust();
  }

  /// 设置开机自启动，返回是否成功。
  /// 操作后通过 [launchAtStartup.isEnabled] 验证注册表实际状态，
  /// 若与预期不符则回滚 UI 状态。
  Future<bool> setAutoStartup(bool value) async {
    if (_autoStartup == value) return true;

    // 先乐观更新 UI
    _autoStartup = value;
    notifyListeners();

    try {
      if (value) {
        await launchAtStartup.enable();
      } else {
        await launchAtStartup.disable();
      }

      // 验证实际状态
      final actual = await launchAtStartup.isEnabled();
      if (actual == value) {
        _saveToRust('auto_startup', value.toString());
        return true;
      }

      // 验证失败 — 回滚
      _autoStartup = !value;
      notifyListeners();
      return false;
    } catch (_) {
      // 异常 — 回滚
      _autoStartup = !value;
      notifyListeners();
      return false;
    }
  }

  // ---------------------------------------------------------------------------
  // 请求 Rust 端加载配置
  // ---------------------------------------------------------------------------

  void requestConfig() {
    const RequestConfig().sendSignalToRust();
  }

  // ---------------------------------------------------------------------------
  // 内部
  // ---------------------------------------------------------------------------

  void _startListening() {
    _configSub = ConfigLoaded.rustSignalStream.listen(_onConfigLoaded);
    if (_enableFileAssoc) {
      _fileAssocSub = FileAssociationStatus.rustSignalStream.listen(
        _onFileAssocStatus,
      );
    }
  }

  void _onFileAssocStatus(RustSignalPack<FileAssociationStatus> pack) {
    final associated = pack.message.isAssociated;
    logInfo('Settings', 'file association status: $associated');
    if (_torrentAssociated != associated) {
      _torrentAssociated = associated;
      notifyListeners();
    }
  }

  void _onConfigLoaded(RustSignalPack<ConfigLoaded> pack) {
    final entries = pack.message.entries;
    logInfo('Settings', '_onConfigLoaded: ${entries.length} entries');
    for (final entry in entries) {
      logInfo('Settings', '  config: ${entry.key}=${entry.value}');
      switch (entry.key) {
        case 'default_save_dir':
          _defaultSaveDir = entry.value;
        case 'default_segments':
          _defaultSegments = int.tryParse(entry.value) ?? 0;
        case 'max_concurrent_tasks':
          _maxConcurrentTasks = int.tryParse(entry.value) ?? 5;
        case 'speed_limit_bytes':
          _speedLimitBytes = int.tryParse(entry.value) ?? 0;
        case 'auto_resume_on_start':
          _autoResumeOnStart = entry.value == 'true';
        case 'close_to_tray':
          _closeToTray = entry.value == 'true';
        case 'auto_startup':
          _autoStartup = entry.value == 'true';
        case 'auto_check_update':
          _autoCheckUpdate = entry.value == 'true';
        case 'bt_enable_dht':
          _btEnableDht = entry.value == 'true';
        case 'bt_enable_upnp':
          _btEnableUpnp = entry.value == 'true';
        case 'bt_port_start':
          _btPortStart = int.tryParse(entry.value) ?? 6881;
        case 'bt_port_end':
          _btPortEnd = int.tryParse(entry.value) ?? 6891;
        case 'bt_custom_trackers':
          _btCustomTrackers = entry.value;
        case 'torrent_assoc_prompted':
          _torrentAssocPrompted = entry.value == 'true';
        case 'analytics_enabled':
          _analyticsEnabled = entry.value != 'false'; // 默认 true
        case 'proxy_mode':
          _proxyMode = entry.value;
        case 'proxy_type':
          _proxyType = entry.value;
        case 'proxy_host':
          _proxyHost = entry.value;
        case 'proxy_port':
          _proxyPort = entry.value;
        case 'proxy_username':
          _proxyUsername = entry.value;
        case 'proxy_password':
          _proxyPassword = entry.value;
        case 'proxy_no_list':
          _proxyNoList = entry.value;
      }
    }
    _loaded = true;
    notifyListeners();
    // 配置加载后，立即查询文件关联的实际状态（仅启用了文件关联功能的实例）
    if (_enableFileAssoc) {
      checkFileAssociation();
    }
  }

  void _saveToRust(String key, String value) {
    SaveConfig(key: key, value: value).sendSignalToRust();
  }

  /// 启动时同步开机启动状态（从系统注册表读取实际状态）
  Future<void> _syncAutoStartupState() async {
    _autoStartup = await launchAtStartup.isEnabled();
  }

  /// 平台默认下载目录
  static String _platformDefaultSaveDir() {
    final home =
        Platform.environment['USERPROFILE'] ??
        Platform.environment['HOME'] ??
        '.';
    return '$home${Platform.pathSeparator}Downloads';
  }
}
