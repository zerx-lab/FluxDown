import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:window_manager/window_manager.dart';
import '../../main.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../models/settings_provider.dart';
import '../pages/settings_page.dart';
import '../services/log_service.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import 'title_drag_area.dart';

// ─────────────────────────────────────────────
// 搜索结果模型
// ─────────────────────────────────────────────

enum SearchResultType { task, settings }

class SearchResult {
  final SearchResultType type;
  final String title;
  final String subtitle;
  final IconData icon;

  /// 任务搜索结果的 taskId
  final String? taskId;

  /// 设置项搜索结果的目标项（含分类/标签/描述，用于导航 + 高亮定位）
  final SettingsSearchItem? settingsItem;

  const SearchResult({
    required this.type,
    required this.title,
    required this.subtitle,
    required this.icon,
    this.taskId,
    this.settingsItem,
  });
}

// ─────────────────────────────────────────────
// HeaderBar
// ─────────────────────────────────────────────

class HeaderBar extends StatefulWidget {
  final VoidCallback onNewDownload;
  final DownloadController controller;
  final void Function(SettingsSearchItem item) onNavigateToSettings;

  const HeaderBar({
    super.key,
    required this.onNewDownload,
    required this.controller,
    required this.onNavigateToSettings,
  });

  @override
  State<HeaderBar> createState() => HeaderBarState();
}

class HeaderBarState extends State<HeaderBar> {
  final _searchController = TextEditingController();
  final _focusNode = FocusNode();
  late final _keyboardFocusNode = FocusNode();
  final _searchBoxKey = GlobalKey();
  final _overlayController = OverlayPortalController();

  List<SearchResult> _results = [];
  int _highlightedIndex = -1;
  bool _isSearchActive = false;

  @override
  void initState() {
    super.initState();
    _searchController.addListener(_onSearchChanged);
    _focusNode.addListener(_onFocusChanged);
  }

  @override
  void dispose() {
    _searchController.removeListener(_onSearchChanged);
    _searchController.dispose();
    _focusNode.removeListener(_onFocusChanged);
    _focusNode.dispose();
    _keyboardFocusNode.dispose();
    super.dispose();
  }

  /// 外部调用：聚焦搜索框（Ctrl+F）
  void focusSearch() {
    _focusNode.requestFocus();
  }

  void _onFocusChanged() {
    if (_focusNode.hasFocus) {
      _onSearchChanged(); // 聚焦时立即搜索
      setState(() => _isSearchActive = true);
    } else {
      // 延迟关闭，确保用户点击搜索结果时 overlay 还在
      Future.delayed(const Duration(milliseconds: 150), () {
        if (!_focusNode.hasFocus && mounted) {
          _hideOverlay();
          setState(() => _isSearchActive = false);
        }
      });
    }
  }

  void _onSearchChanged() {
    final query = _searchController.text.trim().toLowerCase();
    if (query.isEmpty) {
      _hideOverlay();
      setState(() => _results = []);
      return;
    }
    final results = _search(query);
    setState(() {
      _results = results;
      _highlightedIndex = results.isEmpty ? -1 : 0;
    });
    if (results.isNotEmpty) {
      _showOverlay();
    } else {
      _hideOverlay();
    }
  }

  List<SearchResult> _search(String query) {
    final results = <SearchResult>[];

    // 搜索任务名
    for (final task in widget.controller.tasks) {
      if (task.fileName.toLowerCase().contains(query) ||
          task.url.toLowerCase().contains(query)) {
        results.add(
          SearchResult(
            type: SearchResultType.task,
            title: task.fileName,
            subtitle: '${task.statusText} · ${task.sizeText}',
            icon: _iconForCategory(task.fileCategory),
            taskId: task.id,
          ),
        );
      }
      if (results.length >= 5) break; // 任务最多显示 5 条
    }

    // 搜索设置项
    for (final item in settingsSearchItems) {
      final matched =
          item.label.toLowerCase().contains(query) ||
          item.description.toLowerCase().contains(query) ||
          item.keywords.any((k) => k.toLowerCase().contains(query));
      if (matched) {
        results.add(
          SearchResult(
            type: SearchResultType.settings,
            title: item.label,
            subtitle: currentS.settingsSearchSubtitle(
              item.category.localizedLabel,
              item.description,
            ),
            icon: item.icon,
            settingsItem: item,
          ),
        );
      }
    }

    return results;
  }

  IconData _iconForCategory(FileCategory category) {
    return switch (category) {
      FileCategory.video => LucideIcons.film,
      FileCategory.audio => LucideIcons.music,
      FileCategory.document => LucideIcons.fileText,
      FileCategory.image => LucideIcons.image,
      FileCategory.archive => LucideIcons.archive,
      _ => LucideIcons.file,
    };
  }

  void _showOverlay() {
    if (!_overlayController.isShowing) {
      _overlayController.show();
    }
  }

  void _hideOverlay() {
    if (_overlayController.isShowing) {
      _overlayController.hide();
    }
  }

  void _selectResult(SearchResult result) {
    _searchController.clear();
    _focusNode.unfocus();
    _hideOverlay();

    if (result.type == SearchResultType.task && result.taskId != null) {
      widget.controller.selectTask(result.taskId);
    } else if (result.type == SearchResultType.settings &&
        result.settingsItem != null) {
      widget.onNavigateToSettings(result.settingsItem!);
    }
  }

  void _handleKeyEvent(KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) return;
    if (_results.isEmpty) return;

    if (event.logicalKey == LogicalKeyboardKey.arrowDown) {
      setState(() {
        _highlightedIndex = (_highlightedIndex + 1) % _results.length;
      });
    } else if (event.logicalKey == LogicalKeyboardKey.arrowUp) {
      setState(() {
        _highlightedIndex =
            (_highlightedIndex - 1 + _results.length) % _results.length;
      });
    } else if (event.logicalKey == LogicalKeyboardKey.enter) {
      if (_highlightedIndex >= 0 && _highlightedIndex < _results.length) {
        _selectResult(_results[_highlightedIndex]);
      }
    } else if (event.logicalKey == LogicalKeyboardKey.escape) {
      _searchController.clear();
      _focusNode.unfocus();
      _hideOverlay();
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    return TitleDragArea(
      child: Container(
        height: 40,
        padding: const EdgeInsets.only(left: 16),
        decoration: BoxDecoration(
          color: c.surface1,
          border: Border(bottom: BorderSide(color: c.border, width: 1)),
        ),
        child: Row(
          children: [
            // New download button
            ShadButton(
              onPressed: widget.onNewDownload,
              height: 30,
              padding: const EdgeInsets.symmetric(horizontal: 12),
              backgroundColor: c.accent,
              hoverBackgroundColor: c.accentHover,
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  const Icon(LucideIcons.plus, size: 14, color: Colors.white),
                  const SizedBox(width: 6),
                  Text(
                    s.newDownload,
                    style: const TextStyle(
                      fontSize: 13,
                      color: Colors.white,
                      fontWeight: FontWeight.w500,
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            // Search with overlay dropdown
            Flexible(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 320),
                child: OverlayPortal(
                  controller: _overlayController,
                  overlayChildBuilder: (_) => _buildSearchDropdown(c),
                  child: KeyboardListener(
                    focusNode: _keyboardFocusNode,
                    onKeyEvent: _handleKeyEvent,
                    child: Container(
                      decoration: BoxDecoration(
                        color: _isSearchActive ? c.inputFocusBg : c.bg,
                        borderRadius: m.brInput,
                        border: Border.all(
                          color: _isSearchActive
                              ? m.focusRing(c.accent)
                              : m.borderFade(c.border),
                          width: 1,
                        ),
                      ),
                      child: ShadInput(
                        key: _searchBoxKey,
                        controller: _searchController,
                        focusNode: _focusNode,
                        placeholder: Text(s.searchPlaceholder),
                        padding: const EdgeInsets.symmetric(
                          horizontal: 10,
                          vertical: 4,
                        ),
                        constraints: const BoxConstraints(
                          minHeight: 30,
                          maxHeight: 30,
                        ),
                        gap: 6,
                        leading: Icon(
                          LucideIcons.search,
                          size: 14,
                          color: _isSearchActive ? c.accent : c.textMuted,
                        ),
                        trailing: Visibility(
                          visible: !_isSearchActive,
                          maintainSize: true,
                          maintainAnimation: true,
                          maintainState: true,
                          child: Container(
                            padding: const EdgeInsets.symmetric(
                              horizontal: 5,
                              vertical: 1,
                            ),
                            decoration: BoxDecoration(
                              color: c.surface2,
                              borderRadius: m.brSm,
                              border: Border.all(color: c.border, width: 1),
                            ),
                            child: Text(
                              'Ctrl+F',
                              style: TextStyle(
                                fontSize: 10,
                                color: c.textMuted,
                                fontWeight: FontWeight.w500,
                              ),
                            ),
                          ),
                        ),
                        style: const TextStyle(fontSize: 13),
                        decoration: const ShadDecoration(
                          border: ShadBorder.none,
                          focusedBorder: ShadBorder.none,
                          secondaryFocusedBorder: ShadBorder.none,
                          secondaryBorder: ShadBorder.none,
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
            const Spacer(),
            // 工具按钮与窗口控制按钮统一由右上角 Positioned(right:0) 覆盖层
            // [WindowControls] 渲染，隐藏按钮后自动紧凑合并；
            // Windows/Linux：此处按可见按钮数动态预留空间，防止内容被遮挡
            if (!Platform.isMacOS) const _TitlebarOverlayReservation(),
          ],
        ),
      ),
    );
  }

  Widget _buildSearchDropdown(AppColors c) {
    final box = _searchBoxKey.currentContext?.findRenderObject() as RenderBox?;
    if (box == null) return const SizedBox.shrink();

    final offset = box.localToGlobal(Offset.zero);
    final size = box.size;
    final theme = ShadTheme.of(context);

    return Positioned(
      left: offset.dx,
      top: offset.dy + size.height + 4,
      width: size.width.clamp(240, 380),
      child: DefaultTextStyle(
        style: theme.textTheme.p.copyWith(
          color: theme.colorScheme.foreground,
        ),
        child: _SearchResultsPanel(
          results: _results,
          highlightedIndex: _highlightedIndex,
          colors: c,
          onSelect: _selectResult,
          onHover: (index) {
            setState(() => _highlightedIndex = index);
          },
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 搜索结果面板
// ─────────────────────────────────────────────

class _SearchResultsPanel extends StatelessWidget {
  final List<SearchResult> results;
  final int highlightedIndex;
  final AppColors colors;
  final ValueChanged<SearchResult> onSelect;
  final ValueChanged<int> onHover;

  const _SearchResultsPanel({
    required this.results,
    required this.highlightedIndex,
    required this.colors,
    required this.onSelect,
    required this.onHover,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final c = colors;
    // 按类型分组
    final taskResults = results
        .where((r) => r.type == SearchResultType.task)
        .toList();
    final settingsResults = results
        .where((r) => r.type == SearchResultType.settings)
        .toList();

    return Container(
      constraints: const BoxConstraints(maxHeight: 340),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brCard,
        border: Border.all(color: c.border, width: 1),
        boxShadow: [
          BoxShadow(
            // 刻意保留：搜索结果浮层专属投影，一次性装饰值，
            // 不同于对话框/toast 三档阴影语义，无独立可主题化诉求。
            color: Colors.black.withValues(alpha: 0.12),
            blurRadius: 12,
            offset: const Offset(0, 4),
          ),
        ],
      ),
      child: ClipRRect(
        borderRadius: m.brCard,
        child: SingleChildScrollView(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              if (taskResults.isNotEmpty) ...[
                _SectionLabel(
                  label: LocaleScope.of(context).searchGroupTasks,
                  colors: c,
                ),
                for (final r in taskResults)
                  _SearchResultItem(
                    result: r,
                    isHighlighted: results.indexOf(r) == highlightedIndex,
                    colors: c,
                    onTap: () => onSelect(r),
                    onHover: () => onHover(results.indexOf(r)),
                  ),
              ],
              if (taskResults.isNotEmpty && settingsResults.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 8),
                  child: Divider(height: 1, color: c.border),
                ),
              if (settingsResults.isNotEmpty) ...[
                _SectionLabel(
                  label: LocaleScope.of(context).searchGroupSettings,
                  colors: c,
                ),
                for (final r in settingsResults)
                  _SearchResultItem(
                    result: r,
                    isHighlighted: results.indexOf(r) == highlightedIndex,
                    colors: c,
                    onTap: () => onSelect(r),
                    onHover: () => onHover(results.indexOf(r)),
                  ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

class _SectionLabel extends StatelessWidget {
  final String label;
  final AppColors colors;

  const _SectionLabel({required this.label, required this.colors});

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 6, 12, 4),
      child: Text(
        label,
        style: TextStyle(
          fontSize: 10.5,
          fontWeight: FontWeight.w600,
          color: colors.textMuted,
          letterSpacing: 0.3,
        ),
      ),
    );
  }
}

class _SearchResultItem extends StatelessWidget {
  final SearchResult result;
  final bool isHighlighted;
  final AppColors colors;
  final VoidCallback onTap;
  final VoidCallback onHover;

  const _SearchResultItem({
    required this.result,
    required this.isHighlighted,
    required this.colors,
    required this.onTap,
    required this.onHover,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final c = colors;
    final r = result;
    final isSettings = r.type == SearchResultType.settings;

    return MouseRegion(
      onEnter: (_) => onHover(),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          margin: const EdgeInsets.symmetric(horizontal: 4, vertical: 1),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
          decoration: BoxDecoration(
            color: isHighlighted ? c.hoverBg : Colors.transparent,
            borderRadius: m.brMd,
          ),
          child: Row(
            children: [
              Container(
                width: 28,
                height: 28,
                decoration: BoxDecoration(
                  color: isSettings
                      ? m.soft(c.accent)
                      : c.surface2,
                  borderRadius: m.brMd,
                ),
                child: Icon(
                  r.icon,
                  size: 14,
                  color: isSettings ? c.accent : c.textSecondary,
                ),
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      r.title,
                      style: TextStyle(
                        fontSize: 12.5,
                        fontWeight: FontWeight.w500,
                        color: c.textPrimary,
                      ),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                    const SizedBox(height: 1),
                    Text(
                      r.subtitle,
                      style: TextStyle(fontSize: 10.5, color: c.textMuted),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ],
                ),
              ),
              if (isSettings)
                Icon(LucideIcons.arrowRight, size: 12, color: c.textMuted),
            ],
          ),
        ),
      ),
    );
  }
}

/// 窗口控制区。
///
/// macOS：Traffic light（红/黄/绿圆形按钮）放在左上角，工具按钮放在右上角。
/// Windows/Linux：工具按钮 + 窗口控制按钮（最小化/最大化/关闭）全部在右上角。
///
/// [showToolButtons] = true（默认，设置页使用）：显示工具按钮（暂停/恢复/设置/主题）。
/// [showToolButtons] = false（主页使用，工具按钮已移至 HeaderBar）：仅窗口按钮。
class WindowControls extends StatelessWidget {
  final DownloadController controller;
  final VoidCallback? onSettings;
  final bool isSettingsActive;
  final bool showToolButtons;

  const WindowControls({
    super.key,
    required this.controller,
    this.onSettings,
    this.isSettingsActive = false,
    this.showToolButtons = true,
  });

  @override
  Widget build(BuildContext context) {
    if (Platform.isMacOS) {
      // macOS：只渲染工具按钮（traffic light 单独由 MacosTrafficLights 组件负责）
      if (!showToolButtons) return const SizedBox.shrink();
      return _buildToolButtons(context);
    }
    // Windows / Linux：工具按钮 + 窗口控制按钮
    return _buildWindowsControls(context);
  }

  Widget _buildToolButtons(BuildContext context) {
    return SizedBox(
      height: 40,
      child: _TitlebarToolButtons(
        controller: controller,
        onSettings: onSettings,
        isSettingsActive: isSettingsActive,
      ),
    );
  }

  Widget _buildWindowsControls(BuildContext context) {
    final c = AppColors.of(context);
    return SizedBox(
      height: 40,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (showToolButtons)
            _TitlebarToolButtons(
              controller: controller,
              onSettings: onSettings,
              isSettingsActive: isSettingsActive,
            ),
          // 窗口控制按钮
          _WindowButton(
            icon: LucideIcons.minus,
            onPressed: () {
              logInfo('WindowCtrl', 'minimize clicked');
              windowManager.minimize();
            },
            colors: c,
          ),
          _WindowButton(
            icon: LucideIcons.square,
            iconSize: 12,
            onPressed: () async {
              logInfo('WindowCtrl', 'maximize/restore clicked');
              if (await windowManager.isMaximized()) {
                await windowManager.unmaximize();
              } else {
                await windowManager.maximize();
              }
            },
            colors: c,
          ),
          _WindowButton(
            icon: LucideIcons.x,
            onPressed: () {
              logInfo('WindowCtrl', 'close clicked');
              windowManager.close();
            },
            colors: c,
            isClose: true,
          ),
        ],
      ),
    );
  }
}



class _WindowButton extends StatefulWidget {
  final IconData icon;
  final VoidCallback onPressed;
  final AppColors colors;
  final bool isClose;
  final double iconSize;

  const _WindowButton({
    required this.icon,
    required this.onPressed,
    required this.colors,
    this.isClose = false,
    this.iconSize = 14,
  });

  @override
  State<_WindowButton> createState() => _WindowButtonState();
}

class _WindowButtonState extends State<_WindowButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: GestureDetector(
        onTap: widget.onPressed,
        child: Container(
          width: 40,
          height: 40,
          color: _isHovered
              ? (widget.isClose
                // 刻意保留：关闭按钮悬停危险态近不透明红底（Windows 标准），一次性字面量。
                    ? AppColors.red.withValues(alpha: 0.9)
                    : c.surface3)
              : Colors.transparent,
          child: Icon(
            widget.icon,
            size: widget.iconSize,
            color: _isHovered && widget.isClose
                ? Colors.white
                : c.textSecondary,
          ),
        ),
      ),
    );
  }
}

/// 标题栏工具按钮组（全部暂停/全部恢复/设置/主题切换）。
///
/// 每个按钮可在「设置 → 通用 → 标题栏按钮」中开关显示，
/// 也可右键按钮通过上下文菜单直接隐藏。
class _TitlebarToolButtons extends StatelessWidget {
  final DownloadController controller;
  final VoidCallback? onSettings;
  final bool isSettingsActive;


  const _TitlebarToolButtons({
    required this.controller,
    this.onSettings,
    this.isSettingsActive = false,
  });

  @override
  Widget build(BuildContext context) {
    final settings = SettingsProvider.globalInstance;
    if (settings == null) return _buildRow(context, null);
    return ListenableBuilder(
      listenable: settings,
      builder: (context, _) => _buildRow(context, settings),
    );
  }

  void _showHideMenu(
    BuildContext context,
    Offset position,
    VoidCallback onHide,
  ) {
    final c = AppColors.of(context);
    showContextMenu(
      context,
      position,
      items: [
        ContextMenuItem(
          icon: LucideIcons.eyeOff,
          label: LocaleScope.of(context).hideButton,
          color: c.textSecondary,
          action: onHide,
        ),
      ],
    );
  }

  Widget _buildRow(BuildContext context, SettingsProvider? settings) {
    final s = LocaleScope.of(context);
    final themeProvider = FluxDownApp.of(context);
    final showPause = settings?.showTitlebarPauseAll ?? true;
    final showResume = settings?.showTitlebarResumeAll ?? true;
    final showSettings = settings?.showTitlebarSettings ?? true;
    final showTheme = settings?.showTitlebarTheme ?? true;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (showPause)
          _ToolButton(
            icon: LucideIcons.circlePause,
            tooltip: s.pauseAll,
            onPressed: () => controller.pauseAll(),
            iconSize: 16,
            onSecondaryTapUp: settings == null
                ? null
                : (d) => _showHideMenu(
                    context,
                    d.globalPosition,
                    () => settings.setShowTitlebarPauseAll(false),
                  ),
          ),
        if (showResume)
          _ToolButton(
            icon: LucideIcons.circlePlay,
            tooltip: s.resumeAll,
            onPressed: () => controller.resumeAll(),
            iconSize: 16,
            onSecondaryTapUp: settings == null
                ? null
                : (d) => _showHideMenu(
                    context,
                    d.globalPosition,
                    () => settings.setShowTitlebarResumeAll(false),
                  ),
          ),
        if (showSettings)
          _ToolButton(
            icon: LucideIcons.settings,
            tooltip: s.settings,
            onPressed: () => onSettings?.call(),
            iconSize: 16,
            isActive: isSettingsActive,
            onSecondaryTapUp: settings == null
                ? null
                : (d) => _showHideMenu(
                    context,
                    d.globalPosition,
                    () => settings.setShowTitlebarSettings(false),
                  ),
          ),
        if (showTheme)
          _ToolButton(
            icon: themeProvider.isDark(context)
                ? LucideIcons.sun
                : LucideIcons.moon,
            tooltip: themeProvider.isDark(context)
                ? s.toggleToLight
                : s.toggleToDark,
            onPressed: () => themeProvider.toggleTheme(context),
            iconSize: 15,
            onSecondaryTapUp: settings == null
                ? null
                : (d) => _showHideMenu(
                    context,
                    d.globalPosition,
                    () => settings.setShowTitlebarTheme(false),
                  ),
          ),
      ],
    );
  }
}

/// Windows/Linux：HeaderBar 尾部占位，宽度 = 可见工具按钮数 × 40 +
/// 3 个窗口控制按钮（各 40px），与右上角覆盖层 [WindowControls] 严格对齐。
class _TitlebarOverlayReservation extends StatelessWidget {
  const _TitlebarOverlayReservation();

  /// 窗口控制按钮（最小化/最大化/关闭）总宽度
  static const double _windowButtonsWidth = 120;

  /// 单个工具按钮宽度
  static const double _toolButtonWidth = 40;

  @override
  Widget build(BuildContext context) {
    final settings = SettingsProvider.globalInstance;
    if (settings == null) {
      return const SizedBox(width: _windowButtonsWidth + _toolButtonWidth * 4);
    }
    return ListenableBuilder(
      listenable: settings,
      builder: (context, _) {
        final visibleTools = [
          settings.showTitlebarPauseAll,
          settings.showTitlebarResumeAll,
          settings.showTitlebarSettings,
          settings.showTitlebarTheme,
        ].where((v) => v).length;
        return SizedBox(
          width: _windowButtonsWidth + _toolButtonWidth * visibleTools,
        );
      },
    );
  }
}

/// 工具栏按钮（暂停、恢复、设置、主题切换等），与窗口控制按钮同组，hover 效果一致
class _ToolButton extends StatefulWidget {
  final IconData icon;
  final VoidCallback onPressed;
  final double iconSize;
  final String? tooltip;
  final bool isActive;
  final GestureTapUpCallback? onSecondaryTapUp;

  const _ToolButton({
    required this.icon,
    required this.onPressed,
    this.iconSize = 16,
    this.tooltip,
    this.isActive = false,
    this.onSecondaryTapUp,
  });

  @override
  State<_ToolButton> createState() => _ToolButtonState();
}

class _ToolButtonState extends State<_ToolButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final isActive = widget.isActive;
    Widget button = MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: GestureDetector(
        onTap: widget.onPressed,
        onSecondaryTapUp: widget.onSecondaryTapUp,
        child: Container(
          width: 40,
          height: 40,
          decoration: BoxDecoration(
            color: isActive
                ? c.accentBg
                : _isHovered
                ? c.surface3
                : Colors.transparent,
          ),
          child: Icon(
            widget.icon,
            size: widget.iconSize,
            color: isActive ? c.accent : c.textSecondary,
          ),
        ),
      ),
    );
    if (widget.tooltip != null) {
      button = ShadTooltip(
        waitDuration: const Duration(milliseconds: 400),
        showDuration: Duration.zero,
        builder: (_) => Text(widget.tooltip!),
        child: button,
      );
    }
    return button;
  }
}
