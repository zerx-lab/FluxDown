import 'dart:async';
import 'dart:io';
import 'dart:math';

import 'package:file_selector/file_selector.dart';
import '../services/file_picker_service.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../../main.dart';
import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/custom_category.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/settings_provider.dart';
import '../services/app_icon_service.dart';
import '../services/floating_ball/floating_ball_service.dart';
import '../services/log_service.dart';
import '../services/update_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import '../theme/flux_theme_tokens.dart';
import '../theme/theme_provider.dart';
import '../widgets/category_edit_dialog.dart';
import '../widgets/dir_picker_field.dart';
import '../widgets/thread_selector.dart';
import '../widgets/title_drag_area.dart';

// ─────────────────────────────────────────────
// 设置分类枚举
// ─────────────────────────────────────────────

enum SettingsCategory {
  general(icon: LucideIcons.settings2),
  appearance(icon: LucideIcons.palette),
  download(icon: LucideIcons.download),
  bt(icon: LucideIcons.magnet),
  ed2k(icon: LucideIcons.share2),
  proxy(icon: LucideIcons.globe),
  apiService(icon: LucideIcons.server),
  about(icon: LucideIcons.info);

  final IconData icon;

  const SettingsCategory({required this.icon});
}

extension SettingsCategoryI18n on SettingsCategory {
  String get localizedLabel {
    final s = currentS;
    return switch (this) {
      SettingsCategory.general => s.settingsCatGeneral,
      SettingsCategory.appearance => s.settingsCatAppearance,
      SettingsCategory.download => s.settingsCatDownload,
      SettingsCategory.bt => s.settingsCatBt,
      SettingsCategory.ed2k => s.settingsCatEd2k,
      SettingsCategory.proxy => s.settingsCatProxy,
      SettingsCategory.apiService => s.settingsCatApiService,
      SettingsCategory.about => s.settingsCatAbout,
    };
  }

  String get localizedDesc {
    final s = currentS;
    return switch (this) {
      SettingsCategory.general => s.settingsCatGeneralDesc,
      SettingsCategory.appearance => s.settingsCatAppearanceDesc,
      SettingsCategory.download => s.settingsCatDownloadDesc,
      SettingsCategory.bt => s.settingsCatBtDesc,
      SettingsCategory.ed2k => s.settingsCatEd2kDesc,
      SettingsCategory.proxy => s.settingsCatProxyDesc,
      SettingsCategory.apiService => s.settingsCatApiServiceDesc,
      SettingsCategory.about => s.settingsCatAboutDesc,
    };
  }
}

/// 设置项搜索元数据 — 每个设置项对应的分类 + 搜索关键词
class SettingsSearchItem {
  final SettingsCategory category;
  final String label;
  final String description;
  final List<String> keywords;
  final IconData icon;

  SettingsSearchItem({
    required this.category,
    required this.label,
    required this.description,
    required this.keywords,
    required this.icon,
  });

  /// 与查询串（已 toLowerCase）匹配：标签/描述/关键词任一命中。
  bool matches(String query) =>
      label.toLowerCase().contains(query) ||
      description.toLowerCase().contains(query) ||
      keywords.any((k) => k.toLowerCase().contains(query));
}

/// 所有可搜索的设置项列表
List<SettingsSearchItem> get settingsSearchItems {
  final s = currentS;
  return [
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.autoStartup,
      description: s.autoStartupDesc,
      keywords: s.searchKeywordsAutoStartup,
      icon: LucideIcons.power,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.closeToTray,
      description: s.closeToTrayDesc,
      keywords: s.searchKeywordsCloseToTray,
      icon: LucideIcons.panelBottomClose,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.floatingBall,
      description: s.floatingBallDesc,
      keywords: s.searchKeywordsFloatingBall,
      icon: LucideIcons.circleDot,
    ),
    if (Platform.isLinux)
      SettingsSearchItem(
        category: SettingsCategory.general,
        label: s.clipboardWatch,
        description: s.clipboardWatchDesc,
        keywords: s.searchKeywordsClipboardWatch,
        icon: LucideIcons.clipboard,
      ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.torrentFileAssociation,
      description: s.torrentFileAssociationDesc,
      keywords: s.searchKeywordsFileAssoc,
      icon: LucideIcons.fileType,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.notifyOnComplete,
      description: s.notifyOnCompleteDesc,
      keywords: s.searchKeywordsNotifyOnComplete,
      icon: LucideIcons.bellRing,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.keepAwakeWhileDownloading,
      description: s.keepAwakeWhileDownloadingDesc,
      keywords: s.searchKeywordsKeepAwake,
      icon: LucideIcons.coffee,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.sidebarVisibility,
      description: s.sidebarVisibilityDesc,
      keywords: s.searchKeywordsSidebarVisibility,
      icon: LucideIcons.panelLeft,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.titlebarButtons,
      description: s.titlebarButtonsDesc,
      keywords: s.searchKeywordsTitlebarButtons,
      icon: LucideIcons.panelTop,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.customCategories,
      description: s.customCategoriesDesc,
      keywords: s.searchKeywordsCustomCategories,
      icon: LucideIcons.layoutList,
    ),
    SettingsSearchItem(
      category: SettingsCategory.appearance,
      label: s.language,
      description: s.languageDesc,
      keywords: s.searchKeywordsLanguage,
      icon: LucideIcons.languages,
    ),
    SettingsSearchItem(
      category: SettingsCategory.appearance,
      label: s.themeMode,
      description: s.themeModeDesc,
      keywords: s.searchKeywordsThemeMode,
      icon: LucideIcons.sunMoon,
    ),
    SettingsSearchItem(
      category: SettingsCategory.appearance,
      label: s.themeColor,
      description: s.themeColorDesc,
      keywords: s.searchKeywordsThemeColor,
      icon: LucideIcons.palette,
    ),
    SettingsSearchItem(
      category: SettingsCategory.appearance,
      label: s.uiScale,
      description: s.uiScaleDesc,
      keywords: s.searchKeywordsUiScale,
      icon: LucideIcons.maximize,
    ),
    if (Platform.isWindows)
      SettingsSearchItem(
        category: SettingsCategory.appearance,
        label: s.appIcon,
        description: s.appIconDesc,
        keywords: s.searchKeywordsAppIcon,
        icon: LucideIcons.image,
      ),
    SettingsSearchItem(
      category: SettingsCategory.download,
      label: s.defaultSaveDir,
      description: s.defaultSaveDirDesc,
      keywords: s.searchKeywordsSaveDir,
      icon: LucideIcons.folderOpen,
    ),
    SettingsSearchItem(
      category: SettingsCategory.download,
      label: s.rememberLastSaveDir,
      description: s.rememberLastSaveDirDesc,
      keywords: s.searchKeywordsSaveDir,
      icon: LucideIcons.history,
    ),
    SettingsSearchItem(
      category: SettingsCategory.download,
      label: s.silentDownload,
      description: s.silentDownloadDesc,
      keywords: s.searchKeywordsSilentDownload,
      icon: LucideIcons.bellOff,
    ),
    SettingsSearchItem(
      category: SettingsCategory.download,
      label: s.defaultThreads,
      description: s.defaultThreadsDesc,
      keywords: s.searchKeywordsThreads,
      icon: LucideIcons.layers,
    ),
    SettingsSearchItem(
      category: SettingsCategory.download,
      label: s.maxConcurrent,
      description: s.maxConcurrentDesc,
      keywords: s.searchKeywordsConcurrent,
      icon: LucideIcons.listOrdered,
    ),
    SettingsSearchItem(
      category: SettingsCategory.download,
      label: s.speedLimit,
      description: s.speedLimitDesc,
      keywords: s.searchKeywordsSpeedLimit,
      icon: LucideIcons.gauge,
    ),
    SettingsSearchItem(
      category: SettingsCategory.download,
      label: s.userAgent,
      description: s.userAgentDesc,
      keywords: s.searchKeywordsUserAgent,
      icon: LucideIcons.userCheck,
    ),
    SettingsSearchItem(
      category: SettingsCategory.about,
      label: s.softwareUpdate,
      description: s.checkUpdateDesc,
      keywords: s.searchKeywordsUpdate,
      icon: LucideIcons.refreshCw,
    ),
    SettingsSearchItem(
      category: SettingsCategory.bt,
      label: s.btSettings,
      description: s.btSettingsDesc,
      keywords: s.searchKeywordsBtSettings,
      icon: LucideIcons.magnet,
    ),
    SettingsSearchItem(
      category: SettingsCategory.ed2k,
      label: s.ed2kSettings,
      description: s.ed2kSettingsDesc,
      keywords: s.searchKeywordsEd2kSettings,
      icon: LucideIcons.share2,
    ),
    SettingsSearchItem(
      category: SettingsCategory.proxy,
      label: s.proxySettings,
      description: s.proxySettingsDesc,
      keywords: s.searchKeywordsProxy,
      icon: LucideIcons.globe,
    ),
    SettingsSearchItem(
      category: SettingsCategory.apiService,
      label: s.apiServiceEnable,
      description: s.apiServiceEnableDesc,
      keywords: s.searchKeywordsApiService,
      icon: LucideIcons.server,
    ),
    SettingsSearchItem(
      category: SettingsCategory.about,
      label: s.logExport,
      description: s.logExportDesc,
      keywords: s.searchKeywordsLogExport,
      icon: LucideIcons.fileText,
    ),
  ];
}

// ─────────────────────────────────────────────
// 设置页面（带侧边栏导航）
// ─────────────────────────────────────────────

class SettingsPage extends StatefulWidget {
  final VoidCallback onBack;
  final SettingsProvider settingsProvider;
  final DownloadController? downloadController;
  final SettingsCategory? initialCategory;

  /// 从首页搜索跳转进来时携带的高亮项：切到其分类并闪烁定位对应设置卡片。
  final SettingsSearchItem? initialHighlight;

  const SettingsPage({
    super.key,
    required this.onBack,
    required this.settingsProvider,
    this.downloadController,
    this.initialCategory,
    this.initialHighlight,
  });

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

/// 一次「定位到设置项」的高亮请求。
/// [seq] 单调递增，保证连续选择同一项也能重新触发动画。
class SettingsHighlightRequest {
  final int seq;
  final String label;
  final String description;

  const SettingsHighlightRequest({
    required this.seq,
    required this.label,
    required this.description,
  });

  /// 卡片是否为本请求的目标（标签或描述与搜索元数据一致即命中）。
  bool targets(String cardLabel, String cardDescription) =>
      cardLabel == label ||
      (description.isNotEmpty && cardDescription == description);
}

/// 向下传递当前高亮请求；_SettingCard 据此自行滚动定位 + 闪烁。
/// 卡片消费（触发动画）后回调 [onConsumed]，源头清空请求，
/// 防止用户切走再切回时新建的卡片 State 重复消费同一请求（幽灵重闪）。
class _HighlightScope extends InheritedWidget {
  final SettingsHighlightRequest? request;
  final ValueChanged<int> onConsumed;

  const _HighlightScope({
    required this.request,
    required this.onConsumed,
    required super.child,
  });

  static _HighlightScope? of(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<_HighlightScope>();

  @override
  bool updateShouldNotify(_HighlightScope old) => old.request != request;
}

class _SettingsPageState extends State<SettingsPage> {
  late SettingsCategory _selected;

  SettingsHighlightRequest? _highlight;
  int _highlightSeq = 0;

  @override
  void initState() {
    super.initState();
    final hl = widget.initialHighlight;
    _selected =
        hl?.category ?? widget.initialCategory ?? SettingsCategory.general;
    if (hl != null) {
      _highlight = SettingsHighlightRequest(
        seq: ++_highlightSeq,
        label: hl.label,
        description: hl.description,
      );
    }
  }

  /// 搜索结果选中：切换分类并下发高亮请求。
  void _onSearchSelect(SettingsSearchItem item) {
    setState(() {
      _selected = item.category;
      _highlight = SettingsHighlightRequest(
        seq: ++_highlightSeq,
        label: item.label,
        description: item.description,
      );
    });
  }

  /// 卡片已消费高亮请求 → 清空，防止重复触发。
  void _onHighlightConsumed(int seq) {
    if (_highlight?.seq != seq) return;
    // 消费回调发生在 postFrame，可以安全 setState
    setState(() => _highlight = null);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Column(
      children: [
        // 顶部标题栏
        TitleDragArea(
          child: Container(
            height: 40,
            decoration: BoxDecoration(
              color: c.surface1,
              border: Border(bottom: BorderSide(color: c.border, width: 1)),
            ),
            child: Platform.isMacOS
                ? Stack(
                    children: [
                      // 定位到侧边栏(180px)+分隔线(1px)右边
                      Positioned(
                        left: 181,
                        top: 0,
                        bottom: 0,
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          crossAxisAlignment: CrossAxisAlignment.center,
                          children: [
                            ShadButton.ghost(
                              onPressed: widget.onBack,
                              size: ShadButtonSize.sm,
                              child: Row(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  Icon(
                                    LucideIcons.arrowLeft,
                                    size: 14,
                                    color: c.textSecondary,
                                  ),
                                  const SizedBox(width: 6),
                                  Text(
                                    LocaleScope.of(context).back,
                                    style: TextStyle(
                                      fontSize: 13,
                                      color: c.textSecondary,
                                    ),
                                  ),
                                ],
                              ),
                            ),
                            const SizedBox(width: 12),
                            Text(
                              LocaleScope.of(context).settings,
                              style: TextStyle(
                                fontSize: 14,
                                fontWeight: FontWeight.w600,
                                color: c.textPrimary,
                              ),
                            ),
                          ],
                        ),
                      ),
                    ],
                  )
                : Padding(
                    padding: const EdgeInsets.only(left: 12, right: 289),
                    child: Row(
                      children: [
                        ShadButton.ghost(
                          onPressed: widget.onBack,
                          size: ShadButtonSize.sm,
                          child: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Icon(
                                LucideIcons.arrowLeft,
                                size: 14,
                                color: c.textSecondary,
                              ),
                              const SizedBox(width: 6),
                              Text(
                                LocaleScope.of(context).back,
                                style: TextStyle(
                                  fontSize: 13,
                                  color: c.textSecondary,
                                ),
                              ),
                            ],
                          ),
                        ),
                        const SizedBox(width: 12),
                        Text(
                          LocaleScope.of(context).settings,
                          style: TextStyle(
                            fontSize: 14,
                            fontWeight: FontWeight.w600,
                            color: c.textPrimary,
                          ),
                        ),
                      ],
                    ),
                  ),
          ),
        ),
        // 主体：侧边栏 + 内容区
        Expanded(
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // 左侧导航栏
              _SettingsSidebar(
                selected: _selected,
                onSelect: (cat) => setState(() => _selected = cat),
                onSearchSelect: _onSearchSelect,
              ),
              // 分隔线
              Container(width: 1, color: c.border),
              // 右侧内容区
              Expanded(
                child: _HighlightScope(
                  request: _highlight,
                  onConsumed: _onHighlightConsumed,
                  child: _SettingsContent(
                    category: _selected,
                    settingsProvider: widget.settingsProvider,
                    downloadController: widget.downloadController,
                  ),
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 设置侧边栏导航
// ─────────────────────────────────────────────

class _SettingsSidebar extends StatefulWidget {
  final SettingsCategory selected;
  final ValueChanged<SettingsCategory> onSelect;
  final ValueChanged<SettingsSearchItem> onSearchSelect;

  const _SettingsSidebar({
    required this.selected,
    required this.onSelect,
    required this.onSearchSelect,
  });

  @override
  State<_SettingsSidebar> createState() => _SettingsSidebarState();
}

class _SettingsSidebarState extends State<_SettingsSidebar> {
  final _searchController = TextEditingController();
  final _focusNode = FocusNode();
  final _keyboardFocusNode = FocusNode(skipTraversal: true);
  String _query = '';

  @override
  void initState() {
    super.initState();
    _searchController.addListener(() {
      final q = _searchController.text.trim().toLowerCase();
      if (q != _query) setState(() => _query = q);
    });
  }

  @override
  void dispose() {
    _searchController.dispose();
    _focusNode.dispose();
    _keyboardFocusNode.dispose();
    super.dispose();
  }

  List<SettingsSearchItem> get _results => _query.isEmpty
      ? const []
      : settingsSearchItems.where((i) => i.matches(_query)).toList();

  void _select(SettingsSearchItem item) {
    widget.onSearchSelect(item);
    _searchController.clear();
    _focusNode.unfocus();
  }

  /// Esc 清空并失焦。用 Focus.onKeyEvent 返回 handled，
  /// 阻止事件继续冒泡到外层快捷键（KeyboardListener 无拦截语义）。
  KeyEventResult _handleKey(FocusNode node, KeyEvent event) {
    if (event is KeyDownEvent &&
        event.logicalKey == LogicalKeyboardKey.escape) {
      _searchController.clear();
      _focusNode.unfocus();
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  /// Enter 提交：选中第一条搜索结果。
  void _onSubmitted(String _) {
    final results = _results;
    if (results.isNotEmpty) _select(results.first);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final searching = _query.isNotEmpty;
    final results = _results;

    return Container(
      width: 180,
      color: c.surface1,
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // 搜索框
          Container(
            margin: const EdgeInsets.only(bottom: 10),
            decoration: BoxDecoration(
              color: c.bg,
              borderRadius: m.brInput,
              border: Border.all(
                color: searching
                    ? m.focusRing(c.accent)
                    : m.borderFade(c.border),
                width: 1,
              ),
            ),
            child: Focus(
              focusNode: _keyboardFocusNode,
              onKeyEvent: _handleKey,
              child: ShadInput(
                controller: _searchController,
                focusNode: _focusNode,
                placeholder: Text(
                  s.settingsSearchHint,
                  style: const TextStyle(fontSize: 12),
                ),
                padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
                constraints: const BoxConstraints(minHeight: 28, maxHeight: 28),
                gap: 5,
                leading: Icon(
                  LucideIcons.search,
                  size: 13,
                  color: searching ? c.accent : c.textMuted,
                ),
                style: const TextStyle(fontSize: 12),
                decoration: const ShadDecoration(
                  border: ShadBorder.none,
                  focusedBorder: ShadBorder.none,
                ),
                onSubmitted: _onSubmitted,
              ),
            ),
          ),
          // 搜索中 → 结果列表；否则 → 分类导航
          if (searching)
            Expanded(
              child: results.isEmpty
                  ? Padding(
                      padding: const EdgeInsets.only(top: 16),
                      child: Text(
                        s.settingsSearchNoResults,
                        textAlign: TextAlign.center,
                        style: TextStyle(fontSize: 12, color: c.textMuted),
                      ),
                    )
                  : ListView.builder(
                      itemCount: results.length,
                      itemBuilder: (context, i) => _SearchResultItem(
                        item: results[i],
                        onTap: () => _select(results[i]),
                      ),
                    ),
            )
          else
            for (final cat in SettingsCategory.values)
              _SettingsNavItem(
                icon: cat.icon,
                label: cat.localizedLabel,
                isSelected: widget.selected == cat,
                onTap: () => widget.onSelect(cat),
              ),
        ],
      ),
    );
  }
}

/// 侧边栏搜索结果项：图标 + 标签 + 所属分类
class _SearchResultItem extends StatefulWidget {
  final SettingsSearchItem item;
  final VoidCallback onTap;

  const _SearchResultItem({required this.item, required this.onTap});

  @override
  State<_SearchResultItem> createState() => _SearchResultItemState();
}

class _SearchResultItemState extends State<_SearchResultItem> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          margin: const EdgeInsets.only(bottom: 2),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
          decoration: BoxDecoration(
            color: _isHovered ? c.hoverBg : Colors.transparent,
            borderRadius: m.brMd,
          ),
          child: Row(
            children: [
              Icon(widget.item.icon, size: 14, color: c.textSecondary),
              const SizedBox(width: 8),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      widget.item.label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 12,
                        fontWeight: FontWeight.w500,
                        color: c.textPrimary,
                      ),
                    ),
                    Text(
                      widget.item.category.localizedLabel,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(fontSize: 10.5, color: c.textMuted),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _SettingsNavItem extends StatefulWidget {
  final IconData icon;
  final String label;
  final bool isSelected;
  final VoidCallback onTap;

  const _SettingsNavItem({
    required this.icon,
    required this.label,
    required this.isSelected,
    required this.onTap,
  });

  @override
  State<_SettingsNavItem> createState() => _SettingsNavItemState();
}

class _SettingsNavItemState extends State<_SettingsNavItem> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final selected = widget.isSelected;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 150),
          margin: const EdgeInsets.only(bottom: 2),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
          decoration: BoxDecoration(
            color: selected
                ? c.accentBg
                : _isHovered
                ? c.hoverBg
                : c.hoverBg.withValues(alpha: 0),
            borderRadius: m.brMd,
          ),
          child: Row(
            children: [
              Icon(
                widget.icon,
                size: 15,
                color: selected ? c.accent : c.textSecondary,
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  widget.label,
                  style: TextStyle(
                    fontSize: 13,
                    color: selected ? c.accent : c.textPrimary,
                    fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                  ),
                ),
              ),
              if (selected)
                Container(
                  width: 3,
                  height: 14,
                  decoration: BoxDecoration(
                    color: c.accent,
                    borderRadius: m.brXs,
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 设置内容区
// ─────────────────────────────────────────────

class _SettingsContent extends StatefulWidget {
  final SettingsCategory category;
  final SettingsProvider settingsProvider;
  final DownloadController? downloadController;

  const _SettingsContent({
    required this.category,
    required this.settingsProvider,
    this.downloadController,
  });

  @override
  State<_SettingsContent> createState() => _SettingsContentState();
}

class _SettingsContentState extends State<_SettingsContent> {
  final _scrollController = ScrollController();

  @override
  void didUpdateWidget(covariant _SettingsContent oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 切换分类时回到顶部，避免沿用上一分类的滚动位置
    if (widget.category != oldWidget.category &&
        _scrollController.hasClients) {
      _scrollController.jumpTo(0);
    }
  }

  @override
  void dispose() {
    _scrollController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final category = widget.category;
    final settingsProvider = widget.settingsProvider;
    final downloadController = widget.downloadController;
    return SingleChildScrollView(
      controller: _scrollController,
      padding: const EdgeInsets.symmetric(horizontal: 36, vertical: 24),
      child: Align(
        alignment: Alignment.topCenter,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 560),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              _SectionHeader(category: category),
              const SizedBox(height: 20),
              AnimatedSwitcher(
                duration: const Duration(milliseconds: 200),
                layoutBuilder: (currentChild, previousChildren) {
                  return Stack(
                    alignment: Alignment.topCenter,
                    children: [...previousChildren, ?currentChild],
                  );
                },
                child: switch (category) {
                  SettingsCategory.general => _GeneralContent(
                    key: const ValueKey('general'),
                    settingsProvider: settingsProvider,
                  ),
                  SettingsCategory.appearance => const _AppearanceContent(
                    key: ValueKey('appearance'),
                  ),
                  SettingsCategory.download => _DownloadContent(
                    key: ValueKey('download'),
                    settingsProvider: settingsProvider,
                    downloadController: downloadController,
                  ),
                  SettingsCategory.bt => _BtContent(
                    key: const ValueKey('bt'),
                    settingsProvider: settingsProvider,
                  ),
                  SettingsCategory.ed2k => _Ed2kContent(
                    key: const ValueKey('ed2k'),
                    settingsProvider: settingsProvider,
                  ),
                  SettingsCategory.proxy => _ProxyContent(
                    key: const ValueKey('proxy'),
                    settingsProvider: settingsProvider,
                  ),
                  SettingsCategory.apiService => _ApiServiceContent(
                    key: const ValueKey('apiService'),
                    settingsProvider: settingsProvider,
                  ),
                  SettingsCategory.about => _AboutContent(
                    key: const ValueKey('about'),
                    settingsProvider: settingsProvider,
                  ),
                },
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 分类标题头
// ─────────────────────────────────────────────

class _SectionHeader extends StatelessWidget {
  final SettingsCategory category;

  const _SectionHeader({required this.category});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          category.localizedLabel,
          style: TextStyle(
            fontSize: 16,
            fontWeight: FontWeight.w600,
            color: c.textPrimary,
          ),
        ),
        const SizedBox(height: 4),
        Text(
          category.localizedDesc,
          style: TextStyle(fontSize: 12, color: c.textMuted),
        ),
        const SizedBox(height: 14),
        Divider(height: 1, color: m.borderFade(c.border)),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 设置卡片：每个设置项的统一容器
// ─────────────────────────────────────────────

class _SettingCard extends StatefulWidget {
  final String label;
  final String description;
  final Widget child;
  final bool vertical;

  const _SettingCard({
    required this.label,
    required this.description,
    required this.child,
    this.vertical = false,
  });

  @override
  State<_SettingCard> createState() => _SettingCardState();
}

/// 高亮消费公共逻辑：监听 _HighlightScope，命中后滚动定位 + 闪烁。
/// 宿主 State 通过 [highlightLabel]/[highlightDescription] 声明自己的匹配键，
/// 通过 [flashing] 读取当前闪烁态渲染高亮样式。
mixin _HighlightConsumer<T extends StatefulWidget> on State<T> {
  int _consumedSeq = 0;
  bool _flashing = false;
  Timer? _flashTimer;

  String get highlightLabel;
  String get highlightDescription;

  bool get flashing => _flashing;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final scope = _HighlightScope.of(context);
    final req = scope?.request;
    if (scope == null || req == null || req.seq == _consumedSeq) return;
    if (!req.targets(highlightLabel, highlightDescription)) return;
    _consumedSeq = req.seq;
    // 等本帧布局完成后再滚动定位（分类切换 AnimatedSwitcher 期间布局已存在）
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      Scrollable.ensureVisible(
        context,
        alignment: 0.2,
        duration: const Duration(milliseconds: 300),
        curve: Curves.easeOutCubic,
      );
      _startFlash();
      // 上报消费 → 源头清空请求，防止分类切走再切回时幽灵重闪
      scope.onConsumed(req.seq);
    });
  }

  /// 闪烁 3 次：300ms 周期亮/灭交替，总时长约 1.5s
  void _startFlash() {
    _flashTimer?.cancel();
    var ticks = 0;
    setState(() => _flashing = true);
    _flashTimer = Timer.periodic(const Duration(milliseconds: 300), (t) {
      ticks++;
      if (ticks >= 5 || !mounted) {
        t.cancel();
        _flashTimer = null;
        if (mounted) setState(() => _flashing = false);
        return;
      }
      setState(() => _flashing = ticks.isEven);
    });
  }

  @override
  void dispose() {
    _flashTimer?.cancel();
    super.dispose();
  }
}

/// 无边框高亮区域：包裹非 _SettingCard 的设置小节（如「侧边栏显示」），
/// 使其同样支持搜索定位 + 闪烁高亮。
class _HighlightRegion extends StatefulWidget {
  final String label;
  final String description;
  final Widget child;

  const _HighlightRegion({
    required this.label,
    required this.description,
    required this.child,
  });

  @override
  State<_HighlightRegion> createState() => _HighlightRegionState();
}

class _HighlightRegionState extends State<_HighlightRegion>
    with _HighlightConsumer {
  @override
  String get highlightLabel => widget.label;
  @override
  String get highlightDescription => widget.description;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return AnimatedContainer(
      duration: const Duration(milliseconds: 250),
      curve: Curves.easeOut,
      decoration: BoxDecoration(
        color: flashing ? m.subtle(c.accent) : Colors.transparent,
        borderRadius: m.brDialog,
      ),
      child: widget.child,
    );
  }
}

class _SettingCardState extends State<_SettingCard> with _HighlightConsumer {
  @override
  String get highlightLabel => widget.label;
  @override
  String get highlightDescription => widget.description;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return AnimatedContainer(
      duration: const Duration(milliseconds: 250),
      curve: Curves.easeOut,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
      decoration: BoxDecoration(
        color: _flashing ? m.subtle(c.accent) : c.surface1,
        borderRadius: m.brDialog,
        border: Border.all(
          color: _flashing
              ? m.emphasis(c.accent)
              : m.borderMedium(c.border),
          width: 1,
        ),
      ),
      child: widget.vertical
          ? Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  widget.label,
                  style: TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w500,
                    color: c.textPrimary,
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  widget.description,
                  style: TextStyle(fontSize: 11.5, color: c.textMuted),
                ),
                const SizedBox(height: 12),
                widget.child,
              ],
            )
          : Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        widget.label,
                        style: TextStyle(
                          fontSize: 13,
                          fontWeight: FontWeight.w500,
                          color: c.textPrimary,
                        ),
                      ),
                      const SizedBox(height: 2),
                      Text(
                        widget.description,
                        style: TextStyle(fontSize: 11.5, color: c.textMuted),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 16),
                widget.child,
              ],
            ),
    );
  }
}

// ─────────────────────────────────────────────
// 通用设置
// ─────────────────────────────────────────────

class _GeneralContent extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _GeneralContent({super.key, required this.settingsProvider});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: settingsProvider,
      builder: (context, _) {
        return Column(
          children: [
            _SettingCard(
              label: LocaleScope.of(context).autoStartup,
              description: LocaleScope.of(context).autoStartupDesc,
              child: ShadSwitch(
                value: settingsProvider.autoStartup,
                onChanged: (v) async {
                  final ok = await settingsProvider.setAutoStartup(v);
                  if (!ok && context.mounted) {
                    showShadDialog(
                      context: context,
                      barrierColor: AppColors.of(context).dialogBarrier,
                      animateIn: const [],
                      animateOut: const [],
                      builder: (ctx) => ShadDialog.alert(
                        title: Text(LocaleScope.of(ctx).settingFailed),
                        description: Text(
                          LocaleScope.of(ctx).autoStartupFailedDesc,
                        ),
                        actions: [
                          ShadButton(
                            child: Text(LocaleScope.of(ctx).confirm),
                            onPressed: () => Navigator.of(ctx).pop(),
                          ),
                        ],
                      ),
                    );
                  }
                },
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).closeToTray,
              description: LocaleScope.of(context).closeToTrayDesc,
              child: ShadSwitch(
                value: settingsProvider.closeToTray,
                onChanged: (v) => settingsProvider.setCloseToTray(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).floatingBall,
              description: FloatingBallService.instance.isDegraded
                  ? LocaleScope.of(context).floatingBallWaylandUnsupported
                  : LocaleScope.of(context).floatingBallDesc,
              child: ShadSwitch(
                value: settingsProvider.floatingBallEnabled,
                enabled: !FloatingBallService.instance.isDegraded,
                onChanged: (v) => FloatingBallService.instance.setEnabled(v),
              ),
            ),
            if (settingsProvider.floatingBallEnabled &&
                !FloatingBallService.instance.isDegraded) ...[
              const SizedBox(height: 10),
              _SettingCard(
                label: LocaleScope.of(context).floatingBallActiveOnly,
                description:
                    LocaleScope.of(context).floatingBallActiveOnlyDesc,
                child: ShadSwitch(
                  value: settingsProvider.floatingBallActiveOnly,
                  onChanged: (v) {
                    settingsProvider.setFloatingBallActiveOnly(v);
                    FloatingBallService.instance.refreshVisibility();
                  },
                ),
              ),
            ],
            if (Platform.isLinux &&
                FloatingBallService.instance.isDegraded) ...[
              const SizedBox(height: 10),
              _SettingCard(
                label: LocaleScope.of(context).clipboardWatch,
                description: LocaleScope.of(context).clipboardWatchDesc,
                child: ShadSwitch(
                  value: settingsProvider.clipboardWatchEnabled,
                  onChanged: (v) =>
                      settingsProvider.setClipboardWatchEnabled(v),
                ),
              ),
            ],
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).torrentFileAssociation,
              description: LocaleScope.of(context).torrentFileAssociationDesc,
              child: ShadSwitch(
                value: settingsProvider.torrentAssociated,
                onChanged: (v) {
                  settingsProvider.setFileAssociation(v);
                  // 用户手动操作过就标记为已提示
                  settingsProvider.markTorrentAssocPrompted();
                },
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).notifyOnComplete,
              description: LocaleScope.of(context).notifyOnCompleteDesc,
              child: ShadSwitch(
                value: settingsProvider.notifyOnComplete,
                onChanged: (v) => settingsProvider.setNotifyOnComplete(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).keepAwakeWhileDownloading,
              description: LocaleScope.of(
                context,
              ).keepAwakeWhileDownloadingDesc,
              child: ShadSwitch(
                value: settingsProvider.keepAwakeWhileDownloading,
                onChanged: (v) =>
                    settingsProvider.setKeepAwakeWhileDownloading(v),
              ),
            ),
            const SizedBox(height: 20),
            // 侧边栏显示设置 — 小标题（支持搜索定位高亮）
            _HighlightRegion(
              label: LocaleScope.of(context).sidebarVisibility,
              description: LocaleScope.of(context).sidebarVisibilityDesc,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Padding(
                    padding: const EdgeInsets.only(bottom: 8),
                    child: Text(
                      LocaleScope.of(context).sidebarVisibility,
                      style: TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w600,
                        color: AppColors.of(context).textPrimary,
                      ),
                    ),
                  ),
                  Text(
                    LocaleScope.of(context).sidebarVisibilityDesc,
                    style: TextStyle(
                      fontSize: 11.5,
                      color: AppColors.of(context).textMuted,
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).showSidebarStatus,
              description: LocaleScope.of(context).showSidebarStatusDesc,
              child: ShadSwitch(
                value: settingsProvider.showSidebarStatus,
                onChanged: (v) => settingsProvider.setShowSidebarStatus(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).showSidebarQueues,
              description: LocaleScope.of(context).showSidebarQueuesDesc,
              child: ShadSwitch(
                value: settingsProvider.showSidebarQueues,
                onChanged: (v) => settingsProvider.setShowSidebarQueues(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).showSidebarCategory,
              description: LocaleScope.of(context).showSidebarCategoryDesc,
              child: ShadSwitch(
                value: settingsProvider.showSidebarCategory,
                onChanged: (v) => settingsProvider.setShowSidebarCategory(v),
              ),
            ),
            const SizedBox(height: 20),
            // 标题栏按钮设置 — 小标题（支持搜索定位高亮）
            _HighlightRegion(
              label: LocaleScope.of(context).titlebarButtons,
              description: LocaleScope.of(context).titlebarButtonsDesc,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Padding(
                    padding: const EdgeInsets.only(bottom: 8),
                    child: Text(
                      LocaleScope.of(context).titlebarButtons,
                      style: TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w600,
                        color: AppColors.of(context).textPrimary,
                      ),
                    ),
                  ),
                  Text(
                    LocaleScope.of(context).titlebarButtonsDesc,
                    style: TextStyle(
                      fontSize: 11.5,
                      color: AppColors.of(context).textMuted,
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).showTitlebarPauseAll,
              description: LocaleScope.of(context).showTitlebarPauseAllDesc,
              child: ShadSwitch(
                value: settingsProvider.showTitlebarPauseAll,
                onChanged: (v) => settingsProvider.setShowTitlebarPauseAll(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).showTitlebarResumeAll,
              description: LocaleScope.of(context).showTitlebarResumeAllDesc,
              child: ShadSwitch(
                value: settingsProvider.showTitlebarResumeAll,
                onChanged: (v) => settingsProvider.setShowTitlebarResumeAll(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).showTitlebarSettings,
              description: LocaleScope.of(context).showTitlebarSettingsDesc,
              child: ShadSwitch(
                value: settingsProvider.showTitlebarSettings,
                onChanged: (v) => settingsProvider.setShowTitlebarSettings(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).showTitlebarTheme,
              description: LocaleScope.of(context).showTitlebarThemeDesc,
              child: ShadSwitch(
                value: settingsProvider.showTitlebarTheme,
                onChanged: (v) => settingsProvider.setShowTitlebarTheme(v),
              ),
            ),
            const SizedBox(height: 20),
            // 自定义分类管理（支持搜索定位高亮）
            _HighlightRegion(
              label: LocaleScope.of(context).customCategories,
              description: LocaleScope.of(context).customCategoriesDesc,
              child: _CustomCategoryManager(settingsProvider: settingsProvider),
            ),
          ],
        );
      },
    );
  }
}

// ─────────────────────────────────────────────
// 分类管理（内置 + 自定义统一列表）
// ─────────────────────────────────────────────

class _CustomCategoryManager extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _CustomCategoryManager({required this.settingsProvider});

  /// 内置分类的 i18n 名称
  static String _builtinLabel(S s, String? builtinType) =>
      switch (builtinType) {
        'all' => s.categoryAll,
        'video' => s.categoryVideo,
        'audio' => s.categoryAudio,
        'document' => s.categoryDocument,
        'image' => s.categoryImage,
        'archive' => s.categoryArchive,
        'other' => s.categoryOther,
        _ => '',
      };

  /// 获取分类显示名称（内置用 i18n，自定义用用户设置的名称）
  static String displayName(S s, CustomCategory cat) {
    if (cat.isBuiltin) return _builtinLabel(s, cat.builtinType);
    return cat.name;
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final categories = settingsProvider.customCategories;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // 标题行
        Row(
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    s.customCategories,
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                      color: c.textPrimary,
                    ),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    s.categoryPriorityNote,
                    style: TextStyle(fontSize: 11.5, color: c.textMuted),
                  ),
                ],
              ),
            ),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                ShadButton.outline(
                  size: ShadButtonSize.sm,
                  onPressed: () => _confirmResetAll(context, s, c),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(LucideIcons.rotateCcw, size: 13, color: c.textMuted),
                      const SizedBox(width: 4),
                      Text(s.resetBuiltinCategories),
                    ],
                  ),
                ),
                const SizedBox(width: 8),
                ShadButton.outline(
                  size: ShadButtonSize.sm,
                  onPressed: () => _showCategoryDialog(context, s, c),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(LucideIcons.plus, size: 13, color: c.textSecondary),
                      const SizedBox(width: 4),
                      Text(s.addCategory),
                    ],
                  ),
                ),
              ],
            ),
          ],
        ),
        const SizedBox(height: 10),
        // 分类列表（Column 替代 ReorderableListView，避免 MaterialLocalizations 依赖）
        for (int i = 0; i < categories.length; i++)
          _CategoryTile(
            category: categories[i],
            index: i,
            total: categories.length,
            c: c,
            s: s,
            onEdit: () =>
                _showCategoryDialog(context, s, c, existing: categories[i]),
            onDelete: categories[i].builtinType == 'all'
                ? null
                : () => _confirmDelete(context, s, c, categories[i]),
            onReset:
                (categories[i].isBuiltin && categories[i].builtinType != 'all')
                ? () => settingsProvider.resetBuiltinCategory(
                    categories[i].builtinType!,
                  )
                : null,
            onToggleVisible: () => settingsProvider.updateCustomCategory(
              categories[i].copyWith(visible: !categories[i].visible),
            ),
            onMoveUp: i > 0
                ? () => settingsProvider.reorderCustomCategories(i, i - 1)
                : null,
            onMoveDown: i < categories.length - 1
                ? () => settingsProvider.reorderCustomCategories(i, i + 2)
                : null,
          ),
      ],
    );
  }

  void _showCategoryDialog(
    BuildContext context,
    S s,
    AppColors c, {
    CustomCategory? existing,
  }) {
    showCategoryEditDialog(
      context,
      existing: existing,
      onSave: (category) {
        if (existing != null) {
          settingsProvider.updateCustomCategory(category);
        } else {
          settingsProvider.addCustomCategory(category);
        }
      },
      onDelete: (existing != null && existing.builtinType != 'all')
          ? () => settingsProvider.removeCustomCategory(existing.id)
          : null,
    );
  }

  void _confirmDelete(
    BuildContext context,
    S s,
    AppColors c,
    CustomCategory cat,
  ) {
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.deleteCategory),
        description: Text(s.deleteCategoryConfirm),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
          ShadButton.destructive(
            onPressed: () {
              Navigator.of(ctx).pop();
              settingsProvider.removeCustomCategory(cat.id);
            },
            child: Text(s.deleteCategory),
          ),
        ],
      ),
    );
  }

  void _confirmResetAll(BuildContext context, S s, AppColors c) {
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.resetBuiltinCategories),
        description: Text(s.resetAllCategoriesConfirm),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
          ShadButton.destructive(
            onPressed: () {
              Navigator.of(ctx).pop();
              settingsProvider.resetAllCategories();
            },
            child: Text(s.confirm),
          ),
        ],
      ),
    );
  }
}

// 单个分类条目（内置 + 自定义通用）
class _CategoryTile extends StatefulWidget {
  final CustomCategory category;
  final int index;
  final int total;
  final AppColors c;
  final S s;
  final VoidCallback onEdit;
  final VoidCallback? onDelete;
  final VoidCallback? onReset;
  final VoidCallback onToggleVisible;
  final VoidCallback? onMoveUp;
  final VoidCallback? onMoveDown;

  const _CategoryTile({
    required this.category,
    required this.index,
    required this.total,
    required this.c,
    required this.s,
    required this.onEdit,
    this.onDelete,
    this.onReset,
    required this.onToggleVisible,
    this.onMoveUp,
    this.onMoveDown,
  });

  @override
  State<_CategoryTile> createState() => _CategoryTileState();
}

class _CategoryTileState extends State<_CategoryTile> {
  bool _isHovered = false;

  /// 描述文本：内置特殊分类显示"内置"，其余显示扩展名或正则
  String _subtitle(CustomCategory cat, S s) {
    // "全部文件" 和 "其他" 不显示扩展名
    if (cat.builtinType == 'all' || cat.builtinType == 'other') {
      return s.builtinCategory;
    }
    if (cat.matchMode == MatchMode.extension && cat.extensions.isNotEmpty) {
      return cat.extensions.map((e) => '.$e').join(', ');
    }
    if (cat.matchMode == MatchMode.regex && cat.regexPattern.isNotEmpty) {
      return cat.regexPattern;
    }
    return '';
  }

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final cat = widget.category;
    final c = widget.c;
    final s = widget.s;
    final label = _CustomCategoryManager.displayName(s, cat);

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: Container(
        margin: const EdgeInsets.only(bottom: 4),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        decoration: BoxDecoration(
          color: _isHovered ? c.hoverBg : c.surface1,
          borderRadius: m.brCard,
          border: Border.all(color: c.border),
        ),
        child: Row(
          children: [
            // 上下移动按钮
            Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                _TileAction(
                  icon: LucideIcons.chevronUp,
                  color: widget.onMoveUp != null ? c.textMuted : c.border,
                  onTap: widget.onMoveUp ?? () {},
                ),
                _TileAction(
                  icon: LucideIcons.chevronDown,
                  color: widget.onMoveDown != null ? c.textMuted : c.border,
                  onTap: widget.onMoveDown ?? () {},
                ),
              ],
            ),
            const SizedBox(width: 6),
            // 图标
            Icon(categoryIconData(cat.icon), size: 16, color: c.accent),
            const SizedBox(width: 8),
            // 名称 + 匹配规则 + 标签
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Flexible(
                        child: Text(
                          label,
                          style: TextStyle(
                            fontSize: 12.5,
                            fontWeight: FontWeight.w500,
                            color: cat.visible ? c.textPrimary : c.textMuted,
                          ),
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      if (cat.isBuiltin) ...[
                        const SizedBox(width: 6),
                        Container(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 5,
                            vertical: 1,
                          ),
                          decoration: BoxDecoration(
                            color: m.soft(c.accent),
                            borderRadius: m.brSm,
                          ),
                          child: Text(
                            s.builtinCategory,
                            style: TextStyle(
                              fontSize: 9,
                              color: c.accent,
                              fontWeight: FontWeight.w500,
                            ),
                          ),
                        ),
                      ],
                      if (!cat.visible) ...[
                        const SizedBox(width: 6),
                        Icon(LucideIcons.eyeOff, size: 11, color: c.textMuted),
                      ],
                    ],
                  ),
                  if (_subtitle(cat, s).isNotEmpty) ...[
                    const SizedBox(height: 2),
                    Text(
                      _subtitle(cat, s),
                      style: TextStyle(
                        fontSize: 10.5,
                        color: c.textMuted,
                        fontFamily: cat.matchMode == MatchMode.regex
                            ? 'monospace'
                            : null,
                      ),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ],
                ],
              ),
            ),
            // 操作按钮
            if (_isHovered) ...[
              _TileAction(
                icon: cat.visible ? LucideIcons.eye : LucideIcons.eyeOff,
                color: c.textMuted,
                onTap: widget.onToggleVisible,
              ),
              const SizedBox(width: 2),
              // 内置分类: 非 all 可编辑（含"其他"）; 自定义分类: 总是可编辑
              if (!cat.isBuiltin || cat.builtinType != 'all') ...[
                _TileAction(
                  icon: LucideIcons.pencil,
                  color: c.textSecondary,
                  onTap: widget.onEdit,
                ),
                const SizedBox(width: 2),
              ],
              // 内置: 重置按钮（"全部文件"除外）; 自定义: 删除按钮
              if (widget.onReset != null && cat.builtinType != 'all')
                _TileAction(
                  icon: LucideIcons.rotateCcw,
                  color: c.textMuted,
                  onTap: widget.onReset!,
                ),
              if (widget.onDelete != null)
                _TileAction(
                  icon: LucideIcons.trash2,
                  color: AppColors.red,
                  onTap: widget.onDelete!,
                ),
            ],
          ],
        ),
      ),
    );
  }
}

class _TileAction extends StatefulWidget {
  final IconData icon;
  final Color color;
  final VoidCallback onTap;

  const _TileAction({
    required this.icon,
    required this.color,
    required this.onTap,
  });

  @override
  State<_TileAction> createState() => _TileActionState();
}

class _TileActionState extends State<_TileAction> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          width: 22,
          height: 22,
          decoration: BoxDecoration(
            color: _hover
                ? m.soft(widget.color)
                : Colors.transparent,
            borderRadius: m.brSm,
          ),
          child: Icon(widget.icon, size: 12, color: widget.color),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 外观设置
// ─────────────────────────────────────────────

class _AppearanceContent extends StatelessWidget {
  const _AppearanceContent({super.key});

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _SettingCard(
          label: s.language,
          description: s.languageDesc,
          vertical: true,
          child: const _LanguageSelector(),
        ),
        const SizedBox(height: 10),
        _SettingCard(
          label: s.themeMode,
          description: s.themeModeDesc,
          vertical: true,
          child: const _ThemeModeSelector(),
        ),
        const SizedBox(height: 10),
        _SettingCard(
          label: s.themeSelection,
          description: s.themeSelectionDesc,
          vertical: true,
          child: const _ThemeSelector(),
        ),
        const SizedBox(height: 10),
        _SettingCard(
          label: s.themeColor,
          description: s.themeColorDesc,
          vertical: true,
          child: const _ColorSchemeSelector(),
        ),
        const SizedBox(height: 10),
        _SettingCard(
          label: s.uiScale,
          description: s.uiScaleDesc,
          vertical: true,
          child: const _UiScaleSelector(),
        ),
        if (Platform.isWindows) ...[
          const SizedBox(height: 10),
          _SettingCard(
            label: s.appIcon,
            description: s.appIconDesc,
            vertical: true,
            child: const _AppIconSelector(),
          ),
        ],
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 界面缩放选择器
// ─────────────────────────────────────────────

class _UiScaleSelector extends StatelessWidget {
  const _UiScaleSelector();

  static const _options = [0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.5];

  static String _label(double v) => '${(v * 100).round()}%';

  @override
  Widget build(BuildContext context) {
    final tp = FluxDownApp.of(context);
    final c = AppColors.of(context);
    final current = tp.uiScale;
    return Row(
      children: _options.map((v) {
        final selected = (v - current).abs() < 0.01;
        return Padding(
          padding: const EdgeInsets.only(right: 6),
          child: _UiScaleChip(
            label: _label(v),
            selected: selected,
            isDefault: v == 1.0,
            colors: c,
            onTap: () => tp.setUiScale(v),
          ),
        );
      }).toList(),
    );
  }
}

class _UiScaleChip extends StatefulWidget {
  final String label;
  final bool selected;
  final bool isDefault;
  final AppColors colors;
  final VoidCallback onTap;

  const _UiScaleChip({
    required this.label,
    required this.selected,
    required this.isDefault,
    required this.colors,
    required this.onTap,
  });

  @override
  State<_UiScaleChip> createState() => _UiScaleChipState();
}

class _UiScaleChipState extends State<_UiScaleChip> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.colors;
    final m = AppMetrics.of(context);
    final bg = widget.selected
        ? m.active(c.accent)
        : _isHovered
        ? c.surface2
        : c.surface1;
    final border = widget.selected
        ? m.borderFade(c.accent)
        : m.borderFade(c.border);
    final textColor = widget.selected ? c.accent : c.textPrimary;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 150),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
          decoration: BoxDecoration(
            color: bg,
            borderRadius: m.brMd,
            border: Border.all(color: border, width: 1),
          ),
          child: Text(
            widget.label,
            style: TextStyle(
              fontSize: 12,
              fontWeight: widget.selected ? FontWeight.w600 : FontWeight.w400,
              color: textColor,
            ),
          ),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 应用图标选择器（仅 Windows）
// ─────────────────────────────────────────────

class _AppIconSelector extends StatefulWidget {
  const _AppIconSelector();

  @override
  State<_AppIconSelector> createState() => _AppIconSelectorState();
}

class _AppIconSelectorState extends State<_AppIconSelector> {
  bool _busy = false;

  /// 「自定义」点击 → 一律打开文件选择器（取消则保持当前图标不变）。
  /// 曾导入过也重新选图，避免「点了没反应」的切回旧图标歧义。
  Future<void> _pickImage() async {
    final s = LocaleScope.of(context);
    setState(() => _busy = true);
    try {
      final files = await FilePickerService.pickFiles(
        dialogTitle: s.appIconChooseImage,
        allowedExtensions: const ['png', 'jpg', 'jpeg', 'webp', 'bmp', 'ico'],
      );
      if (files == null || files.isEmpty) return;
      await AppIconService.instance.importAndApply(files.first.path);
    } catch (e, stack) {
      logError('AppIconSelector', 'failed to apply custom icon', e, stack);
      if (mounted) {
        ShadSonner.of(context).show(
          ShadToast.destructive(
            title: Text(LocaleScope.of(context).appIconApplyFailed),
            description: Text(e.toString()),
            duration: const Duration(seconds: 3),
          ),
        );
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 弹窗放大查看图标（自定义预览源为 256px PNG；内置闪电为打包资源）
  void _showPreviewDialog(ImageProvider image, int revision) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.appIcon),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.close),
          ),
        ],
        child: Padding(
          padding: const EdgeInsets.only(top: 16),
          child: _IconZoomPreview(image: image, revision: revision),
        ),
      ),
    );
  }

  /// 可点击放大的小尺寸图标缩略图。
  Widget _iconThumb(AppColors c, ImageProvider image, int revision) {
    final m = AppMetrics.of(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () => _showPreviewDialog(image, revision),
        child: Container(
          padding: const EdgeInsets.all(3),
          decoration: BoxDecoration(
            borderRadius: m.brCard,
            border: Border.all(color: m.borderFade(c.border)),
            color: c.surface1,
          ),
          child: ClipRRect(
            borderRadius: m.brMd,
            child: Image(
              key: ValueKey(revision),
              image: image,
              width: 34,
              height: 34,
              filterQuality: FilterQuality.medium,
              gaplessPlayback: true,
            ),
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    return ListenableBuilder(
      listenable: AppIconService.instance,
      builder: (context, _) {
        final svc = AppIconService.instance;
        final previewPath = svc.previewPngPath;
        return Row(
          children: [
            _UiScaleChip(
              label: s.appIconDefault,
              selected: svc.choice == AppIconChoice.defaultIcon,
              isDefault: true,
              colors: c,
              onTap: () {
                if (!_busy) svc.useDefault();
              },
            ),
            const SizedBox(width: 6),
            _UiScaleChip(
              label: s.appIconBolt,
              selected: svc.isBolt,
              isDefault: false,
              colors: c,
              onTap: () {
                if (!_busy) svc.useBolt();
              },
            ),
            const SizedBox(width: 6),
            _UiScaleChip(
              label: s.appIconCustom,
              selected: svc.isCustom,
              isDefault: false,
              colors: c,
              onTap: () {
                if (!_busy) _pickImage();
              },
            ),
            if (svc.isBolt) ...[
              const SizedBox(width: 12),
              _iconThumb(
                c,
                const AssetImage(AppIconService.builtinBoltAsset),
                0,
              ),
            ] else if (svc.isCustom && previewPath != null) ...[
              const SizedBox(width: 12),
              _iconThumb(c, FileImage(File(previewPath)), svc.previewRevision),
            ],
          ],
        );
      },
    );
  }
}

/// 支持滚轮缩放的图标预览视口。
///
/// 悬停在视口上滚动滚轮即可放大/缩小，图标按当前尺寸自适应渲染；
/// 大倍率下切换为最近邻采样，保留像素边缘便于检查图标细节。
class _IconZoomPreview extends StatefulWidget {
  final ImageProvider image;
  final int revision;

  const _IconZoomPreview({required this.image, required this.revision});

  @override
  State<_IconZoomPreview> createState() => _IconZoomPreviewState();
}

class _IconZoomPreviewState extends State<_IconZoomPreview> {
  static const _baseSide = 160.0;
  static const _minScale = 0.25;
  static const _maxScale = 6.0;
  static const _step = 1.15;

  double _scale = 1.0;

  void _onPointerSignal(PointerSignalEvent event) {
    if (event is! PointerScrollEvent) return;
    final factor = event.scrollDelta.dy < 0 ? _step : 1 / _step;
    setState(() {
      _scale = (_scale * factor).clamp(_minScale, _maxScale);
    });
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final side = _baseSide * _scale;
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Listener(
          onPointerSignal: _onPointerSignal,
          child: Container(
            width: double.infinity,
            height: 320,
            decoration: BoxDecoration(
              color: c.surface1,
              borderRadius: m.brDialog,
              border: Border.all(color: m.borderFaint(c.border)),
            ),
            child: ClipRRect(
              borderRadius: m.brDialog,
              child: Center(
                child: Image(
                  key: ValueKey(widget.revision),
                  image: widget.image,
                  width: side,
                  height: side,
                  fit: BoxFit.contain,
                  filterQuality: _scale > 2
                      ? FilterQuality.none
                      : FilterQuality.high,
                  gaplessPlayback: true,
                ),
              ),
            ),
          ),
        ),
        const SizedBox(height: 8),
        Text(
          '${side.round()} px · ${s.appIconZoomHint}',
          style: TextStyle(fontSize: 11, color: c.textMuted),
        ),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 下载设置
// ─────────────────────────────────────────────

class _DownloadContent extends StatelessWidget {
  final SettingsProvider settingsProvider;
  final DownloadController? downloadController;

  const _DownloadContent({
    super.key,
    required this.settingsProvider,
    this.downloadController,
  });

  @override
  Widget build(BuildContext context) {
    final listenable = downloadController != null
        ? Listenable.merge([settingsProvider, downloadController!])
        : settingsProvider;
    return ListenableBuilder(
      listenable: listenable,
      builder: (context, _) {
        final s = LocaleScope.of(context);
        final queues = downloadController?.queues ?? [];
        return Column(
          children: [
            _SettingCard(
              label: s.defaultSaveDir,
              description: s.defaultSaveDirDesc,
              vertical: true,
              child: _SaveDirPicker(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.rememberLastSaveDir,
              description: s.rememberLastSaveDirDesc,
              child: ShadSwitch(
                value: settingsProvider.rememberLastSaveDir,
                onChanged: (v) => settingsProvider.setRememberLastSaveDir(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.silentDownload,
              description: s.silentDownloadDesc,
              child: ShadSwitch(
                value: settingsProvider.silentDownloadEnabled,
                onChanged: (v) => settingsProvider.setSilentDownloadEnabled(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.defaultThreads,
              description: s.defaultThreadsDesc,
              child: _SegmentSelector(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.maxConcurrent,
              description: s.maxConcurrentDesc,
              child: _ConcurrentSelector(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.speedLimit,
              description: s.speedLimitDesc,
              vertical: true,
              child: _SpeedLimitInput(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.autoRetryCount,
              description: s.autoRetryCountDesc,
              child: _AutoRetryCountSelector(
                settingsProvider: settingsProvider,
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.autoRetryDelay,
              description: s.autoRetryDelayDesc,
              vertical: true,
              child: _AutoRetryDelayInput(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.userAgent,
              description: s.userAgentDesc,
              vertical: true,
              child: _UserAgentEditor(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: s.revealFileCmdLabel,
              description: s.revealFileCmdDesc,
              vertical: true,
              child: _FileManagerCmdInput(
                settingsProvider: settingsProvider,
              ),
            ),
            if (queues.isNotEmpty) ...[
              const SizedBox(height: 10),
              _SettingCard(
                label: s.defaultQueueSetting,
                description: s.defaultQueueSettingDesc,
                child: _DefaultQueueSelector(
                  settingsProvider: settingsProvider,
                  queues: queues,
                ),
              ),
            ],
          ],
        );
      },
    );
  }
}

// ─────────────────────────────────────────────
// 默认队列选择器（下载设置内）
// ─────────────────────────────────────────────

class _DefaultQueueSelector extends StatelessWidget {
  final SettingsProvider settingsProvider;
  final List<DownloadQueue> queues;

  const _DefaultQueueSelector({
    required this.settingsProvider,
    required this.queues,
  });

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final allOptions = <DownloadQueue>[
      const DownloadQueue(
        queueId: '',
        name: '',
        speedLimitKbps: 0,
        maxConcurrent: 0,
        defaultSaveDir: '',
        position: -1,
      ),
      ...queues,
    ];
    return ShadSelect<String>(
      initialValue: settingsProvider.defaultQueueId,
      options: allOptions.map((q) {
        final label = q.queueId.isEmpty ? s.defaultQueue : q.name;
        return ShadOption(value: q.queueId, child: Text(label));
      }).toList(),
      selectedOptionBuilder: (context, value) {
        if (value.isEmpty) return Text(s.defaultQueue);
        final q = queues.where((q) => q.queueId == value).firstOrNull;
        return Text(
          q?.name ?? s.defaultQueue,
          overflow: TextOverflow.ellipsis,
          maxLines: 1,
        );
      },
      onChanged: (v) {
        if (v != null) settingsProvider.setDefaultQueueId(v);
      },
    );
  }
}

// ─────────────────────────────────────────────
// BT 设置
// ─────────────────────────────────────────────

class _BtContent extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _BtContent({super.key, required this.settingsProvider});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: settingsProvider,
      builder: (context, _) {
        return Column(
          children: [
            _SettingCard(
              label: LocaleScope.of(context).btListenPort,
              description: LocaleScope.of(context).btListenPortDesc,
              vertical: true,
              child: _BtPortRangeEditor(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).btTrackerList,
              description: LocaleScope.of(context).btTrackerListDesc,
              vertical: true,
              child: _BtTrackerEditor(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).btTrackerSub,
              description: LocaleScope.of(context).btTrackerSubDesc,
              vertical: true,
              child: _BtTrackerSubEditor(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 6),
            // 重启提示
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 4),
              child: Row(
                children: [
                  Icon(
                    LucideIcons.info,
                    size: 12,
                    color: AppColors.of(context).textMuted,
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      LocaleScope.of(context).btSettingsRestartHint,
                      style: TextStyle(
                        fontSize: 11,
                        color: AppColors.of(context).textMuted,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ],
        );
      },
    );
  }
}

// ─────────────────────────────────────────────
// ED2K 设置
// ─────────────────────────────────────────────

class _Ed2kContent extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _Ed2kContent({super.key, required this.settingsProvider});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: settingsProvider,
      builder: (context, _) {
        return Column(
          children: [
            _SettingCard(
              label: LocaleScope.of(context).ed2kServerList,
              description: LocaleScope.of(context).ed2kServerListDesc,
              vertical: true,
              child: _Ed2kServerEditor(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).ed2kServerSub,
              description: LocaleScope.of(context).ed2kServerSubDesc,
              vertical: true,
              child: _Ed2kServerSubEditor(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).ed2kEnableKad,
              description: LocaleScope.of(context).ed2kEnableKadDesc,
              child: ShadSwitch(
                value: settingsProvider.ed2kEnableKad,
                onChanged: (v) => settingsProvider.setEd2kEnableKad(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).ed2kEnableUpnp,
              description: LocaleScope.of(context).ed2kEnableUpnpDesc,
              child: ShadSwitch(
                value: settingsProvider.ed2kEnableUpnp,
                onChanged: (v) => settingsProvider.setEd2kEnableUpnp(v),
              ),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).ed2kListenPort,
              description: LocaleScope.of(context).ed2kListenPortDesc,
              child: _Ed2kListenPortEditor(settingsProvider: settingsProvider),
            ),
          ],
        );
      },
    );
  }
}

// ─────────────────────────────────────────────
// 代理设置
// ─────────────────────────────────────────────

class _ProxyContent extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _ProxyContent({super.key, required this.settingsProvider});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: settingsProvider,
      builder: (context, _) {
        return Column(
          children: [_ProxySettingsCard(settingsProvider: settingsProvider)],
        );
      },
    );
  }
}

// ─────────────────────────────────────────────
// 下载设置子组件
// ─────────────────────────────────────────────

class _SaveDirPicker extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _SaveDirPicker({required this.settingsProvider});

  @override
  State<_SaveDirPicker> createState() => _SaveDirPickerState();
}

class _SaveDirPickerState extends State<_SaveDirPicker> {
  bool _isPicking = false;

  Future<void> _pickDir() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await FilePickerService.pickDirectory(
        dialogTitle: currentS.selectDefaultSaveDir,
        initialDirectory: widget.settingsProvider.defaultSaveDir.isNotEmpty
            ? widget.settingsProvider.defaultSaveDir
            : null,
      );
      if (result != null && mounted) {
        widget.settingsProvider.setDefaultSaveDir(result);
      }
    } on FilePickerException catch (e) {
      if (mounted) _showPickerError(e);
    } finally {
      if (mounted) setState(() => _isPicking = false);
    }
  }

  void _showPickerError(FilePickerException e) {
    final s = currentS;
    final message = switch (e.reason) {
      FilePickerFailReason.timeout => s.filePickerErrorTimeout,
      FilePickerFailReason.noDialogTool => s.filePickerErrorNoTool,
      FilePickerFailReason.comInitFailed => s.filePickerErrorNative,
      FilePickerFailReason.nativeDialogFailed => s.filePickerErrorNative,
      FilePickerFailReason.unknown => s.filePickerErrorGeneric,
    };
    ShadSonner.of(context).show(ShadToast.destructive(title: Text(message)));
  }

  @override
  Widget build(BuildContext context) {
    return DirPickerField(
      path: widget.settingsProvider.defaultSaveDir,
      placeholder: currentS.selectDefaultSaveDir,
      enabled: !_isPicking,
      onTap: _pickDir,
    );
  }
}

class _SegmentSelector extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _SegmentSelector({required this.settingsProvider});

  @override
  Widget build(BuildContext context) {
    final current = settingsProvider.defaultSegments;
    // SettingsProvider: 0 = 自动; ThreadSelector: null = 自动
    final value = current > 0 ? current.toString() : null;

    return ThreadSelector(
      value: value,
      onChanged: (v) {
        final n = int.tryParse(v ?? '') ?? 0;
        settingsProvider.setDefaultSegments(n.clamp(0, 256));
      },
    );
  }
}

class _ConcurrentSelector extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _ConcurrentSelector({required this.settingsProvider});

  static const _options = [1, 2, 3, 5, 8, 10];

  @override
  Widget build(BuildContext context) {
    final current = settingsProvider.maxConcurrentTasks;
    return ShadSelect<int>(
      placeholder: Text('$current'),
      initialValue: current,
      options: _options
          .map((n) => ShadOption(value: n, child: Text('$n')))
          .toList(),
      selectedOptionBuilder: (context, value) => Text(currentS.nTasks(value)),
      onChanged: (v) {
        if (v != null) settingsProvider.setMaxConcurrentTasks(v);
      },
    );
  }
}

class _SpeedLimitInput extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _SpeedLimitInput({required this.settingsProvider});

  @override
  State<_SpeedLimitInput> createState() => _SpeedLimitInputState();
}

class _SpeedLimitInputState extends State<_SpeedLimitInput> {
  late final TextEditingController _controller;

  @override
  void initState() {
    super.initState();
    final kbps = widget.settingsProvider.speedLimitBytes ~/ 1024;
    _controller = TextEditingController(text: kbps == 0 ? '0' : '$kbps');
  }

  @override
  void didUpdateWidget(_SpeedLimitInput oldWidget) {
    super.didUpdateWidget(oldWidget);
    final kbps = widget.settingsProvider.speedLimitBytes ~/ 1024;
    final current = int.tryParse(_controller.text) ?? 0;
    if (kbps != current) {
      _controller.text = kbps == 0 ? '0' : '$kbps';
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _onSubmit(String value) {
    final kbps = int.tryParse(value) ?? 0;
    widget.settingsProvider.setSpeedLimitBytes(kbps * 1024);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Row(
      children: [
        SizedBox(
          width: 120,
          child: ShadInput(
            controller: _controller,
            placeholder: const Text('0'),
            onSubmitted: _onSubmit,
            onChanged: _onSubmit,
          ),
        ),
        const SizedBox(width: 8),
        Text(
          currentS.speedLimitUnit,
          style: TextStyle(fontSize: 12, color: c.textMuted),
        ),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 失败自动重试
// ─────────────────────────────────────────────

/// 重试次数下拉：关闭(0) / 1 / 2 / 3 / 5 / 10 / 无限(-1)。
class _AutoRetryCountSelector extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _AutoRetryCountSelector({required this.settingsProvider});

  static const _options = [0, 1, 2, 3, 5, 10, -1];

  String _label(BuildContext context, int v) {
    final s = LocaleScope.of(context);
    if (v == 0) return s.autoRetryOff;
    if (v == -1) return s.autoRetryUnlimited;
    return s.nRetries(v);
  }

  @override
  Widget build(BuildContext context) {
    final current = settingsProvider.maxAutoRetries;
    return ShadSelect<int>(
      placeholder: Text(_label(context, current)),
      initialValue: current,
      options: _options
          .map((v) => ShadOption(value: v, child: Text(_label(context, v))))
          .toList(),
      selectedOptionBuilder: (context, value) => Text(_label(context, value)),
      onChanged: (v) {
        if (v != null) settingsProvider.setMaxAutoRetries(v);
      },
    );
  }
}

/// 重试间隔数字输入（秒，0 = 立即重试）。
class _AutoRetryDelayInput extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _AutoRetryDelayInput({required this.settingsProvider});

  @override
  State<_AutoRetryDelayInput> createState() => _AutoRetryDelayInputState();
}

class _AutoRetryDelayInputState extends State<_AutoRetryDelayInput> {
  late final TextEditingController _controller;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController(
      text: '${widget.settingsProvider.autoRetryDelaySecs}',
    );
  }

  @override
  void didUpdateWidget(_AutoRetryDelayInput oldWidget) {
    super.didUpdateWidget(oldWidget);
    final secs = widget.settingsProvider.autoRetryDelaySecs;
    final current = int.tryParse(_controller.text) ?? 0;
    if (secs != current) {
      _controller.text = '$secs';
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _onSubmit(String value) {
    final secs = (int.tryParse(value) ?? 0).clamp(0, 3600);
    widget.settingsProvider.setAutoRetryDelaySecs(secs);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return Row(
      children: [
        SizedBox(
          width: 120,
          child: ShadInput(
            controller: _controller,
            placeholder: const Text('0'),
            onSubmitted: _onSubmit,
            onChanged: _onSubmit,
          ),
        ),
        const SizedBox(width: 8),
        Text(
          s.autoRetryDelayUnit,
          style: TextStyle(fontSize: 12, color: c.textMuted),
        ),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// UA 编辑器
// ─────────────────────────────────────────────

/// 预设 UA 映射（key → UA 字符串，'custom' 留空让用户自行输入）
///
/// Chrome / Edge 遵循 UA Reduction 策略，次版本号固定为 0.0.0；
/// Edge 额外携带完整的小版本号（Edg/145.0.3800.70）以匹配官方实际发送的格式。
/// 版本基准：Chrome 145 / Edge 145 / Firefox 147 / Safari 18.3（2025-2026 主流版本）
const _kUaPresets = {
  // Chrome 145（UA Reduction：Win11 与 Win10 发送同一 UA，次版本号全为 0）
  'chrome':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
      '(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36',
  // Firefox 147（Gecko/20100101 为固定占位，仅主版本号暴露）
  'firefox':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) '
      'Gecko/20100101 Firefox/147.0',
  // Edge 145（基于 Chromium，追加 Edg/ 标记；注意是 Edg 而非 Edge）
  'edge':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
      '(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.3800.70',
  // Safari 18.3（macOS Sonoma；WebKit 版本号 605.1.15 长期固定）
  'safari':
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) '
      'AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15',
  // 百度网盘直链专用标识
  'netdisk': 'netdisk',
};

String _detectPreset(String ua) {
  if (ua.isEmpty) return 'chrome'; // 空 = 内置 Chrome UA
  for (final entry in _kUaPresets.entries) {
    if (entry.value == ua) return entry.key;
  }
  return 'custom';
}

class _UserAgentEditor extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _UserAgentEditor({required this.settingsProvider});

  @override
  State<_UserAgentEditor> createState() => _UserAgentEditorState();
}

class _UserAgentEditorState extends State<_UserAgentEditor> {
  late TextEditingController _controller;
  late String _selectedPreset;

  @override
  void initState() {
    super.initState();
    final ua = widget.settingsProvider.globalUserAgent;
    _controller = TextEditingController(text: ua);
    _selectedPreset = _detectPreset(ua);
  }

  @override
  void didUpdateWidget(_UserAgentEditor oldWidget) {
    super.didUpdateWidget(oldWidget);
    final ua = widget.settingsProvider.globalUserAgent;
    if (ua != _controller.text) {
      _controller.text = ua;
      _selectedPreset = _detectPreset(ua);
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _onPresetChanged(String? preset) {
    if (preset == null) return;
    setState(() => _selectedPreset = preset);
    if (preset != 'custom') {
      final ua = _kUaPresets[preset] ?? '';
      _controller.text = ua;
      // 空字符串 = 使用内置 Chrome UA，与 'chrome' 预设语义等价
      widget.settingsProvider.setGlobalUserAgent(ua);
    }
  }

  void _onTextChanged(String value) {
    // 手动编辑时切换到 custom
    final detected = _detectPreset(value);
    if (detected != _selectedPreset) {
      setState(() => _selectedPreset = detected);
    }
  }

  void _onSubmit(String value) {
    widget.settingsProvider.setGlobalUserAgent(value);
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    return Row(
      children: [
        SizedBox(
          width: 150,
          child: ShadSelect<String>(
            initialValue: _selectedPreset,
            options: [
              ShadOption(value: 'chrome', child: Text(s.userAgentPresetChrome)),
              ShadOption(
                value: 'firefox',
                child: Text(s.userAgentPresetFirefox),
              ),
              ShadOption(value: 'edge', child: Text(s.userAgentPresetEdge)),
              ShadOption(value: 'safari', child: Text(s.userAgentPresetSafari)),
              ShadOption(
                value: 'netdisk',
                child: Text(s.userAgentPresetNetdisk),
              ),
              ShadOption(value: 'custom', child: Text(s.userAgentPresetCustom)),
            ],
            selectedOptionBuilder: (context, value) {
              final label = switch (value) {
                'chrome' => 'Chrome',
                'firefox' => 'Firefox',
                'edge' => 'Edge',
                'safari' => 'Safari',
                'netdisk' => 'netdisk',
                _ => s.userAgentPresetCustom,
              };
              return Text(label, overflow: TextOverflow.ellipsis, maxLines: 1);
            },
            onChanged: _onPresetChanged,
          ),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: ShadInput(
            controller: _controller,
            placeholder: Text(s.userAgentPlaceholder),
            onChanged: _onTextChanged,
            onSubmitted: _onSubmit,
          ),
        ),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 文件管理器自定义命令输入
// ─────────────────────────────────────────────

/// 让用户填写第三方文件管理器的命令模板。
/// 提交时机：失焦或回车（与 UA 编辑器一致），避免每次按键写盘。
class _FileManagerCmdInput extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _FileManagerCmdInput({
    required this.settingsProvider,
  });

  @override
  State<_FileManagerCmdInput> createState() => _FileManagerCmdInputState();
}

class _FileManagerCmdInputState extends State<_FileManagerCmdInput> {
  late TextEditingController _controller;
  late FocusNode _focusNode;

  String get _currentValue => widget.settingsProvider.revealFileCmd;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController(text: _currentValue);
    _focusNode = FocusNode();
    _focusNode.addListener(() {
      // 失焦时持久化（与 UA 编辑器交互一致）
      if (!_focusNode.hasFocus) _commit();
    });
  }

  @override
  void didUpdateWidget(_FileManagerCmdInput oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 仅在未聚焦（用户未编辑）时才用外部值回填，避免用户清空/编辑过程中被
    // 外部 rebuild 回灌旧值，导致无法清空、无法重置为默认（留空=平台默认）。
    if (!_focusNode.hasFocus && _currentValue != _controller.text) {
      _controller.text = _currentValue;
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    _focusNode.dispose();
    super.dispose();
  }

  void _commit() {
    widget.settingsProvider.setRevealFileCmd(_controller.text);
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final placeholder = s.revealFileCmdPlaceholder;
    return ShadInput(
      controller: _controller,
      focusNode: _focusNode,
      placeholder: Text(placeholder),
      onSubmitted: (_) => _commit(),
    );
  }
}

// ─────────────────────────────────────────────
// 代理设置子组件
// ─────────────────────────────────────────────

/// 代理设置卡片（模式选择 + 手动配置表单）
class _ProxySettingsCard extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _ProxySettingsCard({required this.settingsProvider});

  @override
  State<_ProxySettingsCard> createState() => _ProxySettingsCardState();
}

class _ProxySettingsCardState extends State<_ProxySettingsCard> {
  late TextEditingController _hostController;
  late TextEditingController _portController;
  late TextEditingController _usernameController;
  late TextEditingController _passwordController;
  late TextEditingController _noListController;

  // 代理测试状态: null=未测试, true=成功, false=失败
  bool? _testResult;
  bool _isTesting = false;
  int _testLatencyMs = 0;
  String _testError = '';
  StreamSubscription<RustSignalPack<ProxyTestResult>>? _testSub;

  // 系统代理检测状态
  bool _sysProxyDetecting = false;
  bool _sysProxyDetected = false;
  String _sysProxyType = '';
  String _sysProxyHost = '';
  String _sysProxyPort = '';
  String _sysProxyNoList = '';
  StreamSubscription<RustSignalPack<SystemProxyInfo>>? _sysProxySub;

  @override
  void initState() {
    super.initState();
    final sp = widget.settingsProvider;
    _hostController = TextEditingController(text: sp.proxyHost);
    _portController = TextEditingController(text: sp.proxyPort);
    _usernameController = TextEditingController(text: sp.proxyUsername);
    _passwordController = TextEditingController(text: sp.proxyPassword);
    _noListController = TextEditingController(text: sp.proxyNoList);
    _testSub = ProxyTestResult.rustSignalStream.listen(_onTestResult);
    _sysProxySub = SystemProxyInfo.rustSignalStream.listen(_onSysProxyResult);
    // 如果当前就是系统代理模式，立即请求检测
    if (sp.proxyMode == 'system') {
      _requestDetectSystemProxy();
    }
  }

  @override
  void didUpdateWidget(_ProxySettingsCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    final sp = widget.settingsProvider;
    // 仅在外部值变化时同步（例如从 Rust 端加载初始值）
    if (sp.proxyHost != _hostController.text) {
      _hostController.text = sp.proxyHost;
    }
    if (sp.proxyPort != _portController.text) {
      _portController.text = sp.proxyPort;
    }
    if (sp.proxyUsername != _usernameController.text) {
      _usernameController.text = sp.proxyUsername;
    }
    if (sp.proxyPassword != _passwordController.text) {
      _passwordController.text = sp.proxyPassword;
    }
    if (sp.proxyNoList != _noListController.text) {
      _noListController.text = sp.proxyNoList;
    }
  }

  void _onTestResult(RustSignalPack<ProxyTestResult> pack) {
    if (!mounted) return;
    final msg = pack.message;
    setState(() {
      _isTesting = false;
      _testResult = msg.success;
      _testLatencyMs = msg.latencyMs.toInt();
      _testError = msg.errorMessage;
    });
  }

  void _testProxy() {
    final sp = widget.settingsProvider;
    if (sp.proxyHost.isEmpty || sp.proxyPort.isEmpty) return;
    setState(() {
      _isTesting = true;
      _testResult = null;
    });
    TestProxyConnection(
      proxyType: sp.proxyType,
      proxyHost: sp.proxyHost,
      proxyPort: sp.proxyPort,
      proxyUsername: sp.proxyUsername,
      proxyPassword: sp.proxyPassword,
    ).sendSignalToRust();
  }

  void _requestDetectSystemProxy() {
    setState(() {
      _sysProxyDetecting = true;
      _sysProxyDetected = false;
    });
    DetectSystemProxy().sendSignalToRust();
  }

  void _onSysProxyResult(RustSignalPack<SystemProxyInfo> pack) {
    if (!mounted) return;
    final msg = pack.message;
    setState(() {
      _sysProxyDetecting = false;
      _sysProxyDetected = msg.detected;
      _sysProxyType = msg.proxyType;
      _sysProxyHost = msg.host;
      _sysProxyPort = msg.port;
      _sysProxyNoList = msg.noProxyList;
    });
  }

  @override
  void dispose() {
    _testSub?.cancel();
    _sysProxySub?.cancel();
    _hostController.dispose();
    _portController.dispose();
    _usernameController.dispose();
    _passwordController.dispose();
    _noListController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final sp = widget.settingsProvider;
    final isManual = sp.proxyMode == 'manual';

    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brDialog,
        border: Border.all(color: m.borderMedium(c.border), width: 1),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 代理模式选择
          Text(
            s.proxySettings,
            style: TextStyle(
              fontSize: 13,
              fontWeight: FontWeight.w500,
              color: c.textPrimary,
            ),
          ),
          const SizedBox(height: 2),
          Text(
            s.proxyBtNote,
            style: TextStyle(fontSize: 11.5, color: c.textMuted),
          ),
          const SizedBox(height: 12),
          Row(
            spacing: 8,
            children: [
              Expanded(
                child: _ProxyModeOption(
                  icon: LucideIcons.unplug,
                  label: s.proxyModeNone,
                  selected: sp.proxyMode == 'none',
                  colors: c,
                  onTap: () => sp.setProxyMode('none'),
                ),
              ),
              Expanded(
                child: _ProxyModeOption(
                  icon: LucideIcons.monitor,
                  label: s.proxyModeSystem,
                  selected: sp.proxyMode == 'system',
                  colors: c,
                  onTap: () {
                    sp.setProxyMode('system');
                    _requestDetectSystemProxy();
                  },
                ),
              ),
              Expanded(
                child: _ProxyModeOption(
                  icon: LucideIcons.settings2,
                  label: s.proxyModeManual,
                  selected: sp.proxyMode == 'manual',
                  colors: c,
                  onTap: () => sp.setProxyMode('manual'),
                ),
              ),
            ],
          ),
          // 系统代理只读展示
          if (sp.proxyMode == 'system') ...[
            const SizedBox(height: 16),
            Divider(height: 1, color: m.borderFaint(c.border)),
            const SizedBox(height: 14),
            if (_sysProxyDetecting)
              Row(
                children: [
                  SizedBox(
                    width: 14,
                    height: 14,
                    child: CircularProgressIndicator(
                      strokeWidth: 1.5,
                      color: c.textMuted,
                    ),
                  ),
                  const SizedBox(width: 8),
                  Text(
                    s.proxySystemDetecting,
                    style: TextStyle(fontSize: 12, color: c.textMuted),
                  ),
                ],
              )
            else if (!_sysProxyDetected)
              Row(
                children: [
                  Icon(LucideIcons.info, size: 14, color: c.textMuted),
                  const SizedBox(width: 8),
                  Text(
                    s.proxySystemNotConfigured,
                    style: TextStyle(fontSize: 12, color: c.textMuted),
                  ),
                ],
              )
            else ...[
              Text(
                s.proxySystemDetected,
                style: TextStyle(fontSize: 11.5, color: c.textMuted),
              ),
              const SizedBox(height: 10),
              // 代理类型
              _ReadOnlyProxyField(
                label: s.proxyType,
                value: _sysProxyType.toUpperCase(),
                colors: c,
              ),
              const SizedBox(height: 8),
              // 地址 + 端口（与手动配置表单布局一致）
              Row(
                children: [
                  SizedBox(
                    width: 80,
                    child: Text(
                      s.proxyHost,
                      style: TextStyle(fontSize: 12, color: c.textSecondary),
                    ),
                  ),
                  Expanded(
                    child: _ReadOnlyValueBox(value: _sysProxyHost, colors: c),
                  ),
                  const SizedBox(width: 8),
                  SizedBox(
                    width: 48,
                    child: Text(
                      s.proxyPort,
                      style: TextStyle(fontSize: 12, color: c.textSecondary),
                      textAlign: TextAlign.center,
                    ),
                  ),
                  SizedBox(
                    width: 90,
                    child: _ReadOnlyValueBox(value: _sysProxyPort, colors: c),
                  ),
                ],
              ),
              if (_sysProxyNoList.isNotEmpty) ...[
                const SizedBox(height: 8),
                _ReadOnlyProxyField(
                  label: s.proxyNoList,
                  value: _sysProxyNoList,
                  colors: c,
                ),
              ],
            ],
          ],
          // 手动配置表单
          if (isManual) ...[
            const SizedBox(height: 16),
            Divider(height: 1, color: m.borderFaint(c.border)),
            const SizedBox(height: 14),
            // 代理类型
            Row(
              children: [
                SizedBox(
                  width: 80,
                  child: Text(
                    s.proxyType,
                    style: TextStyle(fontSize: 12, color: c.textSecondary),
                  ),
                ),
                Expanded(
                  child: ShadSelect<String>(
                    initialValue: sp.proxyType,
                    options: const [
                      ShadOption(value: 'http', child: Text('HTTP')),
                      ShadOption(value: 'https', child: Text('HTTPS')),
                      ShadOption(value: 'socks4', child: Text('SOCKS4')),
                      ShadOption(value: 'socks5', child: Text('SOCKS5')),
                    ],
                    selectedOptionBuilder: (context, value) =>
                        Text(value.toUpperCase()),
                    onChanged: (v) {
                      if (v != null) sp.setProxyType(v);
                    },
                  ),
                ),
              ],
            ),
            const SizedBox(height: 10),
            // 地址 + 端口
            Row(
              children: [
                SizedBox(
                  width: 80,
                  child: Text(
                    s.proxyHost,
                    style: TextStyle(fontSize: 12, color: c.textSecondary),
                  ),
                ),
                Expanded(
                  child: ShadInput(
                    controller: _hostController,
                    placeholder: Text(s.proxyHostPlaceholder),
                    onChanged: (v) => sp.setProxyHost(v),
                  ),
                ),
                const SizedBox(width: 8),
                SizedBox(
                  width: 48,
                  child: Text(
                    s.proxyPort,
                    style: TextStyle(fontSize: 12, color: c.textSecondary),
                    textAlign: TextAlign.center,
                  ),
                ),
                SizedBox(
                  width: 90,
                  child: ShadInput(
                    controller: _portController,
                    placeholder: Text(s.proxyPortPlaceholder),
                    onChanged: (v) => sp.setProxyPort(v),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 10),
            // 用户名 + 密码
            Row(
              children: [
                SizedBox(
                  width: 80,
                  child: Text(
                    s.proxyUsername,
                    style: TextStyle(fontSize: 12, color: c.textSecondary),
                  ),
                ),
                Expanded(
                  child: ShadInput(
                    controller: _usernameController,
                    placeholder: Text(s.proxyUsernamePlaceholder),
                    onChanged: (v) => sp.setProxyUsername(v),
                  ),
                ),
                const SizedBox(width: 8),
                SizedBox(
                  width: 70,
                  child: Text(
                    s.proxyPassword,
                    style: TextStyle(fontSize: 12, color: c.textSecondary),
                    textAlign: TextAlign.center,
                  ),
                ),
                Expanded(
                  child: ShadInput(
                    controller: _passwordController,
                    placeholder: Text(s.proxyPasswordPlaceholder),
                    obscureText: true,
                    onChanged: (v) => sp.setProxyPassword(v),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 10),
            // 排除列表
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                  width: 80,
                  child: Padding(
                    padding: const EdgeInsets.only(top: 8),
                    child: Text(
                      s.proxyNoList,
                      style: TextStyle(fontSize: 12, color: c.textSecondary),
                    ),
                  ),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      ShadInput(
                        controller: _noListController,
                        placeholder: Text(s.proxyNoListPlaceholder),
                        onChanged: (v) => sp.setProxyNoList(v),
                      ),
                      const SizedBox(height: 4),
                      Text(
                        s.proxyNoListDesc,
                        style: TextStyle(fontSize: 10.5, color: c.textMuted),
                      ),
                    ],
                  ),
                ),
              ],
            ),
            const SizedBox(height: 14),
            // 测试连接按钮 + 结果
            Row(
              children: [
                ShadButton.outline(
                  size: ShadButtonSize.sm,
                  enabled:
                      !_isTesting &&
                      sp.proxyHost.isNotEmpty &&
                      sp.proxyPort.isNotEmpty,
                  onPressed: _testProxy,
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      if (_isTesting)
                        SizedBox(
                          width: 12,
                          height: 12,
                          child: CircularProgressIndicator(
                            strokeWidth: 1.5,
                            color: c.textSecondary,
                          ),
                        )
                      else
                        Icon(
                          LucideIcons.plugZap,
                          size: 13,
                          color: c.textSecondary,
                        ),
                      const SizedBox(width: 6),
                      Text(
                        _isTesting ? s.proxyTesting : s.proxyTestConnection,
                        style: TextStyle(fontSize: 12),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                if (_testResult != null)
                  Expanded(
                    child: Text(
                      _testResult!
                          ? s.proxyTestSuccess(_testLatencyMs)
                          : s.proxyTestFailed(_testError),
                      style: TextStyle(
                        fontSize: 11.5,
                        color: _testResult!
                            ? const Color(0xFF22C55E)
                            : const Color(0xFFEF4444),
                      ),
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
              ],
            ),
          ],
        ],
      ),
    );
  }
}

/// 代理模式选项卡片（复用 _ThemeModeCard 的视觉风格）
class _ProxyModeOption extends StatefulWidget {
  final IconData icon;
  final String label;
  final bool selected;
  final AppColors colors;
  final VoidCallback onTap;

  const _ProxyModeOption({
    required this.icon,
    required this.label,
    required this.selected,
    required this.colors,
    required this.onTap,
  });

  @override
  State<_ProxyModeOption> createState() => _ProxyModeOptionState();
}

class _ProxyModeOptionState extends State<_ProxyModeOption> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final theme = ShadTheme.of(context);
    final c = widget.colors;
    final m = AppMetrics.of(context);
    final selected = widget.selected;
    final borderColor = selected ? theme.colorScheme.primary : c.border;
    final bgColor = selected
        ? m.subtle(theme.colorScheme.primary)
        : _isHovered
        ? c.hoverBg
        : c.bg;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 150),
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
          decoration: BoxDecoration(
            color: bgColor,
            borderRadius: m.brCard,
            border: Border.all(color: borderColor, width: selected ? 1.5 : 1),
          ),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Icon(
                widget.icon,
                size: 14,
                color: selected ? theme.colorScheme.primary : c.textSecondary,
              ),
              const SizedBox(width: 6),
              Text(
                widget.label,
                style: TextStyle(
                  fontSize: 12,
                  fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                  color: selected ? theme.colorScheme.primary : c.textSecondary,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 只读代理信息展示字段
class _ReadOnlyProxyField extends StatelessWidget {
  final String label;
  final String value;
  final AppColors colors;

  const _ReadOnlyProxyField({
    required this.label,
    required this.value,
    required this.colors,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Row(
      children: [
        SizedBox(
          width: 80,
          child: Text(
            label,
            style: TextStyle(fontSize: 12, color: colors.textSecondary),
          ),
        ),
        Expanded(
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
            decoration: BoxDecoration(
              color: colors.surface1,
              borderRadius: m.brMd,
              border: Border.all(
                color: colors.border.withValues(alpha: 0.4),
                width: 1,
              ),
            ),
            child: Text(
              value.isEmpty ? '—' : value,
              style: TextStyle(
                fontSize: 12,
                color: value.isEmpty ? colors.textMuted : colors.textPrimary,
              ),
            ),
          ),
        ),
      ],
    );
  }
}

/// 只读代理值展示框（不带 label，用于 Row 内嵌布局）
class _ReadOnlyValueBox extends StatelessWidget {
  final String value;
  final AppColors colors;

  const _ReadOnlyValueBox({required this.value, required this.colors});

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
      decoration: BoxDecoration(
        color: colors.surface1,
        borderRadius: m.brMd,
        border: Border.all(
          color: colors.border.withValues(alpha: 0.4),
          width: 1,
        ),
      ),
      child: Text(
        value.isEmpty ? '—' : value,
        style: TextStyle(
          fontSize: 12,
          color: value.isEmpty ? colors.textMuted : colors.textPrimary,
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// API 服务子组件
// ─────────────────────────────────────────────

class _ApiServiceContent extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _ApiServiceContent({super.key, required this.settingsProvider});

  @override
  State<_ApiServiceContent> createState() => _ApiServiceContentState();
}

class _ApiServiceContentState extends State<_ApiServiceContent> {
  late TextEditingController _portController;
  late FocusNode _portFocusNode;
  late TextEditingController _tokenController;
  late FocusNode _tokenFocusNode;

  @override
  void initState() {
    super.initState();
    _portController = TextEditingController(
      text: widget.settingsProvider.localServerPort.toString(),
    );
    _portFocusNode = FocusNode()..addListener(_onPortFocusChange);
    _tokenController = TextEditingController(
      text: widget.settingsProvider.localServerToken,
    );
    _tokenFocusNode = FocusNode()..addListener(_onTokenFocusChange);
  }

  @override
  void dispose() {
    _portFocusNode.removeListener(_onPortFocusChange);
    _portFocusNode.dispose();
    _portController.dispose();
    _tokenFocusNode.removeListener(_onTokenFocusChange);
    _tokenFocusNode.dispose();
    _tokenController.dispose();
    super.dispose();
  }

  void _onPortFocusChange() {
    if (!_portFocusNode.hasFocus) _commitPort();
  }

  /// 端口失焦/提交时校验 1024-65535；非法则回退为当前生效值
  void _commitPort() {
    final sp = widget.settingsProvider;
    final value = int.tryParse(_portController.text.trim());
    if (value == null || value < 1024 || value > 65535) {
      setState(() => _portController.text = sp.localServerPort.toString());
      ShadSonner.of(context).show(
        ShadToast.destructive(
          title: Text(LocaleScope.of(context).apiServicePortInvalid),
        ),
      );
      return;
    }
    sp.setLocalServerPort(value);
  }

  void _onTokenFocusChange() {
    if (!_tokenFocusNode.hasFocus) _commitToken();
  }

  /// token 失焦提交：允许自定义任意值（含清空），去除首尾空白后持久化。
  void _commitToken() {
    final sp = widget.settingsProvider;
    final value = _tokenController.text.trim();
    if (value != _tokenController.text) _tokenController.text = value;
    if (value != sp.localServerToken) sp.setLocalServerToken(value);
  }

  /// 生成 32 位随机 hex token
  String _generateHexToken() {
    final r = Random.secure();
    return List<int>.generate(
      16,
      (_) => r.nextInt(256),
    ).map((b) => b.toRadixString(16).padLeft(2, '0')).join();
  }

  Future<void> _copyToken() async {
    await Clipboard.setData(
      ClipboardData(text: widget.settingsProvider.localServerToken),
    );
    if (!mounted) return;
    ShadSonner.of(context).show(
      ShadToast(
        title: Text(LocaleScope.of(context).apiServiceCopied),
        duration: const Duration(seconds: 2),
      ),
    );
  }

  void _clearToken() {
    widget.settingsProvider.clearLocalServerToken();
    if (!mounted) return;
    ShadSonner.of(context).show(
      ShadToast(
        title: Text(LocaleScope.of(context).apiServiceTokenCleared),
        duration: const Duration(seconds: 2),
      ),
    );
  }

  void _confirmClearToken() {
    final sp = widget.settingsProvider;
    // token 已空且管理 API 未启用：无需确认，直接返回（按钮通常已禁用）。
    if (sp.localServerToken.isEmpty && !sp.localServerApiEnabled) return;
    // 管理 API 未依赖此令牌：直接清空，无需二次确认。
    if (!sp.localServerApiEnabled) {
      _clearToken();
      return;
    }
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.apiServiceTokenClearConfirmTitle),
        description: Text(s.apiServiceTokenClearConfirmDesc),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
          ShadButton.destructive(
            onPressed: () {
              Navigator.of(ctx).pop();
              _clearToken();
            },
            child: Text(s.apiServiceTokenClear),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.settingsProvider,
      builder: (context, _) {
        final c = AppColors.of(context);
        final m = AppMetrics.of(context);
        final s = LocaleScope.of(context);
        final sp = widget.settingsProvider;
        final enabled = sp.localServerEnabled;
        final committedPortText = sp.localServerPort.toString();
        // 未获焦时随外部配置变化同步（如首次加载配置完成）
        if (!_portFocusNode.hasFocus &&
            _portController.text != committedPortText) {
          _portController.text = committedPortText;
        }
        // token 未获焦时随外部配置变化同步（生成/清空/首次加载配置）
        if (!_tokenFocusNode.hasFocus &&
            _tokenController.text != sp.localServerToken) {
          _tokenController.text = sp.localServerToken;
        }
        // 地址预览随端口输入框实时更新，不等待失焦提交
        final typedPort = _portController.text.trim();
        final livePort = typedPort.isEmpty ? committedPortText : typedPort;

        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
              decoration: BoxDecoration(
                color: c.surface1,
                borderRadius: m.brDialog,
                border: Border.all(
                  color: m.borderMedium(c.border),
                  width: 1,
                ),
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  // 标题 + 描述
                  Text(
                    s.apiServiceEnable,
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w500,
                      color: c.textPrimary,
                    ),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    s.apiServiceEnableDesc,
                    style: TextStyle(fontSize: 11.5, color: c.textMuted),
                  ),
                  const SizedBox(height: 12),
                  // 总开关
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          s.apiServiceEnable,
                          style: TextStyle(fontSize: 13, color: c.textPrimary),
                        ),
                      ),
                      ShadSwitch(
                        value: enabled,
                        onChanged: (v) => sp.setLocalServerEnabled(v),
                      ),
                    ],
                  ),
                  const SizedBox(height: 14),
                  Divider(height: 1, color: m.borderFaint(c.border)),
                  const SizedBox(height: 14),
                  // 端口行
                  Row(
                    children: [
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(
                              s.apiServicePort,
                              style: TextStyle(
                                fontSize: 13,
                                fontWeight: FontWeight.w500,
                                color: enabled ? c.textPrimary : c.textDisabled,
                              ),
                            ),
                            const SizedBox(height: 2),
                            Text(
                              s.apiServicePortDesc,
                              style: TextStyle(
                                fontSize: 11.5,
                                color: enabled ? c.textMuted : c.textDisabled,
                              ),
                            ),
                          ],
                        ),
                      ),
                      const SizedBox(width: 12),
                      SizedBox(
                        width: 120,
                        child: ShadInput(
                          controller: _portController,
                          focusNode: _portFocusNode,
                          enabled: enabled,
                          keyboardType: TextInputType.number,
                          onChanged: (_) => setState(() {}),
                          onSubmitted: (_) => _commitPort(),
                        ),
                      ),
                    ],
                  ),
                  const SizedBox(height: 14),
                  // Token 行
                  Text(
                    s.apiServiceToken,
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w500,
                      color: enabled ? c.textPrimary : c.textDisabled,
                    ),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    s.apiServiceTokenDesc,
                    style: TextStyle(
                      fontSize: 11.5,
                      color: enabled ? c.textMuted : c.textDisabled,
                    ),
                  ),
                  const SizedBox(height: 8),
                  Row(
                    spacing: 8,
                    children: [
                      Expanded(
                        child: ShadInput(
                          controller: _tokenController,
                          focusNode: _tokenFocusNode,
                          enabled: enabled,
                          onSubmitted: (_) => _commitToken(),
                        ),
                      ),
                      ShadButton.outline(
                        size: ShadButtonSize.sm,
                        enabled: enabled,
                        onPressed: () {
                          final t = _generateHexToken();
                          _tokenController.text = t;
                          sp.setLocalServerToken(t);
                        },
                        child: Text(s.apiServiceTokenGenerate),
                      ),
                      ShadButton.outline(
                        size: ShadButtonSize.sm,
                        enabled: enabled && sp.localServerToken.isNotEmpty,
                        onPressed: _copyToken,
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Icon(LucideIcons.copy, size: 13),
                            const SizedBox(width: 4),
                            Text(s.apiServiceCopy),
                          ],
                        ),
                      ),
                      ShadButton.outline(
                        size: ShadButtonSize.sm,
                        enabled:
                            enabled &&
                            (sp.localServerToken.isNotEmpty ||
                                sp.localServerApiEnabled),
                        onPressed: _confirmClearToken,
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Icon(LucideIcons.eraser, size: 13),
                            const SizedBox(width: 4),
                            Text(s.apiServiceTokenClear),
                          ],
                        ),
                      ),
                    ],
                  ),
                ],
              ),
            ),
            const SizedBox(height: 20),
            _HighlightRegion(
              label: s.apiServiceFeaturesTitle,
              description: s.apiServiceFeaturesDesc,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Padding(
                    padding: const EdgeInsets.only(bottom: 8),
                    child: Text(
                      s.apiServiceFeaturesTitle,
                      style: TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w600,
                        color: c.textPrimary,
                      ),
                    ),
                  ),
                  Text(
                    s.apiServiceFeaturesDesc,
                    style: TextStyle(fontSize: 11.5, color: c.textMuted),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 10),
            _ApiSubFeatureCard(
              masterEnabled: enabled,
              label: s.apiServiceTakeover,
              description: s.apiServiceTakeoverDesc,
              value: sp.localServerTakeoverEnabled,
              onChanged: (v) => sp.setLocalServerTakeoverEnabled(v),
              address: 'http://127.0.0.1:$livePort',
              extra: _CopyUserscriptButton(enabled: enabled),
            ),
            const SizedBox(height: 10),
            _ApiSubFeatureCard(
              masterEnabled: enabled,
              label: s.apiServiceJsonrpc,
              description: s.apiServiceJsonrpcDesc,
              value: sp.localServerJsonrpcEnabled,
              onChanged: (v) => sp.setLocalServerJsonrpcEnabled(v),
              address: 'http://127.0.0.1:$livePort/jsonrpc',
            ),
            const SizedBox(height: 10),
            _ApiSubFeatureCard(
              masterEnabled: enabled,
              label: s.apiServiceApi,
              description: s.apiServiceApiDesc,
              value: sp.localServerApiEnabled,
              onChanged: (v) => sp.setLocalServerApiEnabled(v),
              address: 'http://127.0.0.1:$livePort/api/v1',
            ),
            const SizedBox(height: 10),
            _ApiSubFeatureCard(
              masterEnabled: enabled,
              label: s.apiServiceMcp,
              description: s.apiServiceMcpDesc,
              value: sp.localServerMcpEnabled,
              onChanged: (v) => sp.setLocalServerMcpEnabled(v),
              address: 'http://127.0.0.1:$livePort/mcp',
            ),
          ],
        );
      },
    );
  }
}

/// 「功能开关」分组下的单个子功能卡片：标题+描述+开关 与 只读地址+复制 两行。
/// [masterEnabled] 为 false 时整卡禁用置灰（跟随总开关，无需重启提示）。
class _ApiSubFeatureCard extends StatelessWidget {
  final bool masterEnabled;
  final String label;
  final String description;
  final bool value;
  final ValueChanged<bool> onChanged;
  final String address;
  final Widget? extra;

  const _ApiSubFeatureCard({
    required this.masterEnabled,
    required this.label,
    required this.description,
    required this.value,
    required this.onChanged,
    required this.address,
    this.extra,
  });

  Future<void> _copyAddress(BuildContext context) async {
    await Clipboard.setData(ClipboardData(text: address));
    if (!context.mounted) return;
    ShadSonner.of(context).show(
      ShadToast(
        title: Text(LocaleScope.of(context).apiServiceCopied),
        duration: const Duration(seconds: 2),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brDialog,
        border: Border.all(color: m.borderMedium(c.border), width: 1),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      label,
                      style: TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w500,
                        color: masterEnabled ? c.textPrimary : c.textDisabled,
                      ),
                    ),
                    const SizedBox(height: 2),
                    Text(
                      description,
                      style: TextStyle(
                        fontSize: 11.5,
                        color: masterEnabled ? c.textMuted : c.textDisabled,
                      ),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 16),
              ShadSwitch(
                value: value,
                enabled: masterEnabled,
                onChanged: onChanged,
              ),
            ],
          ),
          const SizedBox(height: 10),
          Row(
            children: [
              SizedBox(
                width: 56,
                child: Text(
                  s.apiServiceAddress,
                  style: TextStyle(
                    fontSize: 12,
                    color: masterEnabled ? c.textSecondary : c.textDisabled,
                  ),
                ),
              ),
              Expanded(
                child: _ReadOnlyValueBox(value: address, colors: c),
              ),
              const SizedBox(width: 8),
              ShadButton.outline(
                size: ShadButtonSize.sm,
                enabled: masterEnabled,
                onPressed: () => _copyAddress(context),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(LucideIcons.copy, size: 13),
                    const SizedBox(width: 4),
                    Text(s.apiServiceCopy),
                  ],
                ),
              ),
            ],
          ),
          if (extra != null) ...[const SizedBox(height: 10), extra!],
        ],
      ),
    );
  }
}

/// 复制内置油猴脚本到剪贴板（浏览器脚本接管子功能的便捷入口）
class _CopyUserscriptButton extends StatelessWidget {
  final bool enabled;

  const _CopyUserscriptButton({required this.enabled});

  Future<void> _copy(BuildContext context) async {
    final s = LocaleScope.of(context);
    try {
      final script = await rootBundle.loadString('userscript/fluxdown.user.js');
      await Clipboard.setData(ClipboardData(text: script));
      if (!context.mounted) return;
      ShadSonner.of(context).show(
        ShadToast(
          title: Text(s.apiServiceScriptCopied),
          duration: const Duration(seconds: 3),
        ),
      );
    } catch (e) {
      if (!context.mounted) return;
      ShadSonner.of(
        context,
      ).show(ShadToast.destructive(title: Text('Error: $e')));
    }
  }

  @override
  Widget build(BuildContext context) {
    return ShadButton.outline(
      size: ShadButtonSize.sm,
      enabled: enabled,
      onPressed: () => _copy(context),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(LucideIcons.code, size: 13),
          const SizedBox(width: 4),
          Text(LocaleScope.of(context).apiServiceCopyScript),
        ],
      ),
    );
  }
}

// ─────────────────────────────────────────────
// BT 设置子组件
// ─────────────────────────────────────────────

/// BT 监听端口范围编辑器（起始端口 / 结束端口）
class _BtPortRangeEditor extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _BtPortRangeEditor({required this.settingsProvider});

  @override
  State<_BtPortRangeEditor> createState() => _BtPortRangeEditorState();
}

class _BtPortRangeEditorState extends State<_BtPortRangeEditor> {
  late TextEditingController _startController;
  late TextEditingController _endController;
  String? _error;

  @override
  void initState() {
    super.initState();
    final sp = widget.settingsProvider;
    _startController = TextEditingController(text: '${sp.btPortStart}');
    _endController = TextEditingController(text: '${sp.btPortEnd}');
  }

  @override
  void didUpdateWidget(_BtPortRangeEditor oldWidget) {
    super.didUpdateWidget(oldWidget);
    final sp = widget.settingsProvider;
    final start = int.tryParse(_startController.text);
    final end = int.tryParse(_endController.text);
    if (sp.btPortStart != start) {
      _startController.text = '${sp.btPortStart}';
    }
    if (sp.btPortEnd != end) {
      _endController.text = '${sp.btPortEnd}';
    }
  }

  @override
  void dispose() {
    _startController.dispose();
    _endController.dispose();
    super.dispose();
  }

  bool _isValid(int start, int end) {
    return start >= 1024 &&
        start <= 65535 &&
        end >= 1024 &&
        end <= 65535 &&
        start <= end;
  }

  void _tryCommit() {
    final start = int.tryParse(_startController.text);
    final end = int.tryParse(_endController.text);
    if (start == null || end == null || !_isValid(start, end)) {
      setState(() => _error = LocaleScope.of(context).btPortInvalid);
      return;
    }
    setState(() => _error = null);
    widget.settingsProvider.setBtPortStart(start);
    widget.settingsProvider.setBtPortEnd(end);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            SizedBox(
              width: 110,
              child: ShadInput(
                controller: _startController,
                placeholder: Text(s.btListenPortStart),
                keyboardType: TextInputType.number,
                onSubmitted: (_) => _tryCommit(),
                onChanged: (_) => _tryCommit(),
              ),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              child: Text(
                '–',
                style: TextStyle(fontSize: 14, color: c.textMuted),
              ),
            ),
            SizedBox(
              width: 110,
              child: ShadInput(
                controller: _endController,
                placeholder: Text(s.btListenPortEnd),
                keyboardType: TextInputType.number,
                onSubmitted: (_) => _tryCommit(),
                onChanged: (_) => _tryCommit(),
              ),
            ),
          ],
        ),
        if (_error != null) ...[
          const SizedBox(height: 6),
          Text(_error!, style: TextStyle(fontSize: 11.5, color: c.statusError)),
        ],
      ],
    );
  }
}

/// ED2K 监听端口编辑器（单端口；0 或空 = OS 自动选择）。
class _Ed2kListenPortEditor extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _Ed2kListenPortEditor({required this.settingsProvider});

  @override
  State<_Ed2kListenPortEditor> createState() => _Ed2kListenPortEditorState();
}

class _Ed2kListenPortEditorState extends State<_Ed2kListenPortEditor> {
  late TextEditingController _controller;
  String? _error;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController(
      text: '${widget.settingsProvider.ed2kListenPort}',
    );
  }

  @override
  void didUpdateWidget(_Ed2kListenPortEditor oldWidget) {
    super.didUpdateWidget(oldWidget);
    final port = int.tryParse(_controller.text);
    if (widget.settingsProvider.ed2kListenPort != port) {
      _controller.text = '${widget.settingsProvider.ed2kListenPort}';
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _tryCommit() {
    final raw = _controller.text.trim();
    // 空或 0 = OS 自动选择端口。
    final port = raw.isEmpty ? 0 : int.tryParse(raw);
    if (port == null || port < 0 || port > 65535) {
      setState(() => _error = LocaleScope.of(context).btPortInvalid);
      return;
    }
    setState(() => _error = null);
    widget.settingsProvider.setEd2kListenPort(port);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.end,
      children: [
        SizedBox(
          width: 130,
          child: ShadInput(
            controller: _controller,
            placeholder: const Text('0'),
            keyboardType: TextInputType.number,
            onSubmitted: (_) => _tryCommit(),
            onChanged: (_) => _tryCommit(),
          ),
        ),
        if (_error != null) ...[
          const SizedBox(height: 6),
          Text(_error!, style: TextStyle(fontSize: 11.5, color: c.statusError)),
        ],
      ],
    );
  }
}

/// BT Tracker 列表编辑器
///
/// 使用与 [new_download_dialog] 相同的 Localizations + Material + TextField
/// 方案，确保鼠标选择、复制粘贴等功能正常。
class _BtTrackerEditor extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _BtTrackerEditor({required this.settingsProvider});

  @override
  State<_BtTrackerEditor> createState() => _BtTrackerEditorState();
}

/// 内置默认 Tracker 列表（与 Rust 端 PUBLIC_TRACKERS 保持同步）。
/// "重置为默认"时用此列表恢复。
const _kDefaultTrackers = [
  // CN / Asia
  'udp://tracker.dler.com:6969/announce',
  'udp://admin.52ywp.com:6969/announce',
  'udp://tracker.dler.org:6969/announce',
  'https://tracker.moeblog.cn:443/announce',
  'http://nyaa.tracker.wf:7777/announce',
  'https://tr.zukizuki.org:443/announce',
  // International
  'udp://tracker.opentrackr.org:1337/announce',
  'udp://open.dstud.io:6969/announce',
  'udp://tracker-udp.gbitt.info:80/announce',
  'udp://open.stealth.si:80/announce',
  'udp://tracker.torrent.eu.org:451/announce',
  'udp://exodus.desync.com:6969/announce',
  'udp://explodie.org:6969/announce',
  'udp://tracker.srv00.com:6969/announce',
  'udp://tracker.qu.ax:6969/announce',
  'udp://opentracker.io:6969/announce',
  'udp://tracker.bittor.pw:1337/announce',
  'udp://tracker.theoks.net:6969/announce',
  'udp://tracker.opentorrent.top:6969/announce',
  'udp://open.demonoid.ch:6969/announce',
  'udp://tracker.t-1.org:6969/announce',
  // HTTPS fallbacks
  'https://tracker.ghostchu-services.top:443/announce',
  'https://tracker.bt4g.com:443/announce',
  'https://1337.abcvg.info:443/announce',
  'http://tracker.bt4g.com:2095/announce',
];

class _BtTrackerEditorState extends State<_BtTrackerEditor> {
  late TextEditingController _controller;
  bool _isExpanded = false;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController(
      text: widget.settingsProvider.btCustomTrackers,
    );
  }

  @override
  void didUpdateWidget(_BtTrackerEditor oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 仅在外部值变化时同步（例如从 Rust 端加载初始值）
    if (widget.settingsProvider.btCustomTrackers != _controller.text &&
        !_isExpanded) {
      _controller.text = widget.settingsProvider.btCustomTrackers;
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _save() {
    // 清洗 + 去重：按 trim 后小写、去尾部斜杠的形式判重，保留首次出现的原始行
    // （Rust 端合并订阅时还会按 URL 规范化形式再去重一次）
    final seen = <String>{};
    final lines = <String>[];
    for (final raw in _controller.text.split('\n')) {
      final l = raw.trim();
      if (l.isEmpty) continue;
      final key = l.toLowerCase().replaceAll(RegExp(r'/+$'), '');
      if (seen.add(key)) lines.add(l);
    }
    final cleaned = lines.join('\n');
    _controller.text = cleaned;
    widget.settingsProvider.setBtCustomTrackers(cleaned);
  }

  int get _trackerCount {
    final text = _controller.text.trim();
    if (text.isEmpty) return 0;
    return text.split('\n').where((l) => l.trim().isNotEmpty).length;
  }

  void _resetToDefault() {
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog.alert(
        title: Text(LocaleScope.of(ctx).btResetTrackers),
        description: Text(LocaleScope.of(ctx).btResetTrackersConfirm),
        actions: [
          ShadButton.outline(
            child: Text(LocaleScope.of(ctx).cancel),
            onPressed: () => Navigator.of(ctx).pop(),
          ),
          ShadButton(
            child: Text(LocaleScope.of(ctx).confirm),
            onPressed: () {
              Navigator.of(ctx).pop();
              final defaults = _kDefaultTrackers.join('\n');
              _controller.text = defaults;
              widget.settingsProvider.setBtCustomTrackers(defaults);
              setState(() {});
            },
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // 统计行 + 按钮
        Row(
          children: [
            Text(
              s.btTrackerCount(_trackerCount),
              style: TextStyle(fontSize: 12, color: c.textMuted),
            ),
            const Spacer(),
            ShadButton.ghost(
              size: ShadButtonSize.sm,
              onPressed: _resetToDefault,
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(LucideIcons.rotateCcw, size: 12, color: c.textSecondary),
                  const SizedBox(width: 4),
                  Text(
                    s.btResetTrackers,
                    style: TextStyle(fontSize: 11, color: c.textSecondary),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 4),
            ShadButton.ghost(
              size: ShadButtonSize.sm,
              onPressed: () => setState(() => _isExpanded = !_isExpanded),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(
                    _isExpanded
                        ? LucideIcons.chevronUp
                        : LucideIcons.chevronDown,
                    size: 14,
                    color: c.textSecondary,
                  ),
                  const SizedBox(width: 4),
                  Text(
                    _isExpanded ? s.cancel : s.manage,
                    style: TextStyle(fontSize: 11, color: c.textSecondary),
                  ),
                ],
              ),
            ),
          ],
        ),
        // 展开时显示多行编辑区（与 new_download_dialog 一致的实现）
        if (_isExpanded) ...[
          const SizedBox(height: 8),
          SizedBox(
            height: 240,
            child: Localizations(
              locale: const Locale('en'),
              delegates: const [
                DefaultWidgetsLocalizations.delegate,
                DefaultMaterialLocalizations.delegate,
              ],
              child: Material(
                type: MaterialType.transparency,
                child: TextSelectionTheme(
                  data: TextSelectionThemeData(
                    selectionColor: m.textSelection(c.accent),
                    cursorColor: c.accent,
                    selectionHandleColor: c.accent,
                  ),
                  child: TextField(
                    controller: _controller,
                    maxLines: null,
                    expands: true,
                    textAlignVertical: TextAlignVertical.top,
                    cursorColor: c.accent,
                    style: TextStyle(
                      fontSize: 12,
                      color: c.textPrimary,
                      fontFamily: 'monospace',
                      height: 1.5,
                    ),
                    decoration: InputDecoration(
                      hintText: s.btTrackerPlaceholder,
                      hintStyle: TextStyle(fontSize: 12, color: c.textMuted),
                      hintMaxLines: 5,
                      contentPadding: const EdgeInsets.all(10),
                      filled: true,
                      fillColor: c.inputBg,
                      hoverColor: Colors.transparent,
                      border: OutlineInputBorder(
                        borderRadius: m.brInput,
                        borderSide: BorderSide(color: c.inputBorder),
                      ),
                      enabledBorder: OutlineInputBorder(
                        borderRadius: m.brInput,
                        borderSide: BorderSide(color: c.inputBorder),
                      ),
                      focusedBorder: OutlineInputBorder(
                        borderRadius: m.brInput,
                        borderSide: BorderSide(color: c.inputFocusBorder),
                      ),
                    ),
                    onChanged: (_) => setState(() {}),
                  ),
                ),
              ),
            ),
          ),
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerRight,
            child: ShadButton(
              size: ShadButtonSize.sm,
              onPressed: () {
                _save();
                setState(() => _isExpanded = false);
              },
              child: Text(s.confirm),
            ),
          ),
        ],
      ],
    );
  }
}

// ─────────────────────────────────────────────
// BT Tracker 订阅编辑器
// ─────────────────────────────────────────────

/// 默认订阅地址（与 Rust 端 tracker_subscription::DEFAULT_SUBSCRIPTION_URLS 保持同步）：
/// - trackerslist.com — XIU2/TrackersListCollection 官方 CDN（中文社区最流行，国内可达）
/// - ngosang.github.io — ngosang/trackerslist（每日自动更新，按延迟排序）
const _kDefaultTrackerSubUrls = [
  'https://trackerslist.com/best.txt',
  'https://ngosang.github.io/trackerslist/trackers_best.txt',
];

/// BT Tracker 订阅编辑器：启用开关 + 订阅状态 + 立即更新 + 订阅地址管理。
class _BtTrackerSubEditor extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _BtTrackerSubEditor({required this.settingsProvider});

  @override
  State<_BtTrackerSubEditor> createState() => _BtTrackerSubEditorState();
}

class _BtTrackerSubEditorState extends State<_BtTrackerSubEditor> {
  late TextEditingController _controller;
  bool _isExpanded = false;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController(
      text: widget.settingsProvider.btTrackerSubUrls,
    );
  }

  @override
  void didUpdateWidget(_BtTrackerSubEditor oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 仅在外部值变化时同步（例如从 Rust 端加载初始值）
    if (widget.settingsProvider.btTrackerSubUrls != _controller.text &&
        !_isExpanded) {
      _controller.text = widget.settingsProvider.btTrackerSubUrls;
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _save() {
    // 清洗 + 去重（忽略大小写与尾部斜杠差异）
    final seen = <String>{};
    final lines = <String>[];
    for (final raw in _controller.text.split('\n')) {
      final l = raw.trim();
      if (l.isEmpty) continue;
      final key = l.toLowerCase().replaceAll(RegExp(r'/+$'), '');
      if (seen.add(key)) lines.add(l);
    }
    final cleaned = lines.join('\n');
    _controller.text = cleaned;
    widget.settingsProvider.setBtTrackerSubUrls(cleaned);
  }

  void _resetToDefault() {
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog.alert(
        title: Text(LocaleScope.of(ctx).btResetTrackers),
        description: Text(LocaleScope.of(ctx).btTrackerSubResetConfirm),
        actions: [
          ShadButton.outline(
            child: Text(LocaleScope.of(ctx).cancel),
            onPressed: () => Navigator.of(ctx).pop(),
          ),
          ShadButton(
            child: Text(LocaleScope.of(ctx).confirm),
            onPressed: () {
              Navigator.of(ctx).pop();
              final defaults = _kDefaultTrackerSubUrls.join('\n');
              _controller.text = defaults;
              widget.settingsProvider.setBtTrackerSubUrls(defaults);
              setState(() {});
            },
          ),
        ],
      ),
    );
  }

  String _formatUpdatedAt(int unixSecs) {
    final dt = DateTime.fromMillisecondsSinceEpoch(unixSecs * 1000);
    String two(int v) => v.toString().padLeft(2, '0');
    return '${dt.year}-${two(dt.month)}-${two(dt.day)} '
        '${two(dt.hour)}:${two(dt.minute)}';
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final sp = widget.settingsProvider;

    // 状态文本：已订阅 N 个 Tracker · 更新于… / 尚未更新 / 更新中
    final String statusText;
    if (sp.btTrackerSubRefreshing) {
      statusText = s.btTrackerSubUpdating;
    } else if (sp.btTrackerSubUpdatedAt > 0) {
      statusText =
          '${s.btTrackerSubStatus(sp.btTrackerSubCount)} · '
          '${s.btTrackerSubUpdatedAt(_formatUpdatedAt(sp.btTrackerSubUpdatedAt))}';
    } else {
      statusText = s.btTrackerSubNeverUpdated;
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // 状态行 + 启用开关
        Row(
          children: [
            Expanded(
              child: Text(
                statusText,
                style: TextStyle(fontSize: 12, color: c.textMuted),
              ),
            ),
            ShadSwitch(
              value: sp.btTrackerSubEnabled,
              onChanged: (v) => sp.setBtTrackerSubEnabled(v),
            ),
          ],
        ),
        if (sp.btTrackerSubEnabled) ...[
          // 错误提示
          if (sp.btTrackerSubLastError.isNotEmpty) ...[
            const SizedBox(height: 4),
            Text(
              '${s.btTrackerSubUpdateFailed}: ${sp.btTrackerSubLastError}',
              style: TextStyle(fontSize: 11, color: c.statusError),
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
            ),
          ],
          const SizedBox(height: 4),
          // 操作行：立即更新 / 管理订阅地址
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              ShadButton.ghost(
                size: ShadButtonSize.sm,
                onPressed: sp.btTrackerSubRefreshing
                    ? null
                    : () => sp.refreshTrackerSubscription(),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    if (sp.btTrackerSubRefreshing)
                      SizedBox(
                        width: 12,
                        height: 12,
                        child: CircularProgressIndicator(
                          strokeWidth: 2,
                          color: c.textSecondary,
                        ),
                      )
                    else
                      Icon(
                        LucideIcons.refreshCw,
                        size: 12,
                        color: c.textSecondary,
                      ),
                    const SizedBox(width: 4),
                    Text(
                      sp.btTrackerSubRefreshing
                          ? s.btTrackerSubUpdating
                          : s.btTrackerSubUpdateNow,
                      style: TextStyle(fontSize: 11, color: c.textSecondary),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 4),
              ShadButton.ghost(
                size: ShadButtonSize.sm,
                onPressed: () => setState(() => _isExpanded = !_isExpanded),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(
                      _isExpanded
                          ? LucideIcons.chevronUp
                          : LucideIcons.chevronDown,
                      size: 14,
                      color: c.textSecondary,
                    ),
                    const SizedBox(width: 4),
                    Text(
                      _isExpanded ? s.cancel : s.manage,
                      style: TextStyle(fontSize: 11, color: c.textSecondary),
                    ),
                  ],
                ),
              ),
            ],
          ),
          // 展开时显示订阅地址编辑区（与 Tracker 编辑器一致的实现）
          if (_isExpanded) ...[
            const SizedBox(height: 8),
            SizedBox(
              height: 100,
              child: Localizations(
                locale: const Locale('en'),
                delegates: const [
                  DefaultWidgetsLocalizations.delegate,
                  DefaultMaterialLocalizations.delegate,
                ],
                child: Material(
                  type: MaterialType.transparency,
                  child: TextSelectionTheme(
                    data: TextSelectionThemeData(
                      selectionColor: m.textSelection(c.accent),
                      cursorColor: c.accent,
                      selectionHandleColor: c.accent,
                    ),
                    child: TextField(
                      controller: _controller,
                      maxLines: null,
                      expands: true,
                      textAlignVertical: TextAlignVertical.top,
                      cursorColor: c.accent,
                      style: TextStyle(
                        fontSize: 12,
                        color: c.textPrimary,
                        fontFamily: 'monospace',
                        height: 1.5,
                      ),
                      decoration: InputDecoration(
                        hintText: s.btTrackerSubPlaceholder,
                        hintStyle: TextStyle(fontSize: 12, color: c.textMuted),
                        hintMaxLines: 3,
                        contentPadding: const EdgeInsets.all(10),
                        filled: true,
                        fillColor: c.inputBg,
                        hoverColor: Colors.transparent,
                        border: OutlineInputBorder(
                          borderRadius: m.brInput,
                          borderSide: BorderSide(color: c.inputBorder),
                        ),
                        enabledBorder: OutlineInputBorder(
                          borderRadius: m.brInput,
                          borderSide: BorderSide(color: c.inputBorder),
                        ),
                        focusedBorder: OutlineInputBorder(
                          borderRadius: m.brInput,
                          borderSide: BorderSide(color: c.inputFocusBorder),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                ShadButton.ghost(
                  size: ShadButtonSize.sm,
                  onPressed: _resetToDefault,
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(
                        LucideIcons.rotateCcw,
                        size: 12,
                        color: c.textSecondary,
                      ),
                      const SizedBox(width: 4),
                      Text(
                        s.btResetTrackers,
                        style: TextStyle(fontSize: 11, color: c.textSecondary),
                      ),
                    ],
                  ),
                ),
                const Spacer(),
                ShadButton(
                  size: ShadButtonSize.sm,
                  onPressed: () {
                    _save();
                    setState(() => _isExpanded = false);
                  },
                  child: Text(s.confirm),
                ),
              ],
            ),
          ],
        ],
      ],
    );
  }
}

// ─────────────────────────────────────────────
// ED2K 服务器编辑器
// ─────────────────────────────────────────────

/// 内置默认 ED2K 服务器列表（与 Rust 端 db.rs 的 ed2k_server_list 默认值同步）。
/// "重置为默认"时用此列表恢复。
const _kDefaultEd2kServers = [
  '176.123.5.89:4725',
  '45.82.80.155:5687',
  '85.121.5.137:4232',
  '176.123.2.239:4232',
  '145.239.2.134:4661',
  '91.208.162.87:4232',
  '37.15.61.236:4232',
];

/// 默认 server.met 订阅地址（与 Rust 端 server_subscription::DEFAULT_SERVER_MET_URLS 同步）。
const _kDefaultEd2kMetUrls = [
  'http://upd.emule-security.org/server.met',
  'https://www.shortypower.org/server.met',
];

/// ED2K 手填服务器编辑器：统计 + 重置默认 + 可展开多行编辑（每行一个 host:port）。
/// 存储为逗号分隔（与 Rust `parse_server_list` 一致），编辑区按行展示。
class _Ed2kServerEditor extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _Ed2kServerEditor({required this.settingsProvider});

  @override
  State<_Ed2kServerEditor> createState() => _Ed2kServerEditorState();
}

class _Ed2kServerEditorState extends State<_Ed2kServerEditor> {
  late TextEditingController _controller;
  bool _isExpanded = false;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController(
      text: _toLines(widget.settingsProvider.ed2kServerList),
    );
  }

  @override
  void didUpdateWidget(_Ed2kServerEditor oldWidget) {
    super.didUpdateWidget(oldWidget);
    final asLines = _toLines(widget.settingsProvider.ed2kServerList);
    if (asLines != _controller.text && !_isExpanded) {
      _controller.text = asLines;
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  /// 逗号分隔 → 每行一个（编辑展示）。
  static String _toLines(String csv) =>
      csv.split(',').map((s) => s.trim()).where((s) => s.isNotEmpty).join('\n');

  void _save() {
    // 清洗 + 去重（按 trim 后的小写形式判重），存储为逗号分隔。
    final seen = <String>{};
    final items = <String>[];
    for (final raw in _controller.text.split(RegExp(r'[\n,]'))) {
      final l = raw.trim();
      if (l.isEmpty) continue;
      if (seen.add(l.toLowerCase())) items.add(l);
    }
    _controller.text = items.join('\n');
    widget.settingsProvider.setEd2kServerList(items.join(','));
  }

  int get _serverCount {
    final text = _controller.text.trim();
    if (text.isEmpty) return 0;
    return text
        .split(RegExp(r'[\n,]'))
        .where((l) => l.trim().isNotEmpty)
        .length;
  }

  void _resetToDefault() {
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog.alert(
        title: Text(LocaleScope.of(ctx).ed2kResetServers),
        description: Text(LocaleScope.of(ctx).ed2kResetServersConfirm),
        actions: [
          ShadButton.outline(
            child: Text(LocaleScope.of(ctx).cancel),
            onPressed: () => Navigator.of(ctx).pop(),
          ),
          ShadButton(
            child: Text(LocaleScope.of(ctx).confirm),
            onPressed: () {
              Navigator.of(ctx).pop();
              _controller.text = _kDefaultEd2kServers.join('\n');
              widget.settingsProvider.setEd2kServerList(
                _kDefaultEd2kServers.join(','),
              );
              setState(() {});
            },
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Text(
              s.ed2kServerCount(_serverCount),
              style: TextStyle(fontSize: 12, color: c.textMuted),
            ),
            const Spacer(),
            ShadButton.ghost(
              size: ShadButtonSize.sm,
              onPressed: _resetToDefault,
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(LucideIcons.rotateCcw, size: 12, color: c.textSecondary),
                  const SizedBox(width: 4),
                  Text(
                    s.ed2kResetServers,
                    style: TextStyle(fontSize: 11, color: c.textSecondary),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 4),
            ShadButton.ghost(
              size: ShadButtonSize.sm,
              onPressed: () => setState(() => _isExpanded = !_isExpanded),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(
                    _isExpanded
                        ? LucideIcons.chevronUp
                        : LucideIcons.chevronDown,
                    size: 14,
                    color: c.textSecondary,
                  ),
                  const SizedBox(width: 4),
                  Text(
                    _isExpanded ? s.cancel : s.manage,
                    style: TextStyle(fontSize: 11, color: c.textSecondary),
                  ),
                ],
              ),
            ),
          ],
        ),
        if (_isExpanded) ...[
          const SizedBox(height: 8),
          SizedBox(
            height: 200,
            child: Localizations(
              locale: const Locale('en'),
              delegates: const [
                DefaultWidgetsLocalizations.delegate,
                DefaultMaterialLocalizations.delegate,
              ],
              child: Material(
                type: MaterialType.transparency,
                child: TextSelectionTheme(
                  data: TextSelectionThemeData(
                    selectionColor: m.textSelection(c.accent),
                    cursorColor: c.accent,
                    selectionHandleColor: c.accent,
                  ),
                  child: TextField(
                    controller: _controller,
                    maxLines: null,
                    expands: true,
                    textAlignVertical: TextAlignVertical.top,
                    cursorColor: c.accent,
                    style: TextStyle(
                      fontSize: 12,
                      color: c.textPrimary,
                      fontFamily: 'monospace',
                      height: 1.5,
                    ),
                    decoration: InputDecoration(
                      hintText: s.ed2kServerPlaceholder,
                      hintStyle: TextStyle(fontSize: 12, color: c.textMuted),
                      hintMaxLines: 3,
                      contentPadding: const EdgeInsets.all(10),
                      filled: true,
                      fillColor: c.inputBg,
                      hoverColor: Colors.transparent,
                      border: OutlineInputBorder(
                        borderRadius: m.brInput,
                        borderSide: BorderSide(color: c.inputBorder),
                      ),
                      enabledBorder: OutlineInputBorder(
                        borderRadius: m.brInput,
                        borderSide: BorderSide(color: c.inputBorder),
                      ),
                      focusedBorder: OutlineInputBorder(
                        borderRadius: m.brInput,
                        borderSide: BorderSide(color: c.inputFocusBorder),
                      ),
                    ),
                    onChanged: (_) => setState(() {}),
                  ),
                ),
              ),
            ),
          ),
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerRight,
            child: ShadButton(
              size: ShadButtonSize.sm,
              onPressed: () {
                _save();
                setState(() => _isExpanded = false);
              },
              child: Text(s.confirm),
            ),
          ),
        ],
      ],
    );
  }
}

/// ED2K 服务器订阅编辑器：启用开关 + 订阅状态 + 立即更新 + 订阅地址管理。
class _Ed2kServerSubEditor extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const _Ed2kServerSubEditor({required this.settingsProvider});

  @override
  State<_Ed2kServerSubEditor> createState() => _Ed2kServerSubEditorState();
}

class _Ed2kServerSubEditorState extends State<_Ed2kServerSubEditor> {
  late TextEditingController _controller;
  bool _isExpanded = false;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController(
      text: widget.settingsProvider.ed2kServerSubUrls,
    );
  }

  @override
  void didUpdateWidget(_Ed2kServerSubEditor oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.settingsProvider.ed2kServerSubUrls != _controller.text &&
        !_isExpanded) {
      _controller.text = widget.settingsProvider.ed2kServerSubUrls;
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _save() {
    final seen = <String>{};
    final lines = <String>[];
    for (final raw in _controller.text.split('\n')) {
      final l = raw.trim();
      if (l.isEmpty) continue;
      final key = l.toLowerCase().replaceAll(RegExp(r'/+$'), '');
      if (seen.add(key)) lines.add(l);
    }
    final cleaned = lines.join('\n');
    _controller.text = cleaned;
    widget.settingsProvider.setEd2kServerSubUrls(cleaned);
  }

  void _resetToDefault() {
    showShadDialog(
      context: context,
      barrierColor: AppColors.of(context).dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog.alert(
        title: Text(LocaleScope.of(ctx).ed2kResetServers),
        description: Text(LocaleScope.of(ctx).ed2kServerSubResetConfirm),
        actions: [
          ShadButton.outline(
            child: Text(LocaleScope.of(ctx).cancel),
            onPressed: () => Navigator.of(ctx).pop(),
          ),
          ShadButton(
            child: Text(LocaleScope.of(ctx).confirm),
            onPressed: () {
              Navigator.of(ctx).pop();
              final defaults = _kDefaultEd2kMetUrls.join('\n');
              _controller.text = defaults;
              widget.settingsProvider.setEd2kServerSubUrls(defaults);
              setState(() {});
            },
          ),
        ],
      ),
    );
  }

  String _formatUpdatedAt(int unixSecs) {
    final dt = DateTime.fromMillisecondsSinceEpoch(unixSecs * 1000);
    String two(int v) => v.toString().padLeft(2, '0');
    return '${dt.year}-${two(dt.month)}-${two(dt.day)} '
        '${two(dt.hour)}:${two(dt.minute)}';
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final sp = widget.settingsProvider;

    final String statusText;
    if (sp.ed2kServerSubRefreshing) {
      statusText = s.ed2kServerSubUpdating;
    } else if (sp.ed2kServerSubUpdatedAt > 0) {
      statusText =
          '${s.ed2kServerSubStatus(sp.ed2kServerSubCount)} · '
          '${s.ed2kServerSubUpdatedAt(_formatUpdatedAt(sp.ed2kServerSubUpdatedAt))}';
    } else {
      statusText = s.ed2kServerSubNeverUpdated;
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                statusText,
                style: TextStyle(fontSize: 12, color: c.textMuted),
              ),
            ),
            ShadSwitch(
              value: sp.ed2kServerSubEnabled,
              onChanged: (v) => sp.setEd2kServerSubEnabled(v),
            ),
          ],
        ),
        if (sp.ed2kServerSubEnabled) ...[
          if (sp.ed2kServerSubLastError.isNotEmpty) ...[
            const SizedBox(height: 4),
            Text(
              '${s.ed2kServerSubUpdateFailed}: ${sp.ed2kServerSubLastError}',
              style: TextStyle(fontSize: 11, color: c.statusError),
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
            ),
          ],
          const SizedBox(height: 4),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              ShadButton.ghost(
                size: ShadButtonSize.sm,
                onPressed: sp.ed2kServerSubRefreshing
                    ? null
                    : () => sp.refreshEd2kServerSubscription(),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    if (sp.ed2kServerSubRefreshing)
                      SizedBox(
                        width: 12,
                        height: 12,
                        child: CircularProgressIndicator(
                          strokeWidth: 2,
                          color: c.textSecondary,
                        ),
                      )
                    else
                      Icon(
                        LucideIcons.refreshCw,
                        size: 12,
                        color: c.textSecondary,
                      ),
                    const SizedBox(width: 4),
                    Text(
                      sp.ed2kServerSubRefreshing
                          ? s.ed2kServerSubUpdating
                          : s.ed2kServerSubUpdateNow,
                      style: TextStyle(fontSize: 11, color: c.textSecondary),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 4),
              ShadButton.ghost(
                size: ShadButtonSize.sm,
                onPressed: () => setState(() => _isExpanded = !_isExpanded),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(
                      _isExpanded
                          ? LucideIcons.chevronUp
                          : LucideIcons.chevronDown,
                      size: 14,
                      color: c.textSecondary,
                    ),
                    const SizedBox(width: 4),
                    Text(
                      _isExpanded ? s.cancel : s.manage,
                      style: TextStyle(fontSize: 11, color: c.textSecondary),
                    ),
                  ],
                ),
              ),
            ],
          ),
          if (_isExpanded) ...[
            const SizedBox(height: 8),
            SizedBox(
              height: 100,
              child: Localizations(
                locale: const Locale('en'),
                delegates: const [
                  DefaultWidgetsLocalizations.delegate,
                  DefaultMaterialLocalizations.delegate,
                ],
                child: Material(
                  type: MaterialType.transparency,
                  child: TextSelectionTheme(
                    data: TextSelectionThemeData(
                      selectionColor: m.textSelection(c.accent),
                      cursorColor: c.accent,
                      selectionHandleColor: c.accent,
                    ),
                    child: TextField(
                      controller: _controller,
                      maxLines: null,
                      expands: true,
                      textAlignVertical: TextAlignVertical.top,
                      cursorColor: c.accent,
                      style: TextStyle(
                        fontSize: 12,
                        color: c.textPrimary,
                        fontFamily: 'monospace',
                        height: 1.5,
                      ),
                      decoration: InputDecoration(
                        hintText: s.ed2kServerSubPlaceholder,
                        hintStyle: TextStyle(fontSize: 12, color: c.textMuted),
                        hintMaxLines: 3,
                        contentPadding: const EdgeInsets.all(10),
                        filled: true,
                        fillColor: c.inputBg,
                        hoverColor: Colors.transparent,
                        border: OutlineInputBorder(
                          borderRadius: m.brInput,
                          borderSide: BorderSide(color: c.inputBorder),
                        ),
                        enabledBorder: OutlineInputBorder(
                          borderRadius: m.brInput,
                          borderSide: BorderSide(color: c.inputBorder),
                        ),
                        focusedBorder: OutlineInputBorder(
                          borderRadius: m.brInput,
                          borderSide: BorderSide(color: c.inputFocusBorder),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                ShadButton.ghost(
                  size: ShadButtonSize.sm,
                  onPressed: _resetToDefault,
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(
                        LucideIcons.rotateCcw,
                        size: 12,
                        color: c.textSecondary,
                      ),
                      const SizedBox(width: 4),
                      Text(
                        s.ed2kResetServers,
                        style: TextStyle(fontSize: 11, color: c.textSecondary),
                      ),
                    ],
                  ),
                ),
                const Spacer(),
                ShadButton(
                  size: ShadButtonSize.sm,
                  onPressed: () {
                    _save();
                    setState(() => _isExpanded = false);
                  },
                  child: Text(s.confirm),
                ),
              ],
            ),
          ],
        ],
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 语言选择器（跟随系统 / 中文 / English）
// ─────────────────────────────────────────────

class _LanguageSelector extends StatelessWidget {
  const _LanguageSelector();

  @override
  Widget build(BuildContext context) {
    final current = localeNotifier.preference;
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    final options = [
      (pref: kLocaleSystem, label: s.languageSystem, icon: LucideIcons.monitor),
      (pref: kLocaleZh, label: s.languageChinese, icon: LucideIcons.languages),
      (pref: kLocaleEn, label: s.languageEnglish, icon: LucideIcons.languages),
    ];

    return Wrap(
      spacing: 8,
      runSpacing: 8,
      children: [
        for (final item in options)
          _ThemeModeCard(
            icon: item.icon,
            label: item.label,
            selected: current == item.pref,
            colors: c,
            onTap: () => localeNotifier.setLocale(item.pref),
          ),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 主题选择器（内置主题卡片 + 导入导出）
// ─────────────────────────────────────────────

class _ThemeSelector extends StatelessWidget {
  const _ThemeSelector();

  @override
  Widget build(BuildContext context) {
    final provider = FluxDownApp.of(context);
    // ListenableBuilder 监听 ThemeProvider，确保导入/删除主题后立即 rebuild
    return ListenableBuilder(
      listenable: provider,
      builder: (context, _) {
        final c = AppColors.of(context);
        final s = LocaleScope.of(context);
        final dark = provider.isDark(context);

        final darkThemes = builtinThemes
            .where((e) => e.appearance == Brightness.dark)
            .toList();
        final lightThemes = builtinThemes
            .where((e) => e.appearance == Brightness.light)
            .toList();

        final appearance = dark ? Brightness.dark : Brightness.light;
        final importedForMode = provider.importedThemesFor(appearance);
        final selectedCustomId = dark
            ? provider.selectedCustomDarkId
            : provider.selectedCustomLightId;
        final isCustomActive = dark
            ? provider.isCustomDarkActive
            : provider.isCustomLightActive;

        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _ThemeGroupLabel(
              label: dark ? s.themeDarkTheme : s.themeLightTheme,
              colors: c,
            ),
            const SizedBox(height: 6),
            _ThemeCardRow(
              themes: dark ? darkThemes : lightThemes,
              selectedId: dark
                  ? provider.selectedDarkTheme
                  : provider.selectedLightTheme,
              colors: c,
              isCustomActive: isCustomActive,
              onSelect: (id) =>
                  dark ? provider.setDarkTheme(id) : provider.setLightTheme(id),
              importedThemes: importedForMode,
              selectedCustomId: selectedCustomId,
              onSelectImported: (id) => provider.selectImportedTheme(id),
              onRemoveImported: (id) => provider.removeImportedTheme(id),
            ),
            const SizedBox(height: 14),
            _ThemeActions(colors: c),
          ],
        );
      },
    );
  }
}

/// 导入 / 导出操作行
class _ThemeActions extends StatelessWidget {
  final AppColors colors;

  const _ThemeActions({required this.colors});

  Future<void> _importTheme(BuildContext context) async {
    final provider = FluxDownApp.of(context);
    final s = LocaleScope.of(context);

    List<XFile>? result;
    try {
      result = await FilePickerService.pickFiles(
        dialogTitle: s.themeImport,
        allowedExtensions: ['json'],
        allowMultiple: true,
      );
    } on FilePickerException catch (e) {
      if (!context.mounted) return;
      final msg = switch (e.reason) {
        FilePickerFailReason.timeout => s.filePickerErrorTimeout,
        FilePickerFailReason.noDialogTool => s.filePickerErrorNoTool,
        FilePickerFailReason.comInitFailed => s.filePickerErrorNative,
        FilePickerFailReason.nativeDialogFailed => s.filePickerErrorNative,
        FilePickerFailReason.unknown => s.filePickerErrorGeneric,
      };
      ShadSonner.of(context).show(ShadToast.destructive(title: Text(msg)));
      return;
    }
    if (result == null || result.isEmpty) return;

    int successCount = 0;
    final errors = <String>[];

    for (final picked in result) {
      final path = picked.path;

      try {
        final file = File(path);
        final content = await file.readAsString();
        final tokens = provider.importThemeJson(content);
        provider.addImportedTheme(tokens);
        successCount++;
      } catch (e) {
        errors.add('${picked.name}: $e');
      }
    }

    if (!context.mounted) return;

    if (successCount > 0) {
      ShadSonner.of(context).show(
        ShadToast(
          title: Text('${s.themeImportSuccess} ($successCount)'),
          duration: const Duration(seconds: 2),
        ),
      );
    }
    if (errors.isNotEmpty) {
      ShadSonner.of(context).show(
        ShadToast.destructive(
          title: Text(s.themeImportError),
          description: Text(errors.join('\n')),
          duration: const Duration(seconds: 3),
        ),
      );
    }
  }

  Future<void> _exportTheme(BuildContext context) async {
    final provider = FluxDownApp.of(context);
    final s = LocaleScope.of(context);
    final dark = provider.isDark(context);
    final tokens = provider.getExportableTokens(dark);
    final json = provider.exportThemeJson(tokens);

    final safeName = tokens.name
        .replaceAll(RegExp(r'[^\w\-]'), '_')
        .toLowerCase();
    String? result;
    try {
      result = await FilePickerService.saveFile(
        dialogTitle: s.themeExport,
        fileName: '$safeName.json',
        allowedExtensions: ['json'],
      );
    } on FilePickerException catch (e) {
      if (!context.mounted) return;
      final msg = switch (e.reason) {
        FilePickerFailReason.timeout => s.filePickerErrorTimeout,
        FilePickerFailReason.noDialogTool => s.filePickerErrorNoTool,
        FilePickerFailReason.comInitFailed => s.filePickerErrorNative,
        FilePickerFailReason.nativeDialogFailed => s.filePickerErrorNative,
        FilePickerFailReason.unknown => s.filePickerErrorGeneric,
      };
      ShadSonner.of(context).show(ShadToast.destructive(title: Text(msg)));
      return;
    }
    if (result == null) return;

    try {
      await File(result).writeAsString(json);
      if (!context.mounted) return;
      ShadSonner.of(context).show(
        ShadToast(
          title: Text(s.themeExportSuccess),
          duration: const Duration(seconds: 2),
        ),
      );
    } catch (e) {
      if (!context.mounted) return;
      ShadSonner.of(context).show(
        ShadToast.destructive(
          title: Text(s.themeImportError),
          description: Text(e.toString()),
          duration: const Duration(seconds: 3),
        ),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = colors;
    return Row(
      children: [
        _SmallActionButton(
          icon: LucideIcons.fileUp,
          label: s.themeImport,
          colors: c,
          onTap: () => _importTheme(context),
        ),
        const SizedBox(width: 8),
        _SmallActionButton(
          icon: LucideIcons.fileDown,
          label: s.themeExport,
          colors: c,
          onTap: () => _exportTheme(context),
        ),
      ],
    );
  }
}

class _SmallActionButton extends StatefulWidget {
  final IconData icon;
  final String label;
  final AppColors colors;
  final VoidCallback onTap;

  const _SmallActionButton({
    required this.icon,
    required this.label,
    required this.colors,
    required this.onTap,
  });

  @override
  State<_SmallActionButton> createState() => _SmallActionButtonState();
}

class _SmallActionButtonState extends State<_SmallActionButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.colors;
    final m = AppMetrics.of(context);
    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 150),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
          decoration: BoxDecoration(
            color: _isHovered ? c.hoverBg : Colors.transparent,
            borderRadius: m.brMd,
            border: Border.all(color: c.border, width: 1),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(widget.icon, size: 12, color: c.textSecondary),
              const SizedBox(width: 5),
              Text(
                widget.label,
                style: TextStyle(fontSize: 11, color: c.textSecondary),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _ThemeGroupLabel extends StatelessWidget {
  final String label;
  final AppColors colors;

  const _ThemeGroupLabel({required this.label, required this.colors});

  @override
  Widget build(BuildContext context) {
    return Text(
      label,
      style: TextStyle(
        fontSize: 11,
        fontWeight: FontWeight.w500,
        color: colors.textMuted,
      ),
    );
  }
}

class _ThemeCardRow extends StatelessWidget {
  final List<BuiltinThemeEntry> themes;
  final BuiltinThemeId selectedId;
  final AppColors colors;
  final bool isCustomActive;
  final ValueChanged<BuiltinThemeId> onSelect;
  final List<ImportedThemeEntry> importedThemes;
  final String? selectedCustomId;
  final ValueChanged<String> onSelectImported;
  final ValueChanged<String> onRemoveImported;

  const _ThemeCardRow({
    required this.themes,
    required this.selectedId,
    required this.colors,
    required this.isCustomActive,
    required this.onSelect,
    required this.importedThemes,
    required this.selectedCustomId,
    required this.onSelectImported,
    required this.onRemoveImported,
  });

  @override
  Widget build(BuildContext context) {
    return Wrap(
      spacing: 8,
      runSpacing: 8,
      children: [
        for (final entry in themes)
          _ThemePreviewCard(
            entry: entry,
            selected: selectedId == entry.id && !isCustomActive,
            colors: colors,
            onTap: () => onSelect(entry.id),
          ),
        for (final imported in importedThemes)
          _CustomThemeCard(
            tokens: imported.tokens,
            selected: selectedCustomId == imported.id,
            colors: colors,
            onTap: () => onSelectImported(imported.id),
            onClear: () => onRemoveImported(imported.id),
          ),
      ],
    );
  }
}

/// 自定义主题卡片（导入的自定义主题）
class _CustomThemeCard extends StatefulWidget {
  final FluxThemeTokens tokens;
  final bool selected;
  final AppColors colors;
  final VoidCallback onTap;
  final VoidCallback? onClear;

  const _CustomThemeCard({
    required this.tokens,
    required this.selected,
    required this.colors,
    required this.onTap,
    this.onClear,
  });

  @override
  State<_CustomThemeCard> createState() => _CustomThemeCardState();
}

class _CustomThemeCardState extends State<_CustomThemeCard> {
  bool _isHovered = false;
  bool _deleteRequested = false;

  void _handleTap() {
    if (_deleteRequested) {
      _deleteRequested = false;
      return;
    }
    widget.onTap();
  }

  void _handleDelete() {
    _deleteRequested = true;
    widget.onClear?.call();
  }

  @override
  Widget build(BuildContext context) {
    final theme = ShadTheme.of(context);
    final c = widget.colors;
    final m = AppMetrics.of(context);
    final tokens = widget.tokens;
    // 迷你预览内部用「被预览主题自身」的 metric（圆角/透明度随被预览主题变化），
    // 而非当前生效 App 的 m（那是外层卡片 chrome 用的）。
    final tm = AppMetrics.fromTokens(tokens);
    final selected = widget.selected;
    final borderColor = selected ? theme.colorScheme.primary : c.border;

    return ShadTooltip(
      builder: (_) => Text(tokens.name),
      child: MouseRegion(
        onEnter: (_) => setState(() => _isHovered = true),
        onExit: (_) => setState(() => _isHovered = false),
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: _handleTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 150),
            width: 120,
            padding: const EdgeInsets.all(8),
            decoration: BoxDecoration(
              color: _isHovered && !selected ? c.hoverBg : c.bg,
              borderRadius: m.brDialog,
              border: Border.all(color: borderColor, width: selected ? 1.5 : 1),
              boxShadow: selected
                  ? [
                      BoxShadow(
                        color: theme.colorScheme.primary.withValues(
                          alpha: 0.15,
                        ),
                        blurRadius: 8,
                      ),
                    ]
                  : null,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                // ── 迷你预览（与内置主题卡片相同逻辑）──
                Container(
                  height: 52,
                  decoration: BoxDecoration(
                    color: tokens.background,
                    borderRadius: tm.brMd,
                    border: Border.all(
                      color: tm.borderFade(tokens.border),
                    ),
                  ),
                  child: Row(
                    children: [
                      Container(
                        width: 28,
                        decoration: BoxDecoration(
                          color: tokens.surface1,
                          borderRadius: const BorderRadius.only(
                            topLeft: Radius.circular(5.5),
                            bottomLeft: Radius.circular(5.5),
                          ),
                        ),
                        child: Column(
                          mainAxisAlignment: MainAxisAlignment.center,
                          children: [
                            Container(
                              width: 16,
                              height: 3,
                              margin: const EdgeInsets.only(bottom: 3),
                              decoration: BoxDecoration(
                                color: tokens.accent,
                                borderRadius: tm.brProgress,
                              ),
                            ),
                            Container(
                              width: 16,
                              height: 3,
                              margin: const EdgeInsets.only(bottom: 3),
                              decoration: BoxDecoration(
                                color: tm.borderSubtle(tokens.textMuted),
                                borderRadius: tm.brProgress,
                              ),
                            ),
                            Container(
                              width: 16,
                              height: 3,
                              decoration: BoxDecoration(
                                color: tm.borderSubtle(tokens.textMuted),
                                borderRadius: tm.brProgress,
                              ),
                            ),
                          ],
                        ),
                      ),
                      Expanded(
                        child: Padding(
                          padding: const EdgeInsets.all(4),
                          child: Column(
                            mainAxisAlignment: MainAxisAlignment.center,
                            children: [
                              Container(
                                height: 3,
                                margin: const EdgeInsets.only(bottom: 3),
                                decoration: BoxDecoration(
                                  color: tm.borderFaint(tokens.textPrimary),
                                  borderRadius: tm.brProgress,
                                ),
                              ),
                              Container(
                                height: 3,
                                margin: const EdgeInsets.only(bottom: 3),
                                decoration: BoxDecoration(
                                  // 刻意保留：迷你预览次级文本条示意，固定装饰透明度。
                                  color: tokens.textMuted.withValues(
                                    alpha: 0.2,
                                  ),
                                  borderRadius: tm.brProgress,
                                ),
                              ),
                              Container(
                                height: 4,
                                decoration: BoxDecoration(
                                  color: tokens.surface3,
                                  borderRadius: tm.brXs,
                                ),
                                child: Align(
                                  alignment: Alignment.centerLeft,
                                  child: FractionallySizedBox(
                                    widthFactor: 0.6,
                                    child: Container(
                                      decoration: BoxDecoration(
                                        color: tokens.accent,
                                        borderRadius: tm.brXs,
                                      ),
                                    ),
                                  ),
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: 6),
                // ── 名称 + 删除/勾选 ──
                Row(
                  children: [
                    Expanded(
                      child: Text(
                        tokens.name,
                        style: TextStyle(
                          fontSize: 11,
                          fontWeight: selected
                              ? FontWeight.w600
                              : FontWeight.w400,
                          color: selected
                              ? theme.colorScheme.primary
                              : c.textSecondary,
                        ),
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                    if (selected)
                      Icon(
                        LucideIcons.check,
                        size: 12,
                        color: theme.colorScheme.primary,
                      ),
                    if (_isHovered && widget.onClear != null)
                      Padding(
                        padding: EdgeInsets.only(left: selected ? 4 : 0),
                        child: MouseRegion(
                          cursor: SystemMouseCursors.click,
                          child: Listener(
                            behavior: HitTestBehavior.opaque,
                            onPointerDown: (_) => _handleDelete(),
                            child: Padding(
                              padding: const EdgeInsets.all(2),
                              child: Icon(
                                LucideIcons.x,
                                size: 12,
                                color: c.textMuted,
                              ),
                            ),
                          ),
                        ),
                      ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _ThemePreviewCard extends StatefulWidget {
  final BuiltinThemeEntry entry;
  final bool selected;
  final AppColors colors;
  final VoidCallback onTap;

  const _ThemePreviewCard({
    required this.entry,
    required this.selected,
    required this.colors,
    required this.onTap,
  });

  @override
  State<_ThemePreviewCard> createState() => _ThemePreviewCardState();
}

class _ThemePreviewCardState extends State<_ThemePreviewCard> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final theme = ShadTheme.of(context);
    final c = widget.colors;
    final entry = widget.entry;
    final selected = widget.selected;

    // 预览主题的色值
    final tokens = entry.build();
    // 迷你预览内部用被预览主题自身的 metric（见 _CustomThemeCard 注释）。
    final tm = AppMetrics.fromTokens(tokens);
    final borderColor = selected ? theme.colorScheme.primary : c.border;

    return ShadTooltip(
      builder: (_) => Text(entry.id.label),
      child: MouseRegion(
        onEnter: (_) => setState(() => _isHovered = true),
        onExit: (_) => setState(() => _isHovered = false),
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: widget.onTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 150),
            width: 120,
            padding: const EdgeInsets.all(8),
            decoration: BoxDecoration(
              color: _isHovered && !selected ? c.hoverBg : c.bg,
              borderRadius: m.brDialog,
              border: Border.all(color: borderColor, width: selected ? 1.5 : 1),
              boxShadow: selected
                  ? [
                      BoxShadow(
                        color: theme.colorScheme.primary.withValues(
                          alpha: 0.15,
                        ),
                        blurRadius: 8,
                      ),
                    ]
                  : null,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                // ── 迷你预览 ──
                Container(
                  height: 52,
                  decoration: BoxDecoration(
                    color: tokens.background,
                    borderRadius: tm.brMd,
                    border: Border.all(
                      color: tm.borderFade(tokens.border),
                    ),
                  ),
                  child: Row(
                    children: [
                      // 侧边栏预览
                      Container(
                        width: 28,
                        decoration: BoxDecoration(
                          color: tokens.surface1,
                          borderRadius: const BorderRadius.only(
                            topLeft: Radius.circular(5.5),
                            bottomLeft: Radius.circular(5.5),
                          ),
                        ),
                        child: Column(
                          mainAxisAlignment: MainAxisAlignment.center,
                          children: [
                            Container(
                              width: 16,
                              height: 3,
                              margin: const EdgeInsets.only(bottom: 3),
                              decoration: BoxDecoration(
                                color: tokens.accent,
                                borderRadius: tm.brProgress,
                              ),
                            ),
                            Container(
                              width: 16,
                              height: 3,
                              margin: const EdgeInsets.only(bottom: 3),
                              decoration: BoxDecoration(
                                color: tm.borderSubtle(tokens.textMuted),
                                borderRadius: tm.brProgress,
                              ),
                            ),
                            Container(
                              width: 16,
                              height: 3,
                              decoration: BoxDecoration(
                                color: tm.borderSubtle(tokens.textMuted),
                                borderRadius: tm.brProgress,
                              ),
                            ),
                          ],
                        ),
                      ),
                      // 内容区预览
                      Expanded(
                        child: Padding(
                          padding: const EdgeInsets.all(4),
                          child: Column(
                            mainAxisAlignment: MainAxisAlignment.center,
                            children: [
                              Container(
                                height: 3,
                                margin: const EdgeInsets.only(bottom: 3),
                                decoration: BoxDecoration(
                                  color: tm.borderFaint(tokens.textPrimary),
                                  borderRadius: tm.brProgress,
                                ),
                              ),
                              Container(
                                height: 3,
                                margin: const EdgeInsets.only(bottom: 3),
                                decoration: BoxDecoration(
                                  // 刻意保留：迷你预览次级文本条示意，固定装饰透明度。
                                  color: tokens.textMuted.withValues(
                                    alpha: 0.2,
                                  ),
                                  borderRadius: tm.brProgress,
                                ),
                              ),
                              // 进度条预览
                              Container(
                                height: 4,
                                decoration: BoxDecoration(
                                  color: tokens.surface3,
                                  borderRadius: tm.brXs,
                                ),
                                child: Align(
                                  alignment: Alignment.centerLeft,
                                  child: FractionallySizedBox(
                                    widthFactor: 0.6,
                                    child: Container(
                                      decoration: BoxDecoration(
                                        color: tokens.accent,
                                        borderRadius: tm.brXs,
                                      ),
                                    ),
                                  ),
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: 6),
                // ── 主题名 + 选中勾 ──
                Row(
                  children: [
                    Expanded(
                      child: Text(
                        entry.id.label,
                        style: TextStyle(
                          fontSize: 11,
                          fontWeight: selected
                              ? FontWeight.w600
                              : FontWeight.w400,
                          color: selected
                              ? theme.colorScheme.primary
                              : c.textSecondary,
                        ),
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                    if (selected)
                      Icon(
                        LucideIcons.check,
                        size: 12,
                        color: theme.colorScheme.primary,
                      ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 主题模式选择器（亮色 / 暗色 / 跟随系统）
// ─────────────────────────────────────────────

class _ThemeModeSelector extends StatelessWidget {
  const _ThemeModeSelector();

  @override
  Widget build(BuildContext context) {
    final provider = FluxDownApp.of(context);
    final current = provider.themeMode;
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    final modes = [
      (
        mode: ThemeMode.system,
        label: s.themeModeSystem,
        icon: LucideIcons.monitor,
      ),
      (mode: ThemeMode.light, label: s.themeModeLight, icon: LucideIcons.sun),
      (mode: ThemeMode.dark, label: s.themeModeDark, icon: LucideIcons.moon),
    ];

    return Wrap(
      spacing: 8,
      runSpacing: 8,
      children: [
        for (final item in modes)
          _ThemeModeCard(
            icon: item.icon,
            label: item.label,
            selected: current == item.mode,
            colors: c,
            onTap: () => provider.setThemeMode(item.mode),
          ),
      ],
    );
  }
}

class _ThemeModeCard extends StatefulWidget {
  final IconData icon;
  final String label;
  final bool selected;
  final AppColors colors;
  final VoidCallback onTap;

  const _ThemeModeCard({
    required this.icon,
    required this.label,
    required this.selected,
    required this.colors,
    required this.onTap,
  });

  @override
  State<_ThemeModeCard> createState() => _ThemeModeCardState();
}

class _ThemeModeCardState extends State<_ThemeModeCard> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final theme = ShadTheme.of(context);
    final c = widget.colors;
    final selected = widget.selected;
    final borderColor = selected ? theme.colorScheme.primary : c.border;
    final bgColor = selected
        ? m.subtle(theme.colorScheme.primary)
        : _isHovered
        ? c.hoverBg
        : c.bg;

    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 150),
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
          decoration: BoxDecoration(
            color: bgColor,
            borderRadius: m.brCard,
            border: Border.all(color: borderColor, width: selected ? 1.5 : 1),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                widget.icon,
                size: 14,
                color: selected ? theme.colorScheme.primary : c.textSecondary,
              ),
              const SizedBox(width: 6),
              Text(
                widget.label,
                style: TextStyle(
                  fontSize: 12,
                  fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                  color: selected ? theme.colorScheme.primary : c.textSecondary,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 主题色选择器（4 预设 + 自定义色盘）
// ─────────────────────────────────────────────

/// 预设色列表（排除 custom）
const _presetSchemes = [
  AppColorScheme.blue,
  AppColorScheme.green,
  AppColorScheme.violet,
  AppColorScheme.rose,
];

class _ColorSchemeSelector extends StatelessWidget {
  const _ColorSchemeSelector();

  @override
  Widget build(BuildContext context) {
    final provider = FluxDownApp.of(context);
    final current = provider.colorScheme;
    final c = AppColors.of(context);
    final isCustom = current == AppColorScheme.custom;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // ── 色点行：4 预设 + 1 自定义 ──
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            for (final scheme in _presetSchemes)
              _ColorDot(
                color: scheme.previewColor,
                label: scheme.label,
                selected: current == scheme,
                colors: c,
                onTap: () => provider.setColorScheme(scheme),
              ),
            _ColorDot(
              color: provider.customColor,
              label: AppColorScheme.custom.label,
              selected: isCustom,
              colors: c,
              icon: isCustom ? LucideIcons.check : LucideIcons.palette,
              onTap: () => provider.setColorScheme(AppColorScheme.custom),
            ),
          ],
        ),
        // ── 自定义色盘（仅在选中 custom 时展开） ──
        AnimatedSize(
          duration: const Duration(milliseconds: 200),
          curve: Curves.easeInOut,
          alignment: Alignment.topCenter,
          child: isCustom
              ? Padding(
                  padding: const EdgeInsets.only(top: 14),
                  child: _CustomColorPicker(
                    color: provider.customColor,
                    onChanged: provider.setCustomColor,
                  ),
                )
              : const SizedBox.shrink(),
        ),
      ],
    );
  }
}

class _ColorDot extends StatefulWidget {
  final Color color;
  final String label;
  final bool selected;
  final AppColors colors;
  final IconData? icon;
  final VoidCallback onTap;

  const _ColorDot({
    required this.color,
    required this.label,
    required this.selected,
    required this.colors,
    this.icon,
    required this.onTap,
  });

  @override
  State<_ColorDot> createState() => _ColorDotState();
}

class _ColorDotState extends State<_ColorDot> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final selected = widget.selected;
    final showIcon = selected || widget.icon != null;
    return ShadTooltip(
      builder: (_) => Text(widget.label),
      child: MouseRegion(
        onEnter: (_) => setState(() => _isHovered = true),
        onExit: (_) => setState(() => _isHovered = false),
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: widget.onTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 150),
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: widget.color,
              shape: BoxShape.circle,
              border: Border.all(
                color: selected
                    ? widget.colors.textPrimary
                    : _isHovered
                    ? m.borderMedium(widget.colors.textSecondary)
                    : widget.color,
                width: selected
                    ? 2.5
                    : _isHovered
                    ? 1.5
                    : 0,
              ),
              boxShadow: _isHovered || selected
                  ? [
                      BoxShadow(
                        color: m.shadowStrong(widget.color),
                        blurRadius: 6,
                        spreadRadius: 0,
                      ),
                    ]
                  : null,
            ),
            child: showIcon
                ? Icon(
                    selected
                        ? LucideIcons.check
                        : (widget.icon ?? LucideIcons.check),
                    size: selected ? 13 : 12,
                    color: Colors.white,
                  )
                : null,
          ),
        ),
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 自定义色盘（色相滑块 + Hex 输入）
// ─────────────────────────────────────────────

class _CustomColorPicker extends StatefulWidget {
  final Color color;
  final ValueChanged<Color> onChanged;

  const _CustomColorPicker({required this.color, required this.onChanged});

  @override
  State<_CustomColorPicker> createState() => _CustomColorPickerState();
}

class _CustomColorPickerState extends State<_CustomColorPicker> {
  late TextEditingController _hexController;
  late double _hue;
  late double _saturation;
  late double _lightness;

  @override
  void initState() {
    super.initState();
    final hsl = HSLColor.fromColor(widget.color);
    _hue = hsl.hue;
    _saturation = hsl.saturation;
    _lightness = hsl.lightness;
    _hexController = TextEditingController(text: _colorToHex(widget.color));
  }

  @override
  void didUpdateWidget(_CustomColorPicker oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.color != widget.color) {
      final hsl = HSLColor.fromColor(widget.color);
      _hue = hsl.hue;
      _saturation = hsl.saturation;
      _lightness = hsl.lightness;
      final hex = _colorToHex(widget.color);
      if (_hexController.text != hex) {
        _hexController.text = hex;
      }
    }
  }

  @override
  void dispose() {
    _hexController.dispose();
    super.dispose();
  }

  static String _colorToHex(Color c) {
    final r = (c.r * 255).round();
    final g = (c.g * 255).round();
    final b = (c.b * 255).round();
    return '${r.toRadixString(16).padLeft(2, '0')}'
            '${g.toRadixString(16).padLeft(2, '0')}'
            '${b.toRadixString(16).padLeft(2, '0')}'
        .toUpperCase();
  }

  void _onHueChanged(double hue) {
    setState(() => _hue = hue);
    final color = HSLColor.fromAHSL(1, hue, _saturation, _lightness).toColor();
    _hexController.text = _colorToHex(color);
    widget.onChanged(color);
  }

  void _onSaturationChanged(double sat) {
    setState(() => _saturation = sat);
    final color = HSLColor.fromAHSL(1, _hue, sat, _lightness).toColor();
    _hexController.text = _colorToHex(color);
    widget.onChanged(color);
  }

  void _onLightnessChanged(double lit) {
    setState(() => _lightness = lit);
    final color = HSLColor.fromAHSL(1, _hue, _saturation, lit).toColor();
    _hexController.text = _colorToHex(color);
    widget.onChanged(color);
  }

  void _onHexSubmitted(String value) {
    final hex = value.replaceAll('#', '').trim();
    if (hex.length != 6) return;
    final parsed = int.tryParse(hex, radix: 16);
    if (parsed == null) return;
    final color = Color(0xFF000000 | parsed);
    final hsl = HSLColor.fromColor(color);
    setState(() {
      _hue = hsl.hue;
      _saturation = hsl.saturation;
      _lightness = hsl.lightness;
    });
    widget.onChanged(color);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final currentColor = HSLColor.fromAHSL(
      1,
      _hue,
      _saturation,
      _lightness,
    ).toColor();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // 色相滑块
        _HueSlider(hue: _hue, onChanged: _onHueChanged),
        const SizedBox(height: 10),
        // 饱和度滑块
        _GradientSlider(
          value: _saturation,
          onChanged: _onSaturationChanged,
          leftColor: HSLColor.fromAHSL(1, _hue, 0, _lightness).toColor(),
          rightColor: HSLColor.fromAHSL(1, _hue, 1, _lightness).toColor(),
        ),
        const SizedBox(height: 10),
        // 明度滑块
        _GradientSlider(
          value: _lightness,
          onChanged: _onLightnessChanged,
          leftColor: HSLColor.fromAHSL(1, _hue, _saturation, 0.05).toColor(),
          rightColor: HSLColor.fromAHSL(1, _hue, _saturation, 0.95).toColor(),
        ),
        const SizedBox(height: 12),
        // Hex 输入 + 预览
        Row(
          children: [
            // 颜色预览圆点
            Container(
              width: 20,
              height: 20,
              decoration: BoxDecoration(
                color: currentColor,
                shape: BoxShape.circle,
                border: Border.all(color: c.border, width: 1),
              ),
            ),
            const SizedBox(width: 8),
            Text(
              '#',
              style: TextStyle(
                fontSize: 13,
                fontWeight: FontWeight.w500,
                color: c.textSecondary,
              ),
            ),
            const SizedBox(width: 4),
            SizedBox(
              width: 90,
              child: ShadInput(
                controller: _hexController,
                placeholder: const Text('3B82F6'),
                onSubmitted: _onHexSubmitted,
              ),
            ),
          ],
        ),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 色相滑块（彩虹渐变条）
// ─────────────────────────────────────────────

class _HueSlider extends StatelessWidget {
  final double hue; // 0..360
  final ValueChanged<double> onChanged;

  const _HueSlider({required this.hue, required this.onChanged});

  void _handleInteraction(Offset localPosition, double width) {
    final fraction = (localPosition.dx / width).clamp(0.0, 1.0);
    onChanged(fraction * 360);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        final thumbX = (hue / 360) * width;
        return GestureDetector(
          onTapDown: (d) => _handleInteraction(d.localPosition, width),
          onPanUpdate: (d) => _handleInteraction(d.localPosition, width),
          child: SizedBox(
            height: 22,
            child: Stack(
              alignment: Alignment.centerLeft,
              children: [
                // 彩虹渐变条
                Container(
                  height: 14,
                  decoration: BoxDecoration(
                    borderRadius: m.brMd,
                    gradient: const LinearGradient(
                      colors: [
                        Color(0xFFFF0000), // 0°   Red
                        Color(0xFFFFFF00), // 60°  Yellow
                        Color(0xFF00FF00), // 120° Green
                        Color(0xFF00FFFF), // 180° Cyan
                        Color(0xFF0000FF), // 240° Blue
                        Color(0xFFFF00FF), // 300° Magenta
                        Color(0xFFFF0000), // 360° Red
                      ],
                    ),
                    border: Border.all(
                      color: m.borderSubtle(c.border),
                      width: 0.5,
                    ),
                  ),
                ),
                // 拖动指示器
                Positioned(
                  left: (thumbX - 8).clamp(0, width - 16),
                  child: Container(
                    width: 16,
                    height: 16,
                    decoration: BoxDecoration(
                      color: HSLColor.fromAHSL(1, hue, 0.8, 0.5).toColor(),
                      shape: BoxShape.circle,
                      border: Border.all(color: Colors.white, width: 2),
                      boxShadow: const [
                        BoxShadow(
                          color: Color(0x40000000),
                          blurRadius: 3,
                          spreadRadius: 0,
                        ),
                      ],
                    ),
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

// ─────────────────────────────────────────────
// 通用双色渐变滑块（饱和度 / 明度）
// ─────────────────────────────────────────────

class _GradientSlider extends StatelessWidget {
  final double value; // 0..1
  final ValueChanged<double> onChanged;
  final Color leftColor;
  final Color rightColor;

  const _GradientSlider({
    required this.value,
    required this.onChanged,
    required this.leftColor,
    required this.rightColor,
  });

  void _handleInteraction(Offset localPosition, double width) {
    final fraction = (localPosition.dx / width).clamp(0.0, 1.0);
    onChanged(fraction);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        final thumbX = value * width;
        final thumbColor = Color.lerp(leftColor, rightColor, value)!;
        return GestureDetector(
          onTapDown: (d) => _handleInteraction(d.localPosition, width),
          onPanUpdate: (d) => _handleInteraction(d.localPosition, width),
          child: SizedBox(
            height: 22,
            child: Stack(
              alignment: Alignment.centerLeft,
              children: [
                Container(
                  height: 14,
                  decoration: BoxDecoration(
                    borderRadius: m.brMd,
                    gradient: LinearGradient(colors: [leftColor, rightColor]),
                    border: Border.all(
                      color: m.borderSubtle(c.border),
                      width: 0.5,
                    ),
                  ),
                ),
                Positioned(
                  left: (thumbX - 8).clamp(0, width - 16),
                  child: Container(
                    width: 16,
                    height: 16,
                    decoration: BoxDecoration(
                      color: thumbColor,
                      shape: BoxShape.circle,
                      border: Border.all(color: Colors.white, width: 2),
                      boxShadow: const [
                        BoxShadow(
                          color: Color(0x40000000),
                          blurRadius: 3,
                          spreadRadius: 0,
                        ),
                      ],
                    ),
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

// ─────────────────────────────────────────────
// 关于页面
// ─────────────────────────────────────────────

class _AboutContent extends StatelessWidget {
  const _AboutContent({super.key, required this.settingsProvider});

  final SettingsProvider settingsProvider;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([UpdateService.instance, settingsProvider]),
      builder: (context, _) {
        final svc = UpdateService.instance;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            // App info card
            _SettingCard(
              label: 'FluxDown',
              description: LocaleScope.of(context).appDescription,
              vertical: true,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _infoRow(
                    c,
                    LocaleScope.of(context).currentVersion,
                    svc.currentVersion == 'dev'
                        ? 'dev'
                        : 'v${svc.currentVersion}',
                  ),
                  if (svc.checkResult != null && svc.checkResult!.hasUpdate)
                    _infoRow(
                      c,
                      LocaleScope.of(context).latestVersion,
                      'v${svc.checkResult!.latestVersion}',
                    ),
                  if (svc.checkResult != null && svc.checkResult!.hasUpdate)
                    _infoRow(
                      c,
                      LocaleScope.of(context).publishDate,
                      _formatDate(svc.checkResult!.publishedAt),
                    ),
                ],
              ),
            ),
            const SizedBox(height: 10),
            // Update card
            _SettingCard(
              label: LocaleScope.of(context).softwareUpdate,
              description: LocaleScope.of(context).checkUpdateDesc,
              vertical: true,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(
                              LocaleScope.of(context).autoCheckUpdate,
                              style: TextStyle(
                                fontSize: 12,
                                color: c.textPrimary,
                                fontWeight: FontWeight.w500,
                              ),
                            ),
                            const SizedBox(height: 2),
                            Text(
                              LocaleScope.of(context).autoCheckUpdateDesc,
                              style: TextStyle(
                                fontSize: 11,
                                color: c.textMuted,
                              ),
                            ),
                          ],
                        ),
                      ),
                      ShadSwitch(
                        value: settingsProvider.autoCheckUpdate,
                        onChanged: (v) =>
                            settingsProvider.setAutoCheckUpdate(v),
                      ),
                    ],
                  ),
                  const SizedBox(height: 12),
                  _buildUpdateSection(context, svc, c),
                ],
              ),
            ),
            const SizedBox(height: 10),
            // Log export card
            _LogExportCard(colors: c, settingsProvider: settingsProvider),
          ],
        );
      },
    );
  }

  Widget _infoRow(AppColors c, String label, String value) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Row(
        children: [
          SizedBox(
            width: 72,
            child: Text(
              label,
              style: TextStyle(fontSize: 12, color: c.textMuted),
            ),
          ),
          Text(
            value,
            style: TextStyle(
              fontSize: 12,
              color: c.textPrimary,
              fontWeight: FontWeight.w500,
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildUpdateSection(
    BuildContext context,
    UpdateService svc,
    AppColors c,
  ) {
    final status = svc.status;
    final s = LocaleScope.of(context);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // Status message
        if (status == UpdateStatus.upToDate)
          _statusRow(c, LucideIcons.circleCheck, AppColors.green, s.upToDate),
        if (status == UpdateStatus.error)
          _statusRow(
            c,
            LucideIcons.circleAlert,
            AppColors.red,
            svc.errorMessage,
          ),
        if (status == UpdateStatus.available)
          _statusRow(
            c,
            LucideIcons.circleArrowDown,
            AppColors.amber,
            s.newVersionFound(svc.checkResult?.latestVersion ?? ''),
          ),
        if (status == UpdateStatus.readyToInstall)
          _statusRow(
            c,
            LucideIcons.circleCheck,
            AppColors.green,
            s.downloadComplete,
          ),

        // Download progress
        if (status == UpdateStatus.downloading) ...[
          _statusRow(c, LucideIcons.download, c.accent, s.downloadingUpdate),
          const SizedBox(height: 10),
          _buildProgressSection(context, svc, c),
        ],

        const SizedBox(height: 14),

        // Action buttons
        Row(
          children: [
            if (status == UpdateStatus.idle ||
                status == UpdateStatus.upToDate ||
                status == UpdateStatus.error)
              ShadButton.outline(
                size: ShadButtonSize.sm,
                enabled: status != UpdateStatus.checking,
                onPressed: svc.checkForUpdate,
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    if (status == UpdateStatus.checking) ...[
                      SizedBox(
                        width: 12,
                        height: 12,
                        child: CircularProgressIndicator(
                          strokeWidth: 1.5,
                          color: c.textSecondary,
                        ),
                      ),
                      const SizedBox(width: 6),
                      Text(s.checking),
                    ] else ...[
                      Icon(
                        LucideIcons.refreshCw,
                        size: 13,
                        color: c.textSecondary,
                      ),
                      const SizedBox(width: 6),
                      Text(s.checkUpdate),
                    ],
                  ],
                ),
              ),
            if (status == UpdateStatus.checking)
              ShadButton.outline(
                size: ShadButtonSize.sm,
                enabled: false,
                onPressed: () {},
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    SizedBox(
                      width: 12,
                      height: 12,
                      child: CircularProgressIndicator(
                        strokeWidth: 1.5,
                        color: c.textSecondary,
                      ),
                    ),
                    const SizedBox(width: 6),
                    Text(s.checking),
                  ],
                ),
              ),
            if (status == UpdateStatus.available) ...[
              ShadButton(
                size: ShadButtonSize.sm,
                onPressed: svc.downloadUpdate,
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const Icon(
                      LucideIcons.download,
                      size: 13,
                      color: Colors.white,
                    ),
                    const SizedBox(width: 6),
                    Text(
                      s.downloadUpdate(
                        UpdateService.formatBytes(
                          svc.checkResult?.fileSize ?? 0,
                        ),
                      ),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 8),
              ShadButton.outline(
                size: ShadButtonSize.sm,
                onPressed: svc.checkForUpdate,
                child: Text(s.recheck),
              ),
            ],
            if (status == UpdateStatus.readyToInstall) ...[
              ShadButton(
                size: ShadButtonSize.sm,
                onPressed: svc.installUpdate,
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const Icon(
                      LucideIcons.rotateCcw,
                      size: 13,
                      color: Colors.white,
                    ),
                    const SizedBox(width: 6),
                    Text(s.installAndRestart),
                  ],
                ),
              ),
            ],
            const Spacer(),
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: () => launchUrl(Uri.parse('https://fluxdown.zerx.dev')),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(LucideIcons.globe, size: 12, color: c.accent),
                    const SizedBox(width: 5),
                    Text(
                      s.officialWebsite,
                      style: TextStyle(
                        fontSize: 11,
                        color: c.accent,
                        decoration: TextDecoration.underline,
                        decorationColor: c.accent,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        ),
      ],
    );
  }

  Widget _statusRow(AppColors c, IconData icon, Color color, String text) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Row(
        children: [
          Icon(icon, size: 14, color: color),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              text,
              style: TextStyle(fontSize: 12, color: c.textPrimary),
              overflow: TextOverflow.ellipsis,
              maxLines: 2,
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildProgressSection(
    BuildContext context,
    UpdateService svc,
    AppColors c,
  ) {
    final p = svc.progress;
    if (p == null) return const SizedBox.shrink();

    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final fraction = p.totalBytes > 0
        ? (p.downloadedBytes / p.totalBytes).clamp(0.0, 1.0)
        : 0.0;
    final pctText = '${(fraction * 100).toStringAsFixed(1)}%';
    final sizeText =
        '${UpdateService.formatBytes(p.downloadedBytes)} / ${UpdateService.formatBytes(p.totalBytes)}';
    final speedText = UpdateService.formatSpeed(p.speed);
    final segments = p.segments;
    final activeSegments = p.activeSegments;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        ClipRRect(
          borderRadius: m.brXs,
          child: LinearProgressIndicator(
            value: fraction,
            backgroundColor: c.surface2,
            valueColor: AlwaysStoppedAnimation<Color>(c.accent),
            minHeight: 6,
          ),
        ),
        const SizedBox(height: 6),
        Row(
          children: [
            Text(
              '$pctText  $sizeText',
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
            const Spacer(),
            if (segments > 1) ...[
              Icon(
                LucideIcons.layers,
                size: 11,
                color: m.emphasis(c.accent),
              ),
              const SizedBox(width: 3),
              Text(
                s.segmentsDownloading(activeSegments, segments),
                style: TextStyle(
                  fontSize: 11,
                  color: m.emphasis(c.accent),
                ),
              ),
              const SizedBox(width: 10),
            ],
            Text(speedText, style: TextStyle(fontSize: 11, color: c.textMuted)),
          ],
        ),
      ],
    );
  }

  String _formatDate(String isoDate) {
    if (isoDate.isEmpty) return '';
    final dt = DateTime.tryParse(isoDate);
    if (dt == null) return isoDate;
    return '${dt.year}-${dt.month.toString().padLeft(2, '0')}-${dt.day.toString().padLeft(2, '0')}';
  }
}

// ─────────────────────────────────────────────
// 日志导出卡片
// ─────────────────────────────────────────────

class _LogExportCard extends StatefulWidget {
  final AppColors colors;
  final SettingsProvider settingsProvider;
  const _LogExportCard({required this.colors, required this.settingsProvider});

  @override
  State<_LogExportCard> createState() => _LogExportCardState();
}

class _LogExportCardState extends State<_LogExportCard> {
  bool _exporting = false;

  /// 日志总大小上限可选项（MB）
  static const _maxSizeOptions = [5, 10, 20, 50, 100];

  Future<void> _exportLogs() async {
    if (_exporting) return;
    setState(() => _exporting = true);
    try {
      final s = LocaleScope.of(context);
      final now = DateTime.now();
      final datePart =
          '${now.year}${now.month.toString().padLeft(2, '0')}${now.day.toString().padLeft(2, '0')}';
      final result = await FilePickerService.saveFile(
        dialogTitle: s.logSelectExportDir,
        fileName: 'fluxdown_logs_$datePart.zip',
        allowedExtensions: ['zip'],
      );
      if (result == null || !mounted) {
        if (mounted) setState(() => _exporting = false);
        return;
      }
      final savePath = result.endsWith('.zip') ? result : '$result.zip';
      final count = await LogService.instance.exportLogs(savePath);
      if (!mounted) return;
      if (count > 0) {
        ShadSonner.of(context).show(
          ShadToast(
            title: Text(s.logExportSuccess(count)),
            duration: const Duration(seconds: 3),
          ),
        );
      } else {
        ShadSonner.of(context).show(
          ShadToast(
            title: Text(s.logExportEmpty),
            duration: const Duration(seconds: 2),
          ),
        );
      }
    } catch (e) {
      if (mounted) {
        ShadSonner.of(context).show(
          ShadToast.destructive(
            title: Text(LocaleScope.of(context).logExportFailed),
            description: Text(e.toString()),
            duration: const Duration(seconds: 3),
          ),
        );
      }
    } finally {
      if (mounted) setState(() => _exporting = false);
    }
  }

  void _openLogDir() {
    final path = LogService.instance.logDir.path;
    if (Platform.isWindows) {
      Process.run('explorer', [path]);
    } else if (Platform.isMacOS) {
      Process.run('open', [path]);
    } else {
      Process.run('xdg-open', [path]);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.colors;
    final s = LocaleScope.of(context);
    final fileCount = LogService.instance.logFileCount;
    final sizeBytes = LogService.instance.logDirSizeBytes;
    final sizeText = UpdateService.formatBytes(sizeBytes);

    return _SettingCard(
      label: s.logExport,
      description: s.logExportDesc,
      vertical: true,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            s.logExportInfo(fileCount, sizeText),
            style: TextStyle(fontSize: 12, color: c.textMuted),
          ),
          const SizedBox(height: 10),
          Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      s.logMaxSize,
                      style: TextStyle(
                        fontSize: 12,
                        color: c.textPrimary,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                    const SizedBox(height: 2),
                    Text(
                      s.logMaxSizeDesc,
                      style: TextStyle(fontSize: 11, color: c.textMuted),
                    ),
                  ],
                ),
              ),
              ShadSelect<int>(
                initialValue: widget.settingsProvider.logMaxSizeMb,
                placeholder: Text('${widget.settingsProvider.logMaxSizeMb} MB'),
                options: _maxSizeOptions
                    .map((mb) => ShadOption(value: mb, child: Text('$mb MB')))
                    .toList(),
                selectedOptionBuilder: (context, value) => Text('$value MB'),
                onChanged: (v) {
                  if (v != null) {
                    widget.settingsProvider.setLogMaxSizeMb(v);
                    setState(() {});
                  }
                },
              ),
            ],
          ),
          const SizedBox(height: 10),
          Row(
            children: [
              ShadButton.outline(
                size: ShadButtonSize.sm,
                enabled: !_exporting,
                onPressed: _exportLogs,
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    if (_exporting) ...[
                      SizedBox(
                        width: 12,
                        height: 12,
                        child: CircularProgressIndicator(
                          strokeWidth: 1.5,
                          color: c.textSecondary,
                        ),
                      ),
                    ] else ...[
                      Icon(
                        LucideIcons.fileDown,
                        size: 13,
                        color: c.textSecondary,
                      ),
                    ],
                    const SizedBox(width: 6),
                    Text(s.logExportButton),
                  ],
                ),
              ),
              const SizedBox(width: 8),
              ShadButton.outline(
                size: ShadButtonSize.sm,
                onPressed: _openLogDir,
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(
                      LucideIcons.folderOpen,
                      size: 13,
                      color: c.textSecondary,
                    ),
                    const SizedBox(width: 6),
                    Text(s.logOpenDirButton),
                  ],
                ),
              ),
            ],
          ),
        ],
      ),
    );
  }
}
