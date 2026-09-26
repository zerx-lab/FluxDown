import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/rss_filter.dart';
import '../models/rss_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'overflow_tooltip_text.dart';

/// RSS 条目流：选中侧边栏某个订阅时占据主区（替换任务列表）。
///
/// 复用任务列表的视觉语言（同样的行高/分隔线/hover 反馈/tabular 数字），
/// 让用户不必学第二套列表——设计文档 P3「复用三栏骨架，不建独立空间」。
class RssItemList extends StatefulWidget {
  final RssProvider provider;

  /// 点击「已下载」chip 时跳转到对应任务。
  final void Function(String taskId) onOpenTask;

  /// 打开该订阅的管理对话框（错误态的「检查配置」入口）。
  final void Function(String sourceId) onManage;

  const RssItemList({
    super.key,
    required this.provider,
    required this.onOpenTask,
    required this.onManage,
  });

  @override
  State<RssItemList> createState() => _RssItemListState();
}

class _RssItemListState extends State<RssItemList> {
  final _searchCtrl = TextEditingController();
  final Set<String> _selectedGuids = {};
  String _selectionSourceId = '';
  String _query = '';

  /// 条目排序方向。默认新→旧：引擎回来的快照本来就是这个顺序（DB
  /// `ORDER BY pub_date DESC`），追番看的永远是最新一集。
  bool _oldestFirst = false;

  @override
  void dispose() {
    _searchCtrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.provider,
      builder: (context, _) {
        final s = LocaleScope.of(context);
        final c = AppColors.of(context);
        final source = widget.provider.selectedSource;
        if (source == null) return const SizedBox.shrink();
        final items = widget.provider.selectedItems;
        final visible = _visibleItems(items);
        final selectedGuids = _selectionSourceId == source.sourceId
            ? _selectedGuids
            : const <String>{};
        final selectedItems = items
            .where((item) => selectedGuids.contains(item.guid))
            .toList(growable: false);
        // 底色由主区统一给（HomePage._buildContentArea = surface1）。
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _buildHeader(s, c, source),
            if (visible.isNotEmpty || selectedItems.isNotEmpty)
              _buildSelectionBar(
                s,
                c,
                source.sourceId,
                visible,
                selectedItems,
                selectedGuids,
              ),
            Expanded(
              child: items.isEmpty
                  ? _buildEmpty(s, c, source)
                  : visible.isEmpty
                  ? _buildNoMatch(s, c)
                  : ListView.builder(
                      padding: EdgeInsets.zero,
                      itemCount: visible.length,
                      itemBuilder: (context, index) {
                        final item = visible[index];
                        return RepaintBoundary(
                          child: _RssItemRow(
                            key: ValueKey('${item.sourceId}/${item.guid}'),
                            item: item,
                            selected: selectedGuids.contains(item.guid),
                            busy: widget.provider.isItemDownloading(
                              item.sourceId,
                              item.guid,
                            ),
                            onSelectionChanged: (selected) => _setItemSelected(
                              source.sourceId,
                              item.guid,
                              selected,
                            ),
                            onDownload: () => widget.provider.downloadItem(
                              item.sourceId,
                              item.guid,
                            ),
                            onIgnore: () => widget.provider.ignoreItem(
                              item.sourceId,
                              item.guid,
                            ),
                            onOpenTask: () => widget.onOpenTask(item.taskId),
                          ),
                        );
                      },
                    ),
            ),
          ],
        );
      },
    );
  }

  Widget _buildSelectionBar(
    S s,
    AppColors c,
    String sourceId,
    List<RssItemEntry> visible,
    List<RssItemEntry> selectedItems,
    Set<String> selectedGuids,
  ) {
    final allVisibleSelected =
        visible.isNotEmpty &&
        visible.every((item) => selectedGuids.contains(item.guid));
    return Container(
      height: 36,
      padding: const EdgeInsets.symmetric(horizontal: 16),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: c.border)),
      ),
      child: Row(
        children: [
          _RssSelectionCheckbox(
            checked: allVisibleSelected,
            onTap: () =>
                _setVisibleSelected(sourceId, visible, !allVisibleSelected),
          ),
          const SizedBox(width: 10),
          Text(
            s.rssSelectVisible,
            style: TextStyle(fontSize: 11.5, color: c.textMuted),
          ),
          const Spacer(),
          if (selectedItems.isNotEmpty) ...[
            Text(
              s.rssSelectedCount(selectedItems.length),
              style: TextStyle(fontSize: 11.5, color: c.textMuted),
            ),
            const SizedBox(width: 8),
            ShadButton.secondary(
              size: ShadButtonSize.sm,
              height: 22,
              padding: const EdgeInsets.symmetric(horizontal: 6),
              onPressed: () => _downloadSelected(sourceId, selectedItems),
              child: Text(
                s.rssDownloadSelected,
                style: const TextStyle(fontSize: 11),
              ),
            ),
            const SizedBox(width: 6),
            ShadButton.ghost(
              size: ShadButtonSize.sm,
              height: 22,
              padding: const EdgeInsets.symmetric(horizontal: 4),
              onPressed: () => _clearSelection(sourceId),
              child: Text(
                s.rssClearSelection,
                style: const TextStyle(fontSize: 11),
              ),
            ),
          ],
        ],
      ),
    );
  }

  void _setItemSelected(String sourceId, String guid, bool selected) {
    setState(() {
      _prepareSelectionForSource(sourceId);
      if (selected) {
        _selectedGuids.add(guid);
      } else {
        _selectedGuids.remove(guid);
      }
    });
  }

  void _setVisibleSelected(
    String sourceId,
    List<RssItemEntry> visible,
    bool selected,
  ) {
    setState(() {
      _prepareSelectionForSource(sourceId);
      for (final item in visible) {
        if (selected) {
          _selectedGuids.add(item.guid);
        } else {
          _selectedGuids.remove(item.guid);
        }
      }
    });
  }

  void _clearSelection(String sourceId) {
    setState(() {
      _prepareSelectionForSource(sourceId);
      _selectedGuids.clear();
    });
  }

  void _prepareSelectionForSource(String sourceId) {
    if (_selectionSourceId == sourceId) return;
    _selectionSourceId = sourceId;
    _selectedGuids.clear();
  }

  void _downloadSelected(String sourceId, List<RssItemEntry> selectedItems) {
    for (final item in selectedItems) {
      widget.provider.downloadItem(sourceId, item.guid);
    }
    _clearSelection(sourceId);
  }

  /// 过滤 + 排序后的条目。
  ///
  /// 排序按 `pubDate` 走，并用**原始下标**做 tie-break：`List.sort` 不保证
  /// 稳定，而同一集不同字幕组的 `pubDate` 常常一模一样（Mikan 同秒发布），
  /// 不钉死次序的话每次 setState 行都会乱跳。缺发布时间（`pubDate == 0`）的
  /// 条目一律沉底，不让它们污染时间轴两端。
  List<RssItemEntry> _visibleItems(List<RssItemEntry> items) {
    final filtered = _query.isEmpty
        ? items
        : items
              .where((i) => i.title.toLowerCase().contains(_query))
              .toList(growable: false);
    final indexed = [
      for (var i = 0; i < filtered.length; i++) (i, filtered[i]),
    ];
    indexed.sort((a, b) {
      final (ai, ax) = a;
      final (bi, bx) = b;
      if ((ax.pubDate == 0) != (bx.pubDate == 0)) {
        return ax.pubDate == 0 ? 1 : -1;
      }
      final byDate = _oldestFirst
          ? ax.pubDate.compareTo(bx.pubDate)
          : bx.pubDate.compareTo(ax.pubDate);
      return byDate != 0 ? byDate : ai.compareTo(bi);
    });
    return [for (final (_, item) in indexed) item];
  }

  // ─────────────────────────────────────────────
  // 订阅头：左边身份与健康度（抓取节奏 / 上次结果 / 自动下载去向），
  // 右边这条订阅的动作与筛选。合并成一行而不是再叠一条工具条——标题右侧
  // 本来就是大片空白，多一条 37px 的横带只是把列表往下推。
  // ─────────────────────────────────────────────

  Widget _buildHeader(S s, AppColors c, RssSourceEntry source) {
    final m = AppMetrics.of(context);
    final unhealthy = source.lastError.isNotEmpty;
    return Container(
      padding: const EdgeInsets.fromLTRB(16, 10, 16, 10),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: c.border)),
      ),
      child: Row(
        children: [
          Container(
            width: 30,
            height: 30,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: unhealthy ? m.soft(AppColors.red) : c.surface2,
              borderRadius: m.brMd,
            ),
            child: Icon(
              LucideIcons.rss,
              size: 15,
              color: unhealthy ? AppColors.red : c.textSecondary,
            ),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                OverflowTooltipText(
                  rssDisplayName(source),
                  style: TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                    color: c.textPrimary,
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  _statusLine(s, source),
                  style: TextStyle(
                    fontSize: 11,
                    color: unhealthy ? AppColors.red : c.textMuted,
                  ),
                  overflow: TextOverflow.ellipsis,
                ),
              ],
            ),
          ),
          const SizedBox(width: 12),
          if (unhealthy) ...[
            ShadButton.outline(
              size: ShadButtonSize.sm,
              onPressed: () => widget.onManage(source.sourceId),
              child: Text(s.rssCheckConfig),
            ),
            const SizedBox(width: 8),
          ],
          // 排序：图标即当前方向（箭头朝下 = 新在上），点一下翻转。
          _IconAction(
            icon: _oldestFirst
                ? LucideIcons.arrowUpNarrowWide
                : LucideIcons.arrowDownWideNarrow,
            tooltip: _oldestFirst ? s.rssSortOldest : s.rssSortNewest,
            onPressed: () => setState(() => _oldestFirst = !_oldestFirst),
          ),
          const SizedBox(width: 2),
          _IconAction(
            icon: LucideIcons.checkCheck,
            tooltip: s.rssMarkAllRead,
            onPressed: () => widget.provider.markAllRead(source.sourceId),
          ),
          const SizedBox(width: 2),
          _RefreshButton(
            busy: widget.provider.isRefreshing(source.sourceId),
            onPressed: () => widget.provider.refresh(source.sourceId),
          ),
          const SizedBox(width: 10),
          // 搜索贴最右、与下方条目右缘对齐：它筛的是这条列表，不是全局。
          SizedBox(
            width: 190,
            child: ShadInput(
              controller: _searchCtrl,
              placeholder: Text(s.rssSearchHint),
              padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
              style: const TextStyle(fontSize: 12.5),
              leading: Padding(
                padding: const EdgeInsets.only(left: 2),
                child: Icon(LucideIcons.search, size: 13, color: c.textMuted),
              ),
              onChanged: (v) => setState(() => _query = v.trim().toLowerCase()),
            ),
          ),
        ],
      ),
    );
  }

  /// 一行说清「多久抓一次 / 现在或上次怎么样 / 抓到了往哪放」——订阅的健康度
  /// 不该藏在设置里，它就是用户判断「这条还活着吗」的唯一依据。
  String _statusLine(S s, RssSourceEntry source) {
    final parts = <String>[s.rssEveryMinutes(source.intervalMinutes)];
    if (widget.provider.isRefreshing(source.sourceId)) {
      // 抓取中优先于历史结果：这一行回答的是「现在怎么样」，此刻正在跑就该
      // 这么说，而不是停在上一轮的时间戳或「尚未抓取」上。
      parts.add(s.rssRefreshing);
    } else if (source.lastError.isNotEmpty) {
      parts.add(s.rssFailedTimes(source.failCount));
      parts.add(source.lastError);
    } else if (source.lastSuccessAt > 0) {
      parts.add(s.rssLastFetch(_relativeTime(s, source.lastSuccessAt)));
    } else {
      parts.add(s.rssNeverFetched);
    }
    parts.add(source.autoDownload ? s.rssAutoDownloadOn : s.rssCollectMode);
    return parts.join(' · ');
  }

  String _relativeTime(S s, int unixSeconds) {
    final delta = DateTime.now().millisecondsSinceEpoch ~/ 1000 - unixSeconds;
    if (delta < 60) return s.rssJustNow;
    if (delta < 3600) return s.rssMinutesAgo(delta ~/ 60);
    if (delta < 86400) return s.rssHoursAgo(delta ~/ 3600);
    return s.rssDaysAgo(delta ~/ 86400);
  }

  // ─────────────────────────────────────────────
  // 空态：给引导，不给空白
  // ─────────────────────────────────────────────

  /// 空列表有三种截然不同的处境，用同一段文案糊过去等于什么都没说：
  /// 正在抓（等着就行）／抓失败了（要动手改配置）／抓成功但源里没东西。
  /// 新建订阅后引擎会立刻抓一轮，此时进来看到的必须是转圈而不是空白。
  Widget _buildEmpty(S s, AppColors c, RssSourceEntry source) {
    final fetching = widget.provider.isRefreshing(source.sourceId);
    final failed = !fetching && source.lastError.isNotEmpty;
    final title = fetching
        ? s.rssEmptyFetching
        : failed
        ? s.rssEmptyError
        : s.rssEmptyTitle;
    final desc = fetching
        ? s.rssEmptyFetchingHint
        : failed
        ? s.rssEmptyErrorHint
        : s.rssEmptyDesc;
    return Center(
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 48),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (fetching)
              SizedBox(
                width: 26,
                height: 26,
                child: CircularProgressIndicator(
                  strokeWidth: 2.2,
                  color: c.accent,
                ),
              )
            else
              Icon(
                failed ? LucideIcons.circleAlert : LucideIcons.rss,
                size: 40,
                color: failed ? AppColors.red : c.textDisabled,
              ),
            const SizedBox(height: 14),
            Text(
              title,
              style: TextStyle(
                fontSize: 13.5,
                color: failed ? AppColors.red : c.textSecondary,
              ),
            ),
            const SizedBox(height: 6),
            Text(
              desc,
              textAlign: TextAlign.center,
              style: TextStyle(fontSize: 11.5, color: c.textMuted, height: 1.6),
            ),
            if (failed) ...[
              const SizedBox(height: 16),
              Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  ShadButton.outline(
                    size: ShadButtonSize.sm,
                    onPressed: () => widget.provider.refresh(source.sourceId),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        const Icon(LucideIcons.refreshCw, size: 13),
                        const SizedBox(width: 6),
                        Text(s.rssEmptyRetry),
                      ],
                    ),
                  ),
                  const SizedBox(width: 8),
                  ShadButton.outline(
                    size: ShadButtonSize.sm,
                    onPressed: () => widget.onManage(source.sourceId),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        const Icon(LucideIcons.settings2, size: 13),
                        const SizedBox(width: 6),
                        Text(s.rssCheckConfig),
                      ],
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

  Widget _buildNoMatch(S s, AppColors c) => Center(
    child: Text(
      s.rssNoMatch(_query),
      style: TextStyle(fontSize: 12.5, color: c.textMuted),
    ),
  );
}

/// 一条 RSS 条目行。状态 chip 决定行尾可用操作（P4：条目状态可见 + 可覆盖）。
class _RssItemRow extends StatefulWidget {
  final RssItemEntry item;
  final bool selected;

  /// 引擎正在为这条条目抓种子 / 建任务。
  final bool busy;
  final ValueChanged<bool> onSelectionChanged;
  final VoidCallback onDownload;
  final VoidCallback onIgnore;
  final VoidCallback onOpenTask;

  const _RssItemRow({
    super.key,
    required this.item,
    required this.selected,
    required this.busy,
    required this.onSelectionChanged,
    required this.onDownload,
    required this.onIgnore,
    required this.onOpenTask,
  });

  @override
  State<_RssItemRow> createState() => _RssItemRowState();
}

class _RssItemRowState extends State<_RssItemRow> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final item = widget.item;
    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 9),
        decoration: BoxDecoration(
          // 即时切色（不做动画）：从 Colors.transparent 做 lerp 会闪黑，
          // 见仓库规则 `.omp/rules/no-lerp-from-transparent.md`。
          color: _isHovered ? c.hoverBg : c.hoverBg.withValues(alpha: 0),
          border: Border(bottom: BorderSide(color: c.border)),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            _RssSelectionCheckbox(
              checked: widget.selected,
              onTap: () => widget.onSelectionChanged(!widget.selected),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  OverflowTooltipText(
                    item.title,
                    style: TextStyle(fontSize: 12.5, color: c.textPrimary),
                  ),
                  const SizedBox(height: 3),
                  Row(
                    children: [
                      if (item.pubDate > 0) ...[
                        Text(
                          _formatDate(item.pubDate),
                          style: TextStyle(
                            fontSize: 11,
                            color: c.textMuted,
                            fontFeatures: const [FontFeature.tabularFigures()],
                          ),
                        ),
                        const SizedBox(width: 10),
                      ],
                      if (item.enclosureLength > 0)
                        Text(
                          DownloadTask.formatBytes(item.enclosureLength),
                          style: TextStyle(
                            fontSize: 11,
                            color: c.textMuted,
                            fontFeatures: const [FontFeature.tabularFigures()],
                          ),
                        ),
                    ],
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            // 状态列定宽右对齐：chip 文案长度随状态变化（「新」vs「规则未命中 ·
            // 命中排除词」），不定宽会让相邻行的 chip 与操作按钮左右错位。
            SizedBox(
              width: 210,
              child: Align(
                alignment: Alignment.centerRight,
                child: _StatusChip(item: item, onOpenTask: widget.onOpenTask),
              ),
            ),
            const SizedBox(width: 10),
            SizedBox(
              width: 160,
              child: Row(
                mainAxisAlignment: MainAxisAlignment.end,
                // 「准备中」必须脱离 hover 显示：点完把鼠标移开是常态，
                // 按钮跟着消失等于把刚给出的反馈又抽走。
                children: (_isHovered || widget.busy)
                    ? _actions(s, c)
                    : const <Widget>[],
              ),
            ),
          ],
        ),
      ),
    );
  }

  List<Widget> _actions(S s, AppColors c) {
    final status = widget.item.status;
    if (widget.busy) {
      // 只留一个禁用态按钮：这一行正在被引擎处理，此刻「忽略」既没意义也
      // 容易误点。
      return [
        ShadButton.ghost(
          size: ShadButtonSize.sm,
          onPressed: null,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              SizedBox(
                width: 12,
                height: 12,
                child: CircularProgressIndicator(
                  strokeWidth: 1.6,
                  color: c.accent,
                ),
              ),
              const SizedBox(width: 6),
              Text(s.rssActionPreparing),
            ],
          ),
        ),
      ];
    }
    // 已下载的条目也给操作入口：任务可能被删了、下崩了，或者只是想再来一遍。
    //
    // 被规则拦下的条目按钮同样只写「下载」：状态 chip 已经把原因说清楚了
    // （「规则未命中 · 命中排除词」），按钮再来一句「仍要下载」是在替用户
    // 犹豫——他都点开这一行了，动作就是下载。
    final label = switch (status) {
      RssItemStatusCode.downloaded => s.rssActionRedownload,
      _ => s.rssActionDownload,
    };
    return [
      ShadButton.ghost(
        size: ShadButtonSize.sm,
        onPressed: widget.onDownload,
        child: Text(label),
      ),
      if (status == RssItemStatusCode.isNew) ...[
        const SizedBox(width: 4),
        ShadButton.ghost(
          size: ShadButtonSize.sm,
          onPressed: widget.onIgnore,
          child: Text(s.rssActionIgnore),
        ),
      ],
    ];
  }

  String _formatDate(int unixSeconds) {
    final d = DateTime.fromMillisecondsSinceEpoch(unixSeconds * 1000);
    String two(int n) => n.toString().padLeft(2, '0');
    return '${two(d.month)}-${two(d.day)} ${two(d.hour)}:${two(d.minute)}';
  }
}

/// RSS 选择框沿用「全部文件」管理列表里的方框图标和主题色。
class _RssSelectionCheckbox extends StatelessWidget {
  final bool checked;
  final VoidCallback onTap;

  const _RssSelectionCheckbox({required this.checked, required this.onTap});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap,
        child: SizedBox(
          width: 20,
          height: 20,
          child: Center(
            child: Icon(
              checked ? LucideIcons.squareCheck : LucideIcons.square,
              size: 16,
              color: checked ? c.accent : c.textMuted,
            ),
          ),
        ),
      ),
    );
  }
}

/// 状态 chip。「已下载」可点击跳转到任务，其余只做说明。
class _StatusChip extends StatelessWidget {
  final RssItemEntry item;
  final VoidCallback onOpenTask;

  const _StatusChip({required this.item, required this.onOpenTask});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final (label, color, clickable) = switch (item.status) {
      RssItemStatusCode.isNew => (s.rssStatusNew, c.accent, false),
      RssItemStatusCode.downloaded => (
        s.rssStatusDownloaded,
        AppColors.green,
        true,
      ),
      RssItemStatusCode.ignored => (s.rssStatusIgnored, c.textMuted, false),
      RssItemStatusCode.duplicateEpisode => (
        s.rssStatusDuplicate,
        AppColors.amber,
        false,
      ),
      RssItemStatusCode.seedSkipped => (s.rssStatusHistory, c.textMuted, false),
      _ => (s.rssStatusFiltered, c.textMuted, false),
    };
    final reason = rssReasonLabel(s, item.reason);
    final text = reason.isEmpty ? label : '$label · $reason';
    final chip = Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(color: m.soft(color), borderRadius: m.brSm),
      child: Text(text, style: TextStyle(fontSize: 11, color: color)),
    );
    if (!clickable || item.taskId.isEmpty) return chip;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(onTap: onOpenTask, child: chip),
    );
  }
}

/// 稳定原因码 → 本地化文案。引擎只产出码，文案全在这里。
String rssReasonLabel(S s, String code) => switch (code) {
  RssReasonCode.notIncluded => s.rssReasonNotIncluded,
  RssReasonCode.excluded => s.rssReasonExcluded,
  RssReasonCode.tooSmall => s.rssReasonTooSmall,
  RssReasonCode.tooLarge => s.rssReasonTooLarge,
  RssReasonCode.dupEpisode => s.rssReasonDupEpisode,
  _ => '',
};

/// 订阅头右侧的图标动作。
///
/// 文字按钮在标题行里太占宽（两个就吃掉 200px，把搜索框挤到标题上），换成
/// 图标 + 500ms 悬浮提示：日常操作靠肌肉记忆，第一次用靠 tooltip 兜底。
class _IconAction extends StatelessWidget {
  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;

  const _IconAction({
    required this.icon,
    required this.tooltip,
    required this.onPressed,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return ShadTooltip(
      waitDuration: const Duration(milliseconds: 500),
      // 无入场动画：淡入+位移的 200ms 只是把「已经等了 500ms」再拖长一截，
      // 提示要么不出现，要么立刻在那儿（同 task_list.dart 的管理入口 tooltip）。
      effects: const [],
      builder: (_) => Text(tooltip),
      child: ShadIconButton.ghost(
        width: 28,
        height: 28,
        onPressed: onPressed,
        icon: Icon(icon, size: 15, color: c.textSecondary),
      ),
    );
  }
}

/// 「立即抓取」按钮。抓取是 off-actor 的、常要好几秒，没有进行中反馈用户会
/// 反复点或以为没生效——所以 busy 时换成 spinner 并禁用（`onPressed: null`
/// 同时给出 shadcn 的禁用视觉），tooltip 也跟着改成「抓取中」。
class _RefreshButton extends StatelessWidget {
  final bool busy;
  final VoidCallback onPressed;

  const _RefreshButton({required this.busy, required this.onPressed});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return ShadTooltip(
      waitDuration: const Duration(milliseconds: 500),
      effects: const [],
      builder: (_) => Text(busy ? s.rssRefreshing : s.rssRefreshNow),
      child: ShadIconButton.ghost(
        width: 28,
        height: 28,
        onPressed: busy ? null : onPressed,
        icon: busy
            ? SizedBox(
                width: 14,
                height: 14,
                child: CircularProgressIndicator(
                  strokeWidth: 1.6,
                  color: c.accent,
                ),
              )
            : Icon(LucideIcons.refreshCw, size: 15, color: c.textSecondary),
      ),
    );
  }
}
