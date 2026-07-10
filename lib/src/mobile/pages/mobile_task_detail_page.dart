import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../../i18n/locale_provider.dart';
import '../../models/download_controller.dart';
import '../../models/download_task.dart';
import '../../theme/app_colors.dart';
import '../../theme/app_metrics.dart';
import '../mobile_ui.dart';
import '../sheets/mobile_task_action_sheet.dart';

/// 任务详情（全屏推入页）：进度 + 分段可视化 + 速度曲线 + 任务信息 + 操作
class MobileTaskDetailPage extends StatefulWidget {
  final DownloadController controller;
  final String taskId;

  const MobileTaskDetailPage({
    super.key,
    required this.controller,
    required this.taskId,
  });

  @override
  State<MobileTaskDetailPage> createState() => _MobileTaskDetailPageState();
}

class _MobileTaskDetailPageState extends State<MobileTaskDetailPage> {
  /// 近 60 秒速度采样（bytes/s），每秒一个点
  final List<int> _speedSamples = [];
  Timer? _sampleTimer;
  bool _popped = false;

  DownloadTask? get _task {
    final idx = widget.controller.tasks.indexWhere(
      (t) => t.id == widget.taskId,
    );
    return idx >= 0 ? widget.controller.tasks[idx] : null;
  }

  @override
  void initState() {
    super.initState();
    widget.controller.addListener(_onControllerChanged);
    _sampleTimer = Timer.periodic(const Duration(seconds: 1), (_) {
      final task = _task;
      if (task == null) return;
      _speedSamples.add(
        task.status == TaskStatus.downloading ? task.speed : 0,
      );
      if (_speedSamples.length > 60) _speedSamples.removeAt(0);
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    _sampleTimer?.cancel();
    widget.controller.removeListener(_onControllerChanged);
    super.dispose();
  }

  void _onControllerChanged() {
    // 任务被删除 → 返回列表
    if (_task == null && !_popped && mounted) {
      _popped = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) Navigator.of(context).maybePop();
      });
      return;
    }
    if (mounted) setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final topInset = MediaQuery.paddingOf(context).top;
    final task = _task;

    if (task == null) {
      return Container(color: c.bg);
    }

    return Container(
      color: c.bg,
      child: Stack(
        children: [
          Positioned.fill(
            child: ListView(
              padding: EdgeInsets.fromLTRB(
                m.mobilePageMargin,
                topInset + m.mobileAppBarHeight + 8,
                m.mobilePageMargin,
                40,
              ),
              children: [
                _FileHeaderCard(task: task, controller: widget.controller),
                const SizedBox(height: 12),
                _ProgressCard(task: task),
                const SizedBox(height: 12),
                _SegmentsCard(task: task),
                const SizedBox(height: 12),
                _SpeedCurveCard(task: task, samples: _speedSamples),
                const SizedBox(height: 12),
                _InfoCard(task: task),
                const SizedBox(height: 14),
                _Actions(task: task, controller: widget.controller),
              ],
            ),
          ),
          // 顶栏
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            child: ClipRect(
              child: BackdropFilter(
                filter: mobileBlurFilter,
                child: Container(
                  color: c.bg.withValues(alpha: 0.72),
                  padding: EdgeInsets.only(top: topInset),
                  child: SizedBox(
                    height: m.mobileAppBarHeight,
                    child: Row(
                      children: [
                        const SizedBox(width: 8),
                        MobileIconButton(
                          icon: LucideIcons.arrowLeft,
                          onTap: () => Navigator.of(context).maybePop(),
                        ),
                        Expanded(
                          child: Text(
                            s.mobileTaskDetail,
                            textAlign: TextAlign.center,
                            style: TextStyle(
                              fontSize: 16,
                              fontWeight: FontWeight.w600,
                              color: c.textPrimary,
                            ),
                          ),
                        ),
                        MobileIconButton(
                          icon: LucideIcons.ellipsisVertical,
                          onTap: () => showMobileTaskActionSheet(
                            context,
                            widget.controller,
                            task,
                          ),
                        ),
                        const SizedBox(width: 8),
                      ],
                    ),
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

// ─────────────────────────────────────────────
// 文件头卡片
// ─────────────────────────────────────────────

class _FileHeaderCard extends StatelessWidget {
  final DownloadTask task;
  final DownloadController controller;

  const _FileHeaderCard({required this.task, required this.controller});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final boosted = controller.priorityTaskId == task.id;

    final (Color pillBg, Color pillFg) = switch (task.status) {
      TaskStatus.downloading ||
      TaskStatus.preparing ||
      TaskStatus.resuming ||
      TaskStatus.pending => (c.accent.withValues(alpha: 0.12), c.accent),
      TaskStatus.paused => (
        c.statusWarning.withValues(alpha: 0.14),
        c.statusWarning,
      ),
      TaskStatus.completed => (
        c.statusSuccess.withValues(alpha: 0.14),
        c.statusSuccess,
      ),
      TaskStatus.error => (
        c.statusError.withValues(alpha: 0.14),
        c.statusError,
      ),
    };

    return Container(
      padding: const EdgeInsets.all(14),
      decoration: mobileCardDecoration(c, m),
      child: Row(
        children: [
          Container(
            width: 48,
            height: 48,
            decoration: BoxDecoration(
              color: c.surface2,
              borderRadius: BorderRadius.circular(12),
              border: Border.all(color: c.border),
            ),
            child: Icon(
              mobileCategoryIcon(task.fileCategory),
              size: 24,
              color: c.textSecondary,
            ),
          ),
          const SizedBox(width: 14),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  task.fileName,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(
                    fontSize: 14.5,
                    fontWeight: FontWeight.w600,
                    height: 1.35,
                    color: c.textPrimary,
                  ),
                ),
                const SizedBox(height: 5),
                Row(
                  children: [
                    Container(
                      padding: const EdgeInsets.symmetric(
                        horizontal: 8,
                        vertical: 2,
                      ),
                      decoration: BoxDecoration(
                        color: pillBg,
                        borderRadius: BorderRadius.circular(999),
                      ),
                      child: Text(
                        task.statusText,
                        style: TextStyle(
                          fontSize: 10.5,
                          fontWeight: FontWeight.w600,
                          color: pillFg,
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Text(
                      task.sizeText,
                      style: TextStyle(fontSize: 12, color: c.textSecondary),
                    ),
                    const SizedBox(width: 8),
                    Text(
                      task.protocolLabel,
                      style: TextStyle(
                        fontSize: 10,
                        fontWeight: FontWeight.w700,
                        color: c.textSecondary,
                      ),
                    ),
                    if (boosted) ...[
                      const SizedBox(width: 8),
                      Icon(LucideIcons.zap, size: 12, color: c.statusWarning),
                    ],
                  ],
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 进度卡片
// ─────────────────────────────────────────────

class _ProgressCard extends StatelessWidget {
  final DownloadTask task;

  const _ProgressCard({required this.task});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final active = task.status == TaskStatus.downloading;
    final pct = (task.progress * 100).toStringAsFixed(1);

    final barColor = switch (task.status) {
      TaskStatus.paused => c.statusWarning,
      TaskStatus.error => c.statusError,
      TaskStatus.completed => c.statusSuccess,
      _ => c.accent,
    };

    return Container(
      padding: const EdgeInsets.all(16),
      decoration: mobileCardDecoration(c, m),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.baseline,
            textBaseline: TextBaseline.alphabetic,
            children: [
              Text(
                '$pct%',
                style: TextStyle(
                  fontSize: 34,
                  fontWeight: FontWeight.w700,
                  letterSpacing: -0.5,
                  color: c.textPrimary,
                ),
              ),
              const SizedBox(width: 10),
              if (active) ...[
                Text(
                  task.speedText,
                  style: TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                    color: c.accent,
                  ),
                ),
                const Spacer(),
                Text(
                  task.etaText,
                  style: TextStyle(fontSize: 12, color: c.textSecondary),
                ),
              ],
              if (task.status == TaskStatus.error) ...[
                Expanded(
                  child: Text(
                    task.errorMessage.isEmpty
                        ? s.subtitleError
                        : task.errorMessage,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    textAlign: TextAlign.end,
                    style: TextStyle(fontSize: 12, color: c.statusError),
                  ),
                ),
              ],
            ],
          ),
          const SizedBox(height: 12),
          MobileProgressBar(
            progress: task.isIndeterminate ? 1.0 : task.progress,
            color: barColor,
            height: 8,
          ),
          const SizedBox(height: 8),
          Text(
            '${task.downloadedText} / ${task.sizeText}',
            style: TextStyle(fontSize: 12, color: c.textSecondary),
          ),
        ],
      ),
    );
  }
}

// ─────────────────────────────────────────────
// 分段可视化（IDM 风格网格）
// ─────────────────────────────────────────────

class _SegmentsCard extends StatelessWidget {
  final DownloadTask task;

  static const _cells = 48;

  const _SegmentsCard({required this.task});

  List<double> _cellFills() => mobileSegmentCellFills(task, _cells);

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final active = task.status == TaskStatus.downloading;
    final fills = _cellFills();
    final segCount = task.segments?.length ?? 0;

    Color cellColor(double fill) {
      if (fill >= 0.95) return c.accent;
      if (fill > 0) {
        return (task.status == TaskStatus.paused
                ? c.statusWarning
                : c.accent)
            .withValues(alpha: 0.35 + fill * 0.4);
      }
      return c.switchTrack;
    }

    return Container(
      padding: const EdgeInsets.all(16),
      decoration: mobileCardDecoration(c, m),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  s.mobileSegTitle,
                  style: TextStyle(
                    fontSize: 12,
                    fontWeight: FontWeight.w600,
                    color: c.textMuted,
                    letterSpacing: 0.5,
                  ),
                ),
              ),
              Text(
                segCount > 0
                    ? '${s.infoThreads(segCount)} · ${active ? s.mobileSegRunning : s.mobileSegStopped}'
                    : (active ? s.mobileSegRunning : s.mobileSegStopped),
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
            ],
          ),
          const SizedBox(height: 12),
          GridView.count(
            crossAxisCount: 16,
            shrinkWrap: true,
            physics: const NeverScrollableScrollPhysics(),
            mainAxisSpacing: 3,
            crossAxisSpacing: 3,
            children: [
              for (final fill in fills)
                DecoratedBox(
                  decoration: BoxDecoration(
                    color: cellColor(fill),
                    borderRadius: BorderRadius.circular(2.5),
                  ),
                ),
            ],
          ),
          const SizedBox(height: 12),
          Row(
            children: [
              _legend(c.accent, s.mobileSegDone, c),
              const SizedBox(width: 14),
              _legend(c.accent.withValues(alpha: 0.55), s.mobileSegActive, c),
              const SizedBox(width: 14),
              _legend(c.switchTrack, s.mobileSegPending, c),
            ],
          ),
        ],
      ),
    );
  }

  Widget _legend(Color color, String label, AppColors c) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Container(
          width: 9,
          height: 9,
          decoration: BoxDecoration(
            color: color,
            borderRadius: BorderRadius.circular(2),
          ),
        ),
        const SizedBox(width: 5),
        Text(label, style: TextStyle(fontSize: 11, color: c.textSecondary)),
      ],
    );
  }
}

// ─────────────────────────────────────────────
// 速度曲线（近 60 秒真实采样）
// ─────────────────────────────────────────────

class _SpeedCurveCard extends StatelessWidget {
  final DownloadTask task;
  final List<int> samples;

  const _SpeedCurveCard({required this.task, required this.samples});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final peak = samples.isEmpty
        ? 0
        : samples.reduce((a, b) => a > b ? a : b);

    return Container(
      padding: const EdgeInsets.all(16),
      decoration: mobileCardDecoration(c, m),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            s.mobileSpeedCurve,
            style: TextStyle(
              fontSize: 12,
              fontWeight: FontWeight.w600,
              color: c.textMuted,
              letterSpacing: 0.5,
            ),
          ),
          const SizedBox(height: 10),
          SizedBox(
            height: 68,
            width: double.infinity,
            child: CustomPaint(
              painter: _SparkPainter(
                samples: List.of(samples),
                color: c.accent,
              ),
            ),
          ),
          const SizedBox(height: 6),
          Row(
            children: [
              Text(
                s.mobileSpeedWindow,
                style: TextStyle(fontSize: 11, color: c.textSecondary),
              ),
              const Spacer(),
              Text(
                s.mobileSpeedPeak(
                  '${DownloadTask.formatBytes(peak)}/s',
                ),
                style: TextStyle(fontSize: 11, color: c.textSecondary),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _SparkPainter extends CustomPainter {
  final List<int> samples;
  final Color color;

  _SparkPainter({required this.samples, required this.color});

  @override
  void paint(Canvas canvas, Size size) {
    if (samples.length < 2) return;
    final maxV = samples.reduce((a, b) => a > b ? a : b);
    if (maxV <= 0) return;

    final n = samples.length;
    final points = <Offset>[
      for (var i = 0; i < n; i++)
        Offset(
          size.width * i / (n - 1),
          size.height -
              (samples[i] / maxV) * (size.height - 6) -
              2,
        ),
    ];

    final line = Path()..moveTo(points.first.dx, points.first.dy);
    for (final p in points.skip(1)) {
      line.lineTo(p.dx, p.dy);
    }

    final area = Path.from(line)
      ..lineTo(size.width, size.height)
      ..lineTo(0, size.height)
      ..close();

    canvas.drawPath(
      area,
      Paint()
        ..shader = LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [
            color.withValues(alpha: 0.24),
            color.withValues(alpha: 0.0),
          ],
        ).createShader(Offset.zero & size),
    );
    canvas.drawPath(
      line,
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2
        ..strokeCap = StrokeCap.round
        ..strokeJoin = StrokeJoin.round
        ..color = color,
    );
  }

  @override
  bool shouldRepaint(_SparkPainter oldDelegate) =>
      oldDelegate.samples != samples || oldDelegate.color != color;
}

// ─────────────────────────────────────────────
// 任务信息卡片
// ─────────────────────────────────────────────

class _InfoCard extends StatelessWidget {
  final DownloadTask task;

  const _InfoCard({required this.task});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final created = task.createdAt;
    final createdText =
        '${created.year}-${created.month.toString().padLeft(2, '0')}-'
        '${created.day.toString().padLeft(2, '0')} '
        '${created.hour.toString().padLeft(2, '0')}:'
        '${created.minute.toString().padLeft(2, '0')}';

    Widget row(
      String key,
      String value, {
      bool copyable = false,
      String? copyToast,
    }) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 9),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SizedBox(
              width: 68,
              child: Text(
                key,
                style: TextStyle(fontSize: 12.5, color: c.textMuted),
              ),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                value,
                style: TextStyle(
                  fontSize: 12.5,
                  height: 1.45,
                  color: c.textPrimary,
                ),
              ),
            ),
            if (copyable)
              GestureDetector(
                onTap: () {
                  Clipboard.setData(ClipboardData(text: value));
                  showMobileToast(context, copyToast ?? s.urlCopied);
                },
                child: Padding(
                  padding: const EdgeInsets.only(left: 8),
                  child: Icon(LucideIcons.copy, size: 13, color: c.textMuted),
                ),
              ),
          ],
        ),
      );
    }

    return Container(
      padding: const EdgeInsets.all(16),
      decoration: mobileCardDecoration(c, m),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            s.mobileTaskInfo,
            style: TextStyle(
              fontSize: 12,
              fontWeight: FontWeight.w600,
              color: c.textMuted,
              letterSpacing: 0.5,
            ),
          ),
          const SizedBox(height: 4),
          row(s.infoUrl, task.url, copyable: true),
          _divider(c),
          row(s.infoPath, task.saveDir),
          _divider(c),
          row(s.mobileProtocol, task.protocolLabel),
          _divider(c),
          row(s.mobileCreatedAt, createdText),
          if (task.errorMessage.isNotEmpty) ...[
            _divider(c),
            row(
              s.infoError,
              task.errorMessage,
              copyable: true,
              copyToast: s.errorCopied,
            ),
          ],
        ],
      ),
    );
  }

  Widget _divider(AppColors c) => Container(height: 1, color: c.border);
}

// ─────────────────────────────────────────────
// 操作区
// ─────────────────────────────────────────────

class _Actions extends StatelessWidget {
  final DownloadTask task;
  final DownloadController controller;

  const _Actions({required this.task, required this.controller});

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final active = task.status == TaskStatus.downloading ||
        task.status == TaskStatus.preparing ||
        task.status == TaskStatus.resuming;
    final boosted = controller.priorityTaskId == task.id;

    final children = <Widget>[];
    if (task.status == TaskStatus.completed) {
      children.add(
        Row(
          children: [
            Expanded(
              child: MobilePrimaryButton(
                label: s.copyUrl,
                icon: LucideIcons.copy,
                onTap: () {
                  Clipboard.setData(ClipboardData(text: task.url));
                  showMobileToast(context, s.urlCopied);
                },
              ),
            ),
          ],
        ),
      );
    } else {
      children.add(
        Row(
          children: [
            Expanded(
              child: MobilePrimaryButton(
                label: active
                    ? s.pause
                    : task.status == TaskStatus.error
                        ? s.mobileRetry
                        : s.resume,
                icon: active ? LucideIcons.pause : LucideIcons.play,
                onTap: () => toggleMobileTask(controller, task),
              ),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: MobilePrimaryButton(
                label: boosted ? s.mobileBoosted : s.mobileBoostAction,
                icon: LucideIcons.zap,
                filled: false,
                onTap: () {
                  controller.setPriorityTask(boosted ? '' : task.id);
                  showMobileToast(
                    context,
                    boosted ? s.mobileBoostOff : s.mobileBoostOn,
                  );
                },
              ),
            ),
          ],
        ),
      );
    }
    children.add(const SizedBox(height: 10));
    children.add(
      MobilePrimaryButton(
        label: s.deleteTask,
        icon: LucideIcons.trash2,
        destructive: true,
        onTap: () => confirmMobileDeleteTask(
          context,
          controller,
          task,
          deleteFiles: false,
        ),
      ),
    );

    return Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: children);
  }
}
