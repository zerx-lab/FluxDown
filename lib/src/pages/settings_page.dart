import 'dart:async';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../../main.dart';
import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/settings_provider.dart';
import '../services/update_service.dart';
import '../theme/app_colors.dart';
import '../theme/theme_provider.dart';
import '../widgets/dir_picker_field.dart';
import '../widgets/title_drag_area.dart';

// ─────────────────────────────────────────────
// 设置分类枚举
// ─────────────────────────────────────────────

enum SettingsCategory {
  general(icon: LucideIcons.settings2),
  appearance(icon: LucideIcons.palette),
  download(icon: LucideIcons.download),
  bt(icon: LucideIcons.magnet),
  proxy(icon: LucideIcons.globe),
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
      SettingsCategory.proxy => s.settingsCatProxy,
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
      SettingsCategory.proxy => s.settingsCatProxyDesc,
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
      label: s.torrentFileAssociation,
      description: s.torrentFileAssociationDesc,
      keywords: s.searchKeywordsFileAssoc,
      icon: LucideIcons.fileType,
    ),
    SettingsSearchItem(
      category: SettingsCategory.general,
      label: s.analyticsEnabled,
      description: s.analyticsEnabledDesc,
      keywords: s.searchKeywordsAnalytics,
      icon: LucideIcons.chartLine,
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
      category: SettingsCategory.download,
      label: s.defaultSaveDir,
      description: s.defaultSaveDirDesc,
      keywords: s.searchKeywordsSaveDir,
      icon: LucideIcons.folderOpen,
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
      category: SettingsCategory.about,
      label: s.checkUpdate,
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
      category: SettingsCategory.proxy,
      label: s.proxySettings,
      description: s.proxySettingsDesc,
      keywords: s.searchKeywordsProxy,
      icon: LucideIcons.globe,
    ),
  ];
}

// ─────────────────────────────────────────────
// 设置页面（带侧边栏导航）
// ─────────────────────────────────────────────

class SettingsPage extends StatefulWidget {
  final VoidCallback onBack;
  final SettingsProvider settingsProvider;
  final SettingsCategory? initialCategory;

  const SettingsPage({
    super.key,
    required this.onBack,
    required this.settingsProvider,
    this.initialCategory,
  });

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  late SettingsCategory _selected;

  @override
  void initState() {
    super.initState();
    _selected = widget.initialCategory ?? SettingsCategory.general;
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Column(
      children: [
        // 顶部标题栏
        TitleDragArea(
          child: Container(
            height: 48,
            padding: const EdgeInsets.only(left: 12, right: 289),
            decoration: BoxDecoration(
              color: c.surface1,
              border: Border(bottom: BorderSide(color: c.border, width: 1)),
            ),
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
                        style: TextStyle(fontSize: 13, color: c.textSecondary),
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
        // 主体：侧边栏 + 内容区
        Expanded(
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // 左侧导航栏
              _SettingsSidebar(
                selected: _selected,
                onSelect: (cat) => setState(() => _selected = cat),
              ),
              // 分隔线
              Container(width: 1, color: c.border),
              // 右侧内容区
              Expanded(
                child: _SettingsContent(
                  category: _selected,
                  settingsProvider: widget.settingsProvider,
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

class _SettingsSidebar extends StatelessWidget {
  final SettingsCategory selected;
  final ValueChanged<SettingsCategory> onSelect;

  const _SettingsSidebar({required this.selected, required this.onSelect});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Container(
      width: 180,
      color: c.surface1,
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          for (final cat in SettingsCategory.values)
            _SettingsNavItem(
              icon: cat.icon,
              label: cat.localizedLabel,
              isSelected: selected == cat,
              onTap: () => onSelect(cat),
            ),
        ],
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
            borderRadius: BorderRadius.circular(7),
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
                    borderRadius: BorderRadius.circular(2),
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

class _SettingsContent extends StatelessWidget {
  final SettingsCategory category;
  final SettingsProvider settingsProvider;

  const _SettingsContent({
    required this.category,
    required this.settingsProvider,
  });

  @override
  Widget build(BuildContext context) {
    return SingleChildScrollView(
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
                  ),
                  SettingsCategory.bt => _BtContent(
                    key: const ValueKey('bt'),
                    settingsProvider: settingsProvider,
                  ),
                  SettingsCategory.proxy => _ProxyContent(
                    key: const ValueKey('proxy'),
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
        Divider(height: 1, color: c.border.withValues(alpha: 0.5)),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 设置卡片：每个设置项的统一容器
// ─────────────────────────────────────────────

class _SettingCard extends StatelessWidget {
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
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: c.border.withValues(alpha: 0.6), width: 1),
      ),
      child: vertical
          ? Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  label,
                  style: TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w500,
                    color: c.textPrimary,
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  description,
                  style: TextStyle(fontSize: 11.5, color: c.textMuted),
                ),
                const SizedBox(height: 12),
                child,
              ],
            )
          : Row(
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
                          color: c.textPrimary,
                        ),
                      ),
                      const SizedBox(height: 2),
                      Text(
                        description,
                        style: TextStyle(fontSize: 11.5, color: c.textMuted),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 16),
                child,
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
              label: LocaleScope.of(context).analyticsEnabled,
              description: LocaleScope.of(context).analyticsEnabledDesc,
              child: ShadSwitch(
                value: settingsProvider.analyticsEnabled,
                onChanged: (v) => settingsProvider.setAnalyticsEnabled(v),
              ),
            ),
          ],
        );
      },
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
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _SettingCard(
          label: LocaleScope.of(context).language,
          description: LocaleScope.of(context).languageDesc,
          vertical: true,
          child: const _LanguageSelector(),
        ),
        const SizedBox(height: 10),
        _SettingCard(
          label: LocaleScope.of(context).themeMode,
          description: LocaleScope.of(context).themeModeDesc,
          vertical: true,
          child: const _ThemeModeSelector(),
        ),
        const SizedBox(height: 10),
        _SettingCard(
          label: LocaleScope.of(context).themeColor,
          description: LocaleScope.of(context).themeColorDesc,
          vertical: true,
          child: const _ColorSchemeSelector(),
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

  const _DownloadContent({super.key, required this.settingsProvider});

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: settingsProvider,
      builder: (context, _) {
        return Column(
          children: [
            _SettingCard(
              label: LocaleScope.of(context).defaultSaveDir,
              description: LocaleScope.of(context).defaultSaveDirDesc,
              vertical: true,
              child: _SaveDirPicker(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).defaultThreads,
              description: LocaleScope.of(context).defaultThreadsDesc,
              child: _SegmentSelector(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).maxConcurrent,
              description: LocaleScope.of(context).maxConcurrentDesc,
              child: _ConcurrentSelector(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 10),
            _SettingCard(
              label: LocaleScope.of(context).speedLimit,
              description: LocaleScope.of(context).speedLimitDesc,
              vertical: true,
              child: _SpeedLimitInput(settingsProvider: settingsProvider),
            ),
          ],
        );
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
              label: LocaleScope.of(context).btTrackerList,
              description: LocaleScope.of(context).btTrackerListDesc,
              vertical: true,
              child: _BtTrackerEditor(settingsProvider: settingsProvider),
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
      final result = await FilePicker.platform.getDirectoryPath(
        dialogTitle: currentS.selectDefaultSaveDir,
        lockParentWindow: true,
        initialDirectory: widget.settingsProvider.defaultSaveDir,
      );
      if (result != null && mounted) {
        widget.settingsProvider.setDefaultSaveDir(result);
      }
    } finally {
      if (mounted) setState(() => _isPicking = false);
    }
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

  // 0 = 自动（由 Rust segment_advisor 动态计算最优值）
  static const _options = [0, 4, 8, 16, 32, 64];

  static String _label(int n) => n == 0 ? currentS.auto : currentS.nThreads(n);

  @override
  Widget build(BuildContext context) {
    final current = settingsProvider.defaultSegments;
    return ShadSelect<int>(
      placeholder: Text(currentS.auto),
      initialValue: current,
      options: _options
          .map((n) => ShadOption(value: n, child: Text(_label(n))))
          .toList(),
      selectedOptionBuilder: (context, value) => Text(_label(value)),
      onChanged: (v) {
        if (v != null) settingsProvider.setDefaultSegments(v);
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
    final s = LocaleScope.of(context);
    final sp = widget.settingsProvider;
    final isManual = sp.proxyMode == 'manual';

    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: c.border.withValues(alpha: 0.6), width: 1),
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
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              _ProxyModeOption(
                icon: LucideIcons.unplug,
                label: s.proxyModeNone,
                selected: sp.proxyMode == 'none',
                colors: c,
                onTap: () => sp.setProxyMode('none'),
              ),
              _ProxyModeOption(
                icon: LucideIcons.monitor,
                label: s.proxyModeSystem,
                selected: sp.proxyMode == 'system',
                colors: c,
                onTap: () {
                  sp.setProxyMode('system');
                  _requestDetectSystemProxy();
                },
              ),
              _ProxyModeOption(
                icon: LucideIcons.settings2,
                label: s.proxyModeManual,
                selected: sp.proxyMode == 'manual',
                colors: c,
                onTap: () => sp.setProxyMode('manual'),
              ),
            ],
          ),
          // 系统代理只读展示
          if (sp.proxyMode == 'system') ...[
            const SizedBox(height: 16),
            Divider(height: 1, color: c.border.withValues(alpha: 0.4)),
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
            Divider(height: 1, color: c.border.withValues(alpha: 0.4)),
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
    final selected = widget.selected;
    final borderColor = selected ? theme.colorScheme.primary : c.border;
    final bgColor = selected
        ? theme.colorScheme.primary.withValues(alpha: 0.08)
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
            borderRadius: BorderRadius.circular(8),
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
              borderRadius: BorderRadius.circular(6),
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
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
      decoration: BoxDecoration(
        color: colors.surface1,
        borderRadius: BorderRadius.circular(6),
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
// BT 设置子组件
// ─────────────────────────────────────────────

/// BT 设置分区标题（带分割线）
class _SubSectionHeader extends StatelessWidget {
  final String label;
  final String description;
  final IconData icon;

  const _SubSectionHeader({
    required this.label,
    required this.description,
    this.icon = LucideIcons.magnet,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Divider(height: 1, color: c.border.withValues(alpha: 0.5)),
        const SizedBox(height: 14),
        Row(
          children: [
            Icon(icon, size: 14, color: c.accent),
            const SizedBox(width: 8),
            Text(
              label,
              style: TextStyle(
                fontSize: 14,
                fontWeight: FontWeight.w600,
                color: c.textPrimary,
              ),
            ),
          ],
        ),
        const SizedBox(height: 4),
        Text(description, style: TextStyle(fontSize: 11.5, color: c.textMuted)),
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
    final lines = _controller.text
        .split('\n')
        .map((l) => l.trim())
        .where((l) => l.isNotEmpty)
        .toList();
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
                    selectionColor: c.accent.withValues(alpha: 0.25),
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
                        borderRadius: BorderRadius.circular(8),
                        borderSide: BorderSide(color: c.inputBorder),
                      ),
                      enabledBorder: OutlineInputBorder(
                        borderRadius: BorderRadius.circular(8),
                        borderSide: BorderSide(color: c.inputBorder),
                      ),
                      focusedBorder: OutlineInputBorder(
                        borderRadius: BorderRadius.circular(8),
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
    final theme = ShadTheme.of(context);
    final c = widget.colors;
    final selected = widget.selected;
    final borderColor = selected ? theme.colorScheme.primary : c.border;
    final bgColor = selected
        ? theme.colorScheme.primary.withValues(alpha: 0.08)
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
            borderRadius: BorderRadius.circular(8),
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
// 主题色选择器
// ─────────────────────────────────────────────

class _ColorSchemeSelector extends StatelessWidget {
  const _ColorSchemeSelector();

  @override
  Widget build(BuildContext context) {
    final provider = FluxDownApp.of(context);
    final current = provider.colorScheme;
    final c = AppColors.of(context);

    return Wrap(
      spacing: 8,
      runSpacing: 8,
      children: [
        for (final scheme in AppColorScheme.values)
          _ColorDot(
            scheme: scheme,
            selected: current == scheme,
            colors: c,
            onTap: () => provider.setColorScheme(scheme),
          ),
      ],
    );
  }
}

class _ColorDot extends StatefulWidget {
  final AppColorScheme scheme;
  final bool selected;
  final AppColors colors;
  final VoidCallback onTap;

  const _ColorDot({
    required this.scheme,
    required this.selected,
    required this.colors,
    required this.onTap,
  });

  @override
  State<_ColorDot> createState() => _ColorDotState();
}

class _ColorDotState extends State<_ColorDot> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final selected = widget.selected;
    return ShadTooltip(
      builder: (_) => Text(widget.scheme.label),
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
              color: widget.scheme.previewColor,
              shape: BoxShape.circle,
              border: Border.all(
                color: selected
                    ? widget.colors.textPrimary
                    : _isHovered
                    ? widget.colors.textSecondary.withValues(alpha: 0.6)
                    : widget.scheme.previewColor,
                width: selected
                    ? 2.5
                    : _isHovered
                    ? 1.5
                    : 0,
              ),
              boxShadow: _isHovered || selected
                  ? [
                      BoxShadow(
                        color: widget.scheme.previewColor.withValues(
                          alpha: 0.25,
                        ),
                        blurRadius: 6,
                        spreadRadius: 0,
                      ),
                    ]
                  : null,
            ),
            child: selected
                ? const Icon(LucideIcons.check, size: 13, color: Colors.white)
                : null,
          ),
        ),
      ),
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
          _buildProgressSection(svc, c),
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

  Widget _buildProgressSection(UpdateService svc, AppColors c) {
    final p = svc.progress;
    if (p == null) return const SizedBox.shrink();

    final fraction = p.totalBytes > 0
        ? (p.downloadedBytes / p.totalBytes).clamp(0.0, 1.0)
        : 0.0;
    final pctText = '${(fraction * 100).toStringAsFixed(1)}%';
    final sizeText =
        '${UpdateService.formatBytes(p.downloadedBytes)} / ${UpdateService.formatBytes(p.totalBytes)}';
    final speedText = UpdateService.formatSpeed(p.speed);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        ClipRRect(
          borderRadius: BorderRadius.circular(2),
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
