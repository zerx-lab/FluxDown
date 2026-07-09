import 'package:flutter/widgets.dart';

import '../../i18n/locale_provider.dart';
import '../../models/download_controller.dart';
import '../../models/download_task.dart';
import '../../theme/app_colors.dart';
import '../mobile_ui.dart';

/// 筛选面板（≈ 桌面侧边栏：文件类型 + 队列）
Future<void> showMobileFilterSheet(
  BuildContext context,
  DownloadController controller,
) {
  return showMobileSheet<void>(
    context,
    builder: (ctx) {
      final s = LocaleScope.of(ctx);
      final c = AppColors.of(ctx);
      return ListenableBuilder(
        listenable: controller,
        builder: (ctx2, _) {
          final filtered =
              controller.categoryFilter != FileCategory.all ||
              controller.customCategoryFilter != null ||
              controller.queueFilter != null;
          return MobileSheetContainer(
            title: s.mobileFilterTasks,
            // iOS 惯例：重置放标题行右侧，纯文字强调色按钮，仅在有筛选时可用
            trailing: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: filtered
                  ? () {
                      controller.setCategoryFilter(FileCategory.all);
                      if (controller.queueFilter != null) {
                        controller.setQueueFilter(null);
                      }
                    }
                  : null,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 6),
                child: Text(
                  s.mobileResetFilter,
                  style: TextStyle(
                    fontSize: 14,
                    fontWeight: FontWeight.w600,
                    color: filtered ? c.accent : c.textMuted,
                  ),
                ),
              ),
            ),
            footer: MobilePrimaryButton(
              label: s.confirm,
              onTap: () => Navigator.of(ctx).pop(),
            ),
            child: LayoutBuilder(
              builder: (ctx3, constraints) {
                // 等宽网格分布：按可用宽度动态分列（最小 ~96px，3~4 列），
                // chip 拉满列宽，避免左右大范围空白
                const gap = 8.0;
                final width = constraints.maxWidth;
                final cols = (width / 104).floor().clamp(3, 4);
                final chipWidth = (width - gap * (cols - 1)) / cols;

                Widget cell(Widget chip) =>
                    SizedBox(width: chipWidth, child: chip);

                return Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    MobileFieldLabel(s.mobileFileType),
                    Wrap(
                      spacing: gap,
                      runSpacing: gap,
                      children: [
                        for (final cat in FileCategory.values)
                          cell(
                            MobileChip(
                              label: cat == FileCategory.all
                                  ? s.tabAll
                                  : cat.label,
                              selected:
                                  controller.customCategoryFilter == null &&
                                  controller.categoryFilter == cat,
                              onTap: () => controller.setCategoryFilter(cat),
                            ),
                          ),
                      ],
                    ),
                    MobileFieldLabel(s.mobileByQueue),
                    Wrap(
                      spacing: gap,
                      runSpacing: gap,
                      children: [
                        cell(
                          MobileChip(
                            label: s.tabAll,
                            selected: controller.queueFilter == null,
                            onTap: () {
                              if (controller.queueFilter != null) {
                                controller.setQueueFilter(null);
                              }
                            },
                          ),
                        ),
                        cell(
                          MobileChip(
                            label: s.defaultQueue,
                            selected: controller.queueFilter == '',
                            onTap: () {
                              if (controller.queueFilter != '') {
                                controller.setQueueFilter('');
                              }
                            },
                          ),
                        ),
                        for (final q in controller.queues)
                          cell(
                            MobileChip(
                              label: q.name,
                              selected: controller.queueFilter == q.queueId,
                              onTap: () {
                                if (controller.queueFilter != q.queueId) {
                                  controller.setQueueFilter(q.queueId);
                                }
                              },
                            ),
                          ),
                      ],
                    ),
                    const SizedBox(height: 4),
                  ],
                );
              },
            ),
          );
        },
      );
    },
  );
}
