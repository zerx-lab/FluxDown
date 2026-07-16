// 插件设置分类 body：已安装插件管理（启用/设置/卸载） + 安装区
// （zip 上传 / 目录 + 开发模式）+ 插件市场浏览/安装。

import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:url_launcher/url_launcher.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/plugin_provider.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'dir_picker_field.dart';
import 'plugin_detail_dialog.dart';
import 'plugin_setting_form.dart';

class PluginListView extends StatefulWidget {
  final PluginProvider provider;

  /// 安装的插件缺基础组件（ffmpeg/yt-dlp）时「前往组件设置」的跳转回调
  /// （由设置页注入，切到「扩展 → 组件」Tab）。
  final VoidCallback? onNavigateToComponents;

  const PluginListView({
    super.key,
    required this.provider,
    this.onNavigateToComponents,
  });

  @override
  State<PluginListView> createState() => _PluginListViewState();
}

class _PluginListViewState extends State<PluginListView> {
  int _lastOpSeq = -1;
  bool _installingZip = false;
  bool _installingDir = false;
  bool _devMode = true;
  String _devDirPath = '';
  String _marketQuery = '';
  int _marketLimit = _marketPageSize;

  static const int _marketPageSize = 50;

  @override
  void initState() {
    super.initState();
    _lastOpSeq = widget.provider.opResultSeq;
    widget.provider.addListener(_onProviderChanged);
    widget.provider.requestMarket();
  }

  @override
  void dispose() {
    widget.provider.removeListener(_onProviderChanged);
    super.dispose();
  }

  /// 插件写操作结果的全局提示（save_settings 由对话框自身展示，此处跳过避免重复弹）。
  void _onProviderChanged() {
    if (!mounted) return;
    final seq = widget.provider.opResultSeq;
    if (seq != _lastOpSeq) {
      _lastOpSeq = seq;
      final result = widget.provider.lastOpResult;
      if (result != null && result.op != 'save_settings') {
        _showOpResultToast(result);
      }
    }
    setState(() {});
  }

  void _showOpResultToast(PluginOpResult result) {
    final s = currentS;
    if (result.ok) {
      final message = switch (result.op) {
        'install' || 'market_install' => s.pluginOpInstallSuccess,
        'uninstall' => s.pluginOpUninstallSuccess,
        _ => null,
      };
      if (message == null) return;
      ShadSonner.of(context).show(
        ShadToast(title: Text(message), duration: const Duration(seconds: 2)),
      );
      // 安装成功但声明权限所需的基础组件缺失 → 弹依赖提醒（提醒式非阻断，
      // 组件缺失时对应 flux.* 能力面 available() 为 false，插件本身可运行）。
      if ((result.op == 'install' || result.op == 'market_install') &&
          result.missingComponents.isNotEmpty) {
        _showDepsReminder(result.missingComponents);
      }
      return;
    }
    final message = switch (result.op) {
      'install' || 'market_install' => s.pluginOpInstallFailed(result.message),
      'uninstall' => s.pluginOpUninstallFailed(result.message),
      'set_enabled' => s.pluginOpEnabledFailed(result.message),
      _ => s.pluginOpGenericFailed(result.message),
    };
    ShadSonner.of(context).show(ShadToast.destructive(title: Text(message)));
  }

  Future<void> _pickZip() async {
    if (_installingZip) return;
    setState(() => _installingZip = true);
    try {
      final files = await FilePickerService.pickFiles(
        dialogTitle: currentS.pluginInstallZipButton,
        allowedExtensions: const ['zip'],
      );
      final file = files == null || files.isEmpty ? null : files.first;
      if (file != null) {
        final bytes = await file.readAsBytes();
        widget.provider.install(zipBytes: Uint8List.fromList(bytes));
      }
    } on FilePickerException catch (e) {
      if (mounted) {
        ShadSonner.of(context).show(
          ShadToast.destructive(
            title: Text(currentS.pluginInstallZipFailed(e.toString())),
          ),
        );
      }
    } finally {
      if (mounted) setState(() => _installingZip = false);
    }
  }

  Future<void> _pickDevDir() async {
    if (_installingDir) return;
    setState(() => _installingDir = true);
    try {
      final result = await FilePickerService.pickDirectory(
        dialogTitle: currentS.pluginInstallDirPlaceholder,
        initialDirectory: _devDirPath.isNotEmpty ? _devDirPath : null,
      );
      if (result != null && mounted) {
        setState(() => _devDirPath = result);
      }
    } on FilePickerException catch (_) {
      // 用户取消或选择器错误：静默忽略，与 _SaveDirPickerState 行为一致
    } finally {
      if (mounted) setState(() => _installingDir = false);
    }
  }

  void _installDevDir() {
    if (_devDirPath.isEmpty) return;
    widget.provider.install(dirPath: _devDirPath, devMode: _devMode);
    setState(() => _devDirPath = '');
  }

  void _confirmUninstall(PluginInfoSignal plugin) {
    final s = currentS;
    final c = AppColors.of(context);
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.pluginUninstallTitle),
        description: Text(s.pluginUninstallMsg(plugin.name)),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
          ShadButton.destructive(
            onPressed: () {
              Navigator.of(ctx).pop();
              widget.provider.uninstall(plugin.identity);
            },
            child: Text(s.pluginUninstallTooltip),
          ),
        ],
      ),
    );
  }

  /// 组件名 → 设置页展示名（与「组件」分类标题一致）。
  String _componentDisplayName(String component) {
    final s = currentS;
    return switch (component) {
      'ffmpeg' => s.componentsFfmpegTitle,
      'ytdlp' => s.componentsYtdlpTitle,
      _ => component,
    };
  }

  /// 依赖组件缺失提醒：列出缺失组件，可一键跳转组件设置分类。
  void _showDepsReminder(List<String> missing) {
    final s = currentS;
    final c = AppColors.of(context);
    final names = missing.map(_componentDisplayName).join(', ');
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.pluginDepsMissingTitle),
        description: Text(s.pluginDepsMissingBody(names)),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.pluginDepsLater),
          ),
          ShadButton(
            onPressed: () {
              Navigator.of(ctx).pop();
              widget.onNavigateToComponents?.call();
            },
            child: Text(s.pluginDepsGoToComponents),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final s = currentS;
    final c = AppColors.of(context);
    final provider = widget.provider;
    final installedIds = provider.plugins.map((p) => p.identity).toSet();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                s.pluginsSectionTitle,
                style: TextStyle(
                  fontSize: 14,
                  fontWeight: FontWeight.w600,
                  color: c.textPrimary,
                ),
              ),
            ),
            Text(
              s.pluginDevModeSwitch,
              style: TextStyle(fontSize: 12, color: c.textSecondary),
            ),
            const SizedBox(width: 6),
            ShadSwitch(
              value: _devMode,
              onChanged: (v) => setState(() => _devMode = v),
            ),
          ],
        ),
        const SizedBox(height: 10),
        _buildInstallArea(context),
        const SizedBox(height: 14),
        if (provider.plugins.isEmpty)
          Text(
            s.pluginsEmpty,
            style: TextStyle(fontSize: 12.5, color: c.textMuted),
          )
        else
          for (final p in provider.plugins)
            Padding(
              padding: const EdgeInsets.only(bottom: 6),
              child: _PluginCard(
                plugin: p,
                provider: provider,
                onUninstall: () => _confirmUninstall(p),
              ),
            ),
        const SizedBox(height: 26),
        Text(
          s.marketSectionTitle,
          style: TextStyle(
            fontSize: 14,
            fontWeight: FontWeight.w600,
            color: c.textPrimary,
          ),
        ),
        const SizedBox(height: 2),
        Text(
          s.marketSectionDesc,
          style: TextStyle(fontSize: 11.5, color: c.textMuted),
        ),
        const SizedBox(height: 10),
        _buildMarketArea(context, installedIds),
      ],
    );
  }

  Widget _buildInstallArea(BuildContext context) {
    final s = currentS;
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Container(
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brDialog,
        border: Border.all(color: c.border, width: 1),
      ),
      child: Row(
        children: [
          ShadButton.outline(
            onPressed: _installingZip ? null : _pickZip,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(LucideIcons.upload, size: 14, color: c.textPrimary),
                const SizedBox(width: 6),
                Text(s.pluginInstallZipButton),
              ],
            ),
          ),
          if (_devMode) ...[
            const SizedBox(width: 10),
            Expanded(
              child: ShadTooltip(
                waitDuration: const Duration(milliseconds: 300),
                builder: (_) => Text(s.pluginInstallDirLabel),
                child: DirPickerField(
                  path: _devDirPath,
                  placeholder: s.pluginInstallDirPlaceholder,
                  enabled: !_installingDir,
                  onTap: _pickDevDir,
                ),
              ),
            ),
            const SizedBox(width: 10),
            ShadButton(
              onPressed: _devDirPath.isEmpty ? null : _installDevDir,
              child: Text(s.pluginInstallDirButton),
            ),
          ] else
            const Spacer(),
        ],
      ),
    );
  }

  Widget _buildMarketArea(BuildContext context, Set<String> installedIds) {
    final s = currentS;
    final c = AppColors.of(context);
    final provider = widget.provider;
    if (provider.marketLoading) {
      return Text(
        s.pluginCommonLoading,
        style: TextStyle(fontSize: 12.5, color: c.textMuted),
      );
    }
    if (provider.marketError.isNotEmpty) {
      return Text(
        s.marketLoadFailed(provider.marketError),
        style: TextStyle(fontSize: 12.5, color: c.statusError),
      );
    }
    final query = _marketQuery.trim().toLowerCase();
    final filtered = query.isEmpty
        ? provider.marketEntries
        : provider.marketEntries.where((e) {
            bool hit(String v) => v.toLowerCase().contains(query);
            return hit(e.name) ||
                hit(e.pluginId) ||
                hit(e.description) ||
                hit(e.author) ||
                e.tags.any(hit);
          }).toList();
    final visible = filtered.length > _marketLimit
        ? filtered.sublist(0, _marketLimit)
        : filtered;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (provider.marketEntries.length > 8 || query.isNotEmpty) ...[
          ShadInput(
            placeholder: Text(s.marketSearchPlaceholder),
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
            onChanged: (v) => setState(() {
              _marketQuery = v;
              _marketLimit = _marketPageSize;
            }),
          ),
          const SizedBox(height: 8),
        ],
        if (provider.marketEntries.isEmpty)
          Text(
            s.marketEmpty,
            style: TextStyle(fontSize: 12.5, color: c.textMuted),
          )
        else if (filtered.isEmpty)
          Text(
            s.marketSearchNoResult,
            style: TextStyle(fontSize: 12.5, color: c.textMuted),
          )
        else ...[
          for (final entry in visible)
            Padding(
              padding: const EdgeInsets.only(bottom: 6),
              child: _MarketCard(
                entry: entry,
                installed: installedIds.contains(entry.pluginId),
                provider: provider,
              ),
            ),
          if (filtered.length > _marketLimit)
            ShadButton.ghost(
              onPressed: () => setState(() => _marketLimit += _marketPageSize),
              child: Text(
                s.marketShowMore(filtered.length - _marketLimit),
                style: TextStyle(fontSize: 12, color: c.accent),
              ),
            ),
        ],
      ],
    );
  }
}

// =============================================================================
// 已安装插件卡片
// =============================================================================

class _PluginCard extends StatelessWidget {
  final PluginInfoSignal plugin;
  final PluginProvider provider;
  final VoidCallback onUninstall;

  const _PluginCard({
    required this.plugin,
    required this.provider,
    required this.onUninstall,
  });

  @override
  Widget build(BuildContext context) {
    final s = currentS;
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);

    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: () => showPluginDetailDialog(
        context,
        name: plugin.name,
        version: plugin.version,
        identity: plugin.identity,
        description: plugin.description,
        homepage: plugin.homepage,
        settingsCount: plugin.settings.length,
        permissions: plugin.permissions,
      ),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          decoration: BoxDecoration(
            color: c.surface1,
            borderRadius: m.brDialog,
            border: Border.all(color: c.border, width: 1),
          ),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Wrap(
                      crossAxisAlignment: WrapCrossAlignment.center,
                      spacing: 8,
                      runSpacing: 4,
                      children: [
                        Text(
                          plugin.name,
                          style: TextStyle(
                            fontSize: 13.5,
                            fontWeight: FontWeight.w600,
                            color: c.textPrimary,
                          ),
                        ),
                        Text(
                          'v${plugin.version}',
                          style: TextStyle(fontSize: 11, color: c.textMuted),
                        ),
                        if (plugin.homepage.isNotEmpty)
                          GestureDetector(
                            onTap: () => launchUrl(Uri.parse(plugin.homepage)),
                            child: Text(
                              plugin.homepage,
                              style: TextStyle(fontSize: 11, color: c.accent),
                            ),
                          ),
                        if (plugin.devMode)
                          _Badge(
                            text: s.pluginDevModeBadge,
                            color: c.accent,
                            bg: m.subtle(c.accent),
                          ),
                        if (plugin.disabledReason == 'Manual')
                          _Badge(
                            text: s.pluginDisabledManual,
                            color: c.textSecondary,
                            bg: c.surface2,
                          ),
                        if (plugin.disabledReason == 'CircuitBreaker')
                          _Badge(
                            text: s.pluginDisabledCircuitBreaker,
                            color: AppColors.red,
                            bg: m.subtle(AppColors.red),
                          ),
                      ],
                    ),
                    if (plugin.description.isNotEmpty) ...[
                      const SizedBox(height: 2),
                      _HoverDescription(text: plugin.description),
                    ],
                  ],
                ),
              ),
              const SizedBox(width: 12),
              ShadSwitch(
                value: plugin.enabled,
                onChanged: (v) => provider.setEnabled(plugin.identity, v),
              ),
              const SizedBox(width: 4),
              if (plugin.settings.isNotEmpty)
                ShadIconButton.ghost(
                  icon: Icon(
                    LucideIcons.settings2,
                    size: 16,
                    color: c.textSecondary,
                  ),
                  onPressed: () => showPluginSettingsDialog(
                    context,
                    plugin: plugin,
                    provider: provider,
                  ),
                ),
              ShadIconButton.ghost(
                icon: Icon(LucideIcons.trash2, size: 16, color: AppColors.red),
                onPressed: onUninstall,
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 市场条目卡片
// =============================================================================

class _MarketCard extends StatefulWidget {
  final MarketEntrySignal entry;
  final bool installed;
  final PluginProvider provider;

  const _MarketCard({
    required this.entry,
    required this.installed,
    required this.provider,
  });

  @override
  State<_MarketCard> createState() => _MarketCardState();
}

class _MarketCardState extends State<_MarketCard> {
  bool _pending = false;
  int _lastSeenOpSeq = -1;

  @override
  void initState() {
    super.initState();
    _lastSeenOpSeq = widget.provider.opResultSeq;
  }

  @override
  void didUpdateWidget(covariant _MarketCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    final seq = widget.provider.opResultSeq;
    if (seq != _lastSeenOpSeq) {
      _lastSeenOpSeq = seq;
      if (widget.provider.lastOpResult?.op == 'market_install') {
        _pending = false;
      }
    }
    if (widget.installed) _pending = false;
  }

  void _install() {
    setState(() => _pending = true);
    widget.provider.installMarket(widget.entry.pluginId);
  }

  @override
  Widget build(BuildContext context) {
    final s = currentS;
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final entry = widget.entry;
    final busy = _pending && !widget.installed;
    final yankedLabel = switch (entry.yanked) {
      'deprecated' => s.marketYankedDeprecated,
      'vulnerable' => s.marketYankedVulnerable,
      'malicious' => s.marketYankedMalicious,
      _ => null,
    };

    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: () => showPluginDetailDialog(
        context,
        name: entry.name,
        version: entry.version,
        identity: entry.pluginId,
        description: entry.description,
        homepage: entry.homepage,
        author: entry.author,
        tags: entry.tags,
        publishTime: entry.publishTime,
        minAppVersion: entry.minAppVersion,
        yankedLabel: yankedLabel,
        permissions: entry.permissions,
      ),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          decoration: BoxDecoration(
            color: c.surface1,
            borderRadius: m.brDialog,
            border: Border.all(color: c.border, width: 1),
          ),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Wrap(
                      crossAxisAlignment: WrapCrossAlignment.center,
                      spacing: 8,
                      runSpacing: 4,
                      children: [
                        Text(
                          entry.name.isNotEmpty ? entry.name : entry.pluginId,
                          style: TextStyle(
                            fontSize: 13.5,
                            fontWeight: FontWeight.w600,
                            color: c.textPrimary,
                          ),
                        ),
                        Text(
                          'v${entry.version}',
                          style: TextStyle(fontSize: 11, color: c.textMuted),
                        ),
                        if (entry.author.isNotEmpty)
                          Text(
                            entry.author,
                            style: TextStyle(fontSize: 11, color: c.textMuted),
                          ),
                        if (entry.homepage.isNotEmpty)
                          GestureDetector(
                            onTap: () => launchUrl(Uri.parse(entry.homepage)),
                            child: Text(
                              entry.homepage,
                              style: TextStyle(fontSize: 11, color: c.accent),
                            ),
                          ),
                        if (yankedLabel != null)
                          _Badge(
                            text: yankedLabel,
                            color: AppColors.red,
                            bg: m.subtle(AppColors.red),
                          ),
                      ],
                    ),
                    if (entry.description.isNotEmpty) ...[
                      const SizedBox(height: 2),
                      _HoverDescription(text: entry.description),
                    ],
                  ],
                ),
              ),
              const SizedBox(width: 12),
              ShadButton.outline(
                onPressed: (widget.installed || busy) ? null : _install,
                child: Text(
                  widget.installed
                      ? s.marketInstalledButton
                      : busy
                      ? s.marketInstallingButton
                      : s.marketInstallButton,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 单行省略描述：鼠标悬浮 tooltip 显示全文
// =============================================================================

class _HoverDescription extends StatelessWidget {
  final String text;

  const _HoverDescription({required this.text});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return ShadTooltip(
      waitDuration: const Duration(milliseconds: 300),
      builder: (_) => ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 380),
        child: Text(text),
      ),
      child: Text(
        text,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: TextStyle(fontSize: 12, color: c.textSecondary),
      ),
    );
  }
}

// =============================================================================
// 小徽章
// =============================================================================

class _Badge extends StatelessWidget {
  final String text;
  final Color color;
  final Color bg;

  const _Badge({required this.text, required this.color, required this.bg});

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
      decoration: BoxDecoration(color: bg, borderRadius: m.brPill),
      child: Text(
        text,
        style: TextStyle(
          fontSize: 10.5,
          fontWeight: FontWeight.w500,
          color: color,
        ),
      ),
    );
  }
}
