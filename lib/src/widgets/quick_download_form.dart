/// 快速下载表单 — 主窗口对话框与外部唤起独立小窗共用的表单主体。
///
/// 表单不直接触碰全局单例（SettingsProvider / DownloadController）、
/// 不发送 Rust 信号、不做持久化：
/// - 环境数据（队列列表 / 默认线程数 / 上次线程选择 / 目录选择器）
///   经 [QuickDownloadFormHost] 注入 — 主窗口宿主读全局单例，
///   独立小窗宿主读原生通道注入的载荷；
/// - 提交结果打包为 [QuickDownloadFormResult] 交给调用方处理
///   （主窗口直接发信号，小窗经原生通道中继回主引擎）。
library;

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

import '../i18n/locale_provider.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'dir_picker_field.dart';
import 'thread_selector.dart';

/// UA 预设映射（key → UA 字符串）
const kQuickUaPresets = {
  'chrome':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36',
  'firefox':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0',
  'edge':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0',
  'netdisk': 'netdisk',
};

/// 解析后的单条下载入口（URL + 可选 out= 文件名 + 可选 checksum=）
class QuickDownloadEntry {
  final String url;
  final String fileName;
  final String checksum;
  /// 音频轨 URL（通用「视频轨+音频轨」离散下载对语义，按 MIME video/*
  /// vs audio/* 分轨判定）。空 = 普通单 URL；非空 = url 是视频轨，
  /// 本字段是音频轨。仅由外部下载请求（ExternalDownloadRequest.audioUrl）
  /// 转入；纯文本 URL 列表解析（一行一条）不产生轨对语义。
  final String audioUrl;
  const QuickDownloadEntry(
    this.url, {
    this.fileName = '',
    this.checksum = '',
    this.audioUrl = '',
  });
}

/// 解析 aria2 风格的下载条目（URL + 可选 out=/checksum= 选项行）。
///
/// 外部下载请求的 `url` 字段可能是换行连接的多条 URL（aria2 addUri 多 URI /
/// 脚本接管批量接口约定），快速下载表单与免打扰静默路径共用本解析器。
List<QuickDownloadEntry> parseQuickDownloadEntries(String text) {
  final lines = text.split('\n');
  final entries = <QuickDownloadEntry>[];
  QuickDownloadEntry? current;
  final urlPattern = RegExp(r'^(https?|ftp)://\S+', caseSensitive: false);

  for (final line in lines) {
    // 选项行
    if (line.startsWith(' ') || line.startsWith('\t')) {
      if (current == null) continue;
      final trimmed = line.trim();
      if (trimmed.startsWith('out=')) {
        current = QuickDownloadEntry(
          current.url,
          fileName: trimmed.substring(4),
          checksum: current.checksum,
          audioUrl: current.audioUrl,
        );
      } else if (trimmed.startsWith('checksum=')) {
        current = QuickDownloadEntry(
          current.url,
          fileName: current.fileName,
          checksum: trimmed.substring(9),
          audioUrl: current.audioUrl,
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
      current = QuickDownloadEntry(trimmed);
    } else {
      final match = urlPattern.firstMatch(trimmed);
      if (match != null) {
        current = QuickDownloadEntry(match.group(0)!);
      }
    }
  }
  if (current != null) entries.add(current);
  return entries;
}

/// 表单可选队列（[QuickDownloadFormHost.queues] 的元素）。
///
/// 与 `DownloadQueue` 解耦：独立小窗引擎中不存在 DownloadController，
/// 队列信息经载荷 JSON 注入后以本类型还原。
class QuickQueueOption {
  final String queueId;
  final String name;

  /// 队列默认线程数（0 = 未设置，跟随全局）
  final int defaultSegments;

  const QuickQueueOption({
    required this.queueId,
    required this.name,
    this.defaultSegments = 0,
  });
}

/// 表单提交结果 — 由调用方解析发信号（见 quick_download_submitter.dart）。
class QuickDownloadFormResult {
  /// 编辑后的原始多行 URL 文本（未解析）
  final String urlText;
  final String saveDir;

  /// 重命名（仅单条时有意义，空 = 自动识别）
  final String rename;

  /// 线程数（0 = 自动）
  final int segments;
  final String proxyUrl;
  final String userAgent;
  final String queueId;

  /// 任务 Cookie（预填浏览器捕获值，用户可编辑覆盖；空 = 不带）
  final String cookies;

  /// 哈希校验值（"algo=hexhash"，仅单条时有意义，空 = 跳过校验）
  final String checksum;

  /// 用户是否手动改过线程数（决定是否记忆本次选择）
  final bool threadsUserModified;

  /// 音视频轨对的音频轨 URL（外部请求透传，表单不可编辑；空 = 普通下载）。
  /// 仅单条时有意义，非空时 url 是视频轨、本字段是音频轨。
  final String audioUrl;

  const QuickDownloadFormResult({
    required this.urlText,
    required this.saveDir,
    required this.rename,
    required this.segments,
    required this.proxyUrl,
    required this.userAgent,
    required this.queueId,
    required this.cookies,
    required this.checksum,
    required this.threadsUserModified,
    this.audioUrl = '',
  });
}

/// 表单环境宿主 — 屏蔽"主窗口全局单例"与"小窗载荷注入"的差异。
abstract class QuickDownloadFormHost {
  /// 可选队列列表（空 = 不显示队列选择器）
  List<QuickQueueOption> get queues;

  /// 全局默认线程数（0 = 自动）
  int get defaultSegments;

  /// 上次新建下载选择的线程数（'' = 未记录，'auto' = 自动，数字串 = 固定）
  String get lastDialogThreads;

  /// 弹出目录选择对话框。取消返回 null，失败抛 [FilePickerException]。
  Future<String?> pickDirectory({
    required String dialogTitle,
    String? initialDirectory,
  });
}

/// 快速下载表单主体（URL / 保存目录 / 线程数 / 重命名 / 队列 / 高级选项）
/// + 底部动作按钮（取消 / 开始下载）。
class QuickDownloadForm extends StatefulWidget {
  /// 初始 URL（可能多行）
  final String initialUrl;

  /// 已知文件名（预填重命名输入框；空 = 自动识别）
  final String initialFileName;

  /// 初始保存目录 — 调用方已按"请求方指定 / 分类规则 / 默认目录"预解析
  final String initialSaveDir;

  /// 初始选中队列 ID（'' = 默认队列）
  final String defaultQueueId;

  /// 初始 Cookie（浏览器扩展捕获，预填高级选项供编辑覆盖）
  final String initialCookies;

  /// 音视频轨对的音频轨 URL（外部请求透传，表单不可编辑；空 = 普通下载）
  final String initialAudioUrl;

  final QuickDownloadFormHost host;
  final ValueChanged<QuickDownloadFormResult> onSubmit;
  final VoidCallback onCancel;

  const QuickDownloadForm({
    super.key,
    required this.initialUrl,
    required this.initialFileName,
    required this.initialSaveDir,
    required this.defaultQueueId,
    required this.initialCookies,
    this.initialAudioUrl = '',
    required this.host,
    required this.onSubmit,
    required this.onCancel,
  });

  @override
  State<QuickDownloadForm> createState() => _QuickDownloadFormState();
}

class _QuickDownloadFormState extends State<QuickDownloadForm> {
  final _urlController = TextEditingController();
  final _urlFocusNode = FocusNode();
  final _saveDirController = TextEditingController();
  final _renameController = TextEditingController();
  final _proxyUrlController = TextEditingController();
  final _userAgentController = TextEditingController();
  final _cookieController = TextEditingController();
  final _checksumController = TextEditingController();

  /// 选中的哈希算法（与后端 verify_checksum 支持的算法名一致）
  String _selectedHashAlgo = 'sha-256';
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

  /// 根据队列 ID 计算有效的线程数选项字符串。
  ///
  /// 优先级：自定义队列的 defaultSegments → 全局 defaultSegments → null（Auto）
  String? _effectiveSegmentsOption(String queueId) {
    if (queueId.isNotEmpty) {
      final queue = widget.host.queues
          .where((q) => q.queueId == queueId)
          .firstOrNull;
      if (queue != null && queue.defaultSegments > 0) {
        return queue.defaultSegments.toString();
      }
    }
    final global = widget.host.defaultSegments;
    return global > 0 ? global.toString() : null;
  }

  @override
  void initState() {
    super.initState();
    _selectedQueueId = widget.defaultQueueId;
    _urlController.text = widget.initialUrl;
    _saveDirController.text = widget.initialSaveDir;
    if (widget.initialFileName.isNotEmpty) {
      _renameController.text = widget.initialFileName;
    }
    _cookieController.text = widget.initialCookies;
    _urlController.addListener(_onUrlChanged);
    // 优先沿用上次用户选择的线程数，其次根据队列/全局设置初始化
    final lastThreads = widget.host.lastDialogThreads;
    selectedThreads = lastThreads.isNotEmpty
        ? (lastThreads == 'auto' ? null : lastThreads)
        : _effectiveSegmentsOption(_selectedQueueId);
    // 初始化时计算一次
    _onUrlChanged();
  }

  void _onUrlChanged() {
    final entries = parseQuickDownloadEntries(_urlController.text);
    final count = entries.length;
    if (count != _urlCount) {
      setState(() {
        _urlCount = count;
      });
    }
  }

  @override
  void dispose() {
    _urlController.removeListener(_onUrlChanged);
    _urlController.dispose();
    _urlFocusNode.dispose();
    _saveDirController.dispose();
    _cookieController.dispose();
    _checksumController.dispose();
    _renameController.dispose();
    _proxyUrlController.dispose();
    _userAgentController.dispose();
    super.dispose();
  }

  Future<void> _pickSaveDir() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await widget.host.pickDirectory(
        dialogTitle: currentS.selectSaveDir,
        initialDirectory: _saveDirController.text.trim().isNotEmpty
            ? _saveDirController.text.trim()
            : null,
      );
      if (result != null && mounted) {
        _saveDirController.text = result;
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

  void _startDownload() {
    final saveDir = _saveDirController.text.trim();
    if (saveDir.isEmpty) return;

    final entries = parseQuickDownloadEntries(_urlController.text);
    if (entries.isEmpty) return;

    final parsedSeg = int.tryParse(selectedThreads ?? '') ?? 0;
    final segments = parsedSeg > 0 ? parsedSeg.clamp(1, 256) : 0;

    // 高级选项手填的校验值拼成 aria2 风格 "algo=hexhash"；
    // 为空则由提交器回退到 URL 文本里的 checksum= 选项行。
    final hash = _checksumController.text.trim();
    final checksum = hash.isEmpty ? '' : '$_selectedHashAlgo=$hash';

    widget.onSubmit(
      QuickDownloadFormResult(
        urlText: _urlController.text,
        saveDir: saveDir,
        rename: _renameController.text.trim(),
        segments: segments,
        proxyUrl: _proxyUrlController.text.trim(),
        userAgent: _userAgentController.text.trim(),
        queueId: _selectedQueueId,
        cookies: _cookieController.text.trim(),
        checksum: checksum,
        threadsUserModified: _threadsUserModified,
        audioUrl: widget.initialAudioUrl,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final m = AppMetrics.of(context);

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // URL 输入区 — 多行可编辑
        Row(
          children: [
            QuickSectionLabel(text: s.downloadUrl, c: c),
            const Spacer(),
            if (_urlCount > 0)
              Text(
                s.urlCount(_urlCount),
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
          ],
        ),
        const SizedBox(height: 6),
        // 自适应高度：默认 2 行紧凑，随内容增高到 6 行后内部滚动，
        // 避免单条链接时大片留白（小窗高度跟随内容，同步收窄）
        Localizations(
          locale: const Locale('en'),
          delegates: const [
            DefaultWidgetsLocalizations.delegate,
            DefaultMaterialLocalizations.delegate,
          ],
          child: Material(
            type: MaterialType.transparency,
            child: TextSelectionTheme(
              data: TextSelectionThemeData(
                selectionColor: m.textSelection(c.accent),
                cursorColor: c.accent,
                selectionHandleColor: c.accent,
              ),
              child: TextField(
                controller: _urlController,
                focusNode: _urlFocusNode,
                minLines: 2,
                maxLines: 6,
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
                  hintStyle: TextStyle(fontSize: 12.5, color: c.textMuted),
                  hintMaxLines: 5,
                  contentPadding: const EdgeInsets.all(10),
                  filled: true,
                  fillColor: c.inputBg,
                  hoverColor: Colors.transparent,
                  border: OutlineInputBorder(
                    borderRadius: m.brInput,
                    borderSide: BorderSide(color: c.inputBorder),
                  ),
                  enabledBorder: OutlineInputBorder(
                    borderRadius: m.brInput,
                    borderSide: BorderSide(color: c.inputBorder),
                  ),
                  focusedBorder: OutlineInputBorder(
                    borderRadius: m.brInput,
                    borderSide: BorderSide(color: c.inputFocusBorder),
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
                  QuickSectionLabel(text: s.saveDir, c: c),
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
                  QuickSectionLabel(text: s.threads, c: c),
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
          QuickSectionLabel(text: s.filenameOptional, c: c),
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
              QuickSectionLabel(text: s.taskProxy, c: c),
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
          QuickSectionLabel(text: s.userAgent, c: c),
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
                    return Text(label, overflow: TextOverflow.ellipsis, maxLines: 1);
                  },
                  onChanged: (v) {
                    if (v == null) return;
                    setState(() => _selectedUaPreset = v);
                    final preset = kQuickUaPresets[v];
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
          // Cookie — 预填浏览器捕获值，可编辑覆盖
          const SizedBox(height: 10),
          QuickSectionLabel(text: s.taskCookie, c: c),
          const SizedBox(height: 4),
          Text(
            s.taskCookieDesc,
            style: TextStyle(fontSize: 11, color: c.textMuted),
          ),
          const SizedBox(height: 6),
          ShadInput(
            controller: _cookieController,
            placeholder: Text(s.taskCookiePlaceholder),
            maxLines: 2,
          ),
          // 哈希校验 — 仅单条链接时显示（批量走 URL 行内 checksum= 选项）
          if (!_isBatch) ...[
            const SizedBox(height: 10),
            QuickSectionLabel(text: s.taskChecksum, c: c),
            const SizedBox(height: 4),
            Text(
              s.taskChecksumDesc,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
            const SizedBox(height: 6),
            Row(
              children: [
                SizedBox(
                  width: 110,
                  child: ShadSelect<String>(
                    initialValue: _selectedHashAlgo,
                    options: const [
                      ShadOption(value: 'md5', child: Text('md5')),
                      ShadOption(value: 'sha-1', child: Text('sha-1')),
                      ShadOption(value: 'sha-256', child: Text('sha-256')),
                      ShadOption(value: 'sha-512', child: Text('sha-512')),
                    ],
                    selectedOptionBuilder: (context, value) => Text(
                      value,
                      overflow: TextOverflow.ellipsis,
                      maxLines: 1,
                    ),
                    onChanged: (algo) {
                      if (algo == null) return;
                      setState(() => _selectedHashAlgo = algo);
                    },
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ShadInput(
                    controller: _checksumController,
                    placeholder: Text(s.taskChecksumPlaceholder),
                  ),
                ),
              ],
            ),
          ],
        ],

        // 底部动作按钮（取消 / 开始下载）
        const SizedBox(height: 16),
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            ShadButton.outline(
              onPressed: widget.onCancel,
              child: Text(s.cancel),
            ),
            const SizedBox(width: 8),
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
        ),
      ],
    );
  }

  Widget _buildQueueSelector(S s, AppColors c) {
    final queues = widget.host.queues;
    if (queues.isEmpty) return const SizedBox.shrink();

    final allOptions = <QuickQueueOption>[
      const QuickQueueOption(queueId: '', name: ''),
      ...queues,
    ];

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const SizedBox(height: 14),
        QuickSectionLabel(text: s.taskQueueLabel, c: c),
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

/// 信息标签（文件大小 / MIME 类型）— 对话框标题与小窗标题栏共用
class QuickInfoTag extends StatelessWidget {
  final String text;
  final AppColors c;

  const QuickInfoTag({super.key, required this.text, required this.c});

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: c.surface2,
        borderRadius: m.brSm,
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
class QuickSectionLabel extends StatelessWidget {
  final String text;
  final AppColors c;

  const QuickSectionLabel({super.key, required this.text, required this.c});

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

/// 文件大小格式化（信息标签用）
String formatQuickFileSize(int bytes, {required String unknownLabel}) {
  if (bytes <= 0) return unknownLabel;
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  int unitIndex = 0;
  double size = bytes.toDouble();
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex++;
  }
  return '${size.toStringAsFixed(unitIndex == 0 ? 0 : 1)} ${units[unitIndex]}';
}
