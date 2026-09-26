import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/download_task.dart';
import '../models/plugin_provider.dart';
import '../models/rss_filter.dart';
import '../models/rss_provider.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'flux_sonner.dart';
import 'overflow_tooltip_text.dart';
import 'rss_item_list.dart' show rssReasonLabel;

/// 抓取间隔下拉的候选（分钟）。覆盖「追更」到「日更聚合」两端，
/// 不给自由输入——间隔配得过小只会白打站点，配得过大不如关掉订阅。
const List<int> kRssIntervalOptions = [10, 30, 60, 120, 360, 720, 1440];

/// 打开订阅管理对话框（三 Tab：基本 / 过滤规则 / 高级）。
Future<void> showRssManagerDialog(
  BuildContext context,
  RssProvider rss,
  DownloadController controller,
  PluginProvider pluginProvider,
  String sourceId,
) {
  // 预览区吃的是已缓存条目；打开时补一次拉取，避免刚启动就打开对话框时
  // 预览区空白（用户会以为规则把一切都过滤了）。
  rss.requestItems(sourceId);
  return showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (_) => RssManagerDialog(
      rss: rss,
      controller: controller,
      pluginProvider: pluginProvider,
      sourceId: sourceId,
    ),
  );
}

class RssManagerDialog extends StatefulWidget {
  final RssProvider rss;
  final DownloadController controller;
  final PluginProvider pluginProvider;
  final String sourceId;

  const RssManagerDialog({
    super.key,
    required this.rss,
    required this.controller,
    required this.pluginProvider,
    required this.sourceId,
  });

  @override
  State<RssManagerDialog> createState() => _RssManagerDialogState();
}

class _RssManagerDialogState extends State<RssManagerDialog> {
  int _tab = 0;

  final _nameCtrl = TextEditingController();
  final _urlCtrl = TextEditingController();
  final _saveDirCtrl = TextEditingController();
  final _includeCtrl = TextEditingController();
  final _excludeCtrl = TextEditingController();
  final _sizeMinCtrl = TextEditingController();
  final _sizeMaxCtrl = TextEditingController();
  final _cookiesCtrl = TextEditingController();
  final _uaCtrl = TextEditingController();
  final _proxyCtrl = TextEditingController();
  final _maxPerFetchCtrl = TextEditingController();

  String _queueId = kMainQueueId;
  int _intervalMinutes = 30;
  bool _enabled = true;
  bool _autoDownload = true;
  bool _startPaused = false;
  bool _useRegex = false;
  bool _smartEpisode = false;
  bool _sendReferer = true;
  bool _notifyOnDownload = true;
  bool _isPickingSaveDir = false;
  String _providerId = 'rss';
  String _providerConfig = '';

  @override
  void initState() {
    super.initState();
    final source = _source;
    if (source == null) return;
    _nameCtrl.text = source.name;
    _providerId = source.providerId.isEmpty ? 'rss' : source.providerId;
    _providerConfig = source.providerConfig;
    _urlCtrl.text = source.url;
    _saveDirCtrl.text = source.saveDir;
    _includeCtrl.text = source.includePattern;
    _excludeCtrl.text = source.excludePattern;
    _sizeMinCtrl.text = formatRssSize(source.sizeMinBytes);
    _sizeMaxCtrl.text = formatRssSize(source.sizeMaxBytes);
    _cookiesCtrl.text = source.cookies;
    _uaCtrl.text = source.userAgent;
    _proxyCtrl.text = source.proxyUrl;
    _maxPerFetchCtrl.text = source.maxPerFetch.toString();
    _queueId = source.queueId.isEmpty ? kMainQueueId : source.queueId;
    _intervalMinutes = kRssIntervalOptions.contains(source.intervalMinutes)
        ? source.intervalMinutes
        : 30;
    _enabled = source.enabled;
    _autoDownload = source.autoDownload;
    _startPaused = source.startPaused;
    _useRegex = source.useRegex;
    _smartEpisode = source.smartEpisode;
    _sendReferer = source.sendReferer;
    _notifyOnDownload = source.notifyOnDownload;
  }

  @override
  void dispose() {
    for (final ctrl in [
      _nameCtrl,
      _urlCtrl,
      _saveDirCtrl,
      _includeCtrl,
      _excludeCtrl,
      _sizeMinCtrl,
      _sizeMaxCtrl,
      _cookiesCtrl,
      _uaCtrl,
      _proxyCtrl,
      _maxPerFetchCtrl,
    ]) {
      ctrl.dispose();
    }
    super.dispose();
  }

  RssSourceEntry? get _source {
    for (final s in widget.rss.sources) {
      if (s.sourceId == widget.sourceId) return s;
    }
    return null;
  }

  Future<void> _pickSaveDir() async {
    if (_isPickingSaveDir) return;
    setState(() => _isPickingSaveDir = true);
    try {
      final result = await FilePickerService.pickDirectory(
        dialogTitle: currentS.selectSaveDir,
        initialDirectory: _saveDirCtrl.text.trim().isNotEmpty
            ? _saveDirCtrl.text.trim()
            : null,
      );
      if (result != null && mounted) {
        setState(() => _saveDirCtrl.text = result);
      }
    } on FilePickerException catch (e) {
      if (!mounted) return;
      final s = currentS;
      final message = switch (e.reason) {
        FilePickerFailReason.timeout => s.filePickerErrorTimeout,
        FilePickerFailReason.noDialogTool => s.filePickerErrorNoTool,
        FilePickerFailReason.comInitFailed => s.filePickerErrorNative,
        FilePickerFailReason.nativeDialogFailed => s.filePickerErrorNative,
        FilePickerFailReason.unknown => s.filePickerErrorGeneric,
      };
      FluxSonner.of(context).show(ShadToast.destructive(title: Text(message)));
    } finally {
      if (mounted) setState(() => _isPickingSaveDir = false);
    }
  }

  void _save() {
    final source = _source;
    if (source == null) {
      Navigator.of(context).pop();
      return;
    }
    final url = _urlCtrl.text.trim();
    if (url.isEmpty) {
      setState(() => _tab = 0);
      return;
    }
    widget.rss.update(
      RssSourceEntry(
        sourceId: source.sourceId,
        providerId: _providerId,
        providerConfig: _providerConfig,
        url: url,
        name: _nameCtrl.text.trim(),
        enabled: _enabled,
        autoDownload: _autoDownload,
        startPaused: _startPaused,
        queueId: _queueId,
        saveDir: _saveDirCtrl.text.trim(),
        intervalMinutes: _intervalMinutes,
        includePattern: _includeCtrl.text.trim(),
        excludePattern: _excludeCtrl.text.trim(),
        useRegex: _useRegex,
        smartEpisode: _smartEpisode,
        sizeMinBytes: parseRssSize(_sizeMinCtrl.text) ?? 0,
        sizeMaxBytes: parseRssSize(_sizeMaxCtrl.text) ?? 0,
        sendReferer: _sendReferer,
        notifyOnDownload: _notifyOnDownload,
        maxPerFetch: int.tryParse(_maxPerFetchCtrl.text.trim()) ?? 20,
        cookies: _cookiesCtrl.text.trim(),
        userAgent: _uaCtrl.text.trim(),
        proxyUrl: _proxyCtrl.text.trim(),
        // 运行态字段：引擎侧的 update 只读用户可编辑项，这里原样回传纯粹是为了
        // 满足生成信号结构体的必填约定（rinf 生成的构造器无默认值）。
        lastFetchAt: source.lastFetchAt,
        lastSuccessAt: source.lastSuccessAt,
        lastError: source.lastError,
        failCount: source.failCount,
        seeded: source.seeded,
        position: source.position,
        unreadCount: source.unreadCount,
      ),
    );
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.rss,
      builder: (context, _) {
        final s = LocaleScope.of(context);
        final c = AppColors.of(context);
        final m = AppMetrics.of(context);
        final source = _source;
        if (source == null) return const SizedBox.shrink();
        return ShadDialog(
          title: Text(s.rssManageTitle),
          description: Row(
            children: [
              Expanded(
                child: Text(
                  rssDisplayName(source),
                  overflow: TextOverflow.ellipsis,
                  maxLines: 1,
                ),
              ),
              const SizedBox(width: 8),
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(
                  color: c.surface2,
                  borderRadius: m.brXs,
                  border: Border.all(color: c.border),
                ),
                child: Text(
                  source.providerId == 'rss' ? 'RSS' : source.providerId,
                  style: TextStyle(fontSize: 10, color: c.textMuted),
                ),
              ),
            ],
          ),
          actions: [
            ShadButton.outline(
              onPressed: () {
                widget.rss.refresh(source.sourceId);
                Navigator.of(context).pop();
              },
              child: Text(s.rssRefreshNow),
            ),
            ShadButton.outline(
              onPressed: () => Navigator.of(context).pop(),
              child: Text(s.cancel),
            ),
            ShadButton(onPressed: _save, child: Text(s.confirm)),
          ],
          child: SizedBox(
            width: 560,
            child: Padding(
              padding: const EdgeInsets.symmetric(vertical: 12),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  _buildTabBar(s, c, m),
                  const SizedBox(height: 14),
                  switch (_tab) {
                    0 => _buildBasicTab(s, c),
                    1 => _buildFilterTab(s, c, m),
                    _ => _buildAdvancedTab(s, c),
                  },
                ],
              ),
            ),
          ),
        );
      },
    );
  }

  Widget _buildTabBar(S s, AppColors c, AppMetrics m) {
    final labels = [s.rssTabBasic, s.rssTabFilter, s.rssTabAdvanced];
    return Row(
      children: [
        for (var i = 0; i < labels.length; i++) ...[
          if (i > 0) const SizedBox(width: 4),
          GestureDetector(
            onTap: () => setState(() => _tab = i),
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: AnimatedContainer(
                duration: const Duration(milliseconds: 120),
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 6,
                ),
                decoration: BoxDecoration(
                  color: _tab == i
                      ? c.accentBg
                      : c.accentBg.withValues(alpha: 0),
                  borderRadius: m.brMd,
                ),
                child: Text(
                  labels[i],
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: _tab == i ? FontWeight.w500 : FontWeight.normal,
                    color: _tab == i ? c.accent : c.textSecondary,
                  ),
                ),
              ),
            ),
          ),
        ],
      ],
    );
  }

  Widget _fieldLabel(String text, AppColors c) => Text(
    text,
    style: TextStyle(
      fontSize: 11.5,
      fontWeight: FontWeight.w500,
      color: c.textSecondary,
    ),
  );

  Widget _switchRow(
    AppColors c, {
    required String title,
    required String desc,
    required bool value,
    required ValueChanged<bool> onChanged,
  }) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 6),
    child: Row(
      children: [
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                title,
                style: TextStyle(fontSize: 12.5, color: c.textPrimary),
              ),
              const SizedBox(height: 2),
              Text(
                desc,
                style: TextStyle(fontSize: 11, color: c.textMuted, height: 1.4),
              ),
            ],
          ),
        ),
        const SizedBox(width: 12),
        ShadSwitch(value: value, onChanged: onChanged),
      ],
    ),
  );

  // ─────────────────────────────────────────────
  // 基本
  // ─────────────────────────────────────────────

  Widget _buildBasicTab(S s, AppColors c) {
    final providerOptions = <String, String>{'rss': 'RSS'};
    for (final plugin in widget.pluginProvider.plugins) {
      if (!plugin.enabled || plugin.loadStatus == 'Failed') continue;
      for (final id in plugin.subscriptionProviderIds) {
        providerOptions.putIfAbsent(id, () => '${plugin.name} · $id');
      }
    }
    providerOptions.putIfAbsent(_providerId, () => _providerId);
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _fieldLabel(s.rssProviderLabel, c),
        const SizedBox(height: 6),
        ShadSelect<String>(
          initialValue: _providerId,
          options: [
            for (final entry in providerOptions.entries)
              ShadOption(value: entry.key, child: Text(entry.value)),
          ],
          selectedOptionBuilder: (ctx, value) =>
              Text(providerOptions[value] ?? value),
          onChanged: (value) {
            if (value == null) return;
            setState(() {
              _providerId = value;
              if (value == 'rss') _providerConfig = '';
            });
          },
        ),
        if (_providerId != 'rss') ...[
          const SizedBox(height: 5),
          Text(
            s.rssProviderPluginHint,
            style: TextStyle(fontSize: 11, color: c.textMuted, height: 1.4),
          ),
        ],
        const SizedBox(height: 12),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssNameLabel, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _nameCtrl,
                    placeholder: Text(s.rssNameHint),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            SizedBox(
              width: 150,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssIntervalLabel, c),
                  const SizedBox(height: 6),
                  ShadSelect<int>(
                    initialValue: _intervalMinutes,
                    options: [
                      for (final v in kRssIntervalOptions)
                        ShadOption(value: v, child: Text(_intervalLabel(s, v))),
                    ],
                    selectedOptionBuilder: (ctx, v) =>
                        Text(_intervalLabel(s, v)),
                    onChanged: (v) {
                      if (v != null) setState(() => _intervalMinutes = v);
                    },
                  ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 12),
        _fieldLabel(s.rssUrlLabel, c),
        const SizedBox(height: 6),
        ShadInput(controller: _urlCtrl, placeholder: Text(s.rssUrlHint)),
        const SizedBox(height: 12),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SizedBox(
              width: 170,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssQueueLabel, c),
                  const SizedBox(height: 6),
                  _queueSelect(s),
                ],
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssSaveDirLabel, c),
                  const SizedBox(height: 6),
                  Row(
                    children: [
                      Expanded(
                        child: ShadInput(
                          controller: _saveDirCtrl,
                          placeholder: Text(s.rssSaveDirHint),
                        ),
                      ),
                      const SizedBox(width: 4),
                      Tooltip(
                        message: s.selectSaveDir,
                        child: ShadButton.ghost(
                          onPressed: _isPickingSaveDir ? null : _pickSaveDir,
                          size: ShadButtonSize.sm,
                          width: 32,
                          height: 32,
                          padding: EdgeInsets.zero,
                          child: Icon(LucideIcons.folderOpen, size: 15),
                        ),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        _switchRow(
          c,
          title: s.rssEnabledLabel,
          desc: s.rssEnabledDesc,
          value: _enabled,
          onChanged: (v) => setState(() => _enabled = v),
        ),
        _switchRow(
          c,
          title: s.rssAutoDownloadLabel,
          desc: s.rssAutoDownloadDesc,
          value: _autoDownload,
          onChanged: (v) => setState(() => _autoDownload = v),
        ),
        _switchRow(
          c,
          title: s.rssStartPausedLabel,
          desc: s.rssStartPausedDesc,
          value: _startPaused,
          onChanged: (v) => setState(() => _startPaused = v),
        ),
      ],
    );
  }

  Widget _queueSelect(S s) {
    final queues = widget.controller.queues;
    return ShadSelect<String>(
      initialValue: _queueId,
      options: [
        for (final q in queues)
          ShadOption(value: q.queueId, child: Text(queueDisplayName(s, q))),
      ],
      selectedOptionBuilder: (ctx, v) {
        for (final q in queues) {
          if (q.queueId == v) return Text(queueDisplayName(s, q));
        }
        return Text(s.mainQueue);
      },
      onChanged: (v) {
        if (v != null) setState(() => _queueId = v);
      },
    );
  }

  String _intervalLabel(S s, int minutes) => minutes < 60
      ? s.rssEveryMinutes(minutes)
      : s.rssEveryHours(minutes ~/ 60);

  // ─────────────────────────────────────────────
  // 过滤规则（下半屏实时预览 = 本功能相对 qBittorrent 的核心差异化）
  // ─────────────────────────────────────────────

  Widget _buildFilterTab(S s, AppColors c, AppMetrics m) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssIncludeLabel, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _includeCtrl,
                    placeholder: Text(s.rssIncludeHint),
                    onChanged: (_) => setState(() {}),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssExcludeLabel, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _excludeCtrl,
                    placeholder: Text(s.rssExcludeHint),
                    onChanged: (_) => setState(() {}),
                  ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 12),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssSizeMinLabel, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _sizeMinCtrl,
                    placeholder: const Text('200M'),
                    onChanged: (_) => setState(() {}),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssSizeMaxLabel, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _sizeMaxCtrl,
                    placeholder: const Text('2G'),
                    onChanged: (_) => setState(() {}),
                  ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        _switchRow(
          c,
          title: s.rssUseRegexLabel,
          desc: s.rssUseRegexDesc,
          value: _useRegex,
          onChanged: (v) => setState(() => _useRegex = v),
        ),
        _switchRow(
          c,
          title: s.rssSmartEpisodeLabel,
          desc: s.rssSmartEpisodeDesc,
          value: _smartEpisode,
          onChanged: (v) => setState(() => _smartEpisode = v),
        ),
        const SizedBox(height: 10),
        _buildPreview(s, c, m),
      ],
    );
  }

  Widget _buildPreview(S s, AppColors c, AppMetrics m) {
    final items = widget.rss.itemsOf(widget.sourceId);
    final rule = RssFilterRule(
      include: _includeCtrl.text,
      exclude: _excludeCtrl.text,
      useRegex: _useRegex,
      smartEpisode: _smartEpisode,
      sizeMinBytes: parseRssSize(_sizeMinCtrl.text) ?? 0,
      sizeMaxBytes: parseRssSize(_sizeMaxCtrl.text) ?? 0,
    );
    final verdicts = evaluateRssBatch(items, rule);
    final hits = verdicts.where((v) => v.accepted).length;
    return Container(
      decoration: BoxDecoration(
        border: Border.all(color: c.border),
        borderRadius: m.brMd,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
            decoration: BoxDecoration(
              color: c.surface2,
              border: Border(bottom: BorderSide(color: c.border)),
            ),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    s.rssPreviewHeader(items.length),
                    style: TextStyle(fontSize: 11, color: c.textSecondary),
                  ),
                ),
                Text(
                  s.rssPreviewSummary(hits, items.length - hits),
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
              ],
            ),
          ),
          SizedBox(
            height: 168,
            child: items.isEmpty
                ? Center(
                    child: Text(
                      s.rssPreviewEmpty,
                      style: TextStyle(fontSize: 11.5, color: c.textMuted),
                    ),
                  )
                : ListView.builder(
                    padding: EdgeInsets.zero,
                    itemCount: items.length,
                    itemBuilder: (ctx, i) =>
                        _previewRow(s, c, m, items[i], verdicts[i]),
                  ),
          ),
        ],
      ),
    );
  }

  Widget _previewRow(
    S s,
    AppColors c,
    AppMetrics m,
    RssItemEntry item,
    RssVerdict verdict,
  ) {
    final hit = verdict.accepted;
    final color = hit ? AppColors.green : c.textMuted;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
      child: Row(
        children: [
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
            decoration: BoxDecoration(
              color: m.soft(color),
              borderRadius: m.brXs,
            ),
            child: Text(
              hit ? s.rssPreviewWillDownload : s.rssPreviewFiltered,
              style: TextStyle(fontSize: 10, color: color),
            ),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: OverflowTooltipText(
              item.title,
              style: TextStyle(
                fontSize: 11.5,
                color: hit ? c.textPrimary : c.textMuted,
              ),
            ),
          ),
          const SizedBox(width: 8),
          Text(
            hit
                ? (item.enclosureLength > 0
                      ? DownloadTask.formatBytes(item.enclosureLength)
                      : '')
                : rssReasonLabel(s, verdict.reason ?? ''),
            style: TextStyle(fontSize: 10.5, color: c.textMuted),
          ),
        ],
      ),
    );
  }

  // ─────────────────────────────────────────────
  // 高级
  // ─────────────────────────────────────────────

  Widget _buildAdvancedTab(S s, AppColors c) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _fieldLabel(s.rssCookiesLabel, c),
        const SizedBox(height: 6),
        ShadInput(
          controller: _cookiesCtrl,
          maxLines: 2,
          placeholder: Text(s.rssCookiesHint),
        ),
        const SizedBox(height: 12),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssUserAgentLabel, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _uaCtrl,
                    placeholder: Text(s.rssInheritGlobalHint),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.rssProxyLabel, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _proxyCtrl,
                    placeholder: const Text('socks5://127.0.0.1:1080'),
                  ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 12),
        _fieldLabel(s.rssMaxPerFetchLabel, c),
        const SizedBox(height: 6),
        SizedBox(
          width: 120,
          child: ShadInput(
            controller: _maxPerFetchCtrl,
            keyboardType: TextInputType.number,
          ),
        ),
        const SizedBox(height: 8),
        _switchRow(
          c,
          title: s.rssSendRefererLabel,
          desc: s.rssSendRefererDesc,
          value: _sendReferer,
          onChanged: (v) => setState(() => _sendReferer = v),
        ),
        _switchRow(
          c,
          title: s.rssNotifyLabel,
          desc: s.rssNotifyDesc,
          value: _notifyOnDownload,
          onChanged: (v) => setState(() => _notifyOnDownload = v),
        ),
      ],
    );
  }
}
