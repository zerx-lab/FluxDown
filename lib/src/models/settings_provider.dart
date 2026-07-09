import 'dart:async';
import 'dart:io';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
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
  bool _notifyOnComplete = true; // 默认任务完成时弹出通知
  bool _silentDownloadEnabled = false; // 免打扰下载：外部请求不弹确认框直接下载
  bool _keepAwakeWhileDownloading = false; // 默认不阻止睡眠/息屏
  int _logMaxSizeMb = 10; // 日志总大小上限（MB），超出自动清理

  // 悬浮球设置
  bool _floatingBallEnabled = false; // 默认关闭（与 closeToTray 保守默认一致）
  double _floatingBallX = -1; // 绝对像素坐标；-1 哨兵 = 未设置（用默认停靠）
  double _floatingBallY = -1;
  bool _floatingBallActiveOnly = false; // 仅下载时显示，其余隐藏（默认关=常显）
  bool _clipboardWatchEnabled = false; // 仅 Linux Wayland 降级分支展示

  // 侧边栏区块显示设置
  bool _showSidebarStatus = true; // 显示状态区块
  bool _showSidebarQueues = true; // 显示队列区块
  bool _showSidebarCategory = true; // 显示分类区块

  // 标题栏工具按钮显示设置
  bool _showTitlebarPauseAll = true; // 全部暂停按钮
  bool _showTitlebarResumeAll = true; // 全部恢复按钮
  bool _showTitlebarSettings = true; // 设置按钮
  bool _showTitlebarTheme = true; // 主题切换按钮

  // 侧边栏折叠状态（持久化）
  bool _sidebarQueuesExpanded = true; // 队列区块展开
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

  // BT Tracker 订阅（社区维护的 tracker 列表，Rust 端拉取后合并去重）
  bool _btTrackerSubEnabled = true; // 启用 Tracker 订阅
  String _btTrackerSubUrls = ''; // 订阅地址（换行分隔）
  int _btTrackerSubCount = 0; // 订阅缓存中的 tracker 数量
  int _btTrackerSubUpdatedAt = 0; // 上次订阅更新时间（Unix 秒，0=从未）
  bool _btTrackerSubRefreshing = false; // 是否正在刷新订阅
  String _btTrackerSubLastError = ''; // 上次刷新的错误信息（空=成功）

  // ED2K 服务器（手填列表 + server.met 社区订阅，Rust 端拉取解析后合并去重）
  String _ed2kServerList = ''; // 用户手填服务器（逗号分隔 host:port）
  bool _ed2kServerSubEnabled = true; // 启用 server.met 订阅
  String _ed2kServerSubUrls = ''; // 订阅地址（换行分隔）
  int _ed2kServerSubCount = 0; // 订阅缓存中的服务器数量
  int _ed2kServerSubUpdatedAt = 0; // 上次订阅更新时间（Unix 秒，0=从未）
  bool _ed2kServerSubRefreshing = false; // 是否正在刷新订阅
  String _ed2kServerSubLastError = ''; // 上次刷新的错误信息（空=成功）

  // ED2K 客户端（Kad DHT / UPnP / 监听端口）
  bool _ed2kEnableKad = true; // Kad DHT 去中心化找源
  bool _ed2kEnableUpnp = true; // UPnP 端口映射争取 HighID
  int _ed2kListenPort = 0; // TCP/UDP 监听端口（0=OS 选）

  // 本地 API 服务（浏览器脚本接管 / aria2 RPC 兼容 / 管理 API）
  bool _localServerEnabled = true;
  int _localServerPort = 17800;
  String _localServerToken = '';
  bool _localServerTakeoverEnabled = true;
  bool _localServerJsonrpcEnabled = true;
  bool _localServerApiEnabled = false;
  bool _localServerMcpEnabled = false;

  // UA 设置
  String _globalUserAgent = ''; // 空字符串 = 使用内置 Chrome UA

  // 默认队列设置
  String _defaultQueueId = ''; // 空字符串 = 默认队列

  // 新建下载对话框上次选择的线程数（'' = 未记录，'auto' = 自动，数字串 = 固定）
  String _lastDialogThreads = '';

  // 下载位置自动使用上次保存的位置（开启后新建下载默认目录跟随上次下载的目录）
  bool _rememberLastSaveDir = false;

  // 上次下载确认时使用的保存目录（'' = 未记录）
  String _lastSaveDir = '';

  // 文件管理器自定义命令模板（空 = 用平台默认行为）
  // {path} = 完整文件路径；{dir} = 目录路径；占位符在 Rust 端做 shell 转义
  String _revealFileCmd = '';

  /// 配置是否已从 Rust 端加载完成
  bool _loaded = false;

  /// 是否启用文件关联功能（查询/监听注册表状态）。
  /// `_settingsForExternal`（main.dart）不需要此功能，设为 false 避免重复查询。
  final bool _enableFileAssoc;

  StreamSubscription<RustSignalPack<ConfigLoaded>>? _configSub;
  StreamSubscription<RustSignalPack<FileAssociationStatus>>? _fileAssocSub;
  StreamSubscription<RustSignalPack<TrackerSubscriptionResult>>? _trackerSubSub;
  StreamSubscription<RustSignalPack<Ed2kServerSubscriptionResult>>? _ed2kSubSub;

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
    _trackerSubSub?.cancel();
    _ed2kSubSub?.cancel();
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
  bool get notifyOnComplete => _notifyOnComplete;
  bool get silentDownloadEnabled => _silentDownloadEnabled;
  bool get keepAwakeWhileDownloading => _keepAwakeWhileDownloading;
  int get logMaxSizeMb => _logMaxSizeMb;

  // 悬浮球 Getters
  bool get floatingBallEnabled => _floatingBallEnabled;
  double get floatingBallX => _floatingBallX;
  double get floatingBallY => _floatingBallY;
  bool get floatingBallActiveOnly => _floatingBallActiveOnly;
  bool get clipboardWatchEnabled => _clipboardWatchEnabled;

  // 侧边栏显示 Getters
  bool get showSidebarStatus => _showSidebarStatus;
  bool get showSidebarQueues => _showSidebarQueues;
  bool get showSidebarCategory => _showSidebarCategory;

  // 标题栏工具按钮 Getters
  bool get showTitlebarPauseAll => _showTitlebarPauseAll;
  bool get showTitlebarResumeAll => _showTitlebarResumeAll;
  bool get showTitlebarSettings => _showTitlebarSettings;
  bool get showTitlebarTheme => _showTitlebarTheme;

  bool get sidebarQueuesExpanded => _sidebarQueuesExpanded;
  bool get sidebarCategoryExpanded => _sidebarCategoryExpanded;

  // 自定义分类 Getter
  List<CustomCategory> get customCategories =>
      List.unmodifiable(_customCategories);

  /// 可见的分类（排序后），供侧边栏使用
  List<CustomCategory> get visibleCategories =>
      _customCategories.where((c) => c.visible).toList()
        ..sort((a, b) => a.position.compareTo(b.position));

  /// 按分类规则解析文件的保存目录：
  /// 普通分类（按 position 排序）→ other 分类（无普通分类命中时）。
  /// 无匹配时返回 ''，由调用方决定回退目录。
  ///
  /// [fileName] 为空或无扩展名时，回退用 [url] 路径末段派生的文件名参与匹配
  /// （浏览器扩展右键下载常只带 URL、不带已解析文件名，需靠 URL 扩展名归类）。
  ///
  /// 快速下载对话框、独立小窗、免打扰静默路径与外部下载请求共用本解析器。
  String resolveCategorySaveDir(String fileName, {String url = ''}) {
    var name = fileName;
    if ((name.isEmpty || !name.contains('.')) && url.isNotEmpty) {
      final derived = _fileNameFromUrl(url);
      if (derived.isNotEmpty) name = derived;
    }
    if (name.isEmpty) return '';
    final categories = visibleCategories;
    final normals = categories
        .where((c) => c.builtinType != 'all' && c.builtinType != 'other')
        .toList();
    for (final cat in normals) {
      if (cat.saveDir.isNotEmpty && cat.matches(name)) {
        return cat.saveDir;
      }
    }
    final otherCat = categories
        .where((c) => c.builtinType == 'other')
        .firstOrNull;
    if (otherCat != null &&
        otherCat.saveDir.isNotEmpty &&
        !normals.any((c) => c.matches(name))) {
      return otherCat.saveDir;
    }
    return '';
  }

  /// 从 URL 中提取文件名（取最后一段路径，须含 '.'），失败返回 ''。
  static String _fileNameFromUrl(String url) {
    try {
      final uri = Uri.parse(url.trim());
      final segments = uri.pathSegments;
      if (segments.isNotEmpty) {
        final last = Uri.decodeComponent(segments.last);
        if (last.contains('.')) return last;
      }
    } catch (_) {}
    return '';
  }

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

  // BT Tracker 订阅 Getters
  bool get btTrackerSubEnabled => _btTrackerSubEnabled;
  String get btTrackerSubUrls => _btTrackerSubUrls;
  int get btTrackerSubCount => _btTrackerSubCount;
  int get btTrackerSubUpdatedAt => _btTrackerSubUpdatedAt;
  bool get btTrackerSubRefreshing => _btTrackerSubRefreshing;
  String get btTrackerSubLastError => _btTrackerSubLastError;

  // ED2K 服务器 Getters
  String get ed2kServerList => _ed2kServerList;
  bool get ed2kServerSubEnabled => _ed2kServerSubEnabled;
  String get ed2kServerSubUrls => _ed2kServerSubUrls;
  int get ed2kServerSubCount => _ed2kServerSubCount;
  int get ed2kServerSubUpdatedAt => _ed2kServerSubUpdatedAt;
  bool get ed2kServerSubRefreshing => _ed2kServerSubRefreshing;
  String get ed2kServerSubLastError => _ed2kServerSubLastError;

  // ED2K 客户端 Getters
  bool get ed2kEnableKad => _ed2kEnableKad;
  bool get ed2kEnableUpnp => _ed2kEnableUpnp;
  int get ed2kListenPort => _ed2kListenPort;

  // 本地 API 服务 Getters
  bool get localServerEnabled => _localServerEnabled;
  int get localServerPort => _localServerPort;
  String get localServerToken => _localServerToken;
  bool get localServerTakeoverEnabled => _localServerTakeoverEnabled;
  bool get localServerJsonrpcEnabled => _localServerJsonrpcEnabled;
  bool get localServerApiEnabled => _localServerApiEnabled;
  bool get localServerMcpEnabled => _localServerMcpEnabled;

  // UA 设置 Getter
  String get globalUserAgent => _globalUserAgent;

  // 默认队列 Getter
  String get defaultQueueId => _defaultQueueId;

  // 新建下载对话框上次选择的线程数 Getter
  String get lastDialogThreads => _lastDialogThreads;

  // 记住上次保存位置 Getters
  bool get rememberLastSaveDir => _rememberLastSaveDir;
  String get lastSaveDir => _lastSaveDir;

  /// 生效的默认保存目录：开关开启且已有记录时返回上次保存位置，否则返回固定默认目录
  String get effectiveDefaultSaveDir =>
      _rememberLastSaveDir && _lastSaveDir.isNotEmpty
      ? _lastSaveDir
      : _defaultSaveDir;

  // 文件管理器命令 Getters
  String get revealFileCmd => _revealFileCmd;

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

  void setRememberLastSaveDir(bool value) {
    if (_rememberLastSaveDir == value) return;
    _rememberLastSaveDir = value;
    notifyListeners();
    _saveToRust('remember_last_save_dir', value.toString());
  }

  /// 记录下载确认时使用的保存目录（无条件记录，开关开启后立即生效）
  void recordLastSaveDir(String dir) {
    if (dir.isEmpty || _lastSaveDir == dir) return;
    _lastSaveDir = dir;
    if (_rememberLastSaveDir) notifyListeners();
    _saveToRust('last_save_dir', dir);
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

  void setFloatingBallEnabled(bool value) {
    if (_floatingBallEnabled == value) return;
    _floatingBallEnabled = value;
    notifyListeners();
    _saveToRust('floating_ball_enabled', value.toString());
  }

  void setFloatingBallActiveOnly(bool value) {
    if (_floatingBallActiveOnly == value) return;
    _floatingBallActiveOnly = value;
    notifyListeners();
    _saveToRust('floating_ball_active_only', value.toString());
  }

  /// 保存悬浮球坐标（绝对像素）。拖动结束时调用，不触发 UI 重建。
  void setFloatingBallPosition(double x, double y) {
    if (_floatingBallX == x && _floatingBallY == y) return;
    _floatingBallX = x;
    _floatingBallY = y;
    _saveToRust('floating_ball_x', x.toString());
    _saveToRust('floating_ball_y', y.toString());
  }

  void setClipboardWatchEnabled(bool value) {
    if (_clipboardWatchEnabled == value) return;
    _clipboardWatchEnabled = value;
    notifyListeners();
    _saveToRust('clipboard_watch_enabled', value.toString());
  }

  void setNotifyOnComplete(bool value) {
    if (_notifyOnComplete == value) return;
    _notifyOnComplete = value;
    notifyListeners();
    _saveToRust('notify_on_complete', value.toString());
  }

  void setSilentDownloadEnabled(bool value) {
    if (_silentDownloadEnabled == value) return;
    _silentDownloadEnabled = value;
    notifyListeners();
    _saveToRust('silent_download_enabled', value.toString());
  }

  void setKeepAwakeWhileDownloading(bool value) {
    if (_keepAwakeWhileDownloading == value) return;
    _keepAwakeWhileDownloading = value;
    notifyListeners();
    _saveToRust('keep_awake_while_downloading', value.toString());
  }

  void setLogMaxSizeMb(int value) {
    if (_logMaxSizeMb == value || value < 1) return;
    _logMaxSizeMb = value;
    notifyListeners();
    // Rust 端收到后同步更新 logger 上限并执行超量清理
    _saveToRust('log_max_size_mb', value.toString());
    LogService.instance.maxTotalBytes = value * 1024 * 1024;
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

  // 标题栏工具按钮 Setters

  void setShowTitlebarPauseAll(bool value) {
    if (_showTitlebarPauseAll == value) return;
    _showTitlebarPauseAll = value;
    notifyListeners();
    _saveToRust('show_titlebar_pause_all', value.toString());
  }

  void setShowTitlebarResumeAll(bool value) {
    if (_showTitlebarResumeAll == value) return;
    _showTitlebarResumeAll = value;
    notifyListeners();
    _saveToRust('show_titlebar_resume_all', value.toString());
  }

  void setShowTitlebarSettings(bool value) {
    if (_showTitlebarSettings == value) return;
    _showTitlebarSettings = value;
    notifyListeners();
    _saveToRust('show_titlebar_settings', value.toString());
  }

  void setShowTitlebarTheme(bool value) {
    if (_showTitlebarTheme == value) return;
    _showTitlebarTheme = value;
    notifyListeners();
    _saveToRust('show_titlebar_theme', value.toString());
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
    _saveToRust(
      'custom_categories',
      CustomCategory.encodeList(_customCategories),
    );
  }

  void addCustomCategory(CustomCategory category) {
    _customCategories.add(category);
    notifyListeners();
    _saveToRust(
      'custom_categories',
      CustomCategory.encodeList(_customCategories),
    );
  }

  void updateCustomCategory(CustomCategory updated) {
    final idx = _customCategories.indexWhere((c) => c.id == updated.id);
    if (idx < 0) return;
    _customCategories[idx] = updated;
    notifyListeners();
    _saveToRust(
      'custom_categories',
      CustomCategory.encodeList(_customCategories),
    );
  }

  void removeCustomCategory(String id) {
    _customCategories.removeWhere((c) => c.id == id);
    notifyListeners();
    _saveToRust(
      'custom_categories',
      CustomCategory.encodeList(_customCategories),
    );
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
    _saveToRust(
      'custom_categories',
      CustomCategory.encodeList(_customCategories),
    );
  }

  /// 重置某个内置分类到默认状态
  void resetBuiltinCategory(String builtinType) {
    final defaults = CustomCategory.defaultCategories();
    final defaultCat = defaults
        .where((c) => c.builtinType == builtinType)
        .firstOrNull;
    if (defaultCat == null) return;
    final idx = _customCategories.indexWhere(
      (c) => c.builtinType == builtinType,
    );
    if (idx >= 0) {
      _customCategories[idx] = defaultCat.copyWith(
        position: _customCategories[idx].position,
      );
    }
    notifyListeners();
    _saveToRust(
      'custom_categories',
      CustomCategory.encodeList(_customCategories),
    );
  }

  /// 重置所有分类为默认状态（删除自定义分类，恢复内置分类）
  void resetAllCategories() {
    _customCategories = CustomCategory.defaultCategories();
    notifyListeners();
    _saveToRust(
      'custom_categories',
      CustomCategory.encodeList(_customCategories),
    );
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

  // BT Tracker 订阅 Setters

  void setBtTrackerSubEnabled(bool value) {
    if (_btTrackerSubEnabled == value) return;
    _btTrackerSubEnabled = value;
    notifyListeners();
    _saveToRust('bt_tracker_sub_enabled', value.toString());
  }

  void setBtTrackerSubUrls(String value) {
    if (_btTrackerSubUrls == value) return;
    _btTrackerSubUrls = value;
    // Rust 端会在订阅地址变化后自动后台刷新一次
    _btTrackerSubRefreshing = true;
    _btTrackerSubLastError = '';
    notifyListeners();
    _saveToRust('bt_tracker_sub_urls', value);
  }

  /// 请求 Rust 立即刷新 Tracker 订阅（结果通过 TrackerSubscriptionResult 回传）
  void refreshTrackerSubscription() {
    if (_btTrackerSubRefreshing) return;
    _btTrackerSubRefreshing = true;
    _btTrackerSubLastError = '';
    notifyListeners();
    const UpdateTrackerSubscription().sendSignalToRust();
  }

  // ED2K 服务器 Setters

  void setEd2kServerList(String value) {
    if (_ed2kServerList == value) return;
    _ed2kServerList = value;
    notifyListeners();
    _saveToRust('ed2k_server_list', value);
  }

  void setEd2kServerSubEnabled(bool value) {
    if (_ed2kServerSubEnabled == value) return;
    _ed2kServerSubEnabled = value;
    notifyListeners();
    _saveToRust('ed2k_server_sub_enabled', value.toString());
  }

  void setEd2kEnableKad(bool value) {
    if (_ed2kEnableKad == value) return;
    _ed2kEnableKad = value;
    notifyListeners();
    _saveToRust('ed2k_enable_kad', value.toString());
  }

  void setEd2kEnableUpnp(bool value) {
    if (_ed2kEnableUpnp == value) return;
    _ed2kEnableUpnp = value;
    notifyListeners();
    _saveToRust('ed2k_enable_upnp', value.toString());
  }

  void setEd2kListenPort(int value) {
    if (_ed2kListenPort == value) return;
    _ed2kListenPort = value;
    notifyListeners();
    _saveToRust('ed2k_listen_port', value.toString());
  }

  void setEd2kServerSubUrls(String value) {
    if (_ed2kServerSubUrls == value) return;
    _ed2kServerSubUrls = value;
    // Rust 端会在订阅地址变化后自动后台刷新一次
    _ed2kServerSubRefreshing = true;
    _ed2kServerSubLastError = '';
    notifyListeners();
    _saveToRust('ed2k_server_sub_urls', value);
  }

  /// 请求 Rust 立即刷新 ED2K 服务器订阅（结果通过 Ed2kServerSubscriptionResult 回传）
  void refreshEd2kServerSubscription() {
    if (_ed2kServerSubRefreshing) return;
    _ed2kServerSubRefreshing = true;
    _ed2kServerSubLastError = '';
    notifyListeners();
    const UpdateEd2kServerSubscription().sendSignalToRust();
  }

  // 本地 API 服务 Setters

  void setLocalServerEnabled(bool value) {
    if (_localServerEnabled == value) return;
    _localServerEnabled = value;
    notifyListeners();
    _saveToRust('local_server_enabled', value.toString());
  }

  void setLocalServerPort(int value) {
    if (value < 1024 || value > 65535) return;
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

  /// 清空访问令牌。若管理 API / MCP 正依赖此令牌（已启用），则一并关闭它们，
  /// 避免出现「已启用但无 token」的非法状态。
  void clearLocalServerToken() {
    if (_localServerToken.isEmpty &&
        !_localServerApiEnabled &&
        !_localServerMcpEnabled) {
      return;
    }
    _localServerToken = '';
    _saveToRust('local_server_token', '');
    if (_localServerApiEnabled) {
      _localServerApiEnabled = false;
      _saveToRust('local_server_api_enabled', 'false');
    }
    if (_localServerMcpEnabled) {
      _localServerMcpEnabled = false;
      _saveToRust('local_server_mcp_enabled', 'false');
    }
    notifyListeners();
  }

  void setLocalServerTakeoverEnabled(bool value) {
    if (_localServerTakeoverEnabled == value) return;
    _localServerTakeoverEnabled = value;
    notifyListeners();
    _saveToRust('local_server_takeover_enabled', value.toString());
  }

  void setLocalServerJsonrpcEnabled(bool value) {
    if (_localServerJsonrpcEnabled == value) return;
    _localServerJsonrpcEnabled = value;
    notifyListeners();
    _saveToRust('local_server_jsonrpc_enabled', value.toString());
  }

  /// 管理 API 强制鉴权：从关到开且当前 token 为空时，自动生成 32 位 hex token 并一并保存
  void setLocalServerApiEnabled(bool value) {
    if (_localServerApiEnabled == value) return;
    _localServerApiEnabled = value;
    if (value && _localServerToken.isEmpty) {
      _localServerToken = _generateHexToken();
      _saveToRust('local_server_token', _localServerToken);
    }
    notifyListeners();
    _saveToRust('local_server_api_enabled', value.toString());
  }

  /// MCP 端点强制鉴权（与管理 API 共用 token）：从关到开且当前 token 为空时，自动生成并保存
  void setLocalServerMcpEnabled(bool value) {
    if (_localServerMcpEnabled == value) return;
    _localServerMcpEnabled = value;
    if (value && _localServerToken.isEmpty) {
      _localServerToken = _generateHexToken();
      _saveToRust('local_server_token', _localServerToken);
    }
    notifyListeners();
    _saveToRust('local_server_mcp_enabled', value.toString());
  }

  /// 生成 32 位随机 hex token（管理 API 自动鉴权 / UI 手动重新生成共用）
  static String _generateHexToken() {
    final r = Random.secure();
    return List<int>.generate(
      16,
      (_) => r.nextInt(256),
    ).map((b) => b.toRadixString(16).padLeft(2, '0')).join();
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
    _trackerSubSub = TrackerSubscriptionResult.rustSignalStream.listen(
      _onTrackerSubResult,
    );
    _ed2kSubSub = Ed2kServerSubscriptionResult.rustSignalStream.listen(
      _onEd2kServerSubResult,
    );
    if (_enableFileAssoc) {
      _fileAssocSub = FileAssociationStatus.rustSignalStream.listen(
        _onFileAssocStatus,
      );
    }
  }

  void _onTrackerSubResult(RustSignalPack<TrackerSubscriptionResult> pack) {
    final msg = pack.message;
    logInfo(
      'Settings',
      'tracker subscription result: success=${msg.success}, '
          'count=${msg.trackerCount}, sources=${msg.okSources}/${msg.totalSources}',
    );
    _btTrackerSubRefreshing = false;
    if (msg.success) {
      _btTrackerSubCount = msg.trackerCount;
      _btTrackerSubUpdatedAt = msg.updatedAt;
      _btTrackerSubLastError = '';
    } else {
      _btTrackerSubLastError = msg.error;
    }
    notifyListeners();
  }

  void _onEd2kServerSubResult(
    RustSignalPack<Ed2kServerSubscriptionResult> pack,
  ) {
    final msg = pack.message;
    logInfo(
      'Settings',
      'ed2k server subscription result: success=${msg.success}, '
          'count=${msg.serverCount}, sources=${msg.okSources}/${msg.totalSources}',
    );
    _ed2kServerSubRefreshing = false;
    if (msg.success) {
      _ed2kServerSubCount = msg.serverCount;
      _ed2kServerSubUpdatedAt = msg.updatedAt;
      _ed2kServerSubLastError = '';
    } else {
      _ed2kServerSubLastError = msg.error;
    }
    notifyListeners();
  }

  void _onFileAssocStatus(RustSignalPack<FileAssociationStatus> pack) {
    final associated = pack.message.isAssociated;
    logInfo('Settings', 'file association status: $associated');
    if (_torrentAssociated != associated) {
      _torrentAssociated = associated;
      notifyListeners();
    }
  }

  /// 日志用值截断：压平换行并限制长度，避免 tracker 列表 / base64 缓存
  /// （如 ed2k_nodes_dat_cache ~8KB）把日志文件撑爆。
  static String _truncateForLog(String value) {
    const maxLen = 120;
    final flat = value.replaceAll('\r\n', r'\n').replaceAll('\n', r'\n');
    if (flat.length <= maxLen) return flat;
    return '${flat.substring(0, maxLen)}…(${value.length} chars)';
  }

  void _onConfigLoaded(RustSignalPack<ConfigLoaded> pack) {
    final entries = pack.message.entries;
    logInfo('Settings', '_onConfigLoaded: ${entries.length} entries');
    String legacyOpenDirCmd = '';
    // 追踪 reveal_file_cmd 键是否出现在配置中（区分「从未设置」与「已清空」）。
    bool revealFileCmdPresent = false;
    for (final entry in entries) {
      logInfo('Settings', '  config: ${entry.key}=${_truncateForLog(entry.value)}');
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
        case 'bt_tracker_sub_enabled':
          _btTrackerSubEnabled = entry.value == 'true';
        case 'bt_tracker_sub_urls':
          _btTrackerSubUrls = entry.value;
        case 'bt_tracker_sub_cache':
          final cache = entry.value.trim();
          _btTrackerSubCount = cache.isEmpty ? 0 : cache.split('\n').length;
        case 'bt_tracker_sub_updated_at':
          _btTrackerSubUpdatedAt = int.tryParse(entry.value) ?? 0;
        case 'ed2k_server_list':
          _ed2kServerList = entry.value;
        case 'ed2k_server_sub_enabled':
          _ed2kServerSubEnabled = entry.value == 'true';
        case 'ed2k_server_sub_urls':
          _ed2kServerSubUrls = entry.value;
        case 'ed2k_server_sub_cache':
          final ed2kCache = entry.value.trim();
          _ed2kServerSubCount = ed2kCache.isEmpty
              ? 0
              : ed2kCache.split(',').length;
        case 'ed2k_server_sub_updated_at':
          _ed2kServerSubUpdatedAt = int.tryParse(entry.value) ?? 0;
        case 'ed2k_enable_kad':
          _ed2kEnableKad = entry.value == 'true';
        case 'ed2k_enable_upnp':
          _ed2kEnableUpnp = entry.value == 'true';
        case 'ed2k_listen_port':
          _ed2kListenPort = int.tryParse(entry.value) ?? 0;
        case 'torrent_assoc_prompted':
          _torrentAssocPrompted = entry.value == 'true';
        case 'notify_on_complete':
          _notifyOnComplete = entry.value != 'false'; // 默认 true
        case 'silent_download_enabled':
          _silentDownloadEnabled = entry.value == 'true'; // 默认 false
        case 'keep_awake_while_downloading':
          _keepAwakeWhileDownloading = entry.value == 'true'; // 默认 false
        case 'floating_ball_enabled':
          _floatingBallEnabled = entry.value == 'true'; // 默认 false
        case 'floating_ball_x':
          _floatingBallX = double.tryParse(entry.value) ?? -1;
        case 'floating_ball_y':
          _floatingBallY = double.tryParse(entry.value) ?? -1;
        case 'floating_ball_active_only':
          _floatingBallActiveOnly = entry.value == 'true'; // 默认 false
        case 'clipboard_watch_enabled':
          _clipboardWatchEnabled = entry.value == 'true'; // 默认 false
        case 'log_max_size_mb':
          _logMaxSizeMb = int.tryParse(entry.value) ?? 10;
          LogService.instance.maxTotalBytes = _logMaxSizeMb * 1024 * 1024;
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
        case 'local_server_takeover_enabled':
          _localServerTakeoverEnabled = entry.value == 'true';
        case 'local_server_jsonrpc_enabled':
          _localServerJsonrpcEnabled = entry.value == 'true';
        case 'local_server_api_enabled':
          _localServerApiEnabled = entry.value == 'true';
        case 'local_server_mcp_enabled':
          _localServerMcpEnabled = entry.value == 'true';
        case 'global_user_agent':
          _globalUserAgent = entry.value;
        case 'default_queue_id':
          _defaultQueueId = entry.value;
        case 'last_dialog_threads':
          _lastDialogThreads = entry.value;
        case 'remember_last_save_dir':
          _rememberLastSaveDir = entry.value == 'true';
        case 'last_save_dir':
          _lastSaveDir = entry.value;
        case 'reveal_file_cmd':
          _revealFileCmd = entry.value;
          revealFileCmdPresent = true;
        case 'open_dir_cmd':
          legacyOpenDirCmd = entry.value;
        case 'show_sidebar_status':
          _showSidebarStatus = entry.value != 'false';
        case 'show_sidebar_queues':
          _showSidebarQueues = entry.value != 'false';
        case 'show_sidebar_category':
          _showSidebarCategory = entry.value != 'false';
        case 'show_titlebar_pause_all':
          _showTitlebarPauseAll = entry.value != 'false';
        case 'show_titlebar_resume_all':
          _showTitlebarResumeAll = entry.value != 'false';
        case 'show_titlebar_settings':
          _showTitlebarSettings = entry.value != 'false';
        case 'show_titlebar_theme':
          _showTitlebarTheme = entry.value != 'false';
        case 'sidebar_queues_expanded':
          _sidebarQueuesExpanded = entry.value != 'false';
        case 'sidebar_category_expanded':
          _sidebarCategoryExpanded = entry.value == 'true';
        case 'custom_categories':
          _customCategories = CustomCategory.decodeList(entry.value);
      }
    }
    // 一次性迁移：把旧版拆分的「打开目录」命令(open_dir_cmd)并入统一的文件
    // 管理器命令。仅当 reveal_file_cmd 从未被持久化过（配置中无此键）时才搬；
    // 用户主动清空会留下空串条目（键存在），不再被旧值复活——修复「清空后
    // 无法重置为默认，每次启动都被 open_dir_cmd 搬回来」。
    if (!revealFileCmdPresent && legacyOpenDirCmd.isNotEmpty) {
      _revealFileCmd = legacyOpenDirCmd;
      _saveToRust('reveal_file_cmd', legacyOpenDirCmd);
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

  /// 启动时同步开机启动状态（从系统注册表读取实际状态）。
  /// 移动端无开机启动概念，launch_at_startup 插件也未注册，直接跳过。
  Future<void> _syncAutoStartupState() async {
    if (Platform.isAndroid || Platform.isIOS) return;
    final actual = await launchAtStartup.isEnabled();
    if (_autoStartup != actual) {
      _autoStartup = actual;
      notifyListeners();
    }
  }

  /// 平台默认下载目录（公开只读：供移动端判断「用户是否已自定义」）
  static String get platformDefaultSaveDir => _platformDefaultSaveDir();

  /// 平台默认下载目录
  static String _platformDefaultSaveDir() {
    if (Platform.isAndroid) {
      // 应用专属外部目录，无需存储权限即可写入；
      // 公共 Download 目录（SAF/MediaStore）作为后续跟进项。
      return '/storage/emulated/0/Android/data/com.fluxdown.app/files/Download';
    }
    final home =
        Platform.environment['USERPROFILE'] ??
        Platform.environment['HOME'] ??
        '.';
    return '$home${Platform.pathSeparator}Downloads';
  }
}
