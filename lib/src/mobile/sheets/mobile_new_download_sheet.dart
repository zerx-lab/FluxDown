import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../../bindings/bindings.dart';
import '../../i18n/locale_provider.dart';
import '../../models/download_controller.dart';
import '../../models/settings_provider.dart';
import '../../theme/app_colors.dart';
import '../../theme/app_metrics.dart';
import '../mobile_ui.dart';
import '../services/mobile_storage_service.dart';

/// UA 预设（与桌面新建下载对话框一致）
const _uaPresets = <String, String>{
  'default': '',
  'chrome':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
      '(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36',
  'firefox':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) '
      'Gecko/20100101 Firefox/147.0',
  'netdisk': 'netdisk',
};

/// 新建下载底部弹层
Future<void> showMobileNewDownloadSheet(
  BuildContext context, {
  required DownloadController controller,
  required SettingsProvider settings,
  String initialUrl = '',
}) {
  return showMobileSheet<void>(
    context,
    builder: (ctx) => _NewDownloadSheet(
      controller: controller,
      settings: settings,
      initialUrl: initialUrl,
      rootContext: context,
    ),
  );
}

class _NewDownloadSheet extends StatefulWidget {
  final DownloadController controller;
  final SettingsProvider settings;
  final String initialUrl;

  /// 弹层关闭后仍存活的外层 context（用于 Toast）
  final BuildContext rootContext;

  const _NewDownloadSheet({
    required this.controller,
    required this.settings,
    required this.initialUrl,
    required this.rootContext,
  });

  @override
  State<_NewDownloadSheet> createState() => _NewDownloadSheetState();
}

class _NewDownloadSheetState extends State<_NewDownloadSheet> {
  late final TextEditingController _urlController;
  late final TextEditingController _dirController;
  late final TextEditingController _cookieController;
  late final TextEditingController _checksumController;

  /// 自定义请求头列表（#347），每项含一对 key/value 输入控制器。
  final List<_MobileHeaderRow> _headerRows = [];

  late String _threads; // 'auto' | '4' | '8' | '16' | '32'
  late String _queueId;
  String _uaPreset = 'default';
  bool _advancedOpen = false;

  @override
  void initState() {
    super.initState();
    _urlController = TextEditingController(text: widget.initialUrl);
    _dirController = TextEditingController(
      text: widget.settings.effectiveDefaultSaveDir,
    );
    _cookieController = TextEditingController();
    _checksumController = TextEditingController();
    final last = widget.settings.lastDialogThreads;
    _threads = const {'4', '8', '16', '32'}.contains(last) ? last : 'auto';
    _queueId = widget.settings.defaultQueueId;
    // 默认队列被删除后回退
    if (_queueId.isNotEmpty &&
        !widget.controller.queues.any((q) => q.queueId == _queueId)) {
      _queueId = '';
    }
  }

  @override
  void dispose() {
    _urlController.dispose();
    _dirController.dispose();
    _cookieController.dispose();
    _checksumController.dispose();
    for (final row in _headerRows) {
      row.dispose();
    }
    super.dispose();
  }

  Future<void> _pasteFromClipboard() async {
    final s = LocaleScope.of(context);
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text?.trim() ?? '';
    if (!mounted) return;
    if (text.isEmpty) {
      showMobileToast(context, s.mobileClipboardEmpty);
      return;
    }
    final existing = _urlController.text.trimRight();
    _urlController.text = existing.isEmpty ? text : '$existing\n$text';
    showMobileToast(context, s.mobilePasted);
  }

  /// 调起系统文件管理器选择保存目录
  Future<void> _pickSaveDir() async {
    final picked = await pickMobileDownloadDirectory(context);
    if (picked != null && picked.trim().isNotEmpty && mounted) {
      setState(() => _dirController.text = picked);
    }
  }

  void _start() {
    final s = LocaleScope.of(context);
    final urls = _urlController.text
        .split('\n')
        .map((l) => l.trim())
        .where((l) => l.isNotEmpty)
        .toList();
    if (urls.isEmpty) {
      showMobileToast(context, s.mobileEnterUrl);
      return;
    }

    final saveDir = _dirController.text.trim();
    widget.settings.recordLastSaveDir(saveDir);

    final segments = int.tryParse(_threads) ?? 0;
    widget.settings.setLastDialogThreads(segments > 0 ? _threads : 'auto');

    final userAgent = _uaPresets[_uaPreset] ?? '';
    final cookies = _cookieController.text.trim();
    final checksum = _checksumController.text.trim();
    // 仅保留 key 非空的行；同名 key 后者覆盖前者。
    final extraHeaders = <String, String>{};
    for (final row in _headerRows) {
      final key = row.keyController.text.trim();
      if (key.isEmpty) continue;
      extraHeaders[key] = row.valueController.text.trim();
    }

    if (urls.length == 1) {
      widget.controller.createTask(
        url: urls.first,
        saveDir: saveDir,
        segments: segments,
        cookies: cookies,
        userAgent: userAgent,
        queueId: _queueId,
        checksum: checksum,
        extraHeaders: extraHeaders,
      );
    } else {
      // 批量下载共享目录/线程/UA，校验值仅单任务支持
      widget.controller.batchCreateTask(
        entries: [
          for (final url in urls)
            UrlEntry(url: url, fileName: '', checksum: '', audioUrl: ''),
        ],
        saveDir: saveDir,
        segments: segments,
        userAgent: userAgent,
        queueId: _queueId,
        cookies: cookies,
      );
    }

    Navigator.of(context).pop();
    showMobileToast(widget.rootContext, s.mobileDownloadStarted);
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return MobileSheetContainer(
      title: s.newDownload,
      footer: MobilePrimaryButton(
        label: s.startDownload,
        icon: LucideIcons.download,
        onTap: _start,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          MobileFieldLabel(s.mobileUrlHint),
          MobileTextField(
            controller: _urlController,
            maxLines: 3,
            placeholder: 'https://\nmagnet:?xt=urn:btih:…',
            suffix: GestureDetector(
              onTap: _pasteFromClipboard,
              child: Container(
                padding: const EdgeInsets.symmetric(
                  horizontal: 10,
                  vertical: 5,
                ),
                decoration: BoxDecoration(
                  // 刻意保留：粘贴按钮悬浮于毛玻璃弹层之上，需近不透明底色保证可读，
                  // 属一次性装饰值，非可主题化语义角色。
                  color: c.bg.withValues(alpha: 0.9),
                  borderRadius: m.brPill,
                  border: Border.all(color: c.border),
                ),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(
                      LucideIcons.clipboard,
                      size: 12,
                      color: c.textSecondary,
                    ),
                    const SizedBox(width: 5),
                    Text(
                      s.mobilePaste,
                      style: TextStyle(
                        fontSize: 11.5,
                        fontWeight: FontWeight.w600,
                        color: c.textSecondary,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
          MobileFieldLabel(s.mobileSaveTo),
          if (MobileStorageService.supported)
            _DirPickRow(path: _dirController.text, onTap: _pickSaveDir)
          else
            MobileTextField(
              controller: _dirController,
              placeholder: s.selectSaveDir,
            ),
          MobileFieldLabel(s.threads),
          MobileSegmentedRow(
            options: const ['auto', '4', '8', '16', '32'],
            labels: [s.auto, '4', '8', '16', '32'],
            selected: _threads,
            onSelect: (t) => setState(() => _threads = t),
          ),
          MobileFieldLabel(s.taskQueueLabel),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              MobileChip(
                label: s.defaultQueue,
                selected: _queueId.isEmpty,
                onTap: () => setState(() => _queueId = ''),
              ),
              for (final q in widget.controller.queues)
                MobileChip(
                  label: q.name,
                  selected: _queueId == q.queueId,
                  onTap: () => setState(() => _queueId = q.queueId),
                ),
            ],
          ),
          // 高级选项
          GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: () => setState(() => _advancedOpen = !_advancedOpen),
            child: Padding(
              padding: const EdgeInsets.only(top: 16, bottom: 2),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      s.mobileAdvancedOptions,
                      style: TextStyle(fontSize: 13, color: c.textSecondary),
                    ),
                  ),
                  Icon(
                    _advancedOpen
                        ? LucideIcons.chevronUp
                        : LucideIcons.chevronDown,
                    size: 15,
                    color: c.textSecondary,
                  ),
                ],
              ),
            ),
          ),
          if (_advancedOpen) ...[
            const MobileFieldLabel('User-Agent'),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                MobileChip(
                  label: s.queueUaInheritGlobal,
                  selected: _uaPreset == 'default',
                  onTap: () => setState(() => _uaPreset = 'default'),
                ),
                MobileChip(
                  label: 'Chrome',
                  selected: _uaPreset == 'chrome',
                  onTap: () => setState(() => _uaPreset = 'chrome'),
                ),
                MobileChip(
                  label: 'Firefox',
                  selected: _uaPreset == 'firefox',
                  onTap: () => setState(() => _uaPreset = 'firefox'),
                ),
                MobileChip(
                  label: s.userAgentPresetNetdisk,
                  selected: _uaPreset == 'netdisk',
                  onTap: () => setState(() => _uaPreset = 'netdisk'),
                ),
              ],
            ),
            MobileFieldLabel(s.taskCookie),
            MobileTextField(
              controller: _cookieController,
              maxLines: 2,
              placeholder: s.taskCookiePlaceholder,
            ),
            MobileFieldLabel(s.taskChecksum),
            MobileTextField(
              controller: _checksumController,
              placeholder: 'sha256=e3b0c44298fc1c…',
            ),
            MobileFieldLabel(s.taskHeaders),
            for (int hi = 0; hi < _headerRows.length; hi++) ...[
              if (hi > 0) const SizedBox(height: 8),
              Row(
                children: [
                  Expanded(
                    flex: 2,
                    child: MobileTextField(
                      controller: _headerRows[hi].keyController,
                      placeholder: s.taskHeadersKeyPlaceholder,
                      dense: true,
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    flex: 3,
                    child: MobileTextField(
                      controller: _headerRows[hi].valueController,
                      placeholder: s.taskHeadersValuePlaceholder,
                      dense: true,
                    ),
                  ),
                  GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () =>
                        setState(() => _headerRows.removeAt(hi).dispose()),
                    child: Padding(
                      padding: const EdgeInsets.all(8),
                      child: Icon(LucideIcons.x, size: 16, color: c.textMuted),
                    ),
                  ),
                ],
              ),
            ],
            GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: () => setState(() => _headerRows.add(_MobileHeaderRow())),
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: 6),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(LucideIcons.plus, size: 14, color: c.accent),
                    const SizedBox(width: 6),
                    Text(
                      s.taskHeadersAdd,
                      style: TextStyle(
                        fontSize: 12.5,
                        fontWeight: FontWeight.w600,
                        color: c.accent,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

/// 保存目录行：文件夹图标 + 路径 + 箭头，点按调起系统目录选择器
class _DirPickRow extends StatelessWidget {
  final String path;
  final VoidCallback onTap;

  const _DirPickRow({required this.path, required this.onTap});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
        decoration: BoxDecoration(
          color: c.surface1,
          borderRadius: m.brChipLg,
          border: Border.all(color: c.border),
        ),
        child: Row(
          children: [
            Icon(LucideIcons.folderOpen, size: 17, color: c.textSecondary),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                path,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 13, color: c.textPrimary),
              ),
            ),
            const SizedBox(width: 8),
            Icon(LucideIcons.chevronRight, size: 15, color: c.textMuted),
          ],
        ),
      ),
    );
  }
}

/// 自定义请求头的一行输入：持有 key / value 两个文本控制器（#347）。
class _MobileHeaderRow {
  final TextEditingController keyController = TextEditingController();
  final TextEditingController valueController = TextEditingController();

  void dispose() {
    keyController.dispose();
    valueController.dispose();
  }
}
