// manifest_select_dialog.dart 的文件列表部分（v1.6 下钻导航版）：虚拟化
// ListView.builder + 单行渲染，零缩进——深度已转化为面包屑，本列表恒渲染
// 当前层（或搜索态的全局扁平结果）。
//
// 行布局两段式纪律：名称列弹性省略；右列（计数/大小/进入箭头）固定宽
// 右对齐，跨行像素对齐（design/desktop-task-views/styles.css `.mf-meta`
// 72 / `.mf-size` 74 / `.mf-enter` 16）。

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/manifest_selection.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'bt_file_list_widget.dart' show BtCheckbox;

/// 行高恒 34px（design §4.10：1000+ 项虚拟化后 DOM 仅 ~25 行）。
const double kManifestRowHeight = 34;
const double _kCountColWidth = 72;
const double _kSizeColWidth = 74;
const double _kEnterColWidth = 16;

/// 文件类型 → 色块 tile 配色（对齐 manifest.js `MF_EXT_TYPE` + styles.css
/// `.mf-ftile.t-*`）。program/other/all 没有专属色板，回退中性色（与原型
/// `.mf-ftile` 基础样式一致）。
(Color bg, Color fg) _fileTileColors(FileCategory category, AppColors c) {
  return switch (category) {
    FileCategory.video => (const Color(0x24A855F7), const Color(0xFFA855F7)),
    FileCategory.audio => (const Color(0x2406B6D4), const Color(0xFF06B6D4)),
    FileCategory.document => (c.accentBg, c.accent),
    FileCategory.image => (const Color(0x2422C55E), AppColors.green),
    FileCategory.archive => (const Color(0x24F59E0B), AppColors.amber),
    FileCategory.program || FileCategory.other || FileCategory.all => (
      c.surface2,
      c.textSecondary,
    ),
  };
}

/// 虚拟化文件列表：外部传入当前层（或搜索态）已算好的行流（[manifestRowsAt]
/// 输出），本组件只负责渲染 + 交互回调，不持有导航/选择/筛选状态。
class ManifestBrowseList extends StatelessWidget {
  final List<ManifestRow> rows;
  final Set<String> selectedItemIds;
  final double height;

  /// 点击目录行勾选框：整树选择/取消。
  final ValueChanged<String> onToggleDirSubtree;

  /// 点击目录行其余区域：进入该目录（下钻导航）。
  final ValueChanged<String> onEnterDir;

  /// 点击文件行（勾选框或行内任意区域，文件行没有"进入"语义，整行即勾选）。
  final ValueChanged<String> onToggleFile;

  const ManifestBrowseList({
    super.key,
    required this.rows,
    required this.selectedItemIds,
    required this.onToggleDirSubtree,
    required this.onEnterDir,
    required this.onToggleFile,
    this.height = 320,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    if (rows.isEmpty) {
      return SizedBox(
        height: height,
        child: Center(
          child: Text(
            LocaleScope.of(context).manifestTreeEmpty,
            textAlign: TextAlign.center,
            style: TextStyle(fontSize: 12, color: c.textMuted),
          ),
        ),
      );
    }
    return SizedBox(
      height: height,
      child: ListView.builder(
        padding: const EdgeInsets.symmetric(vertical: 4),
        itemExtent: kManifestRowHeight,
        itemCount: rows.length,
        itemBuilder: (context, index) {
          final row = rows[index];
          return switch (row) {
            ManifestDirRowEntry(row: final dir) => _ManifestDirRowWidget(
              row: dir,
              onToggleSelection: () => onToggleDirSubtree(dir.path),
              onEnter: () => onEnterDir(dir.path),
            ),
            ManifestFileRowEntry(row: final file) => _ManifestFileRowWidget(
              row: file,
              selected: selectedItemIds.contains(file.item.id),
              onToggle: () => onToggleFile(file.item.id),
            ),
          };
        },
      ),
    );
  }
}

/// 行外壳：hover 高亮 + hover 才显现的「进入」箭头（右列占位恒在，箭头
/// 本体透明度切换，右列纪律不受影响）。
class _RowShell extends StatefulWidget {
  final VoidCallback onTap;
  final List<Widget> children;
  final Color? selectedBg;

  const _RowShell({
    required this.onTap,
    required this.children,
    this.selectedBg,
  });

  @override
  State<_RowShell> createState() => _RowShellState();
}

class _RowShellState extends State<_RowShell> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        child: Container(
          height: kManifestRowHeight,
          padding: const EdgeInsets.symmetric(horizontal: 6),
          decoration: BoxDecoration(
            color: widget.selectedBg ?? (_hovered ? c.hoverBg : null),
            borderRadius: m.brSm,
          ),
          child: _HoverScope(
            hovered: _hovered,
            child: Row(children: widget.children),
          ),
        ),
      ),
    );
  }
}

/// 把 hover 状态往下传给「进入箭头」而不必整树重建（简单 InheritedWidget
/// 级别足够——行数有限，非性能瓶颈）。
class _HoverScope extends InheritedWidget {
  final bool hovered;
  const _HoverScope({required this.hovered, required super.child});

  static bool of(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<_HoverScope>()?.hovered ??
      false;

  @override
  bool updateShouldNotify(_HoverScope oldWidget) =>
      oldWidget.hovered != hovered;
}

class _EnterArrow extends StatelessWidget {
  const _EnterArrow();

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final hovered = _HoverScope.of(context);
    return SizedBox(
      width: _kEnterColWidth,
      child: AnimatedOpacity(
        opacity: hovered ? 1 : 0,
        duration: const Duration(milliseconds: 120),
        child: Icon(LucideIcons.chevronRight, size: 12, color: c.textMuted),
      ),
    );
  }
}

class _ManifestDirRowWidget extends StatelessWidget {
  final ManifestDirRow row;
  final VoidCallback onToggleSelection;
  final VoidCallback onEnter;

  const _ManifestDirRowWidget({
    required this.row,
    required this.onToggleSelection,
    required this.onEnter,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final state = manifestDirRowCheckState(row);
    return _RowShell(
      onTap: onEnter,
      children: [
        GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onToggleSelection,
          child: Padding(
            padding: const EdgeInsets.only(right: 8),
            child: BtCheckbox(
              checked: state == ManifestCheckState.checked,
              indeterminate: state == ManifestCheckState.indeterminate,
              accentColor: c.accent,
            ),
          ),
        ),
        Icon(LucideIcons.folder, size: 14, color: AppColors.amber),
        const SizedBox(width: 6),
        Expanded(child: _DirChainText(labels: row.labels, color: c)),
        SizedBox(
          width: _kCountColWidth,
          child: _DirCountText(row: row, s: s, c: c),
        ),
        SizedBox(
          width: _kSizeColWidth,
          child: Text(
            row.size > 0
                ? '${DownloadTask.formatBytes(row.size)}${row.unknown ? "+" : ""}'
                : (row.unknown ? s.manifestDirSizeUnknown : ''),
            textAlign: TextAlign.right,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(fontSize: 11.5, color: c.textSecondary),
          ),
        ),
        const _EnterArrow(),
      ],
    );
  }
}

/// 目录行计数列：`已选/总数 项`；无已选时纯 `总数 项`。
class _DirCountText extends StatelessWidget {
  final ManifestDirRow row;
  final S s;
  final AppColors c;
  const _DirCountText({required this.row, required this.s, required this.c});

  @override
  Widget build(BuildContext context) {
    if (row.selCnt <= 0) {
      return Text(
        s.manifestItemsCount(row.count),
        textAlign: TextAlign.right,
        overflow: TextOverflow.ellipsis,
        style: TextStyle(fontSize: 11, color: c.textMuted),
      );
    }
    return Text.rich(
      TextSpan(
        children: [
          TextSpan(
            text: '${row.selCnt}/',
            style: TextStyle(
              fontSize: 11,
              fontWeight: FontWeight.w600,
              color: c.accent,
            ),
          ),
          TextSpan(text: s.manifestItemsCount(row.count)),
        ],
      ),
      textAlign: TextAlign.right,
      overflow: TextOverflow.ellipsis,
      style: TextStyle(fontSize: 11, color: c.textMuted),
    );
  }
}

/// 目录行单链合并路径链：`a / b / c`，末段加粗。
class _DirChainText extends StatelessWidget {
  final List<String> labels;
  final AppColors color;
  const _DirChainText({required this.labels, required this.color});

  @override
  Widget build(BuildContext context) {
    final spans = <InlineSpan>[];
    for (var i = 0; i < labels.length; i++) {
      final last = i == labels.length - 1;
      spans.add(
        TextSpan(
          text: labels[i],
          style: TextStyle(
            fontSize: 12.5,
            fontWeight: last ? FontWeight.w600 : FontWeight.w500,
            color: color.textPrimary,
          ),
        ),
      );
      if (!last) {
        spans.add(
          TextSpan(
            text: ' / ',
            style: TextStyle(fontSize: 12.5, color: color.textMuted),
          ),
        );
      }
    }
    return Text.rich(
      TextSpan(children: spans),
      overflow: TextOverflow.ellipsis,
      maxLines: 1,
    );
  }
}

class _ManifestFileRowWidget extends StatelessWidget {
  final ManifestFileRow row;
  final bool selected;
  final VoidCallback onToggle;

  const _ManifestFileRowWidget({
    required this.row,
    required this.selected,
    required this.onToggle,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final s = LocaleScope.of(context);
    final item = row.item;
    final category = manifestItemCategory(item);
    final (tileBg, tileFg) = _fileTileColors(category, c);
    final ext = manifestExtensionLabel(item.name);

    Widget nameWidget = Text.rich(
      TextSpan(
        children: [
          if (row.showPath && item.path.isNotEmpty)
            TextSpan(
              text: '${item.path.split('/').last}/',
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
          TextSpan(
            text: item.name,
            style: TextStyle(fontSize: 12.5, color: c.textPrimary),
          ),
        ],
      ),
      overflow: TextOverflow.ellipsis,
      maxLines: 1,
    );
    if (row.showPath && item.path.isNotEmpty) {
      nameWidget = ShadTooltip(
        builder: (_) => Text(item.path),
        child: nameWidget,
      );
    }

    return _RowShell(
      onTap: onToggle,
      selectedBg: selected ? c.selectedBg : null,
      children: [
        Padding(
          padding: const EdgeInsets.only(right: 8),
          child: BtCheckbox(checked: selected, accentColor: c.accent),
        ),
        Container(
          width: 26,
          height: 20,
          alignment: Alignment.center,
          decoration: BoxDecoration(color: tileBg, borderRadius: m.brXs),
          child: Text(
            ext,
            style: TextStyle(
              fontSize: 8.5,
              fontWeight: FontWeight.w700,
              color: tileFg,
              letterSpacing: 0.2,
            ),
          ),
        ),
        const SizedBox(width: 6),
        Expanded(child: nameWidget),
        const SizedBox(width: _kCountColWidth),
        SizedBox(
          width: _kSizeColWidth,
          child: Text(
            item.size == 0
                ? s.manifestFileSizeUnknown
                : DownloadTask.formatBytes(item.size),
            textAlign: TextAlign.right,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(
              fontSize: 11.5,
              color: item.size == 0 ? c.textMuted : c.textSecondary,
            ),
          ),
        ),
        const SizedBox(width: _kEnterColWidth),
      ],
    );
  }
}
