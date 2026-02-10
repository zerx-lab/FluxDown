import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
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
  }

  @override
  void dispose() {
    logInfo('HomePage', 'dispose');
    HardwareKeyboard.instance.removeHandler(_onGlobalKey);
    _controller.removeListener(_onControllerChanged);
    _controller.onTaskCompleted = null;
    _controller.dispose();
    _settingsProvider.dispose();
    super.dispose();
    logInfo('HomePage', 'dispose done');
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
            // 主布局
            ColoredBox(
              color: c.bg,
              child: Row(
                children: [
                  // Sidebar
                  SizedBox(
                    width: _sidebarWidth,
                    child: Sidebar(controller: _controller),
                  ),
                  _ResizeHandle(
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
                  // Main content
                  Expanded(
                    child: Column(
                      children: [
                        HeaderBar(
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
                            });
                          },
                        ),
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
                  // Detail panel (conditional)
                  if (_isDetailOpen) ...[
                    _ResizeHandle(
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
                    SizedBox(
                      width: _detailWidth,
                      child: DetailPanel(
                        controller: _controller,
                        onClose: _closeDetail,
                      ),
                    ),
                  ],
                ],
              ),
            ),
            // 窗口控制按钮 — 始终固定在窗口右上角
            Positioned(
              top: 0,
              right: 0,
              child: WindowControls(
                controller: _controller,
                onSettings: () => setState(() => _showSettings = true),
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
          color: isActive ? c.accent.withValues(alpha: 0.6) : widget.color,
        ),
      ),
    );
  }
}
