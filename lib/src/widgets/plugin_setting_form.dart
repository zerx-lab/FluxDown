// 单个插件的设置表单对话框。
//
// 遍历 PluginInfoSignal.settings 按 widget 类型分发对应输入控件；提交前做
// required/pattern/min-max/select 成员前置校验（全部通过才发起
// SavePluginSettings，避免服务端半路校验失败）。服务端异步返回
// PluginOpResult：ok=false 时展示 message，并把 failedKey 对应字段标红。

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/plugin_provider.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'dir_picker_field.dart';

/// 弹出插件设置对话框。
void showPluginSettingsDialog(
  BuildContext context, {
  required PluginInfoSignal plugin,
  required PluginProvider provider,
}) {
  final c = AppColors.of(context);
  showShadDialog(
    context: context,
    barrierColor: c.dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => PluginSettingForm(plugin: plugin, provider: provider),
  );
}

/// 单条设置项前置校验：required → number/min/max → pattern → select 成员。
/// 首个失败项即返回对应错误文案，全部通过返回 null。
String? _validateField(S s, SettingFieldSignal field, String raw) {
  final value = raw.trim();
  if (field.required && value.isEmpty) return s.pluginErrRequired;
  if (value.isEmpty) return null;

  if (field.settingType == 'number') {
    final n = double.tryParse(value);
    if (n == null) return s.pluginErrNumber;
    if (field.hasMin && n < field.min) {
      return s.pluginErrMin(_trimNum(field.min));
    }
    if (field.hasMax && n > field.max) {
      return s.pluginErrMax(_trimNum(field.max));
    }
  }

  if (field.pattern.isNotEmpty) {
    var ok = true;
    try {
      ok = RegExp(field.pattern).hasMatch(value);
    } catch (_) {
      ok = true; // 插件提供的正则非法：不阻塞提交
    }
    if (!ok) return s.pluginErrPattern;
  }

  if (field.widget == 'select' &&
      field.options.isNotEmpty &&
      !field.options.any((o) => o.value == value)) {
    return s.pluginErrSelect;
  }
  return null;
}

/// 去掉整数值的多余小数位（3.0 → "3"，3.5 → "3.5"）。
String _trimNum(double v) =>
    v == v.truncateToDouble() ? v.truncate().toString() : v.toString();

class PluginSettingForm extends StatefulWidget {
  final PluginInfoSignal plugin;
  final PluginProvider provider;

  const PluginSettingForm({
    super.key,
    required this.plugin,
    required this.provider,
  });

  @override
  State<PluginSettingForm> createState() => _PluginSettingFormState();
}

class _PluginSettingFormState extends State<PluginSettingForm> {
  late Map<String, String> _values;
  final Map<String, String> _errors = {};
  final Map<String, TextEditingController> _controllers = {};

  bool _saving = false;
  int _awaitSeq = -1;
  String? _serverError;

  @override
  void initState() {
    super.initState();
    final saved = {
      for (final e in widget.plugin.settingsValues) e.key: e.value,
    };
    _values = {
      for (final f in widget.plugin.settings)
        f.key: saved[f.key] ??
            (f.widget == 'toggle' && f.defaultValue.isEmpty
                ? 'false'
                : f.defaultValue),
    };
    widget.provider.addListener(_onProviderChanged);
  }

  @override
  void dispose() {
    widget.provider.removeListener(_onProviderChanged);
    for (final ctrl in _controllers.values) {
      ctrl.dispose();
    }
    super.dispose();
  }

  TextEditingController _controllerFor(String key) =>
      _controllers.putIfAbsent(key, () => TextEditingController(text: _values[key]));

  void _setValue(String key, String v) {
    setState(() {
      _values[key] = v;
      _errors.remove(key);
    });
  }

  void _onProviderChanged() {
    if (!mounted || !_saving) return;
    final seq = widget.provider.opResultSeq;
    if (seq == _awaitSeq) return;
    final result = widget.provider.lastOpResult;
    if (result == null || result.op != 'save_settings' ||
        result.identity != widget.plugin.identity) {
      return;
    }
    setState(() {
      _saving = false;
      if (result.ok) {
        _serverError = null;
      } else {
        _serverError = result.message;
        if (result.failedKey.isNotEmpty) {
          _errors[result.failedKey] = result.message;
        }
      }
    });
    if (result.ok && mounted) {
      Navigator.of(context).pop();
    }
  }

  void _submit() {
    final s = currentS;
    final nextErrors = <String, String>{};
    for (final field in widget.plugin.settings) {
      final err = _validateField(s, field, _values[field.key] ?? '');
      if (err != null) nextErrors[field.key] = err;
    }
    setState(() {
      _errors
        ..clear()
        ..addAll(nextErrors);
      _serverError = null;
    });
    if (nextErrors.isNotEmpty) return;

    setState(() {
      _saving = true;
      _awaitSeq = widget.provider.opResultSeq;
    });
    widget.provider.saveSettings(widget.plugin.identity, _values);
  }

  @override
  Widget build(BuildContext context) {
    final s = currentS;
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);

    return ShadDialog(
      title: Text(s.pluginSettingsDialogTitle(widget.plugin.name)),
      constraints: const BoxConstraints(maxWidth: 440),
      actions: [
        ShadButton.outline(
          onPressed: _saving ? null : () => Navigator.of(context).pop(),
          child: Text(s.cancel),
        ),
        ShadButton(
          onPressed: _saving ? null : _submit,
          child: Text(_saving ? s.pluginSettingsSaving : s.pluginSettingsSaveButton),
        ),
      ],
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxHeight: 420),
        child: SingleChildScrollView(
          // 留出内边距：否则输入框获焦时的焦点环（绘制在边框外 ~3px）会紧贴
          // SingleChildScrollView 的裁剪边界、看起来被裁平（尤见于多行 textarea）。
          padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 3),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              if (_serverError != null) ...[
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(10),
                  margin: const EdgeInsets.only(bottom: 12),
                  decoration: BoxDecoration(
                    color: m.subtle(c.statusError),
                    borderRadius: m.brMd,
                  ),
                  child: Text(
                    s.pluginSettingsSaveFailed(_serverError!),
                    style: TextStyle(fontSize: 12, color: c.statusError),
                  ),
                ),
              ],
              for (final field in widget.plugin.settings)
                Padding(
                  padding: const EdgeInsets.only(bottom: 14),
                  child: _SettingFieldRow(
                    field: field,
                    value: _values[field.key] ?? '',
                    error: _errors[field.key],
                    controller: field.widget == 'toggle' ||
                            field.widget == 'select' ||
                            field.widget == 'folder'
                        ? null
                        : _controllerFor(field.key),
                    onChanged: (v) => _setValue(field.key, v),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _SettingFieldRow extends StatelessWidget {
  final SettingFieldSignal field;
  final String value;
  final String? error;
  final TextEditingController? controller;
  final ValueChanged<String> onChanged;

  const _SettingFieldRow({
    required this.field,
    required this.value,
    required this.error,
    required this.controller,
    required this.onChanged,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final title = field.title.isNotEmpty ? field.title : field.key;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          title,
          style: TextStyle(
            fontSize: 13,
            fontWeight: FontWeight.w500,
            color: c.textPrimary,
          ),
        ),
        if (field.description.isNotEmpty) ...[
          const SizedBox(height: 2),
          Text(
            field.description,
            style: TextStyle(fontSize: 11.5, color: c.textMuted),
          ),
        ],
        const SizedBox(height: 6),
        _buildInput(context),
        if (field.helperScript.isNotEmpty) ...[
          const SizedBox(height: 6),
          Align(
            alignment: Alignment.centerLeft,
            child: ShadButton.outline(
              size: ShadButtonSize.sm,
              onPressed: () => _copyHelperScript(context),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  const Icon(LucideIcons.clipboardCopy, size: 13),
                  const SizedBox(width: 5),
                  Text(
                    field.helperLabel.isNotEmpty
                        ? field.helperLabel
                        : currentS.pluginCopyHelperScript,
                    style: const TextStyle(fontSize: 12),
                  ),
                ],
              ),
            ),
          ),
        ],
        if (error != null) ...[
          const SizedBox(height: 4),
          Text(error!, style: TextStyle(fontSize: 11, color: c.statusError)),
        ],
      ],
    );
  }
  /// 复制辅助脚本到剪贴板（绝不执行），toast 提示去开发者工具 Console 粘贴运行。
  Future<void> _copyHelperScript(BuildContext context) async {
    await Clipboard.setData(ClipboardData(text: field.helperScript));
    if (!context.mounted) return;
    ShadSonner.of(context).show(
      ShadToast(title: Text(currentS.pluginHelperScriptCopied)),
    );
  }

  Widget _buildInput(BuildContext context) {
    switch (field.widget) {
      case 'password':
        return ShadInput(
          controller: controller,
          obscureText: true,
          placeholder: field.defaultValue.isEmpty
              ? null
              : Text(field.defaultValue),
          onChanged: onChanged,
        );
      case 'textarea':
        return ShadInput(
          controller: controller,
          maxLines: 4,
          minLines: 3,
          onChanged: onChanged,
        );
      case 'number':
        return ShadInput(
          controller: controller,
          keyboardType: const TextInputType.numberWithOptions(
            decimal: true,
            signed: true,
          ),
          placeholder: field.hasMin || field.hasMax
              ? Text(_rangeHint(field))
              : null,
          onChanged: onChanged,
        );
      case 'toggle':
        return Align(
          alignment: Alignment.centerLeft,
          child: ShadSwitch(
            value: value == 'true',
            onChanged: (v) => onChanged(v ? 'true' : 'false'),
          ),
        );
      case 'select':
        return ShadSelect<String>(
          initialValue: value.isNotEmpty ? value : null,
          placeholder: Text(currentS.pluginSelectPlaceholder),
          options: [
            for (final o in field.options)
              ShadOption(value: o.value, child: Text(o.label)),
          ],
          selectedOptionBuilder: (context, v) {
            final match = field.options.where((o) => o.value == v);
            return Text(match.isEmpty ? v : match.first.label);
          },
          onChanged: (v) {
            if (v != null) onChanged(v);
          },
        );
      case 'folder':
        return DirPickerField(
          path: value,
          placeholder: currentS.pluginFolderPickPlaceholder,
          onTap: () async {
            final result = await FilePickerService.pickDirectory(
              dialogTitle: currentS.pluginFolderPickPlaceholder,
              initialDirectory: value.isNotEmpty ? value : null,
            );
            if (result != null) onChanged(result);
          },
        );
      case 'text':
      default:
        return ShadInput(
          controller: controller,
          placeholder: field.defaultValue.isEmpty
              ? null
              : Text(field.defaultValue),
          onChanged: onChanged,
        );
    }
  }

  String _rangeHint(SettingFieldSignal field) {
    if (field.hasMin && field.hasMax) {
      return '${_trimNum(field.min)} – ${_trimNum(field.max)}';
    }
    if (field.hasMin) return '≥ ${_trimNum(field.min)}';
    return '≤ ${_trimNum(field.max)}';
  }
}
