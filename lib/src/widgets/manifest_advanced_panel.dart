// manifest_select_dialog.dart 的高级选项部分（v1.6 §4.10 第 5 段）：默认收起
// 的折叠条（偏离默认时亮 accent 圆点）+ 展开面板（组级：任务代理 / 线程数 /
// 忽略 HTTPS 证书错误 / UA 模式+输入框 / Cookie / 自定义请求头动态 K-V 行）。
//
// 一份配置随 CreateTaskGroup 下发全部子任务；无哈希校验字段——多文件组
// 无法共用单一哈希，逐文件校验走任务详情（镶镜像新建下载对话框高级区，
// 哈希字段排除）。文本字段用 [TextEditingController] 承载，输入不经父级
// setState（Flutter 的 Element 复用天然保住焦点/光标，无需额外隔离层）。

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/manifest_selection.dart';
import '../theme/app_colors.dart';

/// 自定义请求头一行的输入控制器对（key/value）。
class ManifestHeaderRowControllers {
  final TextEditingController keyController;
  final TextEditingController valueController;

  ManifestHeaderRowControllers({String key = '', String value = ''})
    : keyController = TextEditingController(text: key),
      valueController = TextEditingController(text: value);

  void dispose() {
    keyController.dispose();
    valueController.dispose();
  }
}

/// 高级选项面板的全部可编辑状态：文本字段用 controller（父级持有，跨重建
/// 存活），非文本字段（开关/单选/线程数）作为普通字段随 setState 更新。
class ManifestAdvancedControllers {
  final TextEditingController proxyController;
  final TextEditingController userAgentController;
  final TextEditingController cookieController;
  final List<ManifestHeaderRowControllers> headerRows;

  ManifestAdvancedControllers({
    required String initialProxyUrl,
    required String initialUserAgent,
    required String initialCookies,
    required Map<String, String> initialHeaders,
  }) : proxyController = TextEditingController(text: initialProxyUrl),
       userAgentController = TextEditingController(text: initialUserAgent),
       cookieController = TextEditingController(text: initialCookies),
       headerRows = [
         for (final entry in initialHeaders.entries)
           ManifestHeaderRowControllers(key: entry.key, value: entry.value),
       ];

  void dispose() {
    proxyController.dispose();
    userAgentController.dispose();
    cookieController.dispose();
    for (final row in headerRows) {
      row.dispose();
    }
  }

  List<ManifestHeaderEntry> snapshotHeaders() => [
    for (final row in headerRows)
      ManifestHeaderEntry(
        key: row.keyController.text,
        value: row.valueController.text,
      ),
  ];
}

/// 每子任务线程数固定预设（对齐 manifest.js `["auto","1","4","8","16","32"]`；
/// `"auto"` ↔ 模型层 `segments == 0`）。
const List<String> kManifestSegmentPresets = ['auto', '1', '4', '8', '16', '32'];

class ManifestAdvancedPanel extends StatelessWidget {
  final bool open;
  final bool dirty;
  final VoidCallback onToggleOpen;
  final ManifestAdvancedControllers controllers;

  final bool ignoreTlsErrors;
  final ValueChanged<bool> onIgnoreTlsChanged;

  /// true = 继承全局 UA；false = 自定义（此时 [ManifestAdvancedControllers.
  /// userAgentController] 生效）。
  final bool uaInherit;
  final ValueChanged<bool> onUaInheritChanged;

  /// 0 = 自动。
  final int segments;
  final ValueChanged<int> onSegmentsChanged;

  final VoidCallback onAddHeader;
  final ValueChanged<int> onRemoveHeader;

  const ManifestAdvancedPanel({
    super.key,
    required this.open,
    required this.dirty,
    required this.onToggleOpen,
    required this.controllers,
    required this.ignoreTlsErrors,
    required this.onIgnoreTlsChanged,
    required this.uaInherit,
    required this.onUaInheritChanged,
    required this.segments,
    required this.onSegmentsChanged,
    required this.onAddHeader,
    required this.onRemoveHeader,
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onToggleOpen,
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 8),
            child: Row(
              children: [
                Icon(
                  open ? LucideIcons.chevronDown : LucideIcons.chevronRight,
                  size: 13,
                  color: c.textMuted,
                ),
                const SizedBox(width: 6),
                Text(
                  s.manifestAdvancedToggle,
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: FontWeight.w500,
                    color: c.textSecondary,
                  ),
                ),
                if (dirty) ...[
                  const SizedBox(width: 6),
                  ShadTooltip(
                    builder: (_) => Text(s.manifestAdvancedDotTooltip),
                    child: Container(
                      width: 6,
                      height: 6,
                      decoration: BoxDecoration(
                        color: c.accent,
                        shape: BoxShape.circle,
                      ),
                    ),
                  ),
                ],
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    s.manifestAdvancedHint,
                    overflow: TextOverflow.ellipsis,
                    maxLines: 1,
                    style: TextStyle(fontSize: 10.5, color: c.textMuted),
                  ),
                ),
              ],
            ),
          ),
        ),
        if (open)
          ConstrainedBox(
            constraints: const BoxConstraints(maxHeight: 240),
            child: SingleChildScrollView(
              child: Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Expanded(
                          flex: 2,
                          child: _field(
                            label: s.taskProxy,
                            hint: s.manifestProxyHint,
                            c: c,
                            child: ShadInput(
                              controller: controllers.proxyController,
                              placeholder: Text(s.taskProxyPlaceholder),
                            ),
                          ),
                        ),
                        const SizedBox(width: 8),
                        Expanded(
                          child: _field(
                            label: s.threads,
                            hint: s.manifestSegmentsHint,
                            c: c,
                            child: _buildSegmentsSelect(context, c),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 10),
                    Row(
                      children: [
                        Expanded(
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(
                                s.taskIgnoreTlsErrors,
                                style: TextStyle(
                                  fontSize: 12,
                                  fontWeight: FontWeight.w500,
                                  color: c.textPrimary,
                                ),
                              ),
                              const SizedBox(height: 2),
                              Text(
                                s.manifestIgnoreTlsHint,
                                style: TextStyle(fontSize: 11, color: c.textMuted),
                              ),
                            ],
                          ),
                        ),
                        const SizedBox(width: 12),
                        ShadSwitch(
                          value: ignoreTlsErrors,
                          onChanged: onIgnoreTlsChanged,
                        ),
                      ],
                    ),
                    const SizedBox(height: 10),
                    Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        SizedBox(
                          width: 150,
                          child: _field(
                            label: s.userAgent,
                            hint: null,
                            c: c,
                            child: ShadSelect<bool>(
                              initialValue: uaInherit,
                              options: [
                                ShadOption(
                                  value: true,
                                  child: Text(s.queueUaInheritGlobal),
                                ),
                                ShadOption(
                                  value: false,
                                  child: Text(s.userAgentPresetCustom),
                                ),
                              ],
                              selectedOptionBuilder: (context, value) => Text(
                                value ? s.queueUaInheritGlobal : s.userAgentPresetCustom,
                                overflow: TextOverflow.ellipsis,
                                maxLines: 1,
                              ),
                              onChanged: (value) {
                                if (value != null) onUaInheritChanged(value);
                              },
                            ),
                          ),
                        ),
                        const SizedBox(width: 8),
                        Expanded(
                          child: Padding(
                            padding: const EdgeInsets.only(top: 20),
                            child: ShadInput(
                              controller: controllers.userAgentController,
                              enabled: !uaInherit,
                              placeholder: Text(
                                uaInherit
                                    ? s.userAgentTaskPlaceholder
                                    : s.manifestUaCustomPlaceholder,
                              ),
                            ),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 10),
                    _field(
                      label: s.taskCookie,
                      hint: s.manifestCookieHint,
                      c: c,
                      child: ShadInput(
                        controller: controllers.cookieController,
                        placeholder: Text(s.taskCookiePlaceholder),
                        maxLines: 2,
                      ),
                    ),
                    const SizedBox(height: 10),
                    _field(
                      label: s.taskHeaders,
                      hint: s.manifestHeadersHint,
                      c: c,
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          for (
                            var i = 0;
                            i < controllers.headerRows.length;
                            i++
                          ) ...[
                            if (i > 0) const SizedBox(height: 6),
                            _HeaderRowFields(
                              row: controllers.headerRows[i],
                              s: s,
                              c: c,
                              onDelete: () => onRemoveHeader(i),
                            ),
                          ],
                          const SizedBox(height: 6),
                          Align(
                            alignment: Alignment.centerLeft,
                            child: ShadButton.ghost(
                              size: ShadButtonSize.sm,
                              onPressed: onAddHeader,
                              child: Row(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  Icon(
                                    LucideIcons.plus,
                                    size: 13,
                                    color: c.accent,
                                  ),
                                  const SizedBox(width: 6),
                                  Text(
                                    s.taskHeadersAdd,
                                    style: TextStyle(
                                      fontSize: 12,
                                      color: c.accent,
                                    ),
                                  ),
                                ],
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
      ],
    );
  }

  Widget _buildSegmentsSelect(BuildContext context, AppColors c) {
    final s = LocaleScope.of(context);
    final current = segments == 0 ? 'auto' : segments.toString();
    return ShadSelect<String>(
      initialValue: kManifestSegmentPresets.contains(current) ? current : 'auto',
      options: [
        for (final v in kManifestSegmentPresets)
          ShadOption(value: v, child: Text(v == 'auto' ? s.auto : v)),
      ],
      selectedOptionBuilder: (context, value) =>
          Text(value == 'auto' ? s.auto : value),
      onChanged: (value) {
        if (value == null) return;
        onSegmentsChanged(value == 'auto' ? 0 : int.parse(value));
      },
    );
  }

  Widget _field({
    required String label,
    required String? hint,
    required AppColors c,
    required Widget child,
  }) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text.rich(
          TextSpan(
            children: [
              TextSpan(
                text: label,
                style: TextStyle(
                  fontSize: 11,
                  fontWeight: FontWeight.w500,
                  color: c.textSecondary,
                ),
              ),
              if (hint != null)
                TextSpan(
                  text: '  $hint',
                  style: TextStyle(fontSize: 10, color: c.textMuted),
                ),
            ],
          ),
        ),
        const SizedBox(height: 4),
        child,
      ],
    );
  }
}

class _HeaderRowFields extends StatelessWidget {
  final ManifestHeaderRowControllers row;
  final S s;
  final AppColors c;
  final VoidCallback onDelete;

  const _HeaderRowFields({
    required this.row,
    required this.s,
    required this.c,
    required this.onDelete,
  });

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        Expanded(
          flex: 2,
          child: ShadInput(
            controller: row.keyController,
            placeholder: Text(s.taskHeadersKeyPlaceholder),
          ),
        ),
        const SizedBox(width: 6),
        Expanded(
          flex: 3,
          child: ShadInput(
            controller: row.valueController,
            placeholder: Text(s.taskHeadersValuePlaceholder),
          ),
        ),
        const SizedBox(width: 4),
        GestureDetector(
          onTap: onDelete,
          child: Icon(LucideIcons.x, size: 16, color: c.textMuted),
        ),
      ],
    );
  }
}
