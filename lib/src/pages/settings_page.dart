import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../../main.dart';
import '../i18n/locale_provider.dart';
import '../models/settings_provider.dart';
import '../services/update_service.dart';
import '../theme/app_colors.dart';
import '../theme/theme_provider.dart';
import '../widgets/title_drag_area.dart';

// ─────────────────────────────────────────────
// 设置分类枚举
// ─────────────────────────────────────────────

enum SettingsCategory {
  general(icon: LucideIcons.settings2),
  appearance(icon: LucideIcons.palette),
  download(icon: LucideIcons.download),
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
      SettingsCategory.about => s.settingsCatAbout,
    };
  }

  String get localizedDesc {
    final s = currentS;
    return switch (this) {
      SettingsCategory.general => s.settingsCatGeneralDesc,
      SettingsCategory.appearance => s.settingsCatAppearanceDesc,
      SettingsCategory.download => s.settingsCatDownloadDesc,
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
      width: 200,
      color: c.surface1,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const SizedBox(height: 16),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
            child: Text(
              LocaleScope.of(context).settings,
              style: TextStyle(
                fontSize: 10.5,
                fontWeight: FontWeight.w500,
                color: c.textMuted,
                letterSpacing: 0.5,
              ),
            ),
          ),
          const SizedBox(height: 4),
          for (final cat in SettingsCategory.values)
            _SettingsNavItem(
              icon: cat.icon,
              label: cat.localizedLabel,
              description: cat.localizedDesc,
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
  final String description;
  final bool isSelected;
  final VoidCallback onTap;

  const _SettingsNavItem({
    required this.icon,
    required this.label,
    required this.description,
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
          margin: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
          decoration: BoxDecoration(
            color: selected
                ? c.accentBg
                : _isHovered
                ? c.hoverBg
                : c.hoverBg.withValues(alpha: 0),
            borderRadius: BorderRadius.circular(8),
          ),
          child: Row(
            children: [
              Container(
                width: 32,
                height: 32,
                decoration: BoxDecoration(
                  color: selected
                      ? c.accent.withValues(alpha: 0.12)
                      : c.surface2,
                  borderRadius: BorderRadius.circular(7),
                ),
                child: Icon(
                  widget.icon,
                  size: 15,
                  color: selected ? c.accent : c.textSecondary,
                ),
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      widget.label,
                      style: TextStyle(
                        fontSize: 12.5,
                        color: selected ? c.accent : c.textPrimary,
                        fontWeight: selected
                            ? FontWeight.w600
                            : FontWeight.w500,
                      ),
                    ),
                    const SizedBox(height: 1),
                    Text(
                      widget.description,
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
      padding: const EdgeInsets.symmetric(horizontal: 40, vertical: 28),
      child: Align(
        alignment: Alignment.topCenter,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 600),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              _SectionHeader(category: category),
              const SizedBox(height: 24),
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
        Row(
          children: [
            Icon(category.icon, size: 18, color: c.accent),
            const SizedBox(width: 10),
            Text(
              category.localizedLabel,
              style: TextStyle(
                fontSize: 18,
                fontWeight: FontWeight.w600,
                color: c.textPrimary,
              ),
            ),
          ],
        ),
        const SizedBox(height: 4),
        Text(
          category.localizedDesc,
          style: TextStyle(fontSize: 13, color: c.textMuted),
        ),
        const SizedBox(height: 16),
        Divider(height: 1, color: c.border),
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
      padding: const EdgeInsets.all(16),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: c.border, width: 1),
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
                  style: TextStyle(fontSize: 12, color: c.textMuted),
                ),
                const SizedBox(height: 14),
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
                        style: TextStyle(fontSize: 12, color: c.textMuted),
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
                      barrierColor: const Color(0x1A000000),
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
            const SizedBox(height: 12),
            _SettingCard(
              label: LocaleScope.of(context).closeToTray,
              description: LocaleScope.of(context).closeToTrayDesc,
              child: ShadSwitch(
                value: settingsProvider.closeToTray,
                onChanged: (v) => settingsProvider.setCloseToTray(v),
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
        const SizedBox(height: 12),
        _SettingCard(
          label: LocaleScope.of(context).themeMode,
          description: LocaleScope.of(context).themeModeDesc,
          vertical: true,
          child: const _ThemeModeSelector(),
        ),
        const SizedBox(height: 12),
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
            const SizedBox(height: 12),
            _SettingCard(
              label: LocaleScope.of(context).defaultThreads,
              description: LocaleScope.of(context).defaultThreadsDesc,
              child: _SegmentSelector(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 12),
            _SettingCard(
              label: LocaleScope.of(context).maxConcurrent,
              description: LocaleScope.of(context).maxConcurrentDesc,
              child: _ConcurrentSelector(settingsProvider: settingsProvider),
            ),
            const SizedBox(height: 12),
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
// 下载设置子组件
// ─────────────────────────────────────────────

class _SaveDirPicker extends StatelessWidget {
  final SettingsProvider settingsProvider;

  const _SaveDirPicker({required this.settingsProvider});

  Future<void> _pickDir(BuildContext context) async {
    final result = await FilePicker.platform.getDirectoryPath(
      dialogTitle: currentS.selectDefaultSaveDir,
      initialDirectory: settingsProvider.defaultSaveDir,
    );
    if (result != null) {
      settingsProvider.setDefaultSaveDir(result);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Row(
      children: [
        Expanded(
          child: Container(
            height: 36,
            padding: const EdgeInsets.symmetric(horizontal: 12),
            decoration: BoxDecoration(
              color: c.bg,
              borderRadius: BorderRadius.circular(6),
              border: Border.all(color: c.border, width: 1),
            ),
            alignment: Alignment.centerLeft,
            child: Text(
              settingsProvider.defaultSaveDir,
              style: TextStyle(fontSize: 13, color: c.textPrimary),
              overflow: TextOverflow.ellipsis,
            ),
          ),
        ),
        const SizedBox(width: 8),
        ShadButton.outline(
          size: ShadButtonSize.sm,
          onPressed: () => _pickDir(context),
          child: Text(currentS.browse),
        ),
      ],
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

    return Row(
      children: [
        for (final item in options) ...[
          _ThemeModeCard(
            icon: item.icon,
            label: item.label,
            selected: current == item.pref,
            colors: c,
            onTap: () => localeNotifier.setLocale(item.pref),
          ),
          if (item != options.last) const SizedBox(width: 10),
        ],
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

    return Row(
      children: [
        for (final item in modes) ...[
          _ThemeModeCard(
            icon: item.icon,
            label: item.label,
            selected: current == item.mode,
            colors: c,
            onTap: () => provider.setThemeMode(item.mode),
          ),
          if (item != modes.last) const SizedBox(width: 10),
        ],
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
        ? theme.colorScheme.primary.withValues(alpha: 0.06)
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
          width: 96,
          padding: const EdgeInsets.symmetric(vertical: 14),
          decoration: BoxDecoration(
            color: bgColor,
            borderRadius: BorderRadius.circular(10),
            border: Border.all(color: borderColor, width: selected ? 1.5 : 1),
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                widget.icon,
                size: 20,
                color: selected ? theme.colorScheme.primary : c.textSecondary,
              ),
              const SizedBox(height: 8),
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
      spacing: 10,
      runSpacing: 10,
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
            width: 34,
            height: 34,
            decoration: BoxDecoration(
              color: widget.scheme.previewColor,
              shape: BoxShape.circle,
              border: Border.all(
                color: selected
                    ? widget.colors.textPrimary
                    : _isHovered
                    ? widget.colors.textSecondary
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
                          alpha: 0.3,
                        ),
                        blurRadius: 8,
                        spreadRadius: 1,
                      ),
                    ]
                  : null,
            ),
            child: selected
                ? const Icon(LucideIcons.check, size: 15, color: Colors.white)
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
            const SizedBox(height: 12),
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
