import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';

void showNewDownloadDialog(
  BuildContext context,
  DownloadController controller,
  SettingsProvider settingsProvider,
) {
  showShadDialog(
    context: context,
    barrierColor: const Color(0x1A000000),
    animateIn: const [],
    animateOut: const [],
    builder: (context) => _NewDownloadDialogContent(
      controller: controller,
      settingsProvider: settingsProvider,
    ),
  );
}

class _NewDownloadDialogContent extends StatefulWidget {
  final DownloadController controller;
  final SettingsProvider settingsProvider;

  const _NewDownloadDialogContent({
    required this.controller,
    required this.settingsProvider,
  });

  @override
  State<_NewDownloadDialogContent> createState() =>
      _NewDownloadDialogContentState();
}

class _NewDownloadDialogContentState extends State<_NewDownloadDialogContent> {
  final _urlController = TextEditingController();
  final _saveDirController = TextEditingController();
  final _renameController = TextEditingController();
  String? selectedThreads;

  @override
  void initState() {
    super.initState();
    _saveDirController.text = widget.settingsProvider.defaultSaveDir;
    _pasteUrlFromClipboard();
  }

  /// 读取剪切板内容，如果包含 http/https/ftp 开头的 URL，自动填入下载地址
  Future<void> _pasteUrlFromClipboard() async {
    try {
      final data = await Clipboard.getData(Clipboard.kTextPlain);
      if (data == null || data.text == null) return;
      final text = data.text!.trim();
      final firstLine = text.split('\n').first.trim();
      final urlPattern = RegExp(r'^(https?|ftp)://\S+', caseSensitive: false);
      final match = urlPattern.firstMatch(firstLine);
      if (match != null) {
        _urlController.text = match.group(0)!;
      }
    } catch (_) {
      // 剪切板访问失败时静默忽略
    }
  }

  @override
  void dispose() {
    _urlController.dispose();
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
    final url = _urlController.text.trim();
    if (url.isEmpty) return;

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

    widget.controller.createTask(
      url: url,
      saveDir: saveDir,
      fileName: rename,
      segments: segments,
    );

    Navigator.of(context).pop();
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
        ],
      ),
      description: Text(LocaleScope.of(context).addDownloadTask),
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
            _SectionLabel(text: LocaleScope.of(context).downloadUrl, c: c),
            const SizedBox(height: 6),
            ShadInput(
              controller: _urlController,
              placeholder: Text(LocaleScope.of(context).urlPlaceholder),
            ),
            const SizedBox(height: 14),
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
            _SectionLabel(text: LocaleScope.of(context).renameOptional, c: c),
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
