import 'package:flutter/material.dart' show ThemeMode;
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../bindings/bindings.dart';
import '../../i18n/locale_provider.dart';
import '../../models/download_task.dart';
import '../../models/settings_provider.dart';
import '../../theme/app_colors.dart';
import '../../theme/app_metrics.dart';
import '../../theme/theme_provider.dart';
import '../../services/update_service.dart';
import '../services/mobile_storage_service.dart';
import '../mobile_ui.dart';

const _appVersion = String.fromEnvironment('APP_VERSION', defaultValue: 'dev');

/// 设置屏（移动端：分组卡片列表）
class MobileSettingsScreen extends StatelessWidget {
  final SettingsProvider settings;
  final ThemeProvider themeProvider;
  final LocaleNotifier localeNotifier;

  const MobileSettingsScreen({
    super.key,
    required this.settings,
    required this.themeProvider,
    required this.localeNotifier,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final topInset = MediaQuery.paddingOf(context).top;

    return Container(
      color: c.bg,
      child: Stack(
        children: [
          Positioned.fill(
            child: ListenableBuilder(
              listenable: Listenable.merge([
                settings,
                themeProvider,
                UpdateService.instance,
              ]),
              builder: (context, _) {
                return ListView(
                  padding: EdgeInsets.fromLTRB(
                    m.mobilePageMargin,
                    topInset + m.mobileAppBarHeight + 8,
                    m.mobilePageMargin,
                    m.mobileScrollBottomPadding,
                  ),
                  children: [
                    _GroupLabel(s.settingsCatGeneral),
                    _Group(
                      children: [
                        _Row(
                          label: s.language,
                          value: switch (localeNotifier.preference) {
                            'zh' => s.languageChinese,
                            'en' => s.languageEnglish,
                            _ => s.languageSystem,
                          },
                          onTap: () => _selectLanguage(context),
                        ),
                        _Row(
                          label: s.themeMode,
                          value: switch (themeProvider.themeMode) {
                            ThemeMode.light => s.themeModeLight,
                            ThemeMode.dark => s.themeModeDark,
                            ThemeMode.system => s.themeModeSystem,
                          },
                          onTap: () => _selectThemeMode(context),
                        ),
                        _Row(
                          label: s.notifyOnComplete,
                          trailing: ShadSwitch(
                            value: settings.notifyOnComplete,
                            onChanged: settings.setNotifyOnComplete,
                          ),
                        ),
                      ],
                    ),
                    _GroupLabel(s.settingsCatDownload),
                    _Group(
                      children: [
                        _Row(
                          label: s.defaultSaveDir,
                          value: settings.defaultSaveDir,
                          valueEllipsis: true,
                          onTap: () => _editSaveDir(context),
                        ),
                        _Row(
                          label: s.defaultThreads,
                          value: settings.defaultSegments == 0
                              ? s.auto
                              : '${settings.defaultSegments}',
                          onTap: () => _selectThreads(context),
                        ),
                        _Row(
                          label: s.maxConcurrent,
                          value: '${settings.maxConcurrentTasks}',
                          onTap: () => _selectConcurrent(context),
                        ),
                        _Row(
                          label: s.speedLimitTitle,
                          value: settings.speedLimitBytes == 0
                              ? s.statusSpeedLimitOff
                              : '${DownloadTask.formatBytes(settings.speedLimitBytes)}/s',
                          onTap: () => _selectSpeedLimit(context),
                        ),
                      ],
                    ),
                    _GroupLabel(s.settingsCatBt),
                    _Group(
                      children: [
                        _Row(
                          label: s.btEnableDht,
                          trailing: ShadSwitch(
                            value: settings.btEnableDht,
                            onChanged: settings.setBtEnableDht,
                          ),
                        ),
                        _Row(
                          label: s.btEnableUpnp,
                          trailing: ShadSwitch(
                            value: settings.btEnableUpnp,
                            onChanged: settings.setBtEnableUpnp,
                          ),
                        ),
                        _Row(
                          label: s.btListenPort,
                          value:
                              '${settings.btPortStart} – ${settings.btPortEnd}',
                        ),
                        _Row(
                          label: s.btTrackerList,
                          value: s.btTrackerCount(
                            _trackerCount(settings.btCustomTrackers),
                          ),
                          onTap: () => _editTrackers(context),
                        ),
                      ],
                    ),
                    _GroupLabel(s.settingsCatProxy),
                    _Group(
                      children: [
                        _Row(
                          label: s.proxySettings,
                          value: switch (settings.proxyMode) {
                            'system' => s.proxyModeSystem,
                            'manual' => s.proxyModeManual,
                            _ => s.proxyModeNone,
                          },
                          onTap: () => _selectProxyMode(context),
                        ),
                        if (settings.proxyMode == 'manual') ...[
                          _Row(
                            label: s.proxyType,
                            value: settings.proxyType.toUpperCase(),
                            onTap: () => _selectProxyType(context),
                          ),
                          _Row(
                            label: s.proxyHost,
                            value: settings.proxyHost.isEmpty
                                ? '—'
                                : settings.proxyHost,
                            onTap: () => _editProxyHost(context),
                          ),
                          _Row(
                            label: s.proxyPort,
                            value: settings.proxyPort.isEmpty
                                ? '—'
                                : settings.proxyPort,
                            onTap: () => _editProxyPort(context),
                          ),
                        ],
                      ],
                    ),
                    _GroupLabel(s.settingsCatAbout),
                    _Group(
                      children: [
                        _Row(label: s.currentVersion, value: _appVersion),
                        _buildUpdateRow(context),
                        _Row(
                          label: s.mobilePrivacyPolicy,
                          onTap: () => launchUrl(
                            Uri.parse('https://fluxdown.zerx.dev/privacy'),
                          ),
                        ),
                        _Row(
                          label: s.mobileOpenSource,
                          onTap: () => launchUrl(
                            Uri.parse('https://github.com/zerx-lab'),
                          ),
                        ),
                      ],
                    ),
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 22),
                      child: Text(
                        s.mobileFooter,
                        textAlign: TextAlign.center,
                        style: TextStyle(fontSize: 11.5, color: c.textMuted),
                      ),
                    ),
                  ],
                );
              },
            ),
          ),
          // 顶栏
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            child: ClipRect(
              child: BackdropFilter(
                filter: mobileBlurFilter,
                child: Container(
                  color: c.bg.withValues(alpha: 0.72),
                  padding: EdgeInsets.only(top: topInset),
                  child: SizedBox(
                    height: m.mobileAppBarHeight,
                    child: Row(
                      children: [
                        const SizedBox(width: 8),
                        MobileIconButton(
                          icon: LucideIcons.arrowLeft,
                          onTap: () => Navigator.of(context).maybePop(),
                        ),
                        const SizedBox(width: 4),
                        Expanded(
                          child: Text(
                            s.settings,
                            style: TextStyle(
                              fontSize: 19,
                              fontWeight: FontWeight.w700,
                              color: c.textPrimary,
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }

  int _trackerCount(String trackers) =>
      trackers.split('\n').where((l) => l.trim().isNotEmpty).length;

  /// 检查更新行：按 UpdateService 状态展示并派发对应动作。
  Widget _buildUpdateRow(BuildContext context) {
    final s = LocaleScope.of(context);
    final svc = UpdateService.instance;
    final (value, onTap) = switch (svc.status) {
      UpdateStatus.checking => (s.checking, null),
      UpdateStatus.available => (
        s.newVersionFound(svc.checkResult?.latestVersion ?? ''),
        svc.downloadUpdate,
      ),
      UpdateStatus.downloading => (
        _downloadPercent(svc.progress),
        null,
      ),
      UpdateStatus.readyToInstall => (s.installAndRestart, svc.installUpdate),
      UpdateStatus.upToDate => (s.upToDate, svc.checkForUpdate),
      UpdateStatus.error => (svc.errorMessage, svc.checkForUpdate),
      UpdateStatus.idle => (null, svc.checkForUpdate),
    };
    return _Row(
      label: s.checkUpdate,
      value: value,
      valueEllipsis: true,
      onTap: onTap,
    );
  }

  static String _downloadPercent(UpdateDownloadProgress? p) {
    if (p == null || p.totalBytes <= 0) return '…';
    final pct = (p.downloadedBytes * 100 / p.totalBytes).clamp(0, 100);
    return '${pct.toStringAsFixed(0)}%';
  }

  // ── 选择弹层 ──

  void _selectLanguage(BuildContext context) {
    final s = LocaleScope.of(context);
    _showSelectSheet<String>(
      context,
      title: s.language,
      current: localeNotifier.preference,
      options: [
        (kLocaleSystem, s.languageSystem),
        (kLocaleZh, s.languageChinese),
        (kLocaleEn, s.languageEnglish),
      ],
      onSelect: localeNotifier.setLocale,
    );
  }

  void _selectThemeMode(BuildContext context) {
    final s = LocaleScope.of(context);
    _showSelectSheet<ThemeMode>(
      context,
      title: s.themeMode,
      current: themeProvider.themeMode,
      options: [
        (ThemeMode.system, s.themeModeSystem),
        (ThemeMode.light, s.themeModeLight),
        (ThemeMode.dark, s.themeModeDark),
      ],
      onSelect: themeProvider.setThemeMode,
    );
  }

  void _selectThreads(BuildContext context) {
    final s = LocaleScope.of(context);
    _showSelectSheet<int>(
      context,
      title: s.defaultThreads,
      current: settings.defaultSegments,
      options: [
        (0, s.auto),
        for (final n in const [4, 8, 16, 32, 64]) (n, '$n'),
      ],
      onSelect: settings.setDefaultSegments,
    );
  }

  void _selectConcurrent(BuildContext context) {
    final s = LocaleScope.of(context);
    _showSelectSheet<int>(
      context,
      title: s.maxConcurrent,
      current: settings.maxConcurrentTasks,
      options: [
        for (final n in const [1, 2, 3, 5, 8, 10]) (n, '$n'),
      ],
      onSelect: settings.setMaxConcurrentTasks,
    );
  }

  void _selectSpeedLimit(BuildContext context) {
    final s = LocaleScope.of(context);
    const mb = 1024 * 1024;
    _showSelectSheet<int>(
      context,
      title: s.speedLimitTitle,
      current: settings.speedLimitBytes,
      options: [
        (0, s.statusSpeedLimitOff),
        (512 * 1024, '512 KB/s'),
        (mb, '1 MB/s'),
        (2 * mb, '2 MB/s'),
        (5 * mb, '5 MB/s'),
        (10 * mb, '10 MB/s'),
        (20 * mb, '20 MB/s'),
      ],
      onSelect: settings.setSpeedLimitBytes,
    );
  }

  void _selectProxyMode(BuildContext context) {
    final s = LocaleScope.of(context);
    _showSelectSheet<String>(
      context,
      title: s.proxySettings,
      current: settings.proxyMode,
      options: [
        ('none', s.proxyModeNone),
        ('system', s.proxyModeSystem),
        ('manual', s.proxyModeManual),
      ],
      onSelect: settings.setProxyMode,
    );
  }

  void _selectProxyType(BuildContext context) {
    final s = LocaleScope.of(context);
    _showSelectSheet<String>(
      context,
      title: s.proxyType,
      current: settings.proxyType,
      options: const [
        ('http', 'HTTP'),
        ('https', 'HTTPS'),
        ('socks4', 'SOCKS4'),
        ('socks5', 'SOCKS5'),
      ],
      onSelect: settings.setProxyType,
    );
  }

  // ── 输入弹层 ──

  Future<void> _editSaveDir(BuildContext context) async {
    // Android：调用系统文件管理器选择目录（SAF）；其他平台退回文本输入
    if (MobileStorageService.supported) {
      final picked = await pickMobileDownloadDirectory(context);
      if (picked != null && picked.trim().isNotEmpty) {
        settings.setDefaultSaveDir(picked.trim());
      }
      return;
    }
    if (!context.mounted) return;
    final s = LocaleScope.of(context);
    await _showInputSheet(
      context,
      title: s.defaultSaveDir,
      initial: settings.defaultSaveDir,
      onSave: (v) {
        if (v.trim().isNotEmpty) settings.setDefaultSaveDir(v.trim());
      },
    );
  }

  void _editTrackers(BuildContext context) {
    final s = LocaleScope.of(context);
    _showInputSheet(
      context,
      title: s.btTrackerList,
      initial: settings.btCustomTrackers,
      placeholder: s.btTrackerPlaceholder,
      maxLines: 6,
      onSave: settings.setBtCustomTrackers,
    );
  }

  void _editProxyHost(BuildContext context) {
    final s = LocaleScope.of(context);
    _showInputSheet(
      context,
      title: s.proxyHost,
      initial: settings.proxyHost,
      placeholder: s.proxyHostPlaceholder,
      onSave: settings.setProxyHost,
    );
  }

  void _editProxyPort(BuildContext context) {
    final s = LocaleScope.of(context);
    _showInputSheet(
      context,
      title: s.proxyPort,
      initial: settings.proxyPort,
      placeholder: s.proxyPortPlaceholder,
      onSave: settings.setProxyPort,
    );
  }
}

// ─────────────────────────────────────────────
// 通用弹层
// ─────────────────────────────────────────────

Future<void> _showSelectSheet<T>(
  BuildContext context, {
  required String title,
  required T current,
  required List<(T, String)> options,
  required ValueChanged<T> onSelect,
}) {
  return showMobileSheet<void>(
    context,
    builder: (ctx) {
      final c = AppColors.of(ctx);
      final m = AppMetrics.of(ctx);

      Widget row((T, String) option) {
        final (value, label) = option;
        final selected = value == current;
        return GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () {
            Navigator.of(ctx).pop();
            if (value != current) onSelect(value);
          },
          child: Container(
            height: 48,
            padding: const EdgeInsets.symmetric(horizontal: 14),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 14.5,
                      fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                      color: c.textPrimary,
                    ),
                  ),
                ),
                if (selected)
                  Icon(LucideIcons.check, size: 17, color: c.accent),
              ],
            ),
          ),
        );
      }

      return MobileSheetContainer(
        title: title,
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Container(
            decoration: BoxDecoration(
              color: m.glass(c.surface1),
              borderRadius: m.brMobileCard,
              border: Border.all(color: c.border),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                for (var i = 0; i < options.length; i++) ...[
                  if (i > 0)
                    Padding(
                      padding: const EdgeInsets.only(left: 14),
                      child: Container(height: 1, color: c.border),
                    ),
                  row(options[i]),
                ],
              ],
            ),
          ),
        ),
      );
    },
  );
}

Future<void> _showInputSheet(
  BuildContext context, {
  required String title,
  required String initial,
  String? placeholder,
  int maxLines = 1,
  required ValueChanged<String> onSave,
}) {
  final controller = TextEditingController(text: initial);
  return showMobileSheet<void>(
    context,
    builder: (ctx) {
      final s = LocaleScope.of(ctx);
      return MobileSheetContainer(
        title: title,
        footer: MobilePrimaryButton(
          label: s.confirm,
          onTap: () {
            onSave(controller.text);
            Navigator.of(ctx).pop();
          },
        ),
        child: Padding(
          padding: const EdgeInsets.only(top: 8),
          child: MobileTextField(
            controller: controller,
            maxLines: maxLines,
            placeholder: placeholder,
          ),
        ),
      );
    },
  ).whenComplete(controller.dispose);
}

// ─────────────────────────────────────────────
// 分组与行
// ─────────────────────────────────────────────

class _GroupLabel extends StatelessWidget {
  final String text;

  const _GroupLabel(this.text);

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(6, 16, 6, 8),
      child: Text(
        text,
        style: TextStyle(
          fontSize: 12,
          fontWeight: FontWeight.w600,
          color: c.textMuted,
          letterSpacing: 0.5,
        ),
      ),
    );
  }
}

class _Group extends StatelessWidget {
  final List<Widget> children;

  const _Group({required this.children});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final rows = <Widget>[];
    for (var i = 0; i < children.length; i++) {
      rows.add(children[i]);
      if (i != children.length - 1) {
        rows.add(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Container(height: 1, color: c.border),
          ),
        );
      }
    }
    return Container(
      decoration: mobileCardDecoration(c, m),
      child: Column(children: rows),
    );
  }
}

class _Row extends StatelessWidget {
  final String label;
  final String? value;
  final bool valueEllipsis;
  final Widget? trailing;
  final VoidCallback? onTap;

  const _Row({
    required this.label,
    this.value,
    this.valueEllipsis = false,
    this.trailing,
    this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: Container(
        constraints: const BoxConstraints(minHeight: 50),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 13),
        child: Row(
          children: [
            Text(label, style: TextStyle(fontSize: 14, color: c.textPrimary)),
            // value 承担全部弹性并右对齐：短文本时把 trailing/chevron 推到
            // 最右侧；长文本按 ellipsis/fade 截断。
            Expanded(
              child: Padding(
                padding: const EdgeInsets.only(left: 10),
                child: Text(
                  value ?? '',
                  maxLines: 1,
                  textAlign: TextAlign.right,
                  overflow: valueEllipsis
                      ? TextOverflow.ellipsis
                      : TextOverflow.fade,
                  style: TextStyle(fontSize: 13, color: c.textSecondary),
                ),
              ),
            ),
            if (trailing != null)
              Padding(
                padding: const EdgeInsets.only(left: 10),
                child: trailing!,
              ),
            if (onTap != null) ...[
              const SizedBox(width: 8),
              Icon(LucideIcons.chevronRight, size: 15, color: c.textMuted),
            ],
          ],
        ),
      ),
    );
  }
}
