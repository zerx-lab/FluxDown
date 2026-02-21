// FluxDown i18n — 所有 UI 字符串的翻译映射。
//
// 使用方法:
//   final s = S.of(context);
//   Text(s.newDownload)
//
// 支持语言: zh (中文), en (英文)
// 默认语言: 跟随系统，不支持的语言 fallback 到英文

class S {
  final String locale;

  S._(this.locale);

  // ─────────────────────────────────────────────
  // 工厂方法
  // ─────────────────────────────────────────────

  static S of(String locale) {
    if (locale.startsWith('zh')) return S._('zh');
    return S._('en');
  }

  String _t(String zh, String en) => locale == 'zh' ? zh : en;

  // ─────────────────────────────────────────────
  // 通用
  // ─────────────────────────────────────────────

  String get cancel => _t('取消', 'Cancel');
  String get confirm => _t('确定', 'OK');
  String get back => _t('返回', 'Back');
  String get settings => _t('设置', 'Settings');
  String get browse => _t('浏览', 'Browse');
  String get manage => _t('管理', 'Manage');
  String get auto => _t('自动', 'Auto');

  // ─────────────────────────────────────────────
  // 文件分类
  // ─────────────────────────────────────────────

  String get categoryAll => _t('全部文件', 'All Files');
  String get categoryVideo => _t('视频', 'Video');
  String get categoryAudio => _t('音频', 'Audio');
  String get categoryDocument => _t('文档', 'Document');
  String get categoryImage => _t('图片', 'Image');
  String get categoryArchive => _t('压缩包', 'Archive');
  String get categoryOther => _t('其他', 'Other');

  // ─────────────────────────────────────────────
  // 时间分组
  // ─────────────────────────────────────────────

  String get today => _t('今天', 'Today');
  String get yesterday => _t('昨天', 'Yesterday');
  String get thisWeek => _t('最近一周', 'This Week');
  String get thisMonth => _t('最近一个月', 'This Month');
  String get older => _t('更久以前', 'Older');

  // ─────────────────────────────────────────────
  // 任务状态
  // ─────────────────────────────────────────────

  String get statusPending => _t('等待中', 'Pending');
  String get statusDownloading => _t('下载中', 'Downloading');
  String get statusPaused => _t('已暂停', 'Paused');
  String get statusCompleted => _t('已完成', 'Completed');
  String get statusError => _t('失败', 'Error');
  String get statusPreparing => _t('准备中', 'Preparing');
  String get statusResuming => _t('恢复中', 'Resuming');

  // ─────────────────────────────────────────────
  // 任务副标题
  // ─────────────────────────────────────────────

  String get subtitlePaused => _t('已暂停', 'Paused');
  String get subtitleError => _t('下载失败', 'Download Failed');
  String get subtitlePending => _t('等待中...', 'Pending...');
  String get subtitlePreparing => _t('准备中...', 'Preparing...');
  String get subtitleResuming => _t('恢复中...', 'Resuming...');
  String get unknownSize => _t('未知大小', 'Unknown Size');
  String get unknownFile => _t('未知文件', 'Unknown File');
  String subtitleQueued(int pos) => _t('排队 #$pos', 'Queue #$pos');

  // ─────────────────────────────────────────────
  // 时间单位
  // ─────────────────────────────────────────────

  String etaSeconds(int n) => _t('$n 秒', '${n}s');
  String etaMinutes(int n) => _t('$n 分钟', '${n}m');
  String etaHours(String n) => _t('$n 小时', '${n}h');

  // ─────────────────────────────────────────────
  // Sidebar
  // ─────────────────────────────────────────────

  String get sidebarCategory => _t('分类', 'CATEGORY');
  String downloadUpdateVersion(String v) => _t('下载更新 v$v', 'Download v$v');
  String get installAndRestart => _t('立即安装并重启', 'Install & Restart');

  // ─────────────────────────────────────────────
  // HeaderBar
  // ─────────────────────────────────────────────

  String get newDownload => _t('新建下载', 'New Download');
  String get searchPlaceholder =>
      _t('搜索任务或设置...', 'Search tasks or settings...');
  String get searchGroupTasks => _t('下载任务', 'Downloads');
  String get searchGroupSettings => _t('设置', 'Settings');
  String settingsSearchSubtitle(String catLabel, String desc) =>
      _t('设置 · $catLabel · $desc', 'Settings · $catLabel · $desc');
  String get pauseAll => _t('全部暂停', 'Pause All');
  String get resumeAll => _t('全部恢复', 'Resume All');
  String get toggleToLight => _t('切换到亮色模式', 'Switch to Light Mode');
  String get toggleToDark => _t('切换到暗色模式', 'Switch to Dark Mode');

  // ─────────────────────────────────────────────
  // TaskTabBar
  // ─────────────────────────────────────────────

  String get tabAll => _t('全部', 'All');
  String get tabDownloading => _t('下载中', 'Downloading');
  String get tabCompleted => _t('已完成', 'Completed');
  String get tabPaused => _t('已暂停', 'Paused');
  String get tabError => _t('失败', 'Failed');
  String get selectAll => _t('全选', 'Select All');
  String get deselectAll => _t('取消全选', 'Deselect All');
  String selectedCount(int n) => _t('已选 $n 项', '$n selected');
  String get deleteTask => _t('删除任务', 'Delete Task');
  String get deleteTaskAndFile => _t('删除任务和文件', 'Delete Task & File');

  // ─────────────────────────────────────────────
  // TaskList
  // ─────────────────────────────────────────────

  String get startAll => _t('全部开始', 'Start All');
  String get activeGroupLabel => _t('正在下载', 'Active');
  String get emptyTitle => _t('暂无下载任务', 'No Downloads');
  String get emptySubtitle =>
      _t('点击「新建下载」或右键开始', 'Click "New Download" or right-click to start');
  String get colFileName => _t('文件名', 'File Name');
  String get colProgress => _t('进度', 'Progress');
  String get colSpeed => _t('速度', 'Speed');
  String get colStatus => _t('状态', 'Status');

  // ─────────────────────────────────────────────
  // TaskListItem (右键菜单)
  // ─────────────────────────────────────────────

  String get pause => _t('暂停', 'Pause');
  String get resume => _t('继续', 'Resume');
  String get openFile => _t('打开文件', 'Open File');
  String get openFolder => _t('打开所在文件夹', 'Open Folder');
  String get copyUrl => _t('复制下载地址', 'Copy URL');
  String get urlCopied => _t('已复制下载地址', 'URL Copied');

  // ─────────────────────────────────────────────
  // 删除确认对话框
  // ─────────────────────────────────────────────

  String deleteConfirmTitle(bool deleteFiles) =>
      deleteFiles ? deleteTaskAndFile : deleteTask;
  String deleteConfirmDesc(String fileName, bool deleteFiles) => deleteFiles
      ? _t(
          '确定要删除任务「$fileName」并删除已下载的文件吗？此操作不可撤销。',
          'Delete "$fileName" and its downloaded file? This cannot be undone.',
        )
      : _t(
          '确定要删除任务「$fileName」吗？已下载的文件将保留在磁盘上。',
          'Delete "$fileName"? The downloaded file will be kept on disk.',
        );
  String get batchDeleteTask => _t('批量删除任务', 'Batch Delete Tasks');
  String get batchDeleteTaskAndFile =>
      _t('批量删除任务和文件', 'Batch Delete Tasks & Files');
  String batchDeleteConfirmTitle(bool deleteFiles) =>
      deleteFiles ? batchDeleteTaskAndFile : batchDeleteTask;
  String batchDeleteConfirmDesc(int count, bool deleteFiles) => deleteFiles
      ? _t(
          '确定要删除选中的 $count 个任务并删除已下载的文件吗？此操作不可撤销。',
          'Delete $count selected tasks and their files? This cannot be undone.',
        )
      : _t(
          '确定要删除选中的 $count 个任务吗？已下载的文件将保留在磁盘上。',
          'Delete $count selected tasks? Downloaded files will be kept on disk.',
        );

  // ─────────────────────────────────────────────
  // DetailPanel
  // ─────────────────────────────────────────────

  String get detail => _t('详情', 'Details');
  String get selectTaskHint =>
      _t('选择一个任务查看详情', 'Select a task to view details');
  String get downloadDistribution => _t('下载分布', 'Download Distribution');
  String get infoSize => _t('大小', 'Size');
  String get infoDownloaded => _t('已下载', 'Downloaded');
  String get infoSpeed => _t('速度', 'Speed');
  String get infoRemaining => _t('剩余', 'Remaining');
  String get infoStatus => _t('状态', 'Status');
  String infoThreads(int n) => _t('$n 线程', '$n threads');
  String get infoPath => _t('路径', 'Path');
  String get infoError => _t('错误', 'Error');
  String get infoUrl => _t('地址', 'URL');
  String get resumingClickPause =>
      _t('恢复中...（点击暂停）', 'Resuming... (click to pause)');
  String get dynamicSplit => _t('拆分', 'Split');
  String splitCount(int total, int reactive, int proactive) => _t(
    '$total 次（主动 $proactive · 响应 $reactive）',
    '$total ($proactive proactive · $reactive reactive)',
  );
  String splitLatest(int parentNum, int childNum, String childSize) => _t(
    '最近: #$parentNum 拆分出 #$childNum ($childSize)',
    'Latest: #$parentNum split into #$childNum ($childSize)',
  );

  // ─────────────────────────────────────────────
  // NewDownloadDialog / QuickDownloadDialog
  // ─────────────────────────────────────────────

  String get addDownloadTask => _t('添加新的下载任务', 'Add a new download task');
  String get startDownload => _t('开始下载', 'Start');
  String get downloadUrl => _t('下载链接', 'Download URL');
  String get urlPlaceholder =>
      _t('HTTP / HTTPS / FTP 链接', 'HTTP / HTTPS / FTP URL');
  String get saveDir => _t('保存目录', 'Save Directory');
  String get selectSaveDir => _t('选择保存目录', 'Select save directory');
  String get threads => _t('线程数', 'Threads');
  String get renameOptional =>
      _t('重命名（可选，留空自动识别）', 'Rename (optional, auto-detect if empty)');
  String get autoDetectFilename => _t('自动识别文件名', 'Auto-detect filename');
  String get filenameOptional =>
      _t('文件名（留空自动识别）', 'Filename (auto-detect if empty)');
  String get fromBrowserExtension =>
      _t('来自浏览器扩展的下载请求', 'Download request from browser extension');

  // Torrent file
  String get selectTorrentFile => _t('选择种子文件', 'Select torrent file');
  String get openTorrentFile => _t('打开种子文件', 'Open .torrent file');
  String get torrentFileSelected => _t('已选择种子文件', 'Torrent file selected');
  String torrentFileCount(int count) =>
      _t('已选择 $count 个种子文件', '$count torrent file(s) selected');
  String get orSeparator => _t('或', 'or');

  // Batch download
  String get batchDownloadDesc => _t(
    '每行一个链接，支持 HTTP / FTP / 磁力链接',
    'One URL per line, supports HTTP / FTP / Magnet',
  );
  String get batchUrls => _t('下载链接列表', 'URL List');
  String get batchUrlPlaceholder => _t(
    '每行一个链接，例如：\nhttps://example.com/file1.zip\nhttps://example.com/file2.zip\nmagnet:?xt=urn:btih:...',
    'One URL per line, e.g.:\nhttps://example.com/file1.zip\nhttps://example.com/file2.zip\nmagnet:?xt=urn:btih:...',
  );
  String urlCount(int count) => _t('$count 个链接', '$count URLs');
  String startBatchDownload(int count) =>
      _t('下载 $count 个文件', 'Download $count files');

  // ─────────────────────────────────────────────
  // StatusBar
  // ─────────────────────────────────────────────

  String get statusDownloadingLabel => _t('下载中', 'Downloading');
  String get statusIdle => _t('空闲', 'Idle');
  String statusSummary(int active, int paused, int total) => _t(
    '$active 活跃 · $paused 暂停 · $total 总计',
    '$active active · $paused paused · $total total',
  );

  // ─────────────────────────────────────────────
  // Settings — 分类
  // ─────────────────────────────────────────────

  String get settingsCatGeneral => _t('通用', 'General');
  String get settingsCatGeneralDesc => _t('基本行为设置', 'Basic behavior settings');
  String get settingsCatAppearance => _t('外观', 'Appearance');
  String get settingsCatAppearanceDesc => _t('主题与配色', 'Theme & Colors');
  String get settingsCatDownload => _t('下载', 'Download');
  String get settingsCatDownloadDesc =>
      _t('下载引擎配置', 'Download engine settings');
  String get settingsCatBt => _t('BitTorrent', 'BitTorrent');
  String get settingsCatBtDesc => _t('BT 下载设置', 'BitTorrent settings');
  String get settingsCatProxy => _t('代理', 'Proxy');
  String get settingsCatProxyDesc => _t('网络代理配置', 'Network proxy settings');
  String get settingsCatAbout => _t('关于', 'About');
  String get settingsCatAboutDesc => _t('版本信息与更新', 'Version info & Updates');

  // ─────────────────────────────────────────────
  // Settings — 通用
  // ─────────────────────────────────────────────

  String get autoStartup => _t('开机自启动', 'Launch at Startup');
  String get autoStartupDesc =>
      _t('系统启动时自动运行 FluxDown', 'Automatically run FluxDown on system startup');
  String get closeToTray => _t('关闭时最小化到托盘', 'Minimize to Tray on Close');
  String get closeToTrayDesc => _t(
    '点击关闭按钮时隐藏到系统托盘，而非退出应用',
    'Hide to system tray instead of quitting when closing',
  );
  String get torrentFileAssociation =>
      _t('关联 .torrent 文件', 'Associate .torrent Files');
  String get torrentFileAssociationDesc => _t(
    '将 FluxDown 设为 .torrent 文件的默认打开方式',
    'Set FluxDown as the default app for .torrent files',
  );
  String get torrentAssocDialogTitle => _t('关联种子文件', 'Associate Torrent Files');
  String get torrentAssocDialogDesc => _t(
    '是否将 FluxDown 设为 .torrent 文件的默认打开方式？\n双击种子文件即可直接开始下载。',
    'Set FluxDown as the default app for .torrent files?\nDouble-click a torrent file to start downloading directly.',
  );
  String get analyticsEnabled => _t('数据分析', 'Analytics');
  String get analyticsEnabledDesc => _t(
    '发送匿名使用数据以帮助改进应用，不包含任何个人信息',
    'Send anonymous usage data to help improve the app. No personal information is collected',
  );
  String get settingFailed => _t('设置失败', 'Setting Failed');
  String get autoStartupFailedDesc => _t(
    '无法修改开机自启动设置，请检查系统权限。',
    'Failed to modify startup setting. Please check system permissions.',
  );

  // ─────────────────────────────────────────────
  // Settings — 外观
  // ─────────────────────────────────────────────

  String get language => _t('语言', 'Language');
  String get languageDesc => _t('选择界面显示语言', 'Choose display language');
  String get languageSystem => _t('跟随系统', 'System');
  String get languageChinese => _t('中文', '中文');
  String get languageEnglish => _t('English', 'English');

  String get themeMode => _t('主题模式', 'Theme Mode');
  String get themeModeDesc =>
      _t('选择亮色、暗色或跟随系统', 'Choose light, dark, or follow system');
  String get themeColor => _t('主题色', 'Theme Color');
  String get themeColorDesc => _t('选择应用的主色调', 'Choose the app accent color');
  String get themeModeSystem => _t('跟随系统', 'System');
  String get themeModeLight => _t('亮色', 'Light');
  String get themeModeDark => _t('暗色', 'Dark');

  // ─────────────────────────────────────────────
  // 主题色名称
  // ─────────────────────────────────────────────

  String get colorBlue => _t('蓝色', 'Blue');
  String get colorGreen => _t('绿色', 'Green');
  String get colorViolet => _t('紫色', 'Violet');
  String get colorRose => _t('玫红', 'Rose');
  String get colorCustom => _t('自定义', 'Custom');

  // ─────────────────────────────────────────────
  // Settings — 下载
  // ─────────────────────────────────────────────

  String get defaultSaveDir => _t('默认保存目录', 'Default Save Directory');
  String get defaultSaveDirDesc =>
      _t('新建下载任务时的默认保存位置', 'Default save location for new downloads');
  String get selectDefaultSaveDir =>
      _t('选择默认保存目录', 'Select default save directory');
  String get defaultThreads => _t('默认线程数', 'Default Threads');
  String get defaultThreadsDesc =>
      _t('每个下载任务的默认分片数量', 'Default segment count per download task');
  String get maxConcurrent => _t('最大同时下载数', 'Max Concurrent Downloads');
  String get maxConcurrentDesc =>
      _t('同时进行的最大下载任务数量', 'Maximum number of simultaneous downloads');
  String get speedLimit => _t('速度限制', 'Speed Limit');
  String get speedLimitDesc =>
      _t('限制全局下载速度（0 表示不限制）', 'Limit global download speed (0 = unlimited)');
  String get speedLimitUnit => _t('KB/s（0 = 不限制）', 'KB/s (0 = unlimited)');
  String nThreads(int n) => _t('$n 线程', '$n threads');
  String nTasks(int n) => _t('$n 个任务', '$n tasks');

  // ─────────────────────────────────────────────
  // Settings — 代理
  // ─────────────────────────────────────────────

  String get proxySettings => _t('代理设置', 'Proxy Settings');
  String get proxySettingsDesc => _t(
    '配置 HTTP/FTP 下载的网络代理',
    'Configure network proxy for HTTP/FTP downloads',
  );
  String get proxyModeNone => _t('不使用代理', 'No Proxy');
  String get proxyModeNoneDesc => _t('直接连接', 'Direct connection');
  String get proxyModeSystem => _t('系统代理', 'System Proxy');
  String get proxyModeSystemDesc => _t('从系统设置读取', 'Read from system settings');
  String get proxyModeManual => _t('手动配置', 'Manual');
  String get proxyModeManualDesc => _t('自定义代理服务器', 'Custom proxy server');
  String get proxyType => _t('代理类型', 'Proxy Type');
  String get proxyHost => _t('服务器地址', 'Server Address');
  String get proxyHostPlaceholder => _t('例如 127.0.0.1', 'e.g. 127.0.0.1');
  String get proxyPort => _t('端口', 'Port');
  String get proxyPortPlaceholder => _t('例如 1080', 'e.g. 1080');
  String get proxyUsername => _t('用户名', 'Username');
  String get proxyUsernamePlaceholder => _t('选填', 'Optional');
  String get proxyPassword => _t('密码', 'Password');
  String get proxyPasswordPlaceholder => _t('选填', 'Optional');
  String get proxyNoList => _t('排除列表', 'Bypass List');
  String get proxyNoListDesc =>
      _t('不通过代理的地址，逗号分隔', 'Addresses that bypass proxy, comma separated');
  String get proxyNoListPlaceholder =>
      _t('例如 localhost,192.168.*', 'e.g. localhost,192.168.*');
  String get proxyBtNote =>
      _t('BT 下载不支持代理', 'Proxy is not supported for BitTorrent downloads');
  String get proxySystemDetecting =>
      _t('正在检测系统代理...', 'Detecting system proxy...');
  String get proxySystemNotConfigured =>
      _t('系统未配置代理', 'No system proxy configured');
  String get proxySystemDetected =>
      _t('已检测到系统代理配置（只读）', 'System proxy detected (read-only)');
  String get proxyTestConnection => _t('测试连接', 'Test Connection');
  String get proxyTesting => _t('测试中...', 'Testing...');
  String proxyTestSuccess(int ms) =>
      _t('连接成功，延迟 $ms ms', 'Connected, latency $ms ms');
  String proxyTestFailed(String error) =>
      _t('连接失败: $error', 'Connection failed: $error');

  // Per-task proxy (新建下载对话框)
  String get taskProxy => _t('任务代理', 'Task Proxy');
  String get taskProxyDesc => _t(
    '为此任务使用独立代理（留空则使用全局设置）',
    'Use a separate proxy for this task (empty = use global)',
  );
  String get taskProxyPlaceholder =>
      _t('例如 socks5://127.0.0.1:1080', 'e.g. socks5://127.0.0.1:1080');
  String get taskProxyAdvanced => _t('高级选项', 'Advanced Options');
  String get taskProxyFormatHint => _t(
    '支持的代理格式：\n'
        '\n'
        '不带认证：\n'
        '  http://host:port\n'
        '  socks5://host:port\n'
        '\n'
        '带用户名密码：\n'
        '  http://user:pass@host:port\n'
        '  socks5://user:pass@host:port\n'
        '\n'
        '支持类型：http / https / socks4 / socks5\n'
        '留空则使用全局代理设置',
    'Supported proxy formats:\n'
        '\n'
        'Without auth:\n'
        '  http://host:port\n'
        '  socks5://host:port\n'
        '\n'
        'With username & password:\n'
        '  http://user:pass@host:port\n'
        '  socks5://user:pass@host:port\n'
        '\n'
        'Supported types: http / https / socks4 / socks5\n'
        'Leave empty to use global proxy settings',
  );

  // ─────────────────────────────────────────────
  // Settings — BT 下载
  // ─────────────────────────────────────────────

  String get btSettings => _t('BT 下载设置', 'BitTorrent Settings');
  String get btSettingsDesc =>
      _t('BitTorrent 协议相关配置', 'BitTorrent protocol settings');
  String get btEnableDht => _t('启用 DHT', 'Enable DHT');
  String get btEnableDhtDesc => _t(
    '分布式哈希表，无需 Tracker 即可发现对等节点',
    'Distributed Hash Table for trackerless peer discovery',
  );
  String get btEnableUpnp => _t('启用 UPnP 端口映射', 'Enable UPnP Port Mapping');
  String get btEnableUpnpDesc => _t(
    '自动配置路由器端口转发，提高连接性',
    'Auto-configure router port forwarding for better connectivity',
  );
  String get btListenPort => _t('监听端口范围', 'Listen Port Range');
  String get btListenPortDesc =>
      _t('用于接收 BT 连接的端口范围', 'Port range for incoming BT connections');
  String get btListenPortStart => _t('起始端口', 'Start Port');
  String get btListenPortEnd => _t('结束端口', 'End Port');
  String get btTrackerList => _t('Tracker 列表', 'Tracker List');
  String get btTrackerListDesc => _t(
    '用于发现对等节点的 Tracker 服务器，每行一个地址',
    'Tracker servers for peer discovery, one URL per line',
  );
  String get btTrackerPlaceholder => _t(
    '每行一个 Tracker 地址，例如：\nudp://tracker.opentrackr.org:1337/announce\nhttps://tracker.example.com/announce',
    'One tracker URL per line, e.g.:\nudp://tracker.opentrackr.org:1337/announce\nhttps://tracker.example.com/announce',
  );
  String btTrackerCount(int n) => _t('$n 个 Tracker', '$n trackers');
  String get btResetTrackers => _t('重置为默认', 'Reset to Default');
  String get btResetTrackersConfirm =>
      _t('确定要恢复默认的 Tracker 列表吗？', 'Reset tracker list to defaults?');
  String get btPortInvalid => _t(
    '端口范围无效（1024-65535，起始 ≤ 结束）',
    'Invalid port range (1024-65535, start ≤ end)',
  );
  String get btSettingsRestartHint => _t(
    '部分设置需要重启 BT 引擎后生效',
    'Some settings require BT engine restart to take effect',
  );

  // ─────────────────────────────────────────────
  // Settings — 关于
  // ─────────────────────────────────────────────

  String get appDescription =>
      _t('多协议高速下载工具', 'Multi-protocol high-speed downloader');
  String get currentVersion => _t('当前版本', 'Current Version');
  String get latestVersion => _t('最新版本', 'Latest Version');
  String get publishDate => _t('发布时间', 'Published');
  String get softwareUpdate => _t('软件更新', 'Software Update');
  String get checkUpdateDesc => _t('检查是否有新版本可用', 'Check for available updates');
  String get autoCheckUpdate => _t('自动检查更新', 'Auto Check for Updates');
  String get autoCheckUpdateDesc =>
      _t('启动应用时自动检查新版本', 'Automatically check for updates on startup');
  String get upToDate => _t('已是最新版本', 'Up to Date');
  String newVersionFound(String v) =>
      _t('发现新版本 v$v', 'New version v$v available');
  String get downloadComplete =>
      _t('下载完成，可以安装', 'Download complete, ready to install');
  String get downloadingUpdate => _t('正在下载更新...', 'Downloading update...');
  String get checking => _t('检查中...', 'Checking...');
  String get checkUpdate => _t('检查更新', 'Check for Updates');
  String downloadUpdate(String size) => _t('下载更新 ($size)', 'Download ($size)');
  String get recheck => _t('重新检查', 'Recheck');
  String get officialWebsite => _t('官方网站', 'Official Website');
  String get visitWebsiteForMore =>
      _t('访问官网获取更多信息', 'Visit website for more information');

  // ─────────────────────────────────────────────
  // 更新日志弹窗
  // ─────────────────────────────────────────────

  String get changelogTitle => _t('发现新版本', 'New Version Available');
  String changelogSubtitle(String v) => _t(
    'FluxDown v$v 已发布，以下是更新内容：',
    'FluxDown v$v is out. Here\'s what\'s new:',
  );
  String get changelogUpdateNow => _t('立即更新', 'Update Now');
  String get changelogLater => _t('稍后', 'Later');
  String changelogVersionCount(int n) => _t('跨越 $n 个版本', '$n versions behind');

  // ─────────────────────────────────────────────
  // Settings — 搜索关键词
  // ─────────────────────────────────────────────

  List<String> get searchKeywordsAutoStartup =>
      _t('开机,自启动,启动', 'startup,auto,boot,launch').split(',')
        ..addAll(['startup', 'auto', 'boot']);
  List<String> get searchKeywordsCloseToTray =>
      _t('关闭,托盘,最小化', 'close,tray,minimize').split(',')
        ..addAll(['tray', 'close', 'minimize']);
  List<String> get searchKeywordsLanguage =>
      _t('语言,中文,英文,切换语言', 'language,chinese,english,locale').split(',')
        ..addAll(['language', 'locale', 'lang']);
  List<String> get searchKeywordsThemeMode =>
      _t('主题,亮色,暗色,深色,模式', 'theme,dark,light,mode').split(',')
        ..addAll(['theme', 'dark', 'light']);
  List<String> get searchKeywordsThemeColor =>
      _t('主题色,颜色,配色,色调', 'color,scheme,accent').split(',')
        ..addAll(['color', 'scheme', 'accent']);
  List<String> get searchKeywordsSaveDir =>
      _t('保存,目录,路径,文件夹', 'save,directory,path,folder').split(',')
        ..addAll(['save', 'directory', 'path', 'folder']);
  List<String> get searchKeywordsThreads =>
      _t('线程,分片,并行', 'thread,segment,parallel').split(',')
        ..addAll(['segment', 'thread']);
  List<String> get searchKeywordsConcurrent =>
      _t('同时,并发,并行,数量', 'concurrent,parallel,max').split(',')
        ..addAll(['concurrent', 'parallel', 'max']);
  List<String> get searchKeywordsSpeedLimit =>
      _t('速度,限速,限制,带宽', 'speed,limit,bandwidth').split(',')
        ..addAll(['speed', 'limit', 'bandwidth']);
  List<String> get searchKeywordsUpdate =>
      _t('更新,升级,版本', 'update,upgrade,version').split(',')
        ..addAll(['update', 'upgrade', 'version']);
  List<String> get searchKeywordsFileAssoc => _t(
    '文件关联,种子,torrent,关联,默认程序',
    'file,association,torrent,default,open',
  ).split(',')..addAll(['torrent', 'association', 'file']);
  List<String> get searchKeywordsAnalytics => _t(
    '数据,分析,统计,匿名,隐私,遥测',
    'analytics,data,statistics,anonymous,privacy,telemetry',
  ).split(',')..addAll(['analytics', 'telemetry', 'privacy']);
  List<String> get searchKeywordsBtSettings => _t(
    'BT,BitTorrent,种子,磁力,Tracker,DHT,UPnP,端口',
    'BT,BitTorrent,torrent,magnet,tracker,DHT,UPnP,port',
  ).split(',')..addAll(['bt', 'torrent', 'tracker', 'dht', 'peer']);
  List<String> get searchKeywordsProxy => _t(
    '代理,HTTP,SOCKS,SOCKS5,SOCKS4,网络代理,代理服务器',
    'proxy,HTTP,SOCKS,SOCKS5,SOCKS4,network,server',
  ).split(',')..addAll(['proxy', 'socks', 'http']);

  // ─────────────────────────────────────────────
  // Feedback
  // ─────────────────────────────────────────────

  String get feedback => _t('反馈', 'Feedback');
  String get feedbackTitle => _t('提交反馈', 'Submit Feedback');
  String get feedbackDesc => _t(
    '告诉我们你的想法，帮助改进 FluxDown',
    'Share your thoughts to help improve FluxDown',
  );
  String get feedbackTypeLabel => _t('反馈类型', 'Feedback Type');
  String get feedbackTypeFeature => _t('功能建议', 'Feature Request');
  String get feedbackTypeBug => _t('问题反馈', 'Bug Report');
  String get feedbackTypeOther => _t('其他', 'Other');
  String get feedbackTitleLabel => _t('标题', 'Title');
  String get feedbackTitlePlaceholder =>
      _t('简要描述你的反馈', 'Briefly describe your feedback');
  String get feedbackDescLabel => _t('详细描述', 'Description');
  String get feedbackDescPlaceholder =>
      _t('请详细说明...', 'Please describe in detail...');
  String get feedbackContactLabel => _t('联系方式', 'Contact');
  String get feedbackContactPlaceholder =>
      _t('邮箱或其他联系方式', 'Email or other contact info');
  String get feedbackContactHint => _t(
    '填写邮箱可收到反馈进度通知，其他联系方式可能无法收到通知',
    'Enter your email to receive progress notifications. Other contact methods may not receive notifications.',
  );
  String get feedbackOptional => _t('可选', 'Optional');
  String get feedbackSubmit => _t('提交', 'Submit');
  String get feedbackSubmitting => _t('提交中...', 'Submitting...');
  String get feedbackSuccess => _t('感谢你的反馈！', 'Thank you for your feedback!');
  String get feedbackError =>
      _t('提交失败，请稍后重试', 'Submission failed, please try again later');
  String get feedbackRateLimited =>
      _t('提交过于频繁，请稍后再试', 'Too many requests, please try again later');
  String feedbackTitleCount(int n) => _t('$n/200', '$n/200');
  String feedbackDescCount(int n) => _t('$n/5000', '$n/5000');

  // ─────────────────────────────────────────────
  // HLS 画质选择
  // ─────────────────────────────────────────────

  String get hlsQualityTitle => _t('选择画质', 'Select Quality');
  String get hlsQualityDesc => _t(
    '检测到多个画质版本，请选择要下载的画质',
    'Multiple quality options detected. Choose the one to download',
  );
  String hlsQualityResolution(int w, int h) => '${w}x$h';
  String hlsQualityBandwidth(String speed) => _t(speed, speed);

  // ─────────────────────────────────────────────
  // TrayService
  // ─────────────────────────────────────────────

  String get trayShowWindow => _t('显示主窗口', 'Show Window');
  String get trayExit => _t('退出', 'Exit');

  // ─────────────────────────────────────────────
  // DownloadCompleteWindow
  // ─────────────────────────────────────────────

  String get downloadCompleted => _t('下载完成', 'Download Complete');
  String batchDownloadCompleted(int count) =>
      _t('$count 个任务下载完成', '$count Downloads Complete');
  String andMoreFiles(int count) => _t('等 $count 个文件', 'and $count more');
  String get openFileFolder => _t('打开文件夹', 'Open Folder');
}
