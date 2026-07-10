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
  String get close => _t('关闭', 'Close');
  String get back => _t('返回', 'Back');
  String get settings => _t('设置', 'Settings');
  String get browse => _t('浏览', 'Browse');
  String get manage => _t('管理', 'Manage');
  String get manageTooltip => 'Ctrl+A';
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
  String get statusFileMissing => _t('文件已删除', 'File deleted');

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

  String get sidebarStatus => _t('状态', 'STATUS');
  String get sidebarQueues => _t('队列', 'QUEUES');
  String get sidebarCategory => _t('分类', 'CATEGORY');
  String get defaultQueue => _t('默认队列', 'Default Queue');
  String get createQueueAction => _t('新建队列', 'New Queue');
  String get editQueue => _t('编辑队列', 'Edit Queue');
  String get deleteQueueAction => _t('删除队列', 'Delete Queue');
  String get queueNameLabel => _t('队列名称', 'Queue Name');
  String get queueNameHint => _t('输入队列名称', 'Enter queue name');
  String get queueSpeedLimit => _t('速度限制 (KB/s)', 'Speed Limit (KB/s)');
  String get queueSpeedLimitHint => _t('0 = 不限制', '0 = Unlimited');
  String get queueMaxConcurrent => _t('最大同时下载数', 'Max Concurrent Downloads');
  String get queueMaxConcurrentHint =>
      _t('0 = 使用全局设置', '0 = Use global setting');
  String get queueDefaultSegments => _t('线程数量', 'Threads');
  String get queueDefaultSegmentsHint => _t('0 = 自动', '0 = Auto');
  String get queueSaveDir => _t('默认保存目录', 'Default Save Directory');
  String get queueDefaultUserAgent => _t('默认 User-Agent', 'Default User-Agent');
  String get queueUaInheritGlobal => _t('继承全局设置', 'Inherit Global');
  String get queueUaHint => _t('留空继承全局 UA', 'Leave empty to inherit global UA');
  String queueDeleteConfirmDesc(String name) => _t(
    '确定要删除队列「$name」吗？队列中的任务将移至默认队列。',
    'Delete queue "$name"? Tasks in this queue will be moved to the default queue.',
  );
  String get taskQueueLabel => _t('下载队列', 'Queue');
  String get defaultQueueSetting => _t('默认下载队列', 'Default Queue');
  String get defaultQueueSettingDesc => _t(
    '浏览器扩展和新建下载时默认使用的队列',
    'Default queue for browser extension and new downloads',
  );
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
  String get settingsSearchHint => _t('搜索设置...', 'Search settings...');
  String get settingsSearchNoResults => _t('无匹配的设置项', 'No matching settings');
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
  String get colEta => _t('剩余时间', 'ETA');
  String get colStatus => _t('状态', 'Status');

  // ─────────────────────────────────────────────
  // TaskListItem (右键菜单)
  // ─────────────────────────────────────────────

  String get pause => _t('暂停', 'Pause');
  String get resume => _t('继续', 'Resume');
  String get boostDownload => _t('优先下载', 'Boost Download');
  String get cancelBoost => _t('取消优先', 'Cancel Boost');
  String boostBannerActive(String fileName, int count) => _t(
    '⚡ 优先下载：$fileName — 完成后自动恢复 $count 个任务',
    '⚡ Boost: $fileName — $count task(s) will resume on completion',
  );
  String get boostBannerCancel => _t('取消', 'Cancel');
  String get openFile => _t('打开文件', 'Open File');
  String get openFolder => _t('打开所在文件夹', 'Open Folder');
  String get copyUrl => _t('复制下载地址', 'Copy URL');
  String get urlCopied => _t('已复制下载地址', 'URL Copied');
  String get errorCopied => _t('已复制错误信息', 'Error message copied');

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

  String get batchDeletingTitle => _t('批量删除中', 'Deleting Tasks');
  String batchDeletingProgress(int done, int total) =>
      _t('正在删除 $done / $total 个任务...', 'Deleting $done of $total tasks...');

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
  String get customThreads => _t('自定义', 'Custom');
  String get customThreadsHint => _t('输入 1-256', 'Enter 1-256');
  String customRangeHint(int min, int max) =>
      _t('输入 $min-$max', 'Enter $min-$max');
  String get threadsInvalidRange =>
      _t('线程数须在 1-256 之间', 'Threads must be between 1 and 256');
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
  String get importTxtFile => _t('导入 TXT 文件', 'Import TXT file');
  String get importTxtNoUrls =>
      _t('未在文件中找到有效链接', 'No valid URLs found in file');
  String importTxtFound(int count) =>
      _t('已导入 $count 个链接', 'Imported $count URLs');

  // ─────────────────────────────────────────────
  // BT File Selection Dialog
  // ─────────────────────────────────────────────

  String get btFileSelectTitle => _t('选择要下载的文件', 'Select Files to Download');
  String get btFileSelectDescSingle => _t(
    '该种子包含 1 个文件，确认后开始下载。',
    'This torrent contains 1 file. Confirm to start downloading.',
  );
  String btFileSelectDesc(int count) => _t(
    '该种子包含 $count 个文件，请选择要下载的文件。',
    'This torrent contains $count files. Select which files to download.',
  );
  String get btFileSelectAll => _t('全部文件', 'All Files');
  String btFileSelectConfirm(int count, String size) =>
      _t('下载 $count 个文件（$size）', 'Download $count file(s) ($size)');
  String get btResolvingMagnet =>
      _t('正在解析磁力链接，请稍候...', 'Resolving magnet link, please wait...');
  String get btResolveFailed => _t(
    '磁力链接解析失败：未获取到元数据（无可用 peers 或 DHT 被屏蔽）。任务已标记为错误，可稍后在任务列表重试。',
    'Failed to resolve magnet link: no metadata received (no peers or DHT blocked). The task was marked as error; you can retry it later from the task list.',
  );
  String get btWaitingFiles => _t('请选择要下载的文件', 'Select files to download');
  String get btProbing => _t('正在解析种子文件...', 'Parsing torrent file...');
  String get btProbeError => _t(
    '种子文件解析失败，将在下载开始后重新解析',
    'Failed to parse torrent file; will retry after download starts',
  );
  String btStartWithSelection(int count, String size) =>
      _t('下载 $count 个文件（$size）', 'Download $count file(s) ($size)');

  // ─────────────────────────────────────────────
  // StatusBar
  // ─────────────────────────────────────────────

  String get statusDownloadingLabel => _t('下载中', 'Downloading');
  String get statusIdle => _t('空闲', 'Idle');
  String statusSummary(int active, int paused, int total) => _t(
    '$active 活跃 · $paused 暂停 · $total 总计',
    '$active active · $paused paused · $total total',
  );
  String get statusSpeedLimitOff => _t('无限制', 'Unlimited');
  String get statusSpeedLimitKbs => _t('KB/s', 'KB/s');
  String get statusSpeedLimitHint => _t('自定义速率', 'Custom rate');
  String get speedLimitTitle => _t('全局限速', 'Global Speed Limit');
  String get speedLimitCustom => _t('自定义', 'Custom');
  String get shutdownTriggerLabel => _t('完成后关机', 'Shutdown when done');
  String get shutdownTitle => _t('任务完成后自动关机', 'Auto Shutdown When Done');
  String get shutdownNeedActiveTask =>
      _t('有任务运行中才能开启', 'Enable only while tasks are running');
  String get shutdownDelayLabel => _t('完成后延迟', 'Delay after completion');
  String get shutdownMinutesUnit => _t('分钟', 'min');
  String shutdownDelayMinutes(int m) => _t('$m 分钟', '$m min');
  String get shutdownImmediate => _t('立即', 'Immediately');
  String get shutdownArmedHintImmediate => _t(
    '所有任务完成后将立即关机',
    'PC will shut down immediately after all downloads finish',
  );
  String shutdownArmedHint(int m) => _t(
    '所有任务完成 $m 分钟后将自动关机',
    'PC will shut down $m min after all downloads finish',
  );
  String shutdownCountdown(String time) =>
      _t('将在 $time 后关机', 'Shutting down in $time');
  String get shutdownCancelButton => _t('取消关机', 'Cancel Shutdown');

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
  String get settingsCatEd2k => _t('eD2K', 'eD2K');
  String get settingsCatEd2kDesc =>
      _t('电驴 / eMule 下载设置', 'eD2K / eMule settings');
  String get settingsCatProxy => _t('代理', 'Proxy');
  String get settingsCatProxyDesc => _t('网络代理配置', 'Network proxy settings');
  String get settingsCatApiService => _t('API 服务', 'API Service');
  String get settingsCatApiServiceDesc => _t(
    '浏览器脚本接管、aria2 RPC 兼容与管理 API',
    'Browser takeover, aria2 RPC compatibility & management API',
  );
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
  String get startMinimizedToTray =>
      _t('启动时最小化到托盘', 'Start minimized to tray');
  String get startMinimizedToTrayDesc => _t(
    '启动后不显示主窗口，仅托盘驻留',
    'App starts hidden in the system tray',
  );
  String get floatingBall => _t('桌面悬浮球', 'Desktop Floating Ball');
  String get floatingBallDesc => _t(
    '桌面常驻置顶小球，显示下载速度与进度，支持拖拽链接/种子快速建任务',
    'Always-on-top desktop widget showing speed & progress; drag URLs or torrents onto it to create tasks',
  );
  String get floatingBallActiveOnly => _t('仅下载时显示', 'Show Only While Downloading');
  String get floatingBallActiveOnlyDesc => _t(
    '仅在有下载任务进行时显示悬浮球，其余时候自动隐藏',
    'Show the floating ball only when downloads are active; hide it otherwise',
  );
  String get floatingBallWaylandUnsupported => _t(
    '当前桌面环境（Wayland）不支持悬浮窗定位与置顶，悬浮球不可用。已启用替代方案：托盘显示实时速度；复制链接后唤起主窗口会自动填入下载对话框。本地文件（含 .torrent）请从主窗口添加或双击文件。',
    'Your desktop session (Wayland) does not allow positioned always-on-top windows, so the floating ball is unavailable. Alternatives enabled: the tray shows live speed, and copied URLs are auto-filled into the download dialog when you open the main window. Add local files (incl. .torrent) from the main window or by double-clicking them.',
  );
  String get clipboardWatch => _t('剪贴板监听', 'Clipboard Watcher');
  String get clipboardWatchDesc => _t(
    '主窗口隐藏时监测剪贴板中的下载链接并通知（仅部分桌面环境可用）',
    'Watch the clipboard for download URLs while the main window is hidden (availability depends on desktop environment)',
  );
  String get clipboardUrlDetectedTitle =>
      _t('检测到下载链接', 'Download link detected');
  String get clipboardUrlDetectedBody =>
      _t('点击创建下载任务', 'Click to create a download task');
  String get trayShowFloatingBall => _t('显示悬浮球', 'Show Floating Ball');
  String get hideFloatingBall => _t('隐藏悬浮球', 'Hide Floating Ball');
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
  String get notifyOnComplete => _t('完成通知', 'Completion Notifications');
  String get notifyOnCompleteDesc => _t(
    '任务完成时弹出系统通知，关闭后所有任务完成将不再通知',
    'Show a system notification when a task completes. When disabled, no completion notifications will be shown',
  );
  String get silentDownload => _t('免打扰下载', 'Silent Download');
  String get silentDownloadDesc => _t(
    '浏览器扩展等外部下载请求不再弹出确认框，直接按默认设置开始下载',
    'Start external downloads (e.g. from the browser extension) immediately with default settings, without showing the confirmation dialog',
  );
  String get keepAwakeWhileDownloading =>
      _t('下载时保持唤醒', 'Keep Awake While Downloading');
  String get keepAwakeWhileDownloadingDesc => _t(
    '有任务下载时阻止系统睡眠和息屏，任务完成后自动恢复',
    'Prevent the system from sleeping or turning off the display while downloads are active. Restores automatically when done',
  );

  // 侧边栏显示
  String get sidebarVisibility => _t('侧边栏显示', 'Sidebar Sections');
  String get sidebarVisibilityDesc =>
      _t('选择在侧边栏中显示哪些区块', 'Choose which sections to show in the sidebar');
  String get showSidebarStatus => _t('显示状态', 'Show Status');
  String get showSidebarStatusDesc =>
      _t('在侧边栏显示状态筛选区块', 'Show status filter section in sidebar');
  String get showSidebarQueues => _t('显示队列', 'Show Queues');
  String get showSidebarQueuesDesc =>
      _t('在侧边栏显示队列区块', 'Show queues section in sidebar');
  String get showSidebarCategory => _t('显示分类', 'Show Category');
  String get showSidebarCategoryDesc =>
      _t('在侧边栏显示分类区块', 'Show category section in sidebar');
  String get hideSection => _t('隐藏此区块', 'Hide this section');

  // 标题栏按钮
  String get titlebarButtons => _t('标题栏按钮', 'Titlebar Buttons');
  String get titlebarButtonsDesc => _t(
    '选择在标题栏显示哪些工具按钮，也可右键按钮直接隐藏',
    'Choose which tool buttons to show in the titlebar; right-click a button to hide it',
  );
  String get showTitlebarPauseAll => _t('全部暂停按钮', 'Pause All Button');
  String get showTitlebarPauseAllDesc =>
      _t('在标题栏显示全部暂停按钮', 'Show pause all button in the titlebar');
  String get showTitlebarResumeAll => _t('全部恢复按钮', 'Resume All Button');
  String get showTitlebarResumeAllDesc =>
      _t('在标题栏显示全部恢复按钮', 'Show resume all button in the titlebar');
  String get showTitlebarSettings => _t('设置按钮', 'Settings Button');
  String get showTitlebarSettingsDesc =>
      _t('在标题栏显示设置按钮', 'Show settings button in the titlebar');
  String get showTitlebarTheme => _t('主题切换按钮', 'Theme Toggle Button');
  String get showTitlebarThemeDesc =>
      _t('在标题栏显示主题切换按钮', 'Show theme toggle button in the titlebar');
  String get hideButton => _t('隐藏此按钮', 'Hide this button');

  // ─────────────────────────────────────────────
  // 自定义分类
  // ─────────────────────────────────────────────

  String get customCategories => _t('自定义分类', 'Custom Categories');
  String get customCategoriesDesc => _t(
    '创建自定义文件分类，按扩展名或正则表达式匹配',
    'Create custom file categories, match by extension or regex',
  );
  String get addCategory => _t('添加分类', 'Add Category');
  String get editCategory => _t('编辑分类', 'Edit Category');
  String get deleteCategory => _t('删除分类', 'Delete Category');
  String get deleteCategoryConfirm => _t(
    '确定要删除此分类吗？已下载的文件不受影响。',
    'Are you sure you want to delete this category? Downloaded files are not affected.',
  );
  String get categoryName => _t('分类名称', 'Category Name');
  String get categoryNameHint => _t('例如：电子书', 'e.g. eBooks');
  String get categoryIcon => _t('图标', 'Icon');
  String get matchMode => _t('匹配方式', 'Match Mode');
  String get matchByExtension => _t('按扩展名', 'By Extension');
  String get matchByRegex => _t('按正则表达式', 'By Regex');
  String get extensionsLabel => _t('扩展名', 'Extensions');
  String get extensionsHint =>
      _t('逗号分隔，如：epub, mobi, azw3', 'Comma separated, e.g. epub, mobi, azw3');
  String get regexLabel => _t('正则表达式', 'Regex Pattern');
  String get regexHint =>
      _t(r'匹配文件名，如：.*\.(epub|mobi)$', r'Match filename, e.g. .*\.(epub|mobi)$');
  String get regexInvalid => _t('正则表达式无效', 'Invalid regex pattern');
  String get categoryNameRequired => _t('请输入分类名称', 'Category name is required');
  String get extensionsRequired =>
      _t('请输入至少一个扩展名', 'Enter at least one extension');
  String get categorySaveDir => _t('分类保存目录', 'Category Save Directory');
  String get categorySaveDirDesc =>
      _t('留空则使用全局默认目录', 'Leave empty to use global default');
  String get restoreDefaultPath => _t('恢复默认', 'Restore Default');
  String get nCustomCategories => _t('个自定义分类', ' custom categories');
  String get resetBuiltinCategories => _t('恢复默认', 'Reset Defaults');
  String get resetAllCategoriesConfirm => _t(
    '确定要恢复所有分类为默认状态吗？自定义分类将被删除，内置分类将恢复初始配置。',
    'Reset all categories to defaults? Custom categories will be removed and built-in categories will be restored to their initial configuration.',
  );
  String get builtinCategory => _t('内置', 'Built-in');
  String get customCategory => _t('自定义', 'Custom');
  String get categoryPriorityNote => _t(
    '自定义分类优先于内置分类匹配，上下箭头可调整顺序',
    'Custom categories take priority over built-in ones. Use arrows to reorder.',
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
  String get themeSelection => _t('主题', 'Theme');
  String get themeSelectionDesc =>
      _t('选择界面主题风格', 'Choose the interface theme style');
  String get themeColor => _t('主题色', 'Theme Color');
  String get themeColorDesc => _t('选择应用的主色调', 'Choose the app accent color');
  String get themeModeSystem => _t('跟随系统', 'System');
  String get themeModeLight => _t('亮色', 'Light');
  String get themeModeDark => _t('暗色', 'Dark');

  String get uiScale => _t('界面缩放', 'Interface Scale');
  String get uiScaleDesc =>
      _t('调整界面整体缩放比例', 'Adjust the overall interface scale');

  String get appIcon => _t('应用图标', 'App Icon');
  String get appIconDesc => _t(
    '自定义窗口、任务栏与托盘使用的应用图标',
    'Customize the icon used by the window, taskbar and tray',
  );
  String get appIconDefault => _t('默认', 'Default');
  String get appIconCustom => _t('自定义', 'Custom');
  String get appIconBolt => _t('闪电', 'Bolt');
  String get appIconChooseImage => _t('选择图片…', 'Choose Image…');
  String get appIconApplyFailed => _t('设置应用图标失败', 'Failed to set app icon');
  String get appIconZoomHint => _t('滚轮缩放', 'Scroll to zoom');

  // ─────────────────────────────────────────────
  // 内置主题名称
  // ─────────────────────────────────────────────

  String get themeDefaultDark => _t('默认深色', 'Default Dark');
  String get themeDefaultLight => _t('默认亮色', 'Default Light');
  String get themeMidnightBlue => _t('午夜蓝', 'Midnight Blue');
  String get themeNord => _t('Nord', 'Nord');
  String get themeWarmLight => _t('暖光', 'Warm Light');
  String get themeDarkTheme => _t('深色主题', 'Dark Theme');
  String get themeLightTheme => _t('亮色主题', 'Light Theme');
  String get themeImport => _t('导入', 'Import');
  String get themeExport => _t('导出', 'Export');
  String get themeImportSuccess => _t('主题导入成功', 'Theme imported successfully');
  String get themeExportSuccess => _t('主题导出成功', 'Theme exported successfully');
  String get themeImportError => _t('主题导入失败', 'Failed to import theme');
  String get themeMore => _t('更多主题', 'More Themes');

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
  String get rememberLastSaveDir => _t('跟随上次保存位置', 'Remember Last Save Location');
  String get rememberLastSaveDirDesc => _t(
    '开启后新建下载默认使用上次下载的保存位置，而非固定的默认目录',
    'Use the last download location as the default for new downloads instead of the fixed directory',
  );
  String get defaultThreads => _t('最大连接数', 'Max Connections');
  String get defaultThreadsDesc => _t(
    '每个任务允许的最大并发连接数上限，下载器从低并发起步按服务器表现逐步提升',
    'Per-task connection cap; the downloader ramps up gradually based on server behaviour',
  );
  String get autoMaxConnections => _t('Auto 模式连接上限', 'Auto Mode Connection Cap');
  String get autoMaxConnectionsDesc => _t(
    '连接数设为 Auto 时智能调度允许的最大连接数',
    'Maximum connections the auto scheduler may use when threads are set to Auto',
  );
  String get connPolicyCache => _t('已学习的服务器策略', 'Learned Server Policies');
  String get connPolicyCacheDesc => _t(
    '服务器拒绝多连接（403/429）后引擎会记住其连接上限 24 小时；若服务器已解除限制，可手动清除重新学习',
    'After a server rejects parallel connections (403/429), its cap is remembered for 24h; clear to relearn if the limit was lifted',
  );
  String get connPolicyCacheClear => _t('清除', 'Clear');
  String get connPolicyCacheCleared =>
      _t('已清除服务器策略缓存', 'Server policy cache cleared');
  String get connPolicyCacheEmpty => _t('暂无记录', 'No records');
  String nRecords(int n) => _t('$n 条记录', '$n records');
  String get maxConcurrent => _t('最大同时下载数', 'Max Concurrent Downloads');
  String get maxConcurrentDesc =>
      _t('同时进行的最大下载任务数量', 'Maximum number of simultaneous downloads');
  String get speedLimit => _t('速度限制', 'Speed Limit');
  String get speedLimitDesc =>
      _t('限制全局下载速度（0 表示不限制）', 'Limit global download speed (0 = unlimited)');
  String get speedLimitUnit => _t('KB/s（0 = 不限制）', 'KB/s (0 = unlimited)');
  String nThreads(int n) => _t('$n 线程', '$n threads');
  String nTasks(int n) => _t('$n 个任务', '$n tasks');

  // 失败自动重试
  String get autoRetryCount => _t('失败重试次数', 'Auto-retry Attempts');
  String get autoRetryCountDesc => _t(
    '网络中断等瞬时错误失败后自动重试的次数',
    'Number of automatic retries after transient errors (e.g. network drops)',
  );
  String get autoRetryOff => _t('关闭', 'Off');
  String get autoRetryUnlimited => _t('无限', 'Unlimited');
  String nRetries(int n) => _t('$n 次', '$n times');
  String get autoRetryDelay => _t('重试间隔', 'Retry Interval');
  String get autoRetryDelayDesc => _t(
    '每次自动重试前的等待秒数（按重试次数递增）',
    'Seconds to wait before each auto-retry (increases per attempt)',
  );
  String get autoRetryDelayUnit => _t('秒（0 = 立即）', 'sec (0 = immediate)');

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

  // User-Agent 设置
  String get userAgent => _t('用户代理 (User-Agent)', 'User-Agent');
  String get userAgentDesc => _t(
    '下载请求时使用的浏览器标识。百度网盘直链下载需设为 netdisk',
    'Browser identity used in download requests. Set to "netdisk" for Baidu Pan.',
  );
  String get userAgentPlaceholder => _t(
    '留空使用默认标识 FluxDown/版本号',
    'Leave empty for default identity FluxDown/<version>',
  );
  String get userAgentTaskPlaceholder =>
      _t('留空使用全局设置', 'Leave empty to use global setting');
  String get userAgentPresetDefault =>
      _t('默认（FluxDown）', 'Default (FluxDown)');
  String get userAgentPresetChrome => _t('Chrome', 'Chrome');
  String get userAgentPresetFirefox => _t('Firefox', 'Firefox');
  String get userAgentPresetEdge => _t('Edge', 'Edge');
  String get userAgentPresetSafari => _t('Safari（macOS）', 'Safari (macOS)');
  String get userAgentPresetNetdisk =>
      _t('netdisk（百度网盘）', 'netdisk (Baidu Pan)');
  String get userAgentPresetCustom => _t('自定义', 'Custom');
  List<String> get searchKeywordsUserAgent => _t(
    'UA,用户代理,浏览器标识,netdisk,百度网盘',
    'UA,user agent,browser,netdisk,baidu',
  ).split(',')..addAll(['ua', 'user-agent', 'netdisk']);

  // 文件管理器自定义命令
  String get fileManagerSection => _t('文件管理器', 'File Manager');
  String get revealFileCmdLabel =>
      _t('文件管理器命令', 'File manager command');
  String get revealFileCmdDesc => _t(
    '打开文件/目录时调用的第三方文件管理器命令模板。占位符 {path} = 当前路径（下载完成的文件用完整文件路径，多数管理器会定位并选中该文件；打开目录时为目录路径），{dir} = 目录（文件时为父目录）。留空则使用平台默认（Windows 资源管理器 / Finder / Nautilus 等）。',
    'Third-party file manager command used to open files/folders. Placeholders: {path} = current path (a downloaded file uses its full path — most managers locate and select it; for a directory it is the folder path), {dir} = directory (parent dir for files). Leave empty to use the platform default (Explorer / Finder / Nautilus, etc.).',
  );
  String get revealFileCmdPlaceholder => _t(
    r'例如："C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe" /cmd Go {path} NEW',
    r'e.g. "C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe" /cmd Go {path} NEW',
  );
  List<String> get searchKeywordsFileManager => _t(
    '文件管理器,资源管理器,Explorer,Finder,Nautilus,Total Commander,文件夹',
    'file manager,explorer,finder,nautilus,total commander,folder',
  ).split(',')..addAll(['fm', 'reveal', 'open folder']);

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

  // 任务 Cookie（新建下载对话框高级选项）
  String get taskCookie => _t('Cookie', 'Cookie');
  String get taskCookieDesc => _t(
    '用于需要登录认证的下载（留空则不发送）',
    'Used for downloads requiring login (empty = none)',
  );
  String get taskCookiePlaceholder =>
      _t('例如 name=value; name2=value2', 'e.g. name=value; name2=value2');

  // 任务哈希校验（新建下载对话框高级选项，#247/#248）
  String get taskChecksum => _t('哈希校验', 'Hash Verification');
  String get taskChecksumDesc => _t(
    '下载完成后校验文件完整性（留空则跳过）',
    'Verify file integrity after download (empty = skip)',
  );
  String get taskChecksumPlaceholder => _t('粘贴哈希值', 'Paste the hash value');

  // 任务自定义请求头（新建下载对话框高级选项，#347）
  String get taskHeaders => _t('自定义请求头', 'Custom Headers');
  String get taskHeadersDesc => _t(
    '为此任务附加额外的 HTTP 请求头（Cookie 请用上方独立入口）',
    'Add extra HTTP headers for this task (use the Cookie field above for cookies)',
  );
  String get taskHeadersKeyPlaceholder => _t('请求头名称', 'Header name');
  String get taskHeadersValuePlaceholder => _t('值', 'Value');
  String get taskHeadersAdd => _t('添加请求头', 'Add header');

  // ─────────────────────────────────────────────
  // Settings — API 服务
  // ─────────────────────────────────────────────

  String get apiServiceEnable => _t('启用 API 服务', 'Enable API Service');
  String get apiServiceEnableDesc => _t(
    '启动本机 HTTP API 服务（仅监听 127.0.0.1），供下方各功能模块使用',
    'Start the local HTTP API service (127.0.0.1 only) used by the feature toggles below',
  );

  String get apiServicePort => _t('监听端口', 'Listen Port');
  String get apiServicePortDesc =>
      _t('默认 17800，范围 1024-65535', 'Default 17800, range 1024-65535');
  String get apiServicePortInvalid =>
      _t('端口需在 1024-65535 之间', 'Port must be between 1024 and 65535');

  String get apiServiceToken => _t('访问令牌', 'Access Token');
  String get apiServiceTokenDesc => _t(
    '用于校验 API 请求的访问令牌；可自定义或点「生成」随机生成，启用管理 API 时强制要求设置',
    'Access token for API requests; type your own or click Generate for a random one. Required once the management API is enabled',
  );
  String get apiServiceTokenGenerate => _t('生成', 'Generate');
  String get apiServiceCopy => _t('复制', 'Copy');
  String get apiServiceCopied => _t('已复制', 'Copied');
  String get apiServiceTokenClear => _t('清空', 'Clear');
  String get apiServiceTokenCleared => _t('访问令牌已清空', 'Access token cleared');
  String get apiServiceTokenClearConfirmTitle =>
      _t('清空访问令牌？', 'Clear access token?');
  String get apiServiceTokenClearConfirmDesc => _t(
    '管理 API 已启用并依赖此令牌。清空令牌将同时关闭管理 API 服务。是否继续？',
    'The management API is enabled and depends on this token. Clearing it will also turn off the management API. Continue?',
  );

  String get apiServiceFeaturesTitle => _t('功能开关', 'Feature Toggles');
  String get apiServiceFeaturesDesc => _t(
    '独立控制每个 API 功能模块的开放范围（总开关关闭时整组禁用）',
    'Control which API surface each module exposes (disabled when the master switch is off)',
  );

  String get apiServiceTakeover => _t('浏览器脚本接管', 'Browser Script Takeover');
  String get apiServiceTakeoverDesc => _t(
    '供 FluxDown 油猴脚本接管浏览器下载',
    'Lets the FluxDown userscript take over browser downloads',
  );
  String get apiServiceCopyScript => _t('复制油猴脚本', 'Copy Userscript');
  String get apiServiceScriptCopied => _t(
    '脚本已复制，请在 Tampermonkey 新建脚本粘贴',
    'Script copied; paste it into a new Tampermonkey script',
  );

  String get apiServiceJsonrpc => _t('aria2 RPC 兼容', 'aria2 RPC Compatible');
  String get apiServiceJsonrpcDesc => _t(
    '兼容 aria2 JSON-RPC 协议，供"发送到 aria2"类脚本或 AriaNg 等客户端使用',
    'Implements the aria2 JSON-RPC protocol for "send to aria2" scripts or clients like AriaNg',
  );

  String get apiServiceApi => _t('管理 API', 'Management API');
  String get apiServiceApiDesc => _t(
    '提供任务查询与控制的 HTTP API，供 MCP、自动化脚本等外部程序调用（强制鉴权）',
    'HTTP API for querying and controlling tasks, for MCP servers and automation scripts (authentication required)',
  );

  String get apiServiceMcp => _t('MCP 端点', 'MCP Endpoint');
  String get apiServiceMcpDesc => _t(
    '暴露 Model Context Protocol 端点，供 Claude Desktop、Cursor、Cline 等 AI 客户端接入（与管理 API 共用令牌，强制鉴权）',
    'Exposes a Model Context Protocol endpoint for AI clients like Claude Desktop, Cursor, and Cline (shares the Management API token, authentication required)',
  );

  String get apiServiceAddress => _t('地址', 'Address');

  List<String> get searchKeywordsApiService => _t(
    'api,服务,rpc,油猴,脚本,接管,浏览器,aria2,ariang,令牌,token,端口,mcp,自动化,管理',
    'api,service,rpc,userscript,tampermonkey,capture,browser,aria2,ariang,token,port,mcp,automation,management',
  ).split(',');

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
  String get btTrackerSub => _t('Tracker 订阅', 'Tracker Subscription');
  String get btTrackerSubDesc => _t(
    '定期从社区维护的订阅源获取最新 Tracker，自动与上方列表合并去重',
    'Periodically fetch up-to-date trackers from community-maintained lists, merged and deduplicated with the list above',
  );
  String btTrackerSubStatus(int n) =>
      _t('已订阅 $n 个 Tracker', '$n trackers subscribed');
  String get btTrackerSubNeverUpdated => _t('尚未更新', 'Not updated yet');
  String btTrackerSubUpdatedAt(String time) =>
      _t('更新于 $time', 'Updated at $time');
  String get btTrackerSubUpdateNow => _t('立即更新', 'Update Now');
  String get btTrackerSubUpdating => _t('更新中…', 'Updating…');
  String get btTrackerSubUpdateFailed => _t('更新失败', 'Update failed');
  String get btTrackerSubPlaceholder => _t(
    '每行一个订阅地址，例如：\nhttps://trackerslist.com/best.txt\nhttps://ngosang.github.io/trackerslist/trackers_best.txt',
    'One subscription URL per line, e.g.:\nhttps://trackerslist.com/best.txt\nhttps://ngosang.github.io/trackerslist/trackers_best.txt',
  );
  String get btTrackerSubResetConfirm =>
      _t('确定要恢复默认的订阅地址吗？', 'Reset subscription URLs to defaults?');
  String get btPortInvalid => _t(
    '端口范围无效（1024-65535，起始 ≤ 结束）',
    'Invalid port range (1024-65535, start ≤ end)',
  );

  // ─────────────────────────────────────────────
  // eD2K 服务器设置
  // ─────────────────────────────────────────────
  String get ed2kSettings => _t('eD2K 服务器', 'eD2K Servers');
  String get ed2kSettingsDesc => _t(
    '配置电驴服务器与 server.met 订阅',
    'Configure eD2K servers and server.met subscription',
  );
  String get ed2kServerList => _t('服务器列表', 'Server List');
  String get ed2kServerListDesc => _t(
    '手动填写的电驴服务器（用于找源），与订阅列表自动合并去重',
    'Manually configured eD2K servers (for finding sources), merged and deduplicated with the subscription list',
  );
  String ed2kServerCount(int n) => _t('$n 个服务器', '$n servers');
  String get ed2kResetServers => _t('重置为默认', 'Reset to Default');
  String get ed2kResetServersConfirm =>
      _t('确定要恢复默认的服务器列表吗？', 'Reset server list to defaults?');
  String get ed2kServerPlaceholder => _t(
    '每行一个服务器地址，例如：\n176.123.5.89:4725\n45.82.80.155:5687',
    'One server per line, e.g.:\n176.123.5.89:4725\n45.82.80.155:5687',
  );
  String get ed2kServerSub => _t('服务器订阅', 'Server Subscription');
  String get ed2kServerSubDesc => _t(
    '定期从社区维护的 server.met 获取最新服务器，自动与上方列表合并去重',
    'Periodically fetch up-to-date servers from community-maintained server.met lists, merged and deduplicated with the list above',
  );
  String get ed2kEnableKad => _t('Kad DHT 找源', 'Kad DHT source finding');
  String get ed2kEnableKadDesc => _t(
    '通过 Kad 分布式网络去中心化找源，服务器全挂时仍可找到文件源',
    'Find sources via the decentralized Kad network, works even when all servers are down',
  );
  String get ed2kEnableUpnp => _t('UPnP 端口映射', 'UPnP port mapping');
  String get ed2kEnableUpnpDesc => _t(
    '通过 UPnP 自动映射端口争取 HighID，可连接更多对等端并接收回连',
    'Auto-map ports via UPnP to obtain HighID, connecting to more peers and receiving callbacks',
  );
  String get ed2kListenPort => _t('监听端口', 'Listen port');
  String get ed2kListenPortDesc => _t(
    'eD2K 客户端 TCP/UDP 监听端口（0 = 系统自动选择）',
    'eD2K client TCP/UDP listen port (0 = auto-select)',
  );
  String ed2kServerSubStatus(int n) =>
      _t('已订阅 $n 个服务器', '$n servers subscribed');
  String get ed2kServerSubNeverUpdated => _t('尚未更新', 'Not updated yet');
  String ed2kServerSubUpdatedAt(String time) =>
      _t('更新于 $time', 'Updated at $time');
  String get ed2kServerSubUpdateNow => _t('立即更新', 'Update Now');
  String get ed2kServerSubUpdating => _t('更新中…', 'Updating…');
  String get ed2kServerSubUpdateFailed => _t('更新失败', 'Update failed');
  String get ed2kServerSubPlaceholder => _t(
    '每行一个 server.met 地址，例如：\nhttp://upd.emule-security.org/server.met\nhttps://www.shortypower.org/server.met',
    'One server.met URL per line, e.g.:\nhttp://upd.emule-security.org/server.met\nhttps://www.shortypower.org/server.met',
  );
  String get ed2kServerSubResetConfirm =>
      _t('确定要恢复默认的订阅地址吗？', 'Reset subscription URLs to defaults?');

  // ─────────────────────────────────────────────
  // File picker 错误
  // ─────────────────────────────────────────────

  String get filePickerErrorTimeout => _t(
    '目录选择超时，请重试。\n如果文件选择窗口在后台打开，请先关闭它再重试。',
    'Directory picker timed out, please try again.\nIf a file picker window opened in the background, close it first.',
  );
  String get filePickerErrorNoTool => _t(
    '未找到系统文件选择工具\n请安装 xdg-desktop-portal-gtk（推荐）或 zenity / kdialog：\nsudo pacman -S xdg-desktop-portal-gtk 或 sudo apt install xdg-desktop-portal-gtk',
    'No system file picker found.\nInstall xdg-desktop-portal-gtk (recommended) or zenity / kdialog:\nsudo pacman -S xdg-desktop-portal-gtk or sudo apt install xdg-desktop-portal-gtk',
  );
  String get filePickerErrorNative => _t(
    '无法打开文件选择对话框，请重试。',
    'Failed to open file picker dialog, please try again.',
  );
  String get filePickerErrorGeneric =>
      _t('打开文件选择器失败，请重试。', 'Failed to open file picker, please try again.');
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
  String get donateTitle => _t('支持 FluxDown', 'Support FluxDown');
  String donateDate(int y, int m, int d) => _t(
    '$y 年 $m 月 $d 日',
    '${const [
      'January',
      'February',
      'March',
      'April',
      'May',
      'June',
      'July',
      'August',
      'September',
      'October',
      'November',
      'December',
    ][m - 1]} $d, $y',
  );
  String donateBody(String date, int releases, int commits) => _t(
    '自 $date 首次提交以来，FluxDown 已累计发布 $releases 个版本，提交代码超过 $commits 次。'
        '永久免费、零广告、零追踪的背后，是长期坚持不懈的开发与维护，离不开您的支持与反馈。',
    'Since $date, FluxDown has shipped $releases releases with over $commits commits — '
        'always free, with zero ads and zero tracking. Ongoing development relies on your support and feedback.',
  );
  String get donateThanks => _t(
    '您的支持能让 FluxDown 更好地开发与发展下去，我们会用心做好产品。非常感谢！',
    'Your support helps FluxDown keep improving. Thank you so much!',
  );
  String get donateButton => _t('给开发者捐赠', 'Donate to the Developer');
  String get upToDate => _t('已是最新版本', 'Up to Date');
  String newVersionFound(String v) =>
      _t('发现新版本 v$v', 'New version v$v available');
  String get updateNow => _t('立即更新', 'Update Now');
  String get updateLater => _t('稍后再说', 'Later');
  String get skipThisVersion => _t('跳过此版本', 'Skip This Version');
  String updatePromptBody(String v, String size) => _t(
    '新版本 v$v 已发布（$size）。现在更新？',
    'Version v$v is available ($size). Update now?',
  );
  String get downloadComplete =>
      _t('下载完成，可以安装', 'Download complete, ready to install');
  String get downloadingUpdate => _t('正在下载更新...', 'Downloading update...');
  String segmentsDownloading(int active, int total) =>
      _t('$active/$total 线程并发下载', '$active/$total segments');
  String get checking => _t('检查中...', 'Checking...');
  String get checkUpdate => _t('检查更新', 'Check for Updates');
  String downloadUpdate(String size) => _t('下载更新 ($size)', 'Download ($size)');
  String get recheck => _t('重新检查', 'Recheck');
  String get updateFailedTitle => _t('上次更新失败', 'Previous Update Failed');
  String get updateFailedOpenSite => _t('前往官网下载', 'Download from Website');
  String get officialWebsite => _t('官方网站', 'Official Website');
  String get visitWebsiteForMore =>
      _t('访问官网获取更多信息', 'Visit website for more information');

  // ─────────────────────────────────────────────
  // Settings — 日志导出
  // ─────────────────────────────────────────────

  String get logExport => _t('导出日志', 'Export Logs');
  String get logExportDesc =>
      _t('导出运行日志，方便提交反馈时附带', 'Export logs to attach when submitting feedback');
  String logExportInfo(int count, String size) =>
      _t('$count 个日志文件，共 $size', '$count log file(s), $size total');
  String get logExportButton => _t('导出日志', 'Export Logs');
  String get logOpenDirButton => _t('打开目录', 'Open Folder');
  String logExportSuccess(int count) =>
      _t('已导出 $count 个日志文件', 'Exported $count log file(s)');
  String get logExportEmpty => _t('没有可导出的日志文件', 'No log files to export');
  String get logExportFailed => _t('日志导出失败', 'Failed to export logs');
  String get logSelectExportDir => _t('选择日志导出目录', 'Select Export Directory');
  String get logMaxSize => _t('日志占用上限', 'Max Log Size');
  String get logMaxSizeDesc => _t(
    '日志总大小超出上限时自动从最旧开始清理',
    'Oldest logs are cleaned automatically when total size exceeds the limit',
  );

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
  List<String> get searchKeywordsStartMinimizedToTray =>
      _t('启动,最小化,托盘', 'start,minimized,tray').split(',')
        ..addAll(['startup', 'minimize', 'tray']);
  List<String> get searchKeywordsFloatingBall =>
      _t('悬浮球,悬浮窗,桌面,挂件,速度球', 'floating,ball,widget,overlay,desktop').split(',')
        ..addAll(['floating', 'ball', 'widget', 'overlay']);
  List<String> get searchKeywordsClipboardWatch =>
      _t('剪贴板,监听,链接,复制', 'clipboard,watch,monitor,link,copy').split(',')
        ..addAll(['clipboard', 'watch', 'monitor']);
  List<String> get searchKeywordsLanguage =>
      _t('语言,中文,英文,切换语言', 'language,chinese,english,locale').split(',')
        ..addAll(['language', 'locale', 'lang']);
  List<String> get searchKeywordsThemeMode =>
      _t('主题,亮色,暗色,深色,模式', 'theme,dark,light,mode').split(',')
        ..addAll(['theme', 'dark', 'light']);
  List<String> get searchKeywordsThemeColor =>
      _t('主题色,颜色,配色,色调', 'color,scheme,accent').split(',')
        ..addAll(['color', 'scheme', 'accent']);
  List<String> get searchKeywordsUiScale =>
      _t('缩放,界面,比例,放大,缩小,DPI', 'scale,zoom,interface,size,DPI').split(',')
        ..addAll(['scale', 'zoom', 'size', 'dpi']);
  List<String> get searchKeywordsAppIcon =>
      _t('应用图标,图标,任务栏,托盘,自定义', 'app icon,icon,taskbar,tray,custom').split(',')
        ..addAll(['icon', 'logo', 'taskbar', 'tray']);
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
  List<String> get searchKeywordsNotifyOnComplete => _t(
    '通知,完成,提醒,弹窗,系统通知',
    'notification,complete,alert,popup,system,toast',
  ).split(',')..addAll(['notification', 'complete', 'toast']);
  List<String> get searchKeywordsSilentDownload => _t(
    '免打扰,静默,确认框,弹窗,扩展,自动下载',
    'silent,quiet,confirm,dialog,extension,auto',
  ).split(',')..addAll(['silent', 'confirm', 'dialog']);
  List<String> get searchKeywordsKeepAwake => _t(
    '唤醒,睡眠,息屏,锁屏,休眠,屏幕',
    'awake,sleep,screen,lock,display,wake,caffeinate',
  ).split(',')..addAll(['awake', 'sleep', 'screen', 'wake']);
  List<String> get searchKeywordsBtSettings => _t(
    'BT,BitTorrent,种子,磁力,Tracker,DHT,UPnP,端口',
    'BT,BitTorrent,torrent,magnet,tracker,DHT,UPnP,port',
  ).split(',')..addAll(['bt', 'torrent', 'tracker', 'dht', 'peer']);
  List<String> get searchKeywordsEd2kSettings => _t(
    'eD2K,电驴,eMule,ed2k,服务器,server.met,订阅',
    'eD2K,eMule,ed2k,edonkey,server,server.met,subscription',
  ).split(',')..addAll(['ed2k', 'emule', 'edonkey', 'server']);
  List<String> get searchKeywordsProxy => _t(
    '代理,HTTP,SOCKS,SOCKS5,SOCKS4,网络代理,代理服务器',
    'proxy,HTTP,SOCKS,SOCKS5,SOCKS4,network,server',
  ).split(',')..addAll(['proxy', 'socks', 'http']);
  List<String> get searchKeywordsLogExport => _t(
    '日志,导出,反馈,调试,排查,log',
    'log,export,feedback,debug,diagnostic',
  ).split(',')..addAll(['log', 'export', 'debug']);
  List<String> get searchKeywordsDonate => _t(
    '捐赠,赞助,支持,打赏,donate',
    'donate,sponsor,support',
  ).split(',')..addAll(['donate', 'sponsor', 'support']);
  List<String> get searchKeywordsSidebarVisibility => _t(
    '侧边栏,显示,隐藏,区块,状态,队列,分类',
    'sidebar,show,hide,section,status,queue,category',
  ).split(',');
  List<String> get searchKeywordsTitlebarButtons => _t(
    '标题栏,按钮,显示,隐藏,暂停,恢复,设置,主题',
    'titlebar,button,show,hide,pause,resume,settings,theme',
  ).split(',');
  List<String> get searchKeywordsCustomCategories => _t(
    '自定义,分类,扩展名,正则,文件类型,筛选',
    'custom,category,extension,regex,file type,filter',
  ).split(',');

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
  String get feedbackVersionLabel => _t('应用版本', 'App Version');
  String get feedbackVersionAuto => _t('自动获取', 'Auto-detected');
  String get feedbackSysInfoLabel => _t('附带信息', 'Attached Info');
  String get feedbackSysInfoSystem => _t('系统', 'System');
  String get feedbackSysInfoHint => _t(
    '提交时将附带以上应用版本与系统信息，便于定位问题；不含任何个人敏感数据',
    'The app version and system info above are sent with your feedback to help diagnose issues. No personal or sensitive data is included.',
  );
  String get feedbackAttachLogs => _t('附带今日日志', 'Attach today\'s logs');
  String get feedbackAttachLogsHint => _t(
    '发送时附带当天日志（已脱敏），有助于我们更快定位问题',
    'Include today\'s logs (sanitized) to help us diagnose the issue faster',
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
  // macOS 应用菜单栏
  // ─────────────────────────────────────────────

  String get menuFile => _t('文件', 'File');
  String get menuNewDownload => _t('新建下载…', 'New Download…');
  String get menuCloseWindow => _t('关闭窗口', 'Close Window');
  String get menuEdit => _t('编辑', 'Edit');
  String get menuSelectAll => _t('全选', 'Select All');
  String get menuFind => _t('查找', 'Find');
  String get menuView => _t('视图', 'View');
  String get menuWindow => _t('窗口', 'Window');
  String get menuHelp => _t('帮助', 'Help');
  String get menuCheckForUpdates => _t('检查更新…', 'Check for Updates…');
  String get menuSettings => _t('设置…', 'Settings…');
  String get menuWebsite => _t('FluxDown 官网', 'FluxDown Website');
  String get menuFeedback => _t('发送反馈…', 'Send Feedback…');

  // ─────────────────────────────────────────────
  // DownloadCompleteWindow
  // ─────────────────────────────────────────────

  String get downloadCompleted => _t('下载完成', 'Download Complete');
  String batchDownloadCompleted(int count) =>
      _t('$count 个任务下载完成', '$count Downloads Complete');
  String andMoreFiles(int count) => _t('等 $count 个文件', 'and $count more');
  String get openFileFolder => _t('打开文件夹', 'Open Folder');

  // ─────────────────────────────────────────────
  // 移动端（Mobile）
  // ─────────────────────────────────────────────

  String get mobileNavDownloads => _t('下载', 'Downloads');
  String mobileSpeedSummary(String speed, int n) =>
      _t('↓ $speed · $n 个任务', '↓ $speed · $n task(s)');
  String get mobileIdleSummary => _t('空闲 · 无进行中任务', 'Idle · no active tasks');
  String get mobileSearchHint => _t('搜索任务名称…', 'Search tasks…');
  String get mobileFilterTasks => _t('筛选任务', 'Filter Tasks');
  String get mobileFileType => _t('文件类型', 'File Type');
  String get mobileByQueue => _t('按队列', 'By Queue');
  String get mobileResetFilter => _t('重置筛选', 'Reset Filters');
  String get mobileMoveToQueue => _t('移动到队列…', 'Move to Queue…');
  String get mobileSelectQueue => _t('选择目标队列', 'Choose Target Queue');
  String get mobileMovedToQueue => _t('已移动到队列', 'Moved to queue');
  String get mobilePaste => _t('粘贴', 'Paste');
  String get mobilePasted => _t('已从剪贴板粘贴', 'Pasted from clipboard');
  String get mobileClipboardEmpty => _t('剪贴板为空', 'Clipboard is empty');
  String get mobileSaveTo => _t('保存到', 'Save to');
  String get mobileAdvancedOptions =>
      _t('高级选项（Cookie / User-Agent / 校验）', 'Advanced (Cookie / UA / Checksum)');
  String get mobileEnterUrl => _t('请输入下载链接', 'Enter a download URL');
  String get mobileDownloadStarted => _t('已开始下载', 'Download started');
  String get mobileUrlHint =>
      _t('下载链接（支持多行批量 / magnet / m3u8）', 'URLs (multi-line / magnet / m3u8)');
  String get mobilePausedAllToast => _t('已暂停全部任务', 'All tasks paused');
  String get mobileResumedAllToast => _t('已恢复全部任务', 'All tasks resumed');
  String get mobileTaskDetail => _t('任务详情', 'Task Details');
  String get mobileSegTitle => _t('分段可视化', 'Segments');
  String get mobileSegRunning => _t('动态拆分运行中', 'dynamic split active');
  String get mobileSegStopped => _t('已停止', 'stopped');
  String get mobileSegDone => _t('已完成', 'Done');
  String get mobileSegActive => _t('下载中', 'Active');
  String get mobileSegPending => _t('待下载', 'Pending');
  String get mobileSpeedCurve => _t('速度曲线', 'Speed Curve');
  String get mobileSpeedWindow => _t('近 60 秒', 'Last 60s');
  String mobileSpeedPeak(String v) => _t('峰值 $v', 'Peak $v');
  String get mobileTaskInfo => _t('任务信息', 'Task Info');
  String get mobileProtocol => _t('协议', 'Protocol');
  String get mobileCreatedAt => _t('创建时间', 'Created');
  String get mobileBoostAction => _t('Boost 优先下载', 'Boost Priority');
  String get mobileBoosted => _t('已 Boost', 'Boosted');
  String get mobileBoostOn => _t('已启用 Boost 优先下载', 'Boost enabled');
  String get mobileBoostOff => _t('已取消 Boost', 'Boost cancelled');
  String get mobileRetry => _t('重试', 'Retry');
  String get mobileTaskDeleted => _t('已删除任务', 'Task deleted');
  String get mobileTaskFileDeleted => _t('已删除任务和文件', 'Task & file deleted');
  String get mobilePrivacyPolicy => _t('隐私政策', 'Privacy Policy');
  String get mobileOpenSource => _t('开源许可', 'Open Source Licenses');
  String get mobileFooter =>
      _t('FluxDown · 零广告 · 零追踪 · 本地优先', 'FluxDown · No ads · No tracking · Local-first');
  String get mobilePickDirUnmappable => _t(
    '无法解析所选目录，请选择设备存储中的文件夹',
    "Couldn't resolve the folder; pick one on device storage",
  );
  String get mobileAllFilesTitle =>
      _t('需要「所有文件访问」权限', '"All files access" required');
  String get mobileAllFilesDesc => _t(
    '下载到公共目录（如 Download）需要授予「所有文件访问」权限，点击「去授权」前往系统设置开启。',
    'Saving to public folders (e.g. Download) requires the "All files access" permission. Tap "Grant" to enable it in system settings.',
  );
  String get mobileGoGrant => _t('去授权', 'Grant');

  // ─────────────────────────────────────────────
  // 前台服务（移动端后台下载通知）
  // ─────────────────────────────────────────────

  String get fgServiceChannelName => _t('后台下载', 'Background Download');
  String get fgServiceChannelDesc => _t(
    '应用切换到后台时保持下载继续运行。',
    'Keeps downloads running while the app is in the background.',
  );
  String fgServiceActiveTitle(int count) {
    final unit = count == 1 ? 'task' : 'tasks';
    return _t('正在下载 $count 个任务', 'Downloading $count $unit');
  }

  String fgServiceActiveText(String speed) => _t('速度 $speed', 'Speed $speed');
  String get fgServiceIdleTitle => _t('FluxDown 正在运行', 'FluxDown is running');
  String get fgServiceIdleText => _t('后台待命中', 'Standing by in the background');
}
