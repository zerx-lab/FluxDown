/// 数字选择器 — 预设选项 + 自定义输入
///
/// 默认显示为 [ShadSelect] 下拉（预设数字 / 自定义）。选择「自定义」后，
/// 同一位置原地变为 [ShadInput] 数字输入框；右侧下拉箭头可切回预设选择。
/// 交互模式与 [ThreadSelector] 对齐，但值域为纯整数（无 Auto 语义），
/// 供「最大同时下载数」「Auto 模式连接上限」等设置复用。
library;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';

/// 「自定义」选项的哨兵值（不会与合法数字冲突）。
const _kCustomSentinel = -1;

class NumberSelector extends StatefulWidget {
  /// 当前值。
  final int value;

  /// 下拉预设选项（升序）。
  final List<int> presets;

  /// 自定义输入允许的最小/最大值（含）。
  final int min;
  final int max;

  /// 退出自定义模式（点击输入框右侧箭头）时回退的预设值。
  final int fallback;

  /// 值变化回调（仅在输入合法时触发）。
  final ValueChanged<int> onChanged;

  /// 选中态展示文案（如 `5 个任务`）；null = 直接显示数字。
  final String Function(int value)? selectedLabel;

  /// 下拉预设列表中单个选项的展示文案（如 `0 → 自动`）；null = 直接显示数字。
  /// 与 [selectedLabel] 分开，避免影响未传入本参数的既有调用方的列表展示。
  final String Function(int value)? presetLabel;

  const NumberSelector({
    super.key,
    required this.value,
    required this.presets,
    required this.min,
    required this.max,
    required this.fallback,
    required this.onChanged,
    this.selectedLabel,
    this.presetLabel,
  });

  @override
  State<NumberSelector> createState() => _NumberSelectorState();
}

class _NumberSelectorState extends State<NumberSelector> {
  final _ctrl = TextEditingController();
  final _focusNode = FocusNode();

  /// 是否处于自定义输入模式。
  bool _isCustom = false;

  /// Select 重建计数器，退出自定义模式时递增以确保 ShadSelect 状态刷新。
  int _selectRebuild = 0;

  @override
  void initState() {
    super.initState();
    // 外部持久化的值不在预设列表中（如用户此前自定义过）→ 直接进入自定义模式。
    if (!widget.presets.contains(widget.value)) {
      _isCustom = true;
      _ctrl.text = '${widget.value}';
    }
  }

  @override
  void didUpdateWidget(covariant NumberSelector old) {
    super.didUpdateWidget(old);
    // 外部值变化（异步加载完成/其它入口修改）时与内部形态对账。
    // 用户正在输入（聚焦中）时不介入：自身 onChanged 触发的父级重建
    // 会经这里回流，此刻强行切换形态会打断输入。
    if (old.value == widget.value || _focusNode.hasFocus) return;
    setState(() {
      _isCustom = !widget.presets.contains(widget.value);
      if (_isCustom) _ctrl.text = '${widget.value}';
      _selectRebuild++; // 强制重建 ShadSelect，使 initialValue 跟随新值
    });
  }

  @override
  void dispose() {
    _focusNode.dispose();
    _ctrl.dispose();
    super.dispose();
  }

  /// 进入自定义输入模式，预填充当前值以便快速编辑。
  void _enterCustom() {
    final v = '${widget.value}';
    _ctrl.text = v;
    _ctrl.selection = TextSelection(baseOffset: 0, extentOffset: v.length);
    setState(() => _isCustom = true);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focusNode.requestFocus();
    });
  }

  /// 退出自定义模式：值已是合法数字则保留，否则回退 [NumberSelector.fallback]。
  void _exitCustom() {
    setState(() {
      _isCustom = false;
      _selectRebuild++;
    });
    final n = int.tryParse(_ctrl.text);
    if (n == null || n < widget.min || n > widget.max) {
      widget.onChanged(widget.fallback);
    }
  }

  /// 自定义输入变化：钳制到 [min, max]，仅合法时上报。
  void _onInputChanged(String text) {
    final n = int.tryParse(text);
    if (n == null) return;
    if (n > widget.max) {
      final capped = '${widget.max}';
      _ctrl.text = capped;
      _ctrl.selection = TextSelection.collapsed(offset: capped.length);
      widget.onChanged(widget.max);
      return;
    }
    if (n < widget.min) return; // 输入中间态（如空/0），不上报
    widget.onChanged(n);
  }

  @override
  Widget build(BuildContext context) {
    return _isCustom ? _buildInput(context) : _buildSelect(context);
  }

  /// 预设下拉选择模式。
  Widget _buildSelect(BuildContext context) {
    final s = LocaleScope.of(context);
    String label(int v) => widget.selectedLabel?.call(v) ?? '$v';
    String optionLabel(int v) => widget.presetLabel?.call(v) ?? '$v';

    return ShadSelect<int>(
      key: ValueKey('ns_$_selectRebuild'),
      placeholder: Text(label(widget.value)),
      initialValue: widget.value,
      options: [
        for (final n in widget.presets)
          ShadOption(value: n, child: Text(optionLabel(n))),
        ShadOption(value: _kCustomSentinel, child: Text(s.customThreads)),
      ],
      selectedOptionBuilder: (_, value) => Text(label(value)),
      onChanged: (v) {
        if (v == null) return;
        if (v == _kCustomSentinel) {
          _enterCustom();
        } else {
          widget.onChanged(v);
        }
      },
    );
  }

  /// 自定义输入模式 — ShadInput + 右侧下拉箭头切回预设选择。
  Widget _buildInput(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);

    return ConstrainedBox(
      constraints: const BoxConstraints(minWidth: 100, maxWidth: 180),
      child: ShadInput(
        controller: _ctrl,
        focusNode: _focusNode,
        keyboardType: TextInputType.number,
        placeholder: Text(s.customRangeHint(widget.min, widget.max)),
        inputFormatters: [
          FilteringTextInputFormatter.digitsOnly,
          LengthLimitingTextInputFormatter('${widget.max}'.length),
        ],
        onChanged: _onInputChanged,
        onSubmitted: (_) => _focusNode.unfocus(),
        trailing: MouseRegion(
          cursor: SystemMouseCursors.click,
          child: GestureDetector(
            onTap: _exitCustom,
            behavior: HitTestBehavior.opaque,
            child: Icon(LucideIcons.chevronDown, size: 14, color: c.textMuted),
          ),
        ),
      ),
    );
  }
}
