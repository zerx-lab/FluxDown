import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/download_task.dart';
import '../models/ua_presets.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

/// 打开队列管理对话框。
Future<void> showQueueManagerDialog(
  BuildContext context,
  DownloadController controller,
  String queueId,
) {
  return showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (_) => QueueManagerDialog(controller: controller, queueId: queueId),
  );
}

/// 打开「移动到队列」选择对话框：列出全部队列，点击即移动并关闭。
/// 当前所属队列高亮并带对勾（点击视为无操作，直接关闭）。
Future<void> showMoveToQueueDialog(
  BuildContext context,
  DownloadController controller,
  DownloadTask task,
) {
  return showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) {
      final s = LocaleScope.of(ctx);
      final c = AppColors.of(ctx);
      final m = AppMetrics.of(ctx);
      final queues = controller.queues;
      return ShadDialog(
        title: Text(s.moveToQueueAction),
        description: Text(
          task.fileName,
          overflow: TextOverflow.ellipsis,
          maxLines: 1,
        ),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
        ],
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 12),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              for (final q in queues)
                _MoveTargetRow(
                  queue: q,
                  isCurrent: q.queueId == task.queueId,
                  c: c,
                  m: m,
                  s: s,
                  onTap: () {
                    if (q.queueId != task.queueId) {
                      controller.moveTaskToQueue(task.id, q.queueId);
                    }
                    Navigator.of(ctx).pop();
                  },
                ),
            ],
          ),
        ),
      );
    },
  );
}

/// 队列管理对话框：三个分区 + 即时启停。
///
/// - 「设置」：名称（内置队列不可改）/ 限速 / 并发 / 默认线程 / 默认目录 / UA；
/// - 「定时」：每日定时启停（HH:MM）+ 生效星期（位掩码 bit0=周一）；
/// - 「任务」：队列内未完成任务的启动顺序，上移/下移立即持久化。
///
/// 设置与定时经「保存」一次提交（UpdateQueue + SetQueueSchedule 两个信号）；
/// 启动/停止队列按钮即时生效，不需要保存。
class QueueManagerDialog extends StatefulWidget {
  final DownloadController controller;
  final String queueId;

  const QueueManagerDialog({
    super.key,
    required this.controller,
    required this.queueId,
  });

  @override
  State<QueueManagerDialog> createState() => _QueueManagerDialogState();
}

class _QueueManagerDialogState extends State<QueueManagerDialog> {
  int _tab = 0;

  // ── 设置 ──
  late final TextEditingController _nameCtrl;
  late final TextEditingController _speedCtrl;
  late final TextEditingController _concurrentCtrl;
  late final TextEditingController _saveDirCtrl;
  late final TextEditingController _uaCtrl;
  late String _selectedSegments;
  late String _selectedUaPreset;

  // ── 定时 ──
  late bool _scheduleEnabled;
  late final TextEditingController _startCtrl;
  late final TextEditingController _stopCtrl;
  late int _days;
  String _scheduleError = '';

  static const _segmentOptions = ['0', '4', '8', '16', '32', '64'];

  /// 队列实时快照；对话框打开期间队列被删除时回退为占位对象（仅防御，
  /// 内置队列不可删除，自定义队列删除入口会先关闭本对话框）。
  DownloadQueue get _queue =>
      widget.controller.queueById(widget.queueId) ??
      DownloadQueue(
        queueId: widget.queueId,
        name: widget.queueId,
        speedLimitKbps: 0,
        maxConcurrent: 0,
        defaultSaveDir: '',
        position: 0,
      );

  @override
  void initState() {
    super.initState();
    final q = _queue;
    _nameCtrl = TextEditingController(text: q.name);
    _speedCtrl = TextEditingController(
      text: q.speedLimitKbps > 0 ? q.speedLimitKbps.toString() : '',
    );
    _concurrentCtrl = TextEditingController(
      text: q.maxConcurrent > 0 ? q.maxConcurrent.toString() : '',
    );
    _saveDirCtrl = TextEditingController(text: q.defaultSaveDir);
    _uaCtrl = TextEditingController(text: q.defaultUserAgent);
    _selectedSegments = q.defaultSegments > 0
        ? q.defaultSegments.toString()
        : '0';
    _selectedUaPreset = _detectPreset(q.defaultUserAgent);
    _scheduleEnabled = q.scheduleEnabled;
    _startCtrl = TextEditingController(text: q.scheduleStart);
    _stopCtrl = TextEditingController(text: q.scheduleStop);
    _days = q.scheduleDays & 0x7f;
    if (_days == 0) _days = 0x7f;
  }

  @override
  void dispose() {
    _nameCtrl.dispose();
    _speedCtrl.dispose();
    _concurrentCtrl.dispose();
    _saveDirCtrl.dispose();
    _uaCtrl.dispose();
    _startCtrl.dispose();
    _stopCtrl.dispose();
    super.dispose();
  }

  static String _detectPreset(String ua) {
    final detected = detectUaPreset(ua);
    return detected == 'default' ? '' : detected;
  }

  /// HH:MM 校验（空 = 该边沿不定时，合法）。
  static bool _validHhmm(String v) {
    if (v.isEmpty) return true;
    final m = RegExp(r'^(\d{1,2}):(\d{2})$').firstMatch(v);
    if (m == null) return false;
    final h = int.parse(m.group(1)!);
    final min = int.parse(m.group(2)!);
    return h < 24 && min < 60;
  }

  void _save() {
    final s = LocaleScope.of(context);
    final start = _startCtrl.text.trim();
    final stop = _stopCtrl.text.trim();
    if (!_validHhmm(start) || !_validHhmm(stop)) {
      setState(() {
        _scheduleError = s.queueScheduleTimeInvalid;
        _tab = 1;
      });
      return;
    }
    // 启用定时但两个时刻都空 = 无任何可执行动作，拦截并提示（引擎侧
    // 也会兜底归一为未启用，此处给用户即时反馈而非静默丢弃）。
    if (_scheduleEnabled && start.isEmpty && stop.isEmpty) {
      setState(() {
        _scheduleError = s.scheduleNeedOneTime;
        _tab = 1;
      });
      return;
    }
    final q = _queue;
    final name = _nameCtrl.text.trim();
    if (!q.isBuiltin && name.isEmpty) {
      setState(() => _tab = 0);
      return;
    }
    final speedLimit = (int.tryParse(_speedCtrl.text.trim()) ?? 0).clamp(
      0,
      1 << 30,
    );
    final maxConcurrent = (int.tryParse(_concurrentCtrl.text.trim()) ?? 0)
        .clamp(0, 100);
    widget.controller.updateQueue(
      queueId: widget.queueId,
      // 内置队列名称固定（引擎侧同样拒绝改名），提交存量名即可。
      name: q.isBuiltin ? q.name : name,
      speedLimitKbps: speedLimit,
      maxConcurrent: maxConcurrent,
      defaultSaveDir: _saveDirCtrl.text.trim(),
      defaultSegments: int.tryParse(_selectedSegments) ?? 0,
      defaultUserAgent: _uaCtrl.text.trim(),
    );
    widget.controller.setQueueSchedule(
      queueId: widget.queueId,
      enabled: _scheduleEnabled,
      startTime: start,
      stopTime: stop,
      days: _days == 0 ? 0x7f : _days,
    );
    Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);

    // 队列运行态与任务列表随控制器实时刷新（启停按钮/任务 Tab 立即反映）。
    return ListenableBuilder(
      listenable: widget.controller,
      builder: (context, _) {
        final q = _queue;
        return ShadDialog(
          title: Row(
            children: [
              Container(
                width: 28,
                height: 28,
                decoration: BoxDecoration(
                  color: m.soft(c.accent),
                  borderRadius: m.brMd,
                ),
                child: Icon(
                  q.queueId == kLaterQueueId
                      ? LucideIcons.clock
                      : LucideIcons.layers,
                  size: 14,
                  color: c.accent,
                ),
              ),
              const SizedBox(width: 10),
              Flexible(
                child: Text(
                  queueDisplayName(s, q),
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              const SizedBox(width: 8),
              _RunBadge(running: q.isRunning, s: s, c: c, m: m),
            ],
          ),
          actions: [
            ShadButton.outline(
              onPressed: () => q.isRunning
                  ? widget.controller.stopQueue(q.queueId)
                  : widget.controller.startQueue(q.queueId),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(
                    q.isRunning ? LucideIcons.pause : LucideIcons.play,
                    size: 13,
                    color: c.textSecondary,
                  ),
                  const SizedBox(width: 6),
                  Text(q.isRunning ? s.stopQueueAction : s.startQueueAction),
                ],
              ),
            ),
            ShadButton.outline(
              onPressed: () => Navigator.of(context).pop(),
              child: Text(s.cancel),
            ),
            ShadButton(onPressed: _save, child: Text(s.confirm)),
          ],
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 12),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _buildTabBar(s, c, m),
                const SizedBox(height: 14),
                switch (_tab) {
                  0 => _buildSettingsTab(s, c, q),
                  1 => _buildScheduleTab(s, c, m),
                  _ => _buildTasksTab(s, c, m, q),
                },
              ],
            ),
          ),
        );
      },
    );
  }

  // ─────────────────────────────────────────────
  // Tab 切换条
  // ─────────────────────────────────────────────

  Widget _buildTabBar(S s, AppColors c, AppMetrics m) {
    final labels = [s.queueTabSettings, s.queueTabSchedule, s.queueTabTasks];
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
                  color: _tab == i ? c.accentBg : Colors.transparent,
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

  // ─────────────────────────────────────────────
  // 设置
  // ─────────────────────────────────────────────

  Widget _fieldLabel(String text, AppColors c) => Text(
    text,
    style: TextStyle(
      fontSize: 11.5,
      fontWeight: FontWeight.w500,
      color: c.textSecondary,
    ),
  );

  Widget _buildSettingsTab(S s, AppColors c, DownloadQueue q) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _fieldLabel(s.queueNameLabel, c),
        const SizedBox(height: 6),
        ShadInput(
          controller: _nameCtrl,
          enabled: !q.isBuiltin,
          placeholder: Text(
            q.isBuiltin ? queueDisplayName(s, q) : s.queueNameHint,
          ),
        ),
        if (q.isBuiltin) ...[
          const SizedBox(height: 4),
          Text(
            s.builtinQueueRenameHint,
            style: TextStyle(fontSize: 11, color: c.textMuted),
          ),
        ],
        const SizedBox(height: 12),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.queueSpeedLimit, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _speedCtrl,
                    placeholder: Text(s.queueSpeedLimitHint),
                    keyboardType: TextInputType.number,
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.queueMaxConcurrent, c),
                  const SizedBox(height: 6),
                  ShadInput(
                    controller: _concurrentCtrl,
                    placeholder: Text(s.queueMaxConcurrentHint),
                    keyboardType: TextInputType.number,
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.queueDefaultSegments, c),
                  const SizedBox(height: 6),
                  SizedBox(
                    width: double.infinity,
                    child: ShadSelect<String>(
                      initialValue: _selectedSegments,
                      onChanged: (v) {
                        if (v != null) setState(() => _selectedSegments = v);
                      },
                      options: _segmentOptions
                          .map(
                            (opt) => ShadOption(
                              value: opt,
                              child: Text(
                                opt == '0' ? s.queueDefaultSegmentsHint : opt,
                              ),
                            ),
                          )
                          .toList(),
                      selectedOptionBuilder: (ctx, v) =>
                          Text(v == '0' ? s.queueDefaultSegmentsHint : v),
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 12),
        _fieldLabel(s.queueSaveDir, c),
        const SizedBox(height: 6),
        ShadInput(
          controller: _saveDirCtrl,
          placeholder: Text(s.queueDirInheritHint),
        ),
        const SizedBox(height: 12),
        _fieldLabel(s.queueDefaultUserAgent, c),
        const SizedBox(height: 6),
        Row(
          children: [
            SizedBox(
              width: 130,
              child: ShadSelect<String>(
                initialValue: _selectedUaPreset,
                options: [
                  ShadOption(value: '', child: Text(s.queueUaInheritGlobal)),
                  ShadOption(
                    value: 'chrome',
                    child: Text(s.userAgentPresetChrome),
                  ),
                  ShadOption(
                    value: 'firefox',
                    child: Text(s.userAgentPresetFirefox),
                  ),
                  ShadOption(value: 'edge', child: Text(s.userAgentPresetEdge)),
                  ShadOption(
                    value: 'safari',
                    child: Text(s.userAgentPresetSafari),
                  ),
                  ShadOption(
                    value: 'custom',
                    child: Text(s.userAgentPresetCustom),
                  ),
                ],
                selectedOptionBuilder: (ctx, v) {
                  final label = switch (v) {
                    'chrome' => 'Chrome',
                    'firefox' => 'Firefox',
                    'edge' => 'Edge',
                    'safari' => 'Safari',
                    'custom' => s.userAgentPresetCustom,
                    _ => s.queueUaInheritGlobal,
                  };
                  return Text(label, overflow: TextOverflow.ellipsis, maxLines: 1);
                },
                onChanged: (preset) {
                  if (preset == null) return;
                  setState(() => _selectedUaPreset = preset);
                  if (preset != 'custom') {
                    _uaCtrl.text = kUaPresets[preset] ?? '';
                  }
                },
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: ShadInput(
                controller: _uaCtrl,
                placeholder: Text(s.queueUaHint),
                onChanged: (v) {
                  final detected = _detectPreset(v);
                  if (detected != _selectedUaPreset) {
                    setState(() => _selectedUaPreset = detected);
                  }
                },
              ),
            ),
          ],
        ),
      ],
    );
  }

  // ─────────────────────────────────────────────
  // 定时
  // ─────────────────────────────────────────────

  Widget _buildScheduleTab(S s, AppColors c, AppMetrics m) {
    final weekdays = s.weekdaysShort.split(',');
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(child: _fieldLabel(s.queueScheduleEnable, c)),
            ShadSwitch(
              value: _scheduleEnabled,
              onChanged: (v) => setState(() => _scheduleEnabled = v),
            ),
          ],
        ),
        const SizedBox(height: 6),
        Text(
          s.queueScheduleDesc,
          style: TextStyle(fontSize: 11, color: c.textMuted),
        ),
        const SizedBox(height: 14),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.queueScheduleStartLabel, c),
                  const SizedBox(height: 6),
                  _TimePicker(
                    value: _startCtrl.text.trim(),
                    enabled: _scheduleEnabled,
                    c: c,
                    hint: s.queueScheduleTimeHint,
                    onChanged: (v) => setState(() {
                      _startCtrl.text = v;
                      _scheduleError = '';
                    }),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  _fieldLabel(s.queueScheduleStopLabel, c),
                  const SizedBox(height: 6),
                  _TimePicker(
                    value: _stopCtrl.text.trim(),
                    enabled: _scheduleEnabled,
                    c: c,
                    hint: s.queueScheduleTimeHint,
                    onChanged: (v) => setState(() {
                      _stopCtrl.text = v;
                      _scheduleError = '';
                    }),
                  ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 14),
        _fieldLabel(s.queueScheduleDays, c),
        const SizedBox(height: 8),
        Row(
          children: [
            for (var i = 0; i < 7; i++) ...[
              if (i > 0) const SizedBox(width: 6),
              _DayChip(
                label: i < weekdays.length ? weekdays[i] : '$i',
                selected: (_days & (1 << i)) != 0,
                enabled: _scheduleEnabled,
                c: c,
                m: m,
                onTap: () => setState(() => _days ^= 1 << i),
              ),
            ],
          ],
        ),
        // 实时语义摘要：把两个时刻的组合直接翻译成「会发生什么」，
        // 澄清「只填其一」的合法用法。仅在启用且有生效时刻时显示。
        if (_scheduleEnabled) ...[
          const SizedBox(height: 12),
          Builder(
            builder: (_) {
              final start = _startCtrl.text.trim();
              final stop = _stopCtrl.text.trim();
              final startOk = start.isNotEmpty && _validHhmm(start);
              final stopOk = stop.isNotEmpty && _validHhmm(stop);
              final summary = startOk && stopOk
                  ? s.scheduleSummaryBoth(start, stop)
                  : startOk
                  ? s.scheduleSummaryStartOnly(start)
                  : stopOk
                  ? s.scheduleSummaryStopOnly(stop)
                  : s.scheduleNeedOneTime;
              final warn = !startOk && !stopOk;
              return Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(
                    warn ? LucideIcons.circleAlert : LucideIcons.info,
                    size: 13,
                    color: warn ? AppColors.amber : c.textSecondary,
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      summary,
                      style: TextStyle(
                        fontSize: 11.5,
                        color: warn ? AppColors.amber : c.textSecondary,
                      ),
                    ),
                  ),
                ],
              );
            },
          ),
        ],
        if (_scheduleError.isNotEmpty) ...[
          const SizedBox(height: 10),
          Text(
            _scheduleError,
            style: const TextStyle(fontSize: 11.5, color: AppColors.red),
          ),
        ],
      ],
    );
  }

  // ─────────────────────────────────────────────
  // 任务顺序
  // ─────────────────────────────────────────────

  /// 与引擎恢复顺序一致：queue_order 升序，0（未显式排序）在前按创建时间。
  static int _compareQueueOrder(DownloadTask a, DownloadTask b) {
    if (a.queueOrder != b.queueOrder) {
      return a.queueOrder.compareTo(b.queueOrder);
    }
    final byTime = a.createdAt.compareTo(b.createdAt);
    return byTime != 0 ? byTime : a.id.compareTo(b.id);
  }

  Widget _buildTasksTab(S s, AppColors c, AppMetrics m, DownloadQueue q) {
    final tasks =
        widget.controller.tasks
            .where(
              (t) =>
                  t.queueId == q.queueId && t.status != TaskStatus.completed,
            )
            .toList()
          ..sort(_compareQueueOrder);

    if (tasks.isEmpty) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 28),
        child: Center(
          child: Text(
            s.queueNoPendingTasks,
            style: TextStyle(fontSize: 12, color: c.textMuted),
          ),
        ),
      );
    }

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          s.queueTasksOrderHint,
          style: TextStyle(fontSize: 11, color: c.textMuted),
        ),
        const SizedBox(height: 8),
        ConstrainedBox(
          constraints: const BoxConstraints(maxHeight: 280),
          child: ListView.builder(
            shrinkWrap: true,
            itemCount: tasks.length,
            itemBuilder: (ctx, i) => _TaskOrderRow(
              index: i,
              task: tasks[i],
              c: c,
              m: m,
              s: s,
              canMoveUp: i > 0,
              canMoveDown: i < tasks.length - 1,
              onMoveUp: () => _moveTask(tasks, i, -1),
              onMoveDown: () => _moveTask(tasks, i, 1),
            ),
          ),
        ),
      ],
    );
  }

  void _moveTask(List<DownloadTask> sorted, int index, int delta) {
    final target = index + delta;
    if (target < 0 || target >= sorted.length) return;
    final ids = sorted.map((t) => t.id).toList();
    final moved = ids.removeAt(index);
    ids.insert(target, moved);
    // 提交完整新顺序（1..N 落库）；控制器同步做乐观更新触发本列表重排。
    widget.controller.reorderQueueTasks(widget.queueId, ids);
  }
}

/// 运行状态徽标（圆点 + 文本）。
class _RunBadge extends StatelessWidget {
  final bool running;
  final S s;
  final AppColors c;
  final AppMetrics m;

  const _RunBadge({
    required this.running,
    required this.s,
    required this.c,
    required this.m,
  });

  @override
  Widget build(BuildContext context) {
    final color = running ? AppColors.green : c.textMuted;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(color: m.soft(color), borderRadius: m.brSm),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Container(
            width: 6,
            height: 6,
            decoration: BoxDecoration(color: color, shape: BoxShape.circle),
          ),
          const SizedBox(width: 5),
          Text(
            running ? s.queueRunningBadge : s.queueStoppedBadge,
            style: TextStyle(
              fontSize: 11,
              fontWeight: FontWeight.w500,
              color: color,
            ),
          ),
        ],
      ),
    );
  }
}

/// 星期选择小圆片。
class _DayChip extends StatelessWidget {
  final String label;
  final bool selected;
  final bool enabled;
  final AppColors c;
  final AppMetrics m;
  final VoidCallback onTap;

  const _DayChip({
    required this.label,
    required this.selected,
    required this.enabled,
    required this.c,
    required this.m,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final active = selected && enabled;
    return GestureDetector(
      onTap: enabled ? onTap : null,
      child: MouseRegion(
        cursor: enabled
            ? SystemMouseCursors.click
            : SystemMouseCursors.basic,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          width: 32,
          height: 26,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: active ? c.accentBg : c.surface1,
            borderRadius: m.brSm,
            border: Border.all(
              color: active ? c.accent : c.border,
              width: 1,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              fontSize: 11,
              fontWeight: active ? FontWeight.w600 : FontWeight.normal,
              color: !enabled
                  ? m.disabled(c.textMuted)
                  : active
                  ? c.accent
                  : c.textSecondary,
            ),
          ),
        ),
      ),
    );
  }
}

/// 任务顺序行：序号 + 状态点 + 文件名 + 上移/下移。
class _TaskOrderRow extends StatelessWidget {
  final int index;
  final DownloadTask task;
  final AppColors c;
  final AppMetrics m;
  final S s;
  final bool canMoveUp;
  final bool canMoveDown;
  final VoidCallback onMoveUp;
  final VoidCallback onMoveDown;

  const _TaskOrderRow({
    required this.index,
    required this.task,
    required this.c,
    required this.m,
    required this.s,
    required this.canMoveUp,
    required this.canMoveDown,
    required this.onMoveUp,
    required this.onMoveDown,
  });

  Color get _statusColor => switch (task.status) {
    TaskStatus.downloading || TaskStatus.resuming => AppColors.green,
    TaskStatus.pending || TaskStatus.preparing => AppColors.amber,
    TaskStatus.error => AppColors.red,
    _ => c.textMuted,
  };

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 34,
      margin: const EdgeInsets.only(bottom: 2),
      padding: const EdgeInsets.symmetric(horizontal: 8),
      decoration: BoxDecoration(color: c.surface1, borderRadius: m.brMd),
      child: Row(
        children: [
          SizedBox(
            width: 22,
            child: Text(
              '${index + 1}',
              style: TextStyle(
                fontSize: 11,
                color: c.textMuted,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
          Container(
            width: 6,
            height: 6,
            decoration: BoxDecoration(
              color: _statusColor,
              shape: BoxShape.circle,
            ),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              task.fileName,
              style: TextStyle(fontSize: 12, color: c.textPrimary),
              overflow: TextOverflow.ellipsis,
              maxLines: 1,
            ),
          ),
          const SizedBox(width: 6),
          _OrderIconButton(
            icon: LucideIcons.chevronUp,
            tooltip: s.moveUpAction,
            enabled: canMoveUp,
            c: c,
            m: m,
            onTap: onMoveUp,
          ),
          const SizedBox(width: 2),
          _OrderIconButton(
            icon: LucideIcons.chevronDown,
            tooltip: s.moveDownAction,
            enabled: canMoveDown,
            c: c,
            m: m,
            onTap: onMoveDown,
          ),
        ],
      ),
    );
  }
}

class _OrderIconButton extends StatefulWidget {
  final IconData icon;
  final String tooltip;
  final bool enabled;
  final AppColors c;
  final AppMetrics m;
  final VoidCallback onTap;

  const _OrderIconButton({
    required this.icon,
    required this.tooltip,
    required this.enabled,
    required this.c,
    required this.m,
    required this.onTap,
  });

  @override
  State<_OrderIconButton> createState() => _OrderIconButtonState();
}

class _OrderIconButtonState extends State<_OrderIconButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    final m = widget.m;
    return ShadTooltip(
      builder: (_) => Text(widget.tooltip),
      child: MouseRegion(
        cursor: widget.enabled
            ? SystemMouseCursors.click
            : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          onTap: widget.enabled ? widget.onTap : null,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 120),
            width: 20,
            height: 20,
            decoration: BoxDecoration(
              color: _hovered && widget.enabled
                  ? c.hoverBg
                  : Colors.transparent,
              borderRadius: m.brSm,
            ),
            child: Icon(
              widget.icon,
              size: 13,
              color: widget.enabled
                  ? c.textSecondary
                  : m.disabled(c.textMuted),
            ),
          ),
        ),
      ),
    );
  }
}

/// 「移动到队列」对话框中的单个目标队列行。
class _MoveTargetRow extends StatefulWidget {
  final DownloadQueue queue;
  final bool isCurrent;
  final AppColors c;
  final AppMetrics m;
  final S s;
  final VoidCallback onTap;

  const _MoveTargetRow({
    required this.queue,
    required this.isCurrent,
    required this.c,
    required this.m,
    required this.s,
    required this.onTap,
  });

  @override
  State<_MoveTargetRow> createState() => _MoveTargetRowState();
}

class _MoveTargetRowState extends State<_MoveTargetRow> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    final m = widget.m;
    final q = widget.queue;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          height: 34,
          margin: const EdgeInsets.only(bottom: 2),
          padding: const EdgeInsets.symmetric(horizontal: 10),
          decoration: BoxDecoration(
            color: widget.isCurrent
                ? c.accentBg
                : _hovered
                ? c.hoverBg
                : Colors.transparent,
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
                  queueDisplayName(widget.s, q),
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

/// 每日定时的时刻选择器：字段本身只显示 `HH:MM`（空 = 未设置，该边沿
/// 不定时），点击弹出网格面板——小时 0-23 全展开（6×4）、分钟 5 分钟
/// 步进全展开（6×2），无滚动、一眼选中；带清除回到空态。
///
/// 分钟按 5 分钟粒度：队列定时的实际用例都是整/半点，去掉 60 项滚动
/// 换来「不滚动一眼选」的精致度。
class _TimePicker extends StatefulWidget {
  final String value;
  final bool enabled;
  final AppColors c;
  final String hint;
  final ValueChanged<String> onChanged;

  const _TimePicker({
    required this.value,
    required this.enabled,
    required this.c,
    required this.hint,
    required this.onChanged,
  });

  @override
  State<_TimePicker> createState() => _TimePickerState();
}

class _TimePickerState extends State<_TimePicker> {
  bool _hovered = false;

  (int?, int?) get _parts {
    final m = RegExp(r'^(\d{1,2}):(\d{2})$').firstMatch(widget.value);
    if (m == null) return (null, null);
    return (int.parse(m.group(1)!), int.parse(m.group(2)!));
  }

  static String _two(int n) => n.toString().padLeft(2, '0');

  void _openPanel() {
    final box = context.findRenderObject();
    if (box is! RenderBox || !box.hasSize) return;
    final origin = box.localToGlobal(Offset(0, box.size.height + 6));
    final overlay = Overlay.of(context);
    final (h, min) = _parts;
    late OverlayEntry entry;
    entry = OverlayEntry(
      builder: (_) => _TimeGridPanel(
        left: origin.dx,
        top: origin.dy,
        width: box.size.width,
        c: widget.c,
        hourLabel: LocaleScope.of(context).scheduleHourLabel,
        minuteLabel: LocaleScope.of(context).scheduleMinuteLabel,
        hour: h,
        minute: min,
        onPick: (hh, mm) {
          widget.onChanged('${_two(hh)}:${_two(mm)}');
        },
        onDismiss: () => entry.remove(),
      ),
    );
    overlay.insert(entry);
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    final m = AppMetrics.of(context);
    final (h, min) = _parts;
    final hasValue = h != null && min != null;
    final display = hasValue ? '${_two(h)}:${_two(min)}' : widget.hint;

    return Opacity(
      opacity: widget.enabled ? 1 : 0.5,
      child: MouseRegion(
        cursor: widget.enabled
            ? SystemMouseCursors.click
            : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          onTap: widget.enabled ? _openPanel : null,
          child: Container(
            height: 36,
            padding: const EdgeInsets.only(left: 12, right: 6),
            decoration: BoxDecoration(
              color: c.surface1,
              borderRadius: m.brMd,
              border: Border.all(
                color: _hovered && widget.enabled ? c.accent : c.border,
                width: 1,
              ),
            ),
            child: Row(
              children: [
                Icon(LucideIcons.clock3, size: 13, color: c.textMuted),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    display,
                    style: TextStyle(
                      fontSize: 13,
                      color: hasValue ? c.textPrimary : c.textMuted,
                    ),
                  ),
                ),
                if (hasValue && widget.enabled)
                  _ClearButton(c: c, onTap: () => widget.onChanged(''))
                else
                  Icon(LucideIcons.chevronDown, size: 13, color: c.textMuted),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// 时刻网格弹出面板：小时/分钟两片全展开网格，选中即回调并关闭。
class _TimeGridPanel extends StatefulWidget {
  final double left;
  final double top;
  final double width;
  final AppColors c;
  final String hourLabel;
  final String minuteLabel;
  final int? hour;
  final int? minute;
  final void Function(int hour, int minute) onPick;
  final VoidCallback onDismiss;

  const _TimeGridPanel({
    required this.left,
    required this.top,
    required this.width,
    required this.c,
    required this.hourLabel,
    required this.minuteLabel,
    required this.hour,
    required this.minute,
    required this.onPick,
    required this.onDismiss,
  });

  @override
  State<_TimeGridPanel> createState() => _TimeGridPanelState();
}

class _TimeGridPanelState extends State<_TimeGridPanel> {
  late int? _h = widget.hour;
  late int? _m = widget.minute;

  static String _two(int n) => n.toString().padLeft(2, '0');

  /// 确定：把暂存选择（未选维缺省 0）回填字段并关闭。取消/点面板外
  /// 直接关闭、不改动字段——暂存态随面板销毁丢弃。
  void _confirm() {
    widget.onPick(_h ?? 0, _m ?? 0);
    widget.onDismiss();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    final m = AppMetrics.of(context);
    final screen = MediaQuery.of(context).size;
    // 左右布局：小时列 4 格宽、分钟列 3 格宽 + 分隔线 + 内边距。
    // 单元格 32 + 间距 4：小时列 4*32+3*4=140，分钟列 3*32+2*4=104。
    const hourColWidth = 140.0;
    const minuteColWidth = 104.0;
    const panelWidth = hourColWidth + minuteColWidth + 1 + 20 + 24; // +分隔+padding+间隔
    var left = widget.left;
    if (left + panelWidth > screen.width) left = screen.width - panelWidth - 8;
    var top = widget.top;
    // 面板高度约 236（网格 + 按钮行）；下方空间不足时上翻。
    if (top + 236 > screen.height) top = (widget.top - 236 - 44).clamp(8, top);

    return Stack(
      children: [
        Positioned.fill(
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: widget.onDismiss,
            onSecondaryTap: widget.onDismiss,
            child: const ColoredBox(color: Color(0x00000000)),
          ),
        ),
        Positioned(
          left: left,
          top: top,
          width: panelWidth,
          child: Container(
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              color: c.surface1,
              borderRadius: m.brCard,
              border: Border.all(color: c.border, width: 1),
              boxShadow: [
                BoxShadow(
                  color: m.muted(const Color(0xFF000000)),
                  blurRadius: 12,
                  offset: const Offset(0, 4),
                ),
              ],
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    // 小时列
                    SizedBox(
                      width: hourColWidth,
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          _sectionLabel(widget.hourLabel, c),
                          const SizedBox(height: 6),
                          _grid(
                            count: 24,
                            selected: _h,
                            c: c,
                            m: m,
                            onTap: (v) => setState(() => _h = v),
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: 10),
                    Container(width: 1, color: c.border),
                    const SizedBox(width: 10),
                    // 分钟列
                    SizedBox(
                      width: minuteColWidth,
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          _sectionLabel(widget.minuteLabel, c),
                          const SizedBox(height: 6),
                          _grid(
                            count: 12,
                            step: 5,
                            selected: _m,
                            c: c,
                            m: m,
                            onTap: (v) => setState(() => _m = v),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 10),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    ShadButton.outline(
                      size: ShadButtonSize.sm,
                      onPressed: widget.onDismiss,
                      child: Text(LocaleScope.of(context).cancel),
                    ),
                    const SizedBox(width: 8),
                    ShadButton(
                      size: ShadButtonSize.sm,
                      onPressed: _confirm,
                      child: Text(LocaleScope.of(context).confirm),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }

  Widget _sectionLabel(String text, AppColors c) => Text(
    text,
    style: TextStyle(
      fontSize: 11,
      fontWeight: FontWeight.w500,
      color: c.textMuted,
    ),
  );

  Widget _grid({
    required int count,
    required int? selected,
    required AppColors c,
    required AppMetrics m,
    required ValueChanged<int> onTap,
    int step = 1,
  }) {
    return Wrap(
      spacing: 4,
      runSpacing: 4,
      children: [
        for (var i = 0; i < count; i++)
          _GridCell(
            label: _two(i * step),
            selected: selected == i * step,
            c: c,
            m: m,
            onTap: () => onTap(i * step),
          ),
      ],
    );
  }
}

/// 网格单元格：选中 accent 实心，未选 hover 高亮。
class _GridCell extends StatefulWidget {
  final String label;
  final bool selected;
  final AppColors c;
  final AppMetrics m;
  final VoidCallback onTap;

  const _GridCell({
    required this.label,
    required this.selected,
    required this.c,
    required this.m,
    required this.onTap,
  });

  @override
  State<_GridCell> createState() => _GridCellState();
}

class _GridCellState extends State<_GridCell> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.c;
    // 6 列、单元格间距 4、容器内边距 10*2、面板宽 232 → 单元格约 32。
    const cell = 32.0;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          width: cell,
          height: 26,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: widget.selected
                ? c.accent
                : _hovered
                ? c.hoverBg
                : Colors.transparent,
            borderRadius: widget.m.brSm,
          ),
          child: Text(
            widget.label,
            style: TextStyle(
              fontSize: 12,
              fontWeight: widget.selected ? FontWeight.w600 : FontWeight.normal,
              color: widget.selected ? Colors.white : c.textSecondary,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
          ),
        ),
      ),
    );
  }
}

class _ClearButton extends StatefulWidget {
  final AppColors c;
  final VoidCallback onTap;

  const _ClearButton({required this.c, required this.onTap});

  @override
  State<_ClearButton> createState() => _ClearButtonState();
}

class _ClearButtonState extends State<_ClearButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: Padding(
          padding: const EdgeInsets.only(left: 6),
          child: Container(
            width: 22,
            height: 22,
            decoration: BoxDecoration(
              color: _hovered ? widget.c.hoverBg : Colors.transparent,
              borderRadius: m.brSm,
            ),
            child: Icon(LucideIcons.x, size: 13, color: widget.c.textMuted),
          ),
        ),
      ),
    );
  }
}
