// 显示选项面板 — 视图系统唯一控制面（ShadPopover 内容，contract-dart.md
// §入口/面板/快捷键，design-proto-spec.md §10）。
//
// 节顺序：形态(V) → 密度(Shift+D，网格禁用) → 分组(G) → 排序(S) →
// 显示(开关) → 列(仅列表形态) → 重置为默认。改动即时生效（无确认按钮），
// 全部写入 [ViewPrefsStore.update]（只影响当前状态页签的覆盖层）。

import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/view_prefs.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'flux_sonner.dart';
import 'task_columns.dart';

/// 显示选项面板正文（不含 ShadPopover 自带的玻璃浮层 chrome——沿用
/// `_SpeedLimitPopoverContent` 先例：popover content 只负责内容，边框/
/// 阴影/圆角由 ShadPopover 统一处理）。
class ViewOptionsPanel extends StatelessWidget {
  final DownloadController controller;
  final ViewPrefsStore viewPrefsStore;

  const ViewOptionsPanel({
    super.key,
    required this.controller,
    required this.viewPrefsStore,
  });

  String get _tab => controller.statusTab.name;

  void _apply(ViewPrefs Function(ViewPrefs current) updater) {
    viewPrefsStore.update(_tab, updater);
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    return ListenableBuilder(
      listenable: Listenable.merge([controller, viewPrefsStore]),
      builder: (context, _) {
        final prefs = viewPrefsStore.resolve(_tab);
        final isGrid = prefs.form == ViewForm.grid;

        // 限高：面板七节全展开可达 ~700px，小窗口下必须内部滚动——否则
        // 弹层超出可视区（顶栏按钮下方空间 = 窗高 - 顶栏 40 - 边距）。
        // 标题与底部「重置为默认」常驻，中间节区滚动。
        final maxHeight =
            (MediaQuery.sizeOf(context).height - 64).clamp(240.0, 720.0);
        return ConstrainedBox(
          constraints: BoxConstraints(maxHeight: maxHeight),
          child: SizedBox(
            width: 300,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(14, 13, 14, 11),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(
                    s.viewOptionsTitle,
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                      color: c.textPrimary,
                    ),
                  ),
                  const SizedBox(height: 12),
                  Flexible(
                    child: SingleChildScrollView(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          _section(
                            c: c,
                            label: s.viewSectionForm,
                            hint: 'V',
                            child: _formSegmented(context, c, prefs),
                          ),
                          const SizedBox(height: 12),
                          _section(
                            c: c,
                            label: s.viewSectionDensity,
                            hint: isGrid
                                ? s.viewDensityGridDisabledHint
                                : 'Shift+D',
                            child: _densitySegmented(context, c, prefs, isGrid),
                          ),
                          const SizedBox(height: 12),
                          _section(
                            c: c,
                            label: s.viewSectionGroupBy,
                            hint: 'G',
                            child: _groupByChips(context, c, prefs),
                          ),
                          const SizedBox(height: 12),
                          _section(
                            c: c,
                            label: s.viewSectionSort,
                            hint: 'S',
                            child: _sortChips(context, c, prefs),
                          ),
                          const SizedBox(height: 12),
                          _section(
                            c: c,
                            label: s.viewSectionDisplay,
                            hint: null,
                            child: _displaySwitches(context, c, prefs),
                          ),
                          if (!isGrid) ...[
                            const SizedBox(height: 12),
                            _section(
                              c: c,
                              label: s.viewSectionColumns,
                              hint: null,
                              child: _columnChips(context, c, prefs),
                            ),
                          ],
                        ],
                      ),
                    ),
                  ),
                  const SizedBox(height: 12),
                  Divider(height: 1, color: c.border),
                  const SizedBox(height: 8),
                  GestureDetector(
                    onTap: () {
                      viewPrefsStore.reset(_tab);
                      FluxSonner.of(context).show(
                        ShadToast(
                          title: Text(s.viewResetToast),
                          duration: const Duration(seconds: 2),
                        ),
                      );
                    },
                    child: Text(
                      s.viewResetDefault,
                      style: TextStyle(
                        fontSize: 12,
                        fontWeight: FontWeight.w500,
                        color: c.accent,
                      ),
                    ),
                  ),
                  const SizedBox(height: 4),
                  Text(
                    s.viewResetHint,
                    style: TextStyle(fontSize: 11, color: c.textMuted),
                  ),
                ],
              ),
            ),
          ),
        );
      },
    );
  }

  // ---------------------------------------------------------------------------
  // 节标题（label + 右侧灰色快捷键 hint）
  // ---------------------------------------------------------------------------

  Widget _section({
    required AppColors c,
    required String label,
    required String? hint,
    required Widget child,
  }) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Text(
              label,
              // 12px 常规灰字：此前 10.5px w600 + letterSpacing 在小字号下
              // 笔画过细过密，观感「过度锐化」难辨认（用户反馈）。
              style: TextStyle(fontSize: 12, color: c.textSecondary),
            ),
            const Spacer(),
            if (hint != null)
              Text(
                hint,
                style: TextStyle(fontSize: 10.5, color: c.textMuted),
              ),
          ],
        ),
        const SizedBox(height: 6),
        child,
      ],
    );
  }

  // ---------------------------------------------------------------------------
  // 形态 / 密度 — 二选一 segmented
  // ---------------------------------------------------------------------------

  Widget _formSegmented(BuildContext context, AppColors c, ViewPrefs prefs) {
    final s = LocaleScope.of(context);
    return _segmented<ViewForm>(
      context: context,
      options: [
        (ViewForm.list, s.viewFormList),
        (ViewForm.grid, s.viewFormGrid),
      ],
      selected: prefs.form,
      onChanged: (v) => _apply((p) => p.copyWith(form: v)),
    );
  }

  Widget _densitySegmented(
    BuildContext context,
    AppColors c,
    ViewPrefs prefs,
    bool disabled,
  ) {
    final s = LocaleScope.of(context);
    return _segmented<ViewDensity>(
      context: context,
      options: [
        (ViewDensity.comfortable, s.viewDensityComfortable),
        (ViewDensity.compact, s.viewDensityCompact),
      ],
      selected: prefs.density,
      enabled: !disabled,
      onChanged: (v) => _apply((p) => p.copyWith(density: v)),
    );
  }

  Widget _segmented<T>({
    required BuildContext context,
    required List<(T, String)> options,
    required T selected,
    required ValueChanged<T> onChanged,
    bool enabled = true,
  }) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Opacity(
      opacity: enabled ? 1 : 0.4,
      child: Container(
        height: 28,
        padding: const EdgeInsets.all(2),
        decoration: BoxDecoration(color: c.surface2, borderRadius: m.brSm),
        child: Row(
          children: [
            for (final opt in options)
              Expanded(
                child: GestureDetector(
                  onTap: enabled ? () => onChanged(opt.$1) : null,
                  child: Container(
                    alignment: Alignment.center,
                    decoration: BoxDecoration(
                      color: opt.$1 == selected ? c.accent : Colors.transparent,
                      borderRadius: m.brXs,
                    ),
                    child: Text(
                      opt.$2,
                      style: TextStyle(
                        fontSize: 12,
                        fontWeight: opt.$1 == selected
                            ? FontWeight.w500
                            : FontWeight.w400,
                        color: opt.$1 == selected
                            ? const Color(0xFFFFFFFF)
                            : c.textSecondary,
                      ),
                    ),
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 分组 — chips（7 项）
  // ---------------------------------------------------------------------------

  Widget _groupByChips(BuildContext context, AppColors c, ViewPrefs prefs) {
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final g in kGroupByCycle)
          _chip(
            context: context,
            label: g.label,
            selected: prefs.groupBy == g,
            onTap: () => _apply((p) => p.copyWith(groupBy: g)),
          ),
      ],
    );
  }

  Widget _chip({
    required BuildContext context,
    required String label,
    required bool selected,
    required VoidCallback onTap,
    IconData? trailingIcon,
  }) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    // 选中态前景色：规则 no-material-in-dart——不用 Material 的 Colors.white，
    // 一次性字面量（status_bar 预设 chips 同款先例）。
    final fg = selected ? const Color(0xFFFFFFFF) : c.textSecondary;
    return GestureDetector(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        decoration: BoxDecoration(
          color: selected ? c.accent : c.surface2,
          borderRadius: m.brPill,
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              label,
              style: TextStyle(
                fontSize: 12,
                fontWeight: selected ? FontWeight.w500 : FontWeight.w400,
                color: fg,
              ),
            ),
            if (trailingIcon != null) ...[
              const SizedBox(width: 3),
              Icon(trailingIcon, size: 11, color: fg),
            ],
          ],
        ),
      ),
    );
  }

  // ---------------------------------------------------------------------------
  // 排序 — chips（6 项，与「列」同布局；选中 chip 内嵌方向箭头）
  // 点未选中 chip = 切换排序键并重置为该键默认方向；点已选中非「智能」chip =
  // 翻转 ↑/↓（原型 §10 行为语义不变，仅布局从单选列表改为 chips）。
  // ---------------------------------------------------------------------------

  Widget _sortChips(BuildContext context, AppColors c, ViewPrefs prefs) {
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final key in kSortKeyCycle)
          _chip(
            context: context,
            label: key.label,
            selected: prefs.sortKey == key,
            trailingIcon: prefs.sortKey == key && key != ViewSortKey.smart
                ? (prefs.sortDir == SortDir.asc
                      ? LucideIcons.arrowUp
                      : LucideIcons.arrowDown)
                : null,
            onTap: () {
              if (prefs.sortKey == key && key != ViewSortKey.smart) {
                _apply(
                  (p) => p.copyWith(
                    sortDir: p.sortDir == SortDir.asc
                        ? SortDir.desc
                        : SortDir.asc,
                  ),
                );
              } else {
                _apply(
                  (p) =>
                      p.copyWith(sortKey: key, sortDir: kSortKeyDefaultDir[key]),
                );
              }
            },
          ),
      ],
    );
  }

  // ---------------------------------------------------------------------------
  // 显示 — chips（显示已完成 / 协议徽标，与「列」同布局，选中=开启）
  // ---------------------------------------------------------------------------

  Widget _displaySwitches(BuildContext context, AppColors c, ViewPrefs prefs) {
    final s = LocaleScope.of(context);
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        _chip(
          context: context,
          label: s.viewShowCompleted,
          selected: prefs.showCompleted,
          onTap: () =>
              _apply((p) => p.copyWith(showCompleted: !prefs.showCompleted)),
        ),
        _chip(
          context: context,
          label: s.viewProtocolBadges,
          selected: prefs.protocolBadges,
          onTap: () =>
              _apply((p) => p.copyWith(protocolBadges: !prefs.protocolBadges)),
        ),
      ],
    );
  }

  // ---------------------------------------------------------------------------
  // 列 — chips（9 项，仅列表形态；三入口之一，共用 tryToggleColumn 状态机）
  // ---------------------------------------------------------------------------

  Widget _columnChips(BuildContext context, AppColors c, ViewPrefs prefs) {
    final s = LocaleScope.of(context);
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final id in kColumnCanonicalOrder)
          _chip(
            context: context,
            label: kTaskColumns[id]!.label(s),
            selected: prefs.columns.contains(id),
            onTap: () {
              final rejection = tryToggleColumn(
                current: prefs.columns,
                toggling: id,
                listWidth: controller.listContentWidth,
                s: s,
              );
              if (rejection != null) {
                FluxSonner.of(context).show(
                  ShadToast.destructive(
                    title: Text(rejection),
                    duration: const Duration(seconds: 2),
                  ),
                );
                return;
              }
              final next = {...prefs.columns};
              if (next.contains(id)) {
                next.remove(id);
              } else {
                next.add(id);
              }
              _apply((p) => p.copyWith(columns: next));
            },
          ),
      ],
    );
  }
}
