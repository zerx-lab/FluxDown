import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import '../services/analytics_service.dart';
import '../services/log_service.dart';
import 'download_task.dart';

const _tag = 'DownloadCtrl';

/// 顶部 Tab 状态筛选
enum StatusTab { all, downloading, completed, paused, error }

/// 核心状态管理器 — 桥接 Rust 信号和 Flutter UI
class DownloadController extends ChangeNotifier {
  final List<DownloadTask> _tasks = [];
  String? _selectedTaskId;
  FileCategory _categoryFilter = FileCategory.all;
  StatusTab _statusTab = StatusTab.all;

  // 管理模式（多选）
  bool _isManageMode = false;
  final Set<String> _checkedTaskIds = {};

  // 时间分组折叠状态（key 为 TimeGroup，value 为是否折叠）
  final Map<TimeGroup, bool> _collapsedGroups = {};

  /// 已删除任务 ID 集合 — 防止 Rust 残余信号将已删除任务「复活」到列表中。
  /// 在 _onAllTasks 刷新时清空（Rust 端不再包含该任务即可安全移除）。
  final Set<String> _deletedTaskIds = {};

  /// 下载完成回调 — 当任务状态从非 completed 变为 completed 时触发
  void Function(DownloadTask task)? onTaskCompleted;

  StreamSubscription<RustSignalPack<TaskProgress>>? _progressSub;
  StreamSubscription<RustSignalPack<AllTasks>>? _allTasksSub;
  StreamSubscription<RustSignalPack<SegmentProgress>>? _segmentSub;
  StreamSubscription<RustSignalPack<SegmentSplitEvent>>? _splitSub;

  bool _disposed = false;

  DownloadController() {
    logInfo(_tag, 'constructor — starting listeners');
    _startListening();
    // 启动时请求所有持久化任务
    const RequestAllTasks().sendSignalToRust();
  }

  @override
  void dispose() {
    logInfo(_tag, 'dispose called');
    _disposed = true;
    _progressSub?.cancel();
    _allTasksSub?.cancel();
    _segmentSub?.cancel();
    _splitSub?.cancel();
    super.dispose();
    logInfo(_tag, 'dispose done');
  }

  /// 安全的 notifyListeners — dispose 后不再通知，避免
  /// "A DownloadController was used after being disposed" 异常
  void _safeNotifyListeners() {
    if (!_disposed) notifyListeners();
  }

  /// 从 URL 推断下载协议（用于分析埋点）
  static String _inferProtocol(String url) {
    if (url.isEmpty) return 'bt';
    final lower = url.toLowerCase();
    if (lower.startsWith('ftp')) return 'ftp';
    if (lower.startsWith('magnet:')) return 'bt';
    return 'http';
  }

  /// 将错误消息分类为匿名类别，避免将 URL/路径等敏感信息发送到分析服务。
  static String _classifyError(String msg) {
    if (msg.isEmpty) return 'unknown';
    final lower = msg.toLowerCase();
    if (lower.contains('timeout') || lower.contains('timed out')) {
      return 'timeout';
    }
    if (lower.contains('connection') || lower.contains('network')) {
      return 'network';
    }
    if (lower.contains('disk') ||
        lower.contains('space') ||
        lower.contains('permission') ||
        lower.contains('access')) {
      return 'disk';
    }
    if (lower.contains('404') || lower.contains('not found')) {
      return 'not_found';
    }
    if (lower.contains('403') || lower.contains('forbidden')) {
      return 'forbidden';
    }
    if (lower.contains('ssl') || lower.contains('certificate')) {
      return 'ssl';
    }
    if (lower.contains('cancel')) return 'cancelled';
    return 'other';
  }

  // ---------------------------------------------------------------------------
  // Public getters
  // ---------------------------------------------------------------------------

  List<DownloadTask> get tasks => _tasks;

  FileCategory get categoryFilter => _categoryFilter;
  StatusTab get statusTab => _statusTab;

  /// 按文件类型过滤（侧边栏维度）
  List<DownloadTask> get _categoryFiltered {
    if (_categoryFilter == FileCategory.all) return _tasks;
    return _tasks.where((t) => t.fileCategory == _categoryFilter).toList();
  }

  /// 双维度组合过滤后的任务列表（侧边栏文件类型 + 顶部状态 Tab）
  List<DownloadTask> get filteredTasks {
    final byCategory = _categoryFiltered;
    return switch (_statusTab) {
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
  }

  /// 将 filteredTasks 按时间分组（保持组内顺序不变）
  List<TaskGroup> get groupedTasks {
    final tasks = filteredTasks;
    final Map<TimeGroup, List<DownloadTask>> map = {};
    for (final task in tasks) {
      final group = TimeGroup.fromDateTime(task.createdAt);
      (map[group] ??= []).add(task);
    }
    // 按 TimeGroup 枚举顺序排列（today → older）
    final result = <TaskGroup>[];
    for (final g in TimeGroup.values) {
      final list = map[g];
      if (list != null && list.isNotEmpty) {
        result.add(TaskGroup(group: g, tasks: list));
      }
    }
    return result;
  }

  /// 某个时间分组是否折叠
  bool isGroupCollapsed(TimeGroup group) => _collapsedGroups[group] ?? false;

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

  /// 各文件类型分类的任务数量（用于侧边栏显示计数）
  int countForCategory(FileCategory category) {
    if (category == FileCategory.all) return _tasks.length;
    return _tasks.where((t) => t.fileCategory == category).length;
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

  /// 批量删除选中的任务
  void deleteCheckedTasks({required bool deleteFiles}) {
    final ids = _checkedTaskIds.toList();
    logInfo(
      _tag,
      'deleteCheckedTasks: ${ids.length} tasks, deleteFiles=$deleteFiles',
    );
    for (final id in ids) {
      final action = deleteFiles ? 3 : 4;
      ControlTask(taskId: id, action: action).sendSignalToRust();
      _deletedTaskIds.add(id);
      _tasks.removeWhere((t) => t.id == id);
      if (_selectedTaskId == id) _selectedTaskId = null;
    }
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
  }) {
    logInfo(
      _tag,
      'createTask: url=$url, dir=$saveDir, file=$fileName, seg=$segments, cookies_len=${cookies.length}, torrent_bytes=${torrentFileBytes?.length ?? 0}',
    );
    CreateTask(
      url: url,
      saveDir: saveDir,
      fileName: fileName,
      segments: segments,
      cookies: cookies,
      torrentFileBytes: torrentFileBytes ?? Uint8List(0),
      proxyUrl: proxyUrl,
    ).sendSignalToRust();
    // 分析埋点
    final protocol = (torrentFileBytes != null && torrentFileBytes.isNotEmpty)
        ? 'bt'
        : _inferProtocol(url);
    AnalyticsService.instance.trackDownloadCreated(protocol);
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
  static Future<void> sendTorrentFileSignal(
    String torrentFilePath,
    String saveDir, {
    String proxyUrl = '',
  }) async {
    try {
      final file = File(torrentFilePath);
      final bytes = await file.readAsBytes();
      if (bytes.isEmpty) {
        logInfo(_tag, 'torrent file is empty: $torrentFilePath');
        return;
      }
      // Use the .torrent file name (without extension) as the initial display name.
      final baseName = file.uri.pathSegments.last;
      final displayName = baseName.endsWith('.torrent')
          ? baseName.substring(0, baseName.length - 8)
          : baseName;

      CreateTask(
        url: '',
        saveDir: saveDir,
        fileName: displayName,
        segments: 0,
        cookies: '',
        torrentFileBytes: bytes,
        proxyUrl: proxyUrl,
      ).sendSignalToRust();
      AnalyticsService.instance.trackDownloadCreated('bt');
    } catch (e) {
      logInfo(_tag, 'failed to read torrent file: $e');
    }
  }

  /// 批量创建下载任务（多个 URL 共享同一保存目录和线程数）
  void batchCreateTask({
    required List<String> urls,
    required String saveDir,
    int segments = 0,
    String proxyUrl = '',
  }) {
    logInfo(
      _tag,
      'batchCreateTask: ${urls.length} urls, dir=$saveDir, seg=$segments',
    );
    BatchCreateTask(
      urls: urls,
      saveDir: saveDir,
      segments: segments,
      proxyUrl: proxyUrl,
    ).sendSignalToRust();
    for (final url in urls) {
      final protocol = url.toLowerCase().startsWith('ftp') ? 'ftp' : 'http';
      AnalyticsService.instance.trackDownloadCreated(protocol);
    }
  }

  void pauseTask(String taskId) {
    logInfo(_tag, 'pauseTask: $taskId');
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
    if (_categoryFilter == category) return;
    _categoryFilter = category;
    _safeNotifyListeners();
  }

  void setStatusTab(StatusTab tab) {
    if (_statusTab == tab) return;
    _statusTab = tab;
    _safeNotifyListeners();
  }

  void pauseAll() {
    logInfo(_tag, 'pauseAll');
    for (final t in _tasks) {
      if (t.status == TaskStatus.downloading ||
          t.status == TaskStatus.resuming ||
          t.status == TaskStatus.pending ||
          t.status == TaskStatus.preparing) {
        pauseTask(t.id);
      }
    }
  }

  void resumeAll() {
    logInfo(_tag, 'resumeAll');
    for (final t in _tasks) {
      if (t.status == TaskStatus.paused || t.status == TaskStatus.error) {
        resumeTask(t.id);
      }
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
  }

  void _onAllTasks(RustSignalPack<AllTasks> pack) {
    if (_disposed) {
      logInfo(_tag, '_onAllTasks skipped (disposed)');
      return;
    }
    final incoming = pack.message.tasks;
    logInfo(_tag, '_onAllTasks: received ${incoming.length} tasks');
    _deletedTaskIds.clear(); // 全量刷新后旧的删除标记不再需要
    _tasks.clear();
    for (final info in incoming) {
      _tasks.add(DownloadTask.fromTaskInfo(info));
    }
    _safeNotifyListeners();
  }

  void _onProgress(RustSignalPack<TaskProgress> pack) {
    if (_disposed) return;
    final p = pack.message;
    // 忽略已删除任务的残余信号，防止「僵尸复活」
    if (_deletedTaskIds.contains(p.taskId)) return;
    final newStatus = taskStatusFromInt(p.status);
    final idx = _tasks.indexWhere((t) => t.id == p.taskId);
    if (idx >= 0) {
      final oldStatus = _tasks[idx].status;
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
        final proto = _inferProtocol(_tasks[idx].url);
        AnalyticsService.instance.trackDownloadCompleted(proto, p.totalBytes);
      }
      // 检测下载失败：从非 error 状态变为 error
      if (oldStatus != TaskStatus.error && newStatus == TaskStatus.error) {
        AnalyticsService.instance.trackDownloadFailed(
          _inferProtocol(_tasks[idx].url),
          _classifyError(p.errorMessage),
        );
      }
    } else {
      // 新任务（刚刚创建的）
      logInfo(_tag, 'new task from progress: ${p.taskId} status=$newStatus');
      final task = DownloadTask(
        id: p.taskId,
        url: p.url,
        fileName: p.fileName.isEmpty ? '未知文件' : p.fileName,
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
}
