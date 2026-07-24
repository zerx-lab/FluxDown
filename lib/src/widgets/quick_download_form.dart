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
import 'package:flutter/services.dart' show LogicalKeyboardKey;
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'flux_sonner.dart';

import '../i18n/locale_provider.dart';
import '../models/download_queue.dart';
import '../models/ua_presets.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import 'dir_picker_field.dart';
import 'split_action_button.dart';
import 'thread_selector.dart';

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

/// QuickQueueOption 显示名 — 内置队列本地化，规则与
/// `download_queue.dart` 的 `queueDisplayName` 一致；QuickQueueOption 与
/// DownloadQueue 解耦（独立小窗引擎无 DownloadQueue），故本函数单独定义。
String _quickQueueDisplayName(S s, QuickQueueOption q) => switch (q.queueId) {
  kMainQueueId => s.mainQueue,
  kLaterQueueId => s.laterQueue,
  _ => q.name,
};

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

  /// 是否忽略当前任务的 HTTPS 证书错误。默认 false。
  final bool ignoreTlsErrors;

  /// 用户是否手动改过线程数（决定是否记忆本次选择）
  final bool threadsUserModified;

  /// 音视频轨对的音频轨 URL（外部请求透传，表单不可编辑；空 = 普通下载）。
  /// 仅单条时有意义，非空时 url 是视频轨、本字段是音频轨。
  final String audioUrl;

  /// 自定义请求头（key 非空才保留；同名后者覆盖）。
  /// 单条经 ConfirmExternalDownload、批量经 BatchCreateTask 透传。
  final Map<String, String> extraHeaders;

  /// 「稍后下载」提交 — 建任务但不启动（透传为 startPaused）。
  final bool startLater;

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
    this.ignoreTlsErrors = false,
    required this.threadsUserModified,
    this.audioUrl = '',
    this.extraHeaders = const {},
    this.startLater = false,
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

/// 表单外部控制器 — 供宿主向**已挂载**的表单追加 URL（独立小窗 append
/// 模式：小窗可见期间新到的外部请求合入当前表单，而不是重置/丢弃）。
///
/// 生命周期与 TextEditingController 类似：由调用方持有，表单在
/// initState/dispose 中自行挂接/解除。表单未挂载时调用是安全的空操作。
class QuickDownloadFormController {
  _QuickDownloadFormState? _state;

  /// 把 [urlText]（可多行）中尚未出现在表单里的 URL 追加到 URL 输入框。
  /// 返回实际追加的条数（0 = 全部重复或无有效 URL 或表单未挂载）。
  int appendUrls(String urlText) => _state?._appendUrls(urlText) ?? 0;
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

  /// 可选外部控制器（独立小窗 append 模式用；主窗口对话框不传）。
  final QuickDownloadFormController? controller;

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
    this.controller,
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

  /// 自定义请求头行列表（与主窗口新建下载对话框同款交互）。
  final List<QuickHeaderRow> _headerRows = [];

  /// 选中的哈希算法（与后端 verify_checksum 支持的算法名一致）
  String _selectedHashAlgo = 'sha-256';
  String? selectedThreads;
  String _selectedUaPreset = 'default';

  /// 选中的队列 ID（空字符串 = 默认队列）
  late String _selectedQueueId;

  /// 用户是否手动修改过线程数（用于判断切换队列时是否需要自动更新）
  bool _threadsUserModified = false;

  /// 是否展开高级选项（含任务代理）
  bool _showAdvanced = false;

  /// 当前任务是否显式忽略 HTTPS 证书错误。安全默认值为 false。
  bool _ignoreTlsErrors = false;

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
    _selectedQueueId = widget.defaultQueueId.isEmpty
        ? kMainQueueId
        : widget.defaultQueueId;
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
    widget.controller?._state = this;
  }

  @override
  void didUpdateWidget(covariant QuickDownloadForm oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.controller, widget.controller)) {
      if (identical(oldWidget.controller?._state, this)) {
        oldWidget.controller?._state = null;
      }
      widget.controller?._state = this;
    }
  }

  /// 追加外部请求带来的新 URL（见 [QuickDownloadFormController.appendUrls]）。
  /// 已存在的 URL 去重跳过；追加不重置用户已填的其他字段。
  int _appendUrls(String urlText) {
    final incoming = parseQuickDownloadEntries(urlText);
    if (incoming.isEmpty) return 0;
    final existing = parseQuickDownloadEntries(
      _urlController.text,
    ).map((e) => e.url).toSet();
    final fresh = incoming.where((e) => !existing.contains(e.url)).toList();
    if (fresh.isEmpty) return 0;
    final buffer = StringBuffer(_urlController.text.trimRight());
    for (final entry in fresh) {
      if (buffer.isNotEmpty) buffer.write('\n');
      buffer.write(entry.url);
      if (entry.fileName.isNotEmpty) {
        buffer.write('\n out=${entry.fileName}');
      }
    }
    _urlController.text = buffer.toString();
    return fresh.length;
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
    if (identical(widget.controller?._state, this)) {
      widget.controller?._state = null;
    }
    _urlController.removeListener(_onUrlChanged);
    _urlController.dispose();
    _urlFocusNode.dispose();
    _saveDirController.dispose();
    _cookieController.dispose();
    for (final row in _headerRows) {
      row.dispose();
    }
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
    FluxSonner.of(context).show(ShadToast.destructive(title: Text(message)));
  }

  bool get _isBatch => _urlCount > 1;

  void _startDownload({bool startLater = false, String? queueOverride}) {
    final saveDir = _saveDirController.text.trim();
    if (saveDir.isEmpty) return;

    // 队列归属挂在动作按钮上（表单不再有队列字段）：箭头菜单显式指定 >
    // 动作默认——稍后下载 → 「稍后下载」队列；开始下载 → 默认队列。
    final queueId =
        queueOverride ?? (startLater ? kLaterQueueId : _selectedQueueId);

    final entries = parseQuickDownloadEntries(_urlController.text);
    if (entries.isEmpty) return;

    final parsedSeg = int.tryParse(selectedThreads ?? '') ?? 0;
    final segments = parsedSeg > 0 ? parsedSeg.clamp(1, 256) : 0;

    // 高级选项手填的校验值拼成 aria2 风格 "algo=hexhash"；
    // 为空则由提交器回退到 URL 文本里的 checksum= 选项行。
    final hash = _checksumController.text.trim();
    final checksum = hash.isEmpty ? '' : '$_selectedHashAlgo=$hash';

    // 自定义请求头：仅保留 key 非空的行，同名 key 后者覆盖前者。
    final extraHeaders = <String, String>{};
    for (final row in _headerRows) {
      final key = row.keyController.text.trim();
      if (key.isEmpty) continue;
      extraHeaders[key] = row.valueController.text.trim();
    }

    widget.onSubmit(
      QuickDownloadFormResult(
        urlText: _urlController.text,
        saveDir: saveDir,
        rename: _renameController.text.trim(),
        segments: segments,
        proxyUrl: _proxyUrlController.text.trim(),
        userAgent: _userAgentController.text.trim(),
        queueId: queueId,
        startLater: startLater,
        cookies: _cookieController.text.trim(),
        checksum: checksum,
        ignoreTlsErrors: _ignoreTlsErrors,
        threadsUserModified: _threadsUserModified,
        audioUrl: widget.initialAudioUrl,
        extraHeaders: extraHeaders,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final m = AppMetrics.of(context);

    // Ctrl+Enter（macOS 加 Cmd+Enter）快速提交——小窗确认场景的高频路径；
    // 焦点在任意输入框（含多行 URL 框）时均可触发，纯 Enter 不受影响。
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.enter, control: true):
            _startDownload,
        const SingleActivator(LogicalKeyboardKey.enter, meta: true):
            _startDownload,
      },
      child: Column(
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
            const SizedBox(height: 12),
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        s.taskIgnoreTlsErrors,
                        style: TextStyle(
                          fontSize: 12,
                          fontWeight: FontWeight.w500,
                          color: c.textPrimary,
                        ),
                      ),
                      const SizedBox(height: 2),
                      Text(
                        s.taskIgnoreTlsErrorsDesc,
                        style: TextStyle(fontSize: 11, color: c.textMuted),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                ShadSwitch(
                  value: _ignoreTlsErrors,
                  onChanged: (value) =>
                      setState(() => _ignoreTlsErrors = value),
                ),
              ],
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
                    options: [
                      ShadOption(
                        value: 'default',
                        child: Text(s.queueUaInheritGlobal),
                      ),
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
                        value: 'safari',
                        child: Text(s.userAgentPresetSafari),
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
                        'safari' => 'Safari',
                        'custom' => s.userAgentPresetCustom,
                        _ => s.queueUaInheritGlobal,
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
                      if (v != 'custom') {
                        _userAgentController.text = kUaPresets[v] ?? '';
                      }
                    },
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ShadInput(
                    controller: _userAgentController,
                    placeholder: Text(s.userAgentTaskPlaceholder),
                    onChanged: (value) {
                      final detected = detectUaPreset(value);
                      if (detected != _selectedUaPreset) {
                        setState(() => _selectedUaPreset = detected);
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
              // 批量模式下空 Cookie 框语义不同(留空 = 保留各条目自己捕获的
              // Cookie,由 Rust 侧按 URL 缓存恢复;填写 = 批级覆盖),动态提示
              // 消除歧义。单条模式:留空 = 不发送(用户清空预填即生效)。
              _urlCount > 1 ? s.taskCookieBatchDesc : s.taskCookieDesc,
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
            // 自定义请求头（与主窗口新建下载对话框对齐）
            const SizedBox(height: 10),
            QuickSectionLabel(text: s.taskHeaders, c: c),
            const SizedBox(height: 4),
            Text(
              s.taskHeadersDesc,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
            const SizedBox(height: 6),
            for (int hi = 0; hi < _headerRows.length; hi++) ...[
              if (hi > 0) const SizedBox(height: 6),
              Row(
                children: [
                  Expanded(
                    flex: 2,
                    child: ShadInput(
                      controller: _headerRows[hi].keyController,
                      placeholder: Text(s.taskHeadersKeyPlaceholder),
                    ),
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    flex: 3,
                    child: ShadInput(
                      controller: _headerRows[hi].valueController,
                      placeholder: Text(s.taskHeadersValuePlaceholder),
                    ),
                  ),
                  const SizedBox(width: 4),
                  GestureDetector(
                    onTap: () => setState(() {
                      _headerRows.removeAt(hi).dispose();
                    }),
                    child: Icon(LucideIcons.x, size: 16, color: c.textMuted),
                  ),
                ],
              ),
            ],
            const SizedBox(height: 6),
            Align(
              alignment: Alignment.centerLeft,
              child: ShadButton.ghost(
                size: ShadButtonSize.sm,
                onPressed: () =>
                    setState(() => _headerRows.add(QuickHeaderRow())),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(LucideIcons.plus, size: 13, color: c.accent),
                    const SizedBox(width: 6),
                    Text(
                      s.taskHeadersAdd,
                      style: TextStyle(fontSize: 12, color: c.accent),
                    ),
                  ],
                ),
              ),
            ),
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
              SplitActionButton(
                icon: LucideIcons.clock,
                label: s.downloadLater,
                tooltip: s.laterIntoQueueTooltip(s.laterQueue),
                onPressed: () => _startDownload(startLater: true),
                onPickQueue: (anchor) => _showQueueMenu(anchor, later: true),
              ),
              const SizedBox(width: 8),
              SplitActionButton(
                primary: true,
                icon: LucideIcons.download,
                label: _isBatch
                    ? s.startBatchDownload(_urlCount)
                    : s.startDownload,
                tooltip: s.startIntoQueueTooltip(_defaultTargetName(s)),
                onPressed: () => _startDownload(),
                onPickQueue: (anchor) => _showQueueMenu(anchor, later: false),
              ),
            ],
          ),
        ],
      ),
    );
  }

  /// 默认目标队列的显示名（「开始下载」tooltip 用）。
  String _defaultTargetName(S s) {
    final q = widget.host.queues
        .where((q) => q.queueId == _selectedQueueId)
        .firstOrNull;
    return q == null ? s.mainQueue : _quickQueueDisplayName(s, q);
  }

  /// 在动作按钮箭头下方弹队列菜单：选择即提交（[later] 决定是否以
  /// 暂停态创建）。菜单是动作列表而非选择器——不保留选中态。
  void _showQueueMenu(BuildContext anchor, {required bool later}) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final queues = widget.host.queues;
    if (queues.isEmpty) {
      _startDownload(startLater: later);
      return;
    }
    final box = anchor.findRenderObject();
    if (box is! RenderBox || !box.hasSize) return;
    final origin = box.localToGlobal(Offset(0, box.size.height + 6));
    showContextMenu(
      context,
      origin,
      items: [
        for (final q in queues)
          ContextMenuItem(
            icon: q.queueId == kLaterQueueId
                ? LucideIcons.clock
                : LucideIcons.layers,
            label: _quickQueueDisplayName(s, q),
            color: c.textPrimary,
            action: () =>
                _startDownload(startLater: later, queueOverride: q.queueId),
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
      decoration: BoxDecoration(color: c.surface2, borderRadius: m.brSm),
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

/// 自定义请求头的一行输入：持有 key / value 两个文本控制器。
class QuickHeaderRow {
  final TextEditingController keyController = TextEditingController();
  final TextEditingController valueController = TextEditingController();

  void dispose() {
    keyController.dispose();
    valueController.dispose();
  }
}
