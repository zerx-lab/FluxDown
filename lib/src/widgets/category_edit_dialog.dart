import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../i18n/locale_provider.dart';
import '../models/custom_category.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'dir_picker_field.dart';

/// 显示分类编辑对话框（新建或编辑）
void showCategoryEditDialog(
  BuildContext context, {
  CustomCategory? existing,
  required ValueChanged<CustomCategory> onSave,
  /// 仅自定义分类（非内置）传入；调用后直接执行删除，对话框内会先弹确认
  VoidCallback? onDelete,
}) {
  final s = LocaleScope.of(context);
  final c = AppColors.of(context);
  showShadDialog(
    context: context,
    barrierColor: c.dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (ctx) => CategoryEditDialog(
      s: s,
      c: c,
      existing: existing,
      onSave: onSave,
      onDelete: onDelete,
    ),
  );
}

/// 新建/编辑分类对话框
class CategoryEditDialog extends StatefulWidget {
  final S s;
  final AppColors c;
  final CustomCategory? existing;
  final ValueChanged<CustomCategory> onSave;

  /// 非空时对话框底部显示"删除分类"按钮（仅自定义分类）
  final VoidCallback? onDelete;

  const CategoryEditDialog({
    super.key,
    required this.s,
    required this.c,
    this.existing,
    required this.onSave,
    this.onDelete,
  });

  @override
  State<CategoryEditDialog> createState() => _CategoryEditDialogState();
}

class _CategoryEditDialogState extends State<CategoryEditDialog> {
  late final TextEditingController _nameCtrl;
  late final TextEditingController _extCtrl;
  late final TextEditingController _regexCtrl;
  late MatchMode _matchMode;
  late CategoryIcon _selectedIcon;
  String? _error;
  late final TextEditingController _saveDirCtrl;
  bool _isDirPicking = false;

  /// 是否隐藏"匹配规则"区域。
  /// - 'all'：完全锁定，不可编辑。
  /// - 'other'：用排除逻辑匹配，无显式规则，仍隐藏此区域；
  ///            但允许编辑名称、图标和保存路径（sidebar 已解除限制）。
  bool get _isSpecialBuiltin =>
      widget.existing?.isBuiltin == true &&
      (widget.existing?.builtinType == 'all' ||
          widget.existing?.builtinType == 'other');

  @override
  void initState() {
    super.initState();
    final e = widget.existing;
    _nameCtrl = TextEditingController(text: e?.name ?? '');
    _extCtrl = TextEditingController(
      text: e?.extensions.join(', ') ?? '',
    );
    _regexCtrl = TextEditingController(text: e?.regexPattern ?? '');
    _matchMode = e?.matchMode ?? MatchMode.extension;
    _selectedIcon = e?.icon ?? CategoryIcon.file;
    _saveDirCtrl = TextEditingController(text: e?.saveDir ?? '');
    _saveDirCtrl.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _nameCtrl.dispose();
    _extCtrl.dispose();
    _regexCtrl.dispose();
    _saveDirCtrl.dispose();
    super.dispose();
  }

  Future<void> _pickSaveDir() async {
    if (_isDirPicking) return;
    setState(() => _isDirPicking = true);
    try {
      final result = await FilePickerService.pickDirectory(
        dialogTitle: widget.s.categorySaveDir,
        initialDirectory: _saveDirCtrl.text.trim().isNotEmpty
            ? _saveDirCtrl.text.trim()
            : null,
      );
      if (result != null && mounted) {
        setState(() => _saveDirCtrl.text = result);
      }
    } on FilePickerException catch (_) {
      // FilePickerService 返回 null 表示用户取消，异常仅在真正错误时抛出
      // 静默忽略，避免打扰用户
    } catch (_) {
      // 忽略其他错误
    } finally {
      if (mounted) setState(() => _isDirPicking = false);
    }
  }

  void _save() {
    final name = _nameCtrl.text.trim();
    if (name.isEmpty && !(widget.existing?.isBuiltin ?? false)) {
      setState(() => _error = widget.s.categoryNameRequired);
      return;
    }

    List<String> extensions = widget.existing?.extensions ?? [];
    String regexPattern = widget.existing?.regexPattern ?? '';

    if (!_isSpecialBuiltin) {
      if (_matchMode == MatchMode.extension) {
        extensions = _extCtrl.text
            .split(RegExp(r'[,，\s]+'))
            .map((e) => e.trim().replaceAll('.', '').toLowerCase())
            .where((e) => e.isNotEmpty)
            .toList();
        if (extensions.isEmpty && !(widget.existing?.isBuiltin ?? false)) {
          setState(() => _error = widget.s.extensionsRequired);
          return;
        }
        regexPattern = '';
      } else {
        regexPattern = _regexCtrl.text.trim();
        if (regexPattern.isNotEmpty) {
          try {
            RegExp(regexPattern);
          } catch (_) {
            setState(() => _error = widget.s.regexInvalid);
            return;
          }
        }
        extensions = [];
      }
    }

    final category = CustomCategory(
      id: widget.existing?.id ??
          DateTime.now().microsecondsSinceEpoch.toRadixString(36),
      name: name,
      icon: _selectedIcon,
      matchMode: _matchMode,
      extensions: extensions,
      regexPattern: regexPattern,
      position: widget.existing?.position ?? 999,
      visible: widget.existing?.visible ?? true,
      isBuiltin: widget.existing?.isBuiltin ?? false,
      builtinType: widget.existing?.builtinType,
      saveDir: _saveDirCtrl.text.trim(),
    );

    Navigator.of(context).pop();
    widget.onSave(category);
  }

  /// 弹出二次确认后删除分类，确认后同时关闭本对话框。
  void _deleteWithConfirm() {
    final s = widget.s;
    final c = widget.c;
    showShadDialog(
      context: context,
      barrierColor: c.dialogBarrier,
      animateIn: const [],
      animateOut: const [],
      builder: (ctx) => ShadDialog(
        title: Text(s.deleteCategory),
        description: Text(s.deleteCategoryConfirm),
        actions: [
          ShadButton.outline(
            onPressed: () => Navigator.of(ctx).pop(),
            child: Text(s.cancel),
          ),
          ShadButton.destructive(
            onPressed: () {
              Navigator.of(ctx).pop(); // 关闭确认对话框
              if (!context.mounted) return;
              Navigator.of(context).pop(); // 关闭编辑对话框
              widget.onDelete?.call();
            },
            child: Text(s.deleteCategory),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final s = widget.s;
    final c = widget.c;

    return ShadDialog(
      title: Text(widget.existing != null ? s.editCategory : s.addCategory),
      actions: [
        // 自定义分类才显示删除按钮，放在最左侧
        if (widget.onDelete != null)
          ShadButton.destructive(
            onPressed: _deleteWithConfirm,
            child: Text(s.deleteCategory),
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
            // 名称
            Text(
              s.categoryName,
              style: TextStyle(
                fontSize: 11.5,
                fontWeight: FontWeight.w500,
                color: c.textSecondary,
              ),
            ),
            const SizedBox(height: 6),
            ShadInput(
              controller: _nameCtrl,
              placeholder: Text(s.categoryNameHint),
              autofocus: true,
            ),
            const SizedBox(height: 12),
            // 图标选择
            Text(
              s.categoryIcon,
              style: TextStyle(
                fontSize: 11.5,
                fontWeight: FontWeight.w500,
                color: c.textSecondary,
              ),
            ),
            const SizedBox(height: 6),
            CategoryIconPicker(
              selected: _selectedIcon,
              c: c,
              onChanged: (icon) => setState(() => _selectedIcon = icon),
            ),
            // 匹配规则（all / other 不显示）
            if (!_isSpecialBuiltin) ...[
              const SizedBox(height: 12),
              Text(
                s.matchMode,
                style: TextStyle(
                  fontSize: 11.5,
                  fontWeight: FontWeight.w500,
                  color: c.textSecondary,
                ),
              ),
              const SizedBox(height: 6),
              Row(
                children: [
                  MatchModeChip(
                    label: s.matchByExtension,
                    isSelected: _matchMode == MatchMode.extension,
                    c: c,
                    onTap: () => setState(() {
                      _matchMode = MatchMode.extension;
                      _error = null;
                    }),
                  ),
                  const SizedBox(width: 8),
                  MatchModeChip(
                    label: s.matchByRegex,
                    isSelected: _matchMode == MatchMode.regex,
                    c: c,
                    onTap: () => setState(() {
                      _matchMode = MatchMode.regex;
                      _error = null;
                    }),
                  ),
                ],
              ),
              const SizedBox(height: 12),
              if (_matchMode == MatchMode.extension) ...[
                Text(
                  s.extensionsLabel,
                  style: TextStyle(
                    fontSize: 11.5,
                    fontWeight: FontWeight.w500,
                    color: c.textSecondary,
                  ),
                ),
                const SizedBox(height: 6),
                ShadInput(
                  controller: _extCtrl,
                  placeholder: Text(s.extensionsHint),
                ),
              ] else ...[
                Text(
                  s.regexLabel,
                  style: TextStyle(
                    fontSize: 11.5,
                    fontWeight: FontWeight.w500,
                    color: c.textSecondary,
                  ),
                ),
                const SizedBox(height: 6),
                ShadInput(
                  controller: _regexCtrl,
                  placeholder: Text(s.regexHint),
                ),
              ],
            ],
            // 分类保存目录（"全部" 分类不显示，因其等同于全局默认）
            if (widget.existing?.builtinType != 'all') ...[
              const SizedBox(height: 12),
              Text(
                s.categorySaveDir,
                style: TextStyle(
                  fontSize: 11.5,
                  fontWeight: FontWeight.w500,
                  color: c.textSecondary,
                ),
              ),
              const SizedBox(height: 4),
              Text(
                s.categorySaveDirDesc,
                style: TextStyle(
                  fontSize: 11,
                  color: c.textMuted,
                ),
              ),
              const SizedBox(height: 6),
              Row(
                children: [
                  Expanded(
                    child: DirPickerField(
                      path: _saveDirCtrl.text,
                      placeholder: s.selectSaveDir,
                      enabled: !_isDirPicking,
                      onTap: _pickSaveDir,
                    ),
                  ),
                  if (_saveDirCtrl.text.isNotEmpty) ...[
                    const SizedBox(width: 8),
                    GestureDetector(
                      onTap: () => setState(() => _saveDirCtrl.text = ''),
                      child: MouseRegion(
                        cursor: SystemMouseCursors.click,
                        child: Container(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 8,
                            vertical: 5,
                          ),
                          decoration: BoxDecoration(
                            borderRadius: m.brMd,
                            border: Border.all(color: c.border),
                          ),
                          child: Text(
                            s.restoreDefaultPath,
                            style: TextStyle(
                              fontSize: 11.5,
                              color: c.textSecondary,
                            ),
                          ),
                        ),
                      ),
                    ),
                  ],
                ],
              ),
            ],
            if (_error != null) ...[
              const SizedBox(height: 8),
              Text(
                _error!,
                style: TextStyle(fontSize: 11.5, color: AppColors.red),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// CategoryIcon -> LucideIcons 映射（公共）
IconData categoryIconData(CategoryIcon icon) => switch (icon) {
  CategoryIcon.folders => LucideIcons.folders,
  CategoryIcon.film => LucideIcons.film,
  CategoryIcon.music => LucideIcons.music,
  CategoryIcon.fileText => LucideIcons.fileText,
  CategoryIcon.image => LucideIcons.image,
  CategoryIcon.archive => LucideIcons.archive,
  CategoryIcon.file => LucideIcons.file,
  CategoryIcon.code => LucideIcons.code,
  CategoryIcon.database => LucideIcons.database,
  CategoryIcon.gamepad => LucideIcons.gamepad2,
  CategoryIcon.globe => LucideIcons.globe,
  CategoryIcon.bookmark => LucideIcons.bookmark,
  CategoryIcon.box => LucideIcons.box,
  CategoryIcon.cpu => LucideIcons.cpu,
  CategoryIcon.disc => LucideIcons.disc,
  CategoryIcon.font => LucideIcons.type,
  CategoryIcon.hardDrive => LucideIcons.hardDrive,
  CategoryIcon.library => LucideIcons.library,
  CategoryIcon.package2 => LucideIcons.package2,
  CategoryIcon.pen => LucideIcons.pen,
  CategoryIcon.printer => LucideIcons.printer,
  CategoryIcon.smartphone => LucideIcons.smartphone,
  CategoryIcon.subtitles => LucideIcons.captions,
  CategoryIcon.type => LucideIcons.type,
  CategoryIcon.zap => LucideIcons.zap,
};

/// 图标选择网格
class CategoryIconPicker extends StatelessWidget {
  final CategoryIcon selected;
  final AppColors c;
  final ValueChanged<CategoryIcon> onChanged;

  const CategoryIconPicker({
    super.key,
    required this.selected,
    required this.c,
    required this.onChanged,
  });

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    return Wrap(
      spacing: 4,
      runSpacing: 4,
      children: CategoryIcon.values.map((icon) {
        final isSelected = icon == selected;
        return GestureDetector(
          onTap: () => onChanged(icon),
          child: MouseRegion(
            cursor: SystemMouseCursors.click,
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 120),
              width: 30,
              height: 30,
              decoration: BoxDecoration(
                color: isSelected ? c.accentBg : Colors.transparent,
                borderRadius: m.brMd,
                border: Border.all(
                  color: isSelected ? c.accent : c.border,
                  width: isSelected ? 1.5 : 1,
                ),
              ),
              child: Icon(
                categoryIconData(icon),
                size: 14,
                color: isSelected ? c.accent : c.textSecondary,
              ),
            ),
          ),
        );
      }).toList(),
    );
  }
}

/// 匹配方式选择按钮
class MatchModeChip extends StatefulWidget {
  final String label;
  final bool isSelected;
  final AppColors c;
  final VoidCallback onTap;

  const MatchModeChip({
    super.key,
    required this.label,
    required this.isSelected,
    required this.c,
    required this.onTap,
  });

  @override
  State<MatchModeChip> createState() => _MatchModeChipState();
}

class _MatchModeChipState extends State<MatchModeChip> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final m = AppMetrics.of(context);
    final c = widget.c;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
          decoration: BoxDecoration(
            color: widget.isSelected
                ? c.accentBg
                : _hover
                    ? c.hoverBg
                    : Colors.transparent,
            borderRadius: m.brMd,
            border: Border.all(
              color: widget.isSelected ? c.accent : c.border,
            ),
          ),
          child: Text(
            widget.label,
            style: TextStyle(
              fontSize: 12,
              color: widget.isSelected ? c.accent : c.textSecondary,
              fontWeight:
                  widget.isSelected ? FontWeight.w500 : FontWeight.normal,
            ),
          ),
        ),
      ),
    );
  }
}
