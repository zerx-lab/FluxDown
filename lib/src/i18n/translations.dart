// FluxDown i18n — UI 字符串统一经 I18nStore 查表（assets/i18n/<locale>.json）。
//
// 使用方法:
//   final s = S.of(context);
//   Text(s.newDownload)
//
// 翻译源文件: assets/i18n/en.json（源语言，Weblate 模板）；
// 其他语言由社区经 Weblate 贡献，新增 <locale>.json 资产后运行时自动发现。
// 参数化字符串使用 {name} 占位符，经 _r(key, args) 插值。
// 本文件的成员签名是全部调用点的契约——新增字符串时在 en.json/zh.json
// 添加同名键并在此添加对应 getter/方法。

import 'i18n_store.dart';

class S {
  final String locale;

  const S._(this.locale);

  // ─────────────────────────────────────────────
  // 工厂方法
  // ─────────────────────────────────────────────

  static S of(String locale) => S._(I18nStore.resolve(locale));

  /// 查表 + {name} 占位插值；键级回退英文，再回退键名。
  String _r(String key, [Map<String, Object?>? args]) =>
      I18nStore.lookup(locale, key, args);

  // ─────────────────────────────────────────────
  // 通用
  // ─────────────────────────────────────────────
  String get cancel => _r('cancel');
  String get confirm => _r('confirm');
  String get close => _r('close');
  String get back => _r('back');
  String get settings => _r('settings');
  String get browse => _r('browse');
  String get manage => _r('manage');
  String get manageTooltip => 'Ctrl+A';
  String get auto => _r('auto');

  // ─────────────────────────────────────────────
  // 文件分类
  // ─────────────────────────────────────────────
  String get categoryAll => _r('categoryAll');
  String get categoryVideo => _r('categoryVideo');
  String get categoryAudio => _r('categoryAudio');
  String get categoryDocument => _r('categoryDocument');
  String get categoryImage => _r('categoryImage');
  String get categoryProgram => _r('categoryProgram');
  String get categoryArchive => _r('categoryArchive');
  String get categoryOther => _r('categoryOther');

  // ─────────────────────────────────────────────
  // 时间分组
  // ─────────────────────────────────────────────
  String get today => _r('today');
  String get yesterday => _r('yesterday');
  String get thisWeek => _r('thisWeek');
  String get thisMonth => _r('thisMonth');
  String get older => _r('older');

  // ─────────────────────────────────────────────
  // 任务状态
  // ─────────────────────────────────────────────
  String get statusPending => _r('statusPending');
  String get statusDownloading => _r('statusDownloading');
  String get statusPaused => _r('statusPaused');
  String get statusCompleted => _r('statusCompleted');
  String get statusError => _r('statusError');
  String get statusPreparing => _r('statusPreparing');
  String get statusResuming => _r('statusResuming');
  String get statusFileMissing => _r('statusFileMissing');
  String get pluginProcessing => _r('pluginProcessing');

  // ─────────────────────────────────────────────
  // 任务副标题
  // ─────────────────────────────────────────────
  String get subtitlePaused => _r('subtitlePaused');
  String get subtitleError => _r('subtitleError');
  String get subtitlePending => _r('subtitlePending');
  String get subtitlePreparing => _r('subtitlePreparing');
  String get subtitleResuming => _r('subtitleResuming');
  String get unknownSize => _r('unknownSize');
  String get unknownFile => _r('unknownFile');
  String subtitleQueued(int pos) => _r('subtitleQueued', {'pos': pos});

  // ─────────────────────────────────────────────
  // 时间单位
  // ─────────────────────────────────────────────
  String etaSeconds(int n) => _r('etaSeconds', {'n': n});
  String etaMinutes(int n) => _r('etaMinutes', {'n': n});
  String etaHours(String n) => _r('etaHours', {'n': n});

  // ─────────────────────────────────────────────
  // Sidebar
  // ─────────────────────────────────────────────
  String get sidebarStatus => _r('sidebarStatus');
  String get sidebarQueues => _r('sidebarQueues');
  String get sidebarCategory => _r('sidebarCategory');
  String get defaultQueue => _r('defaultQueue');
  String get createQueueAction => _r('createQueueAction');
  String get editQueue => _r('editQueue');
  String get deleteQueueAction => _r('deleteQueueAction');
  String get queueNameLabel => _r('queueNameLabel');
  String get queueNameHint => _r('queueNameHint');
  String get queueSpeedLimit => _r('queueSpeedLimit');
  String get queueSpeedLimitHint => _r('queueSpeedLimitHint');
  String get queueMaxConcurrent => _r('queueMaxConcurrent');
  String get queueMaxConcurrentHint => _r('queueMaxConcurrentHint');
  String get queueDefaultSegments => _r('queueDefaultSegments');
  String get queueDefaultSegmentsHint => _r('queueDefaultSegmentsHint');
  String get queueSaveDir => _r('queueSaveDir');
  String get queueDefaultUserAgent => _r('queueDefaultUserAgent');
  String get queueUaInheritGlobal => _r('queueUaInheritGlobal');
  String get queueUaHint => _r('queueUaHint');
  String queueDeleteConfirmDesc(String name) =>
      _r('queueDeleteConfirmDesc', {'name': name});
  String get taskQueueLabel => _r('taskQueueLabel');
  String get defaultQueueSetting => _r('defaultQueueSetting');
  String get defaultQueueSettingDesc => _r('defaultQueueSettingDesc');
  String get mainQueue => _r('mainQueue');
  String get laterQueue => _r('laterQueue');
  String get ungroupedTasks => _r('ungroupedTasks');
  String get startQueueAction => _r('startQueueAction');
  String get stopQueueAction => _r('stopQueueAction');
  String get queueRunningBadge => _r('queueRunningBadge');
  String get queueStoppedBadge => _r('queueStoppedBadge');
  String get manageQueueAction => _r('manageQueueAction');
  String get downloadLater => _r('downloadLater');
  String startIntoQueueTooltip(String name) =>
      _r('startIntoQueueTooltip', {'name': name});
  String laterIntoQueueTooltip(String name) =>
      _r('laterIntoQueueTooltip', {'name': name});
  String get subtitleWaitingQueue => _r('subtitleWaitingQueue');
  String get queueTabSettings => _r('queueTabSettings');
  String get queueTabSchedule => _r('queueTabSchedule');
  String get queueTabTasks => _r('queueTabTasks');
  String get queueScheduleEnable => _r('queueScheduleEnable');
  String get queueScheduleDesc => _r('queueScheduleDesc');
  String get queueScheduleStartLabel => _r('queueScheduleStartLabel');
  String get queueScheduleStopLabel => _r('queueScheduleStopLabel');
  String get queueScheduleTimeHint => _r('queueScheduleTimeHint');
  String get queueScheduleTimeInvalid => _r('queueScheduleTimeInvalid');
  String get queueScheduleDays => _r('queueScheduleDays');
  String get weekdaysShort => _r('weekdaysShort');
  String get queueTasksOrderHint => _r('queueTasksOrderHint');
  String scheduleSummaryBoth(String start, String stop) =>
      _r('scheduleSummaryBoth', {'start': start, 'stop': stop});
  String scheduleSummaryStartOnly(String start) =>
      _r('scheduleSummaryStartOnly', {'start': start});
  String scheduleSummaryStopOnly(String stop) =>
      _r('scheduleSummaryStopOnly', {'stop': stop});
  String get scheduleNeedOneTime => _r('scheduleNeedOneTime');
  String get scheduleHourLabel => _r('scheduleHourLabel');
  String get scheduleMinuteLabel => _r('scheduleMinuteLabel');
  String get queueNoPendingTasks => _r('queueNoPendingTasks');
  String get moveUpAction => _r('moveUpAction');
  String get moveDownAction => _r('moveDownAction');
  String get builtinQueueRenameHint => _r('builtinQueueRenameHint');
  String get queueDirInheritHint => _r('queueDirInheritHint');
  String get moveToQueueAction => _r('moveToQueueAction');
  String downloadUpdateVersion(String v) =>
      _r('downloadUpdateVersion', {'v': v});
  String get installAndRestart => _r('installAndRestart');

  // ─────────────────────────────────────────────
  // HeaderBar
  // ─────────────────────────────────────────────
  String get newDownload => _r('newDownload');
  String get searchPlaceholder => _r('searchPlaceholder');
  String get searchGroupTasks => _r('searchGroupTasks');
  String get searchGroupSettings => _r('searchGroupSettings');
  String settingsSearchSubtitle(String catLabel, String desc) =>
      _r('settingsSearchSubtitle', {'catLabel': catLabel, 'desc': desc});
  String get settingsSearchHint => _r('settingsSearchHint');
  String get settingsSearchNoResults => _r('settingsSearchNoResults');
  String get pauseAll => _r('pauseAll');
  String get resumeAll => _r('resumeAll');
  String get toggleToLight => _r('toggleToLight');
  String get toggleToDark => _r('toggleToDark');

  // ─────────────────────────────────────────────
  // TaskTabBar
  // ─────────────────────────────────────────────
  String get tabAll => _r('tabAll');
  String get tabDownloading => _r('tabDownloading');
  String get tabCompleted => _r('tabCompleted');
  String get tabPaused => _r('tabPaused');
  String get tabError => _r('tabError');
  String get selectAll => _r('selectAll');
  String get deselectAll => _r('deselectAll');
  String selectedCount(int n) => _r('selectedCount', {'n': n});
  String get deleteTask => _r('deleteTask');
  String get deleteTaskAndFile => _r('deleteTaskAndFile');

  // ─────────────────────────────────────────────
  // TaskList
  // ─────────────────────────────────────────────
  String get startAll => _r('startAll');
  String get activeGroupLabel => _r('activeGroupLabel');
  String get emptyTitle => _r('emptyTitle');
  String get emptySubtitle => _r('emptySubtitle');
  String get colFileName => _r('colFileName');
  String get colProgress => _r('colProgress');
  String get colSpeed => _r('colSpeed');
  String get colEta => _r('colEta');
  String get colStatus => _r('colStatus');
  String get colSize => _r('colSize');
  String get colCreated => _r('colCreated');
  String get colProtocol => _r('colProtocol');
  String get colSource => _r('colSource');
  String get colQueue => _r('colQueue');

  // ─────────────────────────────────────────────
  // 视图系统（任务列表多形态展现）
  // ─────────────────────────────────────────────
  String get viewOptionsTitle => _r('viewOptionsTitle');
  String viewEntryTooltip(String state) =>
      _r('viewEntryTooltip', {'state': state});
  String get viewSectionForm => _r('viewSectionForm');
  String get viewSectionDensity => _r('viewSectionDensity');
  String get viewSectionGroupBy => _r('viewSectionGroupBy');
  String get viewSectionSort => _r('viewSectionSort');
  String get viewSectionDisplay => _r('viewSectionDisplay');
  String get viewSectionColumns => _r('viewSectionColumns');
  String get viewFormList => _r('viewFormList');
  String get viewFormGrid => _r('viewFormGrid');
  String get viewDensityComfortable => _r('viewDensityComfortable');
  String get viewDensityCompact => _r('viewDensityCompact');
  String get viewDensityGridDisabledHint => _r('viewDensityGridDisabledHint');
  String get viewGroupSmart => _r('viewGroupSmart');
  String get viewGroupDate => _r('viewGroupDate');
  String get viewGroupStatus => _r('viewGroupStatus');
  String get viewGroupType => _r('viewGroupType');
  String get viewGroupQueue => _r('viewGroupQueue');
  String get viewGroupSite => _r('viewGroupSite');
  String get viewGroupNone => _r('viewGroupNone');
  String get viewSortSmart => _r('viewSortSmart');
  String get viewSortCreated => _r('viewSortCreated');
  String get viewSortName => _r('viewSortName');
  String get viewSortSize => _r('viewSortSize');
  String get viewSortProgress => _r('viewSortProgress');
  String get viewSortSpeed => _r('viewSortSpeed');
  String get viewShowCompleted => _r('viewShowCompleted');
  String get viewProtocolBadges => _r('viewProtocolBadges');
  String get viewResetDefault => _r('viewResetDefault');
  String get viewResetHint => _r('viewResetHint');
  String get viewResetToast => _r('viewResetToast');
  String get viewColumnsAtLeastOne => _r('viewColumnsAtLeastOne');
  String get viewColumnsBudgetExceeded => _r('viewColumnsBudgetExceeded');
  String get viewColumnHintCompactRow => _r('viewColumnHintCompactRow');
  String get viewColumnsMenuTitle => _r('viewColumnsMenuTitle');
  String get viewColumnsResetAction => _r('viewColumnsResetAction');
  String get viewBucketRetryAll => _r('viewBucketRetryAll');
  String get viewSiteBt => _r('viewSiteBt');
  String statusScopeSummary(int count, String size) =>
      _r('statusScopeSummary', {'count': count, 'size': size});
  String statusScopeHidden(int count) =>
      _r('statusScopeHidden', {'count': count});
  String statusViewGroupedByLabel(String dim) =>
      _r('statusViewGroupedByLabel', {'dim': dim});
  String statusViewSortedByLabel(String key) =>
      _r('statusViewSortedByLabel', {'key': key});

  // ─────────────────────────────────────────────
  // TaskListItem (右键菜单)
  // ─────────────────────────────────────────────
  String get pause => _r('pause');
  String get resume => _r('resume');
  String get boostDownload => _r('boostDownload');
  String get cancelBoost => _r('cancelBoost');
  String boostBannerActive(String fileName, int count) =>
      _r('boostBannerActive', {'fileName': fileName, 'count': count});
  String get boostBannerCancel => _r('boostBannerCancel');
  String get openFile => _r('openFile');
  String get openFolder => _r('openFolder');
  String get copyUrl => _r('copyUrl');
  String get urlCopied => _r('urlCopied');
  String get errorCopied => _r('errorCopied');

  // ─────────────────────────────────────────────
  // 删除确认对话框
  // ─────────────────────────────────────────────
  String deleteConfirmTitle(bool deleteFiles) =>
      deleteFiles ? deleteTaskAndFile : deleteTask;
  String deleteConfirmDesc(String fileName, bool deleteFiles) => deleteFiles
      ? _r('deleteConfirmDescWithFile', {'fileName': fileName})
      : _r('deleteConfirmDescKeepFile', {'fileName': fileName});
  String get batchDeleteTask => _r('batchDeleteTask');
  String get batchDeleteTaskAndFile => _r('batchDeleteTaskAndFile');
  String batchDeleteConfirmTitle(bool deleteFiles) =>
      deleteFiles ? batchDeleteTaskAndFile : batchDeleteTask;
  String batchDeleteConfirmDesc(int count, bool deleteFiles) => deleteFiles
      ? _r('batchDeleteConfirmDescWithFile', {'count': count})
      : _r('batchDeleteConfirmDescKeepFile', {'count': count});
  String get batchDeletingTitle => _r('batchDeletingTitle');
  String batchDeletingProgress(int done, int total) =>
      _r('batchDeletingProgress', {'done': done, 'total': total});

  // ─────────────────────────────────────────────
  // DetailPanel
  // ─────────────────────────────────────────────
  String get detail => _r('detail');
  String get selectTaskHint => _r('selectTaskHint');
  String get downloadDistribution => _r('downloadDistribution');
  String get infoSize => _r('infoSize');
  String get infoDownloaded => _r('infoDownloaded');
  String get infoSpeed => _r('infoSpeed');
  String get infoRemaining => _r('infoRemaining');
  String get infoStatus => _r('infoStatus');
  String get infoStartedAt => _r('infoStartedAt');
  String get infoCompletedAt => _r('infoCompletedAt');
  String get infoDuration => _r('infoDuration');
  String infoThreads(int n) => _r('infoThreads', {'n': n});
  String get infoPath => _r('infoPath');
  String get infoError => _r('infoError');
  String get infoUrl => _r('infoUrl');
  String get infoSourcePage => _r('infoSourcePage');
  String get resumingClickPause => _r('resumingClickPause');
  String get dynamicSplit => _r('dynamicSplit');
  String splitCount(int total, int reactive, int proactive) => _r(
    'splitCount',
    {'total': total, 'proactive': proactive, 'reactive': reactive},
  );
  String splitLatest(int parentNum, int childNum, String childSize) => _r(
    'splitLatest',
    {'parentNum': parentNum, 'childNum': childNum, 'childSize': childSize},
  );
  String get detailTabGeneral => _r('detailTabGeneral');
  String get detailTabQueue => _r('detailTabQueue');
  String get detailTabLog => _r('detailTabLog');
  String get detailTabAdvanced => _r('detailTabAdvanced');
  String get detailBoostActive => _r('detailBoostActive');
  String get infoProtocolSource => _r('infoProtocolSource');
  String detailSegsSummary(int total, int active) =>
      _r('detailSegsSummary', {'total': total, 'active': active});
  String get detailQueueMoveHint => _r('detailQueueMoveHint');
  String detailQueueMovedToast(String name) =>
      _r('detailQueueMovedToast', {'name': name});
  String get detailLogHint => _r('detailLogHint');
  String get detailLogEmpty => _r('detailLogEmpty');
  String get detailLogCreated => _r('detailLogCreated');
  String detailLogSplit(
    int parentNum,
    int childNum,
    String size,
    String kind,
  ) => _r('detailLogSplit', {
    'parentNum': parentNum,
    'childNum': childNum,
    'size': size,
    'kind': kind,
  });
  String get detailSplitProactive => _r('detailSplitProactive');
  String get detailSplitReactive => _r('detailSplitReactive');
  String get detailLogCompleted => _r('detailLogCompleted');
  String detailLogFailed(String message) =>
      _r('detailLogFailed', {'message': message});
  String get detailNotSet => _r('detailNotSet');
  String get detailFollowGlobal => _r('detailFollowGlobal');
  String get detailActionFolder => _r('detailActionFolder');
  String get detailActionCopyLink => _r('detailActionCopyLink');

  // ─────────────────────────────────────────────
  // NewDownloadDialog / QuickDownloadDialog
  // ─────────────────────────────────────────────
  String get addDownloadTask => _r('addDownloadTask');
  String get startDownload => _r('startDownload');
  String get downloadUrl => _r('downloadUrl');
  String get urlPlaceholder => _r('urlPlaceholder');
  String get saveDir => _r('saveDir');
  String get selectSaveDir => _r('selectSaveDir');
  String get threads => _r('threads');
  String get customThreads => _r('customThreads');
  String get customThreadsHint => _r('customThreadsHint');
  String customRangeHint(int min, int max) =>
      _r('customRangeHint', {'min': min, 'max': max});
  String get threadsInvalidRange => _r('threadsInvalidRange');
  String get editThreads => _r('editThreads');
  String get editThreadsTitle => _r('editThreadsTitle');
  String get editThreadsResetHint => _r('editThreadsResetHint');
  String get threadsChanged => _r('threadsChanged');
  String get threadsChangeRejected => _r('threadsChangeRejected');
  String get configuredThreads => _r('configuredThreads');
  String get activeSegments => _r('activeSegments');
  String get renameOptional => _r('renameOptional');
  String get autoDetectFilename => _r('autoDetectFilename');
  String get filenameOptional => _r('filenameOptional');
  String get fromBrowserExtension => _r('fromBrowserExtension');

  // Torrent file
  String get selectTorrentFile => _r('selectTorrentFile');
  String get openTorrentFile => _r('openTorrentFile');
  String get torrentFileSelected => _r('torrentFileSelected');
  String torrentFileCount(int count) =>
      _r('torrentFileCount', {'count': count});
  String get orSeparator => _r('orSeparator');

  // Batch download
  String get batchDownloadDesc => _r('batchDownloadDesc');
  String get batchUrls => _r('batchUrls');
  String get batchUrlPlaceholder => _r('batchUrlPlaceholder');
  String urlCount(int count) => _r('urlCount', {'count': count});
  String startBatchDownload(int count) =>
      _r('startBatchDownload', {'count': count});
  String get importTxtFile => _r('importTxtFile');
  String get importTxtNoUrls => _r('importTxtNoUrls');
  String importTxtFound(int count) => _r('importTxtFound', {'count': count});

  // ─────────────────────────────────────────────
  // BT File Selection Dialog
  // ─────────────────────────────────────────────
  String get btFileSelectTitle => _r('btFileSelectTitle');
  String get btFileSelectDescSingle => _r('btFileSelectDescSingle');
  String btFileSelectDesc(int count) =>
      _r('btFileSelectDesc', {'count': count});
  String get btFileSelectAll => _r('btFileSelectAll');
  String get btFileTreeView => _r('btFileTreeView');
  String get btFileListView => _r('btFileListView');
  String btFileSelectConfirm(int count, String size) =>
      _r('btFileSelectConfirm', {'count': count, 'size': size});
  String get btResolvingMagnet => _r('btResolvingMagnet');
  String get btResolveFailed => _r('btResolveFailed');
  String get btWaitingFiles => _r('btWaitingFiles');
  String get btProbing => _r('btProbing');
  String get btProbeError => _r('btProbeError');
  String btStartWithSelection(int count, String size) =>
      _r('btStartWithSelection', {'count': count, 'size': size});

  // ─────────────────────────────────────────────
  // StatusBar
  // ─────────────────────────────────────────────
  String get statusDownloadingLabel => _r('statusDownloadingLabel');
  String get statusIdle => _r('statusIdle');
  String statusSummary(int active, int paused, int total) =>
      _r('statusSummary', {'active': active, 'paused': paused, 'total': total});
  String get statusSpeedLimitOff => _r('statusSpeedLimitOff');
  String get statusSpeedLimitKbs => _r('statusSpeedLimitKbs');
  String get statusSpeedLimitHint => _r('statusSpeedLimitHint');
  String get speedLimitTitle => _r('speedLimitTitle');
  String get speedLimitCustom => _r('speedLimitCustom');
  String get shutdownTriggerLabel => _r('shutdownTriggerLabel');
  String get shutdownTitle => _r('shutdownTitle');
  String get shutdownNeedActiveTask => _r('shutdownNeedActiveTask');
  String get shutdownDelayLabel => _r('shutdownDelayLabel');
  String get shutdownMinutesUnit => _r('shutdownMinutesUnit');
  String shutdownDelayMinutes(int m) => _r('shutdownDelayMinutes', {'m': m});
  String get shutdownImmediate => _r('shutdownImmediate');
  String get shutdownArmedHintImmediate => _r('shutdownArmedHintImmediate');
  String shutdownArmedHint(int m) => _r('shutdownArmedHint', {'m': m});
  String shutdownCountdown(String time) =>
      _r('shutdownCountdown', {'time': time});
  String get shutdownCancelButton => _r('shutdownCancelButton');

  // ─────────────────────────────────────────────
  // Settings — 分类
  // ─────────────────────────────────────────────
  String get settingsCatGeneral => _r('settingsCatGeneral');
  String get settingsCatGeneralDesc => _r('settingsCatGeneralDesc');
  String get settingsCatAppearance => _r('settingsCatAppearance');
  String get settingsCatAppearanceDesc => _r('settingsCatAppearanceDesc');
  String get settingsCatDownload => _r('settingsCatDownload');
  String get settingsCatDownloadDesc => _r('settingsCatDownloadDesc');
  String get settingsCatBt => _r('settingsCatBt');
  String get settingsCatBtDesc => _r('settingsCatBtDesc');
  String get settingsCatEd2k => _r('settingsCatEd2k');
  String get settingsCatEd2kDesc => _r('settingsCatEd2kDesc');
  String get settingsCatProxy => _r('settingsCatProxy');
  String get settingsCatProxyDesc => _r('settingsCatProxyDesc');
  String get settingsCatApiService => _r('settingsCatApiService');
  String get settingsCatApiServiceDesc => _r('settingsCatApiServiceDesc');
  String get settingsCatAbout => _r('settingsCatAbout');
  String get settingsCatAboutDesc => _r('settingsCatAboutDesc');
  String get settingsCatAccount => _r('settingsCatAccount');
  String get settingsCatAccountDesc => _r('settingsCatAccountDesc');

  // 账户 —— FluxCloud 登录/注册/设备管理
  String get accountHeroSubtitle => _r('accountHeroSubtitle');
  String get accountLogin => _r('accountLogin');
  String get accountLogout => _r('accountLogout');
  String get accountRegister => _r('accountRegister');
  String get accountOriginIdCopied => _r('accountOriginIdCopied');
  String get accountSecurityGroup => _r('accountSecurityGroup');
  String get accountSecurityGroupDesc => _r('accountSecurityGroupDesc');
  String get accountGroupCloudFeatures => _r('accountGroupCloudFeatures');
  String get accountCloudFeaturesDesc => _r('accountCloudFeaturesDesc');
  String get accountComingSoon => _r('accountComingSoon');
  String get accountFeatureConfigSync => _r('accountFeatureConfigSync');
  String get accountFeatureConfigSyncDesc => _r('accountFeatureConfigSyncDesc');
  String get accountFeatureMultiDevice => _r('accountFeatureMultiDevice');
  String get accountFeatureMultiDeviceDesc =>
      _r('accountFeatureMultiDeviceDesc');
  String get accountLoginDialogTitle => _r('accountLoginDialogTitle');
  String get accountLoginTabCode => _r('accountLoginTabCode');
  String get accountLoginTabPassword => _r('accountLoginTabPassword');
  String get accountEmailPlaceholder => _r('accountEmailPlaceholder');
  String get accountEmailChangeTitle => _r('accountEmailChangeTitle');
  String accountEmailChangeOldSubtitle(String email) =>
      _r('accountEmailChangeOldSubtitle', {'email': email});
  String get accountEmailChangeOldCodePlaceholder =>
      _r('accountEmailChangeOldCodePlaceholder');
  String get accountEmailChangeOldCodeHint =>
      _r('accountEmailChangeOldCodeHint');
  String get accountEmailChangeSendNewCode =>
      _r('accountEmailChangeSendNewCode');
  String get accountEmailChangeNewPlaceholder =>
      _r('accountEmailChangeNewPlaceholder');
  String accountEmailChangeCodeSubtitle(String email) =>
      _r('accountEmailChangeCodeSubtitle', {'email': email});
  String get accountEmailChangeInvalid => _r('accountEmailChangeInvalid');
  String get accountEmailChangeSame => _r('accountEmailChangeSame');
  String get accountEmailChangeSuccess => _r('accountEmailChangeSuccess');
  String get accountLoginAccountPlaceholder =>
      _r('accountLoginAccountPlaceholder');
  String get accountCodePlaceholder => _r('accountCodePlaceholder');
  String get accountSendCode => _r('accountSendCode');
  String get accountPasswordPlaceholder => _r('accountPasswordPlaceholder');
  String get accountLoginTerms => _r('accountLoginTerms');
  String get accountNoAccountYet => _r('accountNoAccountYet');
  String get accountAlreadyHaveAccount => _r('accountAlreadyHaveAccount');
  String get accountRegisterDialogTitle => _r('accountRegisterDialogTitle');
  String get accountNicknamePlaceholder => _r('accountNicknamePlaceholder');
  String get accountNicknameReroll => _r('accountNicknameReroll');
  String get accountPasswordHint => _r('accountPasswordHint');
  String get accountDeviceVerifyTitle => _r('accountDeviceVerifyTitle');
  String accountDeviceVerifySubtitle(String email) =>
      _r('accountDeviceVerifySubtitle', {'email': email});
  String get accountDeviceVerifySubtitleGeneric =>
      _r('accountDeviceVerifySubtitleGeneric');
  String get accountRegisterVerifyTitle => _r('accountRegisterVerifyTitle');
  String accountRegisterVerifySubtitle(String email) =>
      _r('accountRegisterVerifySubtitle', {'email': email});
  String accountCodeExpireIn(int seconds) =>
      _r('accountCodeExpireIn', {'seconds': seconds});
  String get accountResendCode => _r('accountResendCode');
  String accountResendCodeIn(int seconds) =>
      _r('accountResendCodeIn', {'seconds': seconds});
  String get accountVerifySubmit => _r('accountVerifySubmit');
  String get accountErrorInvalidCredentials =>
      _r('accountErrorInvalidCredentials');
  String get accountErrorInvalidCode => _r('accountErrorInvalidCode');
  String get accountErrorRateLimited => _r('accountErrorRateLimited');
  String get accountErrorEmailTaken => _r('accountErrorEmailTaken');
  String get accountErrorAccountDisabled => _r('accountErrorAccountDisabled');
  String get accountErrorRegistrationClosed =>
      _r('accountErrorRegistrationClosed');
  String get accountErrorRegistrationIncomplete =>
      _r('accountErrorRegistrationIncomplete');
  String get accountErrorValidation => _r('accountErrorValidation');
  String get accountErrorNetwork => _r('accountErrorNetwork');
  String get accountErrorUnknown => _r('accountErrorUnknown');
  String get accountDeviceRenameTitle => _r('accountDeviceRenameTitle');
  String get accountDeviceRenameInvalid => _r('accountDeviceRenameInvalid');
  String get accountDeviceDeleteConfirmTitle =>
      _r('accountDeviceDeleteConfirmTitle');
  String get accountDeviceDeleteConfirmDesc =>
      _r('accountDeviceDeleteConfirmDesc');
  String get accountDeviceDeleteCurrentWarning =>
      _r('accountDeviceDeleteCurrentWarning');
  String get accountDeviceCurrent => _r('accountDeviceCurrent');
  String get accountDevicesTitle => _r('accountDevicesTitle');
  String get accountDevicesDesc => _r('accountDevicesDesc');
  String get accountDevicesEmpty => _r('accountDevicesEmpty');
  String get accountDevicesLoadFailed => _r('accountDevicesLoadFailed');
  String get accountDevicesRetry => _r('accountDevicesRetry');
  String accountDevicesManageAll(int count) =>
      _r('accountDevicesManageAll', {'count': count});
  String get accountDevicesManageAllTitle => _r('accountDevicesManageAllTitle');
  String get accountDevicesSearchHint => _r('accountDevicesSearchHint');
  String get accountDevicesSearchNoResults =>
      _r('accountDevicesSearchNoResults');
  String get accountDeviceDetailTitle => _r('accountDeviceDetailTitle');
  String get accountDeviceFieldPlatform => _r('accountDeviceFieldPlatform');
  String get accountDeviceFieldAppVersion => _r('accountDeviceFieldAppVersion');
  String get accountDeviceFieldLastIp => _r('accountDeviceFieldLastIp');
  String get accountDeviceFieldCreatedAt => _r('accountDeviceFieldCreatedAt');
  String get accountDeviceFieldLastSeenAt => _r('accountDeviceFieldLastSeenAt');
  String get accountDeviceFieldId => _r('accountDeviceFieldId');
  String get accountDeviceDeleteAction => _r('accountDeviceDeleteAction');
  String get accountDevicePlatformWindows => _r('accountDevicePlatformWindows');
  String get accountDevicePlatformMacos => _r('accountDevicePlatformMacos');
  String get accountDevicePlatformLinux => _r('accountDevicePlatformLinux');
  String get accountDevicePlatformAndroid => _r('accountDevicePlatformAndroid');
  String get accountDevicePlatformIos => _r('accountDevicePlatformIos');
  String get accountDevicePlatformWeb => _r('accountDevicePlatformWeb');
  String get accountServerAddress => _r('accountServerAddress');
  String get accountServerAddressDesc => _r('accountServerAddressDesc');
  String get accountServerAddressInvalid => _r('accountServerAddressInvalid');
  String get accountServerAddressReset => _r('accountServerAddressReset');
  String get accountServerAddressSaved => _r('accountServerAddressSaved');

  // 配置同步 —— FluxCloud 云端设置同步（见 local://sync-contract.md）
  String get cloudSyncTitle => _r('cloudSyncTitle');
  String get cloudSyncDesc => _r('cloudSyncDesc');
  String get cloudSyncStatusDisabled => _r('cloudSyncStatusDisabled');
  String get cloudSyncStatusConnecting => _r('cloudSyncStatusConnecting');
  String get cloudSyncStatusSyncing => _r('cloudSyncStatusSyncing');
  String get cloudSyncStatusSynced => _r('cloudSyncStatusSynced');
  String cloudSyncStatusError(String reason) =>
      _r('cloudSyncStatusError', {'reason': reason});
  String get cloudSyncNow => _r('cloudSyncNow');
  String get cloudSyncLoginRequired => _r('cloudSyncLoginRequired');
  String get cloudSyncOtherDevice => _r('cloudSyncOtherDevice');
  String cloudSyncAppliedToast(int count, String deviceName) =>
      _r('cloudSyncAppliedToast', {'count': count, 'deviceName': deviceName});
  String cloudSyncFailedToast(String reason) =>
      _r('cloudSyncFailedToast', {'reason': reason});
  String get cloudSyncErrorDeviceLimit => _r('cloudSyncErrorDeviceLimit');
  String get cloudSyncErrorDeviceUntrusted =>
      _r('cloudSyncErrorDeviceUntrusted');
  String get cloudSyncErrorNetwork => _r('cloudSyncErrorNetwork');

  // 分类子 Tab
  String get settingsTabGeneral => _r('settingsTabGeneral');
  String get settingsTabTracker => _r('settingsTabTracker');
  String get settingsTabServers => _r('settingsTabServers');

  // 设置分组小节标题
  String get settingsGroupStartupTray => _r('settingsGroupStartupTray');
  String get settingsGroupSystem => _r('settingsGroupSystem');
  String get settingsGroupSaveLocation => _r('settingsGroupSaveLocation');
  String get settingsGroupBehavior => _r('settingsGroupBehavior');
  String get settingsGroupConnection => _r('settingsGroupConnection');
  String get settingsGroupRetry => _r('settingsGroupRetry');
  String get settingsGroupAdvanced => _r('settingsGroupAdvanced');
  String get settingsGroupTheme => _r('settingsGroupTheme');
  String get settingsGroupInterface => _r('settingsGroupInterface');

  // ─────────────────────────────────────────────
  // Settings — 通用
  // ─────────────────────────────────────────────
  String get autoStartup => _r('autoStartup');
  String get autoStartupDesc => _r('autoStartupDesc');
  String get closeToTray => _r('closeToTray');
  String get closeToTrayDesc => _r('closeToTrayDesc');
  String get startMinimizedToTray => _r('startMinimizedToTray');
  String get startMinimizedToTrayDesc => _r('startMinimizedToTrayDesc');
  String get floatingBall => _r('floatingBall');
  String get floatingBallDesc => _r('floatingBallDesc');
  String get floatingBallActiveOnly => _r('floatingBallActiveOnly');
  String get floatingBallActiveOnlyDesc => _r('floatingBallActiveOnlyDesc');
  String get floatingBallWaylandUnsupported =>
      _r('floatingBallWaylandUnsupported');
  String get clipboardWatch => _r('clipboardWatch');
  String get clipboardWatchDesc => _r('clipboardWatchDesc');
  String get clipboardUrlDetectedTitle => _r('clipboardUrlDetectedTitle');
  String get clipboardUrlDetectedBody => _r('clipboardUrlDetectedBody');
  String get trayShowFloatingBall => _r('trayShowFloatingBall');
  String get hideFloatingBall => _r('hideFloatingBall');
  String get torrentFileAssociation => _r('torrentFileAssociation');
  String get torrentFileAssociationDesc => _r('torrentFileAssociationDesc');
  String get torrentAssocDialogTitle => _r('torrentAssocDialogTitle');
  String get torrentAssocDialogDesc => _r('torrentAssocDialogDesc');
  String get notifyOnComplete => _r('notifyOnComplete');
  String get notifyOnCompleteDesc => _r('notifyOnCompleteDesc');
  String get silentDownload => _r('silentDownload');
  String get silentDownloadDesc => _r('silentDownloadDesc');
  String get useServerTime => _r('useServerTime');
  String get useServerTimeDesc => _r('useServerTimeDesc');
  String get keepAwakeWhileDownloading => _r('keepAwakeWhileDownloading');
  String get keepAwakeWhileDownloadingDesc =>
      _r('keepAwakeWhileDownloadingDesc');

  // 侧边栏显示
  String get sidebarVisibility => _r('sidebarVisibility');
  String get sidebarVisibilityDesc => _r('sidebarVisibilityDesc');
  String get showSidebarStatus => _r('showSidebarStatus');
  String get showSidebarStatusDesc => _r('showSidebarStatusDesc');
  String get showSidebarQueues => _r('showSidebarQueues');
  String get showSidebarQueuesDesc => _r('showSidebarQueuesDesc');
  String get showSidebarCategory => _r('showSidebarCategory');
  String get showSidebarCategoryDesc => _r('showSidebarCategoryDesc');
  String get hideSection => _r('hideSection');

  // 多设备协同
  String get deviceSection => _r('deviceSection');
  String get allDevices => _r('allDevices');
  String get thisDevice => _r('thisDevice');
  String get addDeviceEntry => _r('addDeviceEntry');
  String get deviceOnline => _r('deviceOnline');
  String get deviceOffline => _r('deviceOffline');
  String get showSidebarDevice => _r('showSidebarDevice');
  String get showSidebarDeviceDesc => _r('showSidebarDeviceDesc');
  String get downloadTo => _r('downloadTo');
  String get downloadToHint => _r('downloadToHint');
  String get multiDeviceTitle => _r('multiDeviceTitle');
  String get multiDeviceDesc => _r('multiDeviceDesc');
  String devicesOnlineCount(int count) =>
      _r('devicesOnlineCount', {'count': count});
  String dispatchedToDevice(String device) =>
      _r('dispatchedToDevice', {'device': device});
  String get dispatchFailed => _r('dispatchFailed');
  String get addDeviceHint => _r('addDeviceHint');
  String get addDeviceAccountLabel => _r('addDeviceAccountLabel');
  String get addDeviceLoginRequired => _r('addDeviceLoginRequired');

  // 本地设备互联（局域网配对，免账号）
  String get addDeviceTabAccount => _r('addDeviceTabAccount');
  String get addDeviceTabLocal => _r('addDeviceTabLocal');
  String get localPairingHint => _r('localPairingHint');
  String get localPairingDiscovering => _r('localPairingDiscovering');
  String get localPairingNoDevices => _r('localPairingNoDevices');
  String get localPairingCodeLabel => _r('localPairingCodeLabel');
  String get localPairingCodePlaceholder => _r('localPairingCodePlaceholder');
  String get localPairingCodeHint => _r('localPairingCodeHint');
  String get localPairingConnect => _r('localPairingConnect');
  String get localPairingManualAddress => _r('localPairingManualAddress');
  String get localPairingSasTitle => _r('localPairingSasTitle');
  String get localPairingSasHint => _r('localPairingSasHint');
  String get localPairingConfirm => _r('localPairingConfirm');
  String localPairingPaired(String device) =>
      _r('localPairingPaired', {'device': device});
  String get localPairingFailed => _r('localPairingFailed');
  String get localPairingOnline => _r('localPairingOnline');
  String get localPairingOffline => _r('localPairingOffline');
  String get localGenerateCode => _r('localGenerateCode');
  String localMyCode(String code) => _r('localMyCode', {'code': code});
  String addDeviceAccountSynced(String account) =>
      _r('addDeviceAccountSynced', {'account': account});
  String get addDeviceAccountFooter => _r('addDeviceAccountFooter');

  // 本地设备管理（未登录也可用，免账号）
  String get localDevicesSectionTitle => _r('localDevicesSectionTitle');
  String get localDevicesSectionDesc => _r('localDevicesSectionDesc');
  String get localDevicesEmpty => _r('localDevicesEmpty');
  String get localDeviceThisTitle => _r('localDeviceThisTitle');
  String get localDeviceThisDesc => _r('localDeviceThisDesc');
  String get localDeviceAddressLabel => _r('localDeviceAddressLabel');
  String get localDeviceAddressHint => _r('localDeviceAddressHint');
  String get localDeviceUnpair => _r('localDeviceUnpair');
  String get localDeviceCodeCopied => _r('localDeviceCodeCopied');
  String get apiServiceLanEnable => _r('apiServiceLanEnable');
  String get apiServiceLanEnableDesc => _r('apiServiceLanEnableDesc');

  // 标题栏按钮
  String get titlebarButtons => _r('titlebarButtons');
  String get titlebarButtonsDesc => _r('titlebarButtonsDesc');
  String get showTitlebarPauseAll => _r('showTitlebarPauseAll');
  String get showTitlebarPauseAllDesc => _r('showTitlebarPauseAllDesc');
  String get showTitlebarResumeAll => _r('showTitlebarResumeAll');
  String get showTitlebarResumeAllDesc => _r('showTitlebarResumeAllDesc');
  String get showTitlebarSettings => _r('showTitlebarSettings');
  String get showTitlebarSettingsDesc => _r('showTitlebarSettingsDesc');
  String get showTitlebarTheme => _r('showTitlebarTheme');
  String get showTitlebarThemeDesc => _r('showTitlebarThemeDesc');
  String get hideButton => _r('hideButton');

  // ─────────────────────────────────────────────
  // 自定义分类
  // ─────────────────────────────────────────────
  String get customCategories => _r('customCategories');
  String get customCategoriesDesc => _r('customCategoriesDesc');
  String get addCategory => _r('addCategory');
  String get editCategory => _r('editCategory');
  String get deleteCategory => _r('deleteCategory');
  String get deleteCategoryConfirm => _r('deleteCategoryConfirm');
  String get categoryName => _r('categoryName');
  String get categoryNameHint => _r('categoryNameHint');
  String get categoryIcon => _r('categoryIcon');
  String get matchMode => _r('matchMode');
  String get matchByExtension => _r('matchByExtension');
  String get matchByRegex => _r('matchByRegex');
  String get extensionsLabel => _r('extensionsLabel');
  String get extensionsHint => _r('extensionsHint');
  String get regexLabel => _r('regexLabel');
  String get regexHint => _r('regexHint');
  String get regexInvalid => _r('regexInvalid');
  String get categoryNameRequired => _r('categoryNameRequired');
  String get extensionsRequired => _r('extensionsRequired');
  String get categorySaveDir => _r('categorySaveDir');
  String get categorySaveDirDesc => _r('categorySaveDirDesc');
  String get restoreDefaultPath => _r('restoreDefaultPath');
  String get nCustomCategories => _r('nCustomCategories');
  String get resetBuiltinCategories => _r('resetBuiltinCategories');
  String get resetAllCategoriesConfirm => _r('resetAllCategoriesConfirm');
  String get builtinCategory => _r('builtinCategory');
  String get customCategory => _r('customCategory');
  String get categoryPriorityNote => _r('categoryPriorityNote');
  String get settingFailed => _r('settingFailed');
  String get autoStartupFailedDesc => _r('autoStartupFailedDesc');

  // ─────────────────────────────────────────────
  // Settings — 外观
  // ─────────────────────────────────────────────
  String get language => _r('language');
  String get languageDesc => _r('languageDesc');
  String get languageSystem => _r('languageSystem');
  String get languageChinese => _r('languageChinese');
  String get languageEnglish => _r('languageEnglish');
  String get themeMode => _r('themeMode');
  String get themeModeDesc => _r('themeModeDesc');
  String get themeSelection => _r('themeSelection');
  String get themeSelectionDesc => _r('themeSelectionDesc');
  String get themeColor => _r('themeColor');
  String get themeColorDesc => _r('themeColorDesc');
  String get themeModeSystem => _r('themeModeSystem');
  String get themeModeLight => _r('themeModeLight');
  String get themeModeDark => _r('themeModeDark');
  String get uiScale => _r('uiScale');
  String get uiScaleDesc => _r('uiScaleDesc');
  String get appIcon => _r('appIcon');
  String get appIconDesc => _r('appIconDesc');
  String get appIconDefault => _r('appIconDefault');
  String get appIconCustom => _r('appIconCustom');
  String get appIconBolt => _r('appIconBolt');
  String get appIconChooseImage => _r('appIconChooseImage');
  String get appIconApplyFailed => _r('appIconApplyFailed');
  String get appIconZoomHint => _r('appIconZoomHint');

  // ─────────────────────────────────────────────
  // 内置主题名称
  // ─────────────────────────────────────────────
  String get themeDefaultDark => _r('themeDefaultDark');
  String get themeDefaultLight => _r('themeDefaultLight');
  String get themeMidnightBlue => _r('themeMidnightBlue');
  String get themeNord => _r('themeNord');
  String get themeWarmLight => _r('themeWarmLight');
  String get themeDarkTheme => _r('themeDarkTheme');
  String get themeLightTheme => _r('themeLightTheme');
  String get themeImport => _r('themeImport');
  String get themeExport => _r('themeExport');
  String get themeImportSuccess => _r('themeImportSuccess');
  String get themeExportSuccess => _r('themeExportSuccess');
  String get themeImportError => _r('themeImportError');
  String get themeMore => _r('themeMore');

  // ─────────────────────────────────────────────
  // 主题色名称
  // ─────────────────────────────────────────────
  String get colorBlue => _r('colorBlue');
  String get colorGreen => _r('colorGreen');
  String get colorViolet => _r('colorViolet');
  String get colorRose => _r('colorRose');
  String get colorCustom => _r('colorCustom');

  // ─────────────────────────────────────────────
  // Settings — 下载
  // ─────────────────────────────────────────────
  String get defaultSaveDir => _r('defaultSaveDir');
  String get defaultSaveDirDesc => _r('defaultSaveDirDesc');
  String get selectDefaultSaveDir => _r('selectDefaultSaveDir');
  String get rememberLastSaveDir => _r('rememberLastSaveDir');
  String get rememberLastSaveDirDesc => _r('rememberLastSaveDirDesc');
  String get defaultThreads => _r('defaultThreads');
  String get defaultThreadsDesc => _r('defaultThreadsDesc');
  String get autoMaxConnections => _r('autoMaxConnections');
  String get autoMaxConnectionsDesc => _r('autoMaxConnectionsDesc');
  String get connPolicyCache => _r('connPolicyCache');
  String get connPolicyCacheDesc => _r('connPolicyCacheDesc');
  String get connPolicyCacheClear => _r('connPolicyCacheClear');
  String get connPolicyCacheCleared => _r('connPolicyCacheCleared');
  String get connPolicyCacheEmpty => _r('connPolicyCacheEmpty');
  String nRecords(int n) => _r('nRecords', {'n': n});
  String get maxConcurrent => _r('maxConcurrent');
  String get maxConcurrentDesc => _r('maxConcurrentDesc');
  String get speedLimit => _r('speedLimit');
  String get speedLimitDesc => _r('speedLimitDesc');
  String get speedLimitUnit => _r('speedLimitUnit');
  String nThreads(int n) => _r('nThreads', {'n': n});
  String nTasks(int n) => _r('nTasks', {'n': n});

  // 失败自动重试
  String get autoRetryCount => _r('autoRetryCount');
  String get autoRetryCountDesc => _r('autoRetryCountDesc');
  String get autoRetryOff => _r('autoRetryOff');
  String get autoRetryUnlimited => _r('autoRetryUnlimited');
  String nRetries(int n) => _r('nRetries', {'n': n});
  String get autoRetryDelay => _r('autoRetryDelay');
  String get autoRetryDelayDesc => _r('autoRetryDelayDesc');
  String get autoRetryDelayUnit => _r('autoRetryDelayUnit');

  // ─────────────────────────────────────────────
  // Settings — 代理
  // ─────────────────────────────────────────────
  String get proxySettings => _r('proxySettings');
  String get proxySettingsDesc => _r('proxySettingsDesc');
  String get proxyModeNone => _r('proxyModeNone');
  String get proxyModeNoneDesc => _r('proxyModeNoneDesc');
  String get proxyModeSystem => _r('proxyModeSystem');
  String get proxyModeSystemDesc => _r('proxyModeSystemDesc');
  String get proxyModeManual => _r('proxyModeManual');
  String get proxyModeManualDesc => _r('proxyModeManualDesc');
  String get proxyType => _r('proxyType');
  String get proxyHost => _r('proxyHost');
  String get proxyHostPlaceholder => _r('proxyHostPlaceholder');
  String get proxyPort => _r('proxyPort');
  String get proxyPortPlaceholder => _r('proxyPortPlaceholder');
  String get proxyUsername => _r('proxyUsername');
  String get proxyUsernamePlaceholder => _r('proxyUsernamePlaceholder');
  String get proxyPassword => _r('proxyPassword');
  String get proxyPasswordPlaceholder => _r('proxyPasswordPlaceholder');
  String get proxyNoList => _r('proxyNoList');
  String get proxyNoListDesc => _r('proxyNoListDesc');
  String get proxyNoListPlaceholder => _r('proxyNoListPlaceholder');
  String get proxyBtNote => _r('proxyBtNote');
  String get proxySystemDetecting => _r('proxySystemDetecting');
  String get proxySystemNotConfigured => _r('proxySystemNotConfigured');
  String get proxySystemDetected => _r('proxySystemDetected');
  String get proxyTestConnection => _r('proxyTestConnection');
  String get proxyTesting => _r('proxyTesting');
  String proxyTestSuccess(int ms) => _r('proxyTestSuccess', {'ms': ms});
  String proxyTestFailed(String error) =>
      _r('proxyTestFailed', {'error': error});

  // User-Agent 设置
  String get userAgent => _r('userAgent');
  String get userAgentDesc => _r('userAgentDesc');
  String get userAgentPlaceholder => _r('userAgentPlaceholder');
  String get userAgentTaskPlaceholder => _r('userAgentTaskPlaceholder');
  String get userAgentPresetDefault => _r('userAgentPresetDefault');
  String get userAgentPresetChrome => _r('userAgentPresetChrome');
  String get userAgentPresetFirefox => _r('userAgentPresetFirefox');
  String get userAgentPresetEdge => _r('userAgentPresetEdge');
  String get userAgentPresetSafari => _r('userAgentPresetSafari');
  String get userAgentPresetCustom => _r('userAgentPresetCustom');
  List<String> get searchKeywordsUserAgent =>
      _r('searchKeywordsUserAgent').split(',')..addAll(['ua', 'user-agent']);

  // 文件管理器自定义命令
  String get fileManagerSection => _r('fileManagerSection');
  String get revealFileCmdLabel => _r('revealFileCmdLabel');
  String get revealFileCmdDesc => _r('revealFileCmdDesc');
  String get revealFileCmdPlaceholder => _r('revealFileCmdPlaceholder');
  List<String> get searchKeywordsFileManager =>
      _r('searchKeywordsFileManager').split(',')
        ..addAll(['fm', 'reveal', 'open folder']);

  // Per-task proxy (新建下载对话框)
  String get taskProxy => _r('taskProxy');
  String get taskProxyDesc => _r('taskProxyDesc');
  String get taskProxyPlaceholder => _r('taskProxyPlaceholder');
  String get taskProxyAdvanced => _r('taskProxyAdvanced');
  String get taskProxyFormatHint => _r('taskProxyFormatHint');
  String get taskIgnoreTlsErrors => _r('taskIgnoreTlsErrors');
  String get taskIgnoreTlsErrorsDesc => _r('taskIgnoreTlsErrorsDesc');

  // 任务 Cookie（新建下载对话框高级选项）
  String get taskCookie => _r('taskCookie');
  String get taskCookieDesc => _r('taskCookieDesc');
  String get taskCookieBatchDesc => _r('taskCookieBatchDesc');
  String get taskCookiePlaceholder => _r('taskCookiePlaceholder');

  // 任务哈希校验（新建下载对话框高级选项，#247/#248）
  String get taskChecksum => _r('taskChecksum');
  String get taskChecksumDesc => _r('taskChecksumDesc');
  String get taskChecksumPlaceholder => _r('taskChecksumPlaceholder');

  // 任务自定义请求头（新建下载对话框高级选项，#347）
  String get taskHeaders => _r('taskHeaders');
  String get taskHeadersDesc => _r('taskHeadersDesc');
  String get taskHeadersKeyPlaceholder => _r('taskHeadersKeyPlaceholder');
  String get taskHeadersValuePlaceholder => _r('taskHeadersValuePlaceholder');
  String get taskHeadersAdd => _r('taskHeadersAdd');

  // ─────────────────────────────────────────────
  // Settings — API 服务
  // ─────────────────────────────────────────────
  String get apiServiceEnable => _r('apiServiceEnable');
  String get apiServiceEnableDesc => _r('apiServiceEnableDesc');
  String get apiServicePort => _r('apiServicePort');
  String get apiServicePortDesc => _r('apiServicePortDesc');
  String get apiServicePortInvalid => _r('apiServicePortInvalid');
  String get apiServiceToken => _r('apiServiceToken');
  String get apiServiceTokenDesc => _r('apiServiceTokenDesc');
  String get apiServiceTokenGenerate => _r('apiServiceTokenGenerate');
  String get apiServiceCopy => _r('apiServiceCopy');
  String get apiServiceCopied => _r('apiServiceCopied');
  String get apiServiceTokenClear => _r('apiServiceTokenClear');
  String get apiServiceTokenCleared => _r('apiServiceTokenCleared');
  String get apiServiceTokenClearConfirmTitle =>
      _r('apiServiceTokenClearConfirmTitle');
  String get apiServiceTokenClearConfirmDesc =>
      _r('apiServiceTokenClearConfirmDesc');
  String get apiServiceFeaturesTitle => _r('apiServiceFeaturesTitle');
  String get apiServiceFeaturesDesc => _r('apiServiceFeaturesDesc');
  String get apiServiceTakeover => _r('apiServiceTakeover');
  String get apiServiceTakeoverDesc => _r('apiServiceTakeoverDesc');
  String get apiServiceCopyScript => _r('apiServiceCopyScript');
  String get apiServiceScriptCopied => _r('apiServiceScriptCopied');
  String get apiServiceJsonrpc => _r('apiServiceJsonrpc');
  String get apiServiceJsonrpcDesc => _r('apiServiceJsonrpcDesc');
  String get apiServiceApi => _r('apiServiceApi');
  String get apiServiceApiDesc => _r('apiServiceApiDesc');
  String get apiServiceMcp => _r('apiServiceMcp');
  String get apiServiceMcpDesc => _r('apiServiceMcpDesc');
  String get apiServiceAddress => _r('apiServiceAddress');
  List<String> get searchKeywordsApiService =>
      _r('searchKeywordsApiService').split(',');

  // ─────────────────────────────────────────────
  // Settings — BT 下载
  // ─────────────────────────────────────────────
  String get btSettings => _r('btSettings');
  String get btSettingsDesc => _r('btSettingsDesc');
  String get btEnableDht => _r('btEnableDht');
  String get btEnableDhtDesc => _r('btEnableDhtDesc');
  String get btEnableUpnp => _r('btEnableUpnp');
  String get btEnableUpnpDesc => _r('btEnableUpnpDesc');
  String get btListenPort => _r('btListenPort');
  String get btListenPortDesc => _r('btListenPortDesc');
  String get btListenPortStart => _r('btListenPortStart');
  String get btListenPortEnd => _r('btListenPortEnd');
  String get btTrackerList => _r('btTrackerList');
  String get btTrackerListDesc => _r('btTrackerListDesc');
  String get btTrackerPlaceholder => _r('btTrackerPlaceholder');
  String btTrackerCount(int n) => _r('btTrackerCount', {'n': n});
  String get btResetTrackers => _r('btResetTrackers');
  String get btResetTrackersConfirm => _r('btResetTrackersConfirm');
  String get btTrackerSub => _r('btTrackerSub');
  String get btTrackerSubDesc => _r('btTrackerSubDesc');
  String btTrackerSubStatus(int n) => _r('btTrackerSubStatus', {'n': n});
  String get btTrackerSubNeverUpdated => _r('btTrackerSubNeverUpdated');
  String btTrackerSubUpdatedAt(String time) =>
      _r('btTrackerSubUpdatedAt', {'time': time});
  String get btTrackerSubUpdateNow => _r('btTrackerSubUpdateNow');
  String get btTrackerSubUpdating => _r('btTrackerSubUpdating');
  String get btTrackerSubUpdateFailed => _r('btTrackerSubUpdateFailed');
  String get btTrackerSubPlaceholder => _r('btTrackerSubPlaceholder');
  String get btTrackerSubResetConfirm => _r('btTrackerSubResetConfirm');
  String get btPortInvalid => _r('btPortInvalid');

  // ─────────────────────────────────────────────
  // eD2K 服务器设置
  // ─────────────────────────────────────────────
  String get ed2kSettings => _r('ed2kSettings');
  String get ed2kSettingsDesc => _r('ed2kSettingsDesc');
  String get ed2kServerList => _r('ed2kServerList');
  String get ed2kServerListDesc => _r('ed2kServerListDesc');
  String ed2kServerCount(int n) => _r('ed2kServerCount', {'n': n});
  String get ed2kResetServers => _r('ed2kResetServers');
  String get ed2kResetServersConfirm => _r('ed2kResetServersConfirm');
  String get ed2kServerPlaceholder => _r('ed2kServerPlaceholder');
  String get ed2kServerSub => _r('ed2kServerSub');
  String get ed2kServerSubDesc => _r('ed2kServerSubDesc');
  String get ed2kEnableKad => _r('ed2kEnableKad');
  String get ed2kEnableKadDesc => _r('ed2kEnableKadDesc');
  String get ed2kEnableUpnp => _r('ed2kEnableUpnp');
  String get ed2kEnableUpnpDesc => _r('ed2kEnableUpnpDesc');
  String get ed2kListenPort => _r('ed2kListenPort');
  String get ed2kListenPortDesc => _r('ed2kListenPortDesc');
  String ed2kServerSubStatus(int n) => _r('ed2kServerSubStatus', {'n': n});
  String get ed2kServerSubNeverUpdated => _r('ed2kServerSubNeverUpdated');
  String ed2kServerSubUpdatedAt(String time) =>
      _r('ed2kServerSubUpdatedAt', {'time': time});
  String get ed2kServerSubUpdateNow => _r('ed2kServerSubUpdateNow');
  String get ed2kServerSubUpdating => _r('ed2kServerSubUpdating');
  String get ed2kServerSubUpdateFailed => _r('ed2kServerSubUpdateFailed');
  String get ed2kServerSubPlaceholder => _r('ed2kServerSubPlaceholder');
  String get ed2kServerSubResetConfirm => _r('ed2kServerSubResetConfirm');

  // ─────────────────────────────────────────────
  // File picker 错误
  // ─────────────────────────────────────────────
  String get filePickerErrorTimeout => _r('filePickerErrorTimeout');
  String get filePickerErrorNoTool => _r('filePickerErrorNoTool');
  String get filePickerErrorNative => _r('filePickerErrorNative');
  String get filePickerErrorGeneric => _r('filePickerErrorGeneric');
  String get btSettingsRestartHint => _r('btSettingsRestartHint');

  // ─────────────────────────────────────────────
  // Settings — 关于
  // ─────────────────────────────────────────────
  String get appDescription => _r('appDescription');
  String get currentVersion => _r('currentVersion');
  String get latestVersion => _r('latestVersion');
  String get publishDate => _r('publishDate');
  String get softwareUpdate => _r('softwareUpdate');
  String get checkUpdateDesc => _r('checkUpdateDesc');
  String get autoCheckUpdate => _r('autoCheckUpdate');
  String get autoCheckUpdateDesc => _r('autoCheckUpdateDesc');
  String get analyticsEnabled => _r('analyticsEnabled');
  String get analyticsEnabledDesc => _r('analyticsEnabledDesc');
  String get updateChannel => _r('updateChannel');
  String get updateChannelDesc => _r('updateChannelDesc');
  String get updateChannelStable => _r('updateChannelStable');
  String get updateChannelFrontier => _r('updateChannelFrontier');
  String get donateTitle => _r('donateTitle');
  String donateDate(int y, int m, int d) => _r('donateDate', {
    'y': y,
    'm': m,
    'd': d,
    'month': _r('monthNames').split(',')[m - 1],
  });
  String donateBody(String date, int releases, int commits) => _r(
    'donateBody',
    {'date': date, 'releases': releases, 'commits': commits},
  );
  String get donateThanks => _r('donateThanks');
  String get donateButton => _r('donateButton');
  String get upToDate => _r('upToDate');
  String newVersionFound(String v) => _r('newVersionFound', {'v': v});
  String get updateNow => _r('updateNow');
  String get updateLater => _r('updateLater');
  String get skipThisVersion => _r('skipThisVersion');
  String updatePromptBody(String v, String size) =>
      _r('updatePromptBody', {'v': v, 'size': size});
  String get downloadComplete => _r('downloadComplete');
  String get downloadingUpdate => _r('downloadingUpdate');
  String segmentsDownloading(int active, int total) =>
      _r('segmentsDownloading', {'active': active, 'total': total});
  String get checking => _r('checking');
  String get checkUpdate => _r('checkUpdate');
  String downloadUpdate(String size) => _r('downloadUpdate', {'size': size});
  String get recheck => _r('recheck');
  String get updateFailedTitle => _r('updateFailedTitle');
  String get updateFailedOpenSite => _r('updateFailedOpenSite');
  String get updateFallbackToTask => _r('updateFallbackToTask');
  String get updateFallbackTaskCreated => _r('updateFallbackTaskCreated');
  String get officialWebsite => _r('officialWebsite');
  String get visitWebsiteForMore => _r('visitWebsiteForMore');

  // ─────────────────────────────────────────────
  // Settings — 日志导出
  // ─────────────────────────────────────────────
  String get logExport => _r('logExport');
  String get logExportDesc => _r('logExportDesc');
  String logExportInfo(int count, String size) =>
      _r('logExportInfo', {'count': count, 'size': size});
  String get logExportButton => _r('logExportButton');
  String get logOpenDirButton => _r('logOpenDirButton');
  String logExportSuccess(int count) =>
      _r('logExportSuccess', {'count': count});
  String get logExportEmpty => _r('logExportEmpty');
  String get logExportFailed => _r('logExportFailed');
  String get logSelectExportDir => _r('logSelectExportDir');
  String get logMaxSize => _r('logMaxSize');
  String get logMaxSizeDesc => _r('logMaxSizeDesc');

  // ─────────────────────────────────────────────
  // 更新日志弹窗
  // ─────────────────────────────────────────────
  String get changelogTitle => _r('changelogTitle');
  String changelogSubtitle(String v) => _r('changelogSubtitle', {'v': v});
  String get changelogUpdateNow => _r('changelogUpdateNow');
  String get changelogLater => _r('changelogLater');
  String changelogVersionCount(int n) => _r('changelogVersionCount', {'n': n});

  // ─────────────────────────────────────────────
  // Settings — 搜索关键词
  // ─────────────────────────────────────────────
  List<String> get searchKeywordsAutoStartup =>
      _r('searchKeywordsAutoStartup').split(',')
        ..addAll(['startup', 'auto', 'boot']);
  List<String> get searchKeywordsCloseToTray =>
      _r('searchKeywordsCloseToTray').split(',')
        ..addAll(['tray', 'close', 'minimize']);
  List<String> get searchKeywordsStartMinimizedToTray =>
      _r('searchKeywordsStartMinimizedToTray').split(',')
        ..addAll(['startup', 'minimize', 'tray']);
  List<String> get searchKeywordsFloatingBall =>
      _r('searchKeywordsFloatingBall').split(',')
        ..addAll(['floating', 'ball', 'widget', 'overlay']);
  List<String> get searchKeywordsClipboardWatch =>
      _r('searchKeywordsClipboardWatch').split(',')
        ..addAll(['clipboard', 'watch', 'monitor']);
  List<String> get searchKeywordsLanguage =>
      _r('searchKeywordsLanguage').split(',')
        ..addAll(['language', 'locale', 'lang']);
  List<String> get searchKeywordsThemeMode =>
      _r('searchKeywordsThemeMode').split(',')
        ..addAll(['theme', 'dark', 'light']);
  List<String> get searchKeywordsThemeColor =>
      _r('searchKeywordsThemeColor').split(',')
        ..addAll(['color', 'scheme', 'accent']);
  List<String> get searchKeywordsUiScale =>
      _r('searchKeywordsUiScale').split(',')
        ..addAll(['scale', 'zoom', 'size', 'dpi']);
  List<String> get searchKeywordsAppIcon =>
      _r('searchKeywordsAppIcon').split(',')
        ..addAll(['icon', 'logo', 'taskbar', 'tray']);
  List<String> get searchKeywordsSaveDir =>
      _r('searchKeywordsSaveDir').split(',')
        ..addAll(['save', 'directory', 'path', 'folder']);
  List<String> get searchKeywordsThreads =>
      _r('searchKeywordsThreads').split(',')..addAll(['segment', 'thread']);
  List<String> get searchKeywordsConcurrent =>
      _r('searchKeywordsConcurrent').split(',')
        ..addAll(['concurrent', 'parallel', 'max']);
  List<String> get searchKeywordsSpeedLimit =>
      _r('searchKeywordsSpeedLimit').split(',')
        ..addAll(['speed', 'limit', 'bandwidth']);
  List<String> get searchKeywordsUpdate =>
      _r('searchKeywordsUpdate').split(',')
        ..addAll(['update', 'upgrade', 'version']);
  List<String> get searchKeywordsFileAssoc =>
      _r('searchKeywordsFileAssoc').split(',')
        ..addAll(['torrent', 'association', 'file']);
  List<String> get searchKeywordsNotifyOnComplete =>
      _r('searchKeywordsNotifyOnComplete').split(',')
        ..addAll(['notification', 'complete', 'toast']);
  List<String> get searchKeywordsSilentDownload =>
      _r('searchKeywordsSilentDownload').split(',')
        ..addAll(['silent', 'confirm', 'dialog']);
  List<String> get searchKeywordsUseServerTime =>
      _r('searchKeywordsUseServerTime').split(',')
        ..addAll(['time', 'timestamp', 'mtime', 'last-modified']);
  List<String> get searchKeywordsKeepAwake =>
      _r('searchKeywordsKeepAwake').split(',')
        ..addAll(['awake', 'sleep', 'screen', 'wake']);
  List<String> get searchKeywordsBtSettings =>
      _r('searchKeywordsBtSettings').split(',')
        ..addAll(['bt', 'torrent', 'tracker', 'dht', 'peer']);
  List<String> get searchKeywordsEd2kSettings =>
      _r('searchKeywordsEd2kSettings').split(',')
        ..addAll(['ed2k', 'emule', 'edonkey', 'server']);
  List<String> get searchKeywordsProxy =>
      _r('searchKeywordsProxy').split(',')..addAll(['proxy', 'socks', 'http']);
  List<String> get searchKeywordsLogExport =>
      _r('searchKeywordsLogExport').split(',')
        ..addAll(['log', 'export', 'debug']);
  List<String> get searchKeywordsDonate =>
      _r('searchKeywordsDonate').split(',')
        ..addAll(['donate', 'sponsor', 'support']);
  List<String> get searchKeywordsSidebarVisibility =>
      _r('searchKeywordsSidebarVisibility').split(',');
  List<String> get searchKeywordsTitlebarButtons =>
      _r('searchKeywordsTitlebarButtons').split(',');
  List<String> get searchKeywordsCustomCategories =>
      _r('searchKeywordsCustomCategories').split(',');

  // ─────────────────────────────────────────────
  // Feedback
  // ─────────────────────────────────────────────
  String get feedback => _r('feedback');
  String get feedbackTitle => _r('feedbackTitle');
  String get feedbackDesc => _r('feedbackDesc');
  String get feedbackTypeLabel => _r('feedbackTypeLabel');
  String get feedbackTypeFeature => _r('feedbackTypeFeature');
  String get feedbackTypeBug => _r('feedbackTypeBug');
  String get feedbackTypeOther => _r('feedbackTypeOther');
  String get feedbackTitleLabel => _r('feedbackTitleLabel');
  String get feedbackTitlePlaceholder => _r('feedbackTitlePlaceholder');
  String get feedbackDescLabel => _r('feedbackDescLabel');
  String get feedbackDescPlaceholder => _r('feedbackDescPlaceholder');
  String get feedbackContactLabel => _r('feedbackContactLabel');
  String get feedbackContactPlaceholder => _r('feedbackContactPlaceholder');
  String get feedbackContactHint => _r('feedbackContactHint');
  String get feedbackVersionLabel => _r('feedbackVersionLabel');
  String get feedbackVersionAuto => _r('feedbackVersionAuto');
  String get feedbackSysInfoLabel => _r('feedbackSysInfoLabel');
  String get feedbackSysInfoSystem => _r('feedbackSysInfoSystem');
  String get feedbackSysInfoHint => _r('feedbackSysInfoHint');
  String get feedbackAttachLogs => _r('feedbackAttachLogs');
  String get feedbackAttachLogsHint => _r('feedbackAttachLogsHint');
  String get feedbackOptional => _r('feedbackOptional');
  String get feedbackSubmit => _r('feedbackSubmit');
  String get feedbackSubmitting => _r('feedbackSubmitting');
  String get feedbackSuccess => _r('feedbackSuccess');
  String get feedbackError => _r('feedbackError');
  String get feedbackRateLimited => _r('feedbackRateLimited');
  String feedbackTitleCount(int n) => _r('feedbackTitleCount', {'n': n});
  String feedbackDescCount(int n) => _r('feedbackDescCount', {'n': n});

  // ─────────────────────────────────────────────
  // HLS 画质选择
  // ─────────────────────────────────────────────
  String get hlsQualityTitle => _r('hlsQualityTitle');
  String get hlsQualityDesc => _r('hlsQualityDesc');
  String hlsQualityResolution(int w, int h) => '${w}x$h';
  String hlsQualityBandwidth(String speed) => speed;

  // ─────────────────────────────────────────────
  // 插件 resolve 变体选择
  // ─────────────────────────────────────────────
  String get resolveVariantTitle => _r('resolveVariantTitle');
  String get resolveVariantDesc => _r('resolveVariantDesc');

  // ─────────────────────────────────────────────
  // TrayService
  // ─────────────────────────────────────────────
  String get trayShowWindow => _r('trayShowWindow');
  String get trayExit => _r('trayExit');

  // ─────────────────────────────────────────────
  // macOS 应用菜单栏
  // ─────────────────────────────────────────────
  String get menuFile => _r('menuFile');
  String get menuNewDownload => _r('menuNewDownload');
  String get menuCloseWindow => _r('menuCloseWindow');
  String get menuEdit => _r('menuEdit');
  String get menuSelectAll => _r('menuSelectAll');
  String get menuFind => _r('menuFind');
  String get menuView => _r('menuView');
  String get menuWindow => _r('menuWindow');
  String get menuHelp => _r('menuHelp');
  String get menuCheckForUpdates => _r('menuCheckForUpdates');
  String get menuSettings => _r('menuSettings');
  String get menuWebsite => _r('menuWebsite');
  String get menuFeedback => _r('menuFeedback');
  String get menuAbout => _r('menuAbout');
  String get menuHide => _r('menuHide');
  String get menuHideOthers => _r('menuHideOthers');
  String get menuShowAll => _r('menuShowAll');
  String get menuQuit => _r('menuQuit');
  String get menuToggleFullScreen => _r('menuToggleFullScreen');
  String get menuMinimize => _r('menuMinimize');
  String get menuZoom => _r('menuZoom');
  String get menuBringAllToFront => _r('menuBringAllToFront');

  // ─────────────────────────────────────────────
  // DownloadCompleteWindow
  // ─────────────────────────────────────────────
  String get downloadCompleted => _r('downloadCompleted');
  String batchDownloadCompleted(int count) =>
      _r('batchDownloadCompleted', {'count': count});
  String andMoreFiles(int count) => _r('andMoreFiles', {'count': count});
  String get openFileFolder => _r('openFileFolder');

  // ─────────────────────────────────────────────
  // 移动端（Mobile）
  // ─────────────────────────────────────────────
  String get mobileNavDownloads => _r('mobileNavDownloads');
  String mobileSpeedSummary(String speed, int n) =>
      _r('mobileSpeedSummary', {'speed': speed, 'n': n});
  String get mobileIdleSummary => _r('mobileIdleSummary');
  String get mobileSearchHint => _r('mobileSearchHint');
  String get mobileFilterTasks => _r('mobileFilterTasks');
  String get mobileFileType => _r('mobileFileType');
  String get mobileByQueue => _r('mobileByQueue');
  String get mobileResetFilter => _r('mobileResetFilter');
  String get mobileMoveToQueue => _r('mobileMoveToQueue');
  String get mobileSelectQueue => _r('mobileSelectQueue');
  String get mobileMovedToQueue => _r('mobileMovedToQueue');
  String get mobilePaste => _r('mobilePaste');
  String get mobilePasted => _r('mobilePasted');
  String get mobileClipboardEmpty => _r('mobileClipboardEmpty');
  String get mobileSaveTo => _r('mobileSaveTo');
  String get mobileAdvancedOptions => _r('mobileAdvancedOptions');
  String get mobileEnterUrl => _r('mobileEnterUrl');
  String get mobileDownloadStarted => _r('mobileDownloadStarted');
  String get mobileUrlHint => _r('mobileUrlHint');
  String get mobilePausedAllToast => _r('mobilePausedAllToast');
  String get mobileResumedAllToast => _r('mobileResumedAllToast');
  String get mobileTaskDetail => _r('mobileTaskDetail');
  String get mobileSegTitle => _r('mobileSegTitle');
  String get mobileSegRunning => _r('mobileSegRunning');
  String get mobileSegStopped => _r('mobileSegStopped');
  String get mobileSegDone => _r('mobileSegDone');
  String get mobileSegActive => _r('mobileSegActive');
  String get mobileSegPending => _r('mobileSegPending');
  String get mobileSpeedCurve => _r('mobileSpeedCurve');
  String get mobileSpeedWindow => _r('mobileSpeedWindow');
  String mobileSpeedPeak(String v) => _r('mobileSpeedPeak', {'v': v});
  String get mobileTaskInfo => _r('mobileTaskInfo');
  String get mobileProtocol => _r('mobileProtocol');
  String get mobileCreatedAt => _r('mobileCreatedAt');
  String get mobileBoostAction => _r('mobileBoostAction');
  String get mobileBoosted => _r('mobileBoosted');
  String get mobileBoostOn => _r('mobileBoostOn');
  String get mobileBoostOff => _r('mobileBoostOff');
  String get mobileRetry => _r('mobileRetry');
  String get mobileOpenFile => _r('mobileOpenFile');
  String get mobileOpenFileFailed => _r('mobileOpenFileFailed');
  String get mobileFileNotFound => _r('mobileFileNotFound');
  String get mobileNoAppToOpen => _r('mobileNoAppToOpen');
  String get mobileTaskDeleted => _r('mobileTaskDeleted');
  String get mobileTaskFileDeleted => _r('mobileTaskFileDeleted');
  String get mobilePrivacyPolicy => _r('mobilePrivacyPolicy');
  String get mobileOpenSource => _r('mobileOpenSource');
  String get mobileFooter => _r('mobileFooter');
  String get mobilePickDirUnmappable => _r('mobilePickDirUnmappable');
  String get mobileAllFilesTitle => _r('mobileAllFilesTitle');
  String get mobileAllFilesDesc => _r('mobileAllFilesDesc');
  String get mobileGoGrant => _r('mobileGoGrant');

  // ─────────────────────────────────────────────
  // 扩展（插件 + 组件）
  // ─────────────────────────────────────────────
  String get settingsCatExtensions => _r('settingsCatExtensions');
  String get settingsCatExtensionsDesc => _r('settingsCatExtensionsDesc');

  // 插件系统 — 已安装管理
  String get settingsCatPlugins => _r('settingsCatPlugins');

  String get pluginsSectionTitle => _r('pluginsSectionTitle');
  String get pluginsEmpty => _r('pluginsEmpty');
  String get pluginCommonLoading => _r('pluginCommonLoading');
  String get pluginInstallZipButton => _r('pluginInstallZipButton');
  String pluginInstallZipFailed(String error) =>
      _r('pluginInstallZipFailed', {'error': error});
  String get pluginInstallDirLabel => _r('pluginInstallDirLabel');
  String get pluginInstallDirPlaceholder => _r('pluginInstallDirPlaceholder');
  String get pluginInstallDirButton => _r('pluginInstallDirButton');
  String get pluginDevModeSwitch => _r('pluginDevModeSwitch');
  String get pluginDevModeBadge => _r('pluginDevModeBadge');
  String get pluginDisabledManual => _r('pluginDisabledManual');
  String get pluginDisabledCircuitBreaker => _r('pluginDisabledCircuitBreaker');
  String get pluginSettingsTooltip => _r('pluginSettingsTooltip');
  String get pluginUninstallTooltip => _r('pluginUninstallTooltip');
  String get pluginUninstallTitle => _r('pluginUninstallTitle');
  String pluginUninstallMsg(String name) =>
      _r('pluginUninstallMsg', {'name': name});
  String get pluginOpInstallSuccess => _r('pluginOpInstallSuccess');
  String pluginOpInstallFailed(String message) =>
      _r('pluginOpInstallFailed', {'message': message});
  String get pluginOpUninstallSuccess => _r('pluginOpUninstallSuccess');
  String pluginOpUninstallFailed(String message) =>
      _r('pluginOpUninstallFailed', {'message': message});
  String pluginOpEnabledFailed(String message) =>
      _r('pluginOpEnabledFailed', {'message': message});
  String pluginOpGenericFailed(String message) =>
      _r('pluginOpGenericFailed', {'message': message});
  String get pluginDepsMissingTitle => _r('pluginDepsMissingTitle');
  String pluginDepsMissingBody(String components) =>
      _r('pluginDepsMissingBody', {'components': components});
  String get pluginDepsGoToComponents => _r('pluginDepsGoToComponents');
  String get pluginDepsLater => _r('pluginDepsLater');

  // ─────────────────────────────────────────────
  // 插件系统 — 插件市场
  // ─────────────────────────────────────────────
  String get marketSectionTitle => _r('marketSectionTitle');
  String get marketSectionDesc => _r('marketSectionDesc');
  String get marketEmpty => _r('marketEmpty');
  String marketLoadFailed(String message) =>
      _r('marketLoadFailed', {'message': message});
  String get marketInstallButton => _r('marketInstallButton');
  String get marketInstalledButton => _r('marketInstalledButton');
  String get marketInstallingButton => _r('marketInstallingButton');
  String get marketYankedDeprecated => _r('marketYankedDeprecated');
  String get marketSearchPlaceholder => _r('marketSearchPlaceholder');
  String get marketSearchNoResult => _r('marketSearchNoResult');
  String marketShowMore(int count) => _r('marketShowMore', {'count': '$count'});
  String get pluginDetailIdentity => _r('pluginDetailIdentity');
  String get pluginDetailAuthor => _r('pluginDetailAuthor');
  String get pluginDetailPublishTime => _r('pluginDetailPublishTime');
  String get pluginDetailMinAppVersion => _r('pluginDetailMinAppVersion');
  String get pluginDetailSettings => _r('pluginDetailSettings');
  String pluginDetailSettingsCount(int count) =>
      _r('pluginDetailSettingsCount', {'count': '$count'});
  String get pluginDetailHomepage => _r('pluginDetailHomepage');
  String get pluginDetailDescription => _r('pluginDetailDescription');
  String get pluginDetailPermissions => _r('pluginDetailPermissions');
  String get pluginPermFfmpegName => _r('pluginPermFfmpegName');
  String get pluginPermFfmpegDesc => _r('pluginPermFfmpegDesc');
  String get pluginPermYtdlpName => _r('pluginPermYtdlpName');
  String get pluginPermYtdlpDesc => _r('pluginPermYtdlpDesc');
  String get pluginPermUnknownDesc => _r('pluginPermUnknownDesc');
  String get pluginDetailUsage => _r('pluginDetailUsage');
  String get pluginDetailUsageBody => _r('pluginDetailUsageBody');
  String get marketYankedVulnerable => _r('marketYankedVulnerable');
  String get marketYankedMalicious => _r('marketYankedMalicious');
  String get marketRefreshTooltip => _r('marketRefreshTooltip');

  // ─────────────────────────────────────────────
  // 插件系统 — 设置表单
  // ─────────────────────────────────────────────
  String pluginSettingsDialogTitle(String name) =>
      _r('pluginSettingsDialogTitle', {'name': name});
  String get pluginSettingsSaveButton => _r('pluginSettingsSaveButton');
  String get pluginSettingsSaving => _r('pluginSettingsSaving');
  String pluginSettingsSaveFailed(String message) =>
      _r('pluginSettingsSaveFailed', {'message': message});
  String get pluginErrRequired => _r('pluginErrRequired');
  String get pluginCopyHelperScript => _r('pluginCopyHelperScript');
  String get pluginHelperScriptCopied => _r('pluginHelperScriptCopied');
  String get pluginErrNumber => _r('pluginErrNumber');
  String pluginErrMin(String min) => _r('pluginErrMin', {'min': min});
  String pluginErrMax(String max) => _r('pluginErrMax', {'max': max});
  String get pluginErrPattern => _r('pluginErrPattern');
  String get pluginErrSelect => _r('pluginErrSelect');
  String get pluginSelectPlaceholder => _r('pluginSelectPlaceholder');
  String get pluginFolderPickPlaceholder => _r('pluginFolderPickPlaceholder');

  // ─────────────────────────────────────────────
  // 插件系统 — 任务逃生舱 & 自动禁用通知
  // ─────────────────────────────────────────────
  String get taskIgnorePluginRetry => _r('taskIgnorePluginRetry');
  String get taskIgnorePluginRetryTitle => _r('taskIgnorePluginRetryTitle');
  String get taskIgnorePluginRetryMsg => _r('taskIgnorePluginRetryMsg');
  String pluginAutoDisabledToast(String name) =>
      _r('pluginAutoDisabledToast', {'name': name});

  // ─────────────────────────────────────────────
  // 组件管理（v1 仅 ffmpeg）
  // ─────────────────────────────────────────────
  String get settingsCatComponents => _r('settingsCatComponents');

  String get componentsFfmpegTitle => _r('componentsFfmpegTitle');
  String get componentsFfmpegDesc => _r('componentsFfmpegDesc');
  String get componentsYtdlpTitle => _r('componentsYtdlpTitle');
  String get componentsYtdlpDesc => _r('componentsYtdlpDesc');
  String get componentsStatusLoading => _r('componentsStatusLoading');
  String componentsStatusNotFound(String name) =>
      _r('componentsStatusNotFound', {'name': name});
  String componentsStatusNotFoundUnsupported(String name) =>
      _r('componentsStatusNotFoundUnsupported', {'name': name});
  String componentsManagedUnsupported(String name) =>
      _r('componentsManagedUnsupported', {'name': name});
  String get componentsSourceManual => _r('componentsSourceManual');
  String get componentsSourceManaged => _r('componentsSourceManaged');
  String get componentsSourceSystem => _r('componentsSourceSystem');
  String get componentsSystemPathLabel => _r('componentsSystemPathLabel');
  String get componentsSystemPathNotFound => _r('componentsSystemPathNotFound');

  String get componentsManualPathLabel => _r('componentsManualPathLabel');
  String componentsManualPathDesc(String name) =>
      _r('componentsManualPathDesc', {'name': name});
  String get componentsManualPathHintFfmpeg =>
      _r('componentsManualPathHintFfmpeg');
  String get componentsManualPathHintYtdlp =>
      _r('componentsManualPathHintYtdlp');
  String get componentsManualPathSave => _r('componentsManualPathSave');
  String get componentsManualPathClear => _r('componentsManualPathClear');

  String get componentsInstallSectionTitle =>
      _r('componentsInstallSectionTitle');
  String get componentsInstallSectionDescFfmpeg =>
      _r('componentsInstallSectionDescFfmpeg');
  String get componentsInstallSectionDescYtdlp =>
      _r('componentsInstallSectionDescYtdlp');
  String get componentsFetchVersionsButton =>
      _r('componentsFetchVersionsButton');
  String get componentsVersionsLoading => _r('componentsVersionsLoading');
  String componentsVersionsLoadFailed(String message) =>
      _r('componentsVersionsLoadFailed', {'message': message});
  String get componentsRetryVersions => _r('componentsRetryVersions');
  String get componentsVersionSelectPlaceholder =>
      _r('componentsVersionSelectPlaceholder');
  String componentsManagedVersionLabel(String version) =>
      _r('componentsManagedVersionLabel', {'version': version});
  String get componentsInstallButton => _r('componentsInstallButton');
  String get componentsReinstallButton => _r('componentsReinstallButton');
  String get componentsUninstallButton => _r('componentsUninstallButton');
  String get componentsInstalling => _r('componentsInstalling');
  String get componentsInstallUnknownSize => _r('componentsInstallUnknownSize');
  String componentsInstallSuccess(String name) =>
      _r('componentsInstallSuccess', {'name': name});
  String componentsInstallFailed(String message) =>
      _r('componentsInstallFailed', {'message': message});
  String componentsUninstallSuccess(String name) =>
      _r('componentsUninstallSuccess', {'name': name});
  String componentsUninstallFailed(String message) =>
      _r('componentsUninstallFailed', {'message': message});
  String componentsUninstallConfirmTitle(String name) =>
      _r('componentsUninstallConfirmTitle', {'name': name});
  String componentsUninstallConfirmMsg(String name) =>
      _r('componentsUninstallConfirmMsg', {'name': name});

  // ─────────────────────────────────────────────
  // 预解析清单选择弹窗（manifest*）
  // ─────────────────────────────────────────────
  String get manifestDialogTitle => _r('manifestDialogTitle');
  String get manifestGroupNamePlaceholder => _r('manifestGroupNamePlaceholder');
  String get manifestGroupNameTooltip => _r('manifestGroupNameTooltip');
  String manifestSummary(int count, String size) =>
      _r('manifestSummary', {'count': count, 'size': size});
  String get manifestPluginBadge => _r('manifestPluginBadge');
  String get manifestSearchPlaceholder => _r('manifestSearchPlaceholder');
  String get manifestSortByName => _r('manifestSortByName');
  String get manifestSortBySizeDesc => _r('manifestSortBySizeDesc');
  String get manifestSelectAll => _r('manifestSelectAll');
  String get manifestInvertSelection => _r('manifestInvertSelection');
  String get manifestClearSelection => _r('manifestClearSelection');
  String get manifestBreadcrumbUpTooltip => _r('manifestBreadcrumbUpTooltip');
  String get manifestBreadcrumbMoreTooltip =>
      _r('manifestBreadcrumbMoreTooltip');
  String manifestSearchResultCount(int count) =>
      _r('manifestSearchResultCount', {'count': count});
  String get manifestTreeEmpty => _r('manifestTreeEmpty');
  String manifestItemsCount(int count) =>
      _r('manifestItemsCount', {'count': count});
  String get manifestDirSizeUnknown => _r('manifestDirSizeUnknown');
  String get manifestFileSizeUnknown => _r('manifestFileSizeUnknown');
  String get manifestAdvancedToggle => _r('manifestAdvancedToggle');
  String get manifestAdvancedHint => _r('manifestAdvancedHint');
  String get manifestAdvancedDotTooltip => _r('manifestAdvancedDotTooltip');
  String get manifestProxyHint => _r('manifestProxyHint');
  String get manifestSegmentsHint => _r('manifestSegmentsHint');
  String get manifestIgnoreTlsHint => _r('manifestIgnoreTlsHint');
  String get manifestUaCustomPlaceholder => _r('manifestUaCustomPlaceholder');
  String get manifestCookieHint => _r('manifestCookieHint');
  String get manifestHeadersHint => _r('manifestHeadersHint');
  String manifestStartToQueue(String name) =>
      _r('manifestStartToQueue', {'name': name});
  String manifestLaterToQueue(String name) =>
      _r('manifestLaterToQueue', {'name': name});
  String get manifestNoSelection => _r('manifestNoSelection');
  String manifestSelectedSummary(int count, String size) =>
      _r('manifestSelectedSummary', {'count': count, 'size': size});
  String manifestSelectedSummaryApprox(int count, String size) =>
      _r('manifestSelectedSummaryApprox', {'count': count, 'size': size});
  String manifestUnknownSizeNote(int count) =>
      _r('manifestUnknownSizeNote', {'count': count});
  String manifestStartDownloadWithCount(int count) =>
      _r('manifestStartDownloadWithCount', {'count': count});
  String get manifestResolvingLabel => _r('manifestResolvingLabel');
  String get manifestResolvingCancel => _r('manifestResolvingCancel');

  // ─────────────────────────────────────────────
  // 任务组桌面 UI（组活卡片 / 组详情面板）
  // ─────────────────────────────────────────────
  String groupItemsCount(int n) => _r('groupItemsCount', {'n': n});
  String groupDoneCount(int n) => _r('groupDoneCount', {'n': n});
  String groupDownloadingCount(int n) =>
      _r('groupDownloadingCount', {'n': n});
  String groupPendingCount(int n) => _r('groupPendingCount', {'n': n});
  String groupPausedCount(int n) => _r('groupPausedCount', {'n': n});
  String groupFailedCount(int n) => _r('groupFailedCount', {'n': n});
  String groupDoneOfTotal(int done, int total) =>
      _r('groupDoneOfTotal', {'done': done, 'total': total});
  String groupEtaRemaining(String eta) =>
      _r('groupEtaRemaining', {'eta': eta});
  String get groupPauseAll => _r('groupPauseAll');
  String get groupResumeAll => _r('groupResumeAll');
  String get groupRetryFailed => _r('groupRetryFailed');
  String get groupOpenFolder => _r('groupOpenFolder');
  String get groupCopySourceLink => _r('groupCopySourceLink');
  String get groupDelete => _r('groupDelete');
  String get groupDeleteWithFiles => _r('groupDeleteWithFiles');
  String get groupPluginBadge => _r('groupPluginBadge');
  String get groupMemberExpiredResolve => _r('groupMemberExpiredResolve');
  String groupDirMeta(int count, String size) =>
      _r('groupDirMeta', {'count': count, 'size': size});
  String get groupDetailOverviewTab => _r('groupDetailOverviewTab');
  String get groupDetailMembersTab => _r('groupDetailMembersTab');
  String get groupDetailSource => _r('groupDetailSource');
  String get groupDetailSaveDir => _r('groupDetailSaveDir');
  String get groupDetailCreatedAt => _r('groupDetailCreatedAt');
  String get groupDetailQueue => _r('groupDetailQueue');
  String get groupDetailResolverPlugin => _r('groupDetailResolverPlugin');
  String get groupDetailLazyRenewHint => _r('groupDetailLazyRenewHint');
  String groupDetailSubtitle(String status) =>
      _r('groupDetailSubtitle', {'status': status});
  String get groupDetailNoMembers => _r('groupDetailNoMembers');
  String get groupMemberOfLabel => _r('groupMemberOfLabel');
}
