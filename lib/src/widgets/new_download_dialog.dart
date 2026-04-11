import 'dart:async';
import 'dart:io';

import '../services/file_picker_service.dart';
import 'package:flutter/material.dart'
    show
        AdaptiveTextSelectionToolbar,
        CircularProgressIndicator,
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
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';

import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';
import '../services/bt_file_selection_service.dart';

import 'bt_file_list_widget.dart';
import 'dir_picker_field.dart';
import 'thread_selector.dart';

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
  final _userAgentController = TextEditingController();
  String? selectedThreads;
  String _selectedUaPreset = 'custom';

  /// 选中的队列 ID（空字符串 = 默认队列）
  String _selectedQueueId = '';

  /// 用户是否手动修改过线程数（用于判断切换队列时是否需要自动更新）
  bool _threadsUserModified = false;

  /// 线程选择器的 key 版本，切换队列时递增以强制重建 ShadSelect
  int _threadsSelectVersion = 0;

  /// 是否展开高级选项（含任务代理）
  bool _showAdvanced = false;

  /// 防止双击重复提交
  bool _isSubmitting = false;

  /// 解析出的有效 URL 数量（实时计算）
  int _urlCount = 0;

  /// 是否所有链接都是 magnet
  bool _allMagnet = false;

  /// 已选择的 .torrent 文件路径列表（单次只支持一个，批量 torrent 通过多次添加实现）
  final List<String> _torrentFilePaths = [];

  /// 防止重复打开文件选择器
  bool _isPicking = false;

  /// 用户是否手动通过文件选择器修改过保存目录（是则不再自动覆盖）
  bool _saveDirUserModified = false;

  // ── torrent 文件预解析状态 ──────────────────────────────────────────────────

  /// 当前正在解析的 probe_id → torrent 路径映射（一次只解析一个）
  String? _probingPath;

  /// 解析结果：路径 → TorrentMetaResult
  final Map<String, TorrentMetaResult> _torrentMeta = {};

  /// 解析进行中（显示 loading）
  bool _isProbing = false;

  /// 解析错误消息（非空时显示）
  String _probeError = '';

  /// 每个 torrent 文件的文件勾选状态：路径 → 已选 index 集合
  final Map<String, Set<int>> _torrentSelections = {};

  /// TorrentMetaResult 信号订阅
  StreamSubscription<RustSignalPack<TorrentMetaResult>>? _metaSub;

  // ── 磁力链接等待文件列表状态机 ─────────────────────────────────────────────
  // 状态：null = 普通模式；'probing' = 已创建任务正在等待 DHT 解析；
  //        'selecting' = 文件列表已到达，等待用户选择
  String? _btWaitPhase; // null | 'probing' | 'selecting'

  /// 收到 BtFilesInfo 后记录的真实 task_id（用于发送 SelectBtFiles）
  String? _btPendingTaskId;

  /// 收到的 BT 文件列表（Phase=selecting 时非空）
  List<BtFileEntry> _btFiles = [];

  /// 用户在对话框内对 BT 文件的勾选状态
  Set<int> _btSelectedIndices = {};

  /// 用户在 probing 阶段（task_id 尚未知）点了取消，或对话框被关闭。
  /// 下次收到 BtFilesInfo 时立刻发 [-1] 让 Rust 暂停任务。
  bool _btCancelPending = false;




  /// 根据队列 ID 计算有效的线程数选项字符串。
  ///
  /// 优先级：自定义队列的 defaultSegments → 全局 defaultSegments → null（Auto）
  String? _effectiveSegmentsOption(String queueId) {
    if (queueId.isNotEmpty) {
      final queue = widget.controller.queues
          .where((q) => q.queueId == queueId)
          .firstOrNull;
      if (queue != null && queue.defaultSegments > 0) {
        return queue.defaultSegments.toString();
      }
    }
    final global = widget.settingsProvider.defaultSegments;
    return global > 0 ? global.toString() : null;
  }

  @override
  void initState() {
    super.initState();
    _saveDirController.text = widget.settingsProvider.defaultSaveDir;
    _urlController.addListener(_onUrlChanged);
    _pasteUrlFromClipboard();
    // 优先使用侧边栏队列筛选，否则使用设置中的默认队列
    final qf = widget.controller.queueFilter;
    _selectedQueueId = qf ?? widget.settingsProvider.defaultQueueId;
    // 根据队列/全局设置初始化默认线程数
    selectedThreads = _effectiveSegmentsOption(_selectedQueueId);
    // 订阅 torrent meta 解析结果（.torrent 文件预解析）
    _metaSub = TorrentMetaResult.rustSignalStream.listen(_onTorrentMetaResult);
  }

  /// 由 [BtFileSelectionService] 回调：DHT 解析完成，文件列表已就绪。
  void _onBtFilesInfoReceived(BtFilesInfo msg) {
    // 用户已取消（probing 阶段点取消、或对话框被关闭）：
    // 立刻发 [-1] 让 Rust 将任务暂停，不展示文件列表。
    if (_btCancelPending || !mounted || _btWaitPhase != 'probing') {
      SelectBtFiles(
        taskId: msg.taskId,
        selectedIndices: const [-1],
      ).sendSignalToRust();
      return;
    }
    setState(() {
      _btPendingTaskId = msg.taskId;
      _btWaitPhase = 'selecting';
      _btFiles = msg.files;
      _btSelectedIndices = msg.files.map((f) => f.index.toInt()).toSet();
    });
  }



  void _onTorrentMetaResult(RustSignalPack<TorrentMetaResult> pack) {
    final msg = pack.message;
    // probeId 就是文件路径（_probeTorrentFile 里以 path 作为 probeId）
    final path = msg.probeId;
    // 只处理本对话框发出的 probe（路径必须在当前列表中）
    if (!_torrentFilePaths.contains(path)) return;
    if (!mounted) return;
    setState(() {
      if (_probingPath == path) {
        _isProbing = false;
        _probingPath = null;
      }
      if (msg.error.isNotEmpty) {
        _probeError = msg.error;
      } else {
        _probeError = '';
        _torrentMeta[path] = msg;
        // 默认全选
        _torrentSelections[path] =
            msg.files.map((f) => f.index.toInt()).toSet();
      }
    });
  }

  void _onUrlChanged() {
    final entries = _parseEntries(_urlController.text);
    final count = entries.length;
    final allMagnet =
        entries.isNotEmpty &&
        entries.every((e) => e.url.toLowerCase().startsWith('magnet:'));
    if (count != _urlCount || allMagnet != _allMagnet) {
      setState(() {
        _urlCount = count;
        _allMagnet = allMagnet;
      });
    }
    // 自动从 URL 提取文件名并匹配分类保存目录
    if (entries.isNotEmpty &&
        !entries.first.url.toLowerCase().startsWith('magnet:')) {
      final fileName = _extractFilenameFromUrl(entries.first.url);
      _tryAutoApplySaveDir(fileName);
    }
  }

  /// 将 [_ParsedEntry] 转换回 aria2 风格文本（含 out= / checksum= 选项行）。
  static String _entryToText(_ParsedEntry e) {
    final buf = StringBuffer()..write(e.url);
    if (e.fileName.isNotEmpty) buf.write('\n  out=${e.fileName}');
    if (e.checksum.isNotEmpty) buf.write('\n  checksum=${e.checksum}');
    return buf.toString();
  }

  /// 从文本解析 aria2 风格的下载条目列表。
  ///
  /// 支持格式：
  /// ```
  /// https://example.com/file.zip
  ///   out=myname.zip
  ///   checksum=sha-256=abc123...
  ///
  /// # 注释行（忽略）
  /// https://example.com/plain.zip
  /// ```
  ///
  /// [loose] 为 true 时从行内任意位置提取 URL，适合 TXT 文件导入；
  /// 默认严格模式要求 URL 位于行首，适合手动输入。
  static List<_ParsedEntry> _parseEntries(String text, {bool loose = false}) {
    final lines = text.split('\n');
    final entries = <_ParsedEntry>[];
    _ParsedEntry? current;
    final pattern = RegExp(r'(https?|ftp)://\S+', caseSensitive: false);
    final strictPattern = RegExp(r'^(https?|ftp)://\S+', caseSensitive: false);

    for (final line in lines) {
      // 选项行：原始行以空格或 Tab 开头
      if (line.startsWith(' ') || line.startsWith('\t')) {
        if (current == null) continue;
        final trimmed = line.trim();
        if (trimmed.startsWith('out=')) {
          current = _ParsedEntry(
            current.url,
            fileName: trimmed.substring(4),
            checksum: current.checksum,
          );
        } else if (trimmed.startsWith('checksum=')) {
          current = _ParsedEntry(
            current.url,
            fileName: current.fileName,
            checksum: trimmed.substring(9),
          );
        }
        continue;
      }

      final trimmed = line.trim();
      if (trimmed.isEmpty) continue;
      if (trimmed.startsWith('#')) continue; // 注释行

      // 新 URL 行：先把上一个入队
      if (current != null) {
        entries.add(current);
        current = null;
      }

      final lower = trimmed.toLowerCase();
      final magnetIdx = lower.indexOf('magnet:?');
      if (magnetIdx != -1) {
        current = _ParsedEntry(trimmed.substring(magnetIdx));
      } else if (loose) {
        // loose 模式取行内第一个 URL 并设为 current，使后续选项行（out=/checksum=）
        // 能正常附着。直接 add 会跳过 current，导致 TXT 导入时选项全部丢失。
        final match = pattern.firstMatch(trimmed);
        if (match != null) {
          final url = _trimUrlTail(match.group(0)!);
          if (url.isNotEmpty) current = _ParsedEntry(url);
        }
      } else {
        final match = strictPattern.firstMatch(trimmed);
        if (match != null) {
          current = _ParsedEntry(match.group(0)!);
        }
      }
    }
    if (current != null) entries.add(current);
    return entries;
  }

  /// 去掉 URL 末尾常见标点（TXT 文本中 URL 后可能跟随句号/逗号等）
  static String _trimUrlTail(String url) =>
      url.replaceAll(RegExp(r'[.,;:!?()\[\]{}]+$'), '');

  /// 读取剪切板内容，自动填入所有识别到的条目（支持 aria2 格式）
  Future<void> _pasteUrlFromClipboard() async {
    try {
      final data = await Clipboard.getData(Clipboard.kTextPlain);
      if (!mounted) return;
      if (data == null || data.text == null) return;
      final text = data.text!.trim();

      final entries = _parseEntries(text);
      if (entries.isEmpty) return;

      // 直接保留原始文本（含 aria2 选项行）
      _urlController.text = text;
    } catch (_) {
      // 剪切板访问失败时静默忽略
    }
  }

  @override
  void dispose() {
    // selecting 阶段：已拿到 task_id，直接发 [-1] 让 Rust 暂停任务
    if (_btWaitPhase == 'selecting' && _btPendingTaskId != null) {
      SelectBtFiles(
        taskId: _btPendingTaskId!,
        selectedIndices: const [-1],
      ).sendSignalToRust();
      BtFileSelectionService.registerPendingHandler(null);
    } else if (_btWaitPhase == 'probing') {
      // probing 阶段：task_id 尚未知，标记取消，让回调在收到信号时发 [-1]
      // _onBtFilesInfoReceived 检查 _btCancelPending，即使 mounted=false 也能拦截
      _btCancelPending = true;
      // 不清除 Service 回调——让信号路由过来，回调发 [-1] 后 Rust 暂停任务
    } else {
      // 普通关闭，清除任何残留的 Service 回调
      BtFileSelectionService.registerPendingHandler(null);
    }
    _metaSub?.cancel();
    _urlController.removeListener(_onUrlChanged);
    _urlController.dispose();
    _urlFocusNode.dispose();
    _saveDirController.dispose();
    _renameController.dispose();
    _proxyUrlController.dispose();
    _userAgentController.dispose();
    super.dispose();
  }

  Future<void> _pickTorrentFiles() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await FilePickerService.pickFiles(
        dialogTitle: currentS.selectTorrentFile,
        allowedExtensions: ['torrent'],
        allowMultiple: true,
      );
      if (result != null && result.isNotEmpty && mounted) {
        setState(() {
          for (final file in result) {
            if (!_torrentFilePaths.contains(file.path)) {
              _torrentFilePaths.add(file.path);
            }
          }
        });
        // 自动解析最后一个新添加的 torrent 文件
        final newPath = _torrentFilePaths.last;
        if (!_torrentMeta.containsKey(newPath)) {
          await _probeTorrentFile(newPath);
        }
      }
    } on FilePickerException catch (e) {
      if (mounted) _showPickerError(e);
    } finally {
      if (mounted) setState(() => _isPicking = false);
    }
  }

  /// 发送 ProbeTorrentMeta 信号，触发 Rust 本地解析 .torrent 文件内容
  Future<void> _probeTorrentFile(String path) async {
    if (!mounted) return;
    try {
      final bytes = await File(path).readAsBytes();
      if (!mounted) return;
      setState(() {
        _isProbing = true;
        _probeError = '';
        _probingPath = path;
      });
      ProbeTorrentMeta(
        probeId: path,
        torrentBytes: bytes,
      ).sendSignalToRust();
    } catch (e) {
      if (mounted) {
        setState(() {
          _isProbing = false;
          _probeError = e.toString();
        });
      }
    }
  }

  void _removeTorrentFile(int index) {
    final path = _torrentFilePaths[index];
    setState(() {
      _torrentFilePaths.removeAt(index);
      _torrentMeta.remove(path);
      _torrentSelections.remove(path);
      if (_probingPath == path) {
        _probingPath = null;
        _isProbing = false;
      }
    });
  }

  /// 从 TXT 文件中导入链接，支持多文件选择
  Future<void> _importFromTxt() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await FilePickerService.pickFiles(
        dialogTitle: currentS.importTxtFile,
        allowedExtensions: ['txt', 'text'],
        allowMultiple: true,
      );
      if (result == null || result.isEmpty || !mounted) return;

      final imported = <_ParsedEntry>[];
      for (final file in result) {
        try {
          final content = await File(file.path).readAsString();
          imported.addAll(_parseEntries(content, loose: true));
        } catch (_) {
          // 单文件读取失败时跳过，继续处理其他文件
        }
      }

      if (!mounted) return;

      if (imported.isEmpty) {
        ShadSonner.of(
          context,
        ).show(ShadToast(title: Text(currentS.importTxtNoUrls)));
        return;
      }

      // 追加到已有内容，按 URL 去重，保留 fileName / checksum
      final existing = _parseEntries(_urlController.text);
      final existingUrls = existing.map((e) => e.url).toSet();
      final toAdd = imported.where((e) => !existingUrls.contains(e.url));
      final merged = [...existing, ...toAdd];
      _urlController.text = merged.map(_entryToText).join('\n');

      ShadSonner.of(
        context,
      ).show(ShadToast(title: Text(currentS.importTxtFound(imported.length))));
    } on FilePickerException catch (e) {
      if (mounted) _showPickerError(e);
    } finally {
      if (mounted) setState(() => _isPicking = false);
    }
  }

  /// 根据文件名尝试自动匹配分类的保存目录。
  /// 只在用户未手动修改过保存目录时生效。
  void _tryAutoApplySaveDir(String fileName) {
    if (fileName.isEmpty || _saveDirUserModified) return;
    final categories =
        widget.settingsProvider.customCategories
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

  /// 从 URL 中提取文件名（取最后一段路径，必须包含 '.'）
  static String _extractFilenameFromUrl(String url) {
    try {
      final uri = Uri.parse(url.trim());
      final segments = uri.pathSegments;
      if (segments.isNotEmpty) {
        final last = Uri.decodeComponent(segments.last);
        if (last.contains('.')) return last;
      }
    } catch (_) {}
    return '';
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
  bool get _hasTorrentFiles => _torrentFilePaths.isNotEmpty;

  /// Build the UI block for a single .torrent entry at index [ti].
  Widget _buildTorrentFileEntry(int ti, AppColors c, S s) {
    final path = _torrentFilePaths[ti];
    final fileName = File(path).uri.pathSegments.last;
    final meta = _torrentMeta[path];
    final selection = _torrentSelections[path];
    final isCurrentlyProbing = _isProbing && _probingPath == path;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // ── Header row: name + size + remove ──────────────────────────────
        Row(
          children: [
            Icon(LucideIcons.fileDown, size: 13, color: c.accent),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                meta != null ? meta.name : fileName,
                style: TextStyle(
                  fontSize: 12.5,
                  fontWeight: FontWeight.w500,
                  color: c.textPrimary,
                ),
                overflow: TextOverflow.ellipsis,
                maxLines: 1,
              ),
            ),
            if (meta != null) ...[
              Text(
                formatBtFileSize(meta.totalBytes.toInt()),
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
              const SizedBox(width: 8),
            ],
            GestureDetector(
              onTap: () => _removeTorrentFile(ti),
              child: Icon(LucideIcons.x, size: 14, color: c.textMuted),
            ),
          ],
        ),
        const SizedBox(height: 6),
        // ── Loading indicator ──────────────────────────────────────────────
        if (isCurrentlyProbing)
          Container(
            padding: const EdgeInsets.symmetric(vertical: 20),
            alignment: Alignment.center,
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                SizedBox(
                  width: 14,
                  height: 14,
                  child: CircularProgressIndicator(
                    strokeWidth: 2,
                    color: c.accent,
                  ),
                ),
                const SizedBox(width: 8),
                Text(
                  s.btProbing,
                  style: TextStyle(fontSize: 12, color: c.textMuted),
                ),
              ],
            ),
          )
        // ── Parse error ────────────────────────────────────────────────────
        else if (_probeError.isNotEmpty && meta == null)
          Container(
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              color: c.statusError.withValues(alpha: 0.08),
              borderRadius: BorderRadius.circular(8),
              border: Border.all(
                color: c.statusError.withValues(alpha: 0.3),
              ),
            ),
            child: Row(
              children: [
                Icon(LucideIcons.circleAlert, size: 13, color: c.statusError),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    s.btProbeError,
                    style: TextStyle(fontSize: 12, color: c.statusError),
                  ),
                ),
              ],
            ),
          )
        // ── File list (parsed successfully) ───────────────────────────────
        else if (meta != null && selection != null)
          BtFileListWidget(
            files: meta.files,
            selectedIndices: selection,
            onToggleAll: () {
              setState(() {
                if (selection.length == meta.files.length) {
                  _torrentSelections[path] = {};
                } else {
                  _torrentSelections[path] =
                      meta.files.map((f) => f.index.toInt()).toSet();
                }
              });
            },
            onToggleFile: (idx) {
              setState(() {
                final current = _torrentSelections[path] ?? {};
                if (current.contains(idx)) {
                  _torrentSelections[path] = Set.from(current)..remove(idx);
                } else {
                  _torrentSelections[path] = Set.from(current)..add(idx);
                }
              });
            },
            maxHeight: 260,
          ),
        if (ti < _torrentFilePaths.length - 1) const SizedBox(height: 14),
      ],
    );
  }

  /// 构建下载按钮的标签文字。
  ///
  /// - torrent 已全部解析完成：显示「下载 N 个文件（X MB）」
  /// - torrent 解析中：显示「解析中...」
  /// - torrent 未解析（如解析失败）：显示「开始下载 N 个」
  /// - 普通 URL 批量：显示「下载 N 个文件」
  /// - 普通 URL 单条：显示「开始下载」
  /// 计算 BT 等待阶段用户已选文件的总大小
  int get _btSelectedTotalBytes {
    int total = 0;
    for (final f in _btFiles) {
      if (_btSelectedIndices.contains(f.index.toInt())) {
        total += f.size.toInt();
      }
    }
    return total;
  }

  String _buildStartButtonLabel(S s) {
    if (_hasTorrentFiles) {
      if (_isProbing) return s.btProbing;
      // 统计所有已解析 torrent 中用户选中的文件总数和总大小
      int totalSelected = 0;
      int totalBytes = 0;
      bool allProbed = true;
      for (final path in _torrentFilePaths) {
        final meta = _torrentMeta[path];
        final sel = _torrentSelections[path];
        if (meta == null) {
          allProbed = false;
          continue;
        }
        if (sel != null) {
          totalSelected += sel.length;
          for (final f in meta.files) {
            if (sel.contains(f.index.toInt())) {
              totalBytes += f.size.toInt();
            }
          }
        }
      }
      if (allProbed && totalSelected > 0) {
        return s.btStartWithSelection(
          totalSelected,
          formatBtFileSize(totalBytes),
        );
      }
      return s.startBatchDownload(_torrentFilePaths.length);
    }
    if (_isBatch) return s.startBatchDownload(_urlCount);
    return s.startDownload;
  }

  /// 当前所有 torrent 文件是否都已解析完成（或解析失败）
  bool get _allTorrentsProbed =>
      !_isProbing &&
      _torrentFilePaths.every(
        (p) => _torrentMeta.containsKey(p) || _probeError.isNotEmpty,
      );

  /// 用户是否已从所有 torrent 中选择了至少一个文件
  bool get _hasAnyTorrentSelection =>
      _torrentFilePaths.any((p) {
        final sel = _torrentSelections[p];
        return sel != null && sel.isNotEmpty;
      });

  Future<void> _startDownload() async {
    if (_isSubmitting) return;
    setState(() => _isSubmitting = true);

    try {
      await _startDownloadInner();
    } finally {
      if (mounted) setState(() => _isSubmitting = false);
    }
  }

  Future<void> _startDownloadInner() async {
    final saveDir = _saveDirController.text.trim();
    if (saveDir.isEmpty) return;

    final proxyUrl = _proxyUrlController.text.trim();
    final userAgent = _userAgentController.text.trim();

    // Handle .torrent file downloads
    if (_hasTorrentFiles) {
      for (final path in _torrentFilePaths) {
        final meta = _torrentMeta[path];
        final selection = _torrentSelections[path];
        if (meta != null && selection != null) {
          // Already probed: send torrent bytes with pre-selected file indices
          // so Rust skips the second file-selection dialog entirely.
          final selectedIndices = selection.toList()..sort();
          await DownloadController.sendTorrentFileSignal(
            path,
            saveDir,
            proxyUrl: proxyUrl,
            userAgent: userAgent,
            queueId: _selectedQueueId,
            selectedFileIndices: selectedIndices,
            torrentName: meta.name,
          );
        } else {
          // Probe not yet complete (e.g. user clicked too fast, or parse
          // failed): fall back to the legacy path; Rust will show the
          // file-selection dialog after metadata resolves.
          await widget.controller.createTaskFromTorrentFile(
            torrentFilePath: path,
            saveDir: saveDir,
            proxyUrl: proxyUrl,
          );
        }
      }
      if (mounted) Navigator.of(context).pop();
      return;
    }

    final entries = _parseEntries(_urlController.text);
    if (entries.isEmpty) return;

    final parsed = int.tryParse(selectedThreads ?? '') ?? 0;
    final segments = parsed > 0 ? parsed.clamp(1, 64) : 0;

    // 单条磁力链接：对话框保持打开，转入 loading 阶段等待文件列表
    if (entries.length == 1 &&
        entries.first.url.toLowerCase().startsWith('magnet:')) {
      final entry = entries.first;
      // 先注册回调，再发 CreateTask 信号，保证信号到达时回调已就位（无竞态）
      BtFileSelectionService.registerPendingHandler(_onBtFilesInfoReceived);
      final rename = _renameController.text.trim();
      final fileName = rename.isNotEmpty ? rename : entry.fileName;
      widget.controller.createTask(
        url: entry.url,
        saveDir: saveDir,
        fileName: fileName,
        segments: segments,
        proxyUrl: proxyUrl,
        userAgent: userAgent,
        queueId: _selectedQueueId,
        checksum: entry.checksum,
      );
      setState(() {
        _btWaitPhase = 'probing';
        _btPendingTaskId = null;
      });
      return;
    }

    if (entries.length == 1) {
      // 单条非磁力 — 使用 CreateTask，支持重命名
      final entry = entries.first;
      // 重命名字段优先；其次使用 out= 中的文件名
      final rename = _renameController.text.trim();
      final fileName = rename.isNotEmpty ? rename : entry.fileName;
      widget.controller.createTask(
        url: entry.url,
        saveDir: saveDir,
        fileName: fileName,
        segments: segments,
        proxyUrl: proxyUrl,
        userAgent: userAgent,
        queueId: _selectedQueueId,
        checksum: entry.checksum,
      );
    } else {
      // 多条 — 使用 BatchCreateTask（携带每条的 fileName/checksum）
      widget.controller.batchCreateTask(
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
      );
    }

    if (mounted) Navigator.of(context).pop();
  }

  /// 用户在对话框内确认了 BT 文件选择（磁力链接等待阶段）
  void _onBtSelectionConfirmed() {
    if (_btPendingTaskId == null) return;
    if (_btSelectedIndices.isEmpty) return;
    final indices = _btSelectedIndices.toList()..sort();
    final tid = _btPendingTaskId!;
    SelectBtFiles(
      taskId: tid,
      selectedIndices: indices,
    ).sendSignalToRust();
    // 清理状态，防止 dispose 再次发送 [-1]
    _btPendingTaskId = null;
    _btWaitPhase = null;
    if (mounted) Navigator.of(context).pop();
  }

  /// 用户取消了 BT 文件选择（磁力链接等待阶段）
  void _onBtSelectionCancelled() {
    final tid = _btPendingTaskId;
    if (tid != null) {
      // selecting 阶段：已拿到 task_id，直接发 [-1] 让 Rust 暂停任务
      SelectBtFiles(
        taskId: tid,
        selectedIndices: const [-1],
      ).sendSignalToRust();
      BtFileSelectionService.registerPendingHandler(null);
      _btPendingTaskId = null;
      _btWaitPhase = null;
    } else {
      // probing 阶段：task_id 尚未知，标记取消
      // 当 BtFilesInfo 信号到达时，_onBtFilesInfoReceived 检查
      // _btCancelPending 并立刻发 [-1] 暂停任务
      _btCancelPending = true;
      _btWaitPhase = null; // 退出等待状态，UI 恢复正常
    }
    if (mounted) Navigator.of(context).pop();
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
      description: Text(
        _btWaitPhase != null ? s.btWaitingFiles : s.batchDownloadDesc,
      ),
      actions: _btWaitPhase != null
          ? _buildBtWaitActions(s, c)
          : [
              ShadButton.outline(
                onPressed: () => Navigator.of(context).pop(),
                child: Text(s.cancel),
              ),
              ShadButton(
                onPressed:
                    (_isSubmitting ||
                            _isProbing ||
                            (_hasTorrentFiles &&
                                !_hasAnyTorrentSelection &&
                                _allTorrentsProbed))
                        ? null
                        : () => _startDownload(),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const Icon(
                      LucideIcons.download,
                      size: 13,
                      color: Colors.white,
                    ),
                    const SizedBox(width: 6),
                    Text(
                      _buildStartButtonLabel(s),
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
            // ── BT 等待文件列表阶段 ──────────────────────────────────────────
            if (_btWaitPhase != null) ...[
              _buildBtWaitBody(s, c),
            ] else if (_hasTorrentFiles) ...[
              // ── Per-torrent header + file list ────────────────────────────
              for (int ti = 0; ti < _torrentFilePaths.length; ti++)
                _buildTorrentFileEntry(ti, c, s),
              const SizedBox(height: 8),
              // ── Add more / clear buttons ──────────────────────────────
              Row(
                children: [
                  ShadButton.outline(
                    size: ShadButtonSize.sm,
                    enabled: !_isPicking && !_isProbing,
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
                    onTap: () => setState(() {
                      _torrentFilePaths.clear();
                      _torrentMeta.clear();
                      _torrentSelections.clear();
                      _probingPath = null;
                      _isProbing = false;
                      _probeError = '';
                    }),
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
            ] else if (!_hasTorrentFiles && _btWaitPhase == null) ...[
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
              const SizedBox(height: 6),
              // .torrent 文件选择 + TXT 导入按钮
              Row(
                children: [
                  ShadButton.ghost(
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
                  ShadButton.ghost(
                    size: ShadButtonSize.sm,
                    enabled: !_isPicking,
                    onPressed: _importFromTxt,
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Icon(
                          LucideIcons.fileText,
                          size: 13,
                          color: c.textMuted,
                        ),
                        const SizedBox(width: 6),
                        Text(
                          s.importTxtFile,
                          style: TextStyle(fontSize: 12, color: c.textMuted),
                        ),
                      ],
                    ),
                  ),
                ],
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
              ],
            ),

            // 重命名 — 仅单条 URL 时显示（torrent 文件自动识别名称）
            if (!_isBatch) ...[
              const SizedBox(height: 14),
              _SectionLabel(text: s.renameOptional, c: c),
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
                      onChanged: (preset) {
                        if (preset == null) return;
                        setState(() => _selectedUaPreset = preset);
                        const presets = {
                          'chrome':
                              'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
                              '(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36',
                          'firefox':
                              'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) '
                              'Gecko/20100101 Firefox/133.0',
                          'edge':
                              'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
                              '(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0',
                          'netdisk': 'netdisk',
                        };
                        if (preset != 'custom') {
                          _userAgentController.text = presets[preset] ?? '';
                        }
                      },
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: ShadInput(
                      controller: _userAgentController,
                      placeholder: Text(s.userAgentTaskPlaceholder),
                      onChanged: (_) {
                        setState(() => _selectedUaPreset = 'custom');
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

  /// 构建磁力链接等待阶段的 actions 按钮
  List<Widget> _buildBtWaitActions(S s, AppColors c) {
    if (_btWaitPhase == 'probing') {
      // 解析中：只显示取消按钮
      return [
        ShadButton.outline(
          onPressed: _onBtSelectionCancelled,
          child: Text(s.cancel),
        ),
      ];
    }
    // selecting 阶段
    return [
      ShadButton.outline(
        onPressed: _onBtSelectionCancelled,
        child: Text(s.cancel),
      ),
      ShadButton(
        onPressed: _btSelectedIndices.isEmpty ? null : _onBtSelectionConfirmed,
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(LucideIcons.download, size: 13, color: Colors.white),
            const SizedBox(width: 6),
            Text(
              s.btFileSelectConfirm(
                _btSelectedIndices.length,
                formatBtFileSize(_btSelectedTotalBytes),
              ),
              style: const TextStyle(color: Colors.white),
            ),
          ],
        ),
      ),
    ];
  }

  /// 构建磁力链接等待阶段的对话框主体
  Widget _buildBtWaitBody(S s, AppColors c) {
    if (_btWaitPhase == 'probing') {
      // 解析中：loading 动画
      return Container(
        padding: const EdgeInsets.symmetric(vertical: 32),
        alignment: Alignment.center,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(
              width: 28,
              height: 28,
              child: CircularProgressIndicator(strokeWidth: 2.5, color: c.accent),
            ),
            const SizedBox(height: 16),
            Text(
              s.btResolvingMagnet,
              style: TextStyle(fontSize: 13, color: c.textMuted),
            ),
          ],
        ),
      );
    }
    // selecting 阶段：文件列表
    return BtFileListWidget(
      files: _btFiles,
      selectedIndices: _btSelectedIndices,
      onToggleAll: () {
        setState(() {
          if (_btSelectedIndices.length == _btFiles.length) {
            _btSelectedIndices = {};
          } else {
            _btSelectedIndices = _btFiles.map((f) => f.index.toInt()).toSet();
          }
        });
      },
      onToggleFile: (idx) {
        setState(() {
          if (_btSelectedIndices.contains(idx)) {
            _btSelectedIndices = Set.from(_btSelectedIndices)..remove(idx);
          } else {
            _btSelectedIndices = Set.from(_btSelectedIndices)..add(idx);
          }
        });
      },
      maxHeight: 340,
    );
  }

  Widget _buildQueueSelector(S s, AppColors c) {
    final queues = widget.controller.queues;
    // 没有任何命名队列时不显示
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
        const SizedBox(height: 10),
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

/// 解析后的下载条目：URL + 可选文件名 + 可选 checksum
class _ParsedEntry {
  final String url;

  /// 来自 `out=` 选项的文件名，空字符串表示自动识别
  final String fileName;

  /// 来自 `checksum=` 选项的校验值，格式 "algo=hexhash"，空字符串跳过校验
  final String checksum;

  const _ParsedEntry(this.url, {this.fileName = '', this.checksum = ''});
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
