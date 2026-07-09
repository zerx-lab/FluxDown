import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import '../services/log_service.dart';
import '../i18n/locale_provider.dart';
import 'custom_category.dart';
import 'download_queue.dart';
import 'download_task.dart';

const _tag = 'DownloadCtrl';

/// 顶部 Tab 状态筛选
enum StatusTab { all, downloading, completed, paused, error }

/// 核心状态管理器 — 桥接 Rust 信号和 Flutter UI
class DownloadController extends ChangeNotifier {
  /// 全局单例引用，供无 context 场景（如 ExternalDownloadService）读取队列信息
  static DownloadController? globalInstance;
  final List<DownloadTask> _tasks = [];
  String? _selectedTaskId;
  FileCategory _categoryFilter = FileCategory.all;
  CustomCategory? _customCategoryFilter;

  /// 当前可见的非特殊分类列表（用于计算 "other" 排除逻辑）
  List<CustomCategory> _visibleNormalCategories = [];
  StatusTab _statusTab = StatusTab.all;

  /// 命名队列列表（来自 Rust AllQueues 信号）
  List<DownloadQueue> _queues = [];

  /// 当前队列筛选 ID：null = 不过滤（显示全部），'' = 默认队列，非空 = 指定命名队列
  String? _queueFilter;

  // 缓存 — 避免 filteredTasks / groupedTasks 每次访问重新计算
  List<DownloadTask>? _cachedFilteredTasks;
  List<TaskGroup>? _cachedGroupedTasks;

  // 管理模式（多选）
  bool _isManageMode = false;
  final Set<String> _checkedTaskIds = {};

  // 时间分组折叠状态（key 为 TimeGroup，value 为是否折叠）
  final Map<TimeGroup, bool> _collapsedGroups = {};

  /// 已删除任务 ID 集合 — 防止 Rust 残余信号将已删除任务「复活」到列表中。
  /// 在 _onAllTasks 刷新时清空（Rust 端不再包含该任务即可安全移除）。
  final Set<String> _deletedTaskIds = {};

  /// 批量删除进度追踪 — 存储正在等待 Rust 删除确认的任务 ID。
  /// Rust 每删除一个任务会发 TaskProgress{status:4, error:"deleted"} 确认。
  final Set<String> _pendingDeleteIds = {};
  int _batchDeleteDone = 0;
  int _batchDeleteTotal = 0;

  /// 是否正在批量删除（供 UI 显示进度条）
  bool get isBatchDeleting => _pendingDeleteIds.isNotEmpty;

  /// 批量删除进度 [0.0, 1.0]
  double get batchDeleteProgress =>
      _batchDeleteTotal > 0 ? _batchDeleteDone / _batchDeleteTotal : 0.0;

  /// 批量删除已完成数量
  int get batchDeleteDone => _batchDeleteDone;

  /// 批量删除总数量
  int get batchDeleteTotal => _batchDeleteTotal;

  /// 用户主动暂停的任务 ID 集合（乐观暂停）。
  /// 守卫：阻止 _onAllTasks 从 DB 覆盖 UI 暂停状态，以及阻止积压的 downloading
  /// 信号将 UI 改回下载中。只在 resumeTask / deleteTask 时移除。
  final Set<String> _optimisticPausedIds = {};

  /// 下载完成回调 — 当任务状态从非 completed 变为 completed 时触发
  void Function(DownloadTask task)? onTaskCompleted;

  /// 当前 Boost 优先任务 ID（空字符串 = 无优先任务）
  String _priorityTaskId = '';

  /// 因 Boost 自动暂停的任务数量（用于 Banner 显示）
  int _boostAutoPausedCount = 0;

  /// 因 Boost 被加入 _optimisticPausedIds 的任务 ID 集合。
  /// 用于 boost 取消时精确清理守卫条目，避免遗漏或误删。
  final Set<String> _boostAutoPausedIds = {};

  /// 延迟恢复队列：resumeAll 时超出并发限制的任务 ID 在此排队，
  /// 当活跃任务完成/出错时由 _resumeNextDeferred() 逐个发送到 Rust。
  final List<String> _deferredResumeQueue = [];

  StreamSubscription<RustSignalPack<TaskProgress>>? _progressSub;
  StreamSubscription<RustSignalPack<AllTasks>>? _allTasksSub;
  StreamSubscription<RustSignalPack<SegmentProgress>>? _segmentSub;
  StreamSubscription<RustSignalPack<SegmentSplitEvent>>? _splitSub;
  StreamSubscription<RustSignalPack<TaskMetaProbed>>? _metaProbedSub;
  StreamSubscription<RustSignalPack<QueuePositionsUpdate>>? _queuePosSub;
  StreamSubscription<RustSignalPack<AllQueues>>? _allQueuesSub;
  StreamSubscription<RustSignalPack<PriorityTaskChanged>>? _prioritySub;
  StreamSubscription<RustSignalPack<FileMissingChanged>>? _fileMissingSub;

  bool _disposed = false;

  DownloadController() {
    logInfo(_tag, 'constructor — starting listeners');
    globalInstance = this;
    _startListening();
    // 启动时请求所有持久化任务和队列
    const RequestAllTasks().sendSignalToRust();
    const RequestAllQueues().sendSignalToRust();
  }

  @override
  void dispose() {
    logInfo(_tag, 'dispose called');
    _disposed = true;
    if (globalInstance == this) globalInstance = null;
    _progressSub?.cancel();
    _allTasksSub?.cancel();
    _segmentSub?.cancel();
    _splitSub?.cancel();
    _metaProbedSub?.cancel();
    _queuePosSub?.cancel();
    _allQueuesSub?.cancel();
    _prioritySub?.cancel();
    _fileMissingSub?.cancel();
    super.dispose();
    logInfo(_tag, 'dispose done');
  }

  /// 安全的 notifyListeners — dispose 后不再通知，避免
  /// "A DownloadController was used after being disposed" 异常
  void _safeNotifyListeners() {
    _cachedFilteredTasks = null;
    _cachedGroupedTasks = null;
    if (!_disposed) notifyListeners();
  }

  // ---------------------------------------------------------------------------
  // Public getters
  // ---------------------------------------------------------------------------

  List<DownloadTask> get tasks => _tasks;

  FileCategory get categoryFilter => _categoryFilter;
  CustomCategory? get customCategoryFilter => _customCategoryFilter;
  StatusTab get statusTab => _statusTab;

  /// 命名队列列表（已按 position 排序）
  List<DownloadQueue> get queues => _queues;

  /// 当前队列筛选（null = 不过滤，'' = 默认队列，非空 = 指定命名队列）
  String? get queueFilter => _queueFilter;

  /// 按队列 ID 过滤
  List<DownloadTask> get _queueFiltered {
    if (_queueFilter == null) return _tasks;
    return _tasks.where((t) => t.queueId == _queueFilter).toList();
  }

  /// 队列过滤后的任务列表（公开给侧边栏计数使用）
  List<DownloadTask> get queueFilteredTasks => _queueFiltered;

  /// 按文件类型过滤（在队列过滤基础上叠加）
  /// 统一使用 CustomCategory 匹配（内置 + 自定义）
  List<DownloadTask> get _categoryFiltered {
    final byQueue = _queueFiltered;
    final filter = _customCategoryFilter;
    if (filter == null) {
      // 无分类筛选 或 "全部"
      if (_categoryFilter == FileCategory.all) return byQueue;
      return byQueue.where((t) => t.fileCategory == _categoryFilter).toList();
    }
    // "全部" 内置类型
    if (filter.builtinType == 'all') return byQueue;
    // "其他" — 不匹配任何可见的正常分类
    if (filter.builtinType == 'other') {
      return byQueue
          .where(
            (t) => !_visibleNormalCategories.any((c) => c.matches(t.fileName)),
          )
          .toList();
    }
    // 正常分类（内置或自定义）
    return byQueue.where((t) => filter.matches(t.fileName)).toList();
  }

  /// 双维度组合过滤后的任务列表（侧边栏文件类型 + 顶部状态 Tab）
  List<DownloadTask> get filteredTasks {
    if (_cachedFilteredTasks != null) return _cachedFilteredTasks!;
    final byCategory = _categoryFiltered;
    final result = switch (_statusTab) {
      StatusTab.all => byCategory,
      StatusTab.downloading =>
        byCategory
            .where(
              (t) =>
                  t.status == TaskStatus.downloading ||
                  t.status == TaskStatus.pending ||
                  t.status == TaskStatus.preparing ||
                  t.status == TaskStatus.resuming,
            )
            .toList(),
      StatusTab.completed =>
        byCategory.where((t) => t.status == TaskStatus.completed).toList(),
      StatusTab.paused =>
        byCategory.where((t) => t.status == TaskStatus.paused).toList(),
      StatusTab.error =>
        byCategory.where((t) => t.status == TaskStatus.error).toList(),
    };
    _cachedFilteredTasks = result;
    return result;
  }

  /// 将 filteredTasks 分组：活跃+排队任务置顶，历史任务按时间分组
  List<TaskGroup> get groupedTasks {
    if (_cachedGroupedTasks != null) return _cachedGroupedTasks!;
    final tasks = filteredTasks;

    // "全部" 和 "下载中" Tab：活跃+排队任务组置顶，历史任务按时间分组
    late final List<TaskGroup> result;
    if (_statusTab == StatusTab.all || _statusTab == StatusTab.downloading) {
      final activeTasks = tasks.where((t) => t.status.isActiveOrQueued).toList()
        ..sort(_compareActiveTasks);
      final historicalTasks = tasks
          .where((t) => !t.status.isActiveOrQueued)
          .toList();
      result = [
        if (activeTasks.isNotEmpty) TaskGroup(group: null, tasks: activeTasks),
        ..._buildTimeGroups(historicalTasks),
      ];
    } else {
      result = _buildTimeGroups(tasks);
    }
    _cachedGroupedTasks = result;
    return result;
  }

  List<TaskGroup> _buildTimeGroups(List<DownloadTask> tasks) {
    final Map<TimeGroup, List<DownloadTask>> map = {};
    for (final task in tasks) {
      (map[TimeGroup.fromDateTime(task.createdAt)] ??= []).add(task);
    }
    return [
      for (final g in TimeGroup.values)
        if (map[g] != null && map[g]!.isNotEmpty)
          TaskGroup(group: g, tasks: map[g]!),
    ];
  }

  int _compareActiveTasks(DownloadTask a, DownloadTask b) {
    int priority(TaskStatus s) => switch (s) {
      TaskStatus.downloading => 0,
      TaskStatus.preparing => 1,
      TaskStatus.resuming => 1,
      TaskStatus.pending => 2,
      _ => 3,
    };
    final diff = priority(a.status).compareTo(priority(b.status));
    if (diff != 0) return diff;
    // pending：按队列位置升序（位置仅在入队/出队时变化，稳定）
    if (a.status == TaskStatus.pending) {
      return a.queuePosition.compareTo(b.queuePosition);
    }
    // 活跃任务（downloading/preparing/resuming）：按创建时间升序，顺序稳定不抖动
    return a.createdAt.compareTo(b.createdAt);
  }

  /// 某个时间分组是否折叠（active group 永不折叠）
  bool isGroupCollapsed(TimeGroup? group) {
    if (group == null) return false; // 活跃组不可折叠
    return _collapsedGroups[group] ?? false;
  }

  /// 切换某个时间分组的折叠状态
  void toggleGroupCollapsed(TimeGroup group) {
    _collapsedGroups[group] = !isGroupCollapsed(group);
    _safeNotifyListeners();
  }

  /// 在当前文件类型筛选下，各状态的任务数量（用于 Tab 显示计数）
  int filteredCountForStatus(StatusTab tab) {
    final byCategory = _categoryFiltered;
    return switch (tab) {
      StatusTab.all => byCategory.length,
      StatusTab.downloading =>
        byCategory
            .where(
              (t) =>
                  t.status == TaskStatus.downloading ||
                  t.status == TaskStatus.pending ||
                  t.status == TaskStatus.preparing ||
                  t.status == TaskStatus.resuming,
            )
            .length,
      StatusTab.completed =>
        byCategory.where((t) => t.status == TaskStatus.completed).length,
      StatusTab.paused =>
        byCategory.where((t) => t.status == TaskStatus.paused).length,
      StatusTab.error =>
        byCategory.where((t) => t.status == TaskStatus.error).length,
    };
  }

  /// 各文件类型分类的任务数量（用于侧边栏显示计数）。
  /// 若当前有队列筛选，仅统计该队列内的任务。
  int countForCategory(FileCategory category) {
    final base = _queueFiltered;
    if (category == FileCategory.all) return base.length;
    return base.where((t) => t.fileCategory == category).length;
  }

  /// 自定义分类的任务数量
  int countForCustomCategory(CustomCategory category) {
    final base = _queueFiltered;
    return base.where((t) => category.matches(t.fileName)).length;
  }

  /// 统一分类的任务数量（支持 all/other/normal）
  int countForUnifiedCategory(
    CustomCategory category,
    List<CustomCategory> allVisible,
  ) {
    final base = _queueFiltered;
    if (category.builtinType == 'all') return base.length;
    if (category.builtinType == 'other') {
      final normals = allVisible
          .where((c) => c.builtinType != 'all' && c.builtinType != 'other')
          .toList();
      return base
          .where((t) => !normals.any((c) => c.matches(t.fileName)))
          .length;
    }
    return base.where((t) => category.matches(t.fileName)).length;
  }

  /// 各状态的任务数量（用于侧边栏状态区块计数）。
  /// 若当前有队列筛选，仅统计该队列内的任务，使计数与点击后的实际结果一致。
  int countForStatus(StatusTab tab) {
    final base = _queueFiltered;
    return switch (tab) {
      StatusTab.all => base.length,
      StatusTab.downloading =>
        base
            .where(
              (t) =>
                  t.status == TaskStatus.downloading ||
                  t.status == TaskStatus.pending ||
                  t.status == TaskStatus.preparing ||
                  t.status == TaskStatus.resuming,
            )
            .length,
      StatusTab.completed =>
        base.where((t) => t.status == TaskStatus.completed).length,
      StatusTab.paused =>
        base.where((t) => t.status == TaskStatus.paused).length,
      StatusTab.error => base.where((t) => t.status == TaskStatus.error).length,
    };
  }

  /// 指定队列中的任务数量（用于侧边栏队列计数）
  /// [queueId] 为空字符串表示默认队列
  int countForQueue(String queueId) {
    return _tasks.where((t) => t.queueId == queueId).length;
  }

  // ---------------------------------------------------------------------------
  // 管理模式（多选批量操作）
  // ---------------------------------------------------------------------------

  bool get isManageMode => _isManageMode;
  Set<String> get checkedTaskIds => _checkedTaskIds;
  int get checkedCount => _checkedTaskIds.length;

  /// 进入/退出管理模式
  void toggleManageMode() {
    _isManageMode = !_isManageMode;
    if (!_isManageMode) _checkedTaskIds.clear();
    _safeNotifyListeners();
  }

  void enterManageMode() {
    if (_isManageMode) return;
    _isManageMode = true;
    _safeNotifyListeners();
  }

  void exitManageMode() {
    if (!_isManageMode) return;
    _isManageMode = false;
    _checkedTaskIds.clear();
    _safeNotifyListeners();
  }

  /// 切换单个任务的选中状态
  void toggleTaskChecked(String taskId) {
    if (_checkedTaskIds.contains(taskId)) {
      _checkedTaskIds.remove(taskId);
    } else {
      _checkedTaskIds.add(taskId);
    }
    _safeNotifyListeners();
  }

  /// 全选当前筛选列表中的任务
  void selectAllFiltered() {
    for (final t in filteredTasks) {
      _checkedTaskIds.add(t.id);
    }
    _safeNotifyListeners();
  }

  /// 取消全选
  void deselectAll() {
    _checkedTaskIds.clear();
    _safeNotifyListeners();
  }

  /// 当前筛选列表是否已全选
  bool get isAllFilteredChecked {
    final filtered = filteredTasks;
    if (filtered.isEmpty) return false;
    return filtered.every((t) => _checkedTaskIds.contains(t.id));
  }

  /// 批量删除选中的任务（性能优化版）
  ///
  /// O(n) Set 查找替代旧版 O(n²) 逐个 removeWhere；
  /// 单次 BatchControlTask IPC 替代 N 次 ControlTask。
  void deleteCheckedTasks({required bool deleteFiles}) {
    final ids = _checkedTaskIds.toList();
    logInfo(
      _tag,
      'deleteCheckedTasks: ${ids.length} tasks, deleteFiles=$deleteFiles',
    );
    if (ids.isEmpty) return;

    // 进度追踪：批量删除 ≥2 个任务时启用，等待 Rust 逐个发回删除确认信号。
    const progressThreshold = 2;
    if (ids.length >= progressThreshold || _pendingDeleteIds.isNotEmpty) {
      _pendingDeleteIds.addAll(ids);
      _batchDeleteTotal = _batchDeleteDone + _pendingDeleteIds.length;
    }

    // O(n) 批量清理：构建 Set 一次遍历 _tasks
    final idSet = ids.toSet();
    _optimisticPausedIds.removeAll(idSet);
    _boostAutoPausedIds.removeAll(idSet);
    _deferredResumeQueue.removeWhere((id) => idSet.contains(id));
    _deletedTaskIds.addAll(idSet);
    _tasks.removeWhere((t) => idSet.contains(t.id));
    if (_selectedTaskId != null && idSet.contains(_selectedTaskId)) {
      _selectedTaskId = null;
    }

    // 单次 IPC 替代 N 次
    final action = deleteFiles ? 3 : 4;
    BatchControlTask(taskIds: ids, action: action).sendSignalToRust();

    _checkedTaskIds.clear();
    _isManageMode = false;
    _safeNotifyListeners();
  }

  String? get selectedTaskId => _selectedTaskId;

  DownloadTask? get selectedTask {
    if (_selectedTaskId == null) return null;
    final idx = _tasks.indexWhere((t) => t.id == _selectedTaskId);
    return idx >= 0 ? _tasks[idx] : null;
  }

  /// 当前 Boost 优先任务 ID（空字符串 = 无优先任务）
  String get priorityTaskId => _priorityTaskId;

  /// Boost 模式是否激活
  bool get isBoostActive => _priorityTaskId.isNotEmpty;

  /// 因 Boost 自动暂停的任务数量
  int get boostAutoPausedCount => _boostAutoPausedCount;

  /// 统计数据
  int get downloadingCount =>
      _tasks.where((t) => t.status == TaskStatus.downloading).length;
  int get completedCount =>
      _tasks.where((t) => t.status == TaskStatus.completed).length;
  int get pausedCount =>
      _tasks.where((t) => t.status == TaskStatus.paused).length;
  int get errorCount =>
      _tasks.where((t) => t.status == TaskStatus.error).length;
  int get pendingCount =>
      _tasks.where((t) => t.status == TaskStatus.pending).length;
  int get preparingCount =>
      _tasks.where((t) => t.status == TaskStatus.preparing).length;
  int get resumingCount =>
      _tasks.where((t) => t.status == TaskStatus.resuming).length;
  int get activeCount =>
      downloadingCount + pendingCount + preparingCount + resumingCount;

  /// 全局下载速度
  int get totalDownloadSpeed {
    int sum = 0;
    for (final t in _tasks) {
      if (t.status == TaskStatus.downloading) sum += t.speed;
    }
    return sum;
  }

  // ---------------------------------------------------------------------------
  // Actions — 发送信号到 Rust
  // ---------------------------------------------------------------------------

  void createTask({
    required String url,
    required String saveDir,
    String fileName = '',
    int segments = 0,
    String cookies = '',
    Uint8List? torrentFileBytes,
    String proxyUrl = '',
    String userAgent = '',
    String queueId = '',
    String checksum = '',
    Map<String, String> extraHeaders = const {},
    List<int> selectedFileIndices = const [],
  }) {
    logInfo(
      _tag,
      'createTask: url=$url, dir=$saveDir, file=$fileName, seg=$segments, cookies_len=${cookies.length}, torrent_bytes=${torrentFileBytes?.length ?? 0}, queue=$queueId, headers=${extraHeaders.length}, selected_files=${selectedFileIndices.length}',
    );
    CreateTask(
      url: url,
      saveDir: saveDir,
      fileName: fileName,
      segments: segments,
      cookies: cookies,
      torrentFileBytes: torrentFileBytes ?? Uint8List(0),
      proxyUrl: proxyUrl,
      userAgent: userAgent,
      queueId: queueId,
      checksum: checksum,
      extraHeaders: extraHeaders,
      selectedFileIndices: selectedFileIndices,
    ).sendSignalToRust();
  }

  /// Create a BT download task from already-read torrent bytes, with optional
  /// pre-selected file indices (from the new-download dialog file picker).
  ///
  /// When [selectedFileIndices] is non-empty, Rust skips the file-selection
  /// dialog and downloads only the specified files.
  void createTaskFromTorrentBytes({
    required Uint8List torrentBytes,
    required String torrentName,
    required String saveDir,
    String proxyUrl = '',
    String userAgent = '',
    String queueId = '',
    List<int> selectedFileIndices = const [],
  }) {
    logInfo(
      _tag,
      'createTaskFromTorrentBytes: name=$torrentName, dir=$saveDir, selected=${selectedFileIndices.length}',
    );
    CreateTask(
      url: '',
      saveDir: saveDir,
      fileName: torrentName,
      segments: 0,
      cookies: '',
      torrentFileBytes: torrentBytes,
      proxyUrl: proxyUrl,
      userAgent: userAgent,
      queueId: queueId,
      checksum: '',
      extraHeaders: const {},
      selectedFileIndices: selectedFileIndices,
    ).sendSignalToRust();
  }

  /// Create a download task from a .torrent file on disk.
  Future<void> createTaskFromTorrentFile({
    required String torrentFilePath,
    required String saveDir,
    String proxyUrl = '',
  }) async {
    logInfo(
      _tag,
      'createTaskFromTorrentFile: path=$torrentFilePath, dir=$saveDir',
    );
    await sendTorrentFileSignal(torrentFilePath, saveDir, proxyUrl: proxyUrl);
  }

  /// Read a .torrent file from disk and send a [CreateTask] signal to Rust.
  ///
  /// This is a static helper so it can be called both from a
  /// [DownloadController] instance and from [main.dart] (which has no
  /// controller instance at startup).
  ///
  /// When [selectedFileIndices] is non-empty, Rust skips the file-selection
  /// dialog and downloads only the specified files.
  static Future<void> sendTorrentFileSignal(
    String torrentFilePath,
    String saveDir, {
    String proxyUrl = '',
    String userAgent = '',
    String queueId = '',
    List<int> selectedFileIndices = const [],
    String torrentName = '',
  }) async {
    try {
      final file = File(torrentFilePath);
      final bytes = await file.readAsBytes();
      if (bytes.isEmpty) {
        logInfo(_tag, 'torrent file is empty: $torrentFilePath');
        return;
      }
      // Use the provided name, or derive from file name (without extension).
      final displayName = torrentName.isNotEmpty
          ? torrentName
          : () {
              final baseName = file.uri.pathSegments.last;
              return baseName.endsWith('.torrent')
                  ? baseName.substring(0, baseName.length - 8)
                  : baseName;
            }();

      CreateTask(
        url: '',
        saveDir: saveDir,
        fileName: displayName,
        segments: 0,
        cookies: '',
        torrentFileBytes: bytes,
        proxyUrl: proxyUrl,
        userAgent: userAgent,
        queueId: queueId,
        checksum: '',
        extraHeaders: const {},
        selectedFileIndices: selectedFileIndices,
      ).sendSignalToRust();
    } catch (e) {
      logInfo(_tag, 'failed to read torrent file: $e');
    }
  }

  /// 批量创建下载任务（多个 URL 共享同一保存目录和线程数）
  void batchCreateTask({
    required List<UrlEntry> entries,
    required String saveDir,
    int segments = 0,
    String proxyUrl = '',
    String userAgent = '',
    String queueId = '',
    String cookies = '',
    String referrer = '',
    Map<String, String> extraHeaders = const {},
  }) {
    logInfo(
      _tag,
      'batchCreateTask: ${entries.length} entries, dir=$saveDir, seg=$segments, queue=$queueId, cookies_len=${cookies.length}',
    );
    BatchCreateTask(
      entries: entries,
      saveDir: saveDir,
      segments: segments,
      proxyUrl: proxyUrl,
      userAgent: userAgent,
      queueId: queueId,
      cookies: cookies,
      referrer: referrer,
      extraHeaders: extraHeaders,
    ).sendSignalToRust();
  }

  void pauseTask(String taskId) {
    logInfo(_tag, 'pauseTask: $taskId');
    _optimisticPausedIds.add(taskId);
    // 乐观更新：立即切换到 paused 状态，防止用户快速重复点击
    final idx = _tasks.indexWhere((t) => t.id == taskId);
    if (idx >= 0) {
      final t = _tasks[idx];
      // 仅对活跃状态的任务执行暂停
      if (t.status == TaskStatus.downloading ||
          t.status == TaskStatus.resuming ||
          t.status == TaskStatus.pending ||
          t.status == TaskStatus.preparing) {
        _tasks[idx] = t.copyWith(status: TaskStatus.paused, speed: 0);
        _safeNotifyListeners();
      }
    }
    ControlTask(taskId: taskId, action: 0).sendSignalToRust();
  }

  void resumeTask(String taskId) {
    logInfo(_tag, 'resumeTask: $taskId');
    _optimisticPausedIds.remove(taskId);
    _boostAutoPausedIds.remove(taskId);
    // 立即切换到 resuming 状态，让 UI 即时响应
    final idx = _tasks.indexWhere((t) => t.id == taskId);
    if (idx >= 0) {
      _tasks[idx] = _tasks[idx].copyWith(status: TaskStatus.resuming);
      _safeNotifyListeners();
    }
    ControlTask(taskId: taskId, action: 1).sendSignalToRust();
  }

  void cancelTask(String taskId) {
    logInfo(_tag, 'cancelTask: $taskId');
    ControlTask(taskId: taskId, action: 2).sendSignalToRust();
  }

  /// 删除任务。[deleteFiles] 为 true 时同时删除磁盘上的已下载文件。
  void deleteTask(String taskId, {bool deleteFiles = true}) {
    logInfo(_tag, 'deleteTask: $taskId, deleteFiles=$deleteFiles');
    _optimisticPausedIds.remove(taskId);
    _boostAutoPausedIds.remove(taskId);
    _deferredResumeQueue.remove(taskId);
    final action = deleteFiles ? 3 : 4;
    ControlTask(taskId: taskId, action: action).sendSignalToRust();
    _deletedTaskIds.add(taskId);
    _tasks.removeWhere((t) => t.id == taskId);
    if (_selectedTaskId == taskId) _selectedTaskId = null;
    _safeNotifyListeners();
  }

  void selectTask(String? taskId) {
    if (_selectedTaskId == taskId) return;
    _selectedTaskId = taskId;
    _safeNotifyListeners();
  }

  void setCategoryFilter(FileCategory category) {
    final changed =
        _categoryFilter != category || _customCategoryFilter != null;
    _categoryFilter = category;
    _customCategoryFilter = null;
    if (!changed) return;
    _safeNotifyListeners();
  }

  /// 设置统一分类筛选（内置或自定义均走此方法）。
  /// [allVisible] 当前可见的全部分类列表，用于计算 "other" 排除逻辑。
  void setCustomCategoryFilter(
    CustomCategory? category, {
    List<CustomCategory> allVisible = const [],
  }) {
    if (category == null && _customCategoryFilter == null) return;
    if (category != null && _customCategoryFilter?.id == category.id) return;
    _customCategoryFilter = category;
    _visibleNormalCategories = allVisible
        .where((c) => c.builtinType != 'all' && c.builtinType != 'other')
        .toList();
    if (category != null) {
      _categoryFilter = FileCategory.all;
    }
    _safeNotifyListeners();
  }

  void setStatusTab(StatusTab tab) {
    if (_statusTab == tab) return;
    _statusTab = tab;
    _safeNotifyListeners();
  }

  /// 设置队列筛选。传入相同 ID 则切换回「不过滤」。
  void setQueueFilter(String? queueId) {
    if (_queueFilter == queueId) {
      _queueFilter = null;
    } else {
      _queueFilter = queueId;
    }
    _safeNotifyListeners();
  }

  // ---------------------------------------------------------------------------
  // Queue CRUD — 发送信号到 Rust
  // ---------------------------------------------------------------------------

  void createQueue({
    required String name,
    int speedLimitKbps = 0,
    int maxConcurrent = 0,
    String defaultSaveDir = '',
    int defaultSegments = 0,
    String defaultUserAgent = '',
  }) {
    logInfo(
      _tag,
      'createQueue: name=$name, speedLimit=$speedLimitKbps, maxConcurrent=$maxConcurrent, defaultSegments=$defaultSegments, ua=${defaultUserAgent.isEmpty ? "(global)" : defaultUserAgent.substring(0, defaultUserAgent.length.clamp(0, 20))}',
    );
    CreateQueue(
      name: name,
      speedLimitKbps: speedLimitKbps,
      maxConcurrent: maxConcurrent,
      defaultSaveDir: defaultSaveDir,
      defaultSegments: defaultSegments,
      defaultUserAgent: defaultUserAgent,
    ).sendSignalToRust();
  }

  void updateQueue({
    required String queueId,
    required String name,
    int speedLimitKbps = 0,
    int maxConcurrent = 0,
    String defaultSaveDir = '',
    int defaultSegments = 0,
    String defaultUserAgent = '',
  }) {
    logInfo(
      _tag,
      'updateQueue: id=$queueId, name=$name, defaultSegments=$defaultSegments, ua=${defaultUserAgent.isEmpty ? "(global)" : defaultUserAgent.substring(0, defaultUserAgent.length.clamp(0, 20))}',
    );
    UpdateQueue(
      queueId: queueId,
      name: name,
      speedLimitKbps: speedLimitKbps,
      maxConcurrent: maxConcurrent,
      defaultSaveDir: defaultSaveDir,
      defaultSegments: defaultSegments,
      defaultUserAgent: defaultUserAgent,
    ).sendSignalToRust();
  }

  void deleteQueue(String queueId) {
    logInfo(_tag, 'deleteQueue: id=$queueId');
    DeleteQueue(queueId: queueId).sendSignalToRust();
    // 如果当前正在筛选该队列，取消筛选
    if (_queueFilter == queueId) {
      _queueFilter = null;
      _safeNotifyListeners();
    }
  }

  void moveTaskToQueue(String taskId, String queueId) {
    logInfo(_tag, 'moveTaskToQueue: task=$taskId, queue=$queueId');
    MoveTaskToQueue(taskId: taskId, queueId: queueId).sendSignalToRust();
  }

  /// 设置或取消优先下载任务（Boost 模式）。
  /// 传入当前优先任务 ID 则切换（取消），传入其他 ID 则设置为新优先任务。
  ///
  /// 在发送信号给 Rust 之前先做**乐观 UI 更新**：
  /// 一次性将所有受影响任务设置到目标状态，避免 Rust 分批处理信号导致的抖动。
  void setPriorityTask(String taskId) {
    logInfo(_tag, 'setPriorityTask: $taskId');
    final isCancel = taskId.isEmpty || taskId == _priorityTaskId;

    // 先清除上一轮 boost 守卫（无论激活新任务还是取消都需要重置）
    for (final id in _boostAutoPausedIds) {
      _optimisticPausedIds.remove(id);
    }
    _boostAutoPausedIds.clear();

    if (isCancel) {
      // 乐观取消：重置 boost 状态，后续 Rust resume 信号将还原各任务 UI
      _priorityTaskId = '';
      _boostAutoPausedCount = 0;
    } else {
      // 乐观激活：立即将所有活跃/排队任务（除目标外）设为 paused，
      // 避免 Rust 分批处理期间 UI 多次重建抖动，
      // 同时为后续乱序到达的 status=1 信号建立守卫。
      for (int i = 0; i < _tasks.length; i++) {
        final t = _tasks[i];
        if (t.id == taskId) continue;
        if (!t.status.isActiveOrQueued) continue;
        _boostAutoPausedIds.add(t.id);
        _optimisticPausedIds.add(t.id);
        _tasks[i] = t.copyWith(status: TaskStatus.paused, speed: 0);
      }
      _priorityTaskId = taskId;
      // _boostAutoPausedCount 以 Rust 确认值为准，由 _onPriorityTaskChanged 更新
    }

    _safeNotifyListeners();
    SetPriorityTask(taskId: isCancel ? '' : taskId).sendSignalToRust();
  }

  /// 取消 Boost 模式
  void cancelBoost() {
    logInfo(_tag, 'cancelBoost');
    setPriorityTask('');
  }

  /// 批量暂停所有活跃任务（单次 IPC）
  void pauseAll() {
    logInfo(_tag, 'pauseAll');
    _deferredResumeQueue.clear();
    final toPause = <String>[];
    for (int i = 0; i < _tasks.length; i++) {
      final t = _tasks[i];
      if (t.status == TaskStatus.downloading ||
          t.status == TaskStatus.resuming ||
          t.status == TaskStatus.pending ||
          t.status == TaskStatus.preparing ||
          // 修复：boost 结束后，部分任务在 Rust 侧已入 pending_queue，
          // 但 Dart 侧仍显示 paused（未收到 status=0 信号）。
          // queuePosition > 0 说明任务确实在 Rust 的队列里等待启动。
          (t.status == TaskStatus.paused && t.queuePosition > 0)) {
        toPause.add(t.id);
        _optimisticPausedIds.add(t.id);
        // 乐观 UI 更新
        if (t.status != TaskStatus.paused) {
          _tasks[i] = t.copyWith(status: TaskStatus.paused, speed: 0);
        }
      }
    }
    if (toPause.isEmpty) return;
    BatchControlTask(taskIds: toPause, action: 0).sendSignalToRust();
    _safeNotifyListeners();
  }

  /// 恢复所有暂停/出错的任务。
  ///
  /// 将全部候选任务一次性发送给 Rust 的 DownloadManager，由其
  /// pending_queue 统一管理并发限制，避免 Dart 侧与 Rust 侧双重
  /// 并发控制产生的竞态（重复恢复、槽位计算不一致等问题）。
  /// Dart 侧仅做乐观 UI 更新，将所有候选任务立即显示为 resuming。
  void resumeAll() {
    logInfo(_tag, 'resumeAll');
    _deferredResumeQueue.clear();

    final candidates = <String>[];
    for (int i = 0; i < _tasks.length; i++) {
      final t = _tasks[i];
      if (t.status == TaskStatus.paused || t.status == TaskStatus.error) {
        candidates.add(t.id);
        _boostAutoPausedIds.remove(t.id);
        _optimisticPausedIds.remove(t.id);
        _tasks[i] = t.copyWith(status: TaskStatus.resuming);
      }
    }
    if (candidates.isEmpty) return;
    BatchControlTask(taskIds: candidates, action: 1).sendSignalToRust();
    _safeNotifyListeners();
  }

  /// 从延迟恢复队列中取出下一个任务并发送 resume。
  /// 当活跃任务完成/出错释放槽位时调用。
  void _resumeNextDeferred() {
    while (_deferredResumeQueue.isNotEmpty) {
      final nextId = _deferredResumeQueue.removeAt(0);
      final idx = _tasks.indexWhere((t) => t.id == nextId);
      // 跳过已删除、已完成或已在下载的任务
      if (idx < 0) continue;
      final t = _tasks[idx];
      if (t.status != TaskStatus.paused &&
          t.status != TaskStatus.error &&
          t.status != TaskStatus.pending) {
        continue;
      }
      // 发送单个 resume
      _optimisticPausedIds.remove(nextId);
      _tasks[idx] = t.copyWith(status: TaskStatus.resuming);
      ControlTask(taskId: nextId, action: 1).sendSignalToRust();
      _safeNotifyListeners();
      return;
    }
  }

  /// 默认下载目录
  static String get defaultSaveDir {
    // Windows: C:\Users\<user>\Downloads
    // macOS/Linux: ~/Downloads
    final home =
        Platform.environment['USERPROFILE'] ??
        Platform.environment['HOME'] ??
        '.';
    return '$home${Platform.pathSeparator}Downloads';
  }

  // ---------------------------------------------------------------------------
  // Signal listeners
  // ---------------------------------------------------------------------------

  void _startListening() {
    _allTasksSub = AllTasks.rustSignalStream.listen(_onAllTasks);
    _progressSub = TaskProgress.rustSignalStream.listen(_onProgress);
    _segmentSub = SegmentProgress.rustSignalStream.listen(_onSegmentProgress);
    _splitSub = SegmentSplitEvent.rustSignalStream.listen(_onSplitEvent);
    _metaProbedSub = TaskMetaProbed.rustSignalStream.listen(_onTaskMetaProbed);
    _queuePosSub = QueuePositionsUpdate.rustSignalStream.listen(
      _onQueuePositionsUpdate,
    );
    _allQueuesSub = AllQueues.rustSignalStream.listen(_onAllQueues);
    _prioritySub = PriorityTaskChanged.rustSignalStream.listen(
      _onPriorityTaskChanged,
    );
    _fileMissingSub = FileMissingChanged.rustSignalStream.listen(
      _onFileMissingChanged,
    );
  }

  void _onAllTasks(RustSignalPack<AllTasks> pack) {
    if (_disposed) {
      logInfo(_tag, '_onAllTasks skipped (disposed)');
      return;
    }
    final incoming = pack.message.tasks;
    logInfo(_tag, '_onAllTasks: received ${incoming.length} tasks');

    // Rust 每次创建任务后都会推送 AllTasks（download_actor.rs:230/244/390），
    // 不只是在启动时。若此时批量删除进行中，不能无条件清空 _deletedTaskIds：
    //
    //   1. _pendingDeleteIds 里的 ID 仍在等待 Rust 的删除确认；若守卫被清除，
    //      后续到来的确认信号会落入普通 _onProgress 路径，任务以 error 状态
    //      被「僵尸复活」到列表，且 _pendingDeleteIds 永远无法清空。
    //   2. Rust DB 中还没来得及删除的任务仍在 AllTasks 里，需保留守卫以防止
    //      _onAllTasks 本身把它们重新加回 _tasks（二次僵尸复活）。
    //
    // 修复：只移除已被 Rust 确认彻底删除（不在 DB 中）且不在批量追踪中的 ID。
    final incomingIds = {for (final t in incoming) t.taskId};
    _deletedTaskIds.removeWhere(
      (id) => !incomingIds.contains(id) && !_pendingDeleteIds.contains(id),
    );

    _tasks.clear();
    for (final info in incoming) {
      // 跳过仍在删除中的任务，防止 AllTasks 把它们重新插回列表（僵尸复活）。
      if (_deletedTaskIds.contains(info.taskId)) continue;
      var task = DownloadTask.fromTaskInfo(info);
      // 若用户已乐观暂停该任务，DB 的旧状态（downloading/resuming 等）不得覆盖 UI。
      // 避免 AllTasks 到达时 DB 尚未写入 paused，导致任务被还原成下载中，
      // 随后守卫又因 old=downloading != paused 而失效。
      if (_optimisticPausedIds.contains(info.taskId) &&
          task.status != TaskStatus.paused) {
        task = task.copyWith(status: TaskStatus.paused, speed: 0);
      }
      _tasks.add(task);
    }
    _safeNotifyListeners();
  }

  /// 文件跟踪：引擎扫描后定向更新受影响任务的 fileMissing 标志。只 copyWith
  /// 单个字段、不重建整表，避免活跃下载 UI 闪烁。沿用 _deletedTaskIds 守卫
  /// 防止对已删除任务的残余更新。
  void _onFileMissingChanged(RustSignalPack<FileMissingChanged> pack) {
    if (_disposed) return;
    var changed = false;
    for (final u in pack.message.updates) {
      if (_deletedTaskIds.contains(u.taskId)) continue;
      final idx = _tasks.indexWhere((t) => t.id == u.taskId);
      if (idx >= 0 && _tasks[idx].fileMissing != u.missing) {
        _tasks[idx] = _tasks[idx].copyWith(fileMissing: u.missing);
        changed = true;
      }
    }
    if (changed) _safeNotifyListeners();
  }

  void _onProgress(RustSignalPack<TaskProgress> pack) {
    if (_disposed) return;
    final p = pack.message;
    // 忽略已删除任务的残余信号，防止「僵尸复活」
    // 但在此之前先拦截批量删除确认信号以更新进度
    if (_deletedTaskIds.contains(p.taskId)) {
      if (_pendingDeleteIds.contains(p.taskId) &&
          p.status == 4 &&
          p.errorMessage == 'deleted') {
        _pendingDeleteIds.remove(p.taskId);
        _batchDeleteDone++;
        final isDone = _pendingDeleteIds.isEmpty;
        if (isDone) {
          // 全部删除完成，重置计数（保留 total 供 UI 短暂显示最终值）
          Future.delayed(const Duration(milliseconds: 800), () {
            if (!_disposed) {
              _batchDeleteTotal = 0;
              _batchDeleteDone = 0;
              _safeNotifyListeners();
            }
          });
        }
        // 上万级删除时每次确认都重绘开销过大：按 ~1% 步长节流，完成时强制刷新。
        // step = max(1, total / 100)，保证最多触发 ~100 次重建，与批次大小无关。
        final step = (_batchDeleteTotal / 100).ceil().clamp(
          1,
          _batchDeleteTotal,
        );
        if (isDone || _batchDeleteDone % step == 0) {
          _safeNotifyListeners();
        }
      }
      return;
    }
    final newStatus = taskStatusFromInt(p.status);
    final idx = _tasks.indexWhere((t) => t.id == p.taskId);
    if (idx >= 0) {
      final oldStatus = _tasks[idx].status;
      // 守卫逻辑：防止 Rust 积压/提前到达的信号覆盖乐观 UI 状态。
      // 对在 _optimisticPausedIds 中且当前 UI 为 paused 或 pending（延迟队列）的任务生效。
      if (_optimisticPausedIds.contains(p.taskId) &&
          (oldStatus == TaskStatus.paused || oldStatus == TaskStatus.pending)) {
        if (newStatus == TaskStatus.downloading ||
            newStatus == TaskStatus.preparing ||
            newStatus == TaskStatus.pending) {
          // 该任务仍在守卫中（未被 resumeAll 立即恢复），拦截所有活跃状态信号。
          // 延迟恢复队列会在合适时机移除守卫并发送 resume。
          return;
        }
        // paused/pending → paused / error / completed 等其他状态：不干预，直接放行。
      }
      _tasks[idx] = _tasks[idx].applyProgress(p);
      // 任务离开 downloading 状态时清空 recentSplits，避免内存泄漏
      if (oldStatus == TaskStatus.downloading &&
          newStatus != TaskStatus.downloading &&
          _tasks[idx].recentSplits.isNotEmpty) {
        _tasks[idx] = _tasks[idx].copyWith(recentSplits: const []);
      }
      // 检测下载完成：从非 completed 状态变为 completed
      if (oldStatus != TaskStatus.completed &&
          newStatus == TaskStatus.completed) {
        logInfo(_tag, 'task completed: ${p.taskId} (${p.fileName})');
        onTaskCompleted?.call(_tasks[idx]);
        // 释放槽位，从延迟队列恢复下一个任务
        _resumeNextDeferred();
      }
      // 检测下载失败：从非 error 状态变为 error
      if (oldStatus != TaskStatus.error && newStatus == TaskStatus.error) {
        // 释放槽位，从延迟队列恢复下一个任务
        _resumeNextDeferred();
      }
    } else {
      // 新任务（刚刚创建的）
      logInfo(_tag, 'new task from progress: ${p.taskId} status=$newStatus');
      final task = DownloadTask(
        id: p.taskId,
        url: p.url,
        fileName: p.fileName.isEmpty ? currentS.unknownFile : p.fileName,
        saveDir: p.saveDir,
        status: newStatus,
        downloadedBytes: p.downloadedBytes,
        totalBytes: p.totalBytes,
        speed: p.speed,
        errorMessage: p.errorMessage,
      );
      _tasks.insert(0, task);
      // 新任务直接以 completed 状态出现（如瞬间完成的小文件）
      if (newStatus == TaskStatus.completed) {
        logInfo(_tag, 'new task instantly completed: ${p.taskId}');
        onTaskCompleted?.call(task);
      }
    }
    _safeNotifyListeners();
  }

  void _onSegmentProgress(RustSignalPack<SegmentProgress> pack) {
    if (_disposed) return;
    final sp = pack.message;
    if (_deletedTaskIds.contains(sp.taskId)) return;
    final idx = _tasks.indexWhere((t) => t.id == sp.taskId);
    if (idx < 0) return;

    final segments = sp.segments
        .map(
          (s) => SegmentData(
            index: s.index,
            startByte: s.startByte,
            endByte: s.endByte,
            downloadedBytes: s.downloadedBytes,
          ),
        )
        .toList();

    _tasks[idx] = _tasks[idx].copyWith(segments: segments);
    _safeNotifyListeners();
  }

  /// Maximum number of split events to keep per task (ring buffer).
  static const _maxSplitEvents = 20;

  void _onSplitEvent(RustSignalPack<SegmentSplitEvent> pack) {
    if (_disposed) return;
    final evt = pack.message;
    if (_deletedTaskIds.contains(evt.taskId)) return;
    final idx = _tasks.indexWhere((t) => t.id == evt.taskId);
    if (idx < 0) return;

    // 仅在下载中状态时记录拆分事件
    if (_tasks[idx].status != TaskStatus.downloading) return;

    final splitData = SplitEventData(
      parentIndex: evt.parentIndex,
      parentNewEnd: evt.parentNewEnd,
      childIndex: evt.childIndex,
      childStart: evt.childStart,
      childEnd: evt.childEnd,
      isProactive: evt.isProactive,
      totalSegments: evt.totalSegments,
    );

    // Keep only the most recent split events.
    final current = List<SplitEventData>.from(_tasks[idx].recentSplits);
    current.add(splitData);
    if (current.length > _maxSplitEvents) {
      current.removeRange(0, current.length - _maxSplitEvents);
    }

    _tasks[idx] = _tasks[idx].copyWith(recentSplits: current);
    logInfo(
      _tag,
      'split event: task=${evt.taskId}, '
      'parent=#${evt.parentIndex}→end=${evt.parentNewEnd}, '
      'child=#${evt.childIndex} [${evt.childStart}, ${evt.childEnd}], '
      'proactive=${evt.isProactive}, total=${evt.totalSegments}',
    );
    _safeNotifyListeners();
  }

  void _onTaskMetaProbed(RustSignalPack<TaskMetaProbed> pack) {
    if (_disposed) return;
    final p = pack.message;
    if (_deletedTaskIds.contains(p.taskId)) return;
    final idx = _tasks.indexWhere((t) => t.id == p.taskId);
    if (idx < 0) return;

    final task = _tasks[idx];

    // Guard: if the task already has a confirmed file name (set by the user
    // in the dialog or resolved by the download engine via TaskProgress),
    // do NOT let the background meta-probe overwrite it.
    //
    // This prevents the race where:
    //   1. User sets a custom name → TaskProgress confirms it (fileNameConfirmed=true)
    //   2. pending-queue probe finishes → sends TaskMetaProbed with server name
    //   3. Without this guard the UI name would flip to the server name while
    //      the actual file on disk uses the user's name → mismatch.
    //
    // Rust already guards the DB side (update_task_file_name only writes when
    // file_name is empty), and all probe paths (HTTP/FTP/magnet) return an
    // empty name when file_name is non-empty — so p.fileName should already
    // be empty here whenever fileNameConfirmed is true.  This is a second
    // line of defence in case a future code path forgets that contract.
    final acceptFileName = p.fileName.isNotEmpty && !task.fileNameConfirmed;

    _tasks[idx] = task.copyWith(
      fileName: acceptFileName ? p.fileName : null,
      totalBytes: p.totalBytes > 0 ? p.totalBytes : null,
      // If we accepted a probe-supplied name, mark it confirmed so a second
      // probe signal (unlikely but possible) doesn't keep flipping the name.
      fileNameConfirmed: acceptFileName ? true : null,
    );
    _safeNotifyListeners();
  }

  void _onQueuePositionsUpdate(RustSignalPack<QueuePositionsUpdate> pack) {
    if (_disposed) return;
    final posMap = {
      for (final p in pack.message.positions) p.taskId: p.position,
    };
    bool changed = false;
    for (int i = 0; i < _tasks.length; i++) {
      final newPos = posMap[_tasks[i].id] ?? -1;
      if (_tasks[i].queuePosition != newPos) {
        _tasks[i] = _tasks[i].copyWith(queuePosition: newPos);
        changed = true;
      }
    }
    if (changed) _safeNotifyListeners();
  }

  void _onAllQueues(RustSignalPack<AllQueues> pack) {
    if (_disposed) return;
    final incoming = pack.message.queues;
    logInfo(_tag, '_onAllQueues: ${incoming.length} queues');
    _queues = incoming.map(DownloadQueue.fromQueueInfo).toList()
      ..sort((a, b) => a.position.compareTo(b.position));
    // 如果当前筛选的队列已被删除，取消筛选
    if (_queueFilter != null &&
        _queueFilter!.isNotEmpty &&
        !_queues.any((q) => q.queueId == _queueFilter)) {
      _queueFilter = null;
    }
    _safeNotifyListeners();
  }

  void _onPriorityTaskChanged(RustSignalPack<PriorityTaskChanged> pack) {
    if (_disposed) return;
    final p = pack.message;
    logInfo(
      _tag,
      '_onPriorityTaskChanged: priority=${p.priorityTaskId}, autoPaused=${p.autoPausedCount}',
    );

    if (p.priorityTaskId.isNotEmpty) {
      // Boost 激活确认：守卫和乐观 UI 更新已由 setPriorityTask() 完成，
      // 此处不能清空 _boostAutoPausedIds（否则守卫失效，乱序 status=1 会覆盖 UI）。
      // 仅以 Rust 权威值更新优先任务 ID 和暂停数量。
      _priorityTaskId = p.priorityTaskId;
      _boostAutoPausedCount = p.autoPausedCount;
    } else {
      // Boost 取消（Rust 侧触发：优先任务完成、被手动暂停或删除等）。
      // 若 Dart 侧已通过 setPriorityTask('') 提前清理，_boostAutoPausedIds 为空，此处为 no-op。
      for (final id in _boostAutoPausedIds) {
        _optimisticPausedIds.remove(id);
      }
      _boostAutoPausedIds.clear();
      _priorityTaskId = '';
      _boostAutoPausedCount = 0;
    }

    _safeNotifyListeners();
  }
}
