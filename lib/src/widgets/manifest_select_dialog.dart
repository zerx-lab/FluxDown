// 预解析清单选择弹窗（多文件分享/合集链接建组前的确认框）。
//
// 触发路径：new_download_dialog.dart 对单条 http(s) 非磁力/种子链接先发
// ResolvePreviewRequest 探测是否为多文件清单；命中后弹出本对话框，底层的
// 新建下载表单保持不动。取消 → 回到表单（表单未被改动，可编辑重新提交）；
// 确认 → 发 CreateTaskGroup，两层对话框一起关闭（由调用方在 Future 完成后
// 关闭底层表单，本文件只负责自己的 Navigator.pop）。
//
// 纯逻辑（树构建/单链折叠/三态勾选/扩展名聚合/规格策略/resolver_item 拼接/
// 剧集启发式）全部委托 models/manifest_selection.dart；本文件只持有交互
// 状态（选中集合/折叠集合/筛选/策略/per-item 覆盖）与渲染。
//
// 结构（渐进披露，自上而下，contract-dart.md §选择弹窗）：
// 摘要区 → 智能建议条 → 意图按钮组 → 文件树（扩展名筛选常驻） → 规格策略
// → 底栏（保存目录/队列/已选计数 + 取消/确认）。

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/download_task.dart';
import '../models/manifest_selection.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'bt_file_list_widget.dart' show BtCheckbox;
import 'dir_picker_field.dart';
import 'flux_sonner.dart';
import 'manifest_select_tree.dart';

/// 弹出清单选择框。
///
/// 返回 `true` = 用户确认并已发出 [CreateTaskGroup]（调用方应关闭底层的
/// 新建下载表单对话框）；返回 `false` = 用户取消（表单对话框保持打开）。
Future<bool> showManifestSelectDialog(
  BuildContext context, {
  required DownloadController controller,
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
      controller: controller,
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

class _ManifestSelectDialogContent extends StatefulWidget {
  final DownloadController controller;
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
    required this.controller,
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
  late final ManifestEpisodeSuggestion? _suggestion;

  Set<String> _selectedItemIds = {};
  final Set<String> _collapsedDirPaths = {};
  FileCategory _categoryFilter = FileCategory.all;
  ManifestQualityPolicy _qualityPolicy = ManifestQualityPolicy.highest;
  final Map<String, String> _perItemOverrides = {};
  bool _suggestionDismissed = false;
  bool _submitted = false;
  bool _isPicking = false;

  late String _saveDir;
  late String _queueId;

  List<ManifestItemDto> get _items => widget.manifest.items;

  @override
  void initState() {
    super.initState();
    _selectedItemIds = allManifestItemIds(_items);
    _suggestion = detectManifestEpisodeSuggestion(_items);
    _groupNameController = TextEditingController(
      text: manifestDefaultGroupName(widget.manifest.name, widget.sourceUrl),
    );
    _saveDir = widget.initialSaveDir;
    _queueId = widget.initialQueueId;
  }

  @override
  void dispose() {
    _groupNameController.dispose();
    super.dispose();
  }

  // ── 派生数据 ────────────────────────────────────────────────────────────

  List<ManifestItemDto> get _filteredItems =>
      filterManifestItemsByCategory(_items, _categoryFilter);

  ManifestPolicyResult get _policyResult =>
      applyManifestQualityPolicy(_items, _qualityPolicy);

  Map<String, String?> get _effectiveVariants =>
      resolveEffectiveManifestVariants(_policyResult, _perItemOverrides);

  Set<String> _allDirPaths(List<ManifestNode> nodes) {
    final result = <String>{};
    void walk(ManifestNode node) {
      if (node is ManifestDirNode) {
        result.add(node.path);
        for (final c in node.children) {
          walk(c);
        }
      }
    }

    for (final n in nodes) {
      walk(n);
    }
    return result;
  }

  // ── 交互 ────────────────────────────────────────────────────────────────

  void _applySuggestion() {
    final suggestion = _suggestion;
    if (suggestion == null) return;
    setState(() => _selectedItemIds = Set.from(suggestion.itemIds));
  }

  void _toggleDirCollapse(ManifestDirNode dir) {
    setState(() {
      if (_collapsedDirPaths.contains(dir.path)) {
        _collapsedDirPaths.remove(dir.path);
      } else {
        _collapsedDirPaths.add(dir.path);
      }
    });
  }

  void _toggleDirSelection(ManifestDirNode dir, bool select) {
    setState(
      () => _selectedItemIds = toggleManifestDirSelection(
        dir,
        _selectedItemIds,
        select,
      ),
    );
  }

  void _toggleFileSelection(ManifestFileNode file) {
    setState(() {
      final next = Set<String>.from(_selectedItemIds);
      if (next.contains(file.item.id)) {
        next.remove(file.item.id);
      } else {
        next.add(file.item.id);
      }
      _selectedItemIds = next;
    });
  }

  void _selectVariant(ManifestFileNode file, String variantId) {
    setState(() => _perItemOverrides[file.item.id] = variantId);
  }

  Future<void> _pickSaveDir() async {
    if (_isPicking) return;
    setState(() => _isPicking = true);
    try {
      final result = await FilePickerService.pickDirectory(
        dialogTitle: currentS.selectSaveDir,
        initialDirectory: _saveDir.isNotEmpty ? _saveDir : null,
      );
      if (result != null && mounted) setState(() => _saveDir = result);
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

  void _onConfirm() {
    if (_submitted || _selectedItemIds.isEmpty) return;
    _submitted = true;
    final groupItems = buildManifestGroupItems(
      _items,
      _selectedItemIds,
      _effectiveVariants,
    );
    CreateTaskGroup(
      sourceUrl: widget.sourceUrl,
      groupName: _groupNameController.text.trim(),
      saveDir: _saveDir,
      queueId: _queueId,
      segments: widget.segments,
      cookies: widget.cookies,
      referrer: widget.referrer,
      userAgent: widget.userAgent,
      proxyUrl: widget.proxyUrl,
      extraHeaders: widget.extraHeaders,
      ignoreTlsErrors: widget.ignoreTlsErrors,
      startPaused: widget.later,
      items: groupItems,
    ).sendSignalToRust();
    Navigator.of(context).pop(true);
  }

  // ── build ──────────────────────────────────────────────────────────────

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);

    final tree = buildManifestTree(_filteredItems);
    final visibleRows = flattenManifestTree(tree, _collapsedDirPaths);
    final allDirPaths = _allDirPaths(tree);
    final allCollapsed =
        allDirPaths.isNotEmpty &&
        allDirPaths.every(_collapsedDirPaths.contains);
    final filteredIds = _filteredItems.map((i) => i.id).toSet();
    final allFilteredSelected =
        filteredIds.isNotEmpty && filteredIds.every(_selectedItemIds.contains);
    final anyFilteredSelected = filteredIds.any(_selectedItemIds.contains);
    final hasVariants = _items.any((i) => i.variants.isNotEmpty);
    final policyResult = _policyResult;
    final selectedSize = manifestSelectedSize(_items, _selectedItemIds);
    final confirmLabel = widget.later ? s.downloadLater : s.startDownload;

    return ShadDialog(
      constraints: const BoxConstraints(maxWidth: 680),
      padding: const EdgeInsets.fromLTRB(18, 24, 18, 24),
      scrollPadding: const EdgeInsets.symmetric(horizontal: 6),
      title: Row(
        children: [
          Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: m.soft(c.accent),
              borderRadius: m.brMd,
            ),
            child: Icon(LucideIcons.listChecks, size: 14, color: c.accent),
          ),
          const SizedBox(width: 10),
          Text(s.manifestDialogTitle),
        ],
      ),
      description: Text(s.manifestDialogDesc(_items.length)),
      actions: [
        ShadButton.outline(
          onPressed: () => Navigator.of(context).pop(false),
          child: Text(s.cancel),
        ),
        ShadButton(
          onPressed: _selectedItemIds.isEmpty || _submitted ? null : _onConfirm,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(
                LucideIcons.download,
                size: 13,
                color: Color(0xFFFFFFFF),
              ),
              const SizedBox(width: 6),
              Text(
                confirmLabel,
                style: const TextStyle(color: Color(0xFFFFFFFF)),
              ),
            ],
          ),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.only(top: 16, bottom: 16, right: 10),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            // ── 摘要区 ────────────────────────────────────────────────────
            _SectionLabel(text: s.manifestGroupNameLabel, c: c),
            const SizedBox(height: 6),
            ShadInput(
              controller: _groupNameController,
              placeholder: Text(s.manifestGroupNamePlaceholder),
            ),
            const SizedBox(height: 6),
            Text(
              s.manifestSummary(
                _items.length,
                DownloadTask.formatBytes(manifestTotalSize(_items)),
              ),
              style: TextStyle(fontSize: 11.5, color: c.textMuted),
            ),

            // ── 智能建议条 ────────────────────────────────────────────────
            if (_suggestion != null && !_suggestionDismissed) ...[
              const SizedBox(height: 12),
              _buildSuggestionBar(c, m, s, _suggestion),
            ],

            // ── 意图按钮组 ────────────────────────────────────────────────
            const SizedBox(height: 14),
            _SectionLabel(text: s.manifestIntentLabel, c: c),
            const SizedBox(height: 6),
            _buildIntentBar(c, m, s),

            // ── 文件树区 ──────────────────────────────────────────────────
            const SizedBox(height: 14),
            Row(
              children: [
                _SectionLabel(text: s.manifestFilesLabel, c: c),
                const Spacer(),
                _linkButton(
                  c: c,
                  label: allCollapsed
                      ? s.manifestExpandAll
                      : s.manifestCollapseAll,
                  onTap: () => setState(() {
                    if (allCollapsed) {
                      _collapsedDirPaths.clear();
                    } else {
                      _collapsedDirPaths
                        ..clear()
                        ..addAll(allDirPaths);
                    }
                  }),
                ),
              ],
            ),
            const SizedBox(height: 6),
            _buildExtensionFilterChips(c, m, s),
            const SizedBox(height: 8),
            Row(
              children: [
                GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTap: filteredIds.isEmpty
                      ? null
                      : () => setState(() {
                          _selectedItemIds = allFilteredSelected
                              ? _selectedItemIds.difference(filteredIds)
                              : _selectedItemIds.union(filteredIds);
                        }),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      BtCheckbox(
                        checked: allFilteredSelected,
                        indeterminate:
                            !allFilteredSelected && anyFilteredSelected,
                        accentColor: c.accent,
                      ),
                      const SizedBox(width: 8),
                      Text(
                        s.manifestTreeSelectVisible,
                        style: TextStyle(fontSize: 12, color: c.textSecondary),
                      ),
                    ],
                  ),
                ),
              ],
            ),
            const SizedBox(height: 6),
            ManifestTreeList(
              rows: visibleRows,
              selectedItemIds: _selectedItemIds,
              collapsedDirPaths: _collapsedDirPaths,
              effectiveVariants: _effectiveVariants,
              onToggleDirCollapse: _toggleDirCollapse,
              onToggleDirSelection: _toggleDirSelection,
              onToggleFileSelection: _toggleFileSelection,
              onSelectVariant: _selectVariant,
            ),

            // ── 规格策略 ──────────────────────────────────────────────────
            if (hasVariants) ...[
              const SizedBox(height: 14),
              _SectionLabel(text: s.manifestQualityPolicyLabel, c: c),
              const SizedBox(height: 6),
              _segmented<ManifestQualityPolicy>(
                context: context,
                options: [
                  (ManifestQualityPolicy.highest, s.manifestQualityHighest),
                  (ManifestQualityPolicy.p1080, s.manifestQuality1080p),
                  (ManifestQualityPolicy.p720, s.manifestQuality720p),
                  (ManifestQualityPolicy.lowest, s.manifestQualityLowest),
                ],
                selected: _qualityPolicy,
                onChanged: (v) => setState(() {
                  _qualityPolicy = v;
                  _perItemOverrides.clear();
                }),
              ),
              if (policyResult.fallbackCount > 0) ...[
                const SizedBox(height: 6),
                Row(
                  children: [
                    Icon(LucideIcons.info, size: 12, color: c.textMuted),
                    const SizedBox(width: 4),
                    Expanded(
                      child: Text(
                        s.manifestQualityFallbackHint(
                          policyResult.fallbackCount,
                        ),
                        style: TextStyle(fontSize: 11, color: c.textMuted),
                      ),
                    ),
                    _linkButton(
                      c: c,
                      label: s.manifestAdjustPerItem,
                      onTap: () => setState(_collapsedDirPaths.clear),
                    ),
                  ],
                ),
              ],
            ],

            // ── 底栏：保存目录 / 队列 / 已选计数 ─────────────────────────────
            const SizedBox(height: 14),
            _SectionLabel(text: s.saveDir, c: c),
            const SizedBox(height: 6),
            DirPickerField(
              path: _saveDir,
              placeholder: s.selectSaveDir,
              enabled: !_isPicking,
              onTap: _pickSaveDir,
            ),
            const SizedBox(height: 10),
            Row(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      _SectionLabel(text: s.manifestQueueLabel, c: c),
                      const SizedBox(height: 6),
                      _buildQueueSelect(c, s),
                    ],
                  ),
                ),
                const SizedBox(width: 16),
                Text(
                  s.manifestSelectedSummary(
                    _selectedItemIds.length,
                    DownloadTask.formatBytes(selectedSize),
                  ),
                  style: TextStyle(
                    fontSize: 12,
                    fontWeight: FontWeight.w600,
                    color: c.textPrimary,
                  ),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  // ── 子区块构建 ─────────────────────────────────────────────────────────

  Widget _buildSuggestionBar(
    AppColors c,
    AppMetrics m,
    S s,
    ManifestEpisodeSuggestion suggestion,
  ) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
      decoration: BoxDecoration(
        color: m.subtle(c.accent),
        borderRadius: m.brMd,
        border: Border.all(color: m.borderSubtle(c.accent)),
      ),
      child: Row(
        children: [
          Icon(LucideIcons.sparkles, size: 14, color: c.accent),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              s.manifestSuggestionText(suggestion.count),
              style: TextStyle(fontSize: 12, color: c.textPrimary),
            ),
          ),
          _linkButton(
            c: c,
            label: s.manifestSuggestionApply,
            onTap: _applySuggestion,
          ),
          const SizedBox(width: 12),
          GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: () => setState(() => _suggestionDismissed = true),
            child: Icon(LucideIcons.x, size: 14, color: c.textMuted),
          ),
        ],
      ),
    );
  }

  Widget _buildIntentBar(AppColors c, AppMetrics m, S s) {
    final aggregates = aggregateManifestByCategory(_items);
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final agg in aggregates)
          _actionChip(
            c: c,
            m: m,
            label: s.manifestCategoryChip(
              agg.category.label,
              agg.count,
              DownloadTask.formatBytes(agg.totalSize),
            ),
            onTap: () =>
                setState(() => _selectedItemIds = Set.from(agg.itemIds)),
          ),
        _actionChip(
          c: c,
          m: m,
          label: s.manifestSelectAll,
          onTap: () =>
              setState(() => _selectedItemIds = allManifestItemIds(_items)),
        ),
        _actionChip(
          c: c,
          m: m,
          label: s.manifestInvertSelection,
          onTap: () => setState(
            () => _selectedItemIds = invertManifestSelection(
              _items,
              _selectedItemIds,
            ),
          ),
        ),
      ],
    );
  }

  Widget _buildExtensionFilterChips(AppColors c, AppMetrics m, S s) {
    final aggregates = aggregateManifestByCategory(_items);
    final categories = [FileCategory.all, ...aggregates.map((a) => a.category)];
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final category in categories)
          _filterChip(
            c: c,
            m: m,
            label: category.label,
            selected: _categoryFilter == category,
            onTap: () => setState(() => _categoryFilter = category),
          ),
      ],
    );
  }

  Widget _buildQueueSelect(AppColors c, S s) {
    final queues = widget.controller.queues;
    return SizedBox(
      width: double.infinity,
      child: ShadSelect<String>(
        initialValue: _queueId,
        options: [
          for (final q in queues)
            ShadOption(value: q.queueId, child: Text(queueDisplayName(s, q))),
        ],
        selectedOptionBuilder: (context, value) {
          final q = queues.where((q) => q.queueId == value).firstOrNull;
          return Text(
            q == null ? s.mainQueue : queueDisplayName(s, q),
            overflow: TextOverflow.ellipsis,
            maxLines: 1,
          );
        },
        onChanged: (v) {
          if (v != null) setState(() => _queueId = v);
        },
      ),
    );
  }

  // ── 通用小组件 ─────────────────────────────────────────────────────────

  Widget _linkButton({
    required AppColors c,
    required String label,
    required VoidCallback onTap,
  }) {
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: Text(
        label,
        style: TextStyle(
          fontSize: 11.5,
          fontWeight: FontWeight.w600,
          color: c.accent,
        ),
      ),
    );
  }

  Widget _actionChip({
    required AppColors c,
    required AppMetrics m,
    required String label,
    required VoidCallback onTap,
  }) {
    return GestureDetector(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
        decoration: BoxDecoration(color: c.surface2, borderRadius: m.brPill),
        child: Text(
          label,
          style: TextStyle(
            fontSize: 11.5,
            fontWeight: FontWeight.w500,
            color: c.textSecondary,
          ),
        ),
      ),
    );
  }

  Widget _filterChip({
    required AppColors c,
    required AppMetrics m,
    required String label,
    required bool selected,
    required VoidCallback onTap,
  }) {
    return GestureDetector(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        decoration: BoxDecoration(
          color: selected ? c.accent : c.surface2,
          borderRadius: m.brPill,
        ),
        child: Text(
          label,
          style: TextStyle(
            fontSize: 11.5,
            fontWeight: FontWeight.w500,
            color: selected ? const Color(0xFFFFFFFF) : c.textSecondary,
          ),
        ),
      ),
    );
  }

  Widget _segmented<T>({
    required BuildContext context,
    required List<(T, String)> options,
    required T selected,
    required ValueChanged<T> onChanged,
  }) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Container(
      height: 28,
      padding: const EdgeInsets.all(2),
      decoration: BoxDecoration(color: c.surface2, borderRadius: m.brSm),
      child: Row(
        children: [
          for (final opt in options)
            Expanded(
              child: GestureDetector(
                onTap: () => onChanged(opt.$1),
                child: Container(
                  alignment: Alignment.center,
                  decoration: BoxDecoration(
                    color: opt.$1 == selected
                        ? c.accent
                        : const Color(0x00000000),
                    borderRadius: m.brXs,
                  ),
                  child: Text(
                    opt.$2,
                    style: TextStyle(
                      fontSize: 11.5,
                      fontWeight: FontWeight.w500,
                      color: opt.$1 == selected
                          ? const Color(0xFFFFFFFF)
                          : c.textSecondary,
                    ),
                  ),
                ),
              ),
            ),
        ],
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
