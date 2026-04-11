import '../services/file_picker_service.dart';
import 'package:flutter/material.dart'
    show
        AdaptiveTextSelectionToolbar,
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

import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';
import 'dir_picker_field.dart';
import 'thread_selector.dart';

/// UA 预设映射（key → UA 字符串）
const _kUaPresets = {
  'chrome':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36',
  'firefox':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0',
  'edge':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0',
  'netdisk': 'netdisk',
};

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
  String referrer = '',
  String defaultQueueId = '',
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
      referrer: referrer,
      defaultSaveDir: defaultSaveDir,
      defaultQueueId: defaultQueueId,
    ),
  );
}

class _QuickDownloadDialogContent extends StatefulWidget {
  final String url;
  final String filename;
  final int fileSize;
  final String mimeType;
  final String cookies;
  final String referrer;
  final String defaultSaveDir;
  final String defaultQueueId;

  const _QuickDownloadDialogContent({
    required this.url,
    required this.filename,
    required this.fileSize,
    required this.mimeType,
    required this.cookies,
    required this.referrer,
    required this.defaultSaveDir,
    required this.defaultQueueId,
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
  final _userAgentController = TextEditingController();
  String? selectedThreads;
  String _selectedUaPreset = 'custom';

  /// 选中的队列 ID（空字符串 = 默认队列）
  late String _selectedQueueId;

  /// 用户是否手动修改过线程数（用于判断切换队列时是否需要自动更新）
  bool _threadsUserModified = false;

  /// 线程选择器的 key 版本，切换队列时递增以强制重建 ShadSelect
  int _threadsSelectVersion = 0;

  /// 是否展开高级选项（含任务代理）
  bool _showAdvanced = false;

  /// 解析出的有效 URL 数量（实时计算）
  int _urlCount = 0;

  /// 防止重复打开文件选择器
  bool _isPicking = false;

  /// 用户是否手动通过文件选择器修改过保存目录
  bool _saveDirUserModified = false;

  /// 根据队列 ID 计算有效的线程数选项字符串。
  ///
  /// 优先级：自定义队列的 defaultSegments → 全局 defaultSegments → null（Auto）
  String? _effectiveSegmentsOption(String queueId) {
    if (queueId.isNotEmpty) {
      final queues = DownloadController.globalInstance?.queues ?? [];
      final queue = queues.where((q) => q.queueId == queueId).firstOrNull;
      if (queue != null && queue.defaultSegments > 0) {
        return queue.defaultSegments.toString();
      }
    }
    final global = SettingsProvider.globalInstance?.defaultSegments ?? 0;
    return global > 0 ? global.toString() : null;
  }

  @override
  void initState() {
    super.initState();
    _selectedQueueId = widget.defaultQueueId;
    _urlController.text = widget.url;
    _saveDirController.text = widget.defaultSaveDir;
    if (widget.filename.isNotEmpty) {
      _renameController.text = widget.filename;
    }
    _urlController.addListener(_onUrlChanged);
    // 根据队列/全局设置初始化默认线程数
    selectedThreads = _effectiveSegmentsOption(_selectedQueueId);
    // 根据已知文件名自动匹配分类保存目录
    _tryAutoApplySaveDir(widget.filename);
    // 初始化时计算一次
    _onUrlChanged();
  }

  void _onUrlChanged() {
    final entries = _parseEntries(_urlController.text);
    final count = entries.length;
    if (count != _urlCount) {
      setState(() {
        _urlCount = count;
      });
    }
  }

  /// 解析 aria2 风格的下载条目（URL + 可选 out=/checksum= 选项行）
  static List<_QuickEntry> _parseEntries(String text) {
    final lines = text.split('\n');
    final entries = <_QuickEntry>[];
    _QuickEntry? current;
    final urlPattern = RegExp(r'^(https?|ftp)://\S+', caseSensitive: false);

    for (final line in lines) {
      // 选项行
      if (line.startsWith(' ') || line.startsWith('\t')) {
        if (current == null) continue;
        final trimmed = line.trim();
        if (trimmed.startsWith('out=')) {
          current = _QuickEntry(
            current.url,
            fileName: trimmed.substring(4),
            checksum: current.checksum,
          );
        } else if (trimmed.startsWith('checksum=')) {
          current = _QuickEntry(
            current.url,
            fileName: current.fileName,
            checksum: trimmed.substring(9),
          );
        }
        continue;
      }

      final trimmed = line.trim();
      if (trimmed.isEmpty) continue;
      if (trimmed.startsWith('#')) continue;

      if (current != null) {
        entries.add(current);
        current = null;
      }

      if (trimmed.toLowerCase().startsWith('magnet:?')) {
        current = _QuickEntry(trimmed);
      } else {
        final match = urlPattern.firstMatch(trimmed);
        if (match != null) {
          current = _QuickEntry(match.group(0)!);
        }
      }
    }
    if (current != null) entries.add(current);
    return entries;
  }

  @override
  void dispose() {
    _urlController.removeListener(_onUrlChanged);
    _urlController.dispose();
    _urlFocusNode.dispose();
    _saveDirController.dispose();
    _renameController.dispose();
    _proxyUrlController.dispose();
    _userAgentController.dispose();
    super.dispose();
  }

  Future<void> _pickSaveDir() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await FilePickerService.pickDirectory(
        dialogTitle: currentS.selectSaveDir,
        initialDirectory: _saveDirController.text.trim().isNotEmpty
            ? _saveDirController.text.trim()
            : null,
      );
      if (result != null && mounted) {
        _saveDirController.text = result;
        _saveDirUserModified = true;
      }
    } on FilePickerException catch (e) {
      if (mounted) _showPickerError(e);
    } finally {
      if (mounted) setState(() => _isPicking = false);
    }
  }

  void _showPickerError(FilePickerException e) {
    final s = currentS;
    final message = switch (e.reason) {
      FilePickerFailReason.timeout => s.filePickerErrorTimeout,
      FilePickerFailReason.noDialogTool => s.filePickerErrorNoTool,
      FilePickerFailReason.comInitFailed => s.filePickerErrorNative,
      FilePickerFailReason.nativeDialogFailed => s.filePickerErrorNative,
      FilePickerFailReason.unknown => s.filePickerErrorGeneric,
    };
    ShadSonner.of(context).show(ShadToast.destructive(title: Text(message)));
  }

  bool get _isBatch => _urlCount > 1;

  /// 根据文件名尝试自动匹配分类的保存目录。
  void _tryAutoApplySaveDir(String fileName) {
    if (fileName.isEmpty || _saveDirUserModified) return;
    final settings = SettingsProvider.globalInstance;
    if (settings == null) return;
    final categories = settings.customCategories
        .where((c) => c.visible)
        .toList()
      ..sort((a, b) => a.position.compareTo(b.position));

    // 先查普通分类（非 all / other）
    for (final cat in categories) {
      if (cat.builtinType == 'all' || cat.builtinType == 'other') continue;
      if (cat.saveDir.isNotEmpty && cat.matches(fileName)) {
        _saveDirController.text = cat.saveDir;
        return;
      }
    }

    // 再查 other 分类
    final normals = categories
        .where((c) => c.builtinType != 'all' && c.builtinType != 'other')
        .toList();
    final otherCat = categories
        .where((c) => c.builtinType == 'other')
        .firstOrNull;
    if (otherCat != null && otherCat.saveDir.isNotEmpty) {
      final matchesAny = normals.any((c) => c.matches(fileName));
      if (!matchesAny) {
        _saveDirController.text = otherCat.saveDir;
      }
    }
  }

  void _startDownload() {
    final saveDir = _saveDirController.text.trim();
    if (saveDir.isEmpty) return;

    final entries = _parseEntries(_urlController.text);
    if (entries.isEmpty) return;

    final proxyUrl = _proxyUrlController.text.trim();
    final userAgent = _userAgentController.text.trim();

    final parsedSeg = int.tryParse(selectedThreads ?? '') ?? 0;
    final segments = parsedSeg > 0 ? parsedSeg.clamp(1, 64) : 0;

    if (entries.length == 1) {
      // 单条 — 使用 ConfirmExternalDownload，支持重命名和 cookies
      final entry = entries.first;
      final rename = _renameController.text.trim();
      final fileName = rename.isNotEmpty ? rename : entry.fileName;
      ConfirmExternalDownload(
        url: entry.url,
        saveDir: saveDir,
        fileName: fileName,
        segments: segments,
        cookies: widget.cookies,
        referrer: widget.referrer,
        hintFileSize: widget.fileSize,
        proxyUrl: proxyUrl,
        userAgent: userAgent,
        queueId: _selectedQueueId,
      ).sendSignalToRust();
    } else {
      // 多条 — 使用 BatchCreateTask（携带每条的 fileName/checksum）
      BatchCreateTask(
        entries: entries
            .map(
              (e) => UrlEntry(
                url: e.url,
                fileName: e.fileName,
                checksum: e.checksum,
              ),
            )
            .toList(),
        saveDir: saveDir,
        segments: segments,
        proxyUrl: proxyUrl,
        userAgent: userAgent,
        queueId: _selectedQueueId,
        cookies: widget.cookies,
        referrer: widget.referrer,
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
          if (widget.mimeType.isNotEmpty)
            Flexible(
              child: _InfoTag(text: widget.mimeType, c: c),
            ),
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
                      contextMenuBuilder: (context, editableTextState) {
                        return Localizations(
                          locale: const Locale('en'),
                          delegates: const [
                            DefaultWidgetsLocalizations.delegate,
                            DefaultMaterialLocalizations.delegate,
                          ],
                          child: AdaptiveTextSelectionToolbar.editableText(
                            editableTextState: editableTextState,
                          ),
                        );
                      },
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
                  width: 110,
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      _SectionLabel(text: s.threads, c: c),
                      const SizedBox(height: 6),
                      ThreadSelector(
                        value: selectedThreads,
                        version: _threadsSelectVersion,
                        onChanged: (v) => setState(() {
                          selectedThreads = v;
                          _threadsUserModified = true;
                        }),
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

            // 队列选择器（有命名队列时才显示）
            _buildQueueSelector(s, c),

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
              const SizedBox(height: 10),
              _SectionLabel(text: s.userAgent, c: c),
              const SizedBox(height: 4),
              Text(
                s.userAgentTaskPlaceholder,
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
              const SizedBox(height: 6),
              Row(
                children: [
                  SizedBox(
                    width: 150,
                    child: ShadSelect<String>(
                      initialValue: _selectedUaPreset,
                      placeholder: Text(s.userAgentPresetCustom),
                      options: [
                        ShadOption(
                          value: 'chrome',
                          child: Text(s.userAgentPresetChrome),
                        ),
                        ShadOption(
                          value: 'firefox',
                          child: Text(s.userAgentPresetFirefox),
                        ),
                        ShadOption(
                          value: 'edge',
                          child: Text(s.userAgentPresetEdge),
                        ),
                        ShadOption(
                          value: 'netdisk',
                          child: Text(s.userAgentPresetNetdisk),
                        ),
                        ShadOption(
                          value: 'custom',
                          child: Text(s.userAgentPresetCustom),
                        ),
                      ],
                      selectedOptionBuilder: (context, value) {
                        final label = switch (value) {
                          'chrome' => 'Chrome',
                          'firefox' => 'Firefox',
                          'edge' => 'Edge',
                          'netdisk' => 'netdisk',
                          _ => s.userAgentPresetCustom,
                        };
                        return Text(
                          label,
                          overflow: TextOverflow.ellipsis,
                          maxLines: 1,
                        );
                      },
                      onChanged: (v) {
                        if (v == null) return;
                        setState(() => _selectedUaPreset = v);
                        final preset = _kUaPresets[v];
                        if (preset != null) {
                          _userAgentController.text = preset;
                        }
                      },
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: ShadInput(
                      controller: _userAgentController,
                      placeholder: Text(s.userAgentPlaceholder),
                      onChanged: (_) {
                        if (_selectedUaPreset != 'custom') {
                          setState(() => _selectedUaPreset = 'custom');
                        }
                      },
                    ),
                  ),
                ],
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _buildQueueSelector(S s, AppColors c) {
    final queues = DownloadController.globalInstance?.queues ?? [];
    if (queues.isEmpty) return const SizedBox.shrink();

    final allOptions = <DownloadQueue>[
      const DownloadQueue(
        queueId: '',
        name: '',
        speedLimitKbps: 0,
        maxConcurrent: 0,
        defaultSaveDir: '',
        position: -1,
      ),
      ...queues,
    ];

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const SizedBox(height: 14),
        _SectionLabel(text: s.taskQueueLabel, c: c),
        const SizedBox(height: 6),
        ShadSelect<String>(
          initialValue: _selectedQueueId,
          options: allOptions.map((q) {
            final label = q.queueId.isEmpty ? s.defaultQueue : q.name;
            return ShadOption(value: q.queueId, child: Text(label));
          }).toList(),
          selectedOptionBuilder: (context, value) {
            if (value.isEmpty) return Text(s.defaultQueue);
            final q = queues.where((q) => q.queueId == value).firstOrNull;
            return Text(
              q?.name ?? s.defaultQueue,
              overflow: TextOverflow.ellipsis,
              maxLines: 1,
            );
          },
          onChanged: (v) {
            if (v != null) {
              setState(() {
                _selectedQueueId = v;
                // 用户未手动改过线程数时，跟随新队列/全局默认设置
                if (!_threadsUserModified) {
                  selectedThreads = _effectiveSegmentsOption(v);
                  _threadsSelectVersion++;
                }
              });
            }
          },
        ),
      ],
    );
  }
}

/// 解析后的单条下载入口（URL + 可选 out= 文件名 + 可选 checksum=）
class _QuickEntry {
  final String url;
  final String fileName;
  final String checksum;
  const _QuickEntry(this.url, {this.fileName = '', this.checksum = ''});
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
      child: Text(
        text,
        style: TextStyle(fontSize: 10, color: c.textMuted),
        overflow: TextOverflow.ellipsis,
        maxLines: 1,
      ),
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
