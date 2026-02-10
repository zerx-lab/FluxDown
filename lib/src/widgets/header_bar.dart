import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:window_manager/window_manager.dart';
import '../../main.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../pages/settings_page.dart';
import '../services/log_service.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
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

  /// 设置项搜索结果的目标分类
  final SettingsCategory? settingsCategory;

  const SearchResult({
    required this.type,
    required this.title,
    required this.subtitle,
    required this.icon,
    this.taskId,
    this.settingsCategory,
  });
}

// ─────────────────────────────────────────────
// HeaderBar
// ─────────────────────────────────────────────

class HeaderBar extends StatefulWidget {
  final VoidCallback onNewDownload;
  final DownloadController controller;
  final void Function(SettingsCategory category) onNavigateToSettings;

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
            settingsCategory: item.category,
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
        result.settingsCategory != null) {
      widget.onNavigateToSettings(result.settingsCategory!);
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
    return TitleDragArea(
      child: Container(
        height: 48,
        // right 预留 WindowControls 区域宽度：
        // 4 工具按钮(40*4) + 分隔线(9) + 3 窗口按钮(40*3) = 289
        padding: const EdgeInsets.only(left: 16, right: 289),
        decoration: BoxDecoration(
          color: c.surface1,
          border: Border(bottom: BorderSide(color: c.border, width: 1)),
        ),
        child: Row(
          children: [
            // New download button
            ShadButton(
              onPressed: widget.onNewDownload,
              backgroundColor: c.accent,
              hoverBackgroundColor: c.accentHover,
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  const Icon(LucideIcons.plus, size: 14, color: Colors.white),
                  const SizedBox(width: 6),
                  Text(
                    LocaleScope.of(context).newDownload,
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
                    focusNode: FocusNode(), // dummy node for key events
                    onKeyEvent: _handleKeyEvent,
                    child: ShadInput(
                      key: _searchBoxKey,
                      controller: _searchController,
                      focusNode: _focusNode,
                      placeholder: Text(
                        LocaleScope.of(context).searchPlaceholder,
                      ),
                      padding: const EdgeInsets.symmetric(
                        horizontal: 10,
                        vertical: 4,
                      ),
                      constraints: const BoxConstraints(
                        minHeight: 32,
                        maxHeight: 32,
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
                            borderRadius: BorderRadius.circular(4),
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
                        secondaryFocusedBorder: ShadBorder.none,
                        secondaryBorder: ShadBorder.none,
                      ),
                    ),
                  ),
                ),
              ),
            ),
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

    return Positioned(
      left: offset.dx,
      top: offset.dy + size.height + 4,
      width: size.width.clamp(240, 380),
      child: Material(
        color: Colors.transparent,
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
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: c.border, width: 1),
        boxShadow: [
          BoxShadow(
            color: Colors.black.withValues(alpha: 0.12),
            blurRadius: 12,
            offset: const Offset(0, 4),
          ),
        ],
      ),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(8),
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
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            children: [
              Container(
                width: 28,
                height: 28,
                decoration: BoxDecoration(
                  color: isSettings
                      ? c.accent.withValues(alpha: 0.1)
                      : c.surface2,
                  borderRadius: BorderRadius.circular(6),
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

/// 窗口右上角控制区：全部暂停 | 全部恢复 | 设置 | 主题切换 || 最小化 | 最大化 | 关闭
/// 通过 Positioned 悬浮在窗口右上角，确保这些按钮始终紧挨在一起
class WindowControls extends StatelessWidget {
  final DownloadController controller;
  final VoidCallback? onSettings;
  final bool isSettingsActive;

  const WindowControls({
    super.key,
    required this.controller,
    this.onSettings,
    this.isSettingsActive = false,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final themeProvider = FluxDownApp.of(context);
    return SizedBox(
      height: 48,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          // 全部暂停
          _ToolButton(
            icon: LucideIcons.circlePause,
            tooltip: LocaleScope.of(context).pauseAll,
            onPressed: () => controller.pauseAll(),
            iconSize: 16,
          ),
          // 全部恢复
          _ToolButton(
            icon: LucideIcons.circlePlay,
            tooltip: LocaleScope.of(context).resumeAll,
            onPressed: () => controller.resumeAll(),
            iconSize: 16,
          ),
          // 设置按钮
          _ToolButton(
            icon: LucideIcons.settings,
            tooltip: LocaleScope.of(context).settings,
            onPressed: () => onSettings?.call(),
            iconSize: 16,
            isActive: isSettingsActive,
          ),
          // 主题切换按钮
          _ToolButton(
            icon: themeProvider.isDark(context)
                ? LucideIcons.sun
                : LucideIcons.moon,
            tooltip: themeProvider.isDark(context)
                ? LocaleScope.of(context).toggleToLight
                : LocaleScope.of(context).toggleToDark,
            onPressed: () => themeProvider.toggleTheme(context),
            iconSize: 15,
          ),
          // 分隔线
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 4),
            child: Container(width: 1, height: 16, color: c.border),
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
          height: 48,
          color: _isHovered
              ? (widget.isClose
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

/// 工具栏按钮（暂停、恢复、设置、主题切换等），与窗口控制按钮同组，hover 效果一致
class _ToolButton extends StatefulWidget {
  final IconData icon;
  final VoidCallback onPressed;
  final double iconSize;
  final String? tooltip;
  final bool isActive;

  const _ToolButton({
    required this.icon,
    required this.onPressed,
    this.iconSize = 16,
    this.tooltip,
    this.isActive = false,
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
        child: Container(
          width: 40,
          height: 48,
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
