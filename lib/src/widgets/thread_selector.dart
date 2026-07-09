/// 线程数选择器 — 支持预设选项 + 自定义输入 (1-256)
///
/// 默认显示为 [ShadSelect] 下拉选择器（Auto / 4 / 8 / 16 / 32 / 64 / 自定义）。
/// 选择「自定义」后，同一位置原地变为可编辑输入框；右侧下拉箭头可切回预设选择。
library;

import 'package:flutter/material.dart'
    show
        DefaultMaterialLocalizations,
        InputBorder,
        InputDecoration,
        Material,
        MaterialType,
        TextField;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

/// 预设选项值（与下拉列表顺序一致）
const _kPresets = ['auto', '4', '8', '16', '32', '64'];

/// 线程数选择器。
///
/// [value] 为 null / `'auto'` 时表示自动，数字字符串表示固定线程数。
/// 选中预设时回调值为 null（auto）或预设字符串；
/// 自定义输入有效时回调对应数字字符串，无效时回调 null。
class ThreadSelector extends StatefulWidget {
  /// 当前值：null / 'auto' = 自动，数字字符串 = 固定线程数。
  final String? value;

  /// 值变化回调。null 表示自动（或自定义输入暂时无效）。
  final ValueChanged<String?> onChanged;

  /// key 版本号，变化时强制重建内部 Select（外部切换队列时递增）。
  final int version;

  const ThreadSelector({
    super.key,
    required this.value,
    required this.onChanged,
    this.version = 0,
  });

  @override
  State<ThreadSelector> createState() => _ThreadSelectorState();
}

class _ThreadSelectorState extends State<ThreadSelector> {
  final _ctrl = TextEditingController();
  final _focusNode = FocusNode();

  /// 是否处于自定义输入模式
  bool _isCustom = false;

  /// 输入框是否聚焦（用于切换边框颜色）
  bool _isFocused = false;

  /// 上次同步的 version，避免重复 _detectCustom
  int _prevVersion = -1;

  /// Select 重建计数器，退出自定义模式时递增以确保 ShadSelect 状态刷新
  int _selectRebuild = 0;

  @override
  void initState() {
    super.initState();
    _focusNode.addListener(_onFocusChanged);
    _prevVersion = widget.version;
    _detectCustom();
  }

  @override
  void didUpdateWidget(covariant ThreadSelector old) {
    super.didUpdateWidget(old);
    if (widget.version != _prevVersion) {
      _prevVersion = widget.version;
      _detectCustom();
    }
  }

  /// 根据当前 [widget.value] 判断是否应处于自定义模式。
  ///
  /// 当外部传入的值既不是 null/auto，也不在预设列表中时（如队列默认 10 线程），
  /// 自动切入自定义模式并预填充输入框。
  void _detectCustom() {
    final v = widget.value;
    if (v != null && v != 'auto' && !_kPresets.contains(v)) {
      _isCustom = true;
      _ctrl.text = v;
    } else {
      _isCustom = false;
    }
  }

  void _onFocusChanged() {
    if (mounted) setState(() => _isFocused = _focusNode.hasFocus);
  }

  @override
  void dispose() {
    _focusNode.removeListener(_onFocusChanged);
    _focusNode.dispose();
    _ctrl.dispose();
    super.dispose();
  }

  // ─────────────────────────────────────────────
  // 模式切换
  // ─────────────────────────────────────────────

  /// 进入自定义输入模式
  void _enterCustom() {
    final v = widget.value;
    // 若当前值是数字，预填充以便快速编辑
    if (v != null && v != 'auto' && int.tryParse(v) != null) {
      _ctrl.text = v;
      _ctrl.selection = TextSelection(baseOffset: 0, extentOffset: v.length);
    } else {
      _ctrl.clear();
    }
    setState(() => _isCustom = true);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focusNode.requestFocus();
    });
  }

  /// 退出自定义模式，回到预设选择并重置为 Auto
  void _exitCustom() {
    setState(() {
      _isCustom = false;
      _selectRebuild++;
    });
    widget.onChanged(null);
  }

  /// 自定义输入变化时校验 1-256 范围；超出上限时自动回调整为 256
  void _onInputChanged(String text) {
    final n = int.tryParse(text);
    if (n == null || n < 1) {
      widget.onChanged(null);
      return;
    }
    if (n > 256) {
      _ctrl.text = '256';
      _ctrl.selection = const TextSelection.collapsed(offset: 3);
      widget.onChanged('256');
      return;
    }
    widget.onChanged(text);
  }

  // ─────────────────────────────────────────────
  // Build
  // ─────────────────────────────────────────────

  @override
  Widget build(BuildContext context) {
    return _isCustom ? _buildInput(context) : _buildSelect(context);
  }

  /// 预设下拉选择模式
  Widget _buildSelect(BuildContext context) {
    final s = LocaleScope.of(context);
    final display = widget.value;
    final initial = (display == null || !_kPresets.contains(display))
        ? 'auto'
        : display;

    return ShadSelect<String>(
      key: ValueKey('ts_${widget.version}_$_selectRebuild'),
      placeholder: Text(s.auto),
      initialValue: initial,
      options: [
        ..._kPresets.map(
          (v) => ShadOption(value: v, child: Text(v == 'auto' ? s.auto : v)),
        ),
        ShadOption(value: 'custom', child: Text(s.customThreads)),
      ],
      selectedOptionBuilder: (_, value) =>
          Text(value == 'auto' ? s.auto : value),
      onChanged: (v) {
        if (v == 'custom') {
          _enterCustom();
        } else {
          widget.onChanged(v == 'auto' ? null : v);
        }
      },
    );
  }

  /// 自定义输入模式 — 外观与 ShadSelect 对齐，右侧下拉箭头可切回
  Widget _buildInput(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final borderColor = _isFocused ? c.inputFocusBorder : c.inputBorder;
    final bgColor = _isFocused ? c.inputFocusBg : c.inputBg;

    return ConstrainedBox(
      constraints: const BoxConstraints(minWidth: 100, maxWidth: 180),
      child: Container(
        height: 36,
        decoration: BoxDecoration(
          borderRadius: m.brInput,
          border: Border.all(color: borderColor),
          color: bgColor,
        ),
        clipBehavior: Clip.hardEdge,
        child: Row(
          children: [
            // 可编辑输入区
            Expanded(
              child: Localizations(
                locale: const Locale('en'),
                delegates: const [
                  DefaultWidgetsLocalizations.delegate,
                  DefaultMaterialLocalizations.delegate,
                ],
                child: Material(
                  type: MaterialType.transparency,
                  child: TextField(
                    controller: _ctrl,
                    focusNode: _focusNode,
                    style: TextStyle(fontSize: 14, color: c.textPrimary),
                    decoration: InputDecoration(
                      border: InputBorder.none,
                      isDense: true,
                      contentPadding: const EdgeInsets.symmetric(
                        horizontal: 12,
                        vertical: 9,
                      ),
                      hintText: s.customThreadsHint,
                      hintStyle: TextStyle(fontSize: 14, color: c.textMuted),
                    ),
                    inputFormatters: [
                      FilteringTextInputFormatter.digitsOnly,
                      LengthLimitingTextInputFormatter(3),
                    ],
                    textInputAction: TextInputAction.done,
                    onChanged: _onInputChanged,
                    onSubmitted: (_) => _focusNode.unfocus(),
                  ),
                ),
              ),
            ),
            // 切回预设选择的下拉箭头按钮
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: _exitCustom,
                behavior: HitTestBehavior.opaque,
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 8),
                  child: Icon(
                    LucideIcons.chevronDown,
                    size: 14,
                    color: c.textMuted,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
