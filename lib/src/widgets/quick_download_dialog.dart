import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart'
    show
        Colors,
        DefaultMaterialLocalizations,
        InputDecoration,
        Material,
        MaterialType,
        OutlineInputBorder,
        TextField,
        TextSelectionTheme,
        TextSelectionThemeData;
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import 'dir_picker_field.dart';

/// 浏览器扩展下载请求的快速确认对话框。
///
/// 在主窗口内以 Dialog 形式弹出，无需创建独立子窗口，
/// 延迟从 3-4 秒降低到 <100ms。
///
/// 支持多行 URL 输入，批量下载时使用 [BatchCreateTask] 信号，
/// 单条下载时使用 [ConfirmExternalDownload] 信号。
void showQuickDownloadDialog(
  BuildContext context, {
  required String url,
  required String filename,
  required int fileSize,
  required String mimeType,
  required String cookies,
  required String defaultSaveDir,
}) {
  showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (context) => _QuickDownloadDialogContent(
      url: url,
      filename: filename,
      fileSize: fileSize,
      mimeType: mimeType,
      cookies: cookies,
      defaultSaveDir: defaultSaveDir,
    ),
  );
}

class _QuickDownloadDialogContent extends StatefulWidget {
  final String url;
  final String filename;
  final int fileSize;
  final String mimeType;
  final String cookies;
  final String defaultSaveDir;

  const _QuickDownloadDialogContent({
    required this.url,
    required this.filename,
    required this.fileSize,
    required this.mimeType,
    required this.cookies,
    required this.defaultSaveDir,
  });

  @override
  State<_QuickDownloadDialogContent> createState() =>
      _QuickDownloadDialogContentState();
}

class _QuickDownloadDialogContentState
    extends State<_QuickDownloadDialogContent> {
  final _urlController = TextEditingController();
  final _urlFocusNode = FocusNode();
  final _saveDirController = TextEditingController();
  final _renameController = TextEditingController();
  final _proxyUrlController = TextEditingController();
  String? selectedThreads;

  /// 是否展开高级选项（含任务代理）
  bool _showAdvanced = false;

  /// 解析出的有效 URL 数量（实时计算）
  int _urlCount = 0;

  /// 防止重复打开文件选择器
  bool _isPicking = false;

  @override
  void initState() {
    super.initState();
    _urlController.text = widget.url;
    _saveDirController.text = widget.defaultSaveDir;
    if (widget.filename.isNotEmpty) {
      _renameController.text = widget.filename;
    }
    _urlController.addListener(_onUrlChanged);
    // 初始化时计算一次
    _onUrlChanged();
  }

  void _onUrlChanged() {
    final urls = _parseUrls(_urlController.text);
    final count = urls.length;
    if (count != _urlCount) {
      setState(() {
        _urlCount = count;
      });
    }
  }

  /// 从文本中解析所有有效的 URL（http/https/ftp/magnet）
  static List<String> _parseUrls(String text) {
    final lines = text.split('\n');
    final urls = <String>[];
    final urlPattern = RegExp(r'^(https?|ftp)://\S+', caseSensitive: false);
    for (final line in lines) {
      final trimmed = line.trim();
      if (trimmed.isEmpty) continue;
      if (trimmed.toLowerCase().startsWith('magnet:?')) {
        urls.add(trimmed);
      } else {
        final match = urlPattern.firstMatch(trimmed);
        if (match != null) {
          urls.add(match.group(0)!);
        }
      }
    }
    return urls;
  }

  @override
  void dispose() {
    _urlController.removeListener(_onUrlChanged);
    _urlController.dispose();
    _urlFocusNode.dispose();
    _saveDirController.dispose();
    _renameController.dispose();
    _proxyUrlController.dispose();
    super.dispose();
  }

  Future<void> _pickSaveDir() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await FilePicker.platform.getDirectoryPath(
        dialogTitle: currentS.selectSaveDir,
        lockParentWindow: true,
        initialDirectory: _saveDirController.text.trim().isNotEmpty
            ? _saveDirController.text.trim()
            : null,
      );
      if (result != null && mounted) {
        _saveDirController.text = result;
      }
    } finally {
      if (mounted) setState(() => _isPicking = false);
    }
  }

  bool get _isBatch => _urlCount > 1;

  void _startDownload() {
    final saveDir = _saveDirController.text.trim();
    if (saveDir.isEmpty) return;

    final urls = _parseUrls(_urlController.text);
    if (urls.isEmpty) return;

    final proxyUrl = _proxyUrlController.text.trim();

    final segments = switch (selectedThreads) {
      'auto' => 0,
      '4' => 4,
      '8' => 8,
      '16' => 16,
      '32' => 32,
      '64' => 64,
      _ => 0,
    };

    if (urls.length == 1) {
      // 单条 — 使用 ConfirmExternalDownload，支持重命名和 cookies
      final rename = _renameController.text.trim();
      ConfirmExternalDownload(
        url: urls.first,
        saveDir: saveDir,
        fileName: rename,
        segments: segments,
        cookies: widget.cookies,
        proxyUrl: proxyUrl,
      ).sendSignalToRust();
    } else {
      // 多条 — 使用 BatchCreateTask
      BatchCreateTask(
        urls: urls,
        saveDir: saveDir,
        segments: segments,
        proxyUrl: proxyUrl,
      ).sendSignalToRust();
    }

    Navigator.of(context).pop();
  }

  String _formatFileSize(int bytes) {
    if (bytes <= 0) return currentS.unknownSize;
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    int unitIndex = 0;
    double size = bytes.toDouble();
    while (size >= 1024 && unitIndex < units.length - 1) {
      size /= 1024;
      unitIndex++;
    }
    return '${size.toStringAsFixed(unitIndex == 0 ? 0 : 1)} ${units[unitIndex]}';
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    return ShadDialog(
      title: Row(
        children: [
          Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: c.accent.withValues(alpha: 0.1),
              borderRadius: BorderRadius.circular(6),
            ),
            child: Icon(LucideIcons.download, size: 14, color: c.accent),
          ),
          const SizedBox(width: 10),
          Text(s.newDownload),
          const SizedBox(width: 8),
          if (widget.fileSize > 0)
            _InfoTag(text: _formatFileSize(widget.fileSize), c: c),
          if (widget.fileSize > 0 && widget.mimeType.isNotEmpty)
            const SizedBox(width: 6),
          if (widget.mimeType.isNotEmpty) _InfoTag(text: widget.mimeType, c: c),
        ],
      ),
      description: Text(s.fromBrowserExtension),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(s.cancel),
        ),
        ShadButton(
          onPressed: _startDownload,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(LucideIcons.download, size: 13, color: Colors.white),
              const SizedBox(width: 6),
              Text(
                _isBatch ? s.startBatchDownload(_urlCount) : s.startDownload,
                style: const TextStyle(color: Colors.white),
              ),
            ],
          ),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            // URL 输入区 — 多行可编辑
            Row(
              children: [
                _SectionLabel(text: s.downloadUrl, c: c),
                const Spacer(),
                if (_urlCount > 0)
                  Text(
                    s.urlCount(_urlCount),
                    style: TextStyle(fontSize: 11, color: c.textMuted),
                  ),
              ],
            ),
            const SizedBox(height: 6),
            SizedBox(
              height: 120,
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
                      controller: _urlController,
                      focusNode: _urlFocusNode,
                      maxLines: null,
                      expands: true,
                      textAlignVertical: TextAlignVertical.top,
                      cursorColor: c.accent,
                      style: TextStyle(fontSize: 13, color: c.textPrimary),
                      decoration: InputDecoration(
                        hintText: s.batchUrlPlaceholder,
                        hintStyle: TextStyle(
                          fontSize: 12.5,
                          color: c.textMuted,
                        ),
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
                    ),
                  ),
                ),
              ),
            ),
            const SizedBox(height: 14),

            // 保存目录 + 线程数
            Row(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      _SectionLabel(text: s.saveDir, c: c),
                      const SizedBox(height: 6),
                      DirPickerField(
                        path: _saveDirController.text,
                        placeholder: s.selectSaveDir,
                        enabled: !_isPicking,
                        onTap: _pickSaveDir,
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                SizedBox(
                  width: 100,
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      _SectionLabel(text: s.threads, c: c),
                      const SizedBox(height: 6),
                      ShadSelect<String>(
                        placeholder: Text(s.auto),
                        options: ['auto', '4', '8', '16', '32', '64'].map((v) {
                          return ShadOption(
                            value: v,
                            child: Text(v == 'auto' ? s.auto : v),
                          );
                        }).toList(),
                        selectedOptionBuilder: (context, value) {
                          return Text(value == 'auto' ? s.auto : value);
                        },
                        onChanged: (v) => setState(() => selectedThreads = v),
                      ),
                    ],
                  ),
                ),
              ],
            ),

            // 重命名 — 仅单条时显示
            if (!_isBatch) ...[
              const SizedBox(height: 14),
              _SectionLabel(text: s.filenameOptional, c: c),
              const SizedBox(height: 6),
              ShadInput(
                controller: _renameController,
                placeholder: Text(s.autoDetectFilename),
              ),
            ],

            // 高级选项 — 可折叠，含任务独立代理
            const SizedBox(height: 10),
            GestureDetector(
              onTap: () => setState(() => _showAdvanced = !_showAdvanced),
              child: Row(
                children: [
                  Icon(
                    _showAdvanced
                        ? LucideIcons.chevronDown
                        : LucideIcons.chevronRight,
                    size: 14,
                    color: c.textMuted,
                  ),
                  const SizedBox(width: 4),
                  Text(
                    s.taskProxyAdvanced,
                    style: TextStyle(
                      fontSize: 11.5,
                      fontWeight: FontWeight.w500,
                      color: c.textMuted,
                    ),
                  ),
                ],
              ),
            ),
            if (_showAdvanced) ...[
              const SizedBox(height: 10),
              Row(
                children: [
                  _SectionLabel(text: s.taskProxy, c: c),
                  const SizedBox(width: 4),
                  ShadTooltip(
                    waitDuration: const Duration(milliseconds: 200),
                    showDuration: Duration.zero,
                    builder: (_) => Text(
                      s.taskProxyFormatHint,
                      style: const TextStyle(fontSize: 12, height: 1.5),
                    ),
                    child: ShadGestureDetector(
                      cursor: SystemMouseCursors.help,
                      child: Icon(
                        LucideIcons.circleAlert,
                        size: 13,
                        color: c.textMuted,
                      ),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 4),
              Text(
                s.taskProxyDesc,
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
              const SizedBox(height: 6),
              ShadInput(
                controller: _proxyUrlController,
                placeholder: Text(s.taskProxyPlaceholder),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// 信息标签（文件大小 / MIME 类型）
class _InfoTag extends StatelessWidget {
  final String text;
  final AppColors c;

  const _InfoTag({required this.text, required this.c});

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: c.surface2,
        borderRadius: BorderRadius.circular(4),
      ),
      child: Text(text, style: TextStyle(fontSize: 10, color: c.textMuted)),
    );
  }
}

/// 表单分区标签
class _SectionLabel extends StatelessWidget {
  final String text;
  final AppColors c;

  const _SectionLabel({required this.text, required this.c});

  @override
  Widget build(BuildContext context) {
    return Text(
      text,
      style: TextStyle(
        fontSize: 11.5,
        fontWeight: FontWeight.w500,
        color: c.textSecondary,
      ),
    );
  }
}
