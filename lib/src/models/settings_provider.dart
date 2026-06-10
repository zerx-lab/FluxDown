import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import '../services/analytics_service.dart';
import '../services/log_service.dart';
import 'custom_category.dart';

/// 下载引擎相关配置（持久化在 Rust SQLite 中）
class SettingsProvider extends ChangeNotifier {
  /// 全局单例引用，供 WindowListener 等无 context 场景读取设置
  static SettingsProvider? globalInstance;

  String _defaultSaveDir = _platformDefaultSaveDir();
  int _defaultSegments = 0; // 0 = 自动（由 Rust segment_advisor 动态计算）
  int _maxConcurrentTasks = 5;
  int _speedLimitBytes = 0; // 0 = 无限制
  int _maxAutoRetries = 3; // -1 = 无限, 0 = 关闭, 1..10 = 次数
  int _autoRetryDelaySecs = 5; // 失败重试间隔（秒）
  bool _autoResumeOnStart = false;
  bool _closeToTray = true; // 默认关闭到托盘
  bool _autoStartup = false; // 默认不开机启动
  bool _autoCheckUpdate = true; // 默认启动时自动检查更新
  bool _analyticsEnabled = true; // 默认启用匿名数据分析
  bool _notifyOnComplete = true; // 默认任务完成时弹出通知

  // 侧边栏区块显示设置
  bool _showSidebarStatus = true;    // 显示状态区块
  bool _showSidebarQueues = true;    // 显示队列区块
  bool _showSidebarCategory = true;  // 显示分类区块

  // 侧边栏折叠状态（持久化）
  bool _sidebarQueuesExpanded = true;    // 队列区块展开
  bool _sidebarCategoryExpanded = false; // 分类区块展开（默认折叠）

  // 自定义分类
  List<CustomCategory> _customCategories = [];

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

  // 代理防抖支持
  Timer? _proxyDebounceTimer;
  final Set<String> _pendingProxyKeys = {};

  // BT 设置
  bool _btEnableDht = true; // DHT 分布式哈希表
  bool _btEnableUpnp = true; // UPnP 端口映射
  int _btPortStart = 6881; // 监听端口起始
  int _btPortEnd = 6891; // 监听端口结束
  String _btCustomTrackers = ''; // 用户自定义 Tracker 列表（换行分隔）

  // 本地下载服务（油猴脚本接管）
  bool _localServerEnabled = true;
  int _localServerPort = 17800;
  String _localServerToken = '';

  // UA 设置
  String _globalUserAgent = ''; // 空字符串 = 使用内置 Chrome UA

  // 默认队列设置
  String _defaultQueueId = ''; // 空字符串 = 默认队列

  // 新建下载对话框上次选择的线程数（'' = 未记录，'auto' = 自动，数字串 = 固定）
  String _lastDialogThreads = '';

  // 文件管理器自定义命令模板（空 = 用平台默认行为）
  // {path} = 完整文件路径；{dir} = 目录路径；占位符在 Rust 端做 shell 转义
  String _revealFileCmd = '';
  String _openDirCmd = '';

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
    _proxyDebounceTimer?.cancel();
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
  int get maxAutoRetries => _maxAutoRetries;
  int get autoRetryDelaySecs => _autoRetryDelaySecs;
  bool get autoResumeOnStart => _autoResumeOnStart;
  bool get closeToTray => _closeToTray;
  bool get autoStartup => _autoStartup;
  bool get autoCheckUpdate => _autoCheckUpdate;
  bool get analyticsEnabled => _analyticsEnabled;
  bool get notifyOnComplete => _notifyOnComplete;

  // 侧边栏显示 Getters
  bool get showSidebarStatus => _showSidebarStatus;
  bool get showSidebarQueues => _showSidebarQueues;
  bool get showSidebarCategory => _showSidebarCategory;

  bool get sidebarQueuesExpanded => _sidebarQueuesExpanded;
  bool get sidebarCategoryExpanded => _sidebarCategoryExpanded;

  // 自定义分类 Getter
  List<CustomCategory> get customCategories => List.unmodifiable(_customCategories);

  /// 可见的分类（排序后），供侧边栏使用
  List<CustomCategory> get visibleCategories =>
      _customCategories.where((c) => c.visible).toList()
        ..sort((a, b) => a.position.compareTo(b.position));

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

  // 本地下载服务 Getters
  bool get localServerEnabled => _localServerEnabled;
  int get localServerPort => _localServerPort;
  String get localServerToken => _localServerToken;

  // UA 设置 Getter
  String get globalUserAgent => _globalUserAgent;

  // 默认队列 Getter
  String get defaultQueueId => _defaultQueueId;

  // 新建下载对话框上次选择的线程数 Getter
  String get lastDialogThreads => _lastDialogThreads;

  // 文件管理器命令 Getters
  String get revealFileCmd => _revealFileCmd;
  String get openDirCmd => _openDirCmd;

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

  /// 记住新建下载对话框中用户选择的线程数（'auto' 或数字字符串）
  void setLastDialogThreads(String value) {
    if (_lastDialogThreads == value) return;
    _lastDialogThreads = value;
    notifyListeners();
    _saveToRust('last_dialog_threads', value);
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

  void setMaxAutoRetries(int value) {
    if (_maxAutoRetries == value) return;
    _maxAutoRetries = value;
    notifyListeners();
    _saveToRust('max_auto_retries', value.toString());
  }

  void setAutoRetryDelaySecs(int value) {
    if (_autoRetryDelaySecs == value) return;
    _autoRetryDelaySecs = value;
    notifyListeners();
    _saveToRust('auto_retry_delay_secs', value.toString());
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

  void setNotifyOnComplete(bool value) {
    if (_notifyOnComplete == value) return;
    _notifyOnComplete = value;
    notifyListeners();
    _saveToRust('notify_on_complete', value.toString());
  }

  // 侧边栏显示 Setters

  void setShowSidebarStatus(bool value) {
    if (_showSidebarStatus == value) return;
    _showSidebarStatus = value;
    notifyListeners();
    _saveToRust('show_sidebar_status', value.toString());
  }

  void setShowSidebarQueues(bool value) {
    if (_showSidebarQueues == value) return;
    _showSidebarQueues = value;
    notifyListeners();
    _saveToRust('show_sidebar_queues', value.toString());
  }

  void setShowSidebarCategory(bool value) {
    if (_showSidebarCategory == value) return;
    _showSidebarCategory = value;
    notifyListeners();
    _saveToRust('show_sidebar_category', value.toString());
  }

  void setSidebarQueuesExpanded(bool value) {
    if (_sidebarQueuesExpanded == value) return;
    _sidebarQueuesExpanded = value;
    notifyListeners();
    _saveToRust('sidebar_queues_expanded', value.toString());
  }

  void setSidebarCategoryExpanded(bool value) {
    if (_sidebarCategoryExpanded == value) return;
    _sidebarCategoryExpanded = value;
    notifyListeners();
    _saveToRust('sidebar_category_expanded', value.toString());
  }

  // 自定义分类 Setters

  void setCustomCategories(List<CustomCategory> categories) {
    _customCategories = List.of(categories);
    notifyListeners();
    _saveToRust('custom_categories', CustomCategory.encodeList(_customCategories));
  }

  void addCustomCategory(CustomCategory category) {
    _customCategories.add(category);
    notifyListeners();
    _saveToRust('custom_categories', CustomCategory.encodeList(_customCategories));
  }

  void updateCustomCategory(CustomCategory updated) {
    final idx = _customCategories.indexWhere((c) => c.id == updated.id);
    if (idx < 0) return;
    _customCategories[idx] = updated;
    notifyListeners();
    _saveToRust('custom_categories', CustomCategory.encodeList(_customCategories));
  }

  void removeCustomCategory(String id) {
    _customCategories.removeWhere((c) => c.id == id);
    notifyListeners();
    _saveToRust('custom_categories', CustomCategory.encodeList(_customCategories));
  }

  void reorderCustomCategories(int oldIndex, int newIndex) {
    if (oldIndex < newIndex) newIndex -= 1;
    final item = _customCategories.removeAt(oldIndex);
    _customCategories.insert(newIndex, item);
    // 更新 position 字段
    for (int i = 0; i < _customCategories.length; i++) {
      _customCategories[i] = _customCategories[i].copyWith(position: i);
    }
    notifyListeners();
    _saveToRust('custom_categories', CustomCategory.encodeList(_customCategories));
  }

  /// 重置某个内置分类到默认状态
  void resetBuiltinCategory(String builtinType) {
    final defaults = CustomCategory.defaultCategories();
    final defaultCat = defaults.where((c) => c.builtinType == builtinType).firstOrNull;
    if (defaultCat == null) return;
    final idx = _customCategories.indexWhere((c) => c.builtinType == builtinType);
    if (idx >= 0) {
      _customCategories[idx] = defaultCat.copyWith(position: _customCategories[idx].position);
    }
    notifyListeners();
    _saveToRust('custom_categories', CustomCategory.encodeList(_customCategories));
  }

  /// 重置所有分类为默认状态（删除自定义分类，恢复内置分类）
  void resetAllCategories() {
    _customCategories = CustomCategory.defaultCategories();
    notifyListeners();
    _saveToRust('custom_categories', CustomCategory.encodeList(_customCategories));
  }

  // 代理设置 Setters

  void setProxyMode(String value) {
    if (_proxyMode == value) return;
    _proxyMode = value;
    notifyListeners();
    _saveProxyConfig('proxy_mode', value);
  }

  void setProxyType(String value) {
    if (_proxyType == value) return;
    _proxyType = value;
    notifyListeners();
    _saveProxyConfig('proxy_type', value);
  }

  void setProxyHost(String value) {
    if (_proxyHost == value) return;
    _proxyHost = value;
    notifyListeners();
    _saveProxyConfig('proxy_host', value);
  }

  void setProxyPort(String value) {
    if (_proxyPort == value) return;
    _proxyPort = value;
    notifyListeners();
    _saveProxyConfig('proxy_port', value);
  }

  void setProxyUsername(String value) {
    if (_proxyUsername == value) return;
    _proxyUsername = value;
    notifyListeners();
    _saveProxyConfig('proxy_username', value);
  }

  void setProxyPassword(String value) {
    if (_proxyPassword == value) return;
    _proxyPassword = value;
    notifyListeners();
    _saveProxyConfig('proxy_password', value);
  }

  void setProxyNoList(String value) {
    if (_proxyNoList == value) return;
    _proxyNoList = value;
    notifyListeners();
    _saveProxyConfig('proxy_no_list', value);
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

  // 本地下载服务 Setters

  void setLocalServerEnabled(bool value) {
    if (_localServerEnabled == value) return;
    _localServerEnabled = value;
    notifyListeners();
    _saveToRust('local_server_enabled', value.toString());
  }

  void setLocalServerPort(int value) {
    if (value < 1 || value > 65535) return;
    if (_localServerPort == value) return;
    _localServerPort = value;
    notifyListeners();
    _saveToRust('local_server_port', value.toString());
  }

  void setLocalServerToken(String value) {
    if (_localServerToken == value) return;
    _localServerToken = value;
    notifyListeners();
    _saveToRust('local_server_token', value);
  }

  // UA 设置 Setter

  void setGlobalUserAgent(String value) {
    if (_globalUserAgent == value) return;
    _globalUserAgent = value;
    notifyListeners();
    _saveToRust('global_user_agent', value);
  }

  // 默认队列 Setter
  void setDefaultQueueId(String value) {
    if (_defaultQueueId == value) return;
    _defaultQueueId = value;
    notifyListeners();
    _saveToRust('default_queue_id', value);
  }

  // 文件管理器命令 Setters
  void setRevealFileCmd(String value) {
    if (_revealFileCmd == value) return;
    _revealFileCmd = value;
    notifyListeners();
    _saveToRust('reveal_file_cmd', value);
  }

  void setOpenDirCmd(String value) {
    if (_openDirCmd == value) return;
    _openDirCmd = value;
    notifyListeners();
    _saveToRust('open_dir_cmd', value);
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
        case 'max_auto_retries':
          _maxAutoRetries = int.tryParse(entry.value) ?? 3;
        case 'auto_retry_delay_secs':
          _autoRetryDelaySecs = int.tryParse(entry.value) ?? 5;
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
        case 'notify_on_complete':
          _notifyOnComplete = entry.value != 'false'; // 默认 true
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
        case 'local_server_enabled':
          _localServerEnabled = entry.value == 'true';
        case 'local_server_port':
          _localServerPort = int.tryParse(entry.value) ?? 17800;
        case 'local_server_token':
          _localServerToken = entry.value;
        case 'global_user_agent':
          _globalUserAgent = entry.value;
        case 'default_queue_id':
          _defaultQueueId = entry.value;
        case 'last_dialog_threads':
          _lastDialogThreads = entry.value;
        case 'reveal_file_cmd':
          _revealFileCmd = entry.value;
        case 'open_dir_cmd':
          _openDirCmd = entry.value;
        case 'show_sidebar_status':
          _showSidebarStatus = entry.value != 'false';
        case 'show_sidebar_queues':
          _showSidebarQueues = entry.value != 'false';
        case 'show_sidebar_category':
          _showSidebarCategory = entry.value != 'false';
        case 'sidebar_queues_expanded':
          _sidebarQueuesExpanded = entry.value != 'false';
        case 'sidebar_category_expanded':
          _sidebarCategoryExpanded = entry.value == 'true';
        case 'custom_categories':
          _customCategories = CustomCategory.decodeList(entry.value);
      }
    }
    _loaded = true;
    notifyListeners();
    // 首次启动：若无自定义分类配置，使用内置默认分类
    if (_customCategories.isEmpty) {
      _customCategories = CustomCategory.defaultCategories();
    }
    // 配置加载后，立即查询文件关联的实际状态（仅启用了文件关联功能的实例）
    if (_enableFileAssoc) {
      checkFileAssociation();
    }
  }

  void _saveToRust(String key, String value) {
    SaveConfig(key: key, value: value).sendSignalToRust();
  }

  /// 代理配置防抖保存：200ms 内的多次变更合并为一次批量发送，
  /// 避免用户连续输入时触发多次 reqwest Client 重建。
  void _saveProxyConfig(String key, String value) {
    _pendingProxyKeys.add(key);
    _proxyDebounceTimer?.cancel();
    _proxyDebounceTimer = Timer(const Duration(milliseconds: 200), () {
      for (final k in _pendingProxyKeys) {
        _saveToRust(k, _proxyValueForKey(k));
      }
      _pendingProxyKeys.clear();
      _proxyDebounceTimer = null;
    });
  }

  /// 从当前内存状态读取代理字段值（供防抖 timer 回调使用）。
  String _proxyValueForKey(String key) => switch (key) {
    'proxy_mode' => _proxyMode,
    'proxy_type' => _proxyType,
    'proxy_host' => _proxyHost,
    'proxy_port' => _proxyPort,
    'proxy_username' => _proxyUsername,
    'proxy_password' => _proxyPassword,
    'proxy_no_list' => _proxyNoList,
    _ => '',
  };

  /// 启动时同步开机启动状态（从系统注册表读取实际状态）
  Future<void> _syncAutoStartupState() async {
    final actual = await launchAtStartup.isEnabled();
    if (_autoStartup != actual) {
      _autoStartup = actual;
      notifyListeners();
    }
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
