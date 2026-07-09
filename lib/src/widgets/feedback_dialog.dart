import 'package:flutter/material.dart'
    show
        CircularProgressIndicator,
        Colors,
        DefaultMaterialLocalizations,
        InputDecoration,
        Material,
        MaterialType,
        OutlineInputBorder,
        TextField,
        TextSelectionTheme,
        TextSelectionThemeData;
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../services/feedback_service.dart';
import '../services/log_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';

/// 在底部状态栏点击反馈按钮时调用此方法打开反馈对话框。
void showFeedbackDialog(BuildContext context) {
  showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (context) => const _FeedbackDialogContent(),
  );
}

class _FeedbackDialogContent extends StatefulWidget {
  const _FeedbackDialogContent();

  @override
  State<_FeedbackDialogContent> createState() => _FeedbackDialogContentState();
}

class _FeedbackDialogContentState extends State<_FeedbackDialogContent> {
  final _titleController = TextEditingController();
  final _descController = TextEditingController();
  final _contactController = TextEditingController();

  FeedbackType _type = FeedbackType.feature;

  /// 是否附带今日日志一并提交（默认开启）
  bool _attachLogs = true;

  /// idle | submitting | success | error
  String _status = 'idle';
  String _errorMsg = '';

  @override
  void dispose() {
    _titleController.dispose();
    _descController.dispose();
    _contactController.dispose();
    super.dispose();
  }

  bool get _canSubmit =>
      _titleController.text.trim().isNotEmpty &&
      _descController.text.trim().isNotEmpty &&
      _status != 'submitting';

  Future<void> _submit() async {
    if (!_canSubmit) return;
    setState(() {
      _status = 'submitting';
      _errorMsg = '';
    });

    final logs = _attachLogs ? await LogService.instance.readTodayLog() : null;
    if (!mounted) return;

    final result = await FeedbackService.instance.submit(
      type: _type,
      title: _titleController.text.trim(),
      description: _descController.text.trim(),
      contact: _contactController.text.trim(),
      logs: logs,
    );

    if (!mounted) return;

    if (result.success) {
      setState(() => _status = 'success');
      // 1.5 秒后自动关闭
      Future.delayed(const Duration(milliseconds: 1500), () {
        if (mounted) Navigator.of(context).pop();
      });
    } else {
      final s = LocaleScope.of(context);
      setState(() {
        _status = 'error';
        _errorMsg = result.message == 'rate_limited'
            ? s.feedbackRateLimited
            : s.feedbackError;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);

    return ShadDialog(
      constraints: const BoxConstraints(maxWidth: 480),
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
              LucideIcons.messageSquarePlus,
              size: 14,
              color: c.accent,
            ),
          ),
          const SizedBox(width: 10),
          Text(s.feedbackTitle),
        ],
      ),
      description: Text(s.feedbackDesc),
      actions: [
        // 成功提示
        if (_status == 'success')
          Expanded(
            child: Row(
              children: [
                Icon(LucideIcons.circleCheck, size: 14, color: AppColors.green),
                const SizedBox(width: 6),
                Text(
                  s.feedbackSuccess,
                  style: TextStyle(fontSize: 12, color: AppColors.green),
                ),
              ],
            ),
          )
        // 错误提示
        else if (_status == 'error')
          Expanded(
            child: Row(
              children: [
                Icon(LucideIcons.circleAlert, size: 14, color: AppColors.red),
                const SizedBox(width: 6),
                Flexible(
                  child: Text(
                    _errorMsg,
                    style: TextStyle(fontSize: 12, color: AppColors.red),
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              ],
            ),
          ),
        if (_status != 'success') ...[
          ShadButton.outline(
            onPressed: () => Navigator.of(context).pop(),
            child: Text(s.cancel),
          ),
          ShadButton(
            enabled: _canSubmit,
            onPressed: _submit,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (_status == 'submitting')
                  Padding(
                    padding: const EdgeInsets.only(right: 6),
                    child: SizedBox(
                      width: 13,
                      height: 13,
                      child: CircularProgressIndicator(
                        strokeWidth: 2,
                        color: Colors.white,
                      ),
                    ),
                  )
                else
                  Padding(
                    padding: const EdgeInsets.only(right: 6),
                    child: Icon(
                      LucideIcons.send,
                      size: 13,
                      color: Colors.white,
                    ),
                  ),
                Text(
                  _status == 'submitting'
                      ? s.feedbackSubmitting
                      : s.feedbackSubmit,
                  style: const TextStyle(color: Colors.white),
                ),
              ],
            ),
          ),
        ],
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            // ── 反馈类型选择 ──
            _SectionLabel(text: s.feedbackTypeLabel, c: c),
            const SizedBox(height: 6),
            Row(
              children: [
                _TypeChip(
                  label: s.feedbackTypeFeature,
                  icon: LucideIcons.lightbulb,
                  selected: _type == FeedbackType.feature,
                  color: AppColors.amber,
                  c: c,
                  onTap: () => setState(() => _type = FeedbackType.feature),
                ),
                const SizedBox(width: 8),
                _TypeChip(
                  label: s.feedbackTypeBug,
                  icon: LucideIcons.bug,
                  selected: _type == FeedbackType.bug,
                  color: AppColors.red,
                  c: c,
                  onTap: () => setState(() => _type = FeedbackType.bug),
                ),
                const SizedBox(width: 8),
                _TypeChip(
                  label: s.feedbackTypeOther,
                  icon: LucideIcons.messageCircle,
                  selected: _type == FeedbackType.other,
                  color: c.accent,
                  c: c,
                  onTap: () => setState(() => _type = FeedbackType.other),
                ),
              ],
            ),
            const SizedBox(height: 14),

            // ── 标题 ──
            Row(
              children: [
                _SectionLabel(text: s.feedbackTitleLabel, c: c, required: true),
                const Spacer(),
                Text(
                  s.feedbackTitleCount(_titleController.text.length),
                  style: TextStyle(fontSize: 10, color: c.textMuted),
                ),
              ],
            ),
            const SizedBox(height: 6),
            _buildTextField(
              controller: _titleController,
              placeholder: s.feedbackTitlePlaceholder,
              maxLength: 200,
              maxLines: 1,
              c: c,
              m: m,
            ),
            const SizedBox(height: 14),

            // ── 描述 ──
            Row(
              children: [
                _SectionLabel(text: s.feedbackDescLabel, c: c, required: true),
                const Spacer(),
                Text(
                  s.feedbackDescCount(_descController.text.length),
                  style: TextStyle(fontSize: 10, color: c.textMuted),
                ),
              ],
            ),
            const SizedBox(height: 6),
            _buildTextField(
              controller: _descController,
              placeholder: s.feedbackDescPlaceholder,
              maxLength: 5000,
              maxLines: 5,
              c: c,
              m: m,
            ),
            const SizedBox(height: 14),

            // ── 联系方式（可选） ──
            Row(
              children: [
                _SectionLabel(text: s.feedbackContactLabel, c: c),
                const SizedBox(width: 6),
                Text(
                  '(${s.feedbackOptional})',
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
              ],
            ),
            const SizedBox(height: 6),
            _buildTextField(
              controller: _contactController,
              placeholder: s.feedbackContactPlaceholder,
              maxLines: 1,
              c: c,
              m: m,
            ),
            const SizedBox(height: 4),
            Text(
              s.feedbackContactHint,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
            const SizedBox(height: 14),

            // ── 附带今日日志 ──
            GestureDetector(
              onTap: () => setState(() => _attachLogs = !_attachLogs),
              behavior: HitTestBehavior.opaque,
              child: Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          s.feedbackAttachLogs,
                          style: TextStyle(fontSize: 13, color: c.textPrimary),
                        ),
                        const SizedBox(height: 2),
                        Text(
                          s.feedbackAttachLogsHint,
                          style: TextStyle(fontSize: 11, color: c.textMuted),
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(width: 12),
                  ShadSwitch(
                    value: _attachLogs,
                    onChanged: (v) => setState(() => _attachLogs = v),
                    width: 34,
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  /// 统一的输入框构建，使用 Material TextField 以获得更好的文本选择体验。
  Widget _buildTextField({
    required TextEditingController controller,
    required String placeholder,
    required int maxLines,
    required AppColors c,
    required AppMetrics m,
    int? maxLength,
  }) {
    return Localizations(
      locale: const Locale('en'),
      delegates: const [
        DefaultWidgetsLocalizations.delegate,
        DefaultMaterialLocalizations.delegate,
      ],
      child: Material(
        type: MaterialType.transparency,
        child: TextSelectionTheme(
          data: TextSelectionThemeData(
            cursorColor: c.accent,
            selectionColor: m.textSelection(c.accent),
            selectionHandleColor: c.accent,
          ),
          child: TextField(
            controller: controller,
            maxLines: maxLines,
            maxLength: maxLength,
            buildCounter:
                (_, {required currentLength, required isFocused, maxLength}) =>
                    null,
            onChanged: (_) => setState(() {}),
            style: TextStyle(fontSize: 13, color: c.textPrimary),
            cursorColor: c.accent,
            decoration: InputDecoration(
              hintText: placeholder,
              hintStyle: TextStyle(fontSize: 13, color: c.textMuted),
              contentPadding: const EdgeInsets.symmetric(
                horizontal: 12,
                vertical: 10,
              ),
              filled: true,
              fillColor: c.inputBg,
              border: OutlineInputBorder(
                borderRadius: m.brInput,
                borderSide: BorderSide(color: c.inputBorder),
              ),
              enabledBorder: OutlineInputBorder(
                borderRadius: m.brInput,
                borderSide: BorderSide(color: c.inputBorder),
              ),
              focusedBorder: OutlineInputBorder(
                borderRadius: m.brInput,
                borderSide: BorderSide(color: c.inputFocusBorder, width: 1.5),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 区块标签
class _SectionLabel extends StatelessWidget {
  final String text;
  final AppColors c;
  final bool required;

  const _SectionLabel({
    required this.text,
    required this.c,
    this.required = false,
  });

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          text,
          style: TextStyle(
            fontSize: 12,
            fontWeight: FontWeight.w500,
            color: c.textSecondary,
          ),
        ),
        if (required) ...[
          const SizedBox(width: 3),
          Text('*', style: TextStyle(fontSize: 12, color: AppColors.red)),
        ],
      ],
    );
  }
}

/// 反馈类型选择 Chip
class _TypeChip extends StatelessWidget {
  final String label;
  final IconData icon;
  final bool selected;
  final Color color;
  final AppColors c;
  final VoidCallback onTap;

  const _TypeChip({
    required this.label,
    required this.icon,
    required this.selected,
    required this.color,
    required this.c,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return GestureDetector(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
        decoration: BoxDecoration(
          color: selected ? m.muted(color) : c.surface2,
          borderRadius: m.brCard,
          border: Border.all(
            color: selected ? m.borderFaint(color) : c.border,
          ),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 14, color: selected ? color : c.textMuted),
            const SizedBox(width: 6),
            Text(
              label,
              style: TextStyle(
                fontSize: 12,
                fontWeight: selected ? FontWeight.w500 : FontWeight.normal,
                color: selected ? color : c.textMuted,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
