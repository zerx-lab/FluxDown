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
import 'list_entity.dart';
import 'task_group.dart';
import 'view_prefs.dart';

const _tag = 'DownloadCtrl';

/// 顶部 Tab 状态筛选
enum StatusTab { all, downloading, completed, paused, error }

/// 核心状态管理器 — 桥接 Rust 信号和 Flutter UI
class DownloadController extends ChangeNotifier {
  /// 全局单例引用，供无 context 场景（如 ExternalDownloadService）读取队列信息
  static DownloadController? globalInstance;
  final List<DownloadTask> _tasks = [];
  String? _selectedTaskId;
  /// 当前选中的任务组 ID（与 [_selectedTaskId] 互斥，见 [selectGroup]/
  /// [selectTask]）。
  String? _selectedGroupId;
  FileCategory _categoryFilter = FileCategory.all;
  CustomCategory? _customCategoryFilter;

  /// 当前可见的非特殊分类列表（用于计算 "other" 排除逻辑）
  List<CustomCategory> _visibleNormalCategories = [];
  StatusTab _statusTab = StatusTab.all;

  /// 命名队列列表（来自 Rust AllQueues 信号）
  List<DownloadQueue> _queues = [];

  /// 当前队列筛选 ID：null = 不过滤（显示全部），'' = 默认队列，非空 = 指定命名队列
  String? _queueFilter;

  /// 当前设备筛选 ID：null = 全部设备，'' = 本机，非空 = 远程设备 deviceId
  String? _deviceFilter;

  /// 远程设备任务快照（经 FluxCloud 回流，由 RemoteTaskService.updateRemoteTasks 注入）
  List<DownloadTask> _remoteTasks = const [];

  // 缓存 — 避免 filteredTasks / groupedTasks 每次访问重新计算
  List<DownloadTask>? _cachedFilteredTasks;
  List<TaskGroup>? _cachedGroupedTasks;

  // 视图系统（desktop）— buildListSections() 结果 memo：仅当 ViewPrefs
  // 或底层 filteredTasks 引用变化时才重新分桶排序。
  ViewPrefs? _lastSectionsPrefs;
  List<DownloadTask>? _lastSectionsSourceTasks;
  List<ListSection>? _lastSections;

  // 任务组（桌面 UI）— AllGroups 快照 + 展开/折叠运行态。
  final Map<String, DownloadGroup> _groups = {};

  /// 已展开的组 ID 集合（design-proto-spec §1 `state.expanded`）。
  final Set<String> _expandedGroupIds = {};

  /// 已折叠的组内目录集合，键为 `<groupId>:<dirPath>`
  /// （design-proto-spec §1 `state.collapsedDirs`）。
  final Set<String> _collapsedDirKeys = {};

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

  /// 修改线程数结果回调 —— 收到 Rust 的 [TaskSegmentsUpdated] 后触发。
  /// `ok=false` 表示任务正在下载/准备中/已完成而被拒绝，UI 据此提示。
  void Function(String taskId, int segments, bool ok)? onSegmentsUpdateResult;

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

  /// 插件钩子活动追踪：taskId → 正在运行的 pluginId 集合（旁路 UI 指示器，
  /// 不影响任务状态机）。集合非空即在该任务上显示「插件处理中…」。
  final Map<String, Set<String>> _pluginHookActivity = {};

  /// 插件钩子活动起始时间：taskId → 本轮首个钩子开始时刻（详情面板耗时显示）。
  final Map<String, DateTime> _pluginHookSince = {};

  /// 插件钩子活动看门狗：taskId → Timer。`running=true` 时设置/重置，
  /// 超时未收到对应 `running=false`（通知平面 fire-and-forget，事件可能
  /// 丢失）则清空该任务的活动记录，防止指示器悬挂。
  final Map<String, Timer> _pluginHookWatchdogs = {};

  /// 看门狗超时 = 钩子墙钟硬顶 1830s + 30s 余量。
  static const _pluginHookWatchdogTimeout = Duration(seconds: 1860);

  StreamSubscription<RustSignalPack<TaskProgress>>? _progressSub;
  StreamSubscription<RustSignalPack<AllTasks>>? _allTasksSub;
  StreamSubscription<RustSignalPack<SegmentProgress>>? _segmentSub;
  StreamSubscription<RustSignalPack<SegmentSplitEvent>>? _splitSub;
  StreamSubscription<RustSignalPack<TaskMetaProbed>>? _metaProbedSub;
  StreamSubscription<RustSignalPack<QueuePositionsUpdate>>? _queuePosSub;
  StreamSubscription<RustSignalPack<AllQueues>>? _allQueuesSub;
  StreamSubscription<RustSignalPack<PriorityTaskChanged>>? _prioritySub;
  StreamSubscription<RustSignalPack<FileMissingChanged>>? _fileMissingSub;
  StreamSubscription<RustSignalPack<TaskQueueChanged>>? _taskQueueChangedSub;
  StreamSubscription<RustSignalPack<PluginHookActivityEvent>>? _pluginHookSub;
  StreamSubscription<RustSignalPack<TaskSegmentsUpdated>>? _segmentsUpdatedSub;
  StreamSubscription<RustSignalPack<AllGroups>>? _allGroupsSub;

  bool _disposed = false;

  DownloadController() {
    logInfo(_tag, 'constructor — starting listeners');
    globalInstance = this;
    _startListening();
    // 启动时请求所有持久化任务和队列
    const RequestAllTasks().sendSignalToRust();
    const RequestAllQueues().sendSignalToRust();
    const RequestAllGroups().sendSignalToRust();
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
    _taskQueueChangedSub?.cancel();
    _pluginHookSub?.cancel();
    _segmentsUpdatedSub?.cancel();
    _allGroupsSub?.cancel();
    for (final timer in _pluginHookWatchdogs.values) {
      timer.cancel();
    }
    _pluginHookWatchdogs.clear();
    super.dispose();
    logInfo(_tag, 'dispose done');
  }

  /// 安全的 notifyListeners — dispose 后不再通知，避免
  /// "A DownloadController was used after being disposed" 异常
  void _safeNotifyListeners() {
    _cachedFilteredTasks = null;
    _cachedGroupedTasks = null;
    // 视图 sections memo 与 filteredTasks 缓存同生命周期失效：memo 的
    // identical(源列表) 判定在「全部」页签（filteredTasks 直通返回 _tasks
    // 本体）+ 原地改 _tasks（_onAllTasks clear+addAll / 进度 copyWith 回写）
    // 时实例恒同，不失效会吞掉一切内容变化（启动首帧空列表、进度冻结）。
    _lastSections = null;
    _lastSectionsPrefs = null;
    _lastSectionsSourceTasks = null;
    if (!_disposed) notifyListeners();
  }

  // ---------------------------------------------------------------------------
  // Public getters
  // ---------------------------------------------------------------------------

  List<DownloadTask> get tasks => _tasks;

  /// 任务列表内容区最新已知宽度（由 [TaskList] 每次布局写入，非
  /// notifyListeners 触发字段——纯粹供「显示选项」面板等非列表内部组件
  /// 估算列宽预算 [columnWidthBudget] 使用，避免为此单一需求跨 4 层组件
  /// 显式传参）。初始值取一个宽松默认，首帧布局后立即被覆盖为准确值。
  double listContentWidth = 900;

  FileCategory get categoryFilter => _categoryFilter;
  CustomCategory? get customCategoryFilter => _customCategoryFilter;
  StatusTab get statusTab => _statusTab;

  /// 命名队列列表（已按 position 排序）
  List<DownloadQueue> get queues => _queues;

  /// 当前队列筛选（null = 不过滤，'' = 默认队列，非空 = 指定命名队列）
  String? get queueFilter => _queueFilter;

  /// 当前设备筛选（null = 全部设备；'' = 本机；非空 = 远程设备 deviceId）
  String? get deviceFilter => _deviceFilter;

  /// 远程设备任务（经 FluxCloud 回流，由 RemoteTaskService 注入）
  List<DownloadTask> get remoteTasks => _remoteTasks;

  /// 本地引擎任务快照（供 RemoteTaskService 关联下发任务、上报进度）。
  List<DownloadTask> get localTasks => List.unmodifiable(_tasks);

  /// 设备维度作用域任务集（混排管线最上游）：
  /// deviceFilter==null → 本地+远程混排；''（本机）→ 本地；远程 deviceId → 该设备远程任务。
  List<DownloadTask> get _deviceScopedTasks {
    final f = _deviceFilter;
    if (f == null) {
      if (_remoteTasks.isEmpty) return _tasks;
      return [..._tasks, ..._remoteTasks];
    }
    if (f.isEmpty) return _tasks;
    return _remoteTasks.where((t) => t.deviceId == f).toList();
  }

  /// 按队列 ID 过滤（叠加在设备作用域之上）
  List<DownloadTask> get _queueFiltered {
    final base = _deviceScopedTasks;
    if (_queueFilter == null) return base;
    return base.where((t) => t.queueId == _queueFilter).toList();
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

  // ===========================================================================
  // 视图系统（desktop）— List<ListSection> 产出（契约：contract-dart.md）。
  // 复用现有 队列→分类→状态Tab→搜索 过滤管线（filteredTasks），在其上叠加
  // 「显示已完成」开关 + 分桶 + 排序；不影响 mobile 端仍在使用的
  // groupedTasks/TaskGroup（保留不动，见文件顶部 mobile_tasks_screen.dart）。
  // ===========================================================================

  /// 按 [prefs] 产出桌面任务列表视图的分桶结果（7 维分桶 × 6 键排序）。
  /// 轻量 memo：仅当 [prefs] 或底层 [filteredTasks] 引用变化时才重新计算。
  List<ListSection> buildListSections(ViewPrefs prefs) {
    final tasks = filteredTasks;
    if (_lastSections != null &&
        _lastSectionsPrefs == prefs &&
        identical(_lastSectionsSourceTasks, tasks)) {
      return _lastSections!;
    }
    final visible = prefs.showCompleted
        ? tasks
        : tasks.where((t) => t.status != TaskStatus.completed).toList();

    final partition = partitionTasksByGroup(visible, _groups.keys.toSet());
    final entities = <ListEntity>[...partition.ungrouped];
    for (final entry in partition.byGroup.entries) {
      entities.add(buildGroupEntity(_groups[entry.key]!, entry.value));
    }

    final bucketize = bucketFunctionTable(_queues)[prefs.groupBy]!;
    final bucketed = bucketize(entities);
    for (final section in bucketed) {
      section.entities.sort(
        (a, b) => compareEntities(prefs.sortKey, prefs.sortDir, a, b),
      );
    }
    final sections = orderSections(bucketed, prefs.sortKey, prefs.sortDir);

    // 展开扁平化后处理（design-proto-spec §8）：分桶/排序已完成，只在组行
    // 后面插入其成员/目录行，不改变桶结构/顶层排序。仅列表形态生效——
    // 网格无行内展开机制（§8「网格降级」），组卡点击只选中+开详情面板。
    final result = prefs.form != ViewForm.list
        ? sections
        : [
            for (final section in sections)
              ListSection(
                key: section.key,
                title: section.title,
                entities: [
                  for (final e in section.entities) ...[
                    e,
                    if (e is GroupEntity && isGroupExpanded(e.groupId))
                      ...flattenGroupMembers(
                        group: _groups[e.groupId]!,
                        members: partition.byGroup[e.groupId] ?? const [],
                        isDirCollapsed: (path) =>
                            isDirCollapsed(e.groupId, path),
                      ),
                  ],
                ],
              ),
          ];

    _lastSectionsPrefs = prefs;
    _lastSectionsSourceTasks = tasks;
    _lastSections = result;
    return result;
  }

  /// 「显示已完成」关闭时，当前页签被隐藏的已完成任务数（状态栏摘要用；
  /// 开启时恒为 0）。
  int hiddenCompletedCount(ViewPrefs prefs) {
    if (prefs.showCompleted) return 0;
    return filteredTasks
        .where((t) => t.status == TaskStatus.completed)
        .length;
  }

  /// 当前视图下可见实体的「展开」计数（组按成员数计；本波无组，等价于
  /// 可见任务数；状态栏左侧摘要用）。
  int visibleEntityExpandedCount(ViewPrefs prefs) {
    var count = 0;
    for (final section in buildListSections(prefs)) {
      for (final e in section.entities) {
        // 组展开产出的成员/目录行已经由其所属 GroupEntity 计入一次。
        if (e is GroupMemberEntity || e is GroupDirEntity) continue;
        count +=
            e is GroupEntity ? (e.members.isEmpty ? 1 : e.members.length) : 1;
      }
    }
    return count;
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

  /// 指定设备的任务数（用于侧边栏设备区计数）。'' = 本机。
  int countForDevice(String deviceId) {
    if (deviceId.isEmpty) return _tasks.length;
    return _remoteTasks.where((t) => t.deviceId == deviceId).length;
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
    bool ignoreTlsErrors = false,
    Map<String, String> extraHeaders = const {},
    List<int> selectedFileIndices = const [],
    bool startPaused = false,
  }) {
    logInfo(
      _tag,
      'createTask: url=$url, dir=$saveDir, file=$fileName, seg=$segments, cookies_len=${cookies.length}, torrent_bytes=${torrentFileBytes?.length ?? 0}, queue=$queueId, headers=${extraHeaders.length}, selected_files=${selectedFileIndices.length}, later=$startPaused',
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
      ignoreTlsErrors: ignoreTlsErrors,
      extraHeaders: extraHeaders,
      selectedFileIndices: selectedFileIndices,
      startPaused: startPaused,
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
    bool startPaused = false,
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
      ignoreTlsErrors: false,
      extraHeaders: const {},
      selectedFileIndices: selectedFileIndices,
      startPaused: startPaused,
    ).sendSignalToRust();
  }

  /// Create a download task from a .torrent file on disk.
  Future<void> createTaskFromTorrentFile({
    required String torrentFilePath,
    required String saveDir,
    String proxyUrl = '',
    bool startPaused = false,
  }) async {
    logInfo(
      _tag,
      'createTaskFromTorrentFile: path=$torrentFilePath, dir=$saveDir, later=$startPaused',
    );
    await sendTorrentFileSignal(
      torrentFilePath,
      saveDir,
      proxyUrl: proxyUrl,
      startPaused: startPaused,
    );
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
    bool startPaused = false,
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
        ignoreTlsErrors: false,
        extraHeaders: const {},
        selectedFileIndices: selectedFileIndices,
        startPaused: startPaused,
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
    bool ignoreTlsErrors = false,
    bool startPaused = false,
  }) {
    logInfo(
      _tag,
      'batchCreateTask: ${entries.length} entries, dir=$saveDir, seg=$segments, queue=$queueId, cookies_len=${cookies.length}, later=$startPaused',
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
      ignoreTlsErrors: ignoreTlsErrors,
      startPaused: startPaused,
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
    if (taskId == null) {
      if (_selectedTaskId == null) return;
      _selectedTaskId = null;
      _safeNotifyListeners();
      return;
    }
    if (_selectedTaskId == taskId && _selectedGroupId == null) return;
    // 选中任务与选中组互斥（home_page.dart 详情面板二选一展示）。
    _selectedTaskId = taskId;
    _selectedGroupId = null;
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

  /// 设置设备筛选。传入相同值则切回「全部设备」。null=全部，''=本机，非空=远程设备。
  void setDeviceFilter(String? deviceId) {
    if (_deviceFilter == deviceId) {
      _deviceFilter = null;
    } else {
      _deviceFilter = deviceId;
    }
    _safeNotifyListeners();
  }

  /// 由 RemoteTaskService 注入最新远程任务快照（进度回流驱动 UI 刷新）。
  void updateRemoteTasks(List<DownloadTask> tasks) {
    _remoteTasks = tasks;
    _safeNotifyListeners();
  }

  /// 设备名册变化时清理失效的设备筛选（筛选的远程设备已被移除 → 回到全部）。
  void pruneDeviceFilter(Set<String> knownRemoteDeviceIds) {
    final f = _deviceFilter;
    if (f != null && f.isNotEmpty && !knownRemoteDeviceIds.contains(f)) {
      _deviceFilter = null;
      _safeNotifyListeners();
    }
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

  /// 按 ID 查找队列（不存在返回 null）。
  DownloadQueue? queueById(String queueId) {
    for (final q in _queues) {
      if (q.queueId == queueId) return q;
    }
    return null;
  }

  /// 队列是否运行中。空 ID / 未知队列视作运行中（防御）。
  bool isQueueRunning(String queueId) {
    if (queueId.isEmpty) return true;
    return queueById(queueId)?.isRunning ?? true;
  }

  /// 启动队列：置运行态并按队列内顺序恢复其中所有待下载任务。
  void startQueue(String queueId) {
    logInfo(_tag, 'startQueue: $queueId');
    // 乐观更新运行态；Rust 的 AllQueues 广播随后校正。
    final idx = _queues.indexWhere((q) => q.queueId == queueId);
    if (idx >= 0 && !_queues[idx].isRunning) {
      _queues[idx] = _queues[idx].copyWith(isRunning: true);
      _safeNotifyListeners();
    }
    StartQueue(queueId: queueId).sendSignalToRust();
  }

  /// 停止队列：置停止态并暂停其中所有排队/活跃任务。
  void stopQueue(String queueId) {
    logInfo(_tag, 'stopQueue: $queueId');
    final idx = _queues.indexWhere((q) => q.queueId == queueId);
    if (idx >= 0 && _queues[idx].isRunning) {
      _queues[idx] = _queues[idx].copyWith(isRunning: false);
      _safeNotifyListeners();
    }
    StopQueue(queueId: queueId).sendSignalToRust();
  }

  /// 更新队列每日定时计划（HH:MM，空 = 该边沿不定时；days 位掩码 bit0=周一）。
  void setQueueSchedule({
    required String queueId,
    required bool enabled,
    String startTime = '',
    String stopTime = '',
    int days = 127,
  }) {
    logInfo(
      _tag,
      'setQueueSchedule: id=$queueId, enabled=$enabled, $startTime-$stopTime, days=$days',
    );
    SetQueueSchedule(
      queueId: queueId,
      enabled: enabled,
      startTime: startTime,
      stopTime: stopTime,
      days: days,
    ).sendSignalToRust();
  }

  /// 持久化队列内任务顺序（完整新顺序），并本地乐观更新 queueOrder。
  void reorderQueueTasks(String queueId, List<String> orderedIds) {
    logInfo(_tag, 'reorderQueueTasks: id=$queueId, ${orderedIds.length} tasks');
    if (orderedIds.isEmpty) return;
    final orderOf = <String, int>{
      for (var i = 0; i < orderedIds.length; i++) orderedIds[i]: i + 1,
    };
    var changed = false;
    for (var i = 0; i < _tasks.length; i++) {
      final newOrder = orderOf[_tasks[i].id];
      if (newOrder != null && _tasks[i].queueOrder != newOrder) {
        _tasks[i] = _tasks[i].copyWith(queueOrder: newOrder);
        changed = true;
      }
    }
    if (changed) _safeNotifyListeners();
    ReorderQueueTasks(queueId: queueId, taskIds: orderedIds).sendSignalToRust();
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
        // 停止队列（含「稍后下载」栈）里的任务不参与全局恢复，
        // 由「启动队列」显式恢复——与引擎侧 resume_all_eligible 语义一致。
        if (!isQueueRunning(t.queueId)) continue;
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
    _taskQueueChangedSub = TaskQueueChanged.rustSignalStream.listen(
      _onTaskQueueChanged,
    );
    _pluginHookSub = PluginHookActivityEvent.rustSignalStream.listen(
      _onPluginHookActivity,
    );
    _segmentsUpdatedSub = TaskSegmentsUpdated.rustSignalStream.listen(
      _onTaskSegmentsUpdated,
    );
    _allGroupsSub = AllGroups.rustSignalStream.listen(_onAllGroups);
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

  /// 修改某个任务的分段（线程）数。已完成任务不可改（Rust 回 ok=false）；
  /// 其余状态均可改——引擎对活跃任务自动「暂停→改→恢复」以立即生效。
  /// 已下进度完整保留：恢复时按新线程数续传（增线程拆分现有段、减线程降并发）。
  /// `segments <= 0` = 恢复为「自动」。
  void setTaskSegments(String taskId, int segments) {
    final n = segments < 0 ? 0 : segments;
    logInfo(_tag, 'request set segments: task=$taskId, segments=$n');
    UpdateTaskSegments(taskId: taskId, segments: n).sendSignalToRust();
  }

  /// 处理 Rust 的分段数修改结果：成功则更新 configuredSegments，
  /// 并通过 [onSegmentsUpdateResult] 通知 UI 弹提示。
  void _onTaskSegmentsUpdated(RustSignalPack<TaskSegmentsUpdated> pack) {
    if (_disposed) return;
    final u = pack.message;
    if (u.ok) {
      final idx = _tasks.indexWhere((t) => t.id == u.taskId);
      if (idx >= 0) {
        _tasks[idx] = _tasks[idx].copyWith(configuredSegments: u.segments);
        _safeNotifyListeners();
      }
    }
    onSegmentsUpdateResult?.call(u.taskId, u.segments, u.ok);
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

  /// 任务队列归属变化：move_task_to_queue 后引擎定向广播。只 copyWith 单个
  /// 字段、不重建整表（与文件跟踪同理），沿用 _deletedTaskIds 守卫。
  void _onTaskQueueChanged(RustSignalPack<TaskQueueChanged> pack) {
    if (_disposed) return;
    final m = pack.message;
    if (_deletedTaskIds.contains(m.taskId)) return;
    final idx = _tasks.indexWhere((t) => t.id == m.taskId);
    if (idx >= 0 && _tasks[idx].queueId != m.queueId) {
      _tasks[idx] = _tasks[idx].copyWith(queueId: m.queueId);
      _safeNotifyListeners();
    }
  }

  /// 插件钩子活动指示：`running=true` 加入 `(taskId, pluginId)` 并设/重置
  /// 看门狗；`running=false` 移除，集合空则整条清理并取消看门狗。纯旁路 UI
  /// 状态，不驱动 _tasks 状态机。
  void _onPluginHookActivity(RustSignalPack<PluginHookActivityEvent> pack) {
    if (_disposed) return;
    final e = pack.message;
    if (e.running) {
      final set = _pluginHookActivity[e.taskId] ??= {};
      if (set.isEmpty) {
        // 该任务本轮首个活动钩子：记录起始时间供详情面板显示耗时。
        _pluginHookSince[e.taskId] = DateTime.now();
      }
      set.add(e.pluginId);
      _pluginHookWatchdogs[e.taskId]?.cancel();
      _pluginHookWatchdogs[e.taskId] = Timer(_pluginHookWatchdogTimeout, () {
        logInfo(
          _tag,
          'plugin hook watchdog fired: task=${e.taskId} — 清除悬挂的插件处理指示',
        );
        _pluginHookActivity.remove(e.taskId);
        _pluginHookSince.remove(e.taskId);
        _pluginHookWatchdogs.remove(e.taskId);
        _safeNotifyListeners();
      });
    } else {
      final activePlugins = _pluginHookActivity[e.taskId];
      activePlugins?.remove(e.pluginId);
      if (activePlugins == null || activePlugins.isEmpty) {
        _pluginHookActivity.remove(e.taskId);
        _pluginHookSince.remove(e.taskId);
        _pluginHookWatchdogs.remove(e.taskId)?.cancel();
      }
    }
    _safeNotifyListeners();
  }

  /// 该任务当前是否有插件钩子正在处理（旁路 UI 指示器，不代表任务状态）。
  bool isPluginProcessing(String taskId) =>
      _pluginHookActivity[taskId]?.isNotEmpty ?? false;

  /// 正在处理该任务的插件 identity 集合（快照拷贝；无活动时为空集）。
  Set<String> pluginProcessingIds(String taskId) =>
      Set.unmodifiable(_pluginHookActivity[taskId] ?? const <String>{});

  /// 本轮插件处理的起始时间（无活动时为 null），供详情面板显示耗时。
  DateTime? pluginProcessingSince(String taskId) => _pluginHookSince[taskId];

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
    // 外部途径（浏览器扩展 / aria2 RPC / 管理 API）发起的删除：Dart 侧没有
    // 乐观删除记录，Rust 的删除确认信号（status=4, error="deleted"）会落到
    // 这里。直接移除任务并登记守卫，而不是把它显示为「失败」。
    if (p.status == 4 && p.errorMessage == 'deleted') {
      _deletedTaskIds.add(p.taskId);
      _tasks.removeWhere((t) => t.id == p.taskId);
      if (_selectedTaskId == p.taskId) _selectedTaskId = null;
      _safeNotifyListeners();
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
        // TaskProgress 不携带 queue_id：归属待定（非「未分组」），
        // 紧随其后的 AllTasks 快照会带来真实归属。
        queueId: kQueueAttributionPending,
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

  void _onAllGroups(RustSignalPack<AllGroups> pack) {
    if (_disposed) return;
    final incoming = pack.message.groups;
    logInfo(_tag, '_onAllGroups: ${incoming.length} groups');
    _groups
      ..clear()
      ..addEntries(
        incoming.map((g) => MapEntry(g.groupId, DownloadGroup.fromSignal(g))),
      );
    final incomingIds = _groups.keys.toSet();
    _expandedGroupIds.removeWhere((id) => !incomingIds.contains(id));
    _collapsedDirKeys.removeWhere(
      (key) => !incomingIds.any((id) => key.startsWith('$id:')),
    );
    if (_selectedGroupId != null && !incomingIds.contains(_selectedGroupId)) {
      _selectedGroupId = null;
    }
    _safeNotifyListeners();
  }

  // ---------------------------------------------------------------------------
  // 任务组（桌面 UI）— 查询 / 选中 / 展开折叠 / 操作
  // ---------------------------------------------------------------------------

  /// 全部任务组（无序，UI 按需自行排序/展示）。
  List<DownloadGroup> get groups => _groups.values.toList();

  /// 按 ID 查找任务组（不存在返回 null）。
  DownloadGroup? groupById(String groupId) => _groups[groupId];

  String? get selectedGroupId => _selectedGroupId;

  DownloadGroup? get selectedGroup =>
      _selectedGroupId == null ? null : _groups[_selectedGroupId];

  /// 选中的组当前成员任务（组详情面板「成员」Tab 用）。
  List<DownloadTask> get selectedGroupMembers {
    final gid = _selectedGroupId;
    if (gid == null) return const [];
    return _tasks.where((t) => t.groupId == gid).toList();
  }

  void selectGroup(String? groupId) {
    if (groupId == null) {
      if (_selectedGroupId == null) return;
      _selectedGroupId = null;
      _safeNotifyListeners();
      return;
    }
    if (_selectedGroupId == groupId && _selectedTaskId == null) return;
    // 选中组与选中任务互斥。
    _selectedGroupId = groupId;
    _selectedTaskId = null;
    _safeNotifyListeners();
  }

  bool isGroupExpanded(String groupId) => _expandedGroupIds.contains(groupId);

  void toggleGroupExpanded(String groupId) {
    if (!_expandedGroupIds.add(groupId)) _expandedGroupIds.remove(groupId);
    _safeNotifyListeners();
  }

  void setGroupExpanded(String groupId, bool expanded) {
    final changed = expanded
        ? _expandedGroupIds.add(groupId)
        : _expandedGroupIds.remove(groupId);
    if (changed) _safeNotifyListeners();
  }

  static String _dirKey(String groupId, String path) => '$groupId:$path';

  bool isDirCollapsed(String groupId, String path) =>
      _collapsedDirKeys.contains(_dirKey(groupId, path));

  void toggleDirCollapsed(String groupId, String path) {
    final key = _dirKey(groupId, path);
    if (!_collapsedDirKeys.add(key)) _collapsedDirKeys.remove(key);
    _safeNotifyListeners();
  }

  /// 「N 失败」直达（design-proto-spec §8 `jumpToFail`）：展开组 + 若失败
  /// 成员所在目录被折叠则一并展开。滚动定位与高亮闪烁动效由调用方
  /// （task_group_card.dart / task_list.dart）负责。
  void revealGroupMember(String groupId, String memberTaskId) {
    var changed = _expandedGroupIds.add(groupId);
    final group = _groups[groupId];
    DownloadTask? memberTask;
    for (final t in _tasks) {
      if (t.id == memberTaskId) {
        memberTask = t;
        break;
      }
    }
    if (group != null && memberTask != null) {
      final dir = groupMemberDirPath(memberTask, group);
      if (dir.isNotEmpty && _collapsedDirKeys.remove(_dirKey(groupId, dir))) {
        changed = true;
      }
    }
    if (changed) _safeNotifyListeners();
  }

  /// 按聚合态二选一（design-proto-spec §8 组右键菜单 / §13 Space 键）：
  /// 任一成员活跃/排队则全部暂停，否则全部恢复。
  void toggleGroupPauseResume(String groupId) {
    final hasActive = _tasks.any(
      (t) => t.groupId == groupId && t.status.isActiveOrQueued,
    );
    if (hasActive) {
      pauseGroup(groupId);
    } else {
      resumeGroup(groupId);
    }
  }

  /// 暂停组内全部活跃/排队成员（乐观 UI，同 [pauseAll] 语义仅限定组内）。
  void pauseGroup(String groupId) {
    logInfo(_tag, 'pauseGroup: $groupId');
    for (var i = 0; i < _tasks.length; i++) {
      final t = _tasks[i];
      if (t.groupId != groupId || !t.status.isActiveOrQueued) continue;
      _optimisticPausedIds.add(t.id);
      _tasks[i] = t.copyWith(status: TaskStatus.paused, speed: 0);
    }
    GroupControl(groupId: groupId, action: 0).sendSignalToRust();
    _safeNotifyListeners();
  }

  /// 恢复组内全部暂停/失败成员（乐观 UI，同 [resumeAll] 语义仅限定组内）。
  void resumeGroup(String groupId) {
    logInfo(_tag, 'resumeGroup: $groupId');
    for (var i = 0; i < _tasks.length; i++) {
      final t = _tasks[i];
      if (t.groupId != groupId) continue;
      if (t.status != TaskStatus.paused && t.status != TaskStatus.error) {
        continue;
      }
      _optimisticPausedIds.remove(t.id);
      _tasks[i] = t.copyWith(status: TaskStatus.resuming);
    }
    GroupControl(groupId: groupId, action: 1).sendSignalToRust();
    _safeNotifyListeners();
  }

  /// 仅重试组内失败成员。
  void retryGroupFailed(String groupId) {
    logInfo(_tag, 'retryGroupFailed: $groupId');
    for (var i = 0; i < _tasks.length; i++) {
      final t = _tasks[i];
      if (t.groupId != groupId || t.status != TaskStatus.error) continue;
      _optimisticPausedIds.remove(t.id);
      _tasks[i] = t.copyWith(status: TaskStatus.resuming);
    }
    GroupControl(groupId: groupId, action: 2).sendSignalToRust();
    _safeNotifyListeners();
  }

  /// 删除组（记录，[deleteFiles] 时一并删除磁盘文件）及其全部成员任务。
  void deleteGroup(String groupId, {required bool deleteFiles}) {
    logInfo(_tag, 'deleteGroup: $groupId, deleteFiles=$deleteFiles');
    final ids = _tasks
        .where((t) => t.groupId == groupId)
        .map((t) => t.id)
        .toSet();
    _optimisticPausedIds.removeAll(ids);
    _boostAutoPausedIds.removeAll(ids);
    _deferredResumeQueue.removeWhere(ids.contains);
    _deletedTaskIds.addAll(ids);
    _tasks.removeWhere((t) => ids.contains(t.id));
    if (_selectedTaskId != null && ids.contains(_selectedTaskId)) {
      _selectedTaskId = null;
    }
    if (_selectedGroupId == groupId) _selectedGroupId = null;
    _groups.remove(groupId);
    _expandedGroupIds.remove(groupId);
    _collapsedDirKeys.removeWhere((k) => k.startsWith('$groupId:'));
    GroupControl(
      groupId: groupId,
      action: deleteFiles ? 3 : 4,
    ).sendSignalToRust();
    _safeNotifyListeners();
  }

  /// 重命名任务组（乐观更新本地缓存；`name` trim 后为空则忽略，同引擎侧
  /// 校验语义）。proto 组右键菜单未收录该入口（design-proto-spec §8），本
  /// 方法按契约要求接线信号，暂无 UI 触发点（详见 dart-groups-report.md）。
  void renameGroup(String groupId, String name) {
    final trimmed = name.trim();
    if (trimmed.isEmpty) return;
    logInfo(_tag, 'renameGroup: $groupId -> $trimmed');
    final existing = _groups[groupId];
    if (existing != null) {
      _groups[groupId] = DownloadGroup(
        id: existing.id,
        name: trimmed,
        sourceUrl: existing.sourceUrl,
        saveDir: existing.saveDir,
        createdAt: existing.createdAt,
      );
    }
    RenameGroup(groupId: groupId, name: trimmed).sendSignalToRust();
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

// =============================================================================
// 视图系统 — 分桶函数表 + 排序比较器表（7 维 × 6 键，纯函数）
//
// 行为规格依据：design-proto-spec.md §2/§3。全部为顶层公开纯函数（不依赖
// DownloadController 实例状态，`queue` 维度的队列顺序单独接参数），供
// buildListSections() 组装，也供 test/ 直接单测（DownloadController 因
// 依赖 rinf FFI 无法在测试中实例化，纯函数抽出是唯一可测路径）。
// =============================================================================

bool _isActiveOrQueued(TaskStatus s) =>
    s == TaskStatus.downloading ||
    s == TaskStatus.preparing ||
    s == TaskStatus.resuming ||
    s == TaskStatus.pending;

/// 时间分档序号（design-proto-spec §2 `dateKey`）：0=今天 1=昨天 2=本周
/// 3=本月 4=更早。复用现有 [TimeGroup] 枚举顺序，与 `_buildTimeGroups`
/// 语义一致。
int dateKeyOf(DateTime createdAt) => TimeGroup.fromDateTime(createdAt).index;

List<ListSection> _bucketByDateKey(List<ListEntity> entities) {
  final buckets = <int, List<ListEntity>>{};
  for (final e in entities) {
    (buckets[dateKeyOf(e.createdAt)] ??= []).add(e);
  }
  return [
    for (var k = 0; k < TimeGroup.values.length; k++)
      if (buckets[k] != null && buckets[k]!.isNotEmpty)
        ListSection(
          key: 'date:$k',
          title: TimeGroup.values[k].label,
          entities: buckets[k]!,
        ),
  ];
}

/// 「不分组」：单桶纯平铺，`title=null` 不渲染分组头。
List<ListSection> bucketEntitiesNone(List<ListEntity> entities) => [
  ListSection(key: 'none:all', title: null, entities: entities),
];

/// 「智能」：活跃（下载中/准备中/恢复中/排队）置顶一桶，其余按时间分档
/// （design-proto-spec §2 `by=smart`；等价于现状 `groupedTasks` 分桶行为，
/// 保证默认视图零感知）。
List<ListSection> bucketEntitiesSmart(List<ListEntity> entities) {
  final active = entities
      .where((e) => _isActiveOrQueued(e.statusBucket))
      .toList();
  final historical = entities
      .where((e) => !_isActiveOrQueued(e.statusBucket))
      .toList();
  return [
    if (active.isNotEmpty)
      ListSection(
        key: 'smart:live',
        title: currentS.activeGroupLabel,
        entities: active,
      ),
    ..._bucketByDateKey(historical),
  ];
}

/// 「日期」：全量按时间分档（与 `smart` 的差异是不做活跃置顶）。
List<ListSection> bucketEntitiesByDate(List<ListEntity> entities) =>
    _bucketByDateKey(entities);

String _statusBucketLabel(TaskStatus st) {
  final s = currentS;
  return switch (st) {
    TaskStatus.downloading => s.statusDownloading,
    TaskStatus.pending => s.statusPending,
    TaskStatus.paused => s.statusPaused,
    TaskStatus.error => s.statusError,
    TaskStatus.completed => s.statusCompleted,
    TaskStatus.preparing => s.statusDownloading,
    TaskStatus.resuming => s.statusDownloading,
  };
}

/// 「状态」：固定顺序 [下载中,排队,暂停,失败,完成]，仅保留有成员的桶
/// （design-proto-spec §2 `by=status`；preparing/resuming 视觉上并入
/// 下载中桶，与列表行/网格卡的状态色映射一致）。
List<ListSection> bucketEntitiesByStatus(List<ListEntity> entities) {
  const order = [
    TaskStatus.downloading,
    TaskStatus.pending,
    TaskStatus.paused,
    TaskStatus.error,
    TaskStatus.completed,
  ];
  final buckets = <TaskStatus, List<ListEntity>>{};
  for (final e in entities) {
    final bucket =
        _isActiveOrQueued(e.statusBucket) && e.statusBucket != TaskStatus.pending
        ? TaskStatus.downloading
        : e.statusBucket;
    (buckets[bucket] ??= []).add(e);
  }
  return [
    for (final st in order)
      if (buckets[st] != null && buckets[st]!.isNotEmpty)
        ListSection(
          key: 'status:${st.name}',
          title: _statusBucketLabel(st),
          entities: buckets[st]!,
        ),
  ];
}

/// 「类型」：固定顺序（design-proto-spec §2 `TYPE_ORDER`= `FileCategory`
/// 去掉 `all`），仅保留有成员的桶。
List<ListSection> bucketEntitiesByType(List<ListEntity> entities) {
  final order = FileCategory.values.where((c) => c != FileCategory.all);
  final buckets = <FileCategory, List<ListEntity>>{};
  for (final e in entities) {
    (buckets[e.categoryKey] ??= []).add(e);
  }
  return [
    for (final cat in order)
      if (buckets[cat] != null && buckets[cat]!.isNotEmpty)
        ListSection(
          key: 'type:${cat.name}',
          title: cat.label,
          entities: buckets[cat]!,
        ),
  ];
}

/// 「队列」：默认队列（`queueId==''`）固定排最前，其余按 [queues] 已排序
/// 顺序分桶，仅保留有成员的桶。
List<ListSection> bucketEntitiesByQueue(
  List<ListEntity> entities,
  List<DownloadQueue> queues,
) {
  final buckets = <String, List<ListEntity>>{};
  for (final e in entities) {
    (buckets[e.queueId] ??= []).add(e);
  }
  final s = currentS;
  final sections = <ListSection>[];
  final defaultBucket = buckets[''];
  if (defaultBucket != null && defaultBucket.isNotEmpty) {
    sections.add(
      ListSection(key: 'queue:', title: s.ungroupedTasks, entities: defaultBucket),
    );
  }
  for (final q in queues) {
    final bucket = buckets[q.queueId];
    if (bucket == null || bucket.isEmpty) continue;
    sections.add(
      ListSection(
        key: 'queue:${q.queueId}',
        title: queueDisplayName(s, q),
        entities: bucket,
      ),
    );
  }
  return sections;
}

/// 「站点」：按 [ListEntity.siteKey] 分桶，桶按成员数降序排列
/// （design-proto-spec §2 `by=site`）。
List<ListSection> bucketEntitiesBySite(List<ListEntity> entities) {
  final buckets = <String, List<ListEntity>>{};
  for (final e in entities) {
    (buckets[e.siteKey] ??= []).add(e);
  }
  final keys = buckets.keys.toList()
    ..sort((a, b) => buckets[b]!.length.compareTo(buckets[a]!.length));
  return [
    for (final key in keys)
      ListSection(
        key: 'site:${key.replaceAll(RegExp(r'\W'), '_')}',
        title: buckets[key]!.first.siteLabel,
        entities: buckets[key]!,
      ),
  ];
}

/// 分桶函数表：[ViewGroupBy] → 分桶函数（`queue` 维度需要队列顺序，
/// 经 [queues] 参数柯里化）。
Map<ViewGroupBy, List<ListSection> Function(List<ListEntity>)>
bucketFunctionTable(List<DownloadQueue> queues) => {
  ViewGroupBy.smart: bucketEntitiesSmart,
  ViewGroupBy.date: bucketEntitiesByDate,
  ViewGroupBy.status: bucketEntitiesByStatus,
  ViewGroupBy.type: bucketEntitiesByType,
  ViewGroupBy.queue: (entities) => bucketEntitiesByQueue(entities, queues),
  ViewGroupBy.site: bucketEntitiesBySite,
  ViewGroupBy.none: bucketEntitiesNone,
};

/// 「智能」排序比较器：状态优先级（下载中0 < 准备/恢复中1 < 排队2 <
/// 暂停3 < 失败4 < 完成5）→ 排队内按队列位置 → 其余按创建时间升序，
/// 固定升序稳定、忽略 [SortDir]（design-proto-spec §3 `smart` 语义，对齐
/// 现状 `_compareActiveTasks` 并推广到全部状态，保证默认视图零感知）。
int compareEntitiesSmart(ListEntity a, ListEntity b) {
  int tier(TaskStatus s) => switch (s) {
    TaskStatus.downloading => 0,
    TaskStatus.preparing => 1,
    TaskStatus.resuming => 1,
    TaskStatus.pending => 2,
    TaskStatus.paused => 3,
    TaskStatus.error => 4,
    TaskStatus.completed => 5,
  };
  final diff = tier(a.statusBucket) - tier(b.statusBucket);
  if (diff != 0) return diff;
  if (a.statusBucket == TaskStatus.pending && a is TaskEntity && b is TaskEntity) {
    return a.task.queuePosition.compareTo(b.task.queuePosition);
  }
  return a.createdAt.compareTo(b.createdAt);
}

/// 6 键排序比较器表（`smart` 忽略 [dir]；其余按 [dir] 升/降序，
/// design-proto-spec §3 `sortEnts`）。
int compareEntities(ViewSortKey key, SortDir dir, ListEntity a, ListEntity b) {
  if (key == ViewSortKey.smart) return compareEntitiesSmart(a, b);
  final mul = dir == SortDir.asc ? 1 : -1;
  switch (key) {
    case ViewSortKey.created:
      return a.createdAt.compareTo(b.createdAt) * mul;
    case ViewSortKey.name:
      return a.name.compareTo(b.name) * mul;
    case ViewSortKey.size:
      return a.totalBytes.compareTo(b.totalBytes) * mul;
    case ViewSortKey.progress:
      return a.progress.compareTo(b.progress) * mul;
    case ViewSortKey.speed:
      return a.speedBytesPerSec.compareTo(b.speedBytesPerSec) * mul;
    case ViewSortKey.smart:
      return compareEntitiesSmart(a, b);
  }
}

/// 桶间排序（「排序控全局叙事」）：显式排序键下，分桶结果按各桶首行——
/// 桶内排序完成后该桶在当前比较器下的极值代表——用同一比较器重排，使
/// 全列表首行恒为当前排序键的全局极值（状态分组+进度↓ → 已完成桶置顶、
/// +速度↓ → 下载中桶置顶；日期分组+创建时间↑ → 时间正序阅读）。
/// `smart` 排序保持各维度固定叙事顺序（默认视图零感知）；`smart:live`
/// 活跃桶恒置顶（智能分组的置顶承诺高于排序）；比较相等时保持分桶函数
/// 产出的固定顺序（显式索引平局裁决——`List.sort` 不稳定）。
/// 前置条件：各桶已按同一 [key]/[dir] 完成桶内排序（首行才是极值代表），
/// 且除单桶维度外分桶函数只产出非空桶。
List<ListSection> orderSections(
  List<ListSection> sections,
  ViewSortKey key,
  SortDir dir,
) {
  if (key == ViewSortKey.smart || sections.length < 2) return sections;
  final pinned = <ListSection>[];
  final movable = <ListSection>[];
  for (final s in sections) {
    (s.key == 'smart:live' ? pinned : movable).add(s);
  }
  final order = List<int>.generate(movable.length, (i) => i);
  order.sort((ia, ib) {
    final c = compareEntities(
      key,
      dir,
      movable[ia].entities.first,
      movable[ib].entities.first,
    );
    return c != 0 ? c : ia - ib;
  });
  return [...pinned, for (final i in order) movable[i]];
}

// =============================================================================
// 任务组 — 归组 / 展开扁平化 / 目录路径推导（纯函数）
//
// 行为规格依据：design-proto-spec.md §8 `membersHtml`/`dirRowHtml`/
// `jumpToFail`。全部为顶层公开纯函数（不依赖 DownloadController 实例状态），
// 供 buildListSections() 调用，也供 test/ 直接单测（同上方视图系统分桶/
// 排序纯函数的测试策略——DownloadController 因依赖 rinf FFI 无法在测试中
// 实例化）。
// =============================================================================

/// 按 `groupId` 归组：非空且 [knownGroupIds] 已知 → 归入组；否则（含孤儿
/// groupId——组表未知，GC 竞态或组已删除）→ 降级为普通任务平铺
/// （design-proto-spec 无此场景，工程防御）。
({List<TaskEntity> ungrouped, Map<String, List<DownloadTask>> byGroup})
partitionTasksByGroup(List<DownloadTask> tasks, Set<String> knownGroupIds) {
  final ungrouped = <TaskEntity>[];
  final byGroup = <String, List<DownloadTask>>{};
  for (final t in tasks) {
    final gid = t.groupId;
    if (gid.isEmpty || !knownGroupIds.contains(gid)) {
      ungrouped.add(TaskEntity(t));
    } else {
      (byGroup[gid] ??= []).add(t);
    }
  }
  return (ungrouped: ungrouped, byGroup: byGroup);
}

/// 用真实 [DownloadGroup] 元数据 + 成员任务构造 [GroupEntity]（组聚合字段
/// 由 [GroupEntity] 自身从 `members` 派生，见 list_entity.dart §3.2）。
/// [members] 须非空（[partitionTasksByGroup] 只在有成员时才产出该组键）。
GroupEntity buildGroupEntity(DownloadGroup group, List<DownloadTask> members) {
  return GroupEntity(
    groupId: group.id,
    groupName: group.displayName,
    sourceUrl: group.sourceUrl,
    saveDir: group.saveDir,
    groupCreatedAt: group.createdAt,
    groupQueueId: members.isEmpty ? '' : members.first.queueId,
    members: [for (final t in members) TaskEntity(t)],
  );
}

/// 成员在组内的相对目录路径（''=组根目录）。工程推导：引擎为每个组成员
/// 任务的 `saveDir` 写入「组根目录 + relPath 的目录部分」（组创建时
/// `GroupItemEntry.relPath` 拼接进最终落盘路径，hub-final-signals.md §3）；
/// `TaskInfo` 本身不携带独立的 relPath 字段，因此用 `task.saveDir` 相对
/// `group.saveDir` 的前缀差得到目录路径——两者相同或无法识别前缀关系
/// （不可能场景的防御）时视为组根目录。
String groupMemberDirPath(DownloadTask task, DownloadGroup group) {
  final root = group.saveDir.replaceAll('\\', '/');
  final dir = task.saveDir.replaceAll('\\', '/');
  if (dir == root) return '';
  final rootWithSlash = root.endsWith('/') ? root : '$root/';
  if (!dir.startsWith(rootWithSlash)) return '';
  return dir.substring(rootWithSlash.length);
}

/// 组内成员「path 排序 + 非空目录去重插入目录行 + 折叠隐藏」聚簇
/// （design-proto-spec §8 `membersHtml`）。返回值不含组头行本身
/// （[GroupEntity]），只是紧随其后要插入的扁平行序列；根目录（空 dir）
/// 成员直贴、不产出目录头。
List<ListEntity> flattenGroupMembers({
  required DownloadGroup group,
  required List<DownloadTask> members,
  required bool Function(String path) isDirCollapsed,
}) {
  final withDir = [
    for (final t in members) (task: t, dir: groupMemberDirPath(t, group)),
  ];
  String fullPath(({DownloadTask task, String dir}) e) =>
      e.dir.isEmpty ? e.task.fileName : '${e.dir}/${e.task.fileName}';
  withDir.sort((a, b) => fullPath(a).compareTo(fullPath(b)));

  final result = <ListEntity>[];
  String? currentDir;
  for (final e in withDir) {
    if (e.dir != currentDir) {
      currentDir = e.dir;
      if (e.dir.isNotEmpty) {
        final inDir = withDir.where((x) => x.dir == e.dir);
        result.add(
          GroupDirEntity(
            groupId: group.id,
            path: e.dir,
            fileCount: inDir.length,
            totalDirBytes: inDir.fold(0, (s, x) => s + x.task.totalBytes),
          ),
        );
      }
    }
    if (e.dir.isNotEmpty && isDirCollapsed(e.dir)) continue;
    result.add(
      GroupMemberEntity(task: e.task, groupId: group.id, dirPath: e.dir),
    );
  }
  return result;
}
