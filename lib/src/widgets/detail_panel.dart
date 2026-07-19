import 'dart:math';
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:url_launcher/url_launcher.dart';
import 'flux_sonner.dart';
import '../bindings/bindings.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/download_task.dart';
import '../i18n/locale_provider.dart';
import '../services/open_folder.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import '../theme/segment_palette.dart';
import 'edit_threads_dialog.dart';
import 'task_columns.dart';

/// 插件系统失败任务的错误消息前缀（引擎/hub/server 固定格式，逃生舱按钮据此判断）。
const _pluginErrorPrefix = '[插件]';

class DetailPanel extends StatefulWidget {
  final DownloadController controller;
  final VoidCallback onClose;

  /// 当前是否为底部布局（决定切换按钮图标方向）
  final bool isBottom;

  /// 切换面板位置（底部 ↔ 右侧）
  final VoidCallback? onTogglePosition;

  const DetailPanel({
    super.key,
    required this.controller,
    required this.onClose,
    this.isBottom = true,
    this.onTogglePosition,
  });

  @override
  State<DetailPanel> createState() => _DetailPanelState();
}

class _DetailPanelState extends State<DetailPanel> {
  /// 插件处理耗时显示的 1s 刷新 ticker（仅在有插件活动时运行）。
  Timer? _pluginTicker;

  /// 当前选中 Tab：0=常规 1=队列 2=日志 3=高级（design-proto-spec §12）。
  int _tab = 0;

  /// 上一次渲染的任务 ID —— 用于检测「切换到另一个任务」并把 Tab 重置回常规。
  String? _lastTaskId;

  @override
  void dispose() {
    _pluginTicker?.cancel();
    super.dispose();
  }

  /// 按当前活动状态启停 ticker（build 内调用，幂等）。
  void _syncPluginTicker(bool active) {
    if (active && _pluginTicker == null) {
      _pluginTicker = Timer.periodic(const Duration(seconds: 1), (_) {
        if (mounted) setState(() {});
      });
    } else if (!active && _pluginTicker != null) {
      _pluginTicker?.cancel();
      _pluginTicker = null;
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Container(
      color: c.surface1,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _buildHeader(c),
          Expanded(
            child: ListenableBuilder(
              listenable: widget.controller,
              builder: (context, _) {
                final task = widget.controller.selectedTask;
                if (task == null) {
                  _lastTaskId = null;
                  return _buildNoSelection(c);
                }
                // 选中任务切换时，Tab 重置回常规（design-proto-spec §12 契约）。
                if (task.id != _lastTaskId) {
                  _lastTaskId = task.id;
                  _tab = 0;
                }
                final pluginActive =
                    task.status == TaskStatus.completed &&
                    widget.controller.isPluginProcessing(task.id);
                _syncPluginTicker(pluginActive);
                return Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Padding(
                      padding: const EdgeInsets.fromLTRB(16, 14, 16, 0),
                      child: _buildFileHeader(c, m, task),
                    ),
                    const SizedBox(height: 12),
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 16),
                      child: _buildTabBar(c, m),
                    ),
                    const SizedBox(height: 2),
                    Expanded(
                      child: _buildTabContent(c, m, task, pluginActive),
                    ),
                  ],
                );
              },
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildHeader(AppColors c) {
    return Container(
      height: 42,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: c.border, width: 1)),
      ),
      child: Row(
        children: [
          Text(
            currentS.detail,
            style: TextStyle(
              fontSize: 13,
              fontWeight: FontWeight.w600,
              color: c.textPrimary,
            ),
          ),
          const Spacer(),
          // 切换面板位置按钮（底部 ↔ 右侧）
          ShadButton.ghost(
            onPressed: widget.onTogglePosition,
            size: ShadButtonSize.sm,
            width: 28,
            height: 28,
            padding: EdgeInsets.zero,
            child: Icon(
              widget.isBottom
                  ? LucideIcons.panelRight
                  : LucideIcons.panelBottom,
              size: 14,
              color: c.textMuted,
            ),
          ),
          const SizedBox(width: 4),
          ShadButton.ghost(
            onPressed: widget.onClose,
            size: ShadButtonSize.sm,
            width: 28,
            height: 28,
            padding: EdgeInsets.zero,
            child: Icon(LucideIcons.x, size: 14, color: c.textMuted),
          ),
        ],
      ),
    );
  }

  Widget _buildNoSelection(AppColors c) {
    return Center(
      child: Text(
        currentS.selectTaskHint,
        style: TextStyle(fontSize: 12, color: c.textMuted),
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 文件头部区：图标 + 文件名 + 状态副标题（含 Boost 标记）
  // ---------------------------------------------------------------------------

  Widget _buildFileHeader(AppColors c, AppMetrics m, DownloadTask task) {
    final isBoost = widget.controller.priorityTaskId == task.id;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Container(
          width: 40,
          height: 40,
          decoration: BoxDecoration(color: c.surface2, borderRadius: m.brCard),
          child: Center(
            child: Text(
              task.fileExtension,
              style: TextStyle(
                fontSize: 11,
                fontWeight: FontWeight.w600,
                color: c.textSecondary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
        ),
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                task.fileName,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 13, color: c.textPrimary),
              ),
              const SizedBox(height: 3),
              Row(
                children: [
                  Flexible(
                    child: Text(
                      task.statusText,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(fontSize: 11, color: c.textMuted),
                    ),
                  ),
                  if (isBoost) ...[
                    Text(
                      ' · ',
                      style: TextStyle(fontSize: 11, color: c.textMuted),
                    ),
                    const Icon(LucideIcons.zap, size: 10, color: AppColors.amber),
                    const SizedBox(width: 2),
                    Text(
                      currentS.detailBoostActive,
                      style: const TextStyle(
                        fontSize: 11,
                        color: AppColors.amber,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ],
                ],
              ),
            ],
          ),
        ),
      ],
    );
  }

  // ---------------------------------------------------------------------------
  // Tab 条（chip 式，复用 group_detail_panel.dart 既有 `_buildTabBar` 模式）
  // ---------------------------------------------------------------------------

  Widget _buildTabBar(AppColors c, AppMetrics m) {
    final labels = [
      currentS.detailTabGeneral,
      currentS.detailTabQueue,
      currentS.detailTabLog,
      currentS.detailTabAdvanced,
    ];
    return Row(
      children: [
        for (var i = 0; i < labels.length; i++) ...[
          if (i > 0) const SizedBox(width: 4),
          GestureDetector(
            onTap: () => setState(() => _tab = i),
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: AnimatedContainer(
                duration: const Duration(milliseconds: 120),
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 6,
                ),
                decoration: BoxDecoration(
                  color: _tab == i
                      ? c.accentBg
                      : c.accentBg.withValues(alpha: 0),
                  borderRadius: m.brMd,
                ),
                child: Text(
                  labels[i],
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: _tab == i ? FontWeight.w500 : FontWeight.normal,
                    color: _tab == i ? c.accent : c.textSecondary,
                  ),
                ),
              ),
            ),
          ),
        ],
      ],
    );
  }

  Widget _buildTabContent(
    AppColors c,
    AppMetrics m,
    DownloadTask task,
    bool pluginActive,
  ) {
    switch (_tab) {
      case 1:
        return _buildQueueTab(c, task);
      case 2:
        return _buildLogTab(c, task);
      case 3:
        return _buildAdvancedTab(c, task);
      case 0:
      default:
        return _buildGeneralTab(c, m, task, pluginActive);
    }
  }

  /// Tab 内容通用滚动容器；底部横向布局时限宽 560 居左（design-proto-spec §12
  /// 队列/日志/高级 Tab「全宽单栏」要求），竖直面板本就窄，不额外限宽。
  Widget _tabScroll(Widget child) {
    final content = Padding(padding: const EdgeInsets.all(16), child: child);
    if (!widget.isBottom) return SingleChildScrollView(child: content);
    return SingleChildScrollView(
      child: Align(
        alignment: Alignment.topLeft,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 560),
          child: content,
        ),
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 常规 Tab
  // ---------------------------------------------------------------------------

  Widget _buildGeneralTab(
    AppColors c,
    AppMetrics m,
    DownloadTask task,
    bool pluginActive,
  ) {
    if (widget.isBottom) {
      // 底部横向布局：左（进度头区+下载分布）flex2，1px 分隔，右（信息字段
      // 滚动 + 钉底操作 footer）flex1（design-proto-spec §12 + 用户决策：
      // 操作按钮固定在最底部，不随字段滚动）。
      return Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Expanded(
            flex: 2,
            child: SingleChildScrollView(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [_buildProgress(c, m, task, pluginActive)],
              ),
            ),
          ),
          Container(width: 1, color: c.border),
          Expanded(
            child: Column(
              children: [
                Expanded(
                  child: SingleChildScrollView(
                    padding: const EdgeInsets.all(16),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: _buildGeneralFields(c, m, task),
                    ),
                  ),
                ),
                _buildActionsFooter(c, m, task),
              ],
            ),
          ),
        ],
      );
    }
    return Column(
      children: [
        Expanded(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                _buildProgress(c, m, task, pluginActive),
                const SizedBox(height: 20),
                ..._buildGeneralFields(c, m, task),
              ],
            ),
          ),
        ),
        _buildActionsFooter(c, m, task),
      ],
    );
  }

  /// 信息字段（错误行/逃生舱 + 字段表）—— 操作按钮与删除已移入钉底
  /// [_buildActionsFooter]，本方法只产出滚动区内容。
  List<Widget> _buildGeneralFields(
    AppColors c,
    AppMetrics m,
    DownloadTask task,
  ) {
    final s = currentS;
    final widgets = <Widget>[];
    if (task.errorMessage.isNotEmpty) {
      widgets.add(const SizedBox(height: 10));
      widgets.add(_buildErrorRow(c, task.errorMessage));
    }
    if (task.status == TaskStatus.error &&
        task.errorMessage.startsWith(_pluginErrorPrefix)) {
      widgets.add(const SizedBox(height: 8));
      widgets.add(_buildIgnorePluginRetryButton(c, task));
    }
    widgets.add(const SizedBox(height: 16));
    if (task.groupId.isNotEmpty &&
        widget.controller.groupById(task.groupId) != null) {
      widgets.add(_buildGroupLinkRow(c, task.groupId));
    }
    widgets.add(_buildInfoRow(s.infoPath, task.saveDir, c));
    widgets.add(_buildUrlRow(c, task.url));
    if (task.referrer.isNotEmpty) {
      widgets.add(_buildSourcePageRow(c, task.referrer));
    }
    // 协议·来源：BT/磁力的 siteLabel 本身已是「BT · 磁力」（extractSiteLabel
    // 特例），再拼 protocolLabel 会冗余成「BT · BT · 磁力」——bt 桶直接用
    // siteLabel（对齐 proto 图示「BitTorrent · 磁力链接」语义）。
    widgets.add(
      _buildInfoRow(
        s.infoProtocolSource,
        task.siteKey == 'bt'
            ? task.siteLabel
            : '${task.protocolLabel} · ${task.siteLabel}',
        c,
      ),
    );
    widgets.add(_buildInfoRow(s.infoStartedAt, formatWhen(task.createdAt), c));
    if (task.status == TaskStatus.completed && task.completedAt != null) {
      widgets.add(
        _buildInfoRow(
          s.infoCompletedAt,
          _formatDateTime(task.completedAt!),
          c,
        ),
      );
      widgets.add(
        _buildInfoRow(
          s.infoDuration,
          _formatDuration(task.completedAt!.difference(task.createdAt)),
          c,
        ),
      );
    }
    widgets.add(_buildInfoRow(s.colQueue, _queueLabel(task.queueId), c));
    return widgets;
  }

  String _queueLabel(String queueId) {
    final s = currentS;
    if (queueId.isEmpty) return s.ungroupedTasks;
    final q = widget.controller.queueById(queueId);
    return q == null ? queueId : queueDisplayName(s, q);
  }

  // ---------------------------------------------------------------------------
  // 进度区域：百分比 + done/total + 分段进度条 + IDM 网格 + 图例 + segs-sum
  // ---------------------------------------------------------------------------

  Widget _buildProgress(
    AppColors c,
    AppMetrics m,
    DownloadTask task,
    bool pluginActive,
  ) {
    final rawSegs = task.segments;
    final hasSegs =
        rawSegs != null && rawSegs.isNotEmpty && task.totalBytes > 0;

    // 当任务已完成时，修正分片数据使每个分片的 downloadedBytes 等于其完整大小，
    // 避免因下载太快导致最后一次分片进度没来得及更新而显示不完整的进度。
    final List<SegmentData>? segs;
    if (hasSegs && task.status == TaskStatus.completed) {
      segs = rawSegs
          .map(
            (s) => SegmentData(
              index: s.index,
              startByte: s.startByte,
              endByte: s.endByte,
              downloadedBytes: s.endByte - s.startByte + 1,
            ),
          )
          .toList();
    } else {
      segs = rawSegs;
    }

    // 百分比：始终使用 task.progress（downloadedBytes / totalBytes），
    // 这是 Rust 端传来的权威进度值。分片数据仅用于可视化分段进度条，
    // 不用来反算总百分比（BT 虚拟分片的舍入误差 + 信号时序差异会导致
    // 与任务列表显示不一致）。
    final double pctValue;
    if (task.status == TaskStatus.completed) {
      pctValue = 1.0;
    } else {
      pctValue = task.progress;
    }
    final pctStr = (pctValue * 100).toStringAsFixed(1);
    final doneTotalText = task.totalBytes > 0
        ? '${task.downloadedText} / ${task.sizeText}'
        : task.downloadedText;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (pluginActive) _buildPluginActivityCard(c, task),
        // 大号百分比 + 同行小字 done/total
        Row(
          crossAxisAlignment: CrossAxisAlignment.baseline,
          textBaseline: TextBaseline.alphabetic,
          children: [
            Text(
              '$pctStr%',
              style: TextStyle(
                fontSize: 26,
                fontWeight: FontWeight.w600,
                color: c.textPrimary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
            const SizedBox(width: 8),
            Flexible(
              child: Text(
                '· $doneTotalText',
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  fontSize: 11.5,
                  color: c.textMuted,
                  fontFeatures: const [FontFeature.tabularFigures()],
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),

        // 分段进度条
        if (hasSegs)
          _buildSegmentedBar(c, m, segs!, task.totalBytes)
        else
          _buildSimpleBar(c, m, pctValue),
        const SizedBox(height: 8),
        _buildProgressSubLine(c, task),

        // IDM 网格可视化
        if (hasSegs) ...[
          const SizedBox(height: 16),
          _buildSegmentGrid(c, m, segs!, task.totalBytes),
        ],

        // 分片图例 — 分片过多时（如 BT 多文件）隐藏避免溢出
        if (hasSegs && segs!.length > 1 && segs.length <= 32) ...[
          const SizedBox(height: 12),
          _buildSegmentLegend(c, m, segs),
        ],

        // segs-sum 行：分段数 · 活跃数 · 动态拆分状态（+ 最近拆分详情）
        if (hasSegs) ...[
          const SizedBox(height: 12),
          _buildSegsSummary(c, segs!, task),
        ],
      ],
    );
  }

  /// sub 行：下载中显示 `速度 · 剩余 eta`（绿色），否则状态文字；
  /// 右端仅 HTTP/FTP 任务显示线程数（配置值或「自动」）。
  Widget _buildProgressSubLine(AppColors c, DownloadTask task) {
    final s = currentS;
    final isActive =
        task.status == TaskStatus.downloading ||
        task.status == TaskStatus.resuming;
    final leftText = isActive
        ? '${task.speedText} · ${s.infoRemaining} ${task.etaText}'
        : task.statusText;
    final leftColor = isActive ? AppColors.green : c.textMuted;
    final proto = task.protocolLabel;
    final showThreads = proto == 'HTTP' || proto == 'FTP';
    final threadsText = task.configuredSegments > 0
        ? s.infoThreads(task.configuredSegments)
        : s.auto;
    return Row(
      children: [
        Expanded(
          child: Text(
            leftText,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(
              fontSize: 11.5,
              color: leftColor,
              fontWeight: isActive ? FontWeight.w500 : FontWeight.normal,
            ),
          ),
        ),
        if (showThreads) ...[
          const SizedBox(width: 8),
          Text(
            threadsText,
            style: TextStyle(
              fontSize: 11,
              color: c.textMuted,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
          ),
        ],
      ],
    );
  }

  /// 无分片数据时的简单进度条
  Widget _buildSimpleBar(AppColors c, AppMetrics m, double progress) {
    return Container(
      height: 4,
      decoration: BoxDecoration(color: c.surface3, borderRadius: m.brXs),
      child: FractionallySizedBox(
        alignment: Alignment.centerLeft,
        widthFactor: progress,
        child: Container(
          decoration: BoxDecoration(color: c.accent, borderRadius: m.brXs),
        ),
      ),
    );
  }

  /// 分段进度条 — 每个分片按字节范围比例占位，内部按下载量填充
  Widget _buildSegmentedBar(
    AppColors c,
    AppMetrics m,
    List<SegmentData> segs,
    int totalBytes,
  ) {
    return ClipRRect(
      borderRadius: m.brSm,
      child: SizedBox(
        height: 6,
        child: CustomPaint(
          size: const Size(double.infinity, 6),
          painter: _SegmentBarPainter(
            segments: segs,
            totalBytes: totalBytes,
            emptyColor: c.surface3,
            palette: SegmentPalette.of(c),
          ),
        ),
      ),
    );
  }

  /// IDM 风格网格可视化
  Widget _buildSegmentGrid(
    AppColors c,
    AppMetrics m,
    List<SegmentData> segs,
    int totalBytes,
  ) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          currentS.downloadDistribution,
          style: TextStyle(
            fontSize: 11,
            fontWeight: FontWeight.w500,
            color: c.textMuted,
          ),
        ),
        const SizedBox(height: 8),
        Container(
          decoration: BoxDecoration(
            color: c.surface2,
            borderRadius: m.brMd,
            border: Border.all(color: c.border, width: 1),
          ),
          padding: const EdgeInsets.all(6),
          child: LayoutBuilder(
            builder: (context, constraints) {
              const cellSize = 5.0;
              const cellGap = 1.5;
              final cols = ((constraints.maxWidth - 0) / (cellSize + cellGap))
                  .floor();
              // 行数：根据分片数自适应，至少 8 行，最多 20 行
              final targetCells = cols * max(8, min(20, segs.length * 3 + 4));
              final rows = (targetCells / cols).ceil();
              final totalCells = cols * rows;
              final height = rows * (cellSize + cellGap) - cellGap;
              return SizedBox(
                height: height,
                child: CustomPaint(
                  size: Size(constraints.maxWidth, height),
                  painter: _SegmentGridPainter(
                    segments: segs,
                    totalBytes: totalBytes,
                    totalCells: totalCells,
                    cols: cols,
                    cellSize: cellSize,
                    cellGap: cellGap,
                    emptyColor: c.surface3,
                    unfilledAlpha: c.bg.computeLuminance() < 0.5
                        ? m.alphaMuted
                        : m.alphaActive,
                    palette: SegmentPalette.of(c),
                  ),
                ),
              );
            },
          ),
        ),
      ],
    );
  }

  /// 分片图例 — 每个分片一行，显示颜色块 + 序号 + 进度
  Widget _buildSegmentLegend(
    AppColors c,
    AppMetrics m,
    List<SegmentData> segs,
  ) {
    final palette = SegmentPalette.of(c);
    return Wrap(
      spacing: 12,
      runSpacing: 6,
      children: [
        for (final seg in segs)
          Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(
                width: 8,
                height: 8,
                decoration: BoxDecoration(
                  color: SegmentPalette.colorFor(palette, seg.index),
                  borderRadius: m.brXs,
                ),
              ),
              const SizedBox(width: 4),
              Text(
                '#${seg.index + 1} ${(seg.progress * 100).toStringAsFixed(0)}%',
                style: TextStyle(
                  fontSize: 10,
                  color: c.textMuted,
                  fontFeatures: const [FontFeature.tabularFigures()],
                ),
              ),
            ],
          ),
      ],
    );
  }

  /// segs-sum 行 —— `N 分段 · N 活跃 · 动态拆分：开启 · …`；
  /// recentSplits 非空时尾追最近拆分详情小字（原「拆分信息行」数据重组）。
  Widget _buildSegsSummary(
    AppColors c,
    List<SegmentData> segs,
    DownloadTask task,
  ) {
    final s = currentS;
    final active = segs.where((seg) => seg.progress < 1.0).length;
    final splits = task.recentSplits;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(LucideIcons.split, size: 11, color: c.textMuted),
            const SizedBox(width: 4),
            Expanded(
              child: Text(
                s.detailSegsSummary(segs.length, active),
                style: TextStyle(fontSize: 10.5, color: c.textMuted),
              ),
            ),
          ],
        ),
        if (splits.isNotEmpty) ...[
          const SizedBox(height: 4),
          Padding(
            padding: const EdgeInsets.only(left: 15),
            child: _buildSplitDetail(c, splits),
          ),
        ],
      ],
    );
  }

  /// 最近拆分详情 —— 次数统计 + 最新一次拆分位置/大小。
  Widget _buildSplitDetail(AppColors c, List<SplitEventData> splits) {
    final s = currentS;
    final latest = splits.last;
    final proactiveCount = splits.where((sp) => sp.isProactive).length;
    final reactiveCount = splits.length - proactiveCount;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          s.splitCount(splits.length, reactiveCount, proactiveCount),
          style: TextStyle(
            fontSize: 10.5,
            color: c.textSecondary,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
        const SizedBox(height: 2),
        Text(
          s.splitLatest(
            latest.parentIndex + 1,
            latest.childIndex + 1,
            DownloadTask.formatBytes(latest.childEnd - latest.childStart + 1),
          ),
          style: TextStyle(
            fontSize: 10,
            color: c.textMuted,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
      ],
    );
  }

  // ---------------------------------------------------------------------------
  // 钉底操作 footer：主操作 + 文件夹 + 复制链接（图标+文字等宽）+ 删除按钮。
  // 固定在常规 Tab 最底部，不随字段区滚动（用户决策：按钮放最底部、带文字）。
  // ---------------------------------------------------------------------------

  Widget _buildActionsFooter(AppColors c, AppMetrics m, DownloadTask task) {
    final s = currentS;
    final primary = _buildPrimaryActionButton(c, m, task);
    return Container(
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: c.border, width: 1)),
      ),
      child: Column(
        children: [
          Row(
            children: [
              if (primary != null) ...[
                Expanded(child: primary),
                const SizedBox(width: 8),
              ],
              Expanded(
                child: DetailFooterActionButton(
                  icon: LucideIcons.folderOpen,
                  label: s.detailActionFolder,
                  onPressed: () => openFolder(task.revealFolderPath),
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: DetailFooterActionButton(
                  icon: LucideIcons.link,
                  label: s.detailActionCopyLink,
                  onPressed: () => _copyLinkWithToast(task.url),
                ),
              ),
            ],
          ),
          const SizedBox(height: 8),
          _buildDeleteButton(c, task),
        ],
      ),
    );
  }

  /// 暂停 / 恢复中 / 继续 —— 现有三态逻辑，完成态无主操作（返回 null）。
  Widget? _buildPrimaryActionButton(
    AppColors c,
    AppMetrics m,
    DownloadTask task,
  ) {
    if (task.status == TaskStatus.completed) return null;
    return DetailFooterPrimaryButton(
      status: task.status,
      onPause: () => widget.controller.pauseTask(task.id),
      onResume: () => widget.controller.resumeTask(task.id),
    );
  }

  void _copyLinkWithToast(String url) {
    Clipboard.setData(ClipboardData(text: url));
    FluxSonner.of(context).show(
      ShadToast(
        title: Text(currentS.urlCopied),
        duration: const Duration(seconds: 2),
      ),
    );
  }

  Widget _buildDeleteButton(AppColors c, DownloadTask task) {
    return SizedBox(
      width: double.infinity,
      child: ShadButton.destructive(
        onPressed: () =>
            widget.controller.deleteTask(task.id, deleteFiles: true),
        child: Text(
          currentS.deleteTaskAndFile,
          style: const TextStyle(fontSize: 13, color: Color(0xFFFFFFFF)),
        ),
      ),
    );
  }

  Widget _buildIgnorePluginRetryButton(AppColors c, DownloadTask task) {
    return SizedBox(
      width: double.infinity,
      child: ShadButton.outline(
        onPressed: () => _confirmIgnorePluginRetry(task.id),
        child: Row(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            Icon(LucideIcons.shieldOff, size: 14, color: c.textPrimary),
            const SizedBox(width: 6),
            Text(
              currentS.taskIgnorePluginRetry,
              style: TextStyle(fontSize: 13, color: c.textPrimary),
            ),
          ],
        ),
      ),
    );
  }

  /// 逃生舱：确认后忽略插件重新解析，直接用原始链接恢复下载。
  void _confirmIgnorePluginRetry(String taskId) {
    final c = AppColors.of(context);
    final s = currentS;
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.taskIgnorePluginRetryTitle),
        description: Text(s.taskIgnorePluginRetryMsg),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
          ShadButton(
            onPressed: () {
              Navigator.of(ctx).pop();
              IgnorePluginRetry(taskId: taskId).sendSignalToRust();
            },
            child: Text(s.taskIgnorePluginRetry),
          ),
        ],
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 信息行辅助
  // ---------------------------------------------------------------------------

  Widget _buildInfoRow(String label, String value, AppColors c) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 60,
            child: Text(
              label,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
          ),
          Expanded(
            child: Text(
              value,
              style: TextStyle(
                fontSize: 11,
                color: c.textSecondary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
        ],
      ),
    );
  }

  /// 配置线程数行 —— 显示任务当前配置的线程数（用户设定的上限，稳定值），
  /// 并提供编辑入口。仅对 HTTP/FTP 任务展示（BT/ED2K 不适用分段线程语义）。
  /// 已完成任务不可改（无意义）；其余状态均可改——引擎对活跃任务自动
  /// 暂停/恢复以立即生效，进度完整保留。
  Widget _buildThreadsConfigRow(AppColors c, DownloadTask task) {
    final proto = task.protocolLabel;
    if (proto != 'HTTP' && proto != 'FTP') return const SizedBox.shrink();

    final n = task.configuredSegments;
    final valueText = n > 0 ? currentS.infoThreads(n) : currentS.auto;
    final editable = task.status != TaskStatus.completed;

    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.center,
        children: [
          SizedBox(
            width: 60,
            child: Text(
              currentS.configuredThreads,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
          ),
          Expanded(
            child: Text(
              valueText,
              style: TextStyle(
                fontSize: 11,
                color: c.textSecondary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
          if (editable)
            Tooltip(
              message: currentS.editThreads,
              child: ShadButton.ghost(
                height: 22,
                width: 22,
                padding: EdgeInsets.zero,
                onPressed: () =>
                    showEditThreadsDialog(context, widget.controller, task),
                child: Icon(
                  LucideIcons.pencil,
                  size: 12,
                  color: c.textSecondary,
                ),
              ),
            ),
        ],
      ),
    );
  }

  /// 「所属任务组」链接行——组成员任务专属，点击选中该组（打开组详情面板，
  /// design-proto-spec §12 `taskDetailHtml` `data-goto-group`）。
  Widget _buildGroupLinkRow(AppColors c, String groupId) {
    final s = currentS;
    final group = widget.controller.groupById(groupId);
    final name = group?.displayName ?? groupId;
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 60,
            child: Text(
              s.groupMemberOfLabel,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
          ),
          Expanded(
            child: GestureDetector(
              onTap: () => widget.controller.selectGroup(groupId),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Flexible(
                    child: Text(
                      name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(fontSize: 11, color: c.accent),
                    ),
                  ),
                  const SizedBox(width: 4),
                  Icon(LucideIcons.chevronRight, size: 11, color: c.accent),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  /// 插件处理中卡片 — 已完成任务的 onDone 钩子（如 ffmpeg 转码）仍在运行：
  /// 显示旋转指示、插件 identity 与已耗时（ticker 每秒刷新）。
  /// 旁路 UI 指示，不代表任务状态机。
  Widget _buildPluginActivityCard(AppColors c, DownloadTask task) {
    final ids = widget.controller.pluginProcessingIds(task.id);
    final since = widget.controller.pluginProcessingSince(task.id);
    final elapsed = since == null ? null : DateTime.now().difference(since);
    final title = elapsed == null
        ? currentS.pluginProcessing
        : '${currentS.pluginProcessing} · ${_formatElapsed(elapsed)}';
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Container(
        width: double.infinity,
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        decoration: BoxDecoration(
          color: c.accent.withValues(alpha: 0.06),
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: c.accent.withValues(alpha: 0.25)),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.only(top: 1),
              child: SizedBox(
                width: 12,
                height: 12,
                child: CircularProgressIndicator(
                  strokeWidth: 1.5,
                  color: c.accent,
                ),
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title,
                    style: TextStyle(
                      fontSize: 11.5,
                      fontWeight: FontWeight.w500,
                      color: c.accent,
                    ),
                  ),
                  if (ids.isNotEmpty) ...[
                    const SizedBox(height: 2),
                    Text(
                      ids.join('、'),
                      style: TextStyle(fontSize: 10.5, color: c.textMuted),
                    ),
                  ],
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  /// 耗时格式：`23s` / `1m05s`。
  static String _formatElapsed(Duration d) {
    final mins = d.inMinutes;
    final secs = d.inSeconds % 60;
    if (mins <= 0) return '${secs}s';
    return '${mins}m${secs.toString().padLeft(2, '0')}s';
  }

  /// `yyyy-MM-dd HH:mm:ss` 本地时间格式（结束时间 / 日志时间戳等精确场景）。
  static String _formatDateTime(DateTime dt) {
    String two(int v) => v.toString().padLeft(2, '0');
    return '${dt.year}-${two(dt.month)}-${two(dt.day)} '
        '${two(dt.hour)}:${two(dt.minute)}:${two(dt.second)}';
  }

  /// 任务耗时格式：`23s` / `3m05s` / `1h02m03s`（开始→下载完成，不含 hook）。
  static String _formatDuration(Duration d) {
    if (d.isNegative) d = Duration.zero;
    String two(int v) => v.toString().padLeft(2, '0');
    final hours = d.inHours;
    final mins = d.inMinutes % 60;
    final secs = d.inSeconds % 60;
    if (hours > 0) return '${hours}h${two(mins)}m${two(secs)}s';
    if (mins > 0) return '${mins}m${two(secs)}s';
    return '${secs}s';
  }

  Widget _buildUrlRow(AppColors c, String url) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 60,
            child: Text(
              currentS.infoUrl,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
          ),
          Expanded(
            child: Text(
              url,
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(
                fontSize: 11,
                color: c.textSecondary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
          const SizedBox(width: 4),
          _CopyValueButton(value: url, color: c.textMuted),
        ],
      ),
    );
  }

  /// Source page (referrer) row — copy + open in browser.
  Widget _buildSourcePageRow(AppColors c, String referrer) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 60,
            child: Text(
              currentS.infoSourcePage,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
          ),
          Expanded(
            child: Text(
              referrer,
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(
                fontSize: 11,
                color: c.textSecondary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
          const SizedBox(width: 4),
          _CopyValueButton(value: referrer, color: c.textMuted),
          ShadButton.ghost(
            onPressed: () => launchUrl(Uri.parse(referrer)),
            size: ShadButtonSize.sm,
            width: 24,
            height: 24,
            padding: EdgeInsets.zero,
            child: Icon(
              LucideIcons.externalLink,
              size: 12,
              color: c.textMuted,
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildErrorRow(AppColors c, String message) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 60,
            child: Text(
              currentS.infoError,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
          ),
          Expanded(
            child: Text(
              message,
              style: TextStyle(
                fontSize: 11,
                color: c.textSecondary,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
          const SizedBox(width: 4),
          _CopyValueButton(
            value: message,
            color: c.textMuted,
            toastText: currentS.errorCopied,
          ),
        ],
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 队列 Tab
  // ---------------------------------------------------------------------------

  Widget _buildQueueTab(AppColors c, DownloadTask task) {
    final s = currentS;
    final queues = widget.controller.queues;
    return _tabScroll(
      Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          for (final q in queues)
            _QueueSelectRow(
              queue: q,
              isCurrent: q.queueId == task.queueId,
              onTap: () {
                if (q.queueId == task.queueId) return;
                widget.controller.moveTaskToQueue(task.id, q.queueId);
                FluxSonner.of(context).show(
                  ShadToast(
                    title: Text(
                      s.detailQueueMovedToast(queueDisplayName(s, q)),
                    ),
                    duration: const Duration(seconds: 2),
                  ),
                );
              },
            ),
          const SizedBox(height: 10),
          Text(
            s.detailQueueMoveHint,
            style: TextStyle(fontSize: 10.5, color: c.textMuted, height: 1.5),
          ),
        ],
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 日志 Tab —— 只展示真实可得事件（本次会话内存记录，不持久化）
  // ---------------------------------------------------------------------------

  Widget _buildLogTab(AppColors c, DownloadTask task) {
    final s = currentS;
    final rows = <Widget>[
      _buildLogRow(c, _formatDateTime(task.createdAt), s.detailLogCreated),
    ];
    for (final split in task.recentSplits) {
      final kind = split.isProactive
          ? s.detailSplitProactive
          : s.detailSplitReactive;
      final size = DownloadTask.formatBytes(
        split.childEnd - split.childStart + 1,
      );
      rows.add(
        _buildLogRow(
          c,
          _formatDateTime(split.receivedAt),
          s.detailLogSplit(
            split.parentIndex + 1,
            split.childIndex + 1,
            size,
            kind,
          ),
        ),
      );
    }
    if (task.completedAt != null) {
      rows.add(
        _buildLogRow(
          c,
          _formatDateTime(task.completedAt!),
          s.detailLogCompleted,
        ),
      );
    }
    if (task.status == TaskStatus.error && task.errorMessage.isNotEmpty) {
      rows.add(
        _buildLogRow(
          c,
          null,
          s.detailLogFailed(task.errorMessage),
          isError: true,
        ),
      );
    }
    return _tabScroll(
      Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(LucideIcons.info, size: 11, color: c.textMuted),
              const SizedBox(width: 4),
              Expanded(
                child: Text(
                  s.detailLogHint,
                  style: TextStyle(fontSize: 10.5, color: c.textMuted),
                ),
              ),
            ],
          ),
          const SizedBox(height: 12),
          if (rows.isEmpty)
            Text(
              s.detailLogEmpty,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            )
          else
            ...rows,
        ],
      ),
    );
  }

  Widget _buildLogRow(
    AppColors c,
    String? time,
    String text, {
    bool isError = false,
  }) {
    final textWidget = Text(
      text,
      style: TextStyle(
        fontSize: 11,
        fontFamily: 'monospace',
        color: isError ? AppColors.red : c.textSecondary,
      ),
    );
    if (time == null) {
      return Padding(
        padding: const EdgeInsets.only(bottom: 8),
        child: textWidget,
      );
    }
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 140,
            child: Text(
              time,
              style: TextStyle(
                fontSize: 11,
                fontFamily: 'monospace',
                color: c.textMuted,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
          Expanded(child: textWidget),
        ],
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 高级 Tab
  // ---------------------------------------------------------------------------

  Widget _buildAdvancedTab(AppColors c, DownloadTask task) {
    final s = currentS;
    return _tabScroll(
      Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _buildInfoRow(
            s.taskChecksum,
            task.checksum.isEmpty ? s.detailNotSet : task.checksum,
            c,
          ),
          _buildInfoRow(
            s.taskProxy,
            task.proxyUrl.isEmpty ? s.detailFollowGlobal : task.proxyUrl,
            c,
          ),
          _buildThreadsConfigRow(c, task),
        ],
      ),
    );
  }
}

// =============================================================================
// 队列 Tab —— 单选队列行（同 queue_manager_dialog.dart `_MoveTargetRow` 视觉）
// =============================================================================

class _QueueSelectRow extends StatefulWidget {
  final DownloadQueue queue;
  final bool isCurrent;
  final VoidCallback onTap;

  const _QueueSelectRow({
    required this.queue,
    required this.isCurrent,
    required this.onTap,
  });

  @override
  State<_QueueSelectRow> createState() => _QueueSelectRowState();
}

class _QueueSelectRowState extends State<_QueueSelectRow> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = currentS;
    final q = widget.queue;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          height: 38,
          margin: const EdgeInsets.only(bottom: 4),
          padding: const EdgeInsets.symmetric(horizontal: 10),
          decoration: BoxDecoration(
            color: widget.isCurrent
                ? c.accentBg
                : _hovered
                ? c.hoverBg
                : c.hoverBg.withValues(alpha: 0),
            borderRadius: m.brMd,
          ),
          child: Row(
            children: [
              Icon(
                q.queueId == kLaterQueueId
                    ? LucideIcons.clock
                    : LucideIcons.layers,
                size: 14,
                color: widget.isCurrent ? c.accent : c.textSecondary,
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  queueDisplayName(s, q),
                  style: TextStyle(
                    fontSize: 12.5,
                    color: widget.isCurrent ? c.accent : c.textPrimary,
                  ),
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              Container(
                width: 6,
                height: 6,
                decoration: BoxDecoration(
                  color: q.isRunning ? AppColors.green : c.textMuted,
                  shape: BoxShape.circle,
                ),
              ),
              if (widget.isCurrent) ...[
                const SizedBox(width: 8),
                Icon(LucideIcons.check, size: 13, color: c.accent),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 分段进度条 Painter
// =============================================================================

class _SegmentBarPainter extends CustomPainter {
  final List<SegmentData> segments;
  final int totalBytes;
  final Color emptyColor;
  final List<Color> palette;

  _SegmentBarPainter({
    required this.segments,
    required this.totalBytes,
    required this.emptyColor,
    required this.palette,
  });

  @override
  void paint(Canvas canvas, Size size) {
    // 背景
    canvas.drawRRect(
      RRect.fromRectAndRadius(Offset.zero & size, const Radius.circular(3)),
      Paint()..color = emptyColor,
    );

    if (totalBytes <= 0) return;

    for (final seg in segments) {
      final segSize = seg.endByte - seg.startByte + 1;
      if (segSize <= 0) continue;

      final xStart = (seg.startByte / totalBytes) * size.width;
      final segWidth = (segSize / totalBytes) * size.width;
      final fillRatio = (seg.downloadedBytes / segSize).clamp(0.0, 1.0);
      final fillWidth = segWidth * fillRatio;

      if (fillWidth > 0) {
        final rect = Rect.fromLTWH(xStart, 0, fillWidth, size.height);
        canvas.drawRect(
          rect,
          Paint()..color = SegmentPalette.colorFor(palette, seg.index),
        );
      }
    }
  }

  @override
  bool shouldRepaint(_SegmentBarPainter old) =>
      !identical(segments, old.segments) ||
      totalBytes != old.totalBytes ||
      emptyColor != old.emptyColor ||
      !identical(palette, old.palette);
}

// =============================================================================
// IDM 风格网格 Painter
// =============================================================================

class _SegmentGridPainter extends CustomPainter {
  final List<SegmentData> segments;
  final int totalBytes;
  final int totalCells;
  final int cols;
  final double cellSize;
  final double cellGap;
  final Color emptyColor;
  final double unfilledAlpha;
  final List<Color> palette;

  _SegmentGridPainter({
    required this.segments,
    required this.totalBytes,
    required this.totalCells,
    required this.cols,
    required this.cellSize,
    required this.cellGap,
    required this.emptyColor,
    required this.unfilledAlpha,
    required this.palette,
  });

  @override
  void paint(Canvas canvas, Size size) {
    if (totalBytes <= 0 || totalCells <= 0) return;

    final bytesPerCell = totalBytes / totalCells;
    final radius = Radius.circular(1);

    for (int i = 0; i < totalCells; i++) {
      final col = i % cols;
      final row = i ~/ cols;
      final x = col * (cellSize + cellGap);
      final y = row * (cellSize + cellGap);
      final rect = RRect.fromRectAndRadius(
        Rect.fromLTWH(x, y, cellSize, cellSize),
        radius,
      );

      final cellStart = (i * bytesPerCell).round();
      final cellEnd = ((i + 1) * bytesPerCell).round() - 1;
      final cellMid = (cellStart + cellEnd) ~/ 2;

      // 找到拥有这个字节位置的分片
      SegmentData? owner;
      for (final seg in segments) {
        if (cellMid >= seg.startByte && cellMid <= seg.endByte) {
          owner = seg;
          break;
        }
      }

      if (owner == null) {
        canvas.drawRRect(rect, Paint()..color = emptyColor);
        continue;
      }

      // 该 cell 对应的字节范围在分片内的偏移
      final offsetInSeg = cellMid - owner.startByte;
      final isDownloaded = offsetInSeg < owner.downloadedBytes;

      if (isDownloaded) {
        canvas.drawRRect(
          rect,
          Paint()..color = SegmentPalette.colorFor(palette, owner.index),
        );
      } else {
        // 未下载：分片色半透明
        canvas.drawRRect(
          rect,
          Paint()
            ..color = SegmentPalette.colorFor(
              palette,
              owner.index,
            ).withValues(alpha: unfilledAlpha),
        );
      }
    }
  }

  @override
  bool shouldRepaint(_SegmentGridPainter old) =>
      !identical(segments, old.segments) ||
      totalBytes != old.totalBytes ||
      totalCells != old.totalCells ||
      cols != old.cols ||
      cellSize != old.cellSize ||
      cellGap != old.cellGap ||
      emptyColor != old.emptyColor ||
      unfilledAlpha != old.unfilledAlpha ||
      !identical(palette, old.palette);
}

// =============================================================================
// 复制按钮（带勾号反馈）
// =============================================================================

class _CopyValueButton extends StatefulWidget {
  final String value;
  final Color color;
  final String? toastText;

  const _CopyValueButton({
    required this.value,
    required this.color,
    this.toastText,
  });

  @override
  State<_CopyValueButton> createState() => _CopyValueButtonState();
}

class _CopyValueButtonState extends State<_CopyValueButton> {
  bool _copied = false;

  Future<void> _onCopy() async {
    await Clipboard.setData(ClipboardData(text: widget.value));
    if (!mounted) return;
    setState(() => _copied = true);
    FluxSonner.of(context).show(
      ShadToast(
        title: Text(widget.toastText ?? currentS.urlCopied),
        duration: const Duration(seconds: 2),
      ),
    );
    await Future<void>.delayed(const Duration(seconds: 2));
    if (mounted) setState(() => _copied = false);
  }

  @override
  Widget build(BuildContext context) {
    return ShadButton.ghost(
      onPressed: _onCopy,
      size: ShadButtonSize.sm,
      width: 24,
      height: 24,
      padding: EdgeInsets.zero,
      child: AnimatedSwitcher(
        duration: const Duration(milliseconds: 200),
        child: Icon(
          _copied ? LucideIcons.check : LucideIcons.copy,
          key: ValueKey(_copied),
          size: 12,
          color: _copied ? const Color(0xFF22C55E) : widget.color,
        ),
      ),
    );
  }
}

// ---------------------------------------------------------------------------
// 钉底 footer 按钮（公开 widget：detail_panel_footer_overflow_test.dart
// 直接 pump 真实实现做窄宽防溢出回归，不复刻结构）。
//
// 防溢出机制（2026-07 窄面板 12/24px 溢出根因）：Flutter Flex 给非 flex
// 子项的主轴约束**无界**，ShadButton 内部 Row 因此把无界宽度传给 child——
// FittedBox 直接作 child 拿不到有限上界，scaleDown 永不生效。
// `expands: true` 把 child 包进 Expanded 变 flex 子项（shadcn button.dart
// `effectiveExpands`），获得真实剩余宽度后 FittedBox 才能整体缩小内容。
// ---------------------------------------------------------------------------

/// footer 图标+文字操作按钮（等宽排布；用户决策：不要纯图标）。
class DetailFooterActionButton extends StatelessWidget {
  final IconData icon;
  final String label;
  final VoidCallback onPressed;

  const DetailFooterActionButton({
    super.key,
    required this.icon,
    required this.label,
    required this.onPressed,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    return ShadButton.outline(
      onPressed: onPressed,
      height: 36,
      padding: const EdgeInsets.symmetric(horizontal: 8),
      expands: true,
      child: FittedBox(
        fit: BoxFit.scaleDown,
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 14, color: c.textSecondary),
            const SizedBox(width: 6),
            Text(
              label,
              maxLines: 1,
              style: TextStyle(fontSize: 12.5, color: c.textPrimary),
            ),
          ],
        ),
      ),
    );
  }
}

/// footer 主操作按钮：暂停（活跃/排队）/ 恢复中（点击暂停）/ 继续
/// （暂停/失败）。完成态由调用方省略本按钮。
class DetailFooterPrimaryButton extends StatelessWidget {
  final TaskStatus status;
  final VoidCallback onPause;
  final VoidCallback onResume;

  const DetailFooterPrimaryButton({
    super.key,
    required this.status,
    required this.onPause,
    required this.onResume,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = currentS;
    switch (status) {
      case TaskStatus.downloading:
      case TaskStatus.pending:
      case TaskStatus.preparing:
        return _filled(c, onPause, Text(s.pause, style: _kLabelStyle));
      case TaskStatus.resuming:
        return _filled(
          c,
          onPause,
          Row(
            mainAxisAlignment: MainAxisAlignment.center,
            mainAxisSize: MainAxisSize.min,
            children: [
              SizedBox(
                width: 14,
                height: 14,
                child: CircularProgressIndicator(
                  strokeWidth: 2,
                  color: m.borderStrong(const Color(0xFFFFFFFF)),
                ),
              ),
              const SizedBox(width: 8),
              // 无 Flexible/ellipsis：本 Row 在 FittedBox 的无界测量下布局
              // （Flexible 在无界主轴会断言崩溃），超宽由 scaleDown 整体缩小。
              Text(s.resumingClickPause, maxLines: 1, style: _kLabelStyle),
            ],
          ),
        );
      case TaskStatus.paused:
      case TaskStatus.error:
        return _filled(c, onResume, Text(s.resume, style: _kLabelStyle));
      case TaskStatus.completed:
        return const SizedBox.shrink();
    }
  }

  static const _kLabelStyle = TextStyle(
    fontSize: 13,
    color: Color(0xFFFFFFFF),
    fontWeight: FontWeight.w500,
  );

  Widget _filled(AppColors c, VoidCallback onPressed, Widget content) {
    return ShadButton(
      onPressed: onPressed,
      backgroundColor: c.accent,
      hoverBackgroundColor: c.accentHover,
      expands: true,
      child: FittedBox(fit: BoxFit.scaleDown, child: content),
    );
  }
}
