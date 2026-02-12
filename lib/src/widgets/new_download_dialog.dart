import 'dart:io';

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
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';
import 'dir_picker_field.dart';

void showNewDownloadDialog(
  BuildContext context,
  DownloadController controller,
  SettingsProvider settingsProvider,
) {
  showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
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
  final _urlFocusNode = FocusNode();
  final _saveDirController = TextEditingController();
  final _renameController = TextEditingController();
  final _proxyUrlController = TextEditingController();
  String? selectedThreads;

  /// 是否展开高级选项（含任务代理）
  bool _showAdvanced = false;

  /// 解析出的有效 URL 数量（实时计算）
  int _urlCount = 0;

  /// 是否所有链接都是 magnet
  bool _allMagnet = false;

  /// 已选择的 .torrent 文件路径列表
  final List<String> _torrentFilePaths = [];

  /// 防止重复打开文件选择器
  bool _isPicking = false;

  @override
  void initState() {
    super.initState();
    _saveDirController.text = widget.settingsProvider.defaultSaveDir;
    _urlController.addListener(_onUrlChanged);
    _pasteUrlFromClipboard();
  }

  void _onUrlChanged() {
    final urls = _parseUrls(_urlController.text);
    final count = urls.length;
    final allMagnet =
        urls.isNotEmpty &&
        urls.every((u) => u.toLowerCase().startsWith('magnet:'));
    if (count != _urlCount || allMagnet != _allMagnet) {
      setState(() {
        _urlCount = count;
        _allMagnet = allMagnet;
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

  /// 读取剪切板内容，自动填入所有识别到的 URL
  Future<void> _pasteUrlFromClipboard() async {
    try {
      final data = await Clipboard.getData(Clipboard.kTextPlain);
      if (data == null || data.text == null) return;
      final text = data.text!.trim();

      final urls = _parseUrls(text);
      if (urls.isEmpty) return;

      _urlController.text = urls.join('\n');
    } catch (_) {
      // 剪切板访问失败时静默忽略
    }
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

  Future<void> _pickTorrentFiles() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await FilePicker.platform.pickFiles(
        dialogTitle: currentS.selectTorrentFile,
        type: FileType.custom,
        allowedExtensions: ['torrent'],
        allowMultiple: true,
        lockParentWindow: true,
      );
      if (result != null && result.files.isNotEmpty && mounted) {
        setState(() {
          for (final file in result.files) {
            if (file.path != null && !_torrentFilePaths.contains(file.path)) {
              _torrentFilePaths.add(file.path!);
            }
          }
        });
      }
    } finally {
      if (mounted) setState(() => _isPicking = false);
    }
  }

  void _removeTorrentFile(int index) {
    setState(() {
      _torrentFilePaths.removeAt(index);
    });
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
  bool get _hasTorrentFiles => _torrentFilePaths.isNotEmpty;

  void _startDownload() {
    final saveDir = _saveDirController.text.trim();
    if (saveDir.isEmpty) return;

    final proxyUrl = _proxyUrlController.text.trim();

    // Handle .torrent file downloads
    if (_hasTorrentFiles) {
      for (final path in _torrentFilePaths) {
        widget.controller.createTaskFromTorrentFile(
          torrentFilePath: path,
          saveDir: saveDir,
          proxyUrl: proxyUrl,
        );
      }
      Navigator.of(context).pop();
      return;
    }

    final urls = _parseUrls(_urlController.text);
    if (urls.isEmpty) return;

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
      // 单条 — 使用 CreateTask，支持重命名
      final rename = _renameController.text.trim();
      widget.controller.createTask(
        url: urls.first,
        saveDir: saveDir,
        fileName: rename,
        segments: segments,
        proxyUrl: proxyUrl,
      );
    } else {
      // 多条 — 使用 BatchCreateTask
      widget.controller.batchCreateTask(
        urls: urls,
        saveDir: saveDir,
        segments: segments,
        proxyUrl: proxyUrl,
      );
    }

    Navigator.of(context).pop();
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
        ],
      ),
      description: Text(s.batchDownloadDesc),
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
                _hasTorrentFiles
                    ? s.startBatchDownload(_torrentFilePaths.length)
                    : _isBatch
                    ? s.startBatchDownload(_urlCount)
                    : s.startDownload,
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
            // .torrent 文件选择区域（当有 torrent 文件时替换 URL 输入区）
            if (_hasTorrentFiles) ...[
              Row(
                children: [
                  _SectionLabel(text: s.torrentFileSelected, c: c),
                  const Spacer(),
                  Text(
                    s.torrentFileCount(_torrentFilePaths.length),
                    style: TextStyle(fontSize: 11, color: c.textMuted),
                  ),
                ],
              ),
              const SizedBox(height: 6),
              Container(
                constraints: const BoxConstraints(maxHeight: 120),
                decoration: BoxDecoration(
                  color: c.surface1,
                  borderRadius: BorderRadius.circular(8),
                  border: Border.all(color: c.border),
                ),
                child: ListView.builder(
                  shrinkWrap: true,
                  padding: const EdgeInsets.all(6),
                  itemCount: _torrentFilePaths.length,
                  itemBuilder: (context, index) {
                    final path = _torrentFilePaths[index];
                    final fileName = File(path).uri.pathSegments.last;
                    return Padding(
                      padding: const EdgeInsets.symmetric(vertical: 2),
                      child: Row(
                        children: [
                          Icon(LucideIcons.fileDown, size: 14, color: c.accent),
                          const SizedBox(width: 6),
                          Expanded(
                            child: Text(
                              fileName,
                              style: TextStyle(
                                fontSize: 12.5,
                                color: c.textPrimary,
                              ),
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                          GestureDetector(
                            onTap: () => _removeTorrentFile(index),
                            child: Icon(
                              LucideIcons.x,
                              size: 14,
                              color: c.textMuted,
                            ),
                          ),
                        ],
                      ),
                    );
                  },
                ),
              ),
              const SizedBox(height: 8),
              Row(
                children: [
                  ShadButton.outline(
                    size: ShadButtonSize.sm,
                    enabled: !_isPicking,
                    onPressed: _pickTorrentFiles,
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Icon(
                          LucideIcons.plus,
                          size: 13,
                          color: c.textSecondary,
                        ),
                        const SizedBox(width: 4),
                        Text(
                          s.openTorrentFile,
                          style: TextStyle(
                            fontSize: 12,
                            color: c.textSecondary,
                          ),
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(width: 8),
                  GestureDetector(
                    onTap: () => setState(() => _torrentFilePaths.clear()),
                    child: Text(
                      s.cancel,
                      style: TextStyle(
                        fontSize: 12,
                        color: c.textMuted,
                        decoration: TextDecoration.underline,
                      ),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 14),
            ] else ...[
              // URL 输入区 — 始终多行
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
              const SizedBox(height: 6),
              // .torrent 文件选择按钮
              Align(
                alignment: Alignment.centerLeft,
                child: ShadButton.ghost(
                  size: ShadButtonSize.sm,
                  enabled: !_isPicking,
                  onPressed: _pickTorrentFiles,
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(LucideIcons.fileDown, size: 13, color: c.accent),
                      const SizedBox(width: 6),
                      Text(
                        s.openTorrentFile,
                        style: TextStyle(fontSize: 12, color: c.accent),
                      ),
                    ],
                  ),
                ),
              ),
              const SizedBox(height: 8),
            ],

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
                if (!_allMagnet && !_hasTorrentFiles) ...[
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
                          options: ['auto', '4', '8', '16', '32', '64'].map((
                            v,
                          ) {
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
              ],
            ),

            // 重命名 — 仅单条 URL 时显示（torrent 文件自动识别名称）
            if (!_isBatch && !_hasTorrentFiles) ...[
              const SizedBox(height: 14),
              _SectionLabel(text: s.renameOptional, c: c),
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
