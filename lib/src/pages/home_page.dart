import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../widgets/flux_sonner.dart';
import '../../main.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../models/list_entity.dart';
import '../models/plugin_provider.dart';
import '../models/settings_provider.dart';
import '../models/view_prefs.dart';
import '../services/external_download_service.dart';
import '../services/cloud/config_sync_service.dart';
import '../services/cloud/remote_task_service.dart';
import '../services/link/local_pairing_service.dart';
import '../services/log_service.dart';
import '../services/kv_store.dart';
import '../services/notification_service.dart';
import '../services/power_service.dart';
import '../services/shutdown_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import '../widgets/sidebar.dart';
import '../widgets/header_bar.dart';
import '../widgets/task_tab_bar.dart';
import '../widgets/task_list.dart';
import '../widgets/detail_panel.dart';
import '../widgets/group_detail_panel.dart';
import '../widgets/status_bar.dart';
import '../widgets/new_download_dialog.dart';
import '../widgets/task_list_item.dart';
import '../widgets/title_drag_area.dart';
import 'settings_page.dart';

/// macOS 应用菜单栏操作回调。
/// HomePage 在 initState 中注册，main.dart 的 PlatformMenuBar 调用。
class AppMenuCallbacks {
  AppMenuCallbacks._();

  static VoidCallback? newDownload;
  static VoidCallback? openSettings;
  static VoidCallback? openAbout;
  static VoidCallback? find;
  static VoidCallback? selectAll;
}

class HomePage extends StatefulWidget {
  const HomePage({super.key});

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  final _controller = DownloadController();
  final _settingsProvider = SettingsProvider();
  final _pluginProvider = PluginProvider();
  final _headerBarKey = GlobalKey<HeaderBarState>();
  /// 任务列表视图系统偏好 store（全局 + 按状态页签覆盖层，contract-dart.md）。
  final _viewPrefsStore = ViewPrefsStore();

  // 页面切换
  bool _showSettings = false;
  SettingsCategory? _initialSettingsCategory;
  SettingsSearchItem? _initialSettingsHighlight;

  // Sidebar
  double _sidebarWidth = 224;
  static const double _sidebarMinWidth = 180;
  static const double _sidebarMaxWidth = 320;
  bool _sidebarVisible = true;

  // Detail panel
  bool _isDetailOpen = false;
  /// false=底部，true=右侧。默认右侧，切换后持久化。
  bool _detailOnRight =
      KvStore.instance.getBool('detail_panel_on_right') ?? true;
  double _detailHeight = 280;
  static const double _detailMinHeight = 200;
  static const double _detailMaxHeight = 400;
  double _detailWidth = 280;
  static const double _detailMinWidth = 240;
  static const double _detailMaxWidth = 420;
  // 主内容区最小宽度，保证 HeaderBar 不溢出
  static const double _mainMinWidth = 400;

  @override
  void initState() {
    super.initState();
    logInfo('HomePage', 'initState');
    // 请求 Rust 端加载下载配置
    _settingsProvider.requestConfig();
    // 请求插件列表 + 订阅熔断器自动禁用通知（弹 toast）
    _pluginProvider.requestPlugins();
    _pluginProvider.addListener(_onPluginProviderChanged);
    // 监听下载完成事件 → 发送系统通知
    _controller.onTaskCompleted = _handleTaskCompleted;
    // 监听「修改线程数」结果 → toast 提示
    _controller.onSegmentsUpdateResult = _handleSegmentsUpdateResult;
    // 监听 controller 变化 — 选中任务被删除时自动关闭详情面板
    _controller.addListener(_onControllerChanged);
    // 全局键盘快捷键
    HardwareKeyboard.instance.addHandler(_onGlobalKey);
    // macOS 菜单栏回调
    _registerMenuCallbacks();
    // 浏览器扩展下载请求时自动切回首页
    ExternalDownloadService.onNavigateToHome = _navigateToHomeFromExternal;
    // 监听侧边栏区块可见性变化
    _settingsProvider.addListener(_checkSidebarVisibility);
    // 下载期间阻止系统睡眠/息屏（按设置项）
    PowerService.instance.bind(_controller, _settingsProvider);
    // 「任务完成后关机」服务（纯内存状态，重启不保留）
    ShutdownService.instance.bind(_controller);
    // FluxCloud 配置同步：providers 就绪后接线；远端应用/失败均弹 toast。
    ConfigSyncService.instance.onRemoteApplied = _onSyncRemoteApplied;
    ConfigSyncService.instance.addListener(_onConfigSyncChanged);
    unawaited(
      ConfigSyncService.instance.attach(
        settings: _settingsProvider,
        theme: FluxDownApp.of(context),
        locale: localeNotifier,
      ),
    );
    // FluxCloud 跨设备任务协同：providers 就绪后接线，登录即开 SSE 长连回流进度。
    unawaited(RemoteTaskService.instance.attach());
    // 本地设备互联（局域网配对，免账号）：与账号体系无关，启动即接线监听。
    unawaited(LocalPairingService.instance.attach());
    // 首次启动 .torrent 文件关联提示（仅 Windows）
    if (Platform.isWindows) {
      _settingsProvider.addListener(_onSettingsLoadedForAssocPrompt);
    }
  }

  /// 熔断器自动禁用插件时弹出提示。
  int _lastAutoDisabledSeq = -1;
  void _onPluginProviderChanged() {
    if (!mounted) return;
    final seq = _pluginProvider.autoDisabledSeq;
    if (seq == _lastAutoDisabledSeq) return;
    _lastAutoDisabledSeq = seq;
    final notice = _pluginProvider.lastAutoDisabledNotice;
    if (notice == null) return;
    final matches = _pluginProvider.plugins.where(
      (p) => p.identity == notice.identity,
    );
    final name = matches.isEmpty ? notice.identity : matches.first.name;
    FluxSonner.of(context).show(
      ShadToast.destructive(
        title: Text(currentS.pluginAutoDisabledToast(name)),
        duration: const Duration(seconds: 4),
      ),
    );
  }

  /// 远端设备同步条目被实际应用后弹 toast（[ConfigSyncService.onRemoteApplied]）。
  void _onSyncRemoteApplied(int count, String? deviceName) {
    if (!mounted) return;
    FluxSonner.of(context).show(
      ShadToast(
        title: Text(
          currentS.cloudSyncAppliedToast(count, deviceName ?? currentS.cloudSyncOtherDevice),
        ),
        duration: const Duration(seconds: 3),
      ),
    );
  }

  /// 同步失败态弹 toast；同一条错误文案去重，避免退避重试期间反复弹出。
  String? _lastSyncErrorNotified;
  void _onConfigSyncChanged() {
    if (!mounted) return;
    final sync = ConfigSyncService.instance;
    if (sync.status != CloudSyncStatus.error || sync.lastError == null) {
      _lastSyncErrorNotified = null;
      return;
    }
    if (sync.lastError == _lastSyncErrorNotified) return;
    _lastSyncErrorNotified = sync.lastError;
    FluxSonner.of(context).show(
      ShadToast.destructive(
        title: Text(currentS.cloudSyncFailedToast(sync.lastError!)),
        duration: const Duration(seconds: 4),
      ),
    );
  }

  /// 浏览器扩展触发下载时，若当前在设置页则自动切回首页。
  void _navigateToHomeFromExternal() {
    if (!mounted) return;
    if (_showSettings) {
      setState(() {
        _showSettings = false;
        _initialSettingsCategory = null;
        _initialSettingsHighlight = null;
      });
    }
  }

  @override
  void dispose() {
    logInfo('HomePage', 'dispose');
    ExternalDownloadService.onNavigateToHome = null;
    PowerService.instance.unbind();
    ShutdownService.instance.unbind();
    _clearMenuCallbacks();
    HardwareKeyboard.instance.removeHandler(_onGlobalKey);
    _settingsProvider.removeListener(_checkSidebarVisibility);
    _settingsProvider.removeListener(_onSettingsLoadedForAssocPrompt);
    ConfigSyncService.instance.removeListener(_onConfigSyncChanged);
    ConfigSyncService.instance.onRemoteApplied = null;
    _pluginProvider.removeListener(_onPluginProviderChanged);
    _pluginProvider.dispose();
    _controller.removeListener(_onControllerChanged);
    _controller.onTaskCompleted = null;
    _controller.onSegmentsUpdateResult = null;
    _controller.dispose();
    _settingsProvider.dispose();
    _viewPrefsStore.dispose();
    super.dispose();
    logInfo('HomePage', 'dispose done');
  }

  /// macOS 菜单栏回调注册
  void _registerMenuCallbacks() {
    AppMenuCallbacks.newDownload = () {
      if (!mounted || _showSettings) return;
      showNewDownloadDialog(context, _controller, _settingsProvider);
    };
    AppMenuCallbacks.openSettings = () {
      if (!mounted || _showSettings) return;
      setState(() {
        _showSettings = true;
      });
    };
    AppMenuCallbacks.openAbout = () {
      if (!mounted || _showSettings) return;
      setState(() {
        _initialSettingsCategory = SettingsCategory.about;
        _showSettings = true;
      });
    };
    AppMenuCallbacks.find = () {
      if (!mounted || _showSettings) return;
      _headerBarKey.currentState?.focusSearch();
    };
    AppMenuCallbacks.selectAll = () {
      if (!mounted || _showSettings) return;
      if (!_controller.isManageMode) _controller.enterManageMode();
      _controller.selectAllFiltered();
    };
  }

  void _clearMenuCallbacks() {
    AppMenuCallbacks.newDownload = null;
    AppMenuCallbacks.openSettings = null;
    AppMenuCallbacks.openAbout = null;
    AppMenuCallbacks.find = null;
    AppMenuCallbacks.selectAll = null;
  }

  /// 首次启动时，配置加载完毕后检查是否需要弹窗提示文件关联。
  /// 当三个侧边栏区块全部关闭时，隐藏整个侧边栏
  void _checkSidebarVisibility() {
    final visible =
        _settingsProvider.showSidebarStatus ||
        _settingsProvider.showSidebarQueues ||
        _settingsProvider.showSidebarCategory;
    if (_sidebarVisible != visible) {
      setState(() => _sidebarVisible = visible);
    }
  }

  /// 一旦触发（或不需要）就移除监听，避免重复弹窗。
  void _onSettingsLoadedForAssocPrompt() {
    if (!_settingsProvider.loaded) return;
    // 只触发一次
    _settingsProvider.removeListener(_onSettingsLoadedForAssocPrompt);

    if (_settingsProvider.torrentAssocPrompted) return;
    if (_settingsProvider.torrentAssociated) {
      // 已经关联了（可能安装器设置过），标记已提示
      _settingsProvider.markTorrentAssocPrompted();
      return;
    }

    // 延迟一帧后弹窗，确保 build 完成
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _showTorrentAssocDialog();
    });
  }

  /// 弹窗询问用户是否关联 .torrent 文件
  void _showTorrentAssocDialog() {
    final s = LocaleScope.of(context);
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog.alert(
        title: Text(s.torrentAssocDialogTitle),
        description: Text(s.torrentAssocDialogDesc),
        actions: [
          ShadButton.outline(
            child: Text(s.cancel),
            onPressed: () {
              _settingsProvider.markTorrentAssocPrompted();
              Navigator.of(ctx).pop();
            },
          ),
          ShadButton(
            child: Text(s.confirm),
            onPressed: () {
              _settingsProvider.setFileAssociation(true);
              _settingsProvider.markTorrentAssocPrompted();
              Navigator.of(ctx).pop();
            },
          ),
        ],
      ),
    );
  }

  /// 当选中任务被删除后，controller.selectedTask 变为 null，
  /// 此时自动关闭详情面板。
  void _onControllerChanged() {
    if (_isDetailOpen &&
        _controller.selectedTask == null &&
        _controller.selectedGroupId == null) {
      setState(() => _isDetailOpen = false);
    }
  }

  void _handleTaskCompleted(DownloadTask task) {
    // 通知服务内部做 800ms 防抖合批（多文件 → "N 个文件已下载"），
    // 此处无需再做汇总聚合。
    NotificationService.instance.showDownloadComplete(task);
  }

  /// 「修改线程数」结果提示。成功 → 普通 toast；被拒（任务非暂停态）→
  /// destructive toast 提示先暂停。
  void _handleSegmentsUpdateResult(String taskId, int segments, bool ok) {
    if (!mounted) return;
    if (ok) {
      FluxSonner.of(context).show(
        ShadToast(
          title: Text(currentS.threadsChanged),
          duration: const Duration(seconds: 2),
        ),
      );
    } else {
      FluxSonner.of(context).show(
        ShadToast.destructive(
          title: Text(currentS.threadsChangeRejected),
          duration: const Duration(seconds: 3),
        ),
      );
    }
  }

  /// 全局快捷键处理 — 不依赖焦点树
  bool _onGlobalKey(KeyEvent event) {
    if (event is! KeyDownEvent) return false;

    // 设置页或任何对话框打开时，不处理全局快捷键
    if (_showSettings) return false;
    if (ModalRoute.of(context)?.isCurrent == false) return false;

    // macOS 使用 Cmd 键，Windows/Linux 使用 Ctrl 键
    final isMod = Platform.isMacOS
        ? HardwareKeyboard.instance.isMetaPressed
        : HardwareKeyboard.instance.isControlPressed;

    // Cmd/Ctrl+F → 聚焦搜索框
    if (isMod && event.logicalKey == LogicalKeyboardKey.keyF) {
      _headerBarKey.currentState?.focusSearch();
      return true;
    }

    // Cmd/Ctrl+A → 全选当前筛选列表（自动进入管理模式）
    if (isMod && event.logicalKey == LogicalKeyboardKey.keyA) {
      if (!_controller.isManageMode) {
        _controller.enterManageMode();
      }
      _controller.selectAllFiltered();
      return true;
    }

    // Esc → 退出管理模式
    if (event.logicalKey == LogicalKeyboardKey.escape &&
        _controller.isManageMode) {
      _controller.exitManageMode();
      return true;
    }

    // Del / Cmd+Backspace → 弹出批量删除确认
    final isDelete =
        event.logicalKey == LogicalKeyboardKey.delete ||
        (Platform.isMacOS &&
            isMod &&
            event.logicalKey == LogicalKeyboardKey.backspace);
    if (isDelete && _controller.isManageMode && _controller.checkedCount > 0) {
      if (!mounted) return false;
      showBatchDeleteDialog(
        context,
        count: _controller.checkedCount,
        onDeleteTask: () => _controller.deleteCheckedTasks(deleteFiles: false),
        onDeleteTaskAndFile: () =>
            _controller.deleteCheckedTasks(deleteFiles: true),
      );
      return true;
    }
    // ── 视图系统快捷键（contract-dart.md §入口/面板/快捷键）——
    // 沿用「输入框聚焦时跳过」守卫，避免拦截搜索框等文本输入。
    if (!_isTextFieldFocused) {
      final tab = _controller.statusTab.name;
      final shift = HardwareKeyboard.instance.isShiftPressed;

      // V → 循环形态 列表↔网格
      if (!isMod && !shift && event.logicalKey == LogicalKeyboardKey.keyV) {
        _viewPrefsStore.update(
          tab,
          (p) => p.copyWith(
            form: p.form == ViewForm.list ? ViewForm.grid : ViewForm.list,
          ),
        );
        return true;
      }

      // Shift+D → 切换密度 舒适↔紧凑
      if (!isMod && shift && event.logicalKey == LogicalKeyboardKey.keyD) {
        _viewPrefsStore.update(
          tab,
          (p) => p.copyWith(
            density: p.density == ViewDensity.comfortable
                ? ViewDensity.compact
                : ViewDensity.comfortable,
          ),
        );
        return true;
      }

      // G → 循环分组维度
      if (!isMod && !shift && event.logicalKey == LogicalKeyboardKey.keyG) {
        _viewPrefsStore.update(tab, (p) {
          final idx = kGroupByCycle.indexOf(p.groupBy);
          return p.copyWith(
            groupBy: kGroupByCycle[(idx + 1) % kGroupByCycle.length],
          );
        });
        return true;
      }

      // S → 循环排序键（重置为该键默认方向）
      if (!isMod && !shift && event.logicalKey == LogicalKeyboardKey.keyS) {
        _viewPrefsStore.update(tab, (p) {
          final idx = kSortKeyCycle.indexOf(p.sortKey);
          final next = kSortKeyCycle[(idx + 1) % kSortKeyCycle.length];
          return p.copyWith(sortKey: next, sortDir: kSortKeyDefaultDir[next]);
        });
        return true;
      }

      // ↑/↓ → 跨组行导航（拼接全部分桶实体，忽略折叠态——见实现说明）
      if (event.logicalKey == LogicalKeyboardKey.arrowUp) {
        _navigateSelection(-1);
        return true;
      }
      if (event.logicalKey == LogicalKeyboardKey.arrowDown) {
        _navigateSelection(1);
        return true;
      }

      // Space → 暂停/恢复选中任务
      if (event.logicalKey == LogicalKeyboardKey.space) {
        _toggleSelectedPauseResume();
        return true;
      }
    }

    return false;
  }

  /// 是否有文本输入组件（TextField/ShadInput 等，内部均落到 [EditableText]）
  /// 持有焦点——全局单字母快捷键（V/G/S/Shift+D/↑↓/Space）必须在此时让路，
  /// 否则会拦截用户在搜索框等处的正常输入。Ctrl/Cmd 组合键与 Esc/Del 不受
  /// 此守卫影响（现状行为不变）。
  bool get _isTextFieldFocused {
    final focus = FocusManager.instance.primaryFocus;
    final ctx = focus?.context;
    if (ctx == null) return false;
    if (ctx.widget is EditableText) return true;
    var found = false;
    ctx.visitAncestorElements((element) {
      if (element.widget is EditableText) {
        found = true;
        return false;
      }
      return true;
    });
    return found;
  }

  /// 当前视图分桶展开后的可导航实体（跨组连续，忽略 TaskList 内部折叠态
  /// ——折叠桶内的行虽不可见，仍可通过键盘导航选中并驱动详情面板；已知的
  /// 有限简化，记录在案）。只含顶层任务/组行（design-proto-spec §13
  /// 「跨组导航：取 listMount 内所有 task/group 的 id 序」），组展开产出的
  /// 成员/目录行不参与键盘序列（不是 proto `selectedId` 的语义域）。
  List<ListEntity> _navigableEntities() {
    final prefs = _viewPrefsStore.resolve(_controller.statusTab.name);
    final sections = _controller.buildListSections(prefs);
    return [
      for (final section in sections)
        for (final entity in section.entities)
          if (entity is TaskEntity || entity is GroupEntity) entity,
    ];
  }

  void _navigateSelection(int delta) {
    final entities = _navigableEntities();
    if (entities.isEmpty) return;
    final currentId =
        _controller.selectedTaskId ?? _controller.selectedGroupId;
    final currentIndex = currentId == null
        ? -1
        : entities.indexWhere((e) => e.id == currentId);
    final nextIndex = currentIndex == -1
        ? (delta > 0 ? 0 : entities.length - 1)
        : (currentIndex + delta).clamp(0, entities.length - 1);
    final next = entities[nextIndex];
    if (next is GroupEntity) {
      _controller.selectGroup(next.groupId);
    } else {
      _controller.selectTask(next.id);
    }
    setState(() => _isDetailOpen = true);
  }

  void _toggleSelectedPauseResume() {
    final groupId = _controller.selectedGroupId;
    if (groupId != null) {
      _controller.toggleGroupPauseResume(groupId);
      return;
    }
    final id = _controller.selectedTaskId;
    if (id == null) return;
    DownloadTask? task;
    for (final t in _controller.tasks) {
      if (t.id == id) {
        task = t;
        break;
      }
    }
    if (task == null) return;
    switch (task.status) {
      case TaskStatus.downloading:
      case TaskStatus.pending:
      case TaskStatus.preparing:
      case TaskStatus.resuming:
        _controller.pauseTask(id);
      case TaskStatus.paused:
      case TaskStatus.error:
        _controller.resumeTask(id);
      case TaskStatus.completed:
        break;
    }
  }

  /// 点击任务行：
  /// - 点击未选中的任务 → 选中并打开详情面板
  /// - 再次点击同一任务 → 取消选中并关闭详情面板
  void _toggleDetail(String taskId) {
    final isSame = _controller.selectedTaskId == taskId;
    if (isSame && _isDetailOpen) {
      // 再次点击同一任务 → 关闭面板并取消选中
      _controller.selectTask(null);
      setState(() => _isDetailOpen = false);
    } else {
      // 点击新任务或面板未打开 → 选中并打开
      _controller.selectTask(taskId);
      setState(() => _isDetailOpen = true);
    }
  }

  /// 点击组行 / 组卡：同 [_toggleDetail] 语义，选中组与选中任务互斥。
  void _toggleDetailGroup(String groupId) {
    final isSame = _controller.selectedGroupId == groupId;
    if (isSame && _isDetailOpen) {
      _controller.selectGroup(null);
      setState(() => _isDetailOpen = false);
    } else {
      _controller.selectGroup(groupId);
      setState(() => _isDetailOpen = true);
    }
  }

  void _closeDetail() {
    _controller.selectTask(null);
    _controller.selectGroup(null);
    setState(() => _isDetailOpen = false);
  }

  void _toggleDetailPosition() {
    setState(() => _detailOnRight = !_detailOnRight);
    KvStore.instance.setBool('detail_panel_on_right', _detailOnRight);
  }

  /// 详情面板二选一：选中组时渲染 [GroupDetailPanel]，否则渲染任务
  /// [DetailPanel]（selectTask/selectGroup 互斥，见 download_controller.dart）。
  ///
  /// 二选一判定依赖 controller 状态，必须包 ListenableBuilder 响应通知：
  /// 组面板内点成员（selectTask）与任务面板内点「所属任务组」（selectGroup）
  /// 都不经本页 setState，静态判定会让旧面板滞留在各自的无选中空态
  /// （表现为点成员后右侧只剩「选择一个任务查看详情」，须重新点列表行
  /// 触发 home 重建才恢复）。
  Widget _buildDetailPanel({required bool isBottom}) {
    return ListenableBuilder(
      listenable: _controller,
      builder: (context, _) {
        if (_controller.selectedGroupId != null) {
          return GroupDetailPanel(
            controller: _controller,
            onClose: _closeDetail,
            isBottom: isBottom,
            onTogglePosition: _toggleDetailPosition,
          );
        }
        return DetailPanel(
          controller: _controller,
          onClose: _closeDetail,
          isBottom: isBottom,
          onTogglePosition: _toggleDetailPosition,
        );
      },
    );
  }

  /// 根据总宽度计算 sidebar 的实际最大值
  double _sidebarMax(double totalWidth) {
    final reserved =
        _mainMinWidth + (_isDetailOpen && _detailOnRight ? _detailWidth : 0);
    return (totalWidth - reserved).clamp(_sidebarMinWidth, _sidebarMaxWidth);
  }

  /// 根据总高度计算 detail 的实际最大值
  double _detailMax(double totalHeight) {
    final reserved = 40 + 32 + 50; // titlebar + tabbar + statusbar
    return (totalHeight - reserved).clamp(_detailMinHeight, _detailMaxHeight);
  }

  /// 根据总宽度计算 detail 右侧模式的实际最大宽度
  double _detailMaxW(double totalWidth) {
    final reserved = _mainMinWidth + (_sidebarVisible ? _sidebarWidth : 0);
    return (totalWidth - reserved).clamp(_detailMinWidth, _detailMaxWidth);
  }

  /// 构建主 Row 的 children（侧边栏固定左侧；详情面板可切到右侧）
  List<Widget> _buildRowChildren(
    AppColors c,
    double totalWidth,
    double totalHeight,
  ) {
    return [
      if (_sidebarVisible) ...[
        SizedBox(
          width: _sidebarWidth,
          child: Sidebar(
            controller: _controller,
            settingsProvider: _settingsProvider,
            onOpenAccountSettings: () => setState(() {
              _initialSettingsCategory = SettingsCategory.account;
              _showSettings = true;
            }),
          ),
        ),
      ],
      // 分隔线由内容区边框绘制（全高连续，与 HeaderBar 底边框无缝相接）；
      // 拖拽命中区是骑在边界上的透明浮层（见 build 的 Stack）。
      Expanded(
        child: DecoratedBox(
          position: DecorationPosition.foreground,
          decoration: BoxDecoration(
            border: Border(
              left: _sidebarVisible
                  ? BorderSide(color: c.border, width: 1)
                  : BorderSide.none,
              right: _isDetailOpen && _detailOnRight
                  ? BorderSide(color: c.border, width: 1)
                  : BorderSide.none,
            ),
          ),
          child: _buildContentArea(c, totalHeight),
        ),
      ),
      // 详情面板（右侧模式）
      if (_isDetailOpen && _detailOnRight)
        SizedBox(
          width: _detailWidth,
          child: Column(
            children: [
              const SizedBox(height: 40),
              Expanded(
                child: _buildDetailPanel(isBottom: false),
              ),
            ],
          ),
        ),
    ];
  }

  /// 构建内容区（任务列表在上、详情面板在下）
  Widget _buildContentArea(AppColors c, double totalHeight) {
    return ColoredBox(
      color: c.bg,
      child: Column(
        children: [
          const SizedBox(height: 40),
          TaskTabBar(controller: _controller),
          ListenableBuilder(
            listenable: _controller,
            builder: (context, _) {
              if (!_controller.isBoostActive) {
                return const SizedBox.shrink();
              }
              final tasks = _controller.tasks;
              if (tasks.isEmpty) return const SizedBox.shrink();
              final idx = tasks.indexWhere(
                (t) => t.id == _controller.priorityTaskId,
              );
              if (idx < 0) return const SizedBox.shrink();
              final s = LocaleScope.of(context);
              final c = AppColors.of(context);
              return _BoostBanner(
                fileName: tasks[idx].fileName,
                autoPausedCount: _controller.boostAutoPausedCount,
                onCancel: _controller.cancelBoost,
                s: s,
                c: c,
              );
            },
          ),
          Expanded(
            flex: _isDetailOpen ? 1 : 2,
            child: TaskList(
              controller: _controller,
              viewPrefsStore: _viewPrefsStore,
              onTaskTap: _toggleDetail,
              onGroupTap: _toggleDetailGroup,
              onNewDownload: () => showNewDownloadDialog(
                context,
                _controller,
                _settingsProvider,
              ),
            ),
          ),
          if (_isDetailOpen && !_detailOnRight) ...[
            // 水平分隔线（可拖拽调整详情面板高度）
            _ResizeHandle(
              color: c.border,
              hoverColor: AppMetrics.of(context).selectedBorder(c.accent),
              dragColor: AppMetrics.of(context).focusRing(c.accent),
              isVertical: true,
              onDrag: (dy) {
                setState(() {
                  _detailHeight = (_detailHeight - dy).clamp(
                    _detailMinHeight,
                    _detailMax(totalHeight),
                  );
                });
              },
            ),
            SizedBox(
              height: _detailHeight,
              child: _buildDetailPanel(isBottom: true),
            ),
          ],
        ],
      ),
    );
  }


  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);

    // 设置页面
    if (_showSettings) {
      return Stack(
        children: [
          // 全宽顶部拖拽区域
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            height: 40,
            child: TitleDragArea(child: ColoredBox(color: c.surface1)),
          ),
          ColoredBox(
            color: c.bg,
            child: SettingsPage(
              onBack: () => setState(() {
                _showSettings = false;
                _initialSettingsCategory = null;
                _initialSettingsHighlight = null;
              }),
              settingsProvider: _settingsProvider,
              pluginProvider: _pluginProvider,
              downloadController: _controller,
              initialCategory: _initialSettingsCategory,
              initialHighlight: _initialSettingsHighlight,
            ),
          ),

          // 工具按钮（设置页：暂停/恢复/设置/主题） — 右上角
          Positioned(
            top: 0,
            right: 0,
            child: WindowControls(
              controller: _controller,
              onSettings: () => setState(() {
                _showSettings = false;
                _initialSettingsCategory = null;
                _initialSettingsHighlight = null;
              }),
              isSettingsActive: true,
            ),
          ),
        ],
      );
    }

    // 主页面
    return LayoutBuilder(
      builder: (context, constraints) {
        final totalWidth = constraints.maxWidth;
        final totalHeight = constraints.maxHeight;
        // 自动收缩面板宽度以适应窗口
        _sidebarWidth = _sidebarWidth.clamp(
          _sidebarMinWidth,
          _sidebarMax(totalWidth),
        );
        if (_isDetailOpen) {
          if (_detailOnRight) {
            _detailWidth = _detailWidth.clamp(
              _detailMinWidth,
              _detailMaxW(totalWidth),
            );
          } else {
            _detailHeight = _detailHeight.clamp(
              _detailMinHeight,
              _detailMax(totalHeight),
            );
          }
        }
        return Stack(
          children: [
            // 全宽顶部拖拽区域（在所有内容之下）
            Positioned(
              top: 0,
              left: 0,
              right: 0,
              height: 40,
              child: TitleDragArea(child: ColoredBox(color: c.surface1)),
            ),
            // 内容区 — 全部从 titlebar 下方开始
            Column(
              children: [
                // 主内容行：Sidebar + 右侧内容
                Expanded(
                  child: Row(
                    children: _buildRowChildren(c, totalWidth, totalHeight),
                  ),
                ),
                // StatusBar（保持在最下方）
                StatusBar(
                  controller: _controller,
                  settingsProvider: _settingsProvider,
                  viewPrefsStore: _viewPrefsStore,
                ),
              ],
            ),
            // HeaderBar — 独立于内容区，不受 DetailPanel 宽度影响
            Positioned(
              top: 0,
              left: _sidebarVisible ? _sidebarWidth + 1 : 0,
              right: 0,
              height: 40,
              child: HeaderBar(
                key: _headerBarKey,
                controller: _controller,
                onNewDownload: () => showNewDownloadDialog(
                  context,
                  _controller,
                  _settingsProvider,
                ),
                onNavigateToSettings: (item) {
                  setState(() {
                    _initialSettingsCategory = item.category;
                    _initialSettingsHighlight = item;
                    _showSettings = true;
                  });
                },
              ),
            ),

            // 窗口控制按钮 + 工具按钮 — 右上角覆盖层（与设置页一致），
            // 隐藏的工具按钮自动紧凑合并，HeaderBar 侧按可见按钮数预留空间
            Positioned(
              top: 0,
              right: 0,
              child: WindowControls(
                controller: _controller,
                onSettings: () => setState(() {
                  _initialSettingsHighlight = null;
                  _showSettings = true;
                }),
              ),
            ),
            // 拖拽命中浮层：骑在面板边界上（1px 分隔线居中），不占布局宽度；
            // 平时全透明，悬浮/拖拽时浮现淡线提示。贯穿 titlebar/header
            // （高亮与分隔线全高一致），仅避开 StatusBar(28)。
            if (_sidebarVisible)
              Positioned(
                top: 0,
                bottom: 28,
                left: _sidebarWidth - (_ResizeHandle.hitSize - 1) / 2,
                width: _ResizeHandle.hitSize,
                child: _ResizeHandle(
                  color: m.selectedBorder(c.accent).withValues(alpha: 0),
                  hoverColor: m.selectedBorder(c.accent),
                  dragColor: m.focusRing(c.accent),
                  onDrag: (dx) {
                    setState(() {
                      _sidebarWidth = (_sidebarWidth + dx).clamp(
                        _sidebarMinWidth,
                        _sidebarMax(totalWidth),
                      );
                    });
                  },
                ),
              ),
            if (_isDetailOpen && _detailOnRight)
              Positioned(
                // 详情面板从 header 下方开始（上方 40px 是横跨的 HeaderBar），
                // 高亮线不侵入 header 区
                top: 40,
                bottom: 28,
                right: _detailWidth - (_ResizeHandle.hitSize - 1) / 2,
                width: _ResizeHandle.hitSize,
                child: _ResizeHandle(
                  color: m.selectedBorder(c.accent).withValues(alpha: 0),
                  hoverColor: m.selectedBorder(c.accent),
                  dragColor: m.focusRing(c.accent),
                  onDrag: (dx) {
                    setState(() {
                      _detailWidth = (_detailWidth - dx).clamp(
                        _detailMinWidth,
                        _detailMaxW(totalWidth),
                      );
                    });
                  },
                ),
              ),
            // 批量删除进度覆盖层（带平滑动画）
            _BatchDeleteOverlay(controller: _controller),
          ],
        );
      },
    );
  }
}

/// 可拖拽的分隔线：1px 视觉线居中 + 7px 透明命中区，便于鼠标悬浮命中；
/// 悬浮显示 hoverColor（主题强调色低透明度），拖拽中显示 dragColor（更强）。
class _ResizeHandle extends StatefulWidget {
  final Color color;
  final Color? hoverColor;
  final Color? dragColor;
  final ValueChanged<double> onDrag;
  final bool isVertical; // true=水平线（上下拖拽），false=垂直线（左右拖拽）

  /// 命中区厚度（视觉线居中，两侧透明可命中）
  static const double hitSize = 7;

  const _ResizeHandle({
    required this.color,
    required this.onDrag,
    this.hoverColor,
    this.dragColor,
    this.isVertical = false,
  });

  @override
  State<_ResizeHandle> createState() => _ResizeHandleState();
}

class _ResizeHandleState extends State<_ResizeHandle> {
  bool _isHovered = false;
  bool _isDragging = false;

  @override
  Widget build(BuildContext context) {
    final isVertical = widget.isVertical;
    final lineColor = _isDragging
        ? (widget.dragColor ?? widget.hoverColor ?? widget.color)
        : _isHovered
        ? (widget.hoverColor ?? widget.color)
        : widget.color;
    return GestureDetector(
      behavior: HitTestBehavior.translucent,
      onVerticalDragStart: isVertical
          ? (_) => setState(() => _isDragging = true)
          : null,
      onVerticalDragEnd: isVertical
          ? (_) => setState(() => _isDragging = false)
          : null,
      onVerticalDragUpdate: isVertical
          ? (details) => widget.onDrag(details.delta.dy)
          : null,
      onHorizontalDragStart: !isVertical
          ? (_) => setState(() => _isDragging = true)
          : null,
      onHorizontalDragEnd: !isVertical
          ? (_) => setState(() => _isDragging = false)
          : null,
      onHorizontalDragUpdate: !isVertical
          ? (details) => widget.onDrag(details.delta.dx)
          : null,
      child: MouseRegion(
        cursor: isVertical
            ? SystemMouseCursors.resizeRow
            : SystemMouseCursors.resizeColumn,
        onEnter: (_) => setState(() => _isHovered = true),
        onExit: (_) => setState(() => _isHovered = false),
        child: SizedBox(
          width: isVertical ? double.infinity : _ResizeHandle.hitSize,
          height: isVertical ? _ResizeHandle.hitSize : double.infinity,
          child: Center(
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 120),
              width: isVertical ? double.infinity : 1,
              height: isVertical ? 1 : double.infinity,
              color: lineColor,
            ),
          ),
        ),
      ),
    );
  }
}

/// Boost 优先下载模式提示条
class _BoostBanner extends StatelessWidget {
  final String fileName;
  final int autoPausedCount;
  final VoidCallback onCancel;
  final S s;
  final AppColors c;

  const _BoostBanner({
    required this.fileName,
    required this.autoPausedCount,
    required this.onCancel,
    required this.s,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
      color: m.muted(const Color(0xFFF59E0B)),
      child: Row(
        children: [
          const Icon(LucideIcons.zap, size: 14, color: Color(0xFFF59E0B)),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              s.boostBannerActive(fileName, autoPausedCount),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(
                fontSize: 12,
                color: Color(0xFFF59E0B),
                fontWeight: FontWeight.w500,
              ),
            ),
          ),
          const SizedBox(width: 8),
          GestureDetector(
            onTap: onCancel,
            child: Text(
              s.boostBannerCancel,
              style: TextStyle(
                fontSize: 12,
                color: c.textMuted,
                decoration: TextDecoration.underline,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// 批量删除进度覆盖层
///
/// 使用 AnimationController 确保进度条始终从 0% 平滑动画到目标值。
/// 即使 Rust 端所有信号在同一帧内到达（导致 batchDeleteProgress 瞬间
/// 从 0 跳到 1.0），用户也能看到完整的动画过渡。
/// 动画完成后保持 500ms 显示最终状态再淡出。
class _BatchDeleteOverlay extends StatefulWidget {
  final DownloadController controller;

  const _BatchDeleteOverlay({required this.controller});

  @override
  State<_BatchDeleteOverlay> createState() => _BatchDeleteOverlayState();
}

class _BatchDeleteOverlayState extends State<_BatchDeleteOverlay>
    with SingleTickerProviderStateMixin {
  late final AnimationController _anim;
  bool _visible = false;
  bool _fadingOut = false;

  @override
  void initState() {
    super.initState();
    _anim = AnimationController(vsync: this);
    widget.controller.addListener(_onControllerChanged);
    // 首帧检查（以防 widget 挂载时已在删除中）
    if (widget.controller.isBatchDeleting) {
      _startAnimation();
    }
  }

  @override
  void dispose() {
    widget.controller.removeListener(_onControllerChanged);
    _anim.dispose();
    super.dispose();
  }

  void _onControllerChanged() {
    final deleting = widget.controller.isBatchDeleting;
    if (deleting && !_visible) {
      _startAnimation();
    } else if (deleting && _visible) {
      // 更新目标值 — 驱动进度条前进
      _driveToProgress(widget.controller.batchDeleteProgress);
    } else if (!deleting && _visible && !_fadingOut) {
      // 删除完成：先驱动到 100%，保持短暂显示后淡出
      _driveToProgress(1.0);
      _fadingOut = true;
      Future.delayed(const Duration(milliseconds: 500), () {
        if (mounted) {
          setState(() {
            _visible = false;
            _fadingOut = false;
          });
        }
      });
    }
  }

  void _startAnimation() {
    _visible = true;
    _fadingOut = false;
    _anim.value = 0.0;
    // 最小动画时长 400ms，保证用户看到进度移动
    final target = widget.controller.batchDeleteProgress;
    final duration = target >= 1.0
        ? const Duration(milliseconds: 400)
        : const Duration(milliseconds: 200);
    _anim.animateTo(target, duration: duration, curve: Curves.easeOut);
    setState(() {});
  }

  void _driveToProgress(double target) {
    if (target <= _anim.value) return;
    // 剩余进度越大，动画越长，但至少 150ms
    final remaining = target - _anim.value;
    final ms = (remaining * 400).clamp(150, 500).toInt();
    _anim.animateTo(
      target,
      duration: Duration(milliseconds: ms),
      curve: Curves.easeOut,
    );
  }

  @override
  Widget build(BuildContext context) {
    if (!_visible) return const SizedBox.shrink();
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Positioned.fill(
      child: AbsorbPointer(
        child: ColoredBox(
          color: m.scrim(Colors.black),
          child: Center(
            child: AnimatedBuilder(
              animation: _anim,
              builder: (context, _) {
                return _BatchDeleteProgressCard(
                  animatedProgress: _anim.value,
                  done: widget.controller.batchDeleteDone,
                  total: widget.controller.batchDeleteTotal,
                  s: s,
                  c: c,
                );
              },
            ),
          ),
        ),
      ),
    );
  }
}

/// 批量删除进度卡片（纯展示组件）
class _BatchDeleteProgressCard extends StatelessWidget {
  final double animatedProgress;
  final int done;
  final int total;
  final S s;
  final AppColors c;

  const _BatchDeleteProgressCard({
    required this.animatedProgress,
    required this.done,
    required this.total,
    required this.s,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Container(
      width: 320,
      padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 20),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brChipLg,
        boxShadow: [
          BoxShadow(
            color: m.shadowStrong(Colors.black),
            blurRadius: 20,
            offset: const Offset(0, 8),
          ),
        ],
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            s.batchDeletingTitle,
            style: TextStyle(
              fontSize: 14,
              fontWeight: FontWeight.w600,
              color: c.textPrimary,
            ),
          ),
          const SizedBox(height: 12),
          ClipRRect(
          borderRadius: m.brSm,
            child: LinearProgressIndicator(
              value: animatedProgress,
              backgroundColor: c.surface3,
              valueColor: AlwaysStoppedAnimation<Color>(c.accent),
              minHeight: 6,
            ),
          ),
          const SizedBox(height: 8),
          Text(
            s.batchDeletingProgress(done, total),
            style: TextStyle(fontSize: 12, color: c.textMuted),
          ),
        ],
      ),
    );
  }
}
