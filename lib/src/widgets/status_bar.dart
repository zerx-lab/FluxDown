import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../models/settings_provider.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import '../services/shutdown_service.dart';
import 'feedback_dialog.dart';

// 预设限速值（label 显示用，kbs 为 KB/s）
const _kPresets = [
  (label: '128 KB/s', kbs: 128),
  (label: '512 KB/s', kbs: 512),
  (label: '1 MB/s', kbs: 1024),
  (label: '2 MB/s', kbs: 2048),
  (label: '5 MB/s', kbs: 5120),
];

// 预设关机延迟（分钟；0 = 完成后立即关机）
const _kShutdownPresets = [0, 1, 5, 10, 30];

/// 将字节/秒格式化为可读速率字符串，整数不显示小数
String _formatSpeed(int bytes) {
  if (bytes >= 1024 * 1024) {
    final mb = bytes / (1024 * 1024);
    final rounded = mb.round();
    return rounded == mb ? '$rounded MB/s' : '${mb.toStringAsFixed(1)} MB/s';
  }
  return '${(bytes / 1024).round()} KB/s';
}

class StatusBar extends StatefulWidget {
  final DownloadController controller;
  final SettingsProvider settingsProvider;

  const StatusBar({
    super.key,
    required this.controller,
    required this.settingsProvider,
  });

  @override
  State<StatusBar> createState() => _StatusBarState();
}

class _StatusBarState extends State<StatusBar> {
  final _popoverController = ShadPopoverController();
  final _customController = TextEditingController();
  final _shutdownPopoverController = ShadPopoverController();
  final _shutdownMinutesController = TextEditingController();

  /// 上次已写入 settings 的字节数，用于防循环更新
  int _lastKnownBytes = -1;

  @override
  void initState() {
    super.initState();
    final bytes = widget.settingsProvider.speedLimitBytes;
    _lastKnownBytes = bytes;
    _customController.text = _kbsText(bytes);
    _shutdownMinutesController.text =
        ShutdownService.instance.delayMinutes.toString();
    widget.settingsProvider.addListener(_onSettingsChanged);
    _popoverController.addListener(_onPopoverChanged);
    _shutdownPopoverController.addListener(_onShutdownPopoverChanged);
  }

  @override
  void dispose() {
    _popoverController.removeListener(_onPopoverChanged);
    _shutdownPopoverController.removeListener(_onShutdownPopoverChanged);
    widget.settingsProvider.removeListener(_onSettingsChanged);
    _popoverController.dispose();
    _customController.dispose();
    _shutdownPopoverController.dispose();
    _shutdownMinutesController.dispose();
    super.dispose();
  }

  /// 将 bytes/s 转换为输入框文本（0 → 空字符串）
  String _kbsText(int bytes) {
    if (bytes <= 0) return '';
    return (bytes / 1024).round().toString();
  }

  /// 设置页（外部）修改限速时同步输入框
  void _onSettingsChanged() {
    final newBytes = widget.settingsProvider.speedLimitBytes;
    if (newBytes == _lastKnownBytes) return;
    _lastKnownBytes = newBytes;
    _customController.text = _kbsText(newBytes);
    if (mounted) setState(() {});
  }

  /// Popover 关闭时，若已开启限速，则将自定义输入框的当前值写入设置
  void _onPopoverChanged() {
    if (!_popoverController.isOpen) {
      _applyCustomInput();
    }
  }

  bool get _isLimited => widget.settingsProvider.speedLimitBytes > 0;

  /// 切换开关
  void _toggleLimit(bool on) {
    if (on) {
      final kbs = int.tryParse(_customController.text.trim()) ?? 0;
      final effectiveKbs = kbs > 0 ? kbs : 512;
      if (kbs <= 0) _customController.text = '512';
      final bytes = effectiveKbs * 1024;
      _lastKnownBytes = bytes;
      widget.settingsProvider.setSpeedLimitBytes(bytes);
    } else {
      _lastKnownBytes = 0;
      widget.settingsProvider.setSpeedLimitBytes(0);
    }
  }

  /// 点击预设：直接启用并应用该速率
  void _applyPreset(int kbs) {
    _customController.text = kbs.toString();
    final bytes = kbs * 1024;
    _lastKnownBytes = bytes;
    widget.settingsProvider.setSpeedLimitBytes(bytes);
  }

  /// 自定义输入框的值写入设置（仅限速已开启时有效）
  void _applyCustomInput() {
    if (!_isLimited) return;
    final kbs = int.tryParse(_customController.text.trim()) ?? 0;
    if (kbs > 0) {
      final bytes = kbs * 1024;
      if (bytes != _lastKnownBytes) {
        _lastKnownBytes = bytes;
        widget.settingsProvider.setSpeedLimitBytes(bytes);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // 完成后关机
  // ---------------------------------------------------------------------------

  /// Popover 关闭时，若已开启关机，则应用自定义分钟输入
  void _onShutdownPopoverChanged() {
    if (!_shutdownPopoverController.isOpen) {
      _applyShutdownMinutesInput();
    }
  }

  /// 切换「完成后关机」开关
  void _toggleShutdown(bool on) {
    final svc = ShutdownService.instance;
    if (on) {
      // 空/非法输入 → 保持服务当前延迟；"0" = 立即关机
      final minutes = int.tryParse(_shutdownMinutesController.text.trim());
      final armed = svc.arm(minutes: minutes);
      if (armed) {
        _shutdownMinutesController.text = svc.delayMinutes.toString();
      }
    } else {
      svc.cancel();
    }
  }

  /// 点击预设分钟：设置延迟并（可开启时）直接开启
  void _applyShutdownPreset(int minutes) {
    final svc = ShutdownService.instance;
    _shutdownMinutesController.text = minutes.toString();
    if (svc.isArmed) {
      svc.setDelayMinutes(minutes);
    } else {
      svc.arm(minutes: minutes);
    }
  }

  /// 自定义分钟输入写入服务（仅已开启时有效；0 = 立即关机）
  void _applyShutdownMinutesInput() {
    final svc = ShutdownService.instance;
    final minutes = int.tryParse(_shutdownMinutesController.text.trim());
    if (minutes != null && svc.isArmed) {
      svc.setDelayMinutes(minutes);
      _shutdownMinutesController.text = svc.delayMinutes.toString();
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([
        widget.controller,
        widget.settingsProvider,
        ShutdownService.instance,
      ]),
      builder: (context, _) {
        final dlSpeed = DownloadTask.formatBytes(
          widget.controller.totalDownloadSpeed,
        );
        final active = widget.controller.activeCount;
        final paused = widget.controller.pausedCount;
        final total = widget.controller.tasks.length;

        return Container(
          height: 28,
          padding: const EdgeInsets.symmetric(horizontal: 16),
          decoration: BoxDecoration(
            color: c.surface1,
            border: Border(top: BorderSide(color: c.border, width: 1)),
          ),
          child: Row(
            children: [
              // 状态指示
              Row(
                children: [
                  Icon(
                    LucideIcons.circle,
                    size: 8,
                    color: active > 0 ? AppColors.green : c.textMuted,
                  ),
                  const SizedBox(width: 6),
                  Text(
                    active > 0 ? s.statusDownloadingLabel : s.statusIdle,
                    style: TextStyle(fontSize: 10.5, color: c.textMuted),
                  ),
                ],
              ),
              const SizedBox(width: 20),
              // 实时下载速度
              Row(
                children: [
                  const Icon(
                    LucideIcons.arrowDown,
                    size: 10,
                    color: AppColors.green,
                  ),
                  const SizedBox(width: 4),
                  Text(
                    '$dlSpeed/s',
                    style: TextStyle(
                      fontSize: 10.5,
                      color: c.textMuted,
                      fontFeatures: const [FontFeature.tabularFigures()],
                    ),
                  ),
                ],
              ),
              const SizedBox(width: 20),
              Text(
                s.statusSummary(active, paused, total),
                style: TextStyle(fontSize: 10.5, color: c.textMuted),
              ),
              const Spacer(),
              // 限速 Popover 触发器
              _SpeedLimitTrigger(
                popoverController: _popoverController,
                settingsProvider: widget.settingsProvider,
                customController: _customController,
                isLimited: _isLimited,
                limitBytes: widget.settingsProvider.speedLimitBytes,
                onToggle: _toggleLimit,
                onApplyPreset: _applyPreset,
                onApplyCustom: _applyCustomInput,
                s: s,
                c: c,
              ),
              const SizedBox(width: 12),
              Container(width: 1, height: 12, color: c.border),
              const SizedBox(width: 12),
              // 完成后关机 Popover 触发器
              _ShutdownTrigger(
                popoverController: _shutdownPopoverController,
                controller: widget.controller,
                minutesController: _shutdownMinutesController,
                onToggle: _toggleShutdown,
                onApplyPreset: _applyShutdownPreset,
                onApplyCustom: _applyShutdownMinutesInput,
                s: s,
                c: c,
              ),
              const SizedBox(width: 12),
              Container(width: 1, height: 12, color: c.border),
              const SizedBox(width: 12),
              // 反馈按钮
              GestureDetector(
                onTap: () => showFeedbackDialog(context),
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: Row(
                    children: [
                      Icon(
                        LucideIcons.messageSquarePlus,
                        size: 11,
                        color: c.textMuted,
                      ),
                      const SizedBox(width: 4),
                      Text(
                        s.feedback,
                        style: TextStyle(fontSize: 10.5, color: c.textMuted),
                      ),
                    ],
                  ),
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

// =============================================================================
// 触发器 Widget — 显示当前限速状态，点击展开/收起 Popover
// =============================================================================

class _SpeedLimitTrigger extends StatelessWidget {
  final ShadPopoverController popoverController;
  final SettingsProvider settingsProvider;
  final TextEditingController customController;
  final bool isLimited;
  final int limitBytes;
  final ValueChanged<bool> onToggle;
  final ValueChanged<int> onApplyPreset;
  final VoidCallback onApplyCustom;
  final S s;
  final AppColors c;

  const _SpeedLimitTrigger({
    required this.popoverController,
    required this.settingsProvider,
    required this.customController,
    required this.isLimited,
    required this.limitBytes,
    required this.onToggle,
    required this.onApplyPreset,
    required this.onApplyCustom,
    required this.s,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final triggerColor = isLimited ? c.accent : c.textMuted;
    final triggerText =
        isLimited ? _formatSpeed(limitBytes) : s.statusSpeedLimitOff;

    return ShadPopover(
      controller: popoverController,
      // 弹出在触发器上方，右对齐（状态栏位于屏幕底部）
      anchor: const ShadAnchorAuto(
        offset: Offset(0, -8),
        followerAnchor: Alignment.bottomRight,
        targetAnchor: Alignment.topRight,
      ),
      padding: EdgeInsets.zero,
      // 使用 ListenableBuilder 确保 Popover 内容在设置变更后自动刷新
      popover: (ctx) => ListenableBuilder(
        listenable: settingsProvider,
        builder: (ctx2, _) => _SpeedLimitPopoverContent(
          customController: customController,
          isLimited: settingsProvider.speedLimitBytes > 0,
          limitBytes: settingsProvider.speedLimitBytes,
          onToggle: onToggle,
          onApplyPreset: onApplyPreset,
          onApplyCustom: onApplyCustom,
          s: s,
          c: c,
        ),
      ),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: popoverController.toggle,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(LucideIcons.gauge, size: 11, color: triggerColor),
              const SizedBox(width: 4),
              Text(
                triggerText,
                style: TextStyle(
                  fontSize: 10.5,
                  color: triggerColor,
                  fontFeatures: const [FontFeature.tabularFigures()],
                ),
              ),
              const SizedBox(width: 2),
              Icon(LucideIcons.chevronUp, size: 9, color: triggerColor),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// Popover 内容 — 开关 + 预设速率 + 自定义输入
// =============================================================================

class _SpeedLimitPopoverContent extends StatelessWidget {
  final TextEditingController customController;
  final bool isLimited;
  final int limitBytes;
  final ValueChanged<bool> onToggle;
  final ValueChanged<int> onApplyPreset;
  final VoidCallback onApplyCustom;
  final S s;
  final AppColors c;

  const _SpeedLimitPopoverContent({
    required this.customController,
    required this.isLimited,
    required this.limitBytes,
    required this.onToggle,
    required this.onApplyPreset,
    required this.onApplyCustom,
    required this.s,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return SizedBox(
      width: 220,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          // 标题行 + 开关
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 12, 8, 10),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    s.speedLimitTitle,
                    style: TextStyle(
                      fontSize: 12.5,
                      fontWeight: FontWeight.w600,
                      color: c.textPrimary,
                    ),
                  ),
                ),
                ShadSwitch(
                  value: isLimited,
                  onChanged: onToggle,
                  width: 34,
                  height: 18,
                  margin: 2,
                ),
              ],
            ),
          ),
          // 预设速率 chips
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
            child: Wrap(
              spacing: 5,
              runSpacing: 5,
              children: _kPresets.map((preset) {
                final isSelected = isLimited && limitBytes == preset.kbs * 1024;
                return MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: GestureDetector(
                    onTap: () => onApplyPreset(preset.kbs),
                    child: AnimatedContainer(
                      duration: const Duration(milliseconds: 120),
                      padding: const EdgeInsets.symmetric(
                        horizontal: 8,
                        vertical: 4,
                      ),
                      decoration: BoxDecoration(
                        color: isSelected ? c.accent : c.surface2,
                        borderRadius: m.brSm,
                        border: Border.all(
                          color: isSelected ? c.accent : c.border,
                          width: 0.5,
                        ),
                      ),
                      child: Text(
                        preset.label,
                        style: TextStyle(
                          fontSize: 11,
                          color: isSelected
                              ? const Color(0xFFFFFFFF)
                              : c.textSecondary,
                          fontFeatures: const [FontFeature.tabularFigures()],
                        ),
                      ),
                    ),
                  ),
                );
              }).toList(),
            ),
          ),
          // 分割线
          Divider(color: c.border, height: 1),
          // 自定义输入
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 10, 12, 12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  s.speedLimitCustom,
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
                const SizedBox(height: 6),
                Row(
                  children: [
                    Expanded(
                      child: ShadInput(
                        controller: customController,
                        keyboardType: TextInputType.number,
                        inputFormatters: [
                          FilteringTextInputFormatter.digitsOnly,
                        ],
                        placeholder: Text(s.statusSpeedLimitHint),
                        onSubmitted: (_) => onApplyCustom(),
                      ),
                    ),
                    const SizedBox(width: 6),
                    Text(
                      s.statusSpeedLimitKbs,
                      style: TextStyle(fontSize: 12, color: c.textMuted),
                    ),
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

// =============================================================================
// 完成后关机 — 触发器 Widget
// =============================================================================

class _ShutdownTrigger extends StatelessWidget {
  final ShadPopoverController popoverController;
  final DownloadController controller;
  final TextEditingController minutesController;
  final ValueChanged<bool> onToggle;
  final ValueChanged<int> onApplyPreset;
  final VoidCallback onApplyCustom;
  final S s;
  final AppColors c;

  const _ShutdownTrigger({
    required this.popoverController,
    required this.controller,
    required this.minutesController,
    required this.onToggle,
    required this.onApplyPreset,
    required this.onApplyCustom,
    required this.s,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final svc = ShutdownService.instance;
    final Color triggerColor;
    final String triggerText;
    if (svc.isCountingDown) {
      triggerColor = c.statusWarning;
      triggerText = s.shutdownCountdown(svc.remainingText);
    } else if (svc.isArmed) {
      triggerColor = c.accent;
      triggerText = s.shutdownTriggerLabel;
    } else {
      triggerColor = c.textMuted;
      triggerText = s.shutdownTriggerLabel;
    }

    return ShadPopover(
      controller: popoverController,
      anchor: const ShadAnchorAuto(
        offset: Offset(0, -8),
        followerAnchor: Alignment.bottomRight,
        targetAnchor: Alignment.topRight,
      ),
      padding: EdgeInsets.zero,
      // 监听服务与控制器 —— 倒计时秒数刷新、活跃任务数变化时开关可用性刷新
      popover: (ctx) => ListenableBuilder(
        listenable: Listenable.merge([svc, controller]),
        builder: (ctx2, _) => _ShutdownPopoverContent(
          minutesController: minutesController,
          onToggle: onToggle,
          onApplyPreset: onApplyPreset,
          onApplyCustom: onApplyCustom,
          s: s,
          c: c,
        ),
      ),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: popoverController.toggle,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(LucideIcons.power, size: 11, color: triggerColor),
              const SizedBox(width: 4),
              Text(
                triggerText,
                style: TextStyle(
                  fontSize: 10.5,
                  color: triggerColor,
                  fontFeatures: const [FontFeature.tabularFigures()],
                ),
              ),
              const SizedBox(width: 2),
              Icon(LucideIcons.chevronUp, size: 9, color: triggerColor),
            ],
          ),
        ),
      ),
    );
  }
}

// =============================================================================
// 完成后关机 — Popover 内容：开关 + 预设延迟 + 自定义分钟 + 倒计时/取消
// =============================================================================

class _ShutdownPopoverContent extends StatelessWidget {
  final TextEditingController minutesController;
  final ValueChanged<bool> onToggle;
  final ValueChanged<int> onApplyPreset;
  final VoidCallback onApplyCustom;
  final S s;
  final AppColors c;

  const _ShutdownPopoverContent({
    required this.minutesController,
    required this.onToggle,
    required this.onApplyPreset,
    required this.onApplyCustom,
    required this.s,
    required this.c,
  });

  @override
  Widget build(BuildContext context) {
    final svc = ShutdownService.instance;
    final canInteract = svc.canArm || svc.isArmed;
    final m = AppMetrics.of(context);

    return SizedBox(
      width: 240,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          // 标题行 + 开关
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 12, 8, 10),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    s.shutdownTitle,
                    style: TextStyle(
                      fontSize: 12.5,
                      fontWeight: FontWeight.w600,
                      color: c.textPrimary,
                    ),
                  ),
                ),
                ShadSwitch(
                  value: svc.isArmed,
                  onChanged: canInteract ? onToggle : null,
                  width: 34,
                  height: 18,
                  margin: 2,
                ),
              ],
            ),
          ),
          // 无活跃任务提示
          if (!canInteract)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
              child: Text(
                s.shutdownNeedActiveTask,
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
            ),
          // 倒计时状态 + 取消按钮
          if (svc.isCountingDown) ...[
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
              child: Row(
                children: [
                  Icon(LucideIcons.timer, size: 12, color: c.statusWarning),
                  const SizedBox(width: 5),
                  Expanded(
                    child: Text(
                      s.shutdownCountdown(svc.remainingText),
                      style: TextStyle(
                        fontSize: 11.5,
                        fontWeight: FontWeight.w600,
                        color: c.statusWarning,
                        fontFeatures: const [FontFeature.tabularFigures()],
                      ),
                    ),
                  ),
                  ShadButton.destructive(
                    height: 24,
                    padding: const EdgeInsets.symmetric(horizontal: 10),
                    onPressed: svc.cancel,
                    child: Text(
                      s.shutdownCancelButton,
                      style: const TextStyle(fontSize: 11),
                    ),
                  ),
                ],
              ),
            ),
          ] else if (svc.isArmed)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
              child: Text(
                svc.delayMinutes == 0
                    ? s.shutdownArmedHintImmediate
                    : s.shutdownArmedHint(svc.delayMinutes),
                style: TextStyle(fontSize: 11, color: c.textMuted),
              ),
            ),
          // 预设延迟 chips
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
            child: Wrap(
              spacing: 5,
              runSpacing: 5,
              children: _kShutdownPresets.map((minutes) {
                final isSelected =
                    svc.isArmed && svc.delayMinutes == minutes;
                return MouseRegion(
                  cursor: canInteract
                      ? SystemMouseCursors.click
                      : SystemMouseCursors.basic,
                  child: GestureDetector(
                    onTap: canInteract ? () => onApplyPreset(minutes) : null,
                    child: AnimatedContainer(
                      duration: const Duration(milliseconds: 120),
                      padding: const EdgeInsets.symmetric(
                        horizontal: 8,
                        vertical: 4,
                      ),
                      decoration: BoxDecoration(
                        color: isSelected ? c.accent : c.surface2,
                        borderRadius: m.brSm,
                        border: Border.all(
                          color: isSelected ? c.accent : c.border,
                          width: 0.5,
                        ),
                      ),
                      child: Text(
                        minutes == 0
                            ? s.shutdownImmediate
                            : s.shutdownDelayMinutes(minutes),
                        style: TextStyle(
                          fontSize: 11,
                          color: isSelected
                              ? const Color(0xFFFFFFFF)
                              : canInteract
                                  ? c.textSecondary
                                  : c.textDisabled,
                          fontFeatures: const [FontFeature.tabularFigures()],
                        ),
                      ),
                    ),
                  ),
                );
              }).toList(),
            ),
          ),
          // 分割线
          Divider(color: c.border, height: 1),
          // 自定义分钟输入
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 10, 12, 12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  s.shutdownDelayLabel,
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
                const SizedBox(height: 6),
                Row(
                  children: [
                    Expanded(
                      child: ShadInput(
                        controller: minutesController,
                        enabled: canInteract,
                        keyboardType: TextInputType.number,
                        inputFormatters: [
                          FilteringTextInputFormatter.digitsOnly,
                        ],
                        onSubmitted: (_) => onApplyCustom(),
                      ),
                    ),
                    const SizedBox(width: 6),
                    Text(
                      s.shutdownMinutesUnit,
                      style: TextStyle(fontSize: 12, color: c.textMuted),
                    ),
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
