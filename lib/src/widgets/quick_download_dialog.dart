import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';

/// 浏览器扩展下载请求的快速确认对话框。
///
/// 在主窗口内以 Dialog 形式弹出，无需创建独立子窗口，
/// 延迟从 3-4 秒降低到 <100ms。
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
    barrierColor: const Color(0x1A000000),
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
  final _saveDirController = TextEditingController();
  final _renameController = TextEditingController();
  String? selectedThreads;

  @override
  void initState() {
    super.initState();
    _saveDirController.text = widget.defaultSaveDir;
    if (widget.filename.isNotEmpty) {
      _renameController.text = widget.filename;
    }
  }

  @override
  void dispose() {
    _saveDirController.dispose();
    _renameController.dispose();
    super.dispose();
  }

  Future<void> _pickSaveDir() async {
    final result = await FilePicker.platform.getDirectoryPath(
      dialogTitle: currentS.selectSaveDir,
      initialDirectory: _saveDirController.text.trim().isNotEmpty
          ? _saveDirController.text.trim()
          : null,
    );
    if (result != null) {
      _saveDirController.text = result;
    }
  }

  void _startDownload() {
    final saveDir = _saveDirController.text.trim();
    if (saveDir.isEmpty) return;

    final rename = _renameController.text.trim();
    final segments = switch (selectedThreads) {
      'auto' => 0,
      '4' => 4,
      '8' => 8,
      '16' => 16,
      '32' => 32,
      '64' => 64,
      _ => 0,
    };

    // 直接发送确认信号到 Rust，无需跨窗口 IPC
    ConfirmExternalDownload(
      url: widget.url,
      saveDir: saveDir,
      fileName: rename,
      segments: segments,
      cookies: widget.cookies,
    ).sendSignalToRust();

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
          Text(LocaleScope.of(context).newDownload),
          const SizedBox(width: 8),
          if (widget.fileSize > 0)
            _InfoTag(text: _formatFileSize(widget.fileSize), c: c),
          if (widget.fileSize > 0 && widget.mimeType.isNotEmpty)
            const SizedBox(width: 6),
          if (widget.mimeType.isNotEmpty) _InfoTag(text: widget.mimeType, c: c),
        ],
      ),
      description: Text(LocaleScope.of(context).fromBrowserExtension),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(LocaleScope.of(context).cancel),
        ),
        ShadButton(
          onPressed: _startDownload,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(LucideIcons.download, size: 13, color: Colors.white),
              const SizedBox(width: 6),
              Text(
                LocaleScope.of(context).startDownload,
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
            // URL 显示
            _SectionLabel(text: LocaleScope.of(context).downloadUrl, c: c),
            const SizedBox(height: 6),
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
              decoration: BoxDecoration(
                color: c.surface2,
                borderRadius: BorderRadius.circular(8),
                border: Border.all(color: c.border.withValues(alpha: 0.6)),
              ),
              child: SelectableText(
                widget.url,
                style: TextStyle(
                  fontSize: 12,
                  color: c.textSecondary,
                  fontFamily: 'monospace',
                  height: 1.5,
                ),
                maxLines: 2,
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
                      _SectionLabel(
                        text: LocaleScope.of(context).saveDir,
                        c: c,
                      ),
                      const SizedBox(height: 6),
                      GestureDetector(
                        onTap: _pickSaveDir,
                        child: AbsorbPointer(
                          child: ShadInput(
                            controller: _saveDirController,
                            placeholder: Text(
                              LocaleScope.of(context).selectSaveDir,
                            ),
                            readOnly: true,
                            trailing: Padding(
                              padding: const EdgeInsets.only(right: 4),
                              child: Icon(
                                LucideIcons.folderOpen,
                                size: 14,
                                color: c.textMuted,
                              ),
                            ),
                          ),
                        ),
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
                      _SectionLabel(
                        text: LocaleScope.of(context).threads,
                        c: c,
                      ),
                      const SizedBox(height: 6),
                      ShadSelect<String>(
                        placeholder: Text(LocaleScope.of(context).auto),
                        options: ['auto', '4', '8', '16', '32', '64'].map((v) {
                          final s = LocaleScope.of(context);
                          return ShadOption(
                            value: v,
                            child: Text(v == 'auto' ? s.auto : v),
                          );
                        }).toList(),
                        selectedOptionBuilder: (context, value) {
                          final s = LocaleScope.of(context);
                          return Text(value == 'auto' ? s.auto : value);
                        },
                        onChanged: (v) => setState(() => selectedThreads = v),
                      ),
                    ],
                  ),
                ),
              ],
            ),

            const SizedBox(height: 14),

            // 文件名
            _SectionLabel(text: LocaleScope.of(context).filenameOptional, c: c),
            const SizedBox(height: 6),
            ShadInput(
              controller: _renameController,
              placeholder: Text(LocaleScope.of(context).autoDetectFilename),
            ),
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
