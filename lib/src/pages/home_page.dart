import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../models/settings_provider.dart';
import '../services/external_download_service.dart';
import '../services/log_service.dart';
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
  final _headerBarKey = GlobalKey<HeaderBarState>();

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
    // 监听下载完成事件 → 发送系统通知
    _controller.onTaskCompleted = _handleTaskCompleted;
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
    // 首次启动 .torrent 文件关联提示（仅 Windows）
    if (Platform.isWindows) {
      _settingsProvider.addListener(_onSettingsLoadedForAssocPrompt);
    }
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
    _controller.removeListener(_onControllerChanged);
    _controller.onTaskCompleted = null;
    _controller.dispose();
    _settingsProvider.dispose();
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
    if (_isDetailOpen && _controller.selectedTask == null) {
      setState(() => _isDetailOpen = false);
    }
  }

  void _handleTaskCompleted(DownloadTask task) {
    // 通知服务内部做 800ms 防抖合批（多文件 → "N 个文件已下载"），
    // 此处无需再做汇总聚合。
    NotificationService.instance.showDownloadComplete(task);
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

    return false;
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

  void _closeDetail() {
    _controller.selectTask(null);
    setState(() => _isDetailOpen = false);
  }

  /// 根据总宽度计算 sidebar 的实际最大值
  double _sidebarMax(double totalWidth) {
    final reserved = _mainMinWidth + (_isDetailOpen ? _detailWidth : 0);
    return (totalWidth - reserved).clamp(_sidebarMinWidth, _sidebarMaxWidth);
  }

  /// 根据总宽度计算 detail 的实际最大值
  double _detailMax(double totalWidth) {
    final reserved = _mainMinWidth + _sidebarWidth;
    return (totalWidth - reserved).clamp(_detailMinWidth, _detailMaxWidth);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);

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
        // 自动收缩面板宽度以适应窗口
        _sidebarWidth = _sidebarWidth.clamp(
          _sidebarMinWidth,
          _sidebarMax(totalWidth),
        );
        if (_isDetailOpen) {
          _detailWidth = _detailWidth.clamp(
            _detailMinWidth,
            _detailMax(totalWidth),
          );
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
            Row(
              children: [
                // Sidebar（全高 — 自带 logo 区对齐 titlebar）
                if (_sidebarVisible) ...[
                  SizedBox(
                    width: _sidebarWidth,
                    child: Sidebar(
                      controller: _controller,
                      settingsProvider: _settingsProvider,
                    ),
                  ),
                  // Sidebar resize handle — header 区域保持 1px 静态边框，下方可交互
                  Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Container(height: 40, width: 1, color: c.border),
                      Expanded(
                        child: _ResizeHandle(
                          color: c.border,
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
                    ],
                  ),
                ],
                // Main content — 从 titlebar 下方开始
                Expanded(
                  child: ColoredBox(
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
                          child: TaskList(
                            controller: _controller,
                            onTaskTap: _toggleDetail,
                            onNewDownload: () => showNewDownloadDialog(
                              context,
                              _controller,
                              _settingsProvider,
                            ),
                          ),
                        ),
                        StatusBar(
                          controller: _controller,
                          settingsProvider: _settingsProvider,
                        ),
                      ],
                    ),
                  ),
                ),
                // Detail panel — 从 titlebar 下方开始
                if (_isDetailOpen) ...[
                  Column(
                    children: [
                      const SizedBox(height: 40),
                      Expanded(
                        child: _ResizeHandle(
                          color: c.border,
                          onDrag: (dx) {
                            setState(() {
                              _detailWidth = (_detailWidth - dx).clamp(
                                _detailMinWidth,
                                _detailMax(totalWidth),
                              );
                            });
                          },
                        ),
                      ),
                    ],
                  ),
                  SizedBox(
                    width: _detailWidth,
                    child: Column(
                      children: [
                        const SizedBox(height: 40),
                        Expanded(
                          child: DetailPanel(
                            controller: _controller,
                            onClose: _closeDetail,
                          ),
                        ),
                      ],
                    ),
                  ),
                ],
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
            // 批量删除进度覆盖层（带平滑动画）
            _BatchDeleteOverlay(controller: _controller),
          ],
        );
      },
    );
  }
}

/// 可拖拽的分隔线
class _ResizeHandle extends StatefulWidget {
  final Color color;
  final ValueChanged<double> onDrag;

  const _ResizeHandle({required this.color, required this.onDrag});

  @override
  State<_ResizeHandle> createState() => _ResizeHandleState();
}

class _ResizeHandleState extends State<_ResizeHandle> {
  bool _isHovered = false;
  bool _isDragging = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final isActive = _isHovered || _isDragging;
    return MouseRegion(
      cursor: SystemMouseCursors.resizeColumn,
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: GestureDetector(
        onHorizontalDragStart: (_) => setState(() => _isDragging = true),
        onHorizontalDragUpdate: (details) => widget.onDrag(details.delta.dx),
        onHorizontalDragEnd: (_) => setState(() => _isDragging = false),
        child: Container(
          width: isActive ? 3 : 1,
          color: isActive ? c.accent : widget.color,
        ),
      ),
    );
  }
}

// =============================================================================
// Boost Banner — 优先下载模式提示条
// =============================================================================

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
