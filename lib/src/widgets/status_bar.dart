import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import '../models/download_controller.dart';
import '../models/download_task.dart';
import '../i18n/locale_provider.dart';
import '../theme/app_colors.dart';

class StatusBar extends StatelessWidget {
  final DownloadController controller;

  const StatusBar({super.key, required this.controller});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    return ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        final dlSpeed = DownloadTask.formatBytes(controller.totalDownloadSpeed);
        final active = controller.activeCount;
        final paused = controller.pausedCount;
        final total = controller.tasks.length;

        return Container(
          height: 28,
          padding: const EdgeInsets.symmetric(horizontal: 16),
          decoration: BoxDecoration(
            color: c.surface1,
            border: Border(top: BorderSide(color: c.border, width: 1)),
          ),
          child: Row(
            children: [
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
            ],
          ),
        );
      },
    );
  }
}
