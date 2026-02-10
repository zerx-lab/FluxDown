import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
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

  /// 下载完成回调 — 当任务状态从非 completed 变为 completed 时触发
  void Function(DownloadTask task)? onTaskCompleted;

  StreamSubscription<RustSignalPack<TaskProgress>>? _progressSub;
  StreamSubscription<RustSignalPack<AllTasks>>? _allTasksSub;
  StreamSubscription<RustSignalPack<SegmentProgress>>? _segmentSub;

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
    super.dispose();
    logInfo(_tag, 'dispose done');
  }

  /// 安全的 notifyListeners — dispose 后不再通知，避免
  /// "A DownloadController was used after being disposed" 异常
  void _safeNotifyListeners() {
    if (!_disposed) notifyListeners();
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
  }) {
    logInfo(
      _tag,
      'createTask: url=$url, dir=$saveDir, file=$fileName, seg=$segments, cookies_len=${cookies.length}',
    );
    CreateTask(
      url: url,
      saveDir: saveDir,
      fileName: fileName,
      segments: segments,
      cookies: cookies,
    ).sendSignalToRust();
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
  }

  void _onAllTasks(RustSignalPack<AllTasks> pack) {
    if (_disposed) {
      logInfo(_tag, '_onAllTasks skipped (disposed)');
      return;
    }
    final incoming = pack.message.tasks;
    logInfo(_tag, '_onAllTasks: received ${incoming.length} tasks');
    _tasks.clear();
    for (final info in incoming) {
      _tasks.add(DownloadTask.fromTaskInfo(info));
    }
    _safeNotifyListeners();
  }

  void _onProgress(RustSignalPack<TaskProgress> pack) {
    if (_disposed) return;
    final p = pack.message;
    final newStatus = taskStatusFromInt(p.status);
    final idx = _tasks.indexWhere((t) => t.id == p.taskId);
    if (idx >= 0) {
      final oldStatus = _tasks[idx].status;
      _tasks[idx] = _tasks[idx].applyProgress(p);
      // 检测下载完成：从非 completed 状态变为 completed
      if (oldStatus != TaskStatus.completed &&
          newStatus == TaskStatus.completed) {
        logInfo(_tag, 'task completed: ${p.taskId} (${p.fileName})');
        onTaskCompleted?.call(_tasks[idx]);
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
}
