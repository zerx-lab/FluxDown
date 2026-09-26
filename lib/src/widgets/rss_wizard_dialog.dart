import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/download_task.dart';
import '../models/plugin_provider.dart';
import '../models/rss_provider.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'flux_sonner.dart';

/// 新建订阅向导：**一个输入框 + 一次验证 + 两个选项**，30 秒完成（设计文档 P1）。
///
/// 过滤规则刻意不出现在首次流程里——Mikan/动漫花园这类聚合 feed 本身就是用户
/// 在源站筛过的，默认零规则全量下才是对的路径（AutoBangumi 验证过）；要规则的
/// 用户建完再进「管理订阅 · 过滤规则」。
Future<void> showRssWizardDialog(
  BuildContext context,
  RssProvider rss,
  DownloadController controller,
  PluginProvider pluginProvider,
) {
  return showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (_) => RssWizardDialog(
      rss: rss,
      controller: controller,
      pluginProvider: pluginProvider,
    ),
  );
}

class RssWizardDialog extends StatefulWidget {
  final RssProvider rss;
  final DownloadController controller;
  final PluginProvider pluginProvider;

  const RssWizardDialog({
    super.key,
    required this.rss,
    required this.controller,
    required this.pluginProvider,
  });

  @override
  State<RssWizardDialog> createState() => _RssWizardDialogState();
}

class _RssWizardDialogState extends State<RssWizardDialog> {
  final _urlCtrl = TextEditingController();
  final _saveDirCtrl = TextEditingController();

  /// 本次向导的验证请求 ID——provider 是全局的，用它区分「是不是我等的那次」。
  late final String _requestId = DateTime.now().microsecondsSinceEpoch
      .toString();

  bool _validating = false;
  bool _isPickingSaveDir = false;
  int _seenValidateSeq = 0;
  RssValidateResult? _result;

  String _queueId = kMainQueueId;
  bool _autoDownload = true;
  String _providerId = 'rss';
  String _providerConfig = '';

  @override
  void initState() {
    super.initState();
    _seenValidateSeq = widget.rss.validateSeq;
    widget.rss.addListener(_onRssChanged);
  }

  @override
  void dispose() {
    widget.rss.removeListener(_onRssChanged);
    _urlCtrl.dispose();
    _saveDirCtrl.dispose();
    super.dispose();
  }

  void _onRssChanged() {
    if (!_validating) return;
    if (widget.rss.validateSeq == _seenValidateSeq) return;
    final r = widget.rss.lastValidateResult;
    _seenValidateSeq = widget.rss.validateSeq;
    if (r == null || r.requestId != _requestId) return;
    setState(() {
      _validating = false;
      _result = r;
    });
  }

  void _validate() {
    if (_providerId != 'rss') return;
    final url = _urlCtrl.text.trim();
    if (url.isEmpty) return;
    setState(() {
      _validating = true;
      _result = null;
    });
    widget.rss.validate(requestId: _requestId, url: url);
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

  void _subscribe() {
    final url = _urlCtrl.text.trim();
    if (url.isEmpty) return;
    widget.rss.create(
      RssSourceEntry(
        // 首次流程刻意只问 URL / 队列 / 目录 / 自动下载四项（设计文档 P1），
        // 其余一律取引擎默认值——rinf 生成的构造器没有默认值，只能在这里写全。
        sourceId: '',
        providerId: _providerId,
        providerConfig: _providerConfig,
        url: url,
        name: _result?.feedTitle ?? '',
        enabled: true,
        autoDownload: _autoDownload,
        startPaused: false,
        queueId: _queueId,
        saveDir: _saveDirCtrl.text.trim(),
        intervalMinutes: 30,
        includePattern: '',
        excludePattern: '',
        useRegex: false,
        smartEpisode: false,
        sizeMinBytes: 0,
        sizeMaxBytes: 0,
        sendReferer: true,
        notifyOnDownload: true,
        maxPerFetch: 20,
        cookies: '',
        userAgent: '',
        proxyUrl: '',
        lastFetchAt: 0,
        lastSuccessAt: 0,
        lastError: '',
        failCount: 0,
        seeded: false,
        position: 0,
        unreadCount: 0,
      ),
    );
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final providerOptions = <String, String>{'rss': 'RSS'};
    for (final plugin in widget.pluginProvider.plugins) {
      if (!plugin.enabled || plugin.loadStatus == 'Failed') continue;
      for (final id in plugin.subscriptionProviderIds) {
        providerOptions.putIfAbsent(id, () => '${plugin.name} · $id');
      }
    }
    final validated = _result != null && _result!.error.isEmpty;
    // 插件 provider 没有「验证」步骤，按钮直接是「订阅」；URL 为空时禁用，
    // 而不是让 _subscribe 静默 return（web 端同场景会报 rss.urlRequired）。
    final urlEmpty = _urlCtrl.text.trim().isEmpty;
    return ShadDialog(
      title: Text(s.rssAddSource),
      description: Text(
        _providerId == 'rss'
            ? (validated ? s.rssWizardStep2 : s.rssWizardStep1)
            : s.rssProviderPluginHint,
      ),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(s.cancel),
        ),
        ShadButton(
          onPressed: _validating || urlEmpty
              ? null
              : _providerId != 'rss' || validated
              ? _subscribe
              : _validate,
          child: Text(
            _providerId != 'rss' || validated
                ? s.rssWizardSubscribe
                : s.rssWizardValidate,
          ),
        ),
      ],
      child: SizedBox(
        width: 520,
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 12),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                s.rssProviderLabel,
                style: TextStyle(
                  fontSize: 11.5,
                  fontWeight: FontWeight.w500,
                  color: c.textSecondary,
                ),
              ),
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
                    _result = null;
                    if (value == 'rss') _providerConfig = '';
                  });
                },
              ),
              const SizedBox(height: 14),
              Text(
                s.rssUrlLabel,
                style: TextStyle(
                  fontSize: 11.5,
                  fontWeight: FontWeight.w500,
                  color: c.textSecondary,
                ),
              ),
              const SizedBox(height: 6),
              ShadInput(
                controller: _urlCtrl,
                placeholder: Text(s.rssUrlHint),
                enabled: !_validating,
                onSubmitted: (_) {
                  if (!_validating && !validated) _validate();
                },
                onChanged: (_) {
                  // 改地址即作废上一次验证结果，避免用旧 feed 标题建新订阅；
                  // 同时刷新按钮的空 URL 禁用态。
                  setState(() => _result = null);
                },
              ),
              const SizedBox(height: 6),
              Text(
                s.rssWizardUrlNote,
                style: TextStyle(fontSize: 11, color: c.textMuted, height: 1.5),
              ),
              if (_validating) ...[
                const SizedBox(height: 14),
                Row(
                  children: [
                    SizedBox(
                      width: 13,
                      height: 13,
                      child: CircularProgressIndicator(
                        strokeWidth: 1.6,
                        color: c.accent,
                      ),
                    ),
                    const SizedBox(width: 8),
                    Text(
                      s.rssWizardValidating,
                      style: TextStyle(fontSize: 12, color: c.textSecondary),
                    ),
                  ],
                ),
              ],
              if (_result != null && _result!.error.isNotEmpty) ...[
                const SizedBox(height: 14),
                Container(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 10,
                    vertical: 8,
                  ),
                  decoration: BoxDecoration(
                    color: m.soft(AppColors.red),
                    borderRadius: m.brMd,
                  ),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Icon(
                        LucideIcons.circleAlert,
                        size: 14,
                        color: AppColors.red,
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          _result!.error,
                          style: TextStyle(
                            fontSize: 11.5,
                            color: AppColors.red,
                            height: 1.4,
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ],
              if (validated) ...[
                const SizedBox(height: 14),
                _buildFeedCard(s, c, m, _result!),
                const SizedBox(height: 12),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SizedBox(
                      width: 170,
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            s.rssQueueLabel,
                            style: TextStyle(
                              fontSize: 11.5,
                              fontWeight: FontWeight.w500,
                              color: c.textSecondary,
                            ),
                          ),
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
                          Text(
                            s.rssSaveDirLabel,
                            style: TextStyle(
                              fontSize: 11.5,
                              fontWeight: FontWeight.w500,
                              color: c.textSecondary,
                            ),
                          ),
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
                                  onPressed: _isPickingSaveDir
                                      ? null
                                      : _pickSaveDir,
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
                const SizedBox(height: 10),
                Row(
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Text(
                            s.rssAutoDownloadLabel,
                            style: TextStyle(
                              fontSize: 12.5,
                              color: c.textPrimary,
                            ),
                          ),
                          const SizedBox(height: 2),
                          Text(
                            s.rssWizardSeedNote,
                            style: TextStyle(
                              fontSize: 11,
                              color: c.textMuted,
                              height: 1.4,
                            ),
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: 12),
                    ShadSwitch(
                      value: _autoDownload,
                      onChanged: (v) => setState(() => _autoDownload = v),
                    ),
                  ],
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildFeedCard(
    S s,
    AppColors c,
    AppMetrics m,
    RssValidateResult result,
  ) {
    final preview = result.items.take(3).toList(growable: false);
    return Container(
      decoration: BoxDecoration(
        border: Border.all(color: c.border),
        borderRadius: m.brMd,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Padding(
            padding: const EdgeInsets.all(10),
            child: Row(
              children: [
                Icon(LucideIcons.circleCheck, size: 15, color: AppColors.green),
                const SizedBox(width: 8),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Text(
                        result.feedTitle.isEmpty
                            ? result.url
                            : result.feedTitle,
                        style: TextStyle(
                          fontSize: 12.5,
                          fontWeight: FontWeight.w600,
                          color: c.textPrimary,
                        ),
                        overflow: TextOverflow.ellipsis,
                      ),
                      const SizedBox(height: 2),
                      Text(
                        s.rssWizardFeedSummary(result.items.length),
                        style: TextStyle(fontSize: 11, color: c.textMuted),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
          if (preview.isNotEmpty)
            Container(
              decoration: BoxDecoration(
                border: Border(top: BorderSide(color: c.border)),
              ),
              child: Column(
                children: [
                  for (final item in preview)
                    Padding(
                      padding: const EdgeInsets.symmetric(
                        horizontal: 10,
                        vertical: 5,
                      ),
                      child: Row(
                        children: [
                          Expanded(
                            child: Text(
                              item.title,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                fontSize: 11.5,
                                color: c.textSecondary,
                              ),
                            ),
                          ),
                          const SizedBox(width: 8),
                          if (item.enclosureLength > 0)
                            Text(
                              DownloadTask.formatBytes(item.enclosureLength),
                              style: TextStyle(
                                fontSize: 10.5,
                                color: c.textMuted,
                              ),
                            ),
                        ],
                      ),
                    ),
                ],
              ),
            ),
        ],
      ),
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
}
