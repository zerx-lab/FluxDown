import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
import '../services/analytics_service.dart';
import '../services/log_service.dart';
import '../services/notification_service.dart';
import '../theme/app_colors.dart';
import '../widgets/sidebar.dart';
import '../widgets/header_bar.dart';
import '../widgets/task_tab_bar.dart';
import '../widgets/task_list.dart';
import '../widgets/detail_panel.dart';
import '../widgets/status_bar.dart';
import '../widgets/new_download_dialog.dart';
import '../widgets/title_drag_area.dart';
import 'settings_page.dart';

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

  // Sidebar
  double _sidebarWidth = 224;
  static const double _sidebarMinWidth = 180;
  static const double _sidebarMaxWidth = 320;

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
    _controller.onTaskCompleted =
        NotificationService.instance.showDownloadComplete;
    // 监听 controller 变化 — 选中任务被删除时自动关闭详情面板
    _controller.addListener(_onControllerChanged);
    // 全局键盘快捷键
    HardwareKeyboard.instance.addHandler(_onGlobalKey);
    // 视图追踪
    AnalyticsService.instance.trackView('HomePage');
    // 首次启动 .torrent 文件关联提示（仅 Windows）
    if (Platform.isWindows) {
      _settingsProvider.addListener(_onSettingsLoadedForAssocPrompt);
    }
  }

  @override
  void dispose() {
    logInfo('HomePage', 'dispose');
    HardwareKeyboard.instance.removeHandler(_onGlobalKey);
    _settingsProvider.removeListener(_onSettingsLoadedForAssocPrompt);
    _controller.removeListener(_onControllerChanged);
    _controller.onTaskCompleted = null;
    _controller.dispose();
    _settingsProvider.dispose();
    super.dispose();
    logInfo('HomePage', 'dispose done');
  }

  /// 首次启动时，配置加载完毕后检查是否需要弹窗提示文件关联。
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

  /// 全局快捷键处理 — 不依赖焦点树
  bool _onGlobalKey(KeyEvent event) {
    if (event is! KeyDownEvent) return false;
    // Ctrl+F → 聚焦搜索框
    if (HardwareKeyboard.instance.isControlPressed &&
        event.logicalKey == LogicalKeyboardKey.keyF) {
      _headerBarKey.currentState?.focusSearch();
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
            height: 48,
            child: TitleDragArea(child: ColoredBox(color: c.surface1)),
          ),
          ColoredBox(
            color: c.bg,
            child: SettingsPage(
              onBack: () => setState(() {
                _showSettings = false;
                _initialSettingsCategory = null;
                AnalyticsService.instance.trackView('HomePage');
              }),
              settingsProvider: _settingsProvider,
              initialCategory: _initialSettingsCategory,
            ),
          ),
          // 窗口控制按钮 — 始终固定在窗口右上角
          Positioned(
            top: 0,
            right: 0,
            child: WindowControls(
              controller: _controller,
              onSettings: () => setState(() {
                _showSettings = false;
                _initialSettingsCategory = null;
                AnalyticsService.instance.trackView('HomePage');
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
              height: 48,
              child: TitleDragArea(child: ColoredBox(color: c.surface1)),
            ),
            // 内容区 — 全部从 titlebar 下方开始
            Row(
              children: [
                // Sidebar（全高 — 自带 logo 区对齐 titlebar）
                SizedBox(
                  width: _sidebarWidth,
                  child: Sidebar(controller: _controller),
                ),
                // Sidebar resize handle — 从 titlebar 下方开始
                Column(
                  children: [
                    const SizedBox(height: 48),
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
                // Main content — 从 titlebar 下方开始
                Expanded(
                  child: ColoredBox(
                    color: c.bg,
                    child: Column(
                      children: [
                        const SizedBox(height: 48),
                        TaskTabBar(controller: _controller),
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
                        StatusBar(controller: _controller),
                      ],
                    ),
                  ),
                ),
                // Detail panel — 从 titlebar 下方开始
                if (_isDetailOpen) ...[
                  Column(
                    children: [
                      const SizedBox(height: 48),
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
                        const SizedBox(height: 48),
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
              left: _sidebarWidth,
              right: 0,
              height: 48,
              child: HeaderBar(
                key: _headerBarKey,
                controller: _controller,
                onNewDownload: () => showNewDownloadDialog(
                  context,
                  _controller,
                  _settingsProvider,
                ),
                onNavigateToSettings: (category) {
                  setState(() {
                    _initialSettingsCategory = category;
                    _showSettings = true;
                    AnalyticsService.instance.trackView('SettingsPage');
                  });
                },
              ),
            ),
            // 窗口控制按钮 — 始终固定在窗口右上角
            Positioned(
              top: 0,
              right: 0,
              child: WindowControls(
                controller: _controller,
                onSettings: () => setState(() {
                  _showSettings = true;
                  AnalyticsService.instance.trackView('SettingsPage');
                }),
              ),
            ),
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
