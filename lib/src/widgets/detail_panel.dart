import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import '../theme/segment_palette.dart';

class DetailPanel extends StatelessWidget {
  final DownloadController controller;
  final VoidCallback onClose;

  const DetailPanel({
    super.key,
    required this.controller,
    required this.onClose,
  });

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
              listenable: controller,
              builder: (context, _) {
                final task = controller.selectedTask;
                if (task == null) return _buildNoSelection(c);
                return Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Expanded(
                      child: SingleChildScrollView(
                        padding: const EdgeInsets.all(16),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
            _buildFileInfo(c, m, task),
                            const SizedBox(height: 20),
            _buildProgress(c, m, task),
                            const SizedBox(height: 20),
                            _buildInfoTable(c, task),
                          ],
                        ),
                      ),
                    ),
                    _buildActions(c, m, task),
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
          ShadButton.ghost(
            onPressed: onClose,
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

  Widget _buildFileInfo(AppColors c, AppMetrics m, DownloadTask task) {
    return Row(
      children: [
        Container(
          width: 40,
          height: 40,
          decoration: BoxDecoration(
            color: c.surface2,
            borderRadius: m.brCard,
          ),
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
          child: Text(
            task.fileName,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(fontSize: 13, color: c.textPrimary),
          ),
        ),
      ],
    );
  }

  // ---------------------------------------------------------------------------
  // 进度区域：百分比 + 分段进度条 + IDM 网格 + 图例
  // ---------------------------------------------------------------------------

  Widget _buildProgress(AppColors c, AppMetrics m, DownloadTask task) {
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

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // 百分比大字
        Text(
          '$pctStr%',
          style: TextStyle(
            fontSize: 26,
            fontWeight: FontWeight.w600,
            color: c.textPrimary,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
        const SizedBox(height: 8),

        // 分段进度条
        if (hasSegs)
          _buildSegmentedBar(c, m, segs!, task.totalBytes)
        else
          _buildSimpleBar(c, m, pctValue),

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
      ],
    );
  }

  /// 无分片数据时的简单进度条
  Widget _buildSimpleBar(AppColors c, AppMetrics m, double progress) {
    return Container(
      height: 4,
      decoration: BoxDecoration(
        color: c.surface3,
        borderRadius: m.brXs,
      ),
      child: FractionallySizedBox(
        alignment: Alignment.centerLeft,
        widthFactor: progress,
        child: Container(
          decoration: BoxDecoration(
            color: c.accent,
            borderRadius: m.brXs,
          ),
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
                    unfilledAlpha: c.bg.computeLuminance() < 0.5 ? m.alphaMuted : m.alphaActive,
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
  Widget _buildSegmentLegend(AppColors c, AppMetrics m, List<SegmentData> segs) {
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

  // ---------------------------------------------------------------------------
  // 信息表
  // ---------------------------------------------------------------------------

  Widget _buildInfoTable(AppColors c, DownloadTask task) {
    final segs = task.segments;
    final activeCount =
        segs != null ? segs.where((s) => s.progress < 1.0).length : 0;
    final segCount = activeCount > 0 ? activeCount : null;
    final splitCount = task.recentSplits.length;

    return Column(
      children: [
        _buildInfoRow(currentS.infoSize, task.sizeText, c),
        _buildInfoRow(currentS.infoDownloaded, task.downloadedText, c),
        _buildInfoRow(currentS.infoSpeed, task.speedText, c),
        _buildInfoRow(currentS.infoRemaining, task.etaText, c),
        _buildInfoRow(currentS.infoStatus, task.statusText, c),
        if (segCount != null)
          _buildInfoRow(currentS.threads, currentS.infoThreads(segCount), c),
        if (splitCount > 0) _buildSplitInfoRow(c, task),
        _buildInfoRow(currentS.infoPath, task.saveDir, c),
        _buildUrlRow(c, task.url),
        if (task.errorMessage.isNotEmpty)
          _buildInfoRow(currentS.infoError, task.errorMessage, c),
      ],
    );
  }

  /// 动态分段拆分信息行 — 显示拆分次数和最近拆分详情
  Widget _buildSplitInfoRow(AppColors c, DownloadTask task) {
    final splits = task.recentSplits;
    if (splits.isEmpty) return const SizedBox.shrink();

    final latest = splits.last;
    final proactiveCount = splits.where((s) => s.isProactive).length;
    final reactiveCount = splits.length - proactiveCount;

    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 60,
            child: Row(
              children: [
                Icon(LucideIcons.split, size: 11, color: c.accent),
                const SizedBox(width: 3),
                Text(
                  currentS.dynamicSplit,
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
              ],
            ),
          ),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  currentS.splitCount(
                    splits.length,
                    reactiveCount,
                    proactiveCount,
                  ),
                  style: TextStyle(
                    fontSize: 11,
                    color: c.textSecondary,
                    fontFeatures: const [FontFeature.tabularFigures()],
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  currentS.splitLatest(
                    latest.parentIndex + 1,
                    latest.childIndex + 1,
                    DownloadTask.formatBytes(
                      latest.childEnd - latest.childStart + 1,
                    ),
                  ),
                  style: TextStyle(
                    fontSize: 10,
                    color: c.textMuted,
                    fontFeatures: const [FontFeature.tabularFigures()],
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

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
          _CopyUrlButton(url: url, color: c.textMuted),
        ],
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 操作按钮
  // ---------------------------------------------------------------------------

  Widget _buildActions(AppColors c, AppMetrics m, DownloadTask task) {
    return Container(
      padding: const EdgeInsets.all(16),
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: c.border, width: 1)),
      ),
      child: Column(
        children: [
          // 暂停 / 恢复
          if (task.status == TaskStatus.downloading ||
              task.status == TaskStatus.pending ||
              task.status == TaskStatus.preparing)
            SizedBox(
              width: double.infinity,
              child: ShadButton(
                onPressed: () => controller.pauseTask(task.id),
                backgroundColor: c.accent,
                hoverBackgroundColor: c.accentHover,
                child: Text(
                  currentS.pause,
                  style: const TextStyle(
                    fontSize: 13,
                    color: Colors.white,
                    fontWeight: FontWeight.w500,
                  ),
                ),
              ),
            )
          else if (task.status == TaskStatus.resuming)
            SizedBox(
              width: double.infinity,
              child: ShadButton(
                onPressed: () => controller.pauseTask(task.id),
                backgroundColor: c.accent,
                hoverBackgroundColor: c.accentHover,
                child: Row(
                  mainAxisAlignment: MainAxisAlignment.center,
                  children: [
                    SizedBox(
                      width: 14,
                      height: 14,
                      child: CircularProgressIndicator(
                        strokeWidth: 2,
                        color: m.borderStrong(Colors.white),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Text(
                      currentS.resumingClickPause,
                      style: const TextStyle(
                        fontSize: 13,
                        color: Colors.white,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ],
                ),
              ),
            )
          else if (task.status == TaskStatus.paused ||
              task.status == TaskStatus.error)
            SizedBox(
              width: double.infinity,
              child: ShadButton(
                onPressed: () => controller.resumeTask(task.id),
                backgroundColor: c.accent,
                hoverBackgroundColor: c.accentHover,
                child: Text(
                  currentS.resume,
                  style: const TextStyle(
                    fontSize: 13,
                    color: Colors.white,
                    fontWeight: FontWeight.w500,
                  ),
                ),
              ),
            ),
          const SizedBox(height: 8),
          SizedBox(
            width: double.infinity,
            child: ShadButton.destructive(
              onPressed: () =>
                  controller.deleteTask(task.id, deleteFiles: true),
              child: Text(
                currentS.deleteTaskAndFile,
                style: const TextStyle(fontSize: 13, color: Colors.white),
              ),
            ),
          ),
        ],
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

class _CopyUrlButton extends StatefulWidget {
  final String url;
  final Color color;

  const _CopyUrlButton({required this.url, required this.color});

  @override
  State<_CopyUrlButton> createState() => _CopyUrlButtonState();
}

class _CopyUrlButtonState extends State<_CopyUrlButton> {
  bool _copied = false;

  Future<void> _onCopy() async {
    await Clipboard.setData(ClipboardData(text: widget.url));
    if (!mounted) return;
    setState(() => _copied = true);
    ShadSonner.of(context).show(
      ShadToast(
        title: Text(currentS.urlCopied),
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
