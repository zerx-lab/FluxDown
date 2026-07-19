// 预解析清单选择弹窗（多文件分享/合集链接建组前的确认框，v1.6 下钻导航版）。
//
// 触发路径：new_download_dialog.dart 对单条 http(s) 非磁力/种子链接先发
// ResolvePreviewRequest 探测是否为多文件清单；命中后弹出本对话框，底层的
// 新建下载表单保持不动。取消 → 回到表单；确认 → 发 CreateTaskGroup，两层
// 对话框一起关闭（由调用方在 Future 完成后关闭底层表单，本文件只负责自己
// 的 Navigator.pop）。
//
// 结构（自上而下六段，design/desktop-task-views/DESIGN.md §4.10）：
// 摘要区 → 工具栏（搜索/扩展名筛选/全选反选清空/排序） → 面包屑
// （深度的唯一去处） → 文件列表（下钻导航主体，manifest_browse_list.dart）
// → 高级选项折叠面板（manifest_advanced_panel.dart） → 底栏（保存目录/
// 已选计数/取消/双拆分按钮）。
//
// 纯逻辑（可见性/行流/单链合并/跳级/面包屑折叠/选择作用域/统计）全部委托
// models/manifest_selection.dart；本文件只持有交互状态（cwd/选中集合/
// 筛选/搜索词/排序键/高级选项）与渲染。

import 'package:flutter/services.dart' show KeyDownEvent, LogicalKeyboardKey;
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_queue.dart';
import '../models/manifest_breadcrumb.dart';
import '../models/manifest_selection.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'context_menu.dart';
import 'flux_sonner.dart';
import 'manifest_advanced_panel.dart';
import 'manifest_browse_list.dart';
import 'manifest_dialog_chrome.dart';

/// 弹出清单选择框。
///
/// 返回 `true` = 用户确认并已发出 [CreateTaskGroup]（调用方应关闭底层的
/// 新建下载表单对话框）；返回 `false` = 用户取消（表单对话框保持打开）。
Future<bool> showManifestSelectDialog(
  BuildContext context, {
  required List<DownloadQueue> queues,
  required ResolvePreviewResult manifest,
  required String sourceUrl,
  required String initialSaveDir,
  required String initialQueueId,
  required int segments,
  required String cookies,
  required String referrer,
  required String userAgent,
  required String proxyUrl,
  required Map<String, String> extraHeaders,
  required bool ignoreTlsErrors,
  required bool later,
}) async {
  final result = await showShadDialog<bool>(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    barrierDismissible: false,
    animateIn: const [],
    animateOut: const [],
    builder: (context) => _ManifestSelectDialogContent(
      queues: queues,
      manifest: manifest,
      sourceUrl: sourceUrl,
      initialSaveDir: initialSaveDir,
      initialQueueId: initialQueueId,
      segments: segments,
      cookies: cookies,
      referrer: referrer,
      userAgent: userAgent,
      proxyUrl: proxyUrl,
      extraHeaders: extraHeaders,
      ignoreTlsErrors: ignoreTlsErrors,
      later: later,
    ),
  );
  return result ?? false;
}

/// 固定的每子任务线程数预设集合（对齐 manifest_advanced_panel.dart）。
const Set<int> _kSegmentPresets = {1, 4, 8, 16, 32};

class _ManifestSelectDialogContent extends StatefulWidget {
  final List<DownloadQueue> queues;
  final ResolvePreviewResult manifest;
  final String sourceUrl;
  final String initialSaveDir;
  final String initialQueueId;
  final int segments;
  final String cookies;
  final String referrer;
  final String userAgent;
  final String proxyUrl;
  final Map<String, String> extraHeaders;
  final bool ignoreTlsErrors;
  final bool later;

  const _ManifestSelectDialogContent({
    required this.queues,
    required this.manifest,
    required this.sourceUrl,
    required this.initialSaveDir,
    required this.initialQueueId,
    required this.segments,
    required this.cookies,
    required this.referrer,
    required this.userAgent,
    required this.proxyUrl,
    required this.extraHeaders,
    required this.ignoreTlsErrors,
    required this.later,
  });

  @override
  State<_ManifestSelectDialogContent> createState() =>
      _ManifestSelectDialogContentState();
}

class _ManifestSelectDialogContentState
    extends State<_ManifestSelectDialogContent> {
  late final TextEditingController _groupNameController;
  late final TextEditingController _searchController;
  late final ManifestAdvancedControllers _advControllers;
  late final FocusNode _keyboardFocusNode;

  // 下钻导航状态。初始 sel 为空集（对齐 openManifestModal 语义，0 选中禁用
  // 提交是设计边界态，不是缺陷）。
  String _cwd = '';
  Set<String> _selectedItemIds = {};
  final Set<String> _extFilter = {};
  String _search = '';
  ManifestSortKey _sortKey = ManifestSortKey.name;

  bool _advOpen = false;
  bool _ignoreTlsErrors = false;
  bool _uaInherit = true;
  int _segments = 0;

  late String _saveDir;
  bool _isPickingDir = false;
  bool _submitted = false;

  List<ManifestItemDto> get _items => widget.manifest.items;

  @override
  void initState() {
    super.initState();
    _groupNameController = TextEditingController(
      text: manifestDefaultGroupName(widget.manifest.name, widget.sourceUrl),
    );
    _searchController = TextEditingController();
    _keyboardFocusNode = FocusNode();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _keyboardFocusNode.requestFocus();
    });

    _saveDir = widget.initialSaveDir;
    _ignoreTlsErrors = widget.ignoreTlsErrors;
    _uaInherit = widget.userAgent.trim().isEmpty;
    _segments = _kSegmentPresets.contains(widget.segments) ? widget.segments : 0;
    _advControllers = ManifestAdvancedControllers(
      initialProxyUrl: widget.proxyUrl,
      initialUserAgent: widget.userAgent,
      initialCookies: widget.cookies,
      initialHeaders: widget.extraHeaders,
    );
  }

  @override
  void dispose() {
    _groupNameController.dispose();
    _searchController.dispose();
    _advControllers.dispose();
    _keyboardFocusNode.dispose();
    super.dispose();
  }

  // ── 下钻导航 ──────────────────────────────────────────────────────────

  /// 校验/回退 [_cwd]（筛选后该层被清空时落回根）。只在非搜索态调用。
  void _setCwd(String path) {
    final result = manifestRowsAt(
      items: _items,
      cwd: path,
      selectedItemIds: _selectedItemIds,
      extFilter: _extFilter,
      search: '',
      sortKey: _sortKey,
    );
    _cwd = result.cwd;
  }

  void _navigateTo(String path) => setState(() => _setCwd(path));

  void _navigateUp() {
    if (_cwd.isEmpty || manifestIsSearching(_search)) return;
    final up = manifestUpPath(items: _items, cwd: _cwd, extFilter: _extFilter);
    setState(() => _setCwd(up));
  }

  // ── 选择 ──────────────────────────────────────────────────────────────

  void _toggleDirSubtree(String dirPath) {
    setState(() {
      _selectedItemIds = manifestToggleDirSubtree(
        items: _items,
        dirPath: dirPath,
        selectedItemIds: _selectedItemIds,
        extFilter: _extFilter,
        search: _search,
      );
    });
  }

  void _toggleFile(String itemId) {
    setState(() {
      final next = Set<String>.from(_selectedItemIds);
      if (!next.remove(itemId)) next.add(itemId);
      _selectedItemIds = next;
    });
  }

  void _selectAllVisible() => setState(
    () => _selectedItemIds = manifestSelectAllVisible(
      _items,
      extFilter: _extFilter,
      search: _search,
    ),
  );

  void _invertVisible() => setState(
    () => _selectedItemIds = manifestInvertVisibleSelection(
      _items,
      _selectedItemIds,
      extFilter: _extFilter,
      search: _search,
    ),
  );

  void _clearSelection() => setState(() => _selectedItemIds = {});

  // ── 筛选 / 搜索 / 排序 ────────────────────────────────────────────────

  void _toggleExt(String ext) {
    setState(() {
      if (!_extFilter.remove(ext)) _extFilter.add(ext);
      _setCwd(_cwd);
    });
  }

  void _onSearchChanged(String value) {
    setState(() {
      _search = value;
      if (!manifestIsSearching(_search)) _setCwd(_cwd);
    });
  }

  void _toggleSort() => setState(
    () => _sortKey = _sortKey == ManifestSortKey.name
        ? ManifestSortKey.size
        : ManifestSortKey.name,
  );

  // ── 高级选项 ──────────────────────────────────────────────────────────

  void _toggleAdvOpen() => setState(() => _advOpen = !_advOpen);

  void _addHeaderRow() =>
      setState(() => _advControllers.headerRows.add(ManifestHeaderRowControllers()));

  void _removeHeaderRow(int index) => setState(
    () => _advControllers.headerRows.removeAt(index).dispose(),
  );

  // ── 保存目录 ──────────────────────────────────────────────────────────

  Future<void> _pickSaveDir() async {
    if (_isPickingDir) return;
    setState(() => _isPickingDir = true);
    try {
      final result = await FilePickerService.pickDirectory(
        dialogTitle: currentS.selectSaveDir,
        initialDirectory: _saveDir.isNotEmpty ? _saveDir : null,
      );
      if (result != null && mounted) setState(() => _saveDir = result);
    } on FilePickerException catch (e) {
      if (mounted) _showPickerError(e);
    } finally {
      if (mounted) setState(() => _isPickingDir = false);
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

  // ── 面包屑 ⋯ 溢出菜单 / 队列选择菜单 ─────────────────────────────────

  void _showCrumbOverflowMenu(
    BuildContext anchor,
    List<ManifestCrumbSegment> overflow,
  ) {
    final box = anchor.findRenderObject();
    if (box is! RenderBox || !box.hasSize) return;
    final origin = box.localToGlobal(Offset(0, box.size.height + 4));
    final c = AppColors.of(context);
    showContextMenu(
      context,
      origin,
      items: [
        for (final seg in overflow)
          ContextMenuItem(
            icon: LucideIcons.folder,
            label: seg.label,
            color: c.textPrimary,
            action: () => _navigateTo(seg.path),
          ),
      ],
    );
  }

  /// 拆分按钮 ▾：队列快速选择，选择即提交。启停语义只由按钮决定：与
  /// new_download_dialog.dart `_showQueueMenu` 同一模式，但菜单文案对齐
  /// v1.6 设计（「开始下载到 · X」/「稍后下载到 · X」）。
  void _showQueueMenu(BuildContext anchor, {required bool later}) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    if (widget.queues.isEmpty) {
      _submit(later ? kLaterQueueId : widget.initialQueueId, later);
      return;
    }
    final box = anchor.findRenderObject();
    if (box is! RenderBox || !box.hasSize) return;
    final origin = box.localToGlobal(Offset(0, box.size.height + 6));
    showContextMenu(
      context,
      origin,
      items: [
        for (final q in widget.queues)
          ContextMenuItem(
            icon: q.queueId == kLaterQueueId
                ? LucideIcons.clock
                : LucideIcons.layers,
            label: later
                ? s.manifestLaterToQueue(queueDisplayName(s, q))
                : s.manifestStartToQueue(queueDisplayName(s, q)),
            color: c.textPrimary,
            action: () => _submit(q.queueId, later),
          ),
      ],
    );
  }

  // ── 键盘 ──────────────────────────────────────────────────────────────

  bool _isTextFieldFocused() {
    final ctx = FocusManager.instance.primaryFocus?.context;
    if (ctx == null) return false;
    if (ctx.widget is EditableText) return true;
    var found = false;
    ctx.visitAncestorElements((element) {
      if (element.widget is EditableText) {
        found = true;
        return false;
      }
      return true;
    });
    return found;
  }

  void _handleKey(KeyEvent event) {
    if (event is! KeyDownEvent) return;
    if (event.logicalKey == LogicalKeyboardKey.escape) {
      Navigator.of(context).pop(false);
      return;
    }
    if (event.logicalKey == LogicalKeyboardKey.backspace) {
      if (manifestIsSearching(_search) || _cwd.isEmpty) return;
      if (_isTextFieldFocused()) return;
      _navigateUp();
    }
  }

  // ── 提交 ──────────────────────────────────────────────────────────────

  void _submit(String queueId, bool startPaused) {
    if (_submitted || _selectedItemIds.isEmpty) return;
    _submitted = true;
    final groupItems = buildManifestGroupItems(_items, _selectedItemIds);
    final groupName = _groupNameController.text.trim();
    final effectiveUserAgent = _uaInherit
        ? ''
        : _advControllers.userAgentController.text.trim();
    CreateTaskGroup(
      sourceUrl: widget.sourceUrl,
      groupName: groupName.isEmpty ? widget.manifest.name : groupName,
      saveDir: _saveDir,
      queueId: queueId,
      segments: _segments,
      cookies: _advControllers.cookieController.text.trim(),
      referrer: widget.referrer,
      userAgent: effectiveUserAgent,
      proxyUrl: _advControllers.proxyController.text.trim(),
      extraHeaders: manifestEffectiveHeaders(_advControllers.snapshotHeaders()),
      ignoreTlsErrors: _ignoreTlsErrors,
      startPaused: startPaused,
      items: groupItems,
    ).sendSignalToRust();
    Navigator.of(context).pop(true);
  }

  // ── build ────────────────────────────────────────────────────────────

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    final rowsResult = manifestRowsAt(
      items: _items,
      cwd: _cwd,
      selectedItemIds: _selectedItemIds,
      extFilter: _extFilter,
      search: _search,
      sortKey: _sortKey,
    );
    final breadcrumb = buildManifestBreadcrumb(
      items: _items,
      cwd: _cwd,
      extFilter: _extFilter,
      search: _search,
    );
    final advDirty = manifestAdvancedOptionsDirty(
      ManifestAdvancedOptions(
        proxyUrl: _advControllers.proxyController.text,
        ignoreTlsErrors: _ignoreTlsErrors,
        uaInherit: _uaInherit,
        userAgent: _advControllers.userAgentController.text,
        cookies: _advControllers.cookieController.text,
        segments: _segments,
        headers: _advControllers.snapshotHeaders(),
      ),
    );
    final selStat = manifestSelectionStat(_items, _selectedItemIds);
    final defaultQueue = widget.queues
        .where((q) => q.queueId == widget.initialQueueId)
        .firstOrNull;
    final startTooltipTarget = defaultQueue == null
        ? s.mainQueue
        : queueDisplayName(s, defaultQueue);

    return KeyboardListener(
      focusNode: _keyboardFocusNode,
      onKeyEvent: _handleKey,
      child: ShadDialog(
        constraints: const BoxConstraints(maxWidth: 780, maxHeight: 620),
        padding: EdgeInsets.zero,
        scrollable: false,
        radius: AppMetrics.of(context).brDialog,
        child: SizedBox(
          width: 780,
          height: 620,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(18, 16, 14, 8),
                child: ManifestSummaryHeader(
                  groupNameController: _groupNameController,
                  itemCount: _items.length,
                  totalSize: manifestTotalSize(_items),
                  sourceUrl: widget.sourceUrl,
                  onClose: () => Navigator.of(context).pop(false),
                ),
              ),
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 18),
                child: ManifestToolbar(
                  searchController: _searchController,
                  onSearchChanged: _onSearchChanged,
                  topExtensions: manifestTopExtensions(_items),
                  extFilter: _extFilter,
                  onToggleExt: _toggleExt,
                  onSelectAll: _selectAllVisible,
                  onInvert: _invertVisible,
                  onClear: _clearSelection,
                  sortKey: _sortKey,
                  onToggleSort: _toggleSort,
                ),
              ),
              const SizedBox(height: 8),
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 18),
                child: ManifestBreadcrumbBar(
                  breadcrumb: breadcrumb,
                  onNavigate: _navigateTo,
                  onUp: _navigateUp,
                  onShowOverflowMenu: _showCrumbOverflowMenu,
                ),
              ),
              const SizedBox(height: 4),
              Expanded(
                child: Container(
                  margin: const EdgeInsets.symmetric(horizontal: 12),
                  decoration: BoxDecoration(
                    border: Border(top: BorderSide(color: c.border)),
                  ),
                  child: ManifestBrowseList(
                    rows: rowsResult.rows,
                    selectedItemIds: _selectedItemIds,
                    height: double.infinity,
                    onToggleDirSubtree: _toggleDirSubtree,
                    onEnterDir: _navigateTo,
                    onToggleFile: _toggleFile,
                  ),
                ),
              ),
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 18),
                child: ManifestAdvancedPanel(
                  open: _advOpen,
                  dirty: advDirty,
                  onToggleOpen: _toggleAdvOpen,
                  controllers: _advControllers,
                  ignoreTlsErrors: _ignoreTlsErrors,
                  onIgnoreTlsChanged: (v) => setState(() => _ignoreTlsErrors = v),
                  uaInherit: _uaInherit,
                  onUaInheritChanged: (v) => setState(() => _uaInherit = v),
                  segments: _segments,
                  onSegmentsChanged: (v) => setState(() => _segments = v),
                  onAddHeader: _addHeaderRow,
                  onRemoveHeader: _removeHeaderRow,
                ),
              ),
              Container(
                padding: const EdgeInsets.fromLTRB(18, 10, 18, 14),
                decoration: BoxDecoration(
                  border: Border(top: BorderSide(color: c.border)),
                ),
                child: ManifestFooterBar(
                  saveDir: _saveDir,
                  manifestName: widget.manifest.name,
                  groupNameController: _groupNameController,
                  isPickingDir: _isPickingDir,
                  onPickSaveDir: _pickSaveDir,
                  selStat: selStat,
                  onCancel: () => Navigator.of(context).pop(false),
                  startTooltipTarget: startTooltipTarget,
                  onSubmitLater: () => _submit(kLaterQueueId, true),
                  onPickLaterQueue: (anchor) => _showQueueMenu(anchor, later: true),
                  onSubmitStart: () => _submit(widget.initialQueueId, false),
                  onPickStartQueue: (anchor) => _showQueueMenu(anchor, later: false),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
